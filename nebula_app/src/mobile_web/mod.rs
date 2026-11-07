//! Browser-based remote control over the DSH Mobile Suite sealed tunnel.
//!
//! Pairing, login, the browser application shell, Service Worker, and the
//! `dsh-e2ee-v1` transport come from DSH Mobile Suite. Pebrel supplies the
//! outbound Companion, project-scoped task API, and mobile task page.

mod admission;
mod companion;
mod config;
mod protocol;
mod receipts;
mod storage;
mod task_api;
mod util;

pub(crate) use config::Preferences;
use config::{DeviceCredentials, StoredConfig};
use receipts::ReceiptStore;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use task_api::TaskApi;
use util::{now_millis, now_secs, url_encode};

const PAIR_POLL_INTERVAL: Duration = Duration::from_secs(2);
const PAIR_MAX_BODY_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "message")]
pub(crate) enum Status {
    Stopped,
    Pairing,
    Connecting,
    Connected,
    Offline,
    AuthFailed,
    Failed(String),
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PairingView {
    pub(crate) code: String,
    pub(crate) expires_at: u64,
    pub(crate) qr_payload: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    pub(crate) preferences: Preferences,
    pub(crate) paired: bool,
    pub(crate) device_id: Option<String>,
    pub(crate) status: Status,
    pub(crate) pairing: Option<PairingView>,
    pub(crate) browser_approval: Option<admission::PendingView>,
}

#[derive(Clone)]
struct PairingSession {
    code: String,
    device_id: String,
    device_secret: String,
    expires_at: u64,
    master_key: String,
    qr_payload: String,
}

impl PairingSession {
    fn view(&self) -> PairingView {
        PairingView {
            code: self.code.clone(),
            expires_at: self.expires_at,
            qr_payload: self.qr_payload.clone(),
        }
    }
}

struct Manager {
    initialized: bool,
    config: StoredConfig,
    preferences: Arc<std::sync::RwLock<Preferences>>,
    api: Option<TaskApi>,
    status: Arc<Mutex<Status>>,
    generation: Arc<AtomicU64>,
    companion: Option<tokio::task::JoinHandle<()>>,
    pairing: Option<PairingSession>,
    pairing_task: Option<tokio::task::JoinHandle<()>>,
    enrollment: Option<admission::Bootstrap>,
}

impl Default for Manager {
    fn default() -> Self {
        Self {
            initialized: false,
            config: StoredConfig::default(),
            preferences: Arc::new(std::sync::RwLock::new(Preferences::default())),
            api: None,
            status: Arc::new(Mutex::new(Status::Stopped)),
            generation: Arc::new(AtomicU64::new(0)),
            companion: None,
            pairing: None,
            pairing_task: None,
            enrollment: None,
        }
    }
}

fn manager() -> &'static Mutex<Manager> {
    static MANAGER: OnceLock<Mutex<Manager>> = OnceLock::new();
    MANAGER.get_or_init(|| Mutex::new(Manager::default()))
}

fn runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    static RUNTIME: OnceLock<Option<tokio::runtime::Runtime>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("pebrel-mobile-web")
                .enable_all()
                .build()
                .ok()
        })
        .as_ref()
        .ok_or_else(|| String::from("browser remote runtime is unavailable"))
}

fn lock_manager() -> Result<MutexGuard<'static, Manager>, String> {
    manager().lock().map_err(|_| String::from("browser remote manager lock is poisoned"))
}

fn initialize(manager: &mut Manager) -> Result<(), String> {
    if manager.initialized {
        return Ok(());
    }
    let config = StoredConfig::load()?;
    let receipts_path = config::state_dir().join("receipts.sqlite");
    let receipts = ReceiptStore::open(&receipts_path)?;
    let preferences = Arc::new(std::sync::RwLock::new(config.preferences.clone()));
    manager.api = Some(TaskApi::new(preferences.clone(), receipts));
    manager.preferences = preferences;
    manager.config = config;
    manager.initialized = true;
    Ok(())
}

pub(crate) fn resume_saved() {
    if let Err(error) = resume_saved_inner() {
        log::warn!("browser remote startup skipped: {error}");
    }
}

fn resume_saved_inner() -> Result<(), String> {
    let mut manager = lock_manager()?;
    initialize(&mut manager)?;
    if manager.config.preferences.enabled && manager.config.device.is_some() {
        start_companion(&mut manager)?;
    }
    Ok(())
}

pub(crate) fn shutdown() {
    if let Ok(mut manager) = lock_manager() {
        stop_companion(&mut manager);
        cancel_pairing(&mut manager);
    }
}

pub(crate) fn snapshot() -> Result<Snapshot, String> {
    let mut manager = lock_manager()?;
    initialize(&mut manager)?;
    Ok(snapshot_locked(&manager))
}

pub(crate) fn save_preferences(mut preferences: Preferences) -> Result<Snapshot, String> {
    let mut manager = lock_manager()?;
    initialize(&mut manager)?;
    preferences.relay_url = config::normalized_relay_url(&preferences.relay_url)?;
    preferences.device_name = String::from(preferences.device_name.trim());
    preferences.validate()?;
    if preferences.enabled && preferences.relay_url.trim().is_empty() {
        return Err(String::from("Relay URL is required when browser control is enabled"));
    }
    let relay_changed =
        preferences.relay_url != manager.config.preferences.relay_url.trim().trim_end_matches('/');
    let enabled_changed = preferences.enabled != manager.config.preferences.enabled;
    let previous_device = relay_changed
        .then(|| (manager.config.preferences.relay_url.clone(), manager.config.device.clone()));
    if relay_changed || !preferences.enabled {
        cancel_pairing(&mut manager);
    }
    let mut candidate = manager.config.clone();
    candidate.preferences = preferences;
    if relay_changed {
        candidate.device = None;
    }
    publish_config(&mut manager, candidate)?;
    if let Some((relay, Some(credentials))) = previous_device {
        unbind_credentials(&relay, &credentials)?;
    }
    if enabled_changed || relay_changed || manager.companion.is_none() {
        if manager.config.preferences.enabled
            && manager.config.device.is_some()
            && manager.companion.is_none()
        {
            start_companion(&mut manager)?;
        } else if !manager.config.preferences.enabled || relay_changed {
            stop_companion(&mut manager);
        }
    }
    Ok(snapshot_locked(&manager))
}

pub(crate) fn add_project(path: &str, shell_id: Option<String>) -> Result<Snapshot, String> {
    let mut manager = lock_manager()?;
    initialize(&mut manager)?;
    let mut candidate = manager.config.clone();
    candidate.preferences.upsert_project(path, shell_id)?;
    publish_config(&mut manager, candidate)?;
    Ok(snapshot_locked(&manager))
}

pub(crate) fn remove_project(id: &str) -> Result<Snapshot, String> {
    let mut manager = lock_manager()?;
    initialize(&mut manager)?;
    let mut candidate = manager.config.clone();
    candidate.preferences.remove_project(id);
    publish_config(&mut manager, candidate)?;
    if manager.config.preferences.enabled
        && manager.config.device.is_some()
        && manager.companion.is_none()
    {
        start_companion(&mut manager)?;
    }
    Ok(snapshot_locked(&manager))
}

pub(crate) fn start_pairing() -> Result<Snapshot, String> {
    let mut manager = lock_manager()?;
    initialize(&mut manager)?;
    if !manager.config.preferences.enabled {
        return Err(String::from("Enable browser control before pairing"));
    }
    if manager.config.device.is_some() {
        return Ok(snapshot_locked(&manager));
    }
    let relay = String::from(manager.config.preferences.relay_url.trim().trim_end_matches('/'));
    config::validate_relay_url(&relay)?;
    if relay.is_empty() {
        return Err(String::from("Relay URL is required before pairing"));
    }
    cancel_pairing(&mut manager);
    let response = post_json(&format!("{relay}/pair/session"), &json!({}), None)?;
    if response.status != 201 {
        return Err(format!("Relay rejected pairing session ({})", response.status));
    }
    let code = String::from(
        response.value["code"]
            .as_str()
            .filter(|value| value.len() == 6 && value.bytes().all(|byte| byte.is_ascii_digit()))
            .ok_or_else(|| String::from("Relay returned an invalid pairing code"))?,
    );
    let device_id = required_string(&response.value, "deviceId")?;
    let device_secret = required_string(&response.value, "deviceSecret")?;
    let expires_at = response.value["expiresAt"]
        .as_u64()
        .ok_or_else(|| String::from("Relay returned an invalid pairing expiry"))?;
    let master_key = protocol::random_key()?;
    let qr_payload = format!("{relay}/app/#/pair?code={code}&key={master_key}");
    let session =
        PairingSession { code, device_id, device_secret, expires_at, master_key, qr_payload };
    let poll_session = session.clone();
    manager.pairing_task = Some(runtime()?.spawn(poll_pairing(poll_session)));
    manager.pairing = Some(session);
    set_status(&manager.status, Status::Pairing);
    Ok(snapshot_locked(&manager))
}

pub(crate) fn cancel_pairing_request() -> Result<Snapshot, String> {
    let mut manager = lock_manager()?;
    initialize(&mut manager)?;
    cancel_pairing(&mut manager);
    Ok(snapshot_locked(&manager))
}

pub(crate) fn decide_browser(id: &str, approve: bool) -> Result<Snapshot, String> {
    let manager = lock_manager()?;
    if !manager.config.preferences.enabled {
        return Err(String::from("browser control is disabled"));
    }
    admission::decide(id, approve)?;
    Ok(snapshot_locked(&manager))
}

pub(crate) fn revoke_device() -> Result<Snapshot, String> {
    let mut manager = lock_manager()?;
    initialize(&mut manager)?;
    cancel_pairing(&mut manager);
    let Some(credentials) = manager.config.device.clone() else {
        return Ok(snapshot_locked(&manager));
    };
    let relay = manager.config.preferences.relay_url.trim().trim_end_matches('/').to_owned();
    stop_companion(&mut manager);
    let mut candidate = manager.config.clone();
    candidate.device = None;
    publish_config(&mut manager, candidate)?;
    unbind_credentials(&relay, &credentials)?;
    Ok(snapshot_locked(&manager))
}

fn snapshot_locked(manager: &Manager) -> Snapshot {
    let status = manager
        .status
        .lock()
        .map(|value| value.clone())
        .unwrap_or_else(|_| Status::Failed(String::from("status lock poisoned")));
    let credentials = manager.config.device.as_ref();
    Snapshot {
        preferences: manager.config.preferences.clone(),
        paired: credentials.is_some(),
        device_id: credentials.map(|value| value.device_id.clone()),
        status,
        pairing: manager.pairing.as_ref().map(PairingSession::view),
        browser_approval: admission::pending(),
    }
}

fn start_companion(manager: &mut Manager) -> Result<(), String> {
    if !manager.config.preferences.enabled {
        stop_companion(manager);
        return Ok(());
    }
    let Some(credentials) = manager.config.device.clone() else {
        return Ok(());
    };
    let Some(api) = manager.api.clone() else {
        return Err(String::from("task API is unavailable"));
    };
    let relay = manager.config.preferences.relay_url.clone();
    let runtime = runtime()?;
    let generation = {
        let mut status = manager.status.lock().map_err(|_| String::from("status lock poisoned"))?;
        let generation = manager.generation.fetch_add(1, Ordering::AcqRel) + 1;
        *status = Status::Connecting;
        generation
    };
    drop(manager.companion.take());
    admission::configure(generation, manager.enrollment.take());
    manager.companion = Some(runtime.spawn(companion::run(
        relay,
        credentials,
        api,
        manager.status.clone(),
        manager.generation.clone(),
        generation,
    )));
    Ok(())
}

fn stop_companion(manager: &mut Manager) {
    if let Ok(mut status) = manager.status.lock() {
        manager.generation.fetch_add(1, Ordering::AcqRel);
        *status = Status::Stopped;
    }
    drop(manager.companion.take());
    manager.enrollment = None;
    admission::configure(manager.generation.load(Ordering::Acquire), None);
}

fn cancel_pairing(manager: &mut Manager) {
    manager.pairing = None;
    if let Some(task) = manager.pairing_task.take() {
        task.abort();
    }
    if manager.config.device.is_none()
        && matches!(manager.status.lock().map(|status| status.clone()), Ok(Status::Pairing))
    {
        set_status(&manager.status, Status::Stopped);
    }
}

fn set_status(status: &Arc<Mutex<Status>>, next: Status) {
    if let Ok(mut status) = status.lock() {
        *status = next;
    }
}

async fn poll_pairing(session: PairingSession) {
    loop {
        if now_millis() >= session.expires_at {
            finish_pairing_error(&session.code, String::from("Pairing code expired"));
            return;
        }
        let active = lock_manager().ok().is_some_and(|manager| {
            manager.pairing.as_ref().is_some_and(|current| current.code == session.code)
        });
        if !active {
            return;
        }
        let relay = lock_manager().ok().map(|manager| {
            String::from(manager.config.preferences.relay_url.trim_end_matches('/'))
        });
        let Some(relay) = relay.filter(|value| !value.is_empty()) else {
            finish_pairing_error(&session.code, String::from("Relay URL is unavailable"));
            return;
        };
        let request = json!({
            "deviceId": session.device_id,
            "deviceSecret": session.device_secret,
            "deviceName": lock_manager()
                .ok()
                .map(|manager| manager.config.preferences.device_name.clone())
                .unwrap_or_else(|| String::from("Pebrel Computer")),
        });
        let result = tokio::task::spawn_blocking(move || {
            post_json(&format!("{relay}/pair/confirm"), &request, None)
        })
        .await;
        match result {
            Ok(Ok(response)) if response.status == 200 => {
                let token = match required_string(&response.value, "deviceToken") {
                    Ok(value) => value,
                    Err(error) => {
                        finish_pairing_error(&session.code, error);
                        return;
                    },
                };
                if finish_pairing_success(&session, token).is_err() {
                    return;
                }
                return;
            },
            Ok(Ok(response)) if response.status == 202 => {},
            Ok(Ok(response)) if matches!(response.status, 400 | 401 | 403 | 409 | 410) => {
                finish_pairing_error(
                    &session.code,
                    format!("Relay rejected pairing confirmation ({})", response.status),
                );
                return;
            },
            _ => {},
        }
        tokio::time::sleep(PAIR_POLL_INTERVAL).await;
    }
}

fn finish_pairing_success(session: &PairingSession, token: String) -> Result<(), String> {
    let mut manager = lock_manager()?;
    if !manager.pairing.as_ref().is_some_and(|current| current.code == session.code) {
        return Ok(());
    }
    if !manager.config.preferences.enabled {
        manager.pairing = None;
        manager.pairing_task = None;
        set_status(&manager.status, Status::Stopped);
        return Err(String::from("Browser control was disabled during pairing"));
    }
    let mut candidate = manager.config.clone();
    candidate.device = Some(DeviceCredentials {
        device_id: session.device_id.clone(),
        device_secret: session.device_secret.clone(),
        device_token: token,
        // The QR key never becomes the long-lived browser key.
        e2ee_master_key: protocol::random_key()?,
    });
    publish_config(&mut manager, candidate)?;
    manager.enrollment = Some(admission::Bootstrap {
        key: session.master_key.clone(),
        expires_at: session.expires_at,
    });
    manager.pairing = None;
    manager.pairing_task = None;
    start_companion(&mut manager)
}

fn publish_config(manager: &mut Manager, candidate: StoredConfig) -> Result<(), String> {
    let old = &manager.config;
    let contracts_access = !candidate.preferences.enabled
        || (old.device.is_some() && candidate.device.is_none())
        || old
            .preferences
            .projects
            .iter()
            .any(|project| !candidate.preferences.projects.contains(project));
    // A failed save must never leave a requested permission reduction usable
    // by the old connection. Keep the old visible configuration for retry.
    if contracts_access {
        stop_companion(manager);
    }
    // Holding this lock also prevents requests from seeing a partially published grant set.
    let mut preferences = manager
        .preferences
        .write()
        .map_err(|_| String::from("project authorization lock poisoned"))?;
    candidate.store()?;
    *preferences = candidate.preferences.clone();
    manager.config = candidate;
    Ok(())
}

fn unbind_credentials(relay: &str, credentials: &DeviceCredentials) -> Result<(), String> {
    let relay = relay.trim().trim_end_matches('/');
    let response = post_json(
        &format!("{relay}/device-management/{}/unbind", url_encode(&credentials.device_id)),
        &json!({}),
        Some(&credentials.device_token),
    )?;
    if matches!(response.status, 200 | 401 | 404) {
        Ok(())
    } else {
        Err(format!("Relay rejected device revocation ({})", response.status))
    }
}

fn finish_pairing_error(code: &str, message: String) {
    let Ok(mut manager) = lock_manager() else {
        return;
    };
    if !manager.pairing.as_ref().is_some_and(|current| current.code == code) {
        return;
    }
    manager.pairing = None;
    manager.pairing_task = None;
    set_status(&manager.status, Status::Failed(message));
}

struct HttpReply {
    status: u16,
    value: Value,
}

fn post_json(url: &str, body: &Value, device_token: Option<&str>) -> Result<HttpReply, String> {
    let agent = ureq::config::Config::builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .build()
        .new_agent();
    let payload = serde_json::to_vec(body).map_err(|error| error.to_string())?;
    let mut request = agent.post(url).header("content-type", "application/json");
    if let Some(token) = device_token {
        request = request.header("authorization", &format!("Device {token}"));
    }
    let mut response = request.send(payload).map_err(|error| error.to_string())?;
    let status = response.status().as_u16();
    let bytes = response
        .body_mut()
        .with_config()
        .limit(PAIR_MAX_BODY_BYTES)
        .read_to_vec()
        .map_err(|error| error.to_string())?;
    let value = if bytes.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&bytes).map_err(|_| String::from("Relay returned invalid JSON"))?
    };
    Ok(HttpReply { status, value })
}

fn required_string(value: &Value, key: &str) -> Result<String, String> {
    value[key]
        .as_str()
        .filter(|value| !value.is_empty() && value.len() <= 512)
        .map(str::to_owned)
        .ok_or_else(|| format!("Relay response is missing {key}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_links_encode_device_ids_without_exposing_extra_credentials() {
        assert_eq!(url_encode("dev_a/b"), "dev_a%2Fb");
    }

    #[test]
    fn relay_string_fields_are_bounded() {
        assert!(required_string(&json!({"x":"ok"}), "x").is_ok());
        assert!(required_string(&json!({"x":""}), "x").is_err());
        let oversized = "a".repeat(513);
        assert!(required_string(&json!({"x":oversized}), "x").is_err());
    }
}
