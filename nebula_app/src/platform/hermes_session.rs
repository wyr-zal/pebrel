//! Native Hermes identity from one verified Linux/WSL foreground process.

use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

use super::agent_process_identity::{self, AgentRecheck, ForegroundAgent};
use super::ai_session_identity::{ObservedWslLocation, ObservedWslSessionScope};
use crate::ai_agents::AgentKind;
use crate::runtime_exec::PaneExecContext;

const PROBE: &str = include_str!("../../res/hooks/hermes_session_probe.py");

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct HermesSession {
    pub session_id: String,
    pub cwd: Option<String>,
}

fn parse(output: &[u8]) -> Option<HermesSession> {
    let session: HermesSession = serde_json::from_slice(output).ok()?;
    AgentKind::Hermes.resume_command(&session.session_id)?;
    let cwd = session.cwd.as_deref()?;
    (cwd.starts_with('/') && cwd.len() <= 4096 && !cwd.chars().any(char::is_control))
        .then_some(session)
}

pub(crate) fn probe(
    pane_id: u64,
    context: &PaneExecContext,
    owner: &ForegroundAgent,
) -> Option<HermesSession> {
    if owner.kind != AgentKind::Hermes || owner.process.pid == 0 || owner.process.start_time == 0 {
        return None;
    }
    let mut command = probe_command(context)?;
    command.args([
        pane_id.to_string(),
        context.process_instance()?.to_owned(),
        owner.process.pid.to_string(),
        owner.process.start_time.to_string(),
        context.command_id().unwrap_or_default().to_owned(),
    ]);
    if agent_process_identity::recheck_foreground_agent(pane_id, context, owner, &|| false)
        != AgentRecheck::Current
    {
        return None;
    }
    crate::platform::process::hidden_command(&mut command);
    let output = super::process_output::read(command, Duration::from_secs(2), 8192).ok()?;
    let session = parse(&output)?;
    (agent_process_identity::recheck_foreground_agent(pane_id, context, owner, &|| false)
        == AgentRecheck::Current)
        .then_some(session)
}

fn probe_command(context: &PaneExecContext) -> Option<Command> {
    if let Some(distribution) = context.wsl_distribution() {
        let mut command = Command::new("wsl.exe");
        if let Some(distribution) = distribution {
            command.args(["--distribution", distribution]);
        }
        if let Some(user) = context.wsl_user() {
            command.args(["--user", user]);
        }
        command.args(["--exec", "python3", "-I", "-c", PROBE]);
        return Some(command);
    }
    #[cfg(target_os = "linux")]
    {
        let mut command = Command::new("/usr/bin/python3");
        command.args(["-I", "-c", PROBE]);
        Some(command)
    }
    #[cfg(not(target_os = "linux"))]
    None
}

pub(crate) fn probe_observed(
    pane_id: u64,
    host_context: &PaneExecContext,
    scope: &ObservedWslSessionScope,
    location: &ObservedWslLocation,
) -> Option<HermesSession> {
    #[cfg(windows)]
    {
        if scope.launcher != location.launcher {
            return None;
        }
        let context = host_context.for_observed_wsl(
            &location.distro,
            location.user.as_deref(),
            &scope.command_id,
        );
        let session = probe(pane_id, &context, &location.agent)?;
        super::wsl_location::recheck_observed_launcher(
            scope.boundary.shell.pid,
            &scope.boundary,
            host_context,
            location,
        )
        .then_some(session)
    }
    #[cfg(not(windows))]
    {
        let _ = (pane_id, host_context, scope, location);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermes_native_metadata_keeps_full_local_ids() {
        let id = format!("local-{}", "a".repeat(64));
        let output = serde_json::json!({"session_id": id, "cwd": "/mnt/f/project"});
        let session = parse(&serde_json::to_vec(&output).unwrap()).unwrap();
        assert_eq!(session.session_id, id);
        assert_eq!(session.cwd.as_deref(), Some("/mnt/f/project"));
    }

    #[test]
    fn hermes_metadata_cannot_turn_a_selector_or_host_path_into_a_native_identity() {
        for (id, cwd) in [
            ("latest", "/project"),
            ("20261107_091523_a1b2c3;exit", "/project"),
            ("20261107_091523_a1b2c3", "C:/host"),
            ("20261107_091523_a1b2c3", "/project\nexit"),
        ] {
            let output = serde_json::json!({"session_id": id, "cwd": cwd});
            assert_eq!(parse(&serde_json::to_vec(&output).unwrap()), None);
        }
        assert_eq!(parse(b"{}"), None);
    }
}
