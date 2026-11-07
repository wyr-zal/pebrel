//! Durable request receipts for phone writes.
//!
//! A request is made visible as `dispatching` before it can reach Runtime. If
//! Pebrel exits between that marker and the confirmed response, the next query
//! returns `unknown`; the same request ID is never delivered a second time.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::HashSet,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Receipt {
    pub(crate) request_id: String,
    pub(crate) status: String,
    pub(crate) result: Option<Value>,
    pub(crate) updated_at: u64,
}

pub(crate) enum Begin {
    New,
    Existing(Receipt),
    Conflict,
}

#[derive(Clone)]
pub(crate) struct ReceiptStore {
    connection: Arc<Mutex<Connection>>,
    active: Arc<Mutex<HashSet<(String, String)>>>,
}

impl ReceiptStore {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let connection = Connection::open(path).map_err(|error| error.to_string())?;
        connection
            .execute_batch(
                "
                PRAGMA journal_mode=WAL;
                CREATE TABLE IF NOT EXISTS receipts (
                    device_id TEXT NOT NULL,
                    request_id TEXT NOT NULL,
                    method TEXT NOT NULL,
                    payload_hash TEXT NOT NULL,
                    status TEXT NOT NULL,
                    result_json TEXT,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL,
                    PRIMARY KEY(device_id, request_id)
                );
                CREATE INDEX IF NOT EXISTS receipts_updated
                    ON receipts(updated_at);
                ",
            )
            .map_err(|error| error.to_string())?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            active: Arc::new(Mutex::new(HashSet::new())),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, String> {
        self.connection.lock().map_err(|_| "receipt database lock poisoned".to_owned())
    }

    fn is_active(&self, device_id: &str, request_id: &str) -> Result<bool, String> {
        self.active
            .lock()
            .map(|active| active.contains(&(device_id.to_owned(), request_id.to_owned())))
            .map_err(|_| "receipt activity lock poisoned".to_owned())
    }

    fn mark_active(&self, device_id: &str, request_id: &str) -> Result<(), String> {
        self.active
            .lock()
            .map_err(|_| "receipt activity lock poisoned".to_owned())?
            .insert((device_id.to_owned(), request_id.to_owned()));
        Ok(())
    }

    fn mark_finished(&self, device_id: &str, request_id: &str) -> Result<(), String> {
        self.active
            .lock()
            .map_err(|_| "receipt activity lock poisoned".to_owned())?
            .remove(&(device_id.to_owned(), request_id.to_owned()));
        Ok(())
    }

    pub(crate) fn begin(
        &self,
        device_id: &str,
        request_id: &str,
        method: &str,
        payload_hash: &str,
    ) -> Result<Begin, String> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction().map_err(|error| error.to_string())?;
        let now = now_i64();
        let inserted = transaction
            .execute(
                "
                INSERT OR IGNORE INTO receipts
                    (device_id, request_id, method, payload_hash, status, result_json,
                     created_at, updated_at)
                VALUES (?1, ?2, ?3, ?4, 'prepared', NULL, ?5, ?5)
                ",
                params![device_id, request_id, method, payload_hash, now],
            )
            .map_err(|error| error.to_string())?
            > 0;
        let row = transaction
            .query_row(
                "
                SELECT method, payload_hash, status, result_json, updated_at
                FROM receipts
                WHERE device_id = ?1 AND request_id = ?2
                ",
                params![device_id, request_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;
        if row.0 != method || row.1 != payload_hash {
            return Ok(Begin::Conflict);
        }
        if inserted {
            self.mark_active(device_id, request_id)?;
            return Ok(Begin::New);
        }
        let mut status = row.2;
        if is_in_progress(&status) && !self.is_active(device_id, request_id)? {
            status = String::from("unknown");
            connection
                .execute(
                    "UPDATE receipts SET status = 'unknown', updated_at = ?3
                     WHERE device_id = ?1 AND request_id = ?2",
                    params![device_id, request_id, now_i64()],
                )
                .map_err(|error| error.to_string())?;
        }
        Ok(Begin::Existing(Receipt {
            request_id: request_id.to_owned(),
            status,
            result: row.3.and_then(|value| serde_json::from_str(&value).ok()),
            updated_at: u64::try_from(row.4).unwrap_or(0),
        }))
    }

    pub(crate) fn dispatch(&self, device_id: &str, request_id: &str) -> Result<(), String> {
        self.update(device_id, request_id, "dispatching", None, Some("prepared"))
    }

    pub(crate) fn complete(
        &self,
        device_id: &str,
        request_id: &str,
        status: &str,
        result: Option<Value>,
    ) -> Result<Receipt, String> {
        let connection = self.lock()?;
        let timestamp = now_i64();
        let encoded = result
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| error.to_string())?;
        connection
            .execute(
                "
                UPDATE receipts
                SET status = ?3, result_json = ?4, updated_at = ?5
                WHERE device_id = ?1 AND request_id = ?2
                ",
                params![device_id, request_id, status, encoded, timestamp],
            )
            .map_err(|error| error.to_string())?;
        if is_final(status) {
            self.mark_finished(device_id, request_id)?;
        }
        Ok(Receipt {
            request_id: request_id.to_owned(),
            status: status.to_owned(),
            result,
            updated_at: u64::try_from(timestamp).unwrap_or(0),
        })
    }

    pub(crate) fn get(&self, device_id: &str, request_id: &str) -> Result<Option<Receipt>, String> {
        let connection = self.lock()?;
        let receipt = connection
            .query_row(
                "
                SELECT status, result_json, updated_at
                FROM receipts
                WHERE device_id = ?1 AND request_id = ?2
                ",
                params![device_id, request_id],
                |row| {
                    Ok(Receipt {
                        request_id: request_id.to_owned(),
                        status: row.get(0)?,
                        result: row
                            .get::<_, Option<String>>(1)?
                            .and_then(|value| serde_json::from_str(&value).ok()),
                        updated_at: u64::try_from(row.get::<_, i64>(2)?).unwrap_or(0),
                    })
                },
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let Some(mut receipt) = receipt else {
            return Ok(None);
        };
        if is_in_progress(&receipt.status) && !self.is_active(device_id, request_id)? {
            receipt.status = String::from("unknown");
            receipt.updated_at = u64::try_from(now_i64()).unwrap_or(0);
            connection
                .execute(
                    "UPDATE receipts SET status = 'unknown', updated_at = ?3
                     WHERE device_id = ?1 AND request_id = ?2",
                    params![device_id, request_id, now_i64()],
                )
                .map_err(|error| error.to_string())?;
        }
        Ok(Some(receipt))
    }

    fn update(
        &self,
        device_id: &str,
        request_id: &str,
        status: &str,
        result: Option<Value>,
        expected: Option<&str>,
    ) -> Result<(), String> {
        let connection = self.lock()?;
        let timestamp = now_i64();
        let encoded = result
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| error.to_string())?;
        if let Some(expected) = expected {
            connection
                .execute(
                    "
                    UPDATE receipts
                    SET status = ?3, result_json = ?4, updated_at = ?5
                    WHERE device_id = ?1 AND request_id = ?2 AND status = ?6
                    ",
                    params![device_id, request_id, status, encoded, timestamp, expected],
                )
                .map_err(|error| error.to_string())?;
        } else {
            connection
                .execute(
                    "
                    UPDATE receipts
                    SET status = ?3, result_json = ?4, updated_at = ?5
                    WHERE device_id = ?1 AND request_id = ?2
                    ",
                    params![device_id, request_id, status, encoded, timestamp],
                )
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

fn is_in_progress(status: &str) -> bool {
    matches!(status, "prepared" | "dispatching" | "launching")
}

fn is_final(status: &str) -> bool {
    matches!(status, "accepted" | "rejected" | "failed" | "unknown")
}

pub(crate) fn payload_hash(value: &Value) -> String {
    use sha2::{Digest as _, Sha256};
    let canonical = canonical_json(value);
    Sha256::digest(canonical.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).unwrap_or_default(),
        Value::Array(values) => {
            format!("[{}]", values.iter().map(canonical_json).collect::<Vec<_>>().join(","))
        },
        Value::Object(values) => {
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            format!(
                "{{{}}}",
                entries
                    .into_iter()
                    .map(|(key, value)| {
                        format!(
                            "{}:{}",
                            serde_json::to_string(key).unwrap_or_default(),
                            canonical_json(value)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            )
        },
    }
}

fn now_i64() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| i64::try_from(value.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_payload_returns_the_original_and_conflicts_do_not_replace_it() {
        let directory = tempfile::tempdir().unwrap();
        let store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
        let hash = payload_hash(&serde_json::json!({"b": 2, "a": 1}));
        assert!(matches!(store.begin("device", "request", "send", &hash).unwrap(), Begin::New));
        store.dispatch("device", "request").unwrap();
        let accepted = store
            .complete("device", "request", "accepted", Some(serde_json::json!({"ok": true})))
            .unwrap();
        assert_eq!(accepted.status, "accepted");
        assert!(matches!(
            store.begin("device", "request", "send", &hash).unwrap(),
            Begin::Existing(Receipt { ref status, .. }) if status == "accepted"
        ));
        assert!(matches!(
            store.begin("device", "request", "send", &payload_hash(&serde_json::json!(1))).unwrap(),
            Begin::Conflict
        ));
    }

    #[test]
    fn canonical_hash_ignores_object_key_order() {
        assert_eq!(
            payload_hash(&serde_json::json!({"a": 1, "b": [2, 3]})),
            payload_hash(&serde_json::json!({"b": [2, 3], "a": 1}))
        );
    }

    #[test]
    fn a_dispatched_receipt_from_a_previous_process_becomes_unknown() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("receipts.sqlite");
        {
            let store = ReceiptStore::open(&path).unwrap();
            let hash = payload_hash(&serde_json::json!({"text": "once"}));
            assert!(matches!(store.begin("device", "request", "send", &hash).unwrap(), Begin::New));
            store.dispatch("device", "request").unwrap();
        }

        let reopened = ReceiptStore::open(&path).unwrap();
        let receipt = reopened.get("device", "request").unwrap().unwrap();
        assert_eq!(receipt.status, "unknown");
        assert!(matches!(
            reopened
                .begin(
                    "device",
                    "request",
                    "send",
                    &payload_hash(&serde_json::json!({"text": "once"})),
                )
                .unwrap(),
            Begin::Existing(Receipt { ref status, .. }) if status == "unknown"
        ));
    }

    #[test]
    fn final_receipts_release_in_process_activity() {
        let directory = tempfile::tempdir().unwrap();
        let store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
        let hash = payload_hash(&serde_json::json!({"text": "done"}));
        assert!(matches!(store.begin("device", "request", "send", &hash).unwrap(), Begin::New));
        store.dispatch("device", "request").unwrap();
        store
            .complete("device", "request", "accepted", Some(serde_json::json!({"ok": true})))
            .unwrap();
        assert_eq!(store.get("device", "request").unwrap().unwrap().status, "accepted");
    }

    #[test]
    fn request_ids_are_scoped_to_the_authenticated_device() {
        let directory = tempfile::tempdir().unwrap();
        let store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
        let hash = payload_hash(&serde_json::json!({"text": "phone task"}));
        assert!(matches!(store.begin("device-a", "request", "send", &hash).unwrap(), Begin::New));
        store
            .complete("device-a", "request", "accepted", Some(serde_json::json!({"ok": true})))
            .unwrap();
        assert!(matches!(store.begin("device-b", "request", "send", &hash).unwrap(), Begin::New));
        assert_eq!(store.get("device-b", "request").ok().flatten().unwrap().status, "prepared");
    }
}
