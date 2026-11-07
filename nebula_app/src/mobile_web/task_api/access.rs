//! Authorization for native conversation identities and project scope.

use super::*;

impl TaskApi {
    pub(super) async fn authorize_identity(
        &self,
        window_id: u64,
        pane_id: u64,
        identity: &Value,
    ) -> Result<Value, HttpResponse> {
        self.authorize_identity_with_pane(window_id, pane_id, identity)
            .await
            .map(|(identity, _, _)| identity)
    }

    pub(super) async fn authorize_identity_with_pane(
        &self,
        window_id: u64,
        pane_id: u64,
        identity: &Value,
    ) -> Result<(Value, Value, ProjectGrant), HttpResponse> {
        let identity = normalize_identity(identity)?;
        let snapshot = match runtime_call("runtime.snapshot", json!({})).await {
            Ok(value) => value,
            Err(response) => return Err(response),
        };
        let preferences = match self.current_preferences() {
            Ok(value) => value,
            Err(response) => return Err(response),
        };
        let Some(pane) = pane(&snapshot, window_id, pane_id) else {
            return Err(HttpResponse::error(
                403,
                "project_not_authorized",
                "session is outside authorized projects",
            ));
        };
        let Some(project) = preferences
            .projects
            .iter()
            .find(|project| pane_matches_project(pane, project))
            .cloned()
        else {
            return Err(HttpResponse::error(
                403,
                "project_not_authorized",
                "session is outside authorized projects",
            ));
        };
        let Some(current) = identity_for_pane(&snapshot, Some(window_id), pane_id, &project) else {
            return Err(HttpResponse::error(
                409,
                "conversation_not_ready",
                "native session identity is not available",
            ));
        };
        if current["kind"] != identity["kind"] || current["session_id"] != identity["session_id"] {
            return Err(HttpResponse::error(
                409,
                "conversation_identity_changed",
                "native session was replaced",
            ));
        }
        Ok((identity, pane.clone(), project))
    }
}

fn normalize_identity(value: &Value) -> Result<Value, HttpResponse> {
    let Some(object) = value.as_object() else {
        return Err(HttpResponse::error(
            400,
            "invalid_identity",
            "native conversation identity must be an object",
        ));
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "kind" | "session_id" | "sessionId" | "epoch"))
    {
        return Err(HttpResponse::error(
            400,
            "invalid_identity",
            "native conversation identity contains unknown fields",
        ));
    }
    let kind = object.get("kind").and_then(Value::as_str).unwrap_or_default();
    let snake_session = object.get("session_id").and_then(Value::as_str);
    let camel_session = object.get("sessionId").and_then(Value::as_str);
    if snake_session.is_some() && camel_session.is_some() && snake_session != camel_session {
        return Err(HttpResponse::error(
            400,
            "invalid_identity",
            "native conversation identity has conflicting session ids",
        ));
    }
    let session_id = snake_session.or(camel_session).unwrap_or_default();
    let epoch = object.get("epoch").cloned().unwrap_or(Value::Null);
    if !matches!(kind, "claude" | "codex")
        || session_id.is_empty()
        || session_id.len() > 128
        || session_id.chars().any(char::is_control)
        || (!epoch.is_null() && epoch.as_u64().is_none())
    {
        return Err(HttpResponse::error(
            400,
            "invalid_identity",
            "native conversation identity is invalid",
        ));
    }
    Ok(json!({
        "kind": kind,
        "session_id": session_id,
        "epoch": epoch,
    }))
}

pub(super) fn pane_matches_project(pane: &Value, project: &ProjectGrant) -> bool {
    if pane.get("ssh_destination").is_some_and(|value| !value.is_null()) {
        return false;
    }
    match (project.wsl_distribution(), pane["execution"]["kind"].as_str()) {
        (None, Some("host")) => pane["cwd"].as_str().is_some_and(|cwd| project.contains_cwd(cwd)),
        (Some(expected), Some("wsl")) => {
            pane["execution"]["distribution"].as_str() == Some(expected)
                && pane["execution"]["cwd"].as_str().is_some_and(|cwd| project.contains_cwd(cwd))
        },
        _ => false,
    }
}

pub(super) fn pane(snapshot: &Value, window_id: u64, pane_id: u64) -> Option<&Value> {
    snapshot["windows"]
        .as_array()?
        .iter()
        .filter(|window| window["id"].as_u64() == Some(window_id))
        .flat_map(|window| window["tabs"].as_array().into_iter().flatten())
        .flat_map(|tab| tab["panes"].as_array().into_iter().flatten())
        .find(|pane| pane["id"].as_u64() == Some(pane_id))
}

pub(super) fn identity_for_pane(
    snapshot: &Value,
    window_id: Option<u64>,
    pane_id: u64,
    project: &ProjectGrant,
) -> Option<Value> {
    let pane = snapshot["windows"]
        .as_array()?
        .iter()
        .filter(|window| window_id.is_none_or(|id| window["id"].as_u64() == Some(id)))
        .flat_map(|window| window["tabs"].as_array().into_iter().flatten())
        .flat_map(|tab| tab["panes"].as_array().into_iter().flatten())
        .find(|pane| pane["id"].as_u64() == Some(pane_id))?;
    if !pane_matches_project(pane, project) {
        return None;
    }
    let agent = pane.get("agent")?;
    let kind = agent["kind"].as_str()?;
    let session_id = agent["session_id"].as_str()?;
    if !matches!(kind, "claude" | "codex") || session_id.is_empty() {
        return None;
    }
    Some(json!({
        "kind": kind,
        "session_id": session_id,
        "epoch": null,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_identity_accepts_camel_case_and_normalizes_session_id() {
        let identity = normalize_identity(&json!({
            "kind": "codex",
            "sessionId": "thread-1",
            "epoch": 7
        }))
        .ok()
        .unwrap();
        assert_eq!(identity, json!({"kind": "codex", "session_id": "thread-1", "epoch": 7}));
        assert!(
            normalize_identity(&json!({
                "kind": "codex",
                "session_id": "a",
                "sessionId": "b"
            }))
            .is_err()
        );
    }

    #[test]
    fn project_scope_requires_observed_execution_and_exact_distribution() {
        let root = tempfile::tempdir().unwrap();
        let host = root.path().to_str().unwrap();
        let guest = crate::ssh::wsl_path(root.path());
        let native = ProjectGrant::from_input(host, None).unwrap();
        let wsl = ProjectGrant::from_input(host, Some(String::from("wsl:Ubuntu"))).unwrap();
        let mut pane = json!({"cwd": host, "execution":{"kind":"host"}});
        assert!(pane_matches_project(&pane, &native));
        assert!(!pane_matches_project(&pane, &wsl));
        // A host shell entering WSL uses the observed guest cwd, not its host cwd.
        pane["execution"] = json!({"kind":"wsl", "distribution":"Ubuntu", "cwd":guest});
        assert!(!pane_matches_project(&pane, &native));
        assert!(pane_matches_project(&pane, &wsl));
        pane["execution"]["distribution"] = json!("Debian");
        assert!(!pane_matches_project(&pane, &wsl));
        pane["execution"]["distribution"] = Value::Null;
        assert!(!pane_matches_project(&pane, &wsl));
        pane["execution"] = Value::Null;
        assert!(!pane_matches_project(&pane, &native));
    }
}
