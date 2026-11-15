use super::*;
use std::{cell::RefCell, rc::Rc};

fn network_frame(program: &str) -> String {
    let (marker, footer) = if program == "claude" {
        ("● API Error:", "────────\n❯ \n────────\n? for shortcuts")
    } else {
        ("■", "› Ask Codex to do anything\ngpt-6-astra medium · Context 0% used")
    };
    format!(
        "{marker} 502 Codex: Post \"https://chatgpt.com/backend-api/codex/responses\": dial tcp: lookup chatgpt.com: no such host. This is a server-side issue, usually temporary — try again in a moment.\n{footer}"
    )
}

#[gpui::test]
fn network_error_notifies_once_without_a_session_or_enabled_retry(cx: &mut TestAppContext) {
    for (program, enabled) in
        [("claude", false), ("codex", false), ("claude", true), ("codex", true)]
    {
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
                settings.ai_auto_continue = enabled;
                settings.ai_auto_continue_error_patterns.clear();
            })
        });
        view.update(window, |view, cx| {
            view.running_program = Some(program.into());
            view.agent_activity.begin_command(true);
            screen(view, &network_frame(program));
            for _ in 0..3 {
                view.refresh_agent_screen_state(cx);
            }
            assert!(view.screen_error_indicator());
            assert!(view.auto_continue.pending().is_none());
            assert!(view.auto_continue_task.is_none());
        });
        window.run_until_parked();
        assert_eq!(notifications.borrow().len(), 1, "{program}: one network reminder");
        assert!(notifications.borrow()[0].is_failure());
        window.executor().advance_clock(Duration::from_secs(120));
        window.run_until_parked();
        assert!(!receiver.try_iter().any(|msg| matches!(msg, Msg::Input(_))));
    }
}

#[gpui::test]
fn network_error_at_dispatch_cancels_an_already_queued_hook_retry(cx: &mut TestAppContext) {
    let (view, window, receiver) = open(cx);
    window.update(|_, cx| {
        cx.update_global::<Settings, _>(|settings, _| {
            settings.ai_auto_continue = true;
            settings.ai_auto_continue_interval = Duration::from_secs(5);
        })
    });
    view.update(window, |view, cx| {
        assert!(view.handle_ai_hook(&hook("network-pending", "UserPromptSubmit", 1), cx));
        assert!(view.handle_ai_hook(&hook("network-pending", "StopFailure", 2), cx));
        assert!(view.auto_continue.pending().is_some());
    });
    window.run_until_parked();
    view.update(window, |view, _| screen(view, &network_frame("claude")));
    window.executor().advance_clock(Duration::from_secs(6));
    window.run_until_parked();
    assert!(!receiver.try_iter().any(|msg| matches!(msg, Msg::Input(_))));
    view.read_with(window, |view, _| assert!(view.auto_continue.pending().is_none()));
}

#[gpui::test]
fn network_error_from_before_a_manual_prompt_does_not_flag_the_new_turn(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    window.update(|_, cx| {
        cx.update_global::<Settings, _>(|settings, _| {
            settings.ai_auto_continue = true;
            settings.ai_auto_continue_error_patterns = vec!["overloaded".into()];
        })
    });
    let notifications = Rc::new(RefCell::new(Vec::new()));
    let observed = notifications.clone();
    let _subscription = view.update(window, |_, cx| {
        cx.subscribe(&view, move |_, _, event, _| {
            if let TerminalViewEvent::Notification(notification) = event {
                observed.borrow_mut().push(notification.clone());
            }
        })
    });
    view.update(window, |view, cx| {
        screen(view, &network_frame("claude"));
        assert!(view.handle_ai_hook(&hook("manual-network", "UserPromptSubmit", 1), cx));
        view.refresh_agent_screen_state(cx);
        assert!(!view.screen_error_indicator());
        assert_eq!(view.agent_activity.status(), AgentStatus::Working);
        screen(view, "────────\n❯ \n────────\n? for shortcuts");
        view.refresh_agent_screen_state(cx);
        screen(view, "API Error: overloaded\n────────\n❯ \n────────\n? for shortcuts");
        view.refresh_agent_screen_state(cx);
        assert!(view.auto_continue.pending().is_some(), "the new turn must remain armed");
        screen(view, &network_frame("claude"));
        view.refresh_agent_screen_state(cx);
        assert!(view.screen_error_indicator());
    });
    window.run_until_parked();
    assert_eq!(notifications.borrow().len(), 1, "only the fresh failure should notify");
}

#[gpui::test]
fn network_error_cancels_screen_retry_without_a_composer_and_waits_for_manual_input(
    cx: &mut TestAppContext,
) {
    for program in ["claude", "codex"] {
        let (view, window, receiver) = open(cx);
        window.update(|_, cx| {
            cx.update_global::<Settings, _>(|settings, _| {
                settings.ai_auto_continue = true;
                settings.ai_auto_continue_interval = Duration::from_secs(5);
                settings.ai_auto_continue_error_patterns = vec!["overloaded".into()];
            })
        });
        view.update(window, |view, cx| {
            if program == "claude" {
                view.handle_ai_hook(&hook("network-screen", "UserPromptSubmit", 1), cx);
            } else {
                view.handle_ai_hook(&codex_hook("network-screen", "UserPromptSubmit", "turn"), cx);
                view.handle_ai_hook(&codex_hook("network-screen", "Stop", "turn"), cx);
            }
            let marker = if program == "claude" { "API Error:" } else { "■" };
            let footer = network_frame(program).split_once('\n').unwrap().1.to_owned();
            let retryable = format!("{marker} 502 overloaded\n{footer}");
            screen(view, &retryable);
            view.refresh_agent_screen_state(cx);
            assert!(view.auto_continue.pending().is_some());
            let hook_state = (view.agent_activity.status(), view.agent_activity.source());
            screen(view, &format!("{marker} dial tcp: lookup chatgpt.com: no such host"));
            view.refresh_agent_screen_state(cx);
            assert!(view.auto_continue.pending().is_none());
            assert!(view.auto_continue_task.is_none());
            assert_eq!((view.agent_activity.status(), view.agent_activity.source()), hook_state);
            screen(view, &footer);
            view.refresh_agent_screen_state(cx);
            screen(view, &retryable);
            view.refresh_agent_screen_state(cx);
            assert!(view.auto_continue.pending().is_none(), "a manual prompt must rearm retries");
        });
        window.executor().advance_clock(Duration::from_secs(120));
        window.run_until_parked();
        assert!(!receiver.try_iter().any(|msg| matches!(msg, Msg::Input(_))));
    }
}
