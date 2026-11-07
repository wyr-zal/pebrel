use super::{
    config::{Preferences, ProjectGrant},
    receipts::{Begin, Receipt, ReceiptStore, payload_hash},
};
use crate::runtime_api::ApiResponse;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    sync::{Arc, RwLock},
    time::Duration,
};
mod access;
mod control;
mod session;

use access::pane_matches_project;

const MAX_BODY_BYTES: usize = 64 * 1024;
const RUNTIME_TIMEOUT: Duration = Duration::from_secs(30);
const SESSION_READY_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone)]
struct ApiState {
    preferences: Arc<RwLock<Preferences>>,
    receipts: ReceiptStore,
}

pub(crate) struct HttpResponse {
    pub(crate) status: u16,
    pub(crate) content_type: &'static str,
    pub(crate) cache_control: &'static str,
    pub(crate) body: Vec<u8>,
}

impl HttpResponse {
    fn json(status: u16, value: Value) -> Self {
        Self {
            status,
            content_type: "application/json; charset=utf-8",
            cache_control: "no-store",
            body: serde_json::to_vec(&value).unwrap_or_else(|_| b"{}".to_vec()),
        }
    }

    pub(crate) fn error(status: u16, code: &str, message: impl Into<String>) -> Self {
        Self::json(status, json!({"error":{"code":code,"message":message.into()}}))
    }

    fn asset(body: &'static str, content_type: &'static str, cache: &'static str) -> Self {
        Self { status: 200, content_type, cache_control: cache, body: body.as_bytes().to_vec() }
    }

    fn manifest(device_id: &str) -> Self {
        let path = format!("/remote/{}/", super::url_encode(device_id));
        let mut response = Self::json(
            200,
            json!({
                "name": "Pebrel Mobile",
                "short_name": "Pebrel",
                "description": "从手机操作电脑上的 Claude Code 和 Codex 会话",
                "start_url": path,
                "scope": path,
                "display": "standalone",
                "background_color": "#0c0e12",
                "theme_color": "#0f1115",
                "icons": [{
                    "src": "/icon.svg",
                    "sizes": "any",
                    "type": "image/svg+xml",
                    "purpose": "any maskable"
                }]
            }),
        );
        response.content_type = "application/manifest+json";
        response
    }
}

pub(crate) struct TaskApi {
    state: ApiState,
    connection: Option<(Arc<std::sync::atomic::AtomicU64>, u64)>,
}

impl TaskApi {
    pub(crate) fn new(preferences: Arc<RwLock<Preferences>>, receipts: ReceiptStore) -> Self {
        Self { state: ApiState { preferences, receipts }, connection: None }
    }

    pub(super) fn for_connection(
        mut self,
        generation: Arc<std::sync::atomic::AtomicU64>,
        expected: u64,
    ) -> Self {
        self.connection = Some((generation, expected));
        self
    }

    pub(crate) async fn respond(
        &self,
        device_id: &str,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> HttpResponse {
        if path.starts_with("/api/v1/")
            && let Err(response) = self.require_enabled()
        {
            return response;
        }
        match (method, path) {
            ("GET", "/") | ("GET", "/index.html") => HttpResponse::asset(
                include_str!("../../web/index.html"),
                "text/html; charset=utf-8",
                "no-cache",
            ),
            ("GET", "/app.css") => HttpResponse::asset(
                include_str!("../../web/app.css"),
                "text/css; charset=utf-8",
                "public, max-age=3600",
            ),
            ("GET", "/app.js") => HttpResponse::asset(
                include_str!("../../web/app.js"),
                "text/javascript; charset=utf-8",
                "no-cache",
            ),
            ("GET", "/transport.js") => HttpResponse::asset(
                include_str!("../../web/transport.js"),
                "text/javascript; charset=utf-8",
                "no-cache",
            ),
            ("GET", "/presentation.js") => HttpResponse::asset(
                include_str!("../../web/presentation.js"),
                "text/javascript; charset=utf-8",
                "no-cache",
            ),
            ("GET", "/manifest.webmanifest") => HttpResponse::manifest(device_id),
            ("GET", "/icon.svg") => HttpResponse::asset(
                include_str!("../../web/icon.svg"),
                "image/svg+xml",
                "public, max-age=86400",
            ),
            ("GET", "/api/v1/state") => self.state().await,
            ("POST", "/api/v1/session/read") => self.read(body).await,
            ("POST", "/api/v1/session/new") => self.new_session(device_id, body).await,
            ("POST", "/api/v1/session/send") => self.send(device_id, body).await,
            ("POST", "/api/v1/session/choose") => self.choose(device_id, body).await,
            ("POST", "/api/v1/session/stop") => self.stop(device_id, body).await,
            _ if method == "GET" && path.starts_with("/api/v1/receipt/") => {
                self.receipt(device_id, &path["/api/v1/receipt/".len()..])
            },
            _ => HttpResponse::error(404, "not_found", "resource not found"),
        }
    }

    async fn state(&self) -> HttpResponse {
        let snapshot = match runtime_call("runtime.snapshot", json!({})).await {
            Ok(value) => value,
            Err(response) => return response,
        };
        let preferences = match self.current_preferences() {
            Ok(value) => value,
            Err(response) => return response,
        };
        let projects: Vec<_> = preferences
            .projects
            .iter()
            .map(|project| {
                json!({
                    "id": project.id,
                    "name": project.name,
                    "path": project.path,
                    "shellId": project.shell_id,
                })
            })
            .collect();
        let mut sessions = Vec::new();
        for window in snapshot["windows"].as_array().into_iter().flatten() {
            for tab in window["tabs"].as_array().into_iter().flatten() {
                for pane in tab["panes"].as_array().into_iter().flatten() {
                    let Some(agent) = pane.get("agent").filter(|value| !value.is_null()) else {
                        continue;
                    };
                    let Some(session_id) = agent["session_id"].as_str() else {
                        continue;
                    };
                    let Some(kind) = agent["kind"].as_str() else {
                        continue;
                    };
                    if !matches!(kind, "claude" | "codex") || session_id.is_empty() {
                        continue;
                    }
                    let Some(cwd) = pane["cwd"].as_str() else {
                        continue;
                    };
                    let Some(project) = preferences
                        .projects
                        .iter()
                        .find(|project| pane_matches_project(pane, project))
                    else {
                        continue;
                    };
                    sessions.push(json!({
                        "projectId": project.id,
                        "windowId": window["id"],
                        "paneId": pane["id"],
                        "kind": kind,
                        "sessionId": session_id,
                        "title": pane["title"],
                        "cwd": cwd,
                        "state": pane["task_state"],
                        "stateSource": agent["state_source"],
                        "stateConfirmed": pane["task_state_confirmed"],
                        "stateReason": pane["task_state_reason"],
                        "revision": snapshot["revision"],
                    }));
                }
            }
        }
        HttpResponse::json(
            200,
            json!({
                "revision": snapshot["revision"],
                "processId": snapshot["process_id"],
                "appVersion": snapshot["app_version"],
                "deviceName": preferences.device_name,
                "projects": projects,
                "sessions": sessions,
                "serverTime": super::now_secs(),
            }),
        )
    }

    async fn read(&self, body: &[u8]) -> HttpResponse {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Request {
            window_id: u64,
            pane_id: u64,
            identity: Value,
            before: Option<u64>,
            revision: Option<String>,
        }
        let request: Request = match parse_body(body) {
            Ok(request) => request,
            Err(response) => return response,
        };
        let (identity, _, project) = match self
            .authorize_identity_with_pane(request.window_id, request.pane_id, &request.identity)
            .await
        {
            Ok(authorized) => authorized,
            Err(response) => return response,
        };
        let response = runtime_call(
            "conversation.read",
            json!({
                "window_id": request.window_id,
                "pane_id": request.pane_id,
                "identity": identity,
                "before": request.before,
                "revision": request.revision,
            }),
        )
        .await;
        let value = match response {
            Ok(value) => value,
            Err(response)
                if matches!(
                    error_value_ref(&response).pointer("/error/code").and_then(Value::as_str),
                    Some(
                        "conversation_unavailable"
                            | "conversation_changed"
                            | "conversation_not_ready"
                    )
                ) =>
            {
                let mut status = match runtime_call(
                    "conversation.status",
                    json!({
                        "window_id": request.window_id,
                        "pane_id": request.pane_id,
                        "identity": identity,
                    }),
                )
                .await
                {
                    Ok(status) => status,
                    Err(response) => return response,
                };
                // Keep existing history visible while native metadata/files catch
                // up. Status still comes from the identity-checked live pane.
                status["unchanged"] = json!(true);
                status["transcriptUnavailable"] = json!(true);
                status
            },
            Err(response) => return response,
        };
        if !self.project_is_current(&project) {
            return HttpResponse::error(
                403,
                "project_not_authorized",
                "project authorization changed",
            );
        }
        if let Err(response) =
            self.authorize_identity(request.window_id, request.pane_id, &identity).await
        {
            return response;
        }
        HttpResponse::json(200, value)
    }

    fn receipt(&self, device_id: &str, request_id: &str) -> HttpResponse {
        if !valid_request_id(request_id) {
            return HttpResponse::error(400, "invalid_request", "invalid request id");
        }
        match self.state.receipts.get(device_id, request_id) {
            Ok(Some(receipt)) => HttpResponse::json(200, receipt_json(receipt)),
            Ok(None) => HttpResponse::error(404, "receipt_not_found", "receipt not found"),
            Err(_) => HttpResponse::error(500, "receipt_store_failed", "receipt unavailable"),
        }
    }

    fn begin_write(
        &self,
        device_id: &str,
        request_id: &str,
        method: &str,
        payload: &Value,
    ) -> Result<Option<Receipt>, HttpResponse> {
        match self.state.receipts.begin(device_id, request_id, method, &payload_hash(payload)) {
            Ok(Begin::New) => Ok(None),
            Ok(Begin::Existing(receipt)) => Ok(Some(receipt)),
            Ok(Begin::Conflict) => Err(HttpResponse::error(
                409,
                "request_conflict",
                "request id was reused with different content",
            )),
            Err(_) => {
                Err(HttpResponse::error(500, "receipt_store_failed", "request receipt unavailable"))
            },
        }
    }

    fn mark_dispatching(&self, device_id: &str, request_id: &str) -> Result<(), HttpResponse> {
        self.state
            .receipts
            .dispatch(device_id, request_id)
            .map_err(|_| HttpResponse::error(500, "receipt_store_failed", "receipt unavailable"))
    }

    fn project_is_current(&self, project: &ProjectGrant) -> bool {
        self.current_preferences()
            .is_ok_and(|preferences| project_is_current(&preferences, project))
    }

    fn current_preferences(&self) -> Result<Preferences, HttpResponse> {
        if self.connection.as_ref().is_some_and(|(generation, expected)| {
            generation.load(std::sync::atomic::Ordering::Acquire) != *expected
        }) {
            return Err(HttpResponse::error(
                403,
                "browser_control_revoked",
                "browser connection was revoked",
            ));
        }
        let preferences =
            self.state.preferences.read().map(|value| value.clone()).map_err(|_| {
                HttpResponse::error(500, "configuration_unavailable", "config lock")
            })?;
        if !preferences.enabled {
            return Err(HttpResponse::error(
                403,
                "browser_control_disabled",
                "browser control is disabled",
            ));
        }
        Ok(preferences)
    }

    fn require_enabled(&self) -> Result<(), HttpResponse> {
        self.current_preferences().map(|_| ())
    }
}

impl Clone for TaskApi {
    fn clone(&self) -> Self {
        Self { state: self.state.clone(), connection: self.connection.clone() }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MutationRequest {
    request_id: String,
    window_id: u64,
    pane_id: u64,
    identity: Value,
    #[serde(default)]
    text: String,
    prompt_id: Option<String>,
    option: Option<usize>,
}

fn parse_body<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, HttpResponse> {
    if body.len() > MAX_BODY_BYTES {
        return Err(HttpResponse::error(413, "request_too_large", "request exceeds 64 KiB"));
    }
    serde_json::from_slice(body)
        .map_err(|_| HttpResponse::error(400, "invalid_json", "invalid request body"))
}

async fn runtime_call(method: &str, params: Value) -> Result<Value, HttpResponse> {
    let method = method.to_owned();
    let response = tokio::task::spawn_blocking(move || {
        crate::runtime_api::request_once_bounded(&method, params, RUNTIME_TIMEOUT)
    })
    .await;
    match response {
        Ok(response) => runtime_result(response),
        Err(_) => {
            Err(HttpResponse::error(503, "runtime_worker_failed", "Runtime request worker failed"))
        },
    }
}

fn runtime_result(
    response: Result<ApiResponse, crate::runtime_api::ApiError>,
) -> Result<Value, HttpResponse> {
    match response {
        Ok(ApiResponse { ok: true, result: Some(result), .. }) => Ok(result),
        Ok(ApiResponse { ok: false, error: Some(error), .. }) => Err(HttpResponse::json(
            if error.code == "runtime_timeout" { 503 } else { 409 },
            json!({"error":{"code":error.code,"message":error.message}}),
        )),
        Ok(_) => Err(HttpResponse::error(
            502,
            "invalid_runtime_response",
            "Runtime returned an invalid response",
        )),
        Err(error) => Err(HttpResponse::json(
            // No complete Runtime response means dispatch may already have
            // happened, including an oversized or malformed response. Keep the
            // write receipt unknown rather than reporting a safe rejection.
            503,
            json!({"error":{"code":error.code,"message":error.message}}),
        )),
    }
}

fn receipt_json(receipt: Receipt) -> Value {
    serde_json::to_value(receipt).unwrap_or_else(|_| json!({"status":"unknown"}))
}

fn fallback_receipt(request_id: &str, status: &str) -> Receipt {
    Receipt {
        request_id: request_id.to_owned(),
        status: status.to_owned(),
        result: None,
        updated_at: super::now_secs(),
    }
}

fn project_is_current(preferences: &Preferences, project: &ProjectGrant) -> bool {
    preferences.enabled
        && preferences.project(&project.id).is_some_and(|current| current == project)
}

fn error_value(response: HttpResponse) -> Value {
    serde_json::from_slice(&response.body).unwrap_or_else(|_| json!({"code":"request_failed"}))
}

fn error_value_ref(response: &HttpResponse) -> Value {
    serde_json::from_slice(&response.body).unwrap_or_else(|_| json!({"code":"request_failed"}))
}

pub(crate) fn valid_request_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
        && matches!(bytes[14], b'1'..=b'8')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncertain_runtime_results_cannot_be_reported_as_safe_write_rejections() {
        use crate::runtime_api::ApiError;
        let failure = |code| {
            serde_json::from_value::<ApiResponse>(json!({
                "protocol":"pebrel-runtime", "version":1, "id":"test", "ok":false,
                "error":{"code":code,"message":"unconfirmed"}
            }))
            .unwrap()
        };
        let timeout = failure("runtime_timeout");
        assert_eq!(runtime_result(Ok(timeout)).err().unwrap().status, 503);
        for code in ["runtime_no_response", "runtime_response_too_large", "invalid_response"] {
            let response =
                runtime_result(Err(ApiError::new(code, "no complete response"))).err().unwrap();
            assert_eq!(response.status, 503, "{code}");
        }
        let rejected = failure("conversation_identity_changed");
        assert_eq!(runtime_result(Ok(rejected)).err().unwrap().status, 409);
    }

    #[test]
    fn request_ids_are_uuid_shaped() {
        assert!(valid_request_id("11111111-1111-4111-8111-111111111111"));
        assert!(!valid_request_id("11111111-1111-9111-8111-111111111111"));
        assert!(!valid_request_id("short"));
    }

    #[test]
    fn manifest_scopes_the_installed_app_to_the_current_device() {
        let response = HttpResponse::manifest("device/one");
        assert_eq!(response.content_type, "application/manifest+json");
        let manifest: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(manifest["start_url"], "/remote/device%2Fone/");
        assert_eq!(manifest["scope"], "/remote/device%2Fone/");
        assert_eq!(manifest["icons"][0]["src"], "/icon.svg");
    }

    #[test]
    fn pending_prompt_requires_the_project_grant_to_remain_current() {
        let directory = tempfile::tempdir().unwrap();
        let project = ProjectGrant::from_input(directory.path().to_str().unwrap(), None).unwrap();
        let mut preferences = Preferences {
            enabled: true,
            projects: vec![project.clone()],
            ..Preferences::default()
        };
        assert!(project_is_current(&preferences, &project));
        preferences.enabled = false;
        assert!(!project_is_current(&preferences, &project));
        preferences.enabled = true;
        preferences.projects.clear();
        assert!(!project_is_current(&preferences, &project));
    }

    #[test]
    fn disabling_browser_control_rejects_every_api_route_before_runtime_access() {
        let directory = tempfile::tempdir().unwrap();
        let preferences =
            Arc::new(RwLock::new(Preferences { enabled: true, ..Preferences::default() }));
        let receipts = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
        let api = TaskApi::new(preferences.clone(), receipts);
        assert!(api.require_enabled().is_ok());
        preferences.write().unwrap().enabled = false;
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        for (method, path) in [
            ("GET", "/api/v1/state"),
            ("GET", "/api/v1/receipt/11111111-1111-4111-8111-111111111111"),
            ("POST", "/api/v1/session/read"),
            ("POST", "/api/v1/session/new"),
            ("POST", "/api/v1/session/send"),
            ("POST", "/api/v1/session/choose"),
            ("POST", "/api/v1/session/stop"),
        ] {
            let response = runtime.block_on(api.respond("device", method, path, b"{}"));
            assert_eq!(response.status, 403, "{path}");
            let body: Value = serde_json::from_slice(&response.body).unwrap();
            assert_eq!(body["error"]["code"], "browser_control_disabled", "{path}");
        }
    }

    #[test]
    fn revoked_connections_cannot_use_the_new_pairings_enabled_preferences() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let directory = tempfile::tempdir().unwrap();
        let preferences =
            Arc::new(RwLock::new(Preferences { enabled: true, ..Preferences::default() }));
        let generation = Arc::new(AtomicU64::new(7));
        let api = TaskApi::new(
            preferences,
            ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap(),
        )
        .for_connection(generation.clone(), 7);
        assert!(api.require_enabled().is_ok());
        generation.store(8, Ordering::Release);
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let response = runtime.block_on(api.respond("old-device", "GET", "/api/v1/state", b""));
        assert_eq!(response.status, 403);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(body["error"]["code"], "browser_control_revoked");
    }
}
