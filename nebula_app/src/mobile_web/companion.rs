use super::{
    Status,
    protocol::{ClientHello, Envelope, SealedPayload, SecureCipher, accept_client_hello},
    task_api::{HttpResponse, TaskApi},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures::{SinkExt as _, StreamExt as _};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex as AsyncMutex, Semaphore, mpsc};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

const HEARTBEAT: Duration = Duration::from_secs(25);
const RETRY_SECONDS: [u64; 5] = [1, 2, 5, 10, 30];
const MAX_HTTP_REQUESTS: usize = 32;
const MAX_INNER_REQUEST_BYTES: usize = 64 * 1024;
const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
const MAX_SESSIONS: usize = 8;
const SESSION_IDLE: Duration = Duration::from_secs(15 * 60);

#[cfg(test)]
mod browser_tests;

#[derive(Clone)]
struct Outbound {
    sender: mpsc::Sender<String>,
    failed: Arc<tokio::sync::Notify>,
}

struct SecureSession {
    cipher: SessionCipher,
    last_used: std::time::Instant,
}

type SharedStatus = Arc<Mutex<Status>>;
type SessionCipher = Arc<AsyncMutex<SecureCipher>>;

pub(super) async fn run(
    relay_url: String,
    credentials: super::config::DeviceCredentials,
    api: TaskApi,
    status: SharedStatus,
    generation: Arc<AtomicU64>,
    expected_generation: u64,
) {
    let api = api.for_connection(generation.clone(), expected_generation);
    let permits = Arc::new(Semaphore::new(MAX_HTTP_REQUESTS));
    let mut retry_index = 0;
    loop {
        if !is_current(&generation, expected_generation) {
            return;
        }
        set_status_if_current(&status, &generation, expected_generation, Status::Connecting);
        let endpoint = match websocket_endpoint(&relay_url, "/device") {
            Ok(endpoint) => endpoint,
            Err(message) => {
                set_status_if_current(
                    &status,
                    &generation,
                    expected_generation,
                    Status::Failed(message),
                );
                return;
            },
        };
        let websocket_config = WebSocketConfig::default()
            .max_message_size(Some(MAX_FRAME_BYTES))
            .max_frame_size(Some(MAX_FRAME_BYTES));
        let connection = tokio::select! {
            connection = tokio::time::timeout(Duration::from_secs(15), connect_async_with_config(endpoint.as_str(), Some(websocket_config), false)) => connection,
            () = wait_until_obsolete(generation.clone(), expected_generation) => return,
        };
        match connection {
            Ok(Ok((socket, _))) => {
                retry_index = 0;
                let outcome = pump(
                    socket,
                    &credentials,
                    &api,
                    &status,
                    &generation,
                    expected_generation,
                    &permits,
                )
                .await;
                match outcome {
                    PumpOutcome::Stopped => return,
                    PumpOutcome::AuthFailed => {
                        set_status_if_current(
                            &status,
                            &generation,
                            expected_generation,
                            Status::AuthFailed,
                        );
                        return;
                    },
                    PumpOutcome::Disconnected => {
                        set_status_if_current(
                            &status,
                            &generation,
                            expected_generation,
                            Status::Offline,
                        );
                    },
                }
            },
            failure => {
                set_status_if_current(&status, &generation, expected_generation, Status::Offline);
                log::warn!("mobile web Relay connection failed: {failure:?}");
            },
        }
        if !is_current(&generation, expected_generation) {
            return;
        }
        let seconds = RETRY_SECONDS[retry_index.min(RETRY_SECONDS.len() - 1)];
        retry_index = retry_index.saturating_add(1);
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(seconds)) => {},
            () = wait_until_obsolete(generation.clone(), expected_generation) => return,
        }
    }
}

async fn pump<S>(
    socket: tokio_tungstenite::WebSocketStream<S>,
    credentials: &super::config::DeviceCredentials,
    api: &TaskApi,
    status: &SharedStatus,
    generation: &Arc<AtomicU64>,
    expected_generation: u64,
    permits: &Arc<Semaphore>,
) -> PumpOutcome
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut writer, mut reader) = socket.split();
    let (sender, mut outbound_rx) = mpsc::channel::<String>(4);
    let outbound = Outbound { sender, failed: Arc::new(tokio::sync::Notify::new()) };
    let writer_task = tokio::spawn(async move {
        while let Some(text) = outbound_rx.recv().await {
            if !matches!(
                tokio::time::timeout(
                    Duration::from_secs(15),
                    writer.send(Message::Text(text.into()))
                )
                .await,
                Ok(Ok(()))
            ) {
                break;
            }
        }
    });
    let auth = Envelope::new(
        "auth",
        json!({
            "deviceId": credentials.device_id,
            "deviceToken": credentials.device_token,
            "capabilities": ["sealed-tunnel-v1"],
            "version": env!("CARGO_PKG_VERSION"),
        }),
        None,
    );
    if enqueue(&outbound, &auth).is_err() {
        writer_task.abort();
        return PumpOutcome::Disconnected;
    }

    let mut authenticated = false;
    let mut sessions: HashMap<String, SecureSession> = HashMap::new();
    let mut requests = tokio::task::JoinSet::new();
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    let mut lifecycle = tokio::time::interval(Duration::from_secs(1));
    heartbeat.tick().await;
    lifecycle.tick().await;
    let outcome = loop {
        tokio::select! {
            () = outbound.failed.notified() => break PumpOutcome::Disconnected,
            _ = lifecycle.tick() => {
                if !is_current(generation, expected_generation) {
                    break PumpOutcome::Stopped;
                }
                sessions.retain(|_, session| session.last_used.elapsed() < SESSION_IDLE);
                if let Some((hello, bootstrap_key)) = super::admission::take_approved(expected_generation) {
                    establish_session(&bootstrap_key, hello, &outbound, &mut sessions, Some(&credentials.e2ee_master_key));
                }
            },
            _ = heartbeat.tick(), if authenticated => {
                let ping = Envelope::new("ping", json!({}), None);
                if enqueue(&outbound, &ping).is_err() {
                    break PumpOutcome::Disconnected;
                }
            },
            incoming = reader.next() => {
                let Some(incoming) = incoming else {
                    break PumpOutcome::Disconnected;
                };
                let Ok(message) = incoming else {
                    break PumpOutcome::Disconnected;
                };
                let Message::Text(text) = message else {
                    if matches!(message, Message::Close(_)) {
                        break PumpOutcome::Disconnected;
                    }
                    continue;
                };
                let Ok(envelope) = serde_json::from_str::<Envelope>(text.as_str()) else {
                    break PumpOutcome::Disconnected;
                };
                match envelope.kind.as_str() {
                    "auth_ok" => {
                        authenticated = true;
                        set_status_if_current(
                            status,
                            generation,
                            expected_generation,
                            Status::Connected,
                        );
                        let state = Envelope::new("status", json!({"dsh":"online"}), None);
                        let _ = enqueue(&outbound, &state);
                    },
                    "auth_fail" => break PumpOutcome::AuthFailed,
                    "client_hello" => {
                        accept_session(
                            &credentials.e2ee_master_key,
                            envelope.payload,
                            &outbound,
                            &mut sessions,
                            expected_generation,
                        );
                    },
                    "sealed" => {
                        handle_sealed(
                            &envelope.payload,
                            &mut sessions,
                            &outbound,
                            api,
                            &credentials.device_id,
                            &permits,
                            &mut requests,
                        )
                        .await;
                    },
                    "client_close" => {
                        if let Some(session_id) = envelope.payload["accessSessionId"].as_str() {
                            sessions.remove(session_id);
                            super::admission::client_closed(expected_generation, session_id);
                        }
                    },
                    _ => {},
                }
            },
            Some(_) = requests.join_next(), if !requests.is_empty() => {},
        }
    };
    // Keep accepted HTTP work alive after a browser or Relay disconnect. Each
    // task owns its receipt completion; aborting here could strand it as active.
    requests.detach_all();
    super::admission::clear_pending(expected_generation);
    drop(outbound);
    writer_task.abort();
    outcome
}

fn accept_session(
    master_key: &str,
    payload: Value,
    outbound: &Outbound,
    sessions: &mut HashMap<String, SecureSession>,
    generation: u64,
) {
    let Ok(hello) = serde_json::from_value::<ClientHello>(payload) else {
        return;
    };
    let session_id = hello.access_session_id.clone();
    if sessions.len() >= MAX_SESSIONS || session_id.len() > 256 {
        return;
    }
    if !establish_session(master_key, hello.clone(), outbound, sessions, None) {
        if !super::admission::offer(generation, hello) {
            let close = Envelope::new(
                "device_close",
                json!({"accessSessionId": session_id, "reason": "e2ee_handshake_failed"}),
                None,
            );
            let _ = enqueue(outbound, &close);
        }
    }
}

fn establish_session(
    master_key: &str,
    hello: ClientHello,
    outbound: &Outbound,
    sessions: &mut HashMap<String, SecureSession>,
    replacement_key: Option<&str>,
) -> bool {
    if sessions.len() >= MAX_SESSIONS {
        return false;
    }
    let session_id = hello.access_session_id.clone();
    let Ok(mut accepted) = accept_client_hello(master_key, hello) else { return false };
    accepted.payload["enrollmentRequired"] = json!(replacement_key.is_some());
    if enqueue(outbound, &Envelope::new("server_hello", accepted.payload, None)).is_err() {
        return false;
    }
    if let Some(key) = replacement_key {
        let inner = Envelope::new("pebrel_browser_enrolled", json!({"masterKey":key}), None);
        let Ok(sealed) = accepted.cipher.seal(&serde_json::to_value(inner).unwrap_or(Value::Null))
        else {
            return false;
        };
        let outer = Envelope::new(
            "sealed",
            json!({"accessSessionId": session_id,"seq":sealed.seq,"ciphertextB64":sealed.ciphertext_b64}),
            None,
        );
        if enqueue(outbound, &outer).is_err() {
            return false;
        }
    }
    sessions.insert(
        session_id,
        SecureSession {
            cipher: Arc::new(AsyncMutex::new(accepted.cipher)),
            last_used: std::time::Instant::now(),
        },
    );
    true
}

async fn handle_sealed(
    payload: &Value,
    sessions: &mut HashMap<String, SecureSession>,
    outbound: &Outbound,
    api: &TaskApi,
    device_id: &str,
    permits: &Arc<Semaphore>,
    requests: &mut tokio::task::JoinSet<()>,
) {
    let Some(session_id) = payload["accessSessionId"].as_str().map(str::to_owned) else {
        return;
    };
    let Some(session) = sessions.get_mut(&session_id) else {
        let close = Envelope::new(
            "device_close",
            json!({"accessSessionId": session_id, "reason": "e2ee_auth_failed"}),
            None,
        );
        let _ = enqueue(outbound, &close);
        return;
    };
    session.last_used = std::time::Instant::now();
    let cipher = session.cipher.clone();
    let sealed = SealedPayload {
        seq: String::from(payload["seq"].as_str().unwrap_or_default()),
        ciphertext_b64: String::from(payload["ciphertextB64"].as_str().unwrap_or_default()),
    };
    let inner = {
        let mut cipher = cipher.lock().await;
        cipher.open(&sealed)
    }
    .and_then(|value| serde_json::from_value::<Envelope>(value).map_err(|error| error.to_string()));
    let Ok(inner) = inner else {
        let close = Envelope::new(
            "device_close",
            json!({"accessSessionId": session_id, "reason": "e2ee_auth_failed"}),
            None,
        );
        let _ = enqueue(outbound, &close);
        return;
    };
    if inner.kind != "http_req" {
        return;
    }
    let Ok(permit) = permits.clone().try_acquire_owned() else {
        send_response(
            outbound,
            &session_id,
            &cipher,
            inner.channel.as_deref(),
            HttpResponse::error(429, "too_many_tunnels", "too many concurrent requests"),
        )
        .await;
        return;
    };
    let cipher = cipher.clone();
    let outbound = outbound.clone();
    let api = api.clone();
    let device_id = device_id.to_owned();
    requests.spawn(async move {
        let channel = inner.channel.clone();
        let response = handle_http(&api, &device_id, inner).await;
        send_response(&outbound, &session_id, &cipher, channel.as_deref(), response).await;
        drop(permit);
    });
}

async fn handle_http(api: &TaskApi, device_id: &str, envelope: Envelope) -> HttpResponse {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Request {
        method: String,
        path: String,
        #[serde(default)]
        #[allow(dead_code)]
        headers: Value,
        #[serde(default)]
        body_b64: String,
    }
    let Ok(request) = serde_json::from_value::<Request>(envelope.payload) else {
        return HttpResponse::error(400, "invalid_request", "invalid HTTP envelope");
    };
    let path = match local_path(&request.path) {
        Ok(path) => path,
        Err(response) => return response,
    };
    let body = match STANDARD.decode(request.body_b64) {
        Ok(body) if body.len() <= MAX_INNER_REQUEST_BYTES => body,
        _ => return HttpResponse::error(413, "request_too_large", "request exceeds 64 KiB"),
    };
    if !matches!(request.method.as_str(), "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE") {
        return HttpResponse::error(405, "method_not_allowed", "unsupported method");
    }
    api.respond(device_id, &request.method, path, &body).await
}

async fn send_response(
    outbound: &Outbound,
    session_id: &str,
    cipher: &SessionCipher,
    channel: Option<&str>,
    mut response: HttpResponse,
) {
    if response.body.len() > 1024 * 1024 {
        response =
            HttpResponse::error(502, "response_too_large", "response exceeds mobile tunnel limit");
    }
    let inner = Envelope::new(
        "http_res",
        json!({
            "status": response.status,
            "headers": {
                "content-type": response.content_type,
                "cache-control": response.cache_control,
            },
            "bodyB64": STANDARD.encode(response.body),
            "seq": 0,
            "final": true,
        }),
        channel.map(str::to_owned),
    );
    {
        let mut cipher = cipher.lock().await;
        let sealed = cipher.seal(&serde_json::to_value(inner).unwrap_or(Value::Null));
        let Ok(sealed) = sealed else {
            return;
        };
        let outer = Envelope::new(
            "sealed",
            json!({"accessSessionId": session_id, "seq": sealed.seq, "ciphertextB64": sealed.ciphertext_b64}),
            None,
        );
        let _ = enqueue(outbound, &outer);
    };
}

fn local_path(value: &str) -> Result<&str, HttpResponse> {
    if !value.starts_with('/')
        || value.starts_with("//")
        || value.len() > 4096
        || value.chars().any(char::is_control)
    {
        return Err(HttpResponse::error(400, "invalid_path", "invalid local path"));
    }
    Ok(value.split('?').next().unwrap_or(value))
}

fn websocket_endpoint(relay: &str, path: &str) -> Result<String, String> {
    let base = relay.trim_end_matches('/');
    let endpoint = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}{path}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}{path}")
    } else {
        return Err(String::from("Relay must use HTTPS or loopback HTTP"));
    };
    Ok(endpoint)
}

fn enqueue(outbound: &Outbound, envelope: &Envelope) -> Result<(), String> {
    let value = envelope.encode()?;
    if value.len() > MAX_FRAME_BYTES || outbound.sender.try_send(value).is_err() {
        outbound.failed.notify_one();
        return Err(String::from("Relay output limit reached"));
    }
    Ok(())
}

fn is_current(generation: &AtomicU64, expected: u64) -> bool {
    generation.load(Ordering::Acquire) == expected
}

fn set_status_if_current(
    status: &SharedStatus,
    generation: &AtomicU64,
    expected: u64,
    next: Status,
) {
    if let Ok(mut status) = status.lock() {
        if is_current(generation, expected) {
            *status = next;
        }
    }
}

async fn wait_until_obsolete(generation: Arc<AtomicU64>, expected: u64) {
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if !is_current(&generation, expected) {
            return;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PumpOutcome {
    Disconnected,
    AuthFailed,
    Stopped,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_websocket_endpoint_preserves_the_origin_and_uses_device_route() {
        assert_eq!(
            websocket_endpoint("https://relay.example.com/", "/device").unwrap(),
            "wss://relay.example.com/device"
        );
        assert_eq!(
            websocket_endpoint("http://127.0.0.1:8787", "/device").unwrap(),
            "ws://127.0.0.1:8787/device"
        );
    }

    #[test]
    fn inner_paths_are_absolute_and_queries_are_not_forwarded() {
        assert_eq!(local_path("/api/v1/state").ok().unwrap(), "/api/v1/state");
        assert_eq!(local_path("/app.css?v=1").ok().unwrap(), "/app.css");
        assert!(local_path("https://example.com").is_err());
        assert!(local_path("//127.0.0.1/private").is_err());
    }

    #[test]
    fn slow_output_is_bounded_and_obsolete_status_cannot_overwrite_stopped() {
        let (sender, _receiver) = mpsc::channel(1);
        let outbound = Outbound { sender, failed: Arc::new(tokio::sync::Notify::new()) };
        let envelope = Envelope::new("ping", json!({}), None);
        assert!(enqueue(&outbound, &envelope).is_ok());
        assert!(enqueue(&outbound, &envelope).is_err());
        let status = Arc::new(Mutex::new(Status::Stopped));
        let generation = AtomicU64::new(2);
        set_status_if_current(&status, &generation, 1, Status::Connected);
        assert_eq!(*status.lock().unwrap(), Status::Stopped);
        set_status_if_current(&status, &generation, 2, Status::Offline);
        assert_eq!(*status.lock().unwrap(), Status::Offline);
    }
}
