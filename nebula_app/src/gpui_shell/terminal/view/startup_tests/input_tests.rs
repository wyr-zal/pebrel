use super::*;

#[gpui::test]
fn accept_right_passes_tab_to_the_shell_and_right_accepts_the_ghost(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("pass-through.txt"), b"").unwrap();
    let (view, window, receiver) = open_at(cx, Some(directory.path().to_path_buf()));
    window.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.accept_key = nebula_settings::AcceptKeyName::Right;
            view.suggest.suggest_env = crate::display::SuggestEnv::Local;
            view.ghost_enabled = true;
            view.completion_style = crate::display::CompletionStyle::Inline;
            feed(view, "❯ cat pass-th".as_bytes());
            refresh_completion_from_grid(view, cx);
        });
    });
    window.run_until_parked();
    window.update(|window, cx| {
        view.update(cx, |view, cx| {
            assert_eq!(view.suggest.suggestion, "rough.txt");
            // Tab 不归 accept 时必须完整到达 shell：无 APP_CURSOR、无 WIN32 输入
            // 模式（本会话两者皆未协商），keymap 把 tab 编码为裸 \t。
            view.on_terminal_tab(&TerminalTab, window, cx);
            let input: Vec<u8> = receiver
                .try_iter()
                .filter_map(|message| match message {
                    Msg::Input(bytes) => Some(bytes.into_owned()),
                    _ => None,
                })
                .flatten()
                .collect();
            assert_eq!(input, b"\t", "tab beyond accept falls through to the shell");
            assert!(
                view.suggest.suggestion.is_empty(),
                "stale ghost must not linger over the redraw"
            );
        });
    });
    window.update(|window, cx| {
        view.update(cx, |view, cx| {
            // 右方向键仍按 accept=right 采纳行内建议。
            refresh_completion_from_grid(view, cx);
        });
    });
    window.run_until_parked();
    window.update(|window, cx| {
        view.update(cx, |view, cx| {
            assert_eq!(view.suggest.suggestion, "rough.txt");
            view.on_key_down(
                &KeyDownEvent {
                    keystroke: gpui::Keystroke::parse("right").unwrap(),
                    is_held: false,
                    prefer_character_input: false,
                },
                window,
                cx,
            );
            let input: Vec<u8> = receiver
                .try_iter()
                .filter_map(|message| match message {
                    Msg::Input(bytes) => Some(bytes.into_owned()),
                    _ => None,
                })
                .flatten()
                .collect();
            assert_eq!(input, b"rough.txt", "right keeps accepting the inline ghost");
        });
    });
}

#[gpui::test]
fn accept_tab_passes_right_to_the_shell_and_tab_accepts_the_ghost(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("mode-candidate.txt"), b"").unwrap();
    let (view, window, receiver) = open_at(cx, Some(directory.path().to_path_buf()));
    window.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.accept_key = nebula_settings::AcceptKeyName::Tab;
            view.suggest.suggest_env = crate::display::SuggestEnv::Local;
            view.ghost_enabled = true;
            view.completion_style = crate::display::CompletionStyle::Inline;
            feed(view, "❯ cat mode-c".as_bytes());
            refresh_completion_from_grid(view, cx);
        });
    });
    window.run_until_parked();
    window.update(|window, cx| {
        view.update(cx, |view, cx| {
            assert_eq!(view.suggest.suggestion, "andidate.txt");
            // right 不归 accept 时透传：编码前提同上（无 APP_CURSOR / WIN32 输入）。
            view.on_key_down(
                &KeyDownEvent {
                    keystroke: gpui::Keystroke::parse("right").unwrap(),
                    is_held: false,
                    prefer_character_input: false,
                },
                window,
                cx,
            );
            let input: Vec<u8> = receiver
                .try_iter()
                .filter_map(|message| match message {
                    Msg::Input(bytes) => Some(bytes.into_owned()),
                    _ => None,
                })
                .flatten()
                .collect();
            assert_eq!(input, b"\x1b[C", "right beyond accept falls through to the shell");
            assert!(view.suggest.suggestion.is_empty());
        });
    });
    window.update(|window, cx| {
        view.update(cx, |view, cx| {
            // Tab 仍按 accept=tab 采纳行内建议。
            refresh_completion_from_grid(view, cx);
        });
    });
    window.run_until_parked();
    window.update(|window, cx| {
        view.update(cx, |view, cx| {
            assert_eq!(view.suggest.suggestion, "andidate.txt");
            view.on_terminal_tab(&TerminalTab, window, cx);
            let input: Vec<u8> = receiver
                .try_iter()
                .filter_map(|message| match message {
                    Msg::Input(bytes) => Some(bytes.into_owned()),
                    _ => None,
                })
                .flatten()
                .collect();
            assert_eq!(input, b"andidate.txt", "tab keeps accepting the inline ghost");
        });
    });
}

#[cfg(windows)]
#[gpui::test]
fn native_launch_boundary_survives_late_start_and_expires_with_the_command(
    cx: &mut TestAppContext,
) {
    let (view, window, receiver) = open(cx);
    view.update(window, |view, cx| {
        submit_codex_restore(view, &receiver, cx);
        view.process_event(Event::CommandDone { exit_code: Some(1) }, cx);
        view.suggest.suggest_env = crate::display::SuggestEnv::Local;
        view.session.as_mut().unwrap().shell_pid = std::process::id();
        view.capture_native_command_boundary();
        let first = view.native_command_boundary.clone().unwrap();
        view.on_command_start(cx);
        assert_eq!(view.native_command_boundary, Some(first.clone()));
        view.clear_foreground_agent_state(cx);
        assert_eq!(view.native_command_boundary, None);
        view.on_command_start(cx);
        assert!(view.native_command_boundary.as_ref().unwrap().time >= first.time);
        view.clear_foreground_agent_state(cx);
        view.capture_native_command_boundary();
        let second = view.native_command_boundary.as_ref().unwrap().time;
        view.agent_identity_probe_epoch = 900;
        view.capture_native_command_boundary();
        assert_ne!(view.agent_identity_probe_epoch, 900);
        assert!(view.native_command_boundary.as_ref().unwrap().time >= second);
    });
}

#[cfg(windows)]
#[gpui::test]
fn failed_recovery_runtime_paste_captures_enter_boundary_and_keeps_early_hook(
    cx: &mut TestAppContext,
) {
    use crate::platform::agent_process_identity::{ForegroundAgent, ProcessStamp};
    let (view, window, receiver) = open(cx);
    view.update(window, |view, cx| {
        submit_codex_restore(view, &receiver, cx);
        view.process_event(Event::CommandDone { exit_code: Some(1) }, cx);
        view.suggest.suggest_env = crate::display::SuggestEnv::Local;
        view.session.as_mut().unwrap().shell_pid = std::process::id();
        feed(view, b"\x1b[?2004h");
        view.runtime_paste("claude".into(), true, InputOrigin::Program, cx).unwrap();
        let before_enter = view.native_command_boundary.clone().unwrap();
        assert!(view.pending_runtime_submit.is_some());
        feed(view, b"claude");
        view.flush_pending_runtime_submit(cx);
        assert!(view.pending_runtime_submit.is_none());
        let boundary = view.native_command_boundary.clone().unwrap();
        assert!(boundary.time >= before_enter.time);
        let mut hook = crate::ai_hook::parse_remote_envelope(
            b"nebula-hook/1 source=claude pane=42\n{\"hook_event_name\":\"SessionStart\",\"session_id\":\"pasted-live\"}",
            Some(42),
        ).unwrap();
        hook.agent_pid = Some(9001);
        hook.agent_start_time = Some(boundary.time + 1);
        assert!(!view.handle_ai_hook(&hook, cx));
        assert_eq!(view.pending_recovery_hooks.len(), 1);
        view.on_command_start(cx);
        assert_eq!(view.native_command_boundary, Some(boundary.clone()));
        assert_eq!(view.pending_recovery_hooks.len(), 1, "late C must retain this submission's early Hook");
        view.running_program = Some("claude".into());
        view.agent_identity_receipt = Some(ForegroundAgent {
            kind: crate::ai_agents::AgentKind::Claude,
            process: ProcessStamp { pid: 9001, start_time: boundary.time + 1 },
            shell: boundary.shell, tty_nr: 0, process_group: 0,
        });
        view.replay_pending_recovery_hooks(cx);
        assert_eq!(view.recovery.live.as_ref().unwrap().session_id.as_deref(), Some("pasted-live"));
        assert!(view.agent_activity.hook_seen());
    });
}
