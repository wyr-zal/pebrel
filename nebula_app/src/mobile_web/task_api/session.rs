//! Project-scoped session launch and chat operations.

use super::access::identity_for_pane;
use super::*;

impl TaskApi {
    pub(super) async fn new_session(&self, device_id: &str, body: &[u8]) -> HttpResponse {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Request {
            request_id: String,
            project_id: String,
            kind: String,
            prompt: String,
        }
        let request: Request = match parse_body(body) {
            Ok(request) => request,
            Err(response) => return response,
        };
        if !valid_request_id(&request.request_id)
            || !matches!(request.kind.as_str(), "claude" | "codex")
            || request.prompt.len() > 32 * 1024
            || request.prompt.chars().any(|value| value == '\0')
        {
            return HttpResponse::error(400, "invalid_request", "invalid session launch");
        }
        let preferences = match self.current_preferences() {
            Ok(value) => value,
            Err(response) => return response,
        };
        let Some(project) = preferences.project(&request.project_id).cloned() else {
            return HttpResponse::error(403, "project_not_authorized", "project is not authorized");
        };
        let payload = json!({
            "projectId": request.project_id,
            "kind": request.kind,
            "prompt": request.prompt,
        });
        match self.begin_write(device_id, &request.request_id, "session.new", &payload) {
            Ok(Some(receipt)) => {
                return HttpResponse::json(200, receipt_json(receipt));
            },
            Ok(None) => {},
            Err(response) => return response,
        }
        let name = format!("mobile-{}", request.request_id);
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
        let launch = runtime_call(
            "agent.start",
            json!({
                "name": name,
                "kind": request.kind,
                "cwd": project.runtime_path(),
                "shell_id": project.shell_id.clone(),
            }),
        )
        .await;
        let value = match launch {
            Ok(value) => value,
            Err(response) => {
                let error = error_value_ref(&response);
                let _ = self.state.receipts.complete(
                    device_id,
                    &request.request_id,
                    if response.status >= 500 { "unknown" } else { "rejected" },
                    Some(error),
                );
                return response;
            },
        };
        let Some(pane_id) = value
            .pointer("/action/pane_id")
            .and_then(Value::as_u64)
            .or_else(|| value.get("pane_id").and_then(Value::as_u64))
        else {
            let _ = self.state.receipts.complete(
                device_id,
                &request.request_id,
                "unknown",
                Some(json!({"error":{"code":"launch_receipt_invalid"}})),
            );
            return HttpResponse::error(502, "launch_receipt_invalid", "launch result had no pane");
        };
        let Some(window_id) = value
            .pointer("/action/window_id")
            .and_then(Value::as_u64)
            .or_else(|| value.get("window_id").and_then(Value::as_u64))
        else {
            let _ = self.state.receipts.complete(
                device_id,
                &request.request_id,
                "unknown",
                Some(json!({"error":{"code":"launch_receipt_invalid"}})),
            );
            return HttpResponse::error(
                502,
                "launch_receipt_invalid",
                "launch result had no window",
            );
        };
        let mut result = json!({
            "windowId": window_id,
            "paneId": pane_id,
            "projectId": request.project_id,
            "kind": request.kind,
        });
        if request.prompt.trim().is_empty() {
            let receipt = self
                .state
                .receipts
                .complete(device_id, &request.request_id, "accepted", Some(result))
                .unwrap_or_else(|_| fallback_receipt(&request.request_id, "unknown"));
            return HttpResponse::json(200, receipt_json(receipt));
        }
        let Some(run_id) = value.pointer("/action/run_id").and_then(Value::as_u64) else {
            let _ = self.state.receipts.complete(
                device_id,
                &request.request_id,
                "failed",
                Some(json!({"error":{"code":"launch_owner_missing"}})),
            );
            return HttpResponse::error(
                502,
                "launch_owner_missing",
                "launch result had no command owner",
            );
        };
        let launch_agent = value.pointer("/action/agent").cloned().unwrap_or(Value::Null);
        result["launchAgentId"] = launch_agent["agent_id"].clone();
        result["launchGeneration"] = launch_agent["generation"].clone();
        result["launchRunId"] = json!(run_id);
        result["phase"] = json!("waiting_for_native_session");
        let _ = self.state.receipts.complete(
            device_id,
            &request.request_id,
            "launching",
            Some(result.clone()),
        );
        let api = self.clone();
        let device_id = device_id.to_owned();
        let request_id = request.request_id.clone();
        let project = project.clone();
        let prompt = request.prompt;
        let kind = request.kind;
        let background_result = result.clone();
        tokio::spawn(async move {
            api.deliver_initial_prompt(
                device_id,
                request_id,
                window_id,
                pane_id,
                project,
                kind,
                prompt,
                background_result,
            )
            .await;
        });
        HttpResponse::json(
            202,
            json!({
                "requestId": request.request_id,
                "status": "launching",
                "result": result,
            }),
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn deliver_initial_prompt(
        &self,
        device_id: String,
        request_id: String,
        window_id: u64,
        pane_id: u64,
        project: ProjectGrant,
        kind: String,
        prompt: String,
        mut result: Value,
    ) {
        let deadline = tokio::time::Instant::now() + SESSION_READY_TIMEOUT;
        let identity = loop {
            if !self.project_is_current(&project) {
                result["phase"] = json!("project_authorization_changed");
                let _ =
                    self.state.receipts.complete(&device_id, &request_id, "rejected", Some(result));
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                result["phase"] = json!("identity_timeout");
                let _ =
                    self.state.receipts.complete(&device_id, &request_id, "failed", Some(result));
                return;
            }
            let snapshot = runtime_call("runtime.snapshot", json!({})).await.ok();
            if let Some(snapshot) = snapshot.as_ref() {
                if !initial_launch_is_current(snapshot, window_id, pane_id, &result) {
                    result["phase"] = json!("launch_replaced_or_ended");
                    let _ = self.state.receipts.complete(
                        &device_id,
                        &request_id,
                        "failed",
                        Some(result),
                    );
                    return;
                }
            }
            if let Some(identity) = snapshot
                .as_ref()
                .and_then(|snapshot| {
                    identity_for_pane(snapshot, Some(window_id), pane_id, &project)
                })
                .filter(|identity| identity["kind"] == kind)
            {
                break identity;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        result["identityReady"] = json!(true);
        let _ = self.state.receipts.complete(
            &device_id,
            &request_id,
            "launching",
            Some(result.clone()),
        );
        let read = loop {
            if tokio::time::Instant::now() >= deadline || !self.project_is_current(&project) {
                result["phase"] = json!("initial_prompt_not_ready");
                let _ =
                    self.state.receipts.complete(&device_id, &request_id, "failed", Some(result));
                return;
            }
            let Ok(snapshot) = runtime_call("runtime.snapshot", json!({})).await else {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            };
            if !initial_launch_is_current(&snapshot, window_id, pane_id, &result) {
                result["phase"] = json!("launch_replaced_or_ended");
                let _ =
                    self.state.receipts.complete(&device_id, &request_id, "failed", Some(result));
                return;
            }
            match runtime_call(
                "conversation.status",
                json!({
                    "window_id": window_id,
                    "pane_id": pane_id,
                    "identity": identity,
                }),
            )
            .await
            {
                Ok(value) if value["can_send"].as_bool() == Some(true) => break value,
                Ok(_) => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                },
                Err(response)
                    if response.status >= 500
                        || matches!(
                            error_value_ref(&response)
                                .pointer("/error/code")
                                .and_then(Value::as_str),
                            Some(
                                "conversation_unavailable"
                                    | "conversation_changed"
                                    | "conversation_not_ready"
                            )
                        ) =>
                {
                    // Status reads do not need a transcript. Each retry still
                    // checks the original launch owner and current project grant.
                    tokio::time::sleep(Duration::from_millis(500)).await;
                },
                Err(response) => {
                    let _ = self.state.receipts.complete(
                        &device_id,
                        &request_id,
                        "rejected",
                        Some(json!({"error":error_value(response)})),
                    );
                    return;
                },
            }
        };
        if !self.project_is_current(&project) {
            result["phase"] = json!("project_authorization_changed");
            let _ = self.state.receipts.complete(&device_id, &request_id, "rejected", Some(result));
            return;
        }
        let Some(epoched_identity) = read.get("identity").cloned() else {
            let _ = self.state.receipts.complete(
                &device_id,
                &request_id,
                "failed",
                Some(json!({"error":{"code":"identity_missing"}})),
            );
            return;
        };
        match runtime_call(
            "conversation.send",
            json!({
                "window_id": window_id,
                "pane_id": pane_id,
                "identity": epoched_identity,
                "text": prompt,
            }),
        )
        .await
        {
            Ok(value) => {
                result["phase"] = json!("message_accepted");
                result["message"] = value;
                let _ =
                    self.state.receipts.complete(&device_id, &request_id, "accepted", Some(result));
            },
            Err(response) => {
                let unknown = response.status >= 500;
                let _ = self.state.receipts.complete(
                    &device_id,
                    &request_id,
                    if unknown { "unknown" } else { "rejected" },
                    Some(json!({"error":error_value(response)})),
                );
            },
        }
    }

    pub(super) async fn send(&self, device_id: &str, body: &[u8]) -> HttpResponse {
        self.mutation(device_id, "session.send", body, "conversation.send", |request| {
            json!({
                "window_id": request.window_id,
                "pane_id": request.pane_id,
                "identity": request.identity,
                "text": request.text,
            })
        })
        .await
    }

    pub(super) async fn choose(&self, device_id: &str, body: &[u8]) -> HttpResponse {
        self.mutation(device_id, "session.choose", body, "conversation.choose", |request| {
            json!({
                "window_id": request.window_id,
                "pane_id": request.pane_id,
                "identity": request.identity,
                "prompt_id": request.prompt_id,
                "option": request.option,
            })
        })
        .await
    }
}

fn initial_launch_is_current(
    snapshot: &Value,
    window_id: u64,
    pane_id: u64,
    launch: &Value,
) -> bool {
    let Some(pane) = super::access::pane(snapshot, window_id, pane_id) else { return false };
    if pane.pointer("/active_run/run_id").and_then(Value::as_u64) != launch["launchRunId"].as_u64()
    {
        return false;
    }
    // Before identity observation the registry has no session yet. Once a pane
    // has an Agent it must belong to this exact managed launch generation.
    let agent = &pane["agent"];
    agent.is_null()
        || (agent["agent_id"] == launch["launchAgentId"]
            && agent["generation"] == launch["launchGeneration"])
}

#[cfg(test)]
mod launch_tests {
    use super::*;

    #[test]
    fn initial_prompt_cannot_adopt_a_replacement_in_the_same_pane() {
        let launch = json!({"launchRunId":21,"launchAgentId":"agent-a","launchGeneration":1});
        let mut snapshot = json!({"windows":[{"id":2,"tabs":[{"panes":[{
            "id":3,"active_run":{"run_id":21},"agent":null
        }]}]}]});
        assert!(initial_launch_is_current(&snapshot, 2, 3, &launch));
        let pane = &mut snapshot["windows"][0]["tabs"][0]["panes"][0];
        pane["agent"] = json!({"agent_id":"agent-a","generation":1});
        assert!(initial_launch_is_current(&snapshot, 2, 3, &launch));
        snapshot["windows"][0]["tabs"][0]["panes"][0]["active_run"] = Value::Null;
        assert!(!initial_launch_is_current(&snapshot, 2, 3, &launch));
        snapshot["windows"][0]["tabs"][0]["panes"][0]["active_run"] = json!({"run_id":22});
        assert!(!initial_launch_is_current(&snapshot, 2, 3, &launch));
    }
}
