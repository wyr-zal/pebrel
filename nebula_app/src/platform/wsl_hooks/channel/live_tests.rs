//! Explicit opt-in only: never start WSL or touch a user's pane in ordinary tests.
use super::*;

#[test]
#[ignore = "requires isolated PEBREL_WSL_HOOK_TEST_ROOT, DISTRO, USER and HELPER"]
fn wsl_hook_live_bridge_helper_and_owner_proof_keep_tui_clean() {
    let root = std::env::var("PEBREL_WSL_HOOK_TEST_ROOT").expect("isolated Windows task directory");
    let distro = std::env::var("PEBREL_WSL_HOOK_TEST_DISTRO").expect("explicit distro");
    let user = std::env::var("PEBREL_WSL_HOOK_TEST_USER").expect("explicit guest account");
    let helper = std::env::var("PEBREL_WSL_HOOK_TEST_HELPER").expect("candidate helper");
    assert!(std::path::Path::new(&helper).is_file());
    for (index, source) in ["claude", "codex"].into_iter().enumerate() {
        let pane = 91000 + index as u64;
        let mut options = tty::Options::default();
        options.shell = Some(tty::Shell::new(
            "wsl.exe".into(),
            vec!["--distribution".into(), distro.clone(), "--user".into(), user.clone()],
        ));
        options.env.extend([(crate::ai_hook::PANE_ENV.into(), pane.to_string()),
            (crate::agent_env::PROCESS_ENV.into(), std::process::id().to_string()),
            ("WSLENV".into(), "PEBREL_PANE_ID/u:PEBREL_PROCESS_ID/u:PEBREL_HOOK_EXE/up:PEBREL_REMOTE_HOOK_TOKEN/u:PEBREL_WSL_HOOK_TEST_ROOT/up:PEBREL_WSL_HOOK_TEST_SOURCE/u:PEBREL_WSL_HOOK_TEST_BRIDGE/up".into()),
            (crate::ai_hook::HOOK_EXE_ENV.into(), helper.clone()),
            (crate::ai_hook::remote::TOKEN_ENV.into(), "c".repeat(32)),
            ("PEBREL_WSL_HOOK_TEST_ROOT".into(), format!("{root}/{source}")),
            ("PEBREL_WSL_HOOK_TEST_SOURCE".into(), source.into()),
            ("PEBREL_WSL_HOOK_TEST_BRIDGE".into(), format!("{}/res/hooks/remote_bridge.py", env!("CARGO_MANIFEST_DIR")))]);
        prepare(&mut options, true).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let channel = Channel::start(&options, move |generation, envelope| {
            sender.send((generation, envelope)).is_ok()
        })
        .unwrap()
        .unwrap();
        let mut input = tempfile::tempfile().unwrap();
        input
            .write_all(
                &std::fs::read(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../scripts/tests/fixtures/wsl_hook_pty.py"
                ))
                .unwrap(),
            )
            .unwrap();
        input.rewind().unwrap();
        let mut command = Command::new(crate::platform::shell::wsl_executable().unwrap());
        command
            .args(["--distribution", &distro, "--user", &user, "--exec", "python3", "-"])
            .envs(&options.env)
            .stdin(Stdio::from(input));
        crate::platform::process::hidden_command(&mut command);
        let output = crate::platform::process_output::read_cancellable_with_stdin(
            command,
            Duration::from_secs(40),
            128 * 1024,
            &|| false,
        )
        .unwrap();
        let summary: serde_json::Value =
            serde_json::from_str(String::from_utf8_lossy(&output).lines().last().unwrap()).unwrap();
        assert_eq!(summary["child_ok"], true);
        assert!(
            output.windows(4).any(|bytes| bytes == b"\x1b[2K"),
            "real TUI redraw must be present"
        );
        for forbidden in [b"\x1b]777".as_slice(), b"terminalSequence", b"eHh4eHh4eHh4"] {
            assert!(
                !output.windows(forbidden.len()).any(|bytes| bytes == forbidden),
                "hook transport leaked into the PTY"
            );
        }
        let events: Vec<_> = receiver.try_iter().collect();
        assert_eq!(
            events.len(),
            if source == "claude" { 6 } else { 7 },
            "valid events arrive; wrong-token/owner events do not"
        );
        for (index, (generation, bytes)) in events.iter().enumerate() {
            assert!(channel.accepts(generation));
            let event = crate::ai_hook::parse_remote_envelope(bytes, Some(pane)).unwrap();
            assert_eq!(event.source, source);
            assert_eq!(event.bridge_sequence, Some(index as u64 + 1));
            assert!(event.client_pid.is_none() && event.agent_pid.is_none());
            let body: serde_json::Value =
                serde_json::from_slice(bytes.splitn(2, |byte| *byte == b'\n').nth(1).unwrap())
                    .unwrap();
            assert_eq!(body["tool_response"].as_str().unwrap().len(), 24000);
        }
        channel.cancel();
        assert!(!channel.accepts(&events[0].0));
    }
}
