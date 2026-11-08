use super::*;
use crate::session::AgentSession;

const OLD: &str = "01a1aedc-d6f0-7db0-8875-882c249ef4a2";
const CURRENT: &str = "0199a213-c2a4-7cf5-8f6b-d746fbb6e86c";

fn capture(kind: AgentKind, guest: &[&str], cwd: &str) -> WslAgentLaunch {
    let mut args =
        vec!["-d".into(), "Ubuntu-22.04".into(), "--cd".into(), "/old".into(), "-e".into()];
    args.extend(guest.iter().map(|arg| (*arg).into()));
    WslAgentLaunch::capture("wsl.exe", &args, "Ubuntu-22.04", None, cwd, kind)
}

fn magpie(kind: AgentKind, cwd: &str) -> WslAgentLaunch {
    let cli = kind.resume_command(OLD).unwrap();
    capture(kind, &["sh", "-lc", &format!("exec ${{SHELL:-sh}} -lic '{cli}'")], cwd)
}

#[test]
fn wsl_agent_intent_uses_only_a_literal_agent_execution() {
    for kind in [AgentKind::Claude, AgentKind::Codex, AgentKind::Hermes] {
        let direct = format!("wsl.exe -d Ubuntu-22.04 -e {}", kind.slug());
        let argv = WslAgentLaunch::literal_invocation(&direct).unwrap();
        assert_eq!(WslAgentLaunch::requested_agent(&argv), Some(kind));
        let inner = quoted(&[kind.resume_command(CURRENT).unwrap()], PathQuote::Posix).unwrap();
        let guest = format!("exec ${{SHELL:-sh}} -lic {inner}");
        let argv = ["wsl.exe", "-d", "Ubuntu-22.04", "-e", "sh", "-lc", &guest].map(str::to_owned);
        assert_eq!(WslAgentLaunch::requested_agent(&argv), Some(kind));
    }
    for command in [
        "wsl.exe -e cargo build",
        "wsl.exe -e printf claude",
        "wsl.exe -e sh -c 'echo claude'",
        "wsl.exe -e claude -p 'only print'",
        "wsl.exe --help",
    ] {
        let argv = WslAgentLaunch::literal_invocation(command).unwrap();
        assert_eq!(WslAgentLaunch::requested_agent(&argv), None, "{command}");
    }
}

#[test]
fn magpie_keeps_login_shell_and_replaces_directory_and_resume_target() {
    for kind in [AgentKind::Claude, AgentKind::Codex] {
        let launch = magpie(kind, "/mnt/f/Project O'Brien 项目");
        assert_eq!(launch.requested_session_id(kind).as_deref(), Some(OLD));
        let command = launch.resume_command(kind, CURRENT, PathQuote::PowerShell).unwrap();
        let cli = kind.resume_command(CURRENT).unwrap();
        assert_eq!(
            command,
            format!(
                "& wsl.exe --cd '/mnt/f/Project O''Brien 项目' -d Ubuntu-22.04 -e sh -lc \
             'exec ${{SHELL:-sh}} -lic ''{cli}'''"
            )
        );
        assert!(!command.contains(OLD));
        assert!(!command.contains("/old"));
    }
}

#[test]
fn direct_launch_keeps_user_program_and_necessary_agent_options() {
    let mut launch = capture(
        AgentKind::Codex,
        &[
            "/opt/codex",
            "--model",
            "gpt-6",
            "resume",
            OLD,
            "--last",
            "--all",
            "--cd",
            "/old-agent-dir",
            "-c",
            "model_reasoning_effort=\"xhigh\"",
            "--full-auto",
        ],
        "/current",
    );
    launch.user = Some("fixture".into());
    launch.options.extend(["-u".into(), "fixture".into()]);
    let command = launch.resume_command(AgentKind::Codex, CURRENT, PathQuote::PowerShell).unwrap();
    assert!(command.contains(&format!("-e /opt/codex --model gpt-6 resume {CURRENT}")));
    assert!(command.contains("-u fixture"));
    assert!(command.contains("model_reasoning_effort=\"xhigh\""));
    assert!(command.contains("--full-auto"));
    for removed in [OLD, "--last", "--all", "/old-agent-dir"] {
        assert!(!command.contains(removed), "{removed}: {command}");
    }
}

#[test]
fn codex_root_and_resume_options_keep_their_original_precedence_scopes() {
    let launch = capture(
        AgentKind::Codex,
        &["codex", "--model", "root-model", "resume", OLD, "--model", "resume-model", "--yolo"],
        "/current",
    );
    let command = launch.resume_command(AgentKind::Codex, CURRENT, PathQuote::PowerShell).unwrap();
    assert!(command.ends_with(&format!(
        "-e codex --model root-model resume {CURRENT} --model resume-model --yolo"
    )));
}

#[test]
fn claude_continue_and_fork_need_the_confirmed_current_identity() {
    for args in [
        vec!["claude", "--continue"],
        vec!["claude", "--resume"],
        vec!["claude", "--resume", OLD, "--fork-session"],
    ] {
        let launch = capture(AgentKind::Claude, &args, "/current");
        assert!(launch.requested_session_id(AgentKind::Claude).is_none());
        let command =
            launch.resume_command(AgentKind::Claude, CURRENT, PathQuote::PowerShell).unwrap();
        assert!(command.contains(&format!("claude --resume {CURRENT}")));
        assert!(!command.contains("--continue"));
        assert!(!command.contains("--fork-session"));
    }
}

#[test]
fn a_session_name_is_not_saved_as_an_exact_native_uuid() {
    for (kind, argv) in [
        (AgentKind::Claude, ["claude", "--resume", "named-session"]),
        (AgentKind::Codex, ["codex", "resume", "named-session"]),
    ] {
        let launch = capture(kind, &argv, "/current");
        assert!(launch.requested_session_id(kind).is_none());
        assert!(
            launch.resume_command(kind, CURRENT, PathQuote::PowerShell).unwrap().contains(CURRENT)
        );
    }
}

#[test]
fn codex_selection_uses_the_recorded_guest_environment_and_wrapper() {
    let launch = magpie(AgentKind::Codex, "/current");
    let command = launch.codex_selector_command(PathQuote::PowerShell).unwrap();
    assert_eq!(
        command,
        "& wsl.exe --cd /current -d Ubuntu-22.04 -e sh -lc \
        'exec ${SHELL:-sh} -lic ''codex resume'''"
    );
    assert!(!command.contains(OLD));
}

#[test]
fn unknown_scripts_commands_and_initial_prompts_are_not_replayed() {
    for guest in [
        vec!["sh", "-lc", "cd /elsewhere; codex resume saved"],
        vec!["sh", "-lc", "exec ${SHELL:-sh} -lic 'codex resume saved; echo extra'"],
        vec!["sh", "-lc", "exec ${SHELL:-sh} -lic \"codex resume $SESSION\""],
        vec!["codex", "exec", "do something"],
        vec!["codex", "please change these files"],
        vec!["codex", "--unknown-flag"],
    ] {
        let launch = capture(AgentKind::Codex, &guest, "/current");
        assert_eq!(launch.cwd, "/current");
        assert!(launch.resume_command(AgentKind::Codex, CURRENT, PathQuote::PowerShell).is_none());
    }
}

#[test]
fn scope_mismatch_and_unsafe_imported_launches_cannot_run_a_host_fallback() {
    let original = magpie(AgentKind::Codex, "/current");
    for variant in 0..7 {
        let mut launch = original.clone();
        match variant {
            0 => launch.distribution = "another-distro".into(),
            1 => launch.user = Some("another-user".into()),
            2 => {
                launch.options.extend(["-e".into(), "sh".into(), "-c".into(), "echo extra".into()])
            },
            3 => launch.launcher = "pwsh.exe".into(),
            4 => launch.cwd = "F:/host-path".into(),
            5 => launch.cwd = "/bad\npath".into(),
            _ => launch.options = vec!["-d".into()],
        }
        let agent = AgentSession {
            source: "codex".into(),
            session_id: Some(CURRENT.into()),
            session_file: None,
            wsl_launch: Some(launch),
        };
        assert!(agent.resume_command().is_none(), "variant={variant}");
        assert!(agent.codex_selector_command(PathQuote::PowerShell).is_none());
    }
}

#[test]
fn old_snapshots_load_without_guessing_a_missing_claude_id() {
    for source in ["claude", "codex"] {
        let agent: AgentSession =
            serde_json::from_value(serde_json::json!({"source": source})).unwrap();
        assert!(agent.wsl_launch.is_none());
        assert!(agent.resume_command().is_none());
        assert_eq!(serde_json::to_value(&agent).unwrap(), serde_json::json!({"source": source}));
    }
}

#[test]
fn hermes_saved_session_resumes_its_exact_native_id() {
    for id in ["20261107_091523_a1b2c3".to_owned(), format!("local-{}", "a".repeat(64))] {
        let saved: AgentSession = serde_json::from_value(serde_json::json!({
            "source": "hermes", "session_id": id,
        }))
        .unwrap();
        let restored: AgentSession =
            serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        assert_eq!(restored, saved);
        assert_eq!(restored.resume_command(), Some(format!("hermes --resume {id}")));
    }
}

#[test]
fn hermes_wsl_resume_keeps_guest_scope_and_replaces_the_saved_target() {
    let old = "20261106_101112_123abc";
    let current = "20261107_091523_a1b2c3";
    for argv in [
        vec!["hermes", "--resume", old],
        vec!["hermes", "chat", "-r", old, "--model", "fixture-model"],
    ] {
        let launch = capture(AgentKind::Hermes, &argv, "/mnt/f/Hermes O'Brien 项目");
        assert!(launch.is_replayable(), "{argv:?}");
        assert_eq!(launch.requested_session_id(AgentKind::Hermes).as_deref(), Some(old));
        let text =
            launch.resume_command(AgentKind::Hermes, current, PathQuote::PowerShell).unwrap();
        assert!(text.contains("--cd '/mnt/f/Hermes O''Brien 项目' -d Ubuntu-22.04"));
        assert!(text.contains(&format!("--resume {current}")));
        assert!(text.contains("--no-restore-cwd"));
        assert!(!text.contains(old));
        assert!(!text.contains("/old"));
    }
}

#[test]
fn hermes_login_shell_and_profile_survive_resume_of_a_full_local_id() {
    let id = format!("local-{}", "b".repeat(64));
    let inner = format!("exec ${{SHELL:-sh}} -lic 'hermes --profile fixture --resume {id}'");
    let launch = capture(AgentKind::Hermes, &["sh", "-lc", &inner], "/current");
    assert_eq!(launch.requested_session_id(AgentKind::Hermes).as_deref(), Some(id.as_str()));
    let text = launch.resume_command(AgentKind::Hermes, &id, PathQuote::PowerShell).unwrap();
    assert!(text.contains("-e sh -lc"));
    assert!(text.contains("--profile fixture"));
    assert!(text.contains("--no-restore-cwd"));
    assert!(text.contains(&id));
    assert_eq!(AgentKind::Claude.resume_command(&id), None);
}

#[test]
fn hermes_missing_or_unsafe_identity_never_selects_a_different_conversation() {
    let saved: AgentSession = serde_json::from_str(r#"{"source":"hermes"}"#).unwrap();
    assert_eq!(saved.resume_command(), None);
    assert_eq!(serde_json::to_value(&saved).unwrap(), serde_json::json!({"source": "hermes"}));
    for id in ["latest", "a title", "20261107_091523_a1b2c3;exit", "", "-c"] {
        assert_eq!(AgentKind::Hermes.resume_command(id), None, "{id}");
    }
}

#[test]
fn durable_launch_roundtrip_prefers_the_native_rollout_thread() {
    let agent = AgentSession {
        source: "codex".into(),
        session_id: Some(OLD.into()),
        session_file: Some(format!("/home/fixture/.codex/sessions/rollout-date-{CURRENT}.jsonl")),
        wsl_launch: Some(magpie(AgentKind::Codex, "/current")),
    };
    let restored: AgentSession =
        serde_json::from_slice(&serde_json::to_vec(&agent).unwrap()).unwrap();
    assert_eq!(agent, restored);
    let command = restored.resume_command().unwrap();
    assert!(command.contains(CURRENT));
    assert!(!command.contains(OLD));
}

#[test]
fn outer_posix_and_cmd_quoting_keep_shell_expansion_literal() {
    let mut launch = magpie(AgentKind::Codex, "/项目 O'Brien");
    launch.launcher = r"C:\Windows\System32\wsl.exe".into();
    let command = launch.resume_command(AgentKind::Codex, CURRENT, PathQuote::Posix).unwrap();
    assert!(command.starts_with("wsl.exe --cd "));
    assert!(command.contains("O'\\''Brien"));
    assert!(!command.contains("C:\\Windows"));
    assert!(launch.resume_command(AgentKind::Codex, CURRENT, PathQuote::CommandPrompt).is_some());
    launch.cwd = "/percent%path".into();
    assert!(launch.resume_command(AgentKind::Codex, CURRENT, PathQuote::CommandPrompt).is_none());
}
