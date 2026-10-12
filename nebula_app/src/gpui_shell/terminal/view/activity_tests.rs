use super::startup_tests::{feed, open};
use super::*;
use crate::ai_agents::{AgentStatus, AgentStatusSource};
use crate::ai_hook::AiHookEvent;
use gpui::{BorrowAppContext as _, TestAppContext};

fn hook(session: &str, name: &str, sequence: u64) -> AiHookEvent {
    let payload = serde_json::json!({
        "hook_event_name": name,
        "session_id": session,
        "bridge_sequence": sequence,
    });
    let wire = format!("nebula-hook/1 source=claude pane=42\n{payload}");
    crate::ai_hook::parse_remote_envelope(wire.as_bytes(), Some(42)).unwrap()
}

#[gpui::test]
fn pi_redraw_anchor_follows_accepted_hook_and_command_lifecycle(cx: &mut TestAppContext) {
    use nebula_terminal::term::test::TermSize;
    use nebula_terminal::vte::ansi::Processor;

    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        let event = |kind: &str, seq: u64| {
            crate::ai_hook::parse_remote_envelope(
                format!("nebula-hook/1 source=pi pane=42\n{{\"kind\":\"{kind}\",\"session_id\":\"redraw-policy\",\"bridge_sequence\":{seq}}}").as_bytes(),
                Some(42),
            ).unwrap()
        };
        let check = |view: &mut TerminalView, expected: usize| {
            let mut term = view.session.as_ref().unwrap().term.lock();
            term.resize(TermSize::new(60, 5));
            let mut stream = Processor::<nebula_terminal::vte::ansi::StdSyncHandler>::default();
            let text = (0..12).map(|n| format!("ROW_{n:03}")).collect::<Vec<_>>().join("\r\n");
            let frame = format!("\x1b[?2026h\x1b[2J\x1b[H\x1b[3J{text}\x1b[?2026l");
            term.scroll_display(Scroll::Bottom);
            stream.advance(&mut *term, frame.as_bytes());
            term.scroll_display(Scroll::Top);
            term.scroll_display(Scroll::Delta(-2));
            stream.advance(&mut *term, frame.as_bytes());
            assert_eq!(term.grid().display_offset(), expected);
        };
        check(view, 0);
        assert!(view.handle_ai_hook(&event("session-start", 1), cx));
        check(view, 5);
        assert!(view.handle_ai_hook(&event("done", 2), cx));
        check(view, 5); // 回合结束不是 Pi 进程退出。
        assert!(view.handle_ai_hook(&event("session-end", 3), cx));
        check(view, 0);
        assert!(view.handle_ai_hook(&event("session-start", 4), cx));
        check(view, 5);
        view.finish_foreground_command(Some(0), cx);
        check(view, 0);
    });
}

fn screen(view: &mut TerminalView, text: &str) {
    feed(view, format!("\x1b[2J\x1b[H{}", text.replace('\n', "\r\n")).as_bytes());
}

#[gpui::test]
fn hook_lifecycle_cannot_be_rewritten_by_screen_words_or_idle_samples(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        view.handle_ai_hook(&hook("authority", "UserPromptSubmit", 1), cx);
        for text in [
            "code: contains = [\"(y/n)\"]\n❯ \n? for shortcuts",
            "Do you want to proceed?\n❯ 1. Yes\n2. No\nEsc to cancel",
            "────────\n❯ \n────────\n? for shortcuts",
        ] {
            screen(view, text);
            for _ in 0..6 {
                view.refresh_agent_screen_state(cx);
            }
            assert_eq!(view.agent_activity.status(), AgentStatus::Working, "{text}");
            assert_eq!(view.agent_activity.source(), AgentStatusSource::Hook);
        }
        screen(view, "Do you want to proceed?\n❯ 1. Yes\n2. No\nEsc to cancel");
        view.handle_ai_hook(&hook("authority", "Stop", 2), cx);
        view.refresh_agent_screen_state(cx);
        assert_eq!(view.agent_activity.status(), AgentStatus::Done, "Stop is a terminal event");
        view.handle_ai_hook(&hook("authority", "PermissionRequest", 3), cx);
        assert_eq!(view.agent_activity.status(), AgentStatus::Blocked);
        screen(view, "────────\n❯ \n────────\n? for shortcuts");
        for _ in 0..6 {
            view.refresh_agent_screen_state(cx);
        }
        assert_eq!(
            view.agent_activity.status(),
            AgentStatus::Blocked,
            "only the owner can resolve its request"
        );
        view.handle_ai_hook(&hook("authority", "PostToolUse", 4), cx);
        assert_eq!(view.agent_activity.status(), AgentStatus::Working);
    });
}

#[gpui::test]
fn nested_agent_hooks_do_not_finish_or_replace_the_primary_turn(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        let mut main = hook("primary", "UserPromptSubmit", 1);
        main.agent_pid = Some(100);
        view.handle_ai_hook(&main, cx);
        for (index, name) in
            ["SessionStart", "Stop", "PermissionRequest", "SessionEnd"].iter().enumerate()
        {
            let mut nested = hook("nested", name, index as u64 + 1);
            nested.agent_pid = Some(200);
            view.handle_ai_hook(&nested, cx);
            assert_eq!(view.agent_activity.status(), AgentStatus::Working, "nested {name}");
            assert_eq!(view.ai_session.as_ref().unwrap().session_id, "primary");
        }
    });
}

#[gpui::test]
fn shell_metadata_cannot_replace_a_hook_owned_agent_identity(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        view.handle_ai_hook(&hook("hook-owned-identity", "UserPromptSubmit", 1), cx);
        view.suggest.last_committed = "cc --resume".into();
        for title in ["NEBULA|/project|main|node", "NEBULA|/project|main"] {
            view.process_event(TermEvent::Title(title.into()), cx);
            view.on_command_start(cx);
            assert_eq!(view.running_program.as_deref(), Some("claude"));
            assert_eq!(view.ai_session.as_ref().unwrap().session_id, "hook-owned-identity");
            assert_eq!(view.agent_activity.source(), AgentStatusSource::Hook);
            assert_eq!(view.agent_activity.status(), AgentStatus::Working);
        }
    });
}

#[gpui::test]
fn command_end_clears_progress_and_stale_agent_state(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        for code in [0, 1] {
            view.suggest.last_committed = "pi update --extensions".into();
            view.on_command_start(cx);
            view.process_event(TermEvent::Progress { state: 3, value: None }, cx);
            assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
            view.process_event(TermEvent::CommandDone { exit_code: Some(code) }, cx);
            screen(view, "◦ Working (1s • esc to interrupt)\n› Ask Codex to do anything");
            view.refresh_agent_screen_state(cx);
            assert_eq!(view.progress, crate::taskbar::TaskProgress::None);
            assert_eq!(view.agent_activity.status(), AgentStatus::Unknown);
            assert_eq!(
                view.sidebar_activity(),
                if code == 0 { SidebarActivity::Idle } else { SidebarActivity::CommandFailed }
            );
        }
    });
}

#[gpui::test]
fn failed_local_snapshot_does_not_finish_a_shell_builtin(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        view.suggest.last_committed = "Start-Sleep -Seconds 30".into();
        view.suggest.suggest_env = crate::display::SuggestEnv::Local;
        view.on_command_start(cx);
        view.session.as_mut().unwrap().shell_pid = u32::MAX;
        view.command_started = Some(std::time::Instant::now() - std::time::Duration::from_secs(4));
        view.reconcile_shell_activity(cx);
        assert!(!view.command_running_disproved);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
    });
}

#[gpui::test]
fn ssh_shell_prompt_return_clears_a_command_without_a_done_marker(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        view.suggest.suggest_env =
            crate::display::SuggestEnv::Ssh { destination: "user@example.test".into() };
        view.suggest.last_committed = "sleep 30".into();
        view.suggest.pending_command_prompt = Some("user@host:~$".into());
        view.on_command_start(cx);
        screen(view, "quiet remote work");
        for _ in 0..6 {
            view.refresh_agent_screen_state(cx);
        }
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running, "silence is not completion");
        screen(view, "user@host:~$ next command");
        view.refresh_agent_screen_state(cx);
        assert_eq!(
            view.sidebar_activity(),
            SidebarActivity::Running,
            "nonempty draft is not an empty prompt"
        );
        screen(view, "user@host:~$ ");
        view.refresh_agent_screen_state(cx);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Idle);
        assert!(view.running_program.is_none());
        assert!(view.command_started.is_none());
    });
}

#[gpui::test]
fn output_rule_source_does_not_turn_a_working_codex_into_attention(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        view.running_program = Some("codex".into());
        screen(view, "  { contains = [\"(y/n)\"] },\n  { contains = [\"[y/n]\"] },\n]\n\n◦ Working (12m 36s • esc to interrupt)\n\n› Ask Codex to do anything");
        view.refresh_agent_screen_state(cx);
        assert_eq!(view.agent_activity.status(), AgentStatus::Working);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
    });
}

#[gpui::test]
fn codex_screen_retry_and_idle_do_not_emit_completion(cx: &mut TestAppContext) {
    use crate::runtime_api::RuntimeTaskState;
    use std::cell::RefCell;
    use std::rc::Rc;

    let (view, window, _) = open(cx);
    let notifications = Rc::new(RefCell::new(Vec::new()));
    let observed = notifications.clone();
    let _subscription = view.update(window, |_, cx| {
        cx.subscribe(&view, move |_, _, event, _| {
            if let TerminalViewEvent::Notification(notification) = event {
                observed.borrow_mut().push(notification.clone());
            }
        })
    });
    let footer =
        "› Ask Codex to do anything\ngpt-6-astra medium · Context 4% used\n? for shortcuts";
    for (output, expected) in [
        ("Completed answer.", AgentStatus::Idle),
        ("■ stream disconnected before completion: failed to send request", AgentStatus::Failed),
        ("■ Rate limit exceeded", AgentStatus::Failed),
        ("■ Turn interrupted", AgentStatus::Idle),
    ] {
        notifications.borrow_mut().clear();
        view.update(window, |view, cx| {
            view.running_program = Some("codex".into());
            screen(view, &format!("• Working (12s • esc to interrupt)\n{footer}"));
            view.refresh_agent_screen_state(cx);
            assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
            for attempt in [1, 5] {
                screen(view, &format!(
                    "• Reconnecting… {attempt}/5 (5m 03s • esc to interrupt)\n  └ Rate limit exceeded.\n  └ Tip: Use /hooks to view and manage lifecycle hooks.\n{footer}"
                ));
                for _ in 0..4 {
                    view.refresh_agent_screen_state(cx);
                }
                assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
                assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
            }
            screen(view, &format!("{output}\n{footer}"));
            for _ in 0..4 {
                view.refresh_agent_screen_state(cx);
            }
            assert_eq!(view.agent_activity.status(), expected, "{output}");
            assert_eq!(view.agent_activity.source(), AgentStatusSource::Screen);
            assert!(!view.agent_activity.hook_seen());
            let expected_runtime = if expected == AgentStatus::Failed {
                RuntimeTaskState::Failed
            } else {
                RuntimeTaskState::Idle
            };
            let expected_sidebar = if expected == AgentStatus::Failed {
                SidebarActivity::Failed
            } else {
                SidebarActivity::Idle
            };
            assert_eq!(view.runtime_task_state(), expected_runtime);
            assert_eq!(view.sidebar_activity(), expected_sidebar);
        });
        let notes = notifications.borrow();
        if expected == AgentStatus::Failed {
            assert_eq!(notes.len(), 1, "{output}: one failure notification");
            assert!(matches!(
                notes.first(),
                Some(crate::notify::Notification::AiTurnIssue {
                    outcome: crate::ai_hook::AiTurnOutcome::Failed,
                    ..
                })
            ));
        } else {
            assert!(notes.is_empty(), "{output}: no unverified completion");
        }
    }
}

#[gpui::test]
fn configured_screen_error_badge_preserves_hook_owned_activity(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    window.update(|_, cx| {
        cx.update_global::<crate::gpui_shell::config::Settings, _>(|settings, _| {
            settings.ai_auto_continue = false;
            settings.ai_auto_continue_error_patterns =
                crate::ai_agents::parse_auto_continue_error_patterns(
                    nebula_settings::DEFAULT_AUTO_CONTINUE_ERROR_PATTERNS,
                );
        })
    });
    let codex_prompt = crate::ai_hook::parse_remote_envelope(
        br#"nebula-hook/1 source=codex codex_hooks=full pane=42
{"hook_event_name":"UserPromptSubmit","session_id":"badge-session","turn_id":"badge-turn"}"#,
        Some(42),
    )
    .unwrap();
    view.update(window, |view, cx| {
        assert!(view.handle_ai_hook(&codex_prompt, cx));
        let hook_status = view.agent_activity.status();
        screen(
            view,
            "■ We’re currently experiencing high demand, which may cause temporary errors.\n› Ask Codex to do anything",
        );
        view.refresh_agent_screen_state(cx);
        assert!(view.screen_error_indicator());
        assert_eq!(view.agent_activity.status(), hook_status);
        assert_eq!(view.agent_activity.status(), AgentStatus::Working);
        assert_eq!(view.agent_activity.source(), AgentStatusSource::Hook);

        screen(view, "Working on the request...\n› Ask Codex to do anything");
        view.refresh_agent_screen_state(cx);
        assert!(!view.screen_error_indicator());
        assert_eq!(view.agent_activity.status(), AgentStatus::Working);
        assert_eq!(view.agent_activity.source(), AgentStatusSource::Hook);
    });
}

#[gpui::test]
fn codex_hooks_still_complete_once_after_screen_only_idle(cx: &mut TestAppContext) {
    use crate::runtime_api::RuntimeTaskState;
    use std::cell::RefCell;
    use std::rc::Rc;

    let (view, window, _) = open(cx);
    let notifications = Rc::new(RefCell::new(Vec::new()));
    let observed = notifications.clone();
    let _subscription = view.update(window, |_, cx| {
        cx.subscribe(&view, move |_, _, event, _| {
            if let TerminalViewEvent::Notification(notification) = event {
                observed.borrow_mut().push(notification.clone());
            }
        })
    });
    let native = |name: &str, turn: &str| {
        let payload = serde_json::json!({
            "hook_event_name": name, "session_id": "screen-hook-recovery", "turn_id": turn,
        });
        crate::ai_hook::parse_remote_envelope(
            format!("nebula-hook/1 source=codex codex_hooks=full\n{payload}").as_bytes(),
            Some(42),
        )
        .unwrap()
    };
    let done = native("Stop", "turn1");
    view.update(window, |view, cx| {
        view.running_program = Some("codex".into());
        screen(view, "• Working (12s • esc to interrupt)");
        view.refresh_agent_screen_state(cx);
        screen(view, "› Ask Codex to do anything\ngpt-6-astra medium · Context 4% used");
        for _ in 0..4 {
            view.refresh_agent_screen_state(cx);
        }
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Idle);
        assert!(view.handle_ai_hook(&native("UserPromptSubmit", "turn1"), cx));
        assert!(view.handle_ai_hook(&done, cx));
        assert!(!view.agent_activity.accepts_hook(&done));
        view.handle_ai_hook(&done, cx);
        screen(view, "• Reconnecting… 1/5 (2s • esc to interrupt)");
        view.refresh_agent_screen_state(cx);
        assert_eq!(view.agent_activity.source(), AgentStatusSource::Hook);
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Finished);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Done);
    });
    assert_eq!(notifications.borrow().len(), 1);
    view.update(window, |view, cx| {
        assert!(view.handle_ai_hook(&native("UserPromptSubmit", "turn2"), cx));
        assert!(!view.agent_activity.accepts_hook(&done));
        view.handle_ai_hook(&done, cx);
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
        assert!(view.handle_ai_hook(&native("Interrupt", "turn2"), cx));
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Idle);
    });
    assert_eq!(notifications.borrow().len(), 1, "interrupt must not notify completion");
}

#[gpui::test]
fn ignored_idle_notification_does_not_reopen_the_hook_ordering_gate(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        for name in ["UserPromptSubmit", "Stop", "Notification", "PostToolUse"] {
            let wire = format!("nebula-hook/1 source=claude\n{{\"hook_event_name\":\"{name}\",\"session_id\":\"idle-notification-order\"}}");
            let event = crate::ai_hook::parse_remote_envelope(wire.as_bytes(), Some(42)).unwrap();
            view.handle_ai_hook(&event, cx);
            if name != "UserPromptSubmit" {
                assert_eq!(view.agent_activity.status(), AgentStatus::Done, "{name}");
            }
        }
    });
}

#[gpui::test]
fn ssh_bell_does_not_report_a_blocked_or_finished_command(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        view.suggest.suggest_env =
            crate::display::SuggestEnv::Ssh { destination: "user@example.test".into() };
        view.suggest.last_committed = "sleep 30".into();
        view.on_command_start(cx);
        view.on_bell(cx);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
        assert!(!view.awaiting_input);
    });
}

#[gpui::test]
fn claude_shift_enter_reaches_pty_without_committing_shell_history(cx: &mut TestAppContext) {
    let (view, window, receiver) = open(cx);
    window.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.running_program = Some("claude".into());
            view.suggest.last_committed = "claude".into();
            view.suggest.line_buf = "a multiline prompt".into();
            feed(view, b"\x1b[?9001h");
            view.on_key_down(
                &KeyDownEvent {
                    keystroke: gpui::Keystroke::parse("shift-enter").unwrap(),
                    is_held: false,
                    prefer_character_input: false,
                },
                window,
                cx,
            );
            assert_eq!(view.suggest.last_committed, "claude");
            assert!(
                view.suggest.line_buf.is_empty(),
                "multiline invalidates the single-line shadow"
            );
        });
    });
    let bytes: Vec<u8> = receiver
        .try_iter()
        .filter_map(|message| match message {
            Msg::Input(bytes) => Some(bytes.into_owned()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(bytes, b"\n");
}
