use super::super::startup_tests::open;
use super::*;
use gpui::{BorrowAppContext as _, TestAppContext};
use std::time::Duration;

fn hook(owner: &str, name: &str, sequence: u64) -> AiHookEvent {
    crate::ai_hook::parse_remote_envelope(
        format!("nebula-hook/1 source=claude process={owner}\n{{\"hook_event_name\":\"{name}\",\"session_id\":\"{owner}\",\"error\":\"unknown\",\"bridge_sequence\":{sequence}}}").as_bytes(), Some(42),
    ).unwrap()
}

fn codex_hook(owner: &str, name: &str, turn: &str) -> AiHookEvent {
    let payload = serde_json::json!({
        "hook_event_name": name,
        "session_id": owner,
        "turn_id": turn,
    });
    crate::ai_hook::parse_remote_envelope(
        format!("nebula-hook/1 source=codex pane=42 codex_hooks=full\n{payload}").as_bytes(),
        Some(42),
    )
    .unwrap()
}

fn screen(view: &mut TerminalView, text: &str) {
    let frame = format!("\x1b[2J\x1b[H{}", text.replace('\n', "\r\n"));
    super::super::startup_tests::feed(view, frame.as_bytes());
}

#[gpui::test]
fn auto_continue_sends_once_to_owning_pane_without_hiding_failure(cx: &mut TestAppContext) {
    let owner = "auto-sends";
    let (view, window, receiver) = open(cx);
    window.update(|_, cx| {
        cx.update_global::<Settings, _>(|settings, _| {
            settings.ai_auto_continue = true;
            // Every send waits the full configured interval; no 1.5s quick-first floor.
            settings.ai_auto_continue_interval = Duration::from_secs(5);
        })
    });
    view.update(window, |view, cx| {
        let mut prompt = hook(owner, "UserPromptSubmit", 1);
        prompt.turn_id = Some("current-prompt".into());
        view.handle_ai_hook(&prompt, cx);
        let mut failure = hook(owner, "StopFailure", 2);
        failure.turn_id = prompt.turn_id.clone();
        view.handle_ai_hook(&failure, cx);
        assert!(view.last_command_failed);
        view.handle_ai_hook(&failure, cx);
        failure.bridge_sequence = Some(3);
        view.handle_ai_hook(&failure, cx);
    });
    window.run_until_parked();
    window.executor().advance_clock(Duration::from_millis(4999));
    window.run_until_parked();
    assert!(!receiver.try_iter().any(|msg| matches!(msg, Msg::Input(_))));
    window.executor().advance_clock(Duration::from_millis(1));
    window.run_until_parked();
    let inputs: Vec<_> = receiver
        .try_iter()
        .filter_map(|msg| match msg {
            Msg::Input(bytes) => Some(bytes.into_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(inputs, vec![b"continue\r".to_vec()]);
    view.read_with(window, |view, _| {
        assert_eq!(view.agent_activity.status(), AgentStatus::Working);
    });
    window.executor().advance_clock(Duration::from_secs(10));
    window.run_until_parked();
    assert!(!receiver.try_iter().any(|msg| matches!(msg, Msg::Input(_))));
}

#[gpui::test]
fn shared_screen_error_retries_claude_and_codex_without_rewriting_hook_state(
    cx: &mut TestAppContext,
) {
    use std::cell::RefCell;
    use std::rc::Rc;

    for program in ["claude", "codex"] {
        let owner = format!("screen-retry-{program}");
        let turn = format!("turn-{program}");
        let (view, window, receiver) = open(cx);
        let notifications = Rc::new(RefCell::new(Vec::new()));
        let observed = notifications.clone();
        let _subscription = view.update(window, |_, cx| {
            cx.subscribe(&view, move |_, _, event, _| {
                if let TerminalViewEvent::Notification(notification) = event {
                    observed.borrow_mut().push(notification.clone());
                }
            })
        });
        window.update(|_, cx| {
            cx.update_global::<Settings, _>(|settings, _| {
                settings.ai_auto_continue = true;
                settings.ai_auto_continue_attempts = 3;
                settings.ai_auto_continue_interval = Duration::from_secs(60);
                settings.ai_auto_continue_error_patterns =
                    crate::ai_agents::parse_auto_continue_error_patterns(
                        nebula_settings::DEFAULT_AUTO_CONTINUE_ERROR_PATTERNS,
                    );
            })
        });
        view.update(window, |view, cx| {
            if program == "claude" {
                view.handle_ai_hook(&hook(&owner, "UserPromptSubmit", 1), cx);
            } else {
                view.handle_ai_hook(&codex_hook(&owner, "UserPromptSubmit", &turn), cx);
                view.handle_ai_hook(&codex_hook(&owner, "Stop", &turn), cx);
            }
        });
        window.run_until_parked();
        let hook_state = view.read_with(window, |view, _| {
            (view.agent_activity.status(), view.agent_activity.source())
        });
        notifications.borrow_mut().clear();

        let frame = if program == "claude" {
            "API Error: stream error: stream disconnected before completion: stream closed before response.completed\n✻ Worked for 5m 45s · done 8:45 AM\n❯ \n? for shortcuts"
        } else {
            "■ We’re currently experiencing high demand, which may cause temporary errors.\n› Ask Codex to do anything\ngpt-6-astra medium · Context 0% used"
        };
        view.update(window, |view, cx| {
            screen(view, frame);
            for _ in 0..3 {
                view.refresh_agent_screen_state(cx);
            }
            assert_eq!(
                (view.agent_activity.status(), view.agent_activity.source()),
                hook_state,
                "screen-only retry evidence must not rewrite Hook lifecycle state"
            );
            assert!(view.auto_continue_task.is_some());
        });
        window.run_until_parked();
        assert!(
            notifications.borrow().is_empty(),
            "screen match must not emit a failure notification"
        );
        window.executor().advance_clock(Duration::from_millis(59_999));
        window.run_until_parked();
        assert!(!receiver.try_iter().any(|msg| matches!(msg, Msg::Input(_))));
        window.executor().advance_clock(Duration::from_millis(1));
        window.run_until_parked();
        let inputs: Vec<_> = receiver
            .try_iter()
            .filter_map(|msg| match msg {
                Msg::Input(bytes) => Some(bytes.into_owned()),
                _ => None,
            })
            .collect();
        let expected = if program == "codex" {
            b"continue\x1b[C\r".as_slice()
        } else {
            b"continue\r".as_slice()
        };
        assert_eq!(inputs, vec![expected.to_vec()], "{program}");
        window.executor().advance_clock(Duration::from_secs(120));
        window.run_until_parked();
        assert!(!receiver.try_iter().any(|msg| matches!(msg, Msg::Input(_))));
    }
}

#[gpui::test]
fn screen_before_terminal_hook_preserves_one_retry_and_its_deadline(cx: &mut TestAppContext) {
    for program in ["claude", "codex"] {
        let owner = format!("screen-first-{program}");
        let (view, window, receiver) = open(cx);
        window.update(|_, cx| {
            cx.update_global::<Settings, _>(|settings, _| {
                settings.ai_auto_continue = true;
                settings.ai_auto_continue_interval = Duration::from_secs(60);
                settings.ai_auto_continue_error_patterns =
                    crate::ai_agents::parse_auto_continue_error_patterns(
                        nebula_settings::DEFAULT_AUTO_CONTINUE_ERROR_PATTERNS,
                    );
            })
        });
        view.update(window, |view, cx| {
            let prompt = if program == "claude" {
                hook(&owner, "UserPromptSubmit", 1)
            } else {
                codex_hook(&owner, "UserPromptSubmit", "turn-1")
            };
            assert!(view.handle_ai_hook(&prompt, cx));
            let frame = if program == "claude" {
                "API Error: stream error: stream disconnected before completion\n❯ \n? for shortcuts"
            } else {
                "■ We’re currently experiencing high demand, which may cause temporary errors.\n› Ask Codex to do anything\ngpt-6 medium · Context 2% used"
            };
            screen(view, frame);
            let snapshot = view.auto_continue_screen_snapshot().unwrap();
            assert!(
                view.auto_continue_screen_error(program, &snapshot, cx.global::<Settings>()).is_some(),
                "{program}: screen error phrase should match"
            );
            assert!(
                view.auto_continue_screen_can_retry(program, &snapshot),
                "{program}: status={:?}",
                view.agent_activity.status()
            );
            view.refresh_agent_screen_state(cx);
            assert!(view.auto_continue_task.is_some(), "{program}");
        });
        window.run_until_parked();
        window.executor().advance_clock(Duration::from_secs(30));
        window.run_until_parked();
        view.update(window, |view, cx| {
            let stop = if program == "claude" {
                hook(&owner, "StopFailure", 2)
            } else {
                codex_hook(&owner, "Stop", "turn-1")
            };
            assert!(view.handle_ai_hook(&stop, cx));
        });
        window.run_until_parked();
        window.executor().advance_clock(Duration::from_secs(30));
        window.run_until_parked();
        let inputs: Vec<_> = receiver
            .try_iter()
            .filter_map(|msg| match msg {
                Msg::Input(bytes) => Some(bytes.into_owned()),
                _ => None,
            })
            .collect();
        let expected = if program == "codex" {
            b"continue\x1b[C\r".as_slice()
        } else {
            b"continue\r".as_slice()
        };
        assert_eq!(inputs, vec![expected.to_vec()], "{program}");
        window.executor().advance_clock(Duration::from_secs(60));
        window.run_until_parked();
        assert!(!receiver.try_iter().any(|msg| matches!(msg, Msg::Input(_))));
    }
}

#[gpui::test]
fn codex_retry_uses_the_existing_submission_boundary_in_each_input_mode(cx: &mut TestAppContext) {
    for mode in ["legacy", "bracketed", "app-cursor", "win32"] {
        if mode == "win32" && !cfg!(windows) {
            continue;
        }
        let (view, window, receiver) = open(cx);
        window.update(|_, cx| {
            cx.update_global::<Settings, _>(|settings, _| {
                settings.ai_auto_continue = true;
                settings.ai_auto_continue_interval = Duration::from_secs(1);
            })
        });
        view.update(window, |view, cx| {
            view.handle_ai_hook(&codex_hook(mode, "UserPromptSubmit", "input-turn"), cx);
            view.handle_ai_hook(&codex_hook(mode, "Stop", "input-turn"), cx);
            let setup = match mode {
                "bracketed" => b"\x1b[?2004h".as_slice(),
                "app-cursor" => b"\x1b[?1h".as_slice(),
                "win32" => b"\x1b[?9001h".as_slice(),
                _ => b"".as_slice(),
            };
            super::super::startup_tests::feed(view, setup);
            screen(view, "■ 503 Service Unavailable\n› Ask Codex to do anything\ngpt-6 medium · Context 2% used");
            view.refresh_agent_screen_state(cx);
        });
        window.run_until_parked();
        window.executor().advance_clock(Duration::from_secs(2));
        window.run_until_parked();
        let inputs: Vec<_> = receiver
            .try_iter()
            .filter_map(|msg| match msg {
                Msg::Input(bytes) => Some(bytes.into_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(inputs.len(), 1, "{mode}: one owning-pane write");
        if mode == "win32" {
            let sequence = std::str::from_utf8(&inputs[0]).unwrap();
            let records: Vec<_> =
                sequence.strip_prefix("continue").unwrap().split_inclusive('_').collect();
            assert_eq!(records.len(), 4, "Right press/release before Enter press/release");
            assert!(records[0].starts_with("\x1b[39;") && records[1].starts_with("\x1b[39;"));
            assert!(records[2].starts_with("\x1b[13;") && records[3].starts_with("\x1b[13;"));
        } else {
            let expected = match mode {
                "bracketed" => b"\x1b[200~continue\x1b[201~\x1b[C\r".as_slice(),
                "app-cursor" => b"continue\x1bOC\r".as_slice(),
                _ => b"continue\x1b[C\r".as_slice(),
            };
            assert_eq!(inputs[0], expected, "{mode}");
        }
    }
}

#[gpui::test]
fn restored_session_error_does_not_authorize_screen_retry(cx: &mut TestAppContext) {
    for program in ["claude", "codex"] {
        let (view, window, receiver) = open(cx);
        window.update(|_, cx| {
            cx.update_global::<Settings, _>(|settings, _| {
                settings.ai_auto_continue = true;
                settings.ai_auto_continue_interval = Duration::from_secs(1);
            })
        });
        view.update(window, |view, cx| {
            let start = if program == "claude" {
                hook("restore-only", "SessionStart", 1)
            } else {
                codex_hook("restore-only", "SessionStart", "")
            };
            assert!(view.handle_ai_hook(&start, cx));
            screen(view, if program == "claude" {
                "API Error: stream error: stream disconnected before completion\n❯ \n? for shortcuts"
            } else {
                "■ 503 Service Unavailable\n› Ask Codex to do anything\ngpt-6 medium · Context 2% used"
            });
            view.refresh_agent_screen_state(cx);
        });
        window.run_until_parked();
        window.executor().advance_clock(Duration::from_secs(2));
        window.run_until_parked();
        assert!(!receiver.try_iter().any(|msg| matches!(msg, Msg::Input(_))), "{program}");
        view.update(window, |view, cx| {
            let prompt = if program == "claude" {
                hook("restore-only", "UserPromptSubmit", 2)
            } else {
                codex_hook("restore-only", "UserPromptSubmit", "new-turn")
            };
            assert!(view.handle_ai_hook(&prompt, cx));
            view.refresh_agent_screen_state(cx);
        });
        window.run_until_parked();
        window.executor().advance_clock(Duration::from_secs(2));
        window.run_until_parked();
        assert!(
            !receiver.try_iter().any(|msg| matches!(msg, Msg::Input(_))),
            "{program}: a new prompt must not reuse the already visible error"
        );
    }
}

#[gpui::test]
fn working_frames_do_not_forget_an_unchanged_historical_error(cx: &mut TestAppContext) {
    let (view, window, receiver) = open(cx);
    window.update(|_, cx| {
        cx.update_global::<Settings, _>(|settings, _| {
            settings.ai_auto_continue = true;
            settings.ai_auto_continue_interval = Duration::from_secs(1);
        })
    });
    view.update(window, |view, cx| {
        screen(view, "■ 503 Service Unavailable\n› Ask Codex to do anything\ngpt-6 medium · Context 2% used");
        view.handle_ai_hook(&codex_hook("old-error", "UserPromptSubmit", "new-turn"), cx);
        screen(view, "■ 503 Service Unavailable\n• Working (12s · esc to interrupt)\n› Ask Codex to do anything");
        view.refresh_agent_screen_state(cx);
        view.handle_ai_hook(&codex_hook("old-error", "Stop", "new-turn"), cx);
        screen(view, "■ 503 Service Unavailable\nA short successful answer.\n› Ask Codex to do anything\ngpt-6 medium · Context 2% used");
        view.refresh_agent_screen_state(cx);
    });
    window.run_until_parked();
    window.executor().advance_clock(Duration::from_secs(2));
    window.run_until_parked();
    assert!(!receiver.try_iter().any(|msg| matches!(msg, Msg::Input(_))));
}

#[gpui::test]
fn revoked_screen_candidate_does_not_consume_the_later_claude_hook(cx: &mut TestAppContext) {
    let (view, window, receiver) = open(cx);
    window.update(|_, cx| {
        cx.update_global::<Settings, _>(|settings, _| {
            settings.ai_auto_continue = true;
            settings.ai_auto_continue_interval = Duration::from_secs(1);
            settings.ai_auto_continue_error_patterns =
                crate::ai_agents::parse_auto_continue_error_patterns(
                    nebula_settings::DEFAULT_AUTO_CONTINUE_ERROR_PATTERNS,
                );
        })
    });
    view.update(window, |view, cx| {
        view.handle_ai_hook(&hook("screen-redraw", "UserPromptSubmit", 1), cx);
        screen(
            view,
            "API Error: stream error: stream disconnected before completion\n❯ \n? for shortcuts",
        );
        let snapshot = view.auto_continue_screen_snapshot().unwrap();
        assert!(view.auto_continue_screen_error("claude", &snapshot, cx.global::<Settings>()).is_some());
        assert!(view.auto_continue_screen_can_retry("claude", &snapshot));
        view.refresh_agent_screen_state(cx);
        assert!(view.auto_continue_task.is_some());
        screen(view, "Clean redraw.\n❯ \n? for shortcuts");
        view.refresh_agent_screen_state(cx);
        assert!(view.auto_continue_task.is_none());
        view.handle_ai_hook(&hook("screen-redraw", "StopFailure", 2), cx);
    });
    window.run_until_parked();
    window.executor().advance_clock(Duration::from_secs(2));
    window.run_until_parked();
    let inputs: Vec<_> = receiver
        .try_iter()
        .filter_map(|msg| match msg {
            Msg::Input(bytes) => Some(bytes.into_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(inputs, vec![b"continue\r".to_vec()]);
}

#[gpui::test]
fn next_turn_new_error_retries_without_an_intermediate_clear_frame(cx: &mut TestAppContext) {
    let (view, window, receiver) = open(cx);
    window.update(|_, cx| {
        cx.update_global::<Settings, _>(|settings, _| {
            settings.ai_auto_continue = true;
            settings.ai_auto_continue_interval = Duration::from_secs(1);
            settings.ai_auto_continue_error_patterns =
                crate::ai_agents::parse_auto_continue_error_patterns(
                    nebula_settings::DEFAULT_AUTO_CONTINUE_ERROR_PATTERNS,
                );
        })
    });
    let first = "■ We’re currently experiencing high demand, which may cause temporary errors.\n› Ask Codex to do anything\ngpt-6 medium · Context 2% used";
    view.update(window, |view, cx| {
        view.handle_ai_hook(&codex_hook("next-error", "UserPromptSubmit", "first"), cx);
        view.handle_ai_hook(&codex_hook("next-error", "Stop", "first"), cx);
        screen(view, first);
        let snapshot = view.auto_continue_screen_snapshot().unwrap();
        assert!(view.auto_continue_screen_error("codex", &snapshot, cx.global::<Settings>()).is_some());
        assert!(view.auto_continue_screen_can_retry("codex", &snapshot));
        view.refresh_agent_screen_state(cx);
    });
    window.run_until_parked();
    window.executor().advance_clock(Duration::from_secs(2));
    window.run_until_parked();
    assert_eq!(receiver.try_iter().filter(|msg| matches!(msg, Msg::Input(_))).count(), 1);
    view.update(window, |view, cx| {
        view.handle_ai_hook(&codex_hook("next-error", "UserPromptSubmit", "second"), cx);
        view.refresh_agent_screen_state(cx);
        assert!(view.auto_continue_task.is_none(), "old visible error must remain consumed");
        screen(
            view,
            "■ 503 Service Unavailable\n› Ask Codex to do anything\ngpt-6 medium · Context 2% used",
        );
        view.refresh_agent_screen_state(cx);
        assert!(view.auto_continue_task.is_some(), "a new error in the new turn is fresh evidence");
        view.handle_ai_hook(&codex_hook("next-error", "Stop", "second"), cx);
    });
    window.run_until_parked();
    window.executor().advance_clock(Duration::from_secs(2));
    window.run_until_parked();
    assert_eq!(receiver.try_iter().filter(|msg| matches!(msg, Msg::Input(_))).count(), 1);
}

#[gpui::test]
fn codex_screen_retry_cancels_when_the_error_clears_or_user_types(cx: &mut TestAppContext) {
    for action in ["clear", "replace", "input", "permission"] {
        let owner = format!("screen-cancel-{action}");
        let turn = format!("turn-{action}");
        let (view, window, receiver) = open(cx);
        window.update(|_, cx| {
            cx.update_global::<Settings, _>(|settings, _| {
                settings.ai_auto_continue = true;
                settings.ai_auto_continue_interval = Duration::from_secs(1);
                settings.ai_auto_continue_error_patterns =
                    crate::ai_agents::parse_auto_continue_error_patterns(
                        nebula_settings::DEFAULT_AUTO_CONTINUE_ERROR_PATTERNS,
                    );
            })
        });
        view.update(window, |view, cx| {
            view.handle_ai_hook(&codex_hook(&owner, "UserPromptSubmit", &turn), cx);
            view.handle_ai_hook(&codex_hook(&owner, "Stop", &turn), cx);
            screen(
                view,
                "■ We’re currently experiencing high demand, which may cause temporary errors.\n› Ask Codex to do anything\ngpt-6-astra medium · Context 0% used",
            );
            view.refresh_agent_screen_state(cx);
            assert!(view.auto_continue_task.is_some());
            match action {
                "clear" => {
                    screen(view, "Completed answer.\n› Ask Codex to do anything\ngpt-6-astra medium · Context 0% used");
                    view.refresh_agent_screen_state(cx);
                },
                "replace" => {
                    screen(view, "■ 503 Service Unavailable\n› Ask Codex to do anything\ngpt-6-astra medium · Context 0% used");
                    view.refresh_agent_screen_state(cx);
                },
                "input" => view.write_input(b"manual".to_vec(), cx),
                "permission" => screen(view, "■ 503 Service Unavailable\nAllow command?\n› 1. Yes\n2. No\nPress enter to confirm or esc to cancel"),
                _ => unreachable!(),
            }
        });
        receiver.try_iter().for_each(drop);
        window.executor().advance_clock(Duration::from_secs(2));
        window.run_until_parked();
        assert!(!receiver.try_iter().any(|msg| matches!(msg, Msg::Input(_))), "{action}");
    }
}

#[gpui::test]
fn auto_continue_cancels_on_input_setting_changes_new_events_and_exit(cx: &mut TestAppContext) {
    for action in ["input", "toggle", "prompt", "stop", "end", "command", "identity", "closed"] {
        let owner = action;
        let (view, window, receiver) = open(cx);
        window.update(|_, cx| {
            cx.update_global::<Settings, _>(|s, _| {
                s.ai_auto_continue = true;
                // Short interval keeps the cancellation assertions discriminating.
                s.ai_auto_continue_interval = Duration::from_secs(1);
            })
        });
        view.update(window, |view, cx| {
            view.handle_ai_hook(&hook(owner, "UserPromptSubmit", 1), cx);
            view.handle_ai_hook(&hook(owner, "StopFailure", 2), cx);
        });
        window.run_until_parked();
        if action == "toggle" {
            window
                .update(|_, cx| cx.update_global::<Settings, _>(|s, _| s.ai_auto_continue = false));
            window.run_until_parked();
            window
                .update(|_, cx| cx.update_global::<Settings, _>(|s, _| s.ai_auto_continue = true));
        } else {
            view.update(window, |view, cx| match action {
                "input" => view.write_input(b"manual".to_vec(), cx),
                "prompt" => {
                    view.handle_ai_hook(&hook(owner, "UserPromptSubmit", 3), cx);
                },
                "stop" => {
                    view.handle_ai_hook(&hook(owner, "Stop", 3), cx);
                },
                "end" => {
                    view.handle_ai_hook(&hook(owner, "SessionEnd", 3), cx);
                },
                "command" => {
                    view.clear_foreground_agent_state(cx);
                },
                "identity" => {
                    view.ai_session = None;
                },
                "closed" => {
                    view.session = None;
                },
                _ => unreachable!(),
            });
        }
        receiver.try_iter().for_each(drop);
        window.executor().advance_clock(Duration::from_secs(10));
        window.run_until_parked();
        assert!(!receiver.try_iter().any(|msg| matches!(msg, Msg::Input(_))), "{action}");
    }
}

#[gpui::test]
fn auto_continue_disabled_and_foreign_failure_never_submit(cx: &mut TestAppContext) {
    for enabled in [false, true] {
        let owner = if enabled { "auto-foreign" } else { "auto-disabled" };
        let (view, window, receiver) = open(cx);
        window.update(|_, cx| {
            cx.update_global::<Settings, _>(|s, _| {
                s.ai_auto_continue = enabled;
                s.ai_auto_continue_interval = Duration::from_secs(1);
            })
        });
        view.update(window, |view, cx| {
            view.handle_ai_hook(&hook(owner, "UserPromptSubmit", 1), cx);
            let mut event = hook(owner, "StopFailure", 2);
            if enabled {
                event.session_id = Some("foreign".into());
            }
            view.handle_ai_hook(&event, cx);
        });
        window.run_until_parked();
        window.executor().advance_clock(Duration::from_secs(10));
        window.run_until_parked();
        assert!(!receiver.try_iter().any(|msg| matches!(msg, Msg::Input(_))));
    }
}

#[gpui::test]
fn auto_continue_cancels_when_ime_preedit_is_abandoned(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler as _;

    for clear_with_unmark in [true, false] {
        let owner = if clear_with_unmark { "ime-unmark" } else { "ime-empty" };
        let (view, window, receiver) = open(cx);
        window.update(|_, cx| {
            cx.update_global::<Settings, _>(|s, _| {
                s.ai_auto_continue = true;
                // Short interval keeps the cancellation assertions discriminating.
                s.ai_auto_continue_interval = Duration::from_secs(1);
            })
        });
        view.update(window, |view, cx| {
            view.handle_ai_hook(&hook(owner, "UserPromptSubmit", 1), cx);
            view.handle_ai_hook(&hook(owner, "StopFailure", 2), cx);
        });
        window.run_until_parked();
        window.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.replace_and_mark_text_in_range(None, "ni", None, window, cx);
                if clear_with_unmark {
                    view.unmark_text(window, cx);
                } else {
                    view.replace_and_mark_text_in_range(None, "", None, window, cx);
                }
                assert!(view.marked_text.is_none());
            });
        });
        window.executor().advance_clock(Duration::from_secs(2));
        window.run_until_parked();
        assert!(!receiver.try_iter().any(|msg| matches!(msg, Msg::Input(_))), "{owner}");
    }
}

#[gpui::test]
fn auto_continue_cancels_before_clipboard_image_staging(cx: &mut TestAppContext) {
    let (view, window, receiver) = open(cx);
    window.update(|_, cx| cx.update_global::<Settings, _>(|s, _| s.ai_auto_continue = true));
    view.update(window, |view, cx| {
        view.handle_ai_hook(&hook("paste-intent", "UserPromptSubmit", 1), cx);
        view.handle_ai_hook(&hook("paste-intent", "StopFailure", 2), cx);
    });
    window.run_until_parked();
    window.update(|window, cx| {
        // Invalid bytes keep this regression independent of image files and host staging.
        // Accepting the paste must cancel retry before asynchronous decoding can finish.
        let image = gpui::Image::from_bytes(gpui::ImageFormat::Png, vec![1, 2, 3]);
        cx.write_to_clipboard(gpui::ClipboardItem::new_image(&image));
        view.update(cx, |view, cx| {
            assert!(view.auto_continue_task.is_some());
            view.paste(window, cx);
            assert!(view.auto_continue_task.is_none(), "paste intent must cancel the timer");
        });
    });
    window.run_until_parked();
    window.executor().advance_clock(Duration::from_secs(2));
    window.run_until_parked();
    assert!(!receiver.try_iter().any(|msg| matches!(msg, Msg::Input(_))));
}

#[gpui::test]
fn auto_continue_cancels_before_external_path_translation(cx: &mut TestAppContext) {
    let (view, window, receiver) = open(cx);
    window.update(|_, cx| cx.update_global::<Settings, _>(|s, _| s.ai_auto_continue = true));
    view.update(window, |view, cx| {
        view.handle_ai_hook(&hook("drop-intent", "UserPromptSubmit", 1), cx);
        view.handle_ai_hook(&hook("drop-intent", "StopFailure", 2), cx);
        view.exec_context = Some(crate::runtime_exec::PaneExecContext::from_pty_options(
            &nebula_terminal::tty::Options {
                shell: Some(nebula_terminal::tty::Shell::new("wsl.exe".into(), Vec::new())),
                ..Default::default()
            },
        ));
    });
    window.run_until_parked();
    window.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.drop_external_paths(
                &[std::path::PathBuf::from("C:/fixture/image.png")],
                window,
                cx,
            );
            // Do not run the executor: cancellation precedes the WSL conversion job.
            assert!(view.auto_continue_task.is_none(), "accepted drop must cancel the timer");
        });
    });
    assert!(!receiver.try_iter().any(|msg| matches!(msg, Msg::Input(_))));
}
