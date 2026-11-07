//! Receipt-backed session controls that need explicit runtime evidence.

use super::{
    access::{pane, pane_matches_project},
    *,
};

impl TaskApi {
    pub(super) async fn stop(&self, device_id: &str, body: &[u8]) -> HttpResponse {
        let request: MutationRequest = match parse_body(body) {
            Ok(request) => request,
            Err(response) => return response,
        };
        if request.identity["epoch"].as_u64().is_none()
            || !valid_request_id(&request.request_id)
            || !request.text.is_empty()
            || request.prompt_id.is_some()
            || request.option.is_some()
        {
            return HttpResponse::error(400, "invalid_request", "invalid stop request");
        }
        let (identity, baseline_pane, project) = match self
            .authorize_identity_with_pane(request.window_id, request.pane_id, &request.identity)
            .await
        {
            Ok(value) => value,
            Err(response) => return response,
        };
        if !self.project_is_current(&project) {
            return HttpResponse::error(
                403,
                "project_not_authorized",
                "project authorization changed",
            );
        }
        let baseline_seq = baseline_pane["state_change_seq"].as_u64().unwrap_or(0);
        let before_state = baseline_pane["task_state"].as_str().unwrap_or("unknown");
        if baseline_pane["task_state_confirmed"].as_bool().unwrap_or(false)
            && matches!(before_state, "idle" | "finished")
        {
            return HttpResponse::error(409, "agent_idle", "the Agent is not running");
        }
        let payload = json!({
            "window_id": request.window_id,
            "pane_id": request.pane_id,
            "identity": identity,
        });
        match self.begin_write(device_id, &request.request_id, "session.stop", &payload) {
            Ok(Some(receipt)) => return HttpResponse::json(200, receipt_json(receipt)),
            Ok(None) => {},
            Err(response) => return response,
        }
        if let Err(response) = self.mark_dispatching(device_id, &request.request_id) {
            return response;
        }
        if !self.project_is_current(&project) {
            let error = json!({"error":{"code":"project_not_authorized","message":"project authorization changed"}});
            let _ = self.state.receipts.complete(
                device_id,
                &request.request_id,
                "rejected",
                Some(error),
            );
            return HttpResponse::error(
                403,
                "project_not_authorized",
                "project authorization changed",
            );
        }
        let key_result = runtime_call(
            "conversation.key",
            json!({
                "window_id": request.window_id,
                "pane_id": request.pane_id,
                "identity": identity,
                "key": "Ctrl+C",
            }),
        )
        .await;
        let key_value = match key_result {
            Ok(value) => value,
            Err(response) => {
                let unknown = response.status >= 500;
                let _ = self.state.receipts.complete(
                    device_id,
                    &request.request_id,
                    if unknown { "unknown" } else { "rejected" },
                    Some(error_value_ref(&response)),
                );
                return response;
            },
        };
        let wait = runtime_call(
            "pane.wait",
            json!({
                "window_id": request.window_id,
                "pane_id": request.pane_id,
                "state": "settled",
                "timeout_ms": 10_000,
                "after_seq": baseline_seq,
            }),
        )
        .await;
        let stop = match wait {
            Ok(snapshot) => stop_confirmation(
                Some(&snapshot),
                request.window_id,
                request.pane_id,
                &project,
                &identity,
                baseline_seq,
                before_state,
            ),
            Err(response) => {
                let mut stop = stop_confirmation(
                    None,
                    request.window_id,
                    request.pane_id,
                    &project,
                    &identity,
                    baseline_seq,
                    before_state,
                );
                stop["error"] = error_value(response);
                stop
            },
        };
        let result = json!({"key": key_value, "stop": stop});
        let receipt = self
            .state
            .receipts
            .complete(device_id, &request.request_id, "accepted", Some(result))
            .unwrap_or_else(|_| fallback_receipt(&request.request_id, "unknown"));
        HttpResponse::json(200, receipt_json(receipt))
    }

    pub(super) async fn mutation<F>(
        &self,
        device_id: &str,
        method_name: &str,
        body: &[u8],
        runtime_method: &str,
        params: F,
    ) -> HttpResponse
    where
        F: FnOnce(MutationRequest) -> Value,
    {
        let request: MutationRequest = match parse_body(body) {
            Ok(request) => request,
            Err(response) => return response,
        };
        if request.identity["epoch"].as_u64().is_none()
            || !valid_request_id(&request.request_id)
            || request.text.len() > 32 * 1024
        {
            return HttpResponse::error(400, "invalid_request", "invalid request id or text");
        }
        let (identity, _, project) = match self
            .authorize_identity_with_pane(request.window_id, request.pane_id, &request.identity)
            .await
        {
            Ok(value) => value,
            Err(response) => return response,
        };
        if !self.project_is_current(&project) {
            return HttpResponse::error(
                403,
                "project_not_authorized",
                "project authorization changed",
            );
        }
        let payload = params(MutationRequest {
            request_id: request.request_id.clone(),
            window_id: request.window_id,
            pane_id: request.pane_id,
            identity,
            text: request.text,
            prompt_id: request.prompt_id,
            option: request.option,
        });
        match self.begin_write(device_id, &request.request_id, method_name, &payload) {
            Ok(Some(receipt)) => return HttpResponse::json(200, receipt_json(receipt)),
            Ok(None) => {},
            Err(response) => return response,
        }
        if let Err(response) = self.mark_dispatching(device_id, &request.request_id) {
            return response;
        }
        if !self.project_is_current(&project) {
            let error = json!({"error":{"code":"project_not_authorized","message":"project authorization changed"}});
            let _ = self.state.receipts.complete(
                device_id,
                &request.request_id,
                "rejected",
                Some(error),
            );
            return HttpResponse::error(
                403,
                "project_not_authorized",
                "project authorization changed",
            );
        }
        let response = runtime_call(runtime_method, payload).await;
        match response {
            Ok(result) => {
                let receipt = self
                    .state
                    .receipts
                    .complete(device_id, &request.request_id, "accepted", Some(result))
                    .unwrap_or_else(|_| fallback_receipt(&request.request_id, "unknown"));
                HttpResponse::json(200, receipt_json(receipt))
            },
            Err(response) => {
                let unknown = response.status >= 500;
                let _ = self.state.receipts.complete(
                    device_id,
                    &request.request_id,
                    if unknown { "unknown" } else { "rejected" },
                    Some(error_value_ref(&response)),
                );
                response
            },
        }
    }
}

fn stop_confirmation(
    snapshot: Option<&Value>,
    window_id: u64,
    pane_id: u64,
    project: &ProjectGrant,
    expected_identity: &Value,
    baseline_seq: u64,
    before_state: &str,
) -> Value {
    let Some(snapshot) = snapshot else {
        return json!({
            "confirmed": false,
            "reason": "no_settled_state",
            "beforeState": before_state,
        });
    };
    let Some(pane) = pane(snapshot, window_id, pane_id) else {
        return json!({
            "confirmed": false,
            "reason": "pane_closed",
            "beforeState": before_state,
        });
    };
    if !pane_matches_project(pane, project) {
        return json!({
            "confirmed": false,
            "reason": "project_scope_changed",
            "beforeState": before_state,
        });
    }
    let Some(agent) = pane.get("agent").filter(|value| !value.is_null()) else {
        return json!({
            "confirmed": false,
            "reason": "conversation_ended",
            "beforeState": before_state,
        });
    };
    if agent["kind"] != expected_identity["kind"]
        || agent["session_id"] != expected_identity["session_id"]
    {
        return json!({
            "confirmed": false,
            "reason": "conversation_identity_changed",
            "beforeState": before_state,
        });
    }
    let state = pane["task_state"].as_str().unwrap_or("unknown");
    let source = agent["state_source"].as_str().unwrap_or("unknown");
    let confirmed_state = pane["task_state_confirmed"].as_bool().unwrap_or(false);
    let state_change_seq = pane["state_change_seq"].as_u64().unwrap_or(0);
    let stopped = confirmed_state
        && state_change_seq > baseline_seq
        && source == "hook"
        && matches!(state, "idle" | "finished");
    let reason = if stopped {
        "hook_settled"
    } else if !confirmed_state {
        "state_unconfirmed"
    } else if state_change_seq <= baseline_seq {
        "state_not_changed"
    } else if source != "hook" {
        "state_source_not_authoritative"
    } else {
        "not_settled"
    };
    json!({
        "confirmed": stopped,
        "reason": reason,
        "beforeState": before_state,
        "state": state,
        "stateSource": source,
        "stateChangeSeq": state_change_seq,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_confirmation_requires_a_new_authoritative_hook_state() {
        let directory = tempfile::tempdir().unwrap();
        let project = ProjectGrant::from_input(directory.path().to_str().unwrap(), None).unwrap();
        let pane = json!({
            "id": 9,
            "cwd": directory.path(),
            "execution": {"kind":"host"},
            "ssh_destination": null,
            "task_state": "idle",
            "task_state_confirmed": true,
            "state_change_seq": 12,
            "agent": {
                "kind": "codex",
                "session_id": "thread-1",
                "state_source": "hook"
            }
        });
        let snapshot = json!({
            "windows": [{
                "id": 2,
                "tabs": [{"panes": [pane]}]
            }]
        });
        let identity = json!({"kind": "codex", "session_id": "thread-1", "epoch": 3});
        let confirmed =
            stop_confirmation(Some(&snapshot), 2, 9, &project, &identity, 10, "running");
        assert_eq!(confirmed["confirmed"], true);
        assert_eq!(confirmed["reason"], "hook_settled");

        let mut screen = snapshot.clone();
        screen["windows"][0]["tabs"][0]["panes"][0]["agent"]["state_source"] = json!("screen");
        let unconfirmed =
            stop_confirmation(Some(&screen), 2, 9, &project, &identity, 10, "running");
        assert_eq!(unconfirmed["confirmed"], false);
        assert_eq!(unconfirmed["reason"], "state_source_not_authoritative");
    }
}
