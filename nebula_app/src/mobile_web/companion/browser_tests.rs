//! Opt-in loopback fixture: API disabled, no desktop Runtime or real Agent access.

use super::super::{
    admission,
    config::{DeviceCredentials, Preferences},
    receipts::ReceiptStore,
};
use super::*;
use std::{path::PathBuf, sync::RwLock, time::Instant};

#[test]
#[ignore = "requires the local Suite Service Worker browser fixture"]
fn suite_service_worker_pairs_rotates_key_and_reconnects() {
    let relay = std::env::var("PEBREL_DSH_TEST_RELAY_URL").expect("loopback fixture URL");
    assert!(relay.starts_with("http://127.0.0.1:"));
    super::super::config::validate_relay_url(&relay).unwrap();
    let directory =
        PathBuf::from(std::env::var("PEBREL_DSH_TEST_DIRECTORY").expect("fixture directory"));
    let bootstrap = std::env::var("PEBREL_DSH_TEST_BOOTSTRAP_KEY").expect("ephemeral fixture key");
    let trusted = std::env::var("PEBREL_DSH_TEST_MASTER_KEY").expect("ephemeral fixture key");
    let database = tempfile::tempdir().unwrap();
    // The endpoint cannot call Runtime even if the test browser sends API requests.
    let preferences = Arc::new(RwLock::new(Preferences::default()));
    let api = TaskApi::new(
        preferences,
        ReceiptStore::open(&database.path().join("receipts.sqlite")).unwrap(),
    );
    let credentials = DeviceCredentials {
        device_id: String::from("dev_fixture"),
        device_secret: String::from("fixture-only"),
        device_token: String::from("fixture-only"),
        e2ee_master_key: trusted,
    };
    let generation = Arc::new(AtomicU64::new(1));
    admission::configure(
        1,
        Some(admission::Bootstrap {
            key: bootstrap,
            expires_at: super::super::now_millis() + 90_000,
        }),
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let worker = runtime.spawn(run(
        relay,
        credentials,
        api,
        Arc::new(Mutex::new(Status::Stopped)),
        generation.clone(),
        1,
    ));
    let deadline = Instant::now() + Duration::from_secs(100);
    let mut approved = String::new();
    while Instant::now() < deadline {
        if let Ok(id) = std::fs::read_to_string(directory.join("approve.txt")) {
            if id != approved {
                admission::decide(&id, true).unwrap();
                approved = id;
            }
        }
        if directory.join("done.json").is_file() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    generation.fetch_add(1, Ordering::AcqRel);
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(3), worker).await.unwrap().unwrap();
    });
    let result: Value = serde_json::from_slice(
        &std::fs::read(directory.join("done.json")).expect("browser fixture completed"),
    )
    .unwrap();
    assert_eq!(result["passed"], true, "browser fixture failed");
    assert!(!approved.is_empty(), "fixture never approved a browser");
    admission::configure(2, None);
}
