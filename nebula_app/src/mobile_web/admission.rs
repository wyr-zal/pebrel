//! A pairing QR is a short-lived bootstrap, consumed by one desktop-approved browser.

use super::{
    now_millis,
    protocol::{ClientHello, accept_client_hello},
};
use serde::Serialize;
use std::sync::{Mutex, OnceLock};

#[derive(Clone)]
pub(super) struct Bootstrap {
    pub(super) key: String,
    pub(super) expires_at: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PendingView {
    pub(crate) id: String,
    pub(crate) code: String,
    pub(crate) expires_at: u64,
}

struct Pending {
    hello: ClientHello,
    expires_at: u64,
    approved: bool,
}

#[derive(Default)]
struct Admission {
    generation: u64,
    bootstrap: Option<Bootstrap>,
    pending: Option<Pending>,
}

fn state() -> &'static Mutex<Admission> {
    static STATE: OnceLock<Mutex<Admission>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(Admission::default()))
}

pub(super) fn configure(generation: u64, bootstrap: Option<Bootstrap>) {
    if let Ok(mut state) = state().lock() {
        *state = Admission { generation, bootstrap, pending: None };
    }
}

impl Admission {
    fn expire(&mut self) {
        let now = now_millis();
        if self.bootstrap.as_ref().is_some_and(|bootstrap| bootstrap.expires_at <= now) {
            self.bootstrap = None;
        }
        if self.bootstrap.is_none()
            || self.pending.as_ref().is_some_and(|pending| pending.expires_at <= now)
        {
            self.pending = None;
        }
    }

    fn offer(&mut self, generation: u64, hello: ClientHello) -> bool {
        self.expire();
        if self.generation != generation {
            return false;
        }
        let Some(bootstrap) = self.bootstrap.as_ref() else { return false };
        if hello.access_session_id.len() < 6
            || hello.access_session_id.len() > 256
            || accept_client_hello(&bootstrap.key, hello.clone()).is_err()
        {
            return false;
        }
        if let Some(pending) = self.pending.as_ref() {
            return pending.hello == hello;
        }
        let expires_at = bootstrap.expires_at.min(now_millis() + 120_000);
        self.pending = Some(Pending { hello, expires_at, approved: false });
        true
    }

    fn take_approved(&mut self, generation: u64) -> Option<(ClientHello, String)> {
        self.expire();
        if self.generation != generation || !self.pending.as_ref()?.approved {
            return None;
        }
        let pending = self.pending.take()?;
        let bootstrap = self.bootstrap.take()?;
        Some((pending.hello, bootstrap.key))
    }

    fn client_closed(&mut self, generation: u64, id: &str) {
        if self.generation == generation
            && self.pending.as_ref().is_some_and(|pending| pending.hello.access_session_id == id)
        {
            self.pending = None;
        }
    }
}

pub(super) fn offer(generation: u64, hello: ClientHello) -> bool {
    state().lock().is_ok_and(|mut state| state.offer(generation, hello))
}

pub(super) fn take_approved(generation: u64) -> Option<(ClientHello, String)> {
    state().lock().ok()?.take_approved(generation)
}

pub(super) fn clear_pending(generation: u64) {
    if let Ok(mut state) = state().lock() {
        if state.generation == generation {
            state.pending = None;
        }
    }
}

pub(super) fn client_closed(generation: u64, id: &str) {
    if let Ok(mut state) = state().lock() {
        state.client_closed(generation, id);
    }
}

pub(super) fn pending() -> Option<PendingView> {
    let mut state = state().lock().ok()?;
    state.expire();
    let pending = state.pending.as_ref()?;
    let id = pending.hello.access_session_id.clone();
    Some(PendingView {
        code: id.chars().rev().take(6).collect::<String>().chars().rev().collect(),
        id,
        expires_at: pending.expires_at,
    })
}

pub(super) fn decide(id: &str, approve: bool) -> Result<(), String> {
    let mut state = state().lock().map_err(|_| String::from("browser approval lock poisoned"))?;
    state.expire();
    let pending = state
        .pending
        .as_mut()
        .filter(|pending| pending.hello.access_session_id == id)
        .ok_or_else(|| String::from("browser request expired or changed"))?;
    if approve {
        pending.approved = true;
    } else {
        state.pending = None;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bootstrap_requires_approval_and_is_consumed_once() {
        let key = super::super::protocol::random_key().unwrap();
        let mut state = Admission {
            generation: 7,
            bootstrap: Some(Bootstrap { key: key.clone(), expires_at: now_millis() + 10_000 }),
            pending: None,
        };
        // The cryptographic proof is tested by protocol interoperability. Here
        // exercise the authorization state without a global manager or credentials.
        let hello: ClientHello = serde_json::from_value(
            json!({"accessSessionId":"access-123456", "clientRandomB64":"", "clientProofB64":""}),
        )
        .unwrap();
        assert!(!state.offer(7, hello.clone()));
        state.pending = Some(Pending {
            hello: hello.clone(),
            expires_at: now_millis() + 10_000,
            approved: false,
        });
        assert!(state.take_approved(7).is_none());
        state.client_closed(6, "access-123456");
        assert!(state.pending.is_some());
        state.client_closed(7, "access-123456");
        assert!(state.pending.is_none());
        assert!(state.bootstrap.is_some());
        state.pending = Some(Pending {
            hello: hello.clone(),
            expires_at: now_millis() + 10_000,
            approved: false,
        });
        state.pending.as_mut().unwrap().approved = true;
        assert!(state.take_approved(6).is_none());
        assert_eq!(state.take_approved(7).unwrap().0, hello);
        assert!(state.take_approved(7).is_none());
        assert!(state.bootstrap.is_none());
    }
}
