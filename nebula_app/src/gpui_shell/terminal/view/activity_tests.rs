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

fn remote_hook(session: &str, name: &str, process: &str, sequence: u64) -> AiHookEvent {
    let payload = serde_json::json!({
        "hook_event_name": name,
        "session_id": session,
        "bridge_sequence": sequence,
    });
    let wire = format!("nebula-hook/1 source=claude process={process}\n{payload}");
    crate::ai_hook::parse_remote_envelope(wire.as_bytes(), None).unwrap()
}

#[cfg(windows)]
#[gpui::test]
fn wsl_hook_from_a_previous_pty_generation_has_no_effect(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        let (session, _) = session::test_session();
        view.session = Some(session);
        let wire = b"nebula-hook/1 source=claude process=20:200\n{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"retired\",\"bridge_sequence\":1}";
        view.process_event(super::super::event_mailbox::SessionEvent::WslHook {
            generation: "retired".into(), envelope: wire.to_vec(),
        }, cx);
        assert!(!view.agent_activity.hook_seen(), "retired WSL event must not claim the pane");
        assert!(view.ai_session.is_none());
    });
}

#[cfg(windows)]
#[gpui::test]
fn wsl_hook_codex_failure_notifies_once_and_rejects_closed_generation(cx: &mut TestAppContext) {
    use crate::ai_hook::lifecycle::CodexTerminalEvidence;
    use crate::platform::wsl_hooks::channel::{Channel, PIPE_ENV, TOKEN_ENV};
    use crate::runtime_api::RuntimeTaskState;
    let (view, window, _) = open(cx);
    let notifications = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let observed = notifications.clone();
    let _subscription = view.update(window, |_, cx| {
        cx.subscribe(&view, move |_, _, event, _| {
            if let TerminalViewEvent::Notification(note) = event {
                observed.borrow_mut().push(note.clone());
            }
        })
    });
    view.update(window, |view, cx| {
        let mut options = nebula_terminal::tty::Options::default();
        options.shell = Some(nebula_terminal::tty::Shell::new("wsl.exe".into(), vec!["--distribution".into(), "fixture".into()]));
        let generation = format!(r"\\.\pipe\pebrel-wsl-hook-failure-{}", crate::ai_hook::remote::new_token().unwrap());
        options.env.extend([(PIPE_ENV.into(), generation.clone()), (TOKEN_ENV.into(), "a".repeat(32)),
            (crate::ai_hook::PANE_ENV.into(), "42".into()), (crate::agent_env::PROCESS_ENV.into(), "900".into())]);
        let (mut session, _) = session::test_session();
        session.wsl_hook = Channel::start(&options, |_, _| true).unwrap();
        view.session = Some(session);
        let start = crate::ai_hook::parse_remote_envelope(b"nebula-hook/1 source=codex process=20:200 codex_hooks=full\n{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"main\",\"turn_id\":\"first\",\"bridge_sequence\":1}", Some(42)).unwrap();
        view.handle_ai_hook(&start, cx);
        let target = view.agent_activity.codex_failure_target().unwrap();
        let input_epoch = view.prompt_input_epoch;
        let failure = |error: &str| CodexTerminalEvidence::Failed { error: error.into() };
        view.apply_wsl_codex_terminal("old-generation", input_epoch, &target, failure("temporary failure"), cx);
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
        view.apply_wsl_codex_terminal(&generation, input_epoch, &target, failure("temporary failure"), cx);
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Failed);
        view.apply_wsl_codex_terminal(&generation, input_epoch, &target, failure("duplicate"), cx);
        view.agent_activity.begin_command(true);
        let next = crate::ai_hook::parse_remote_envelope(b"nebula-hook/1 source=codex process=20:200 codex_hooks=full\n{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"main\",\"turn_id\":\"second\",\"bridge_sequence\":2}", Some(42)).unwrap();
        assert!(view.handle_ai_hook(&next, cx));
        view.shutdown();
        view.apply_wsl_codex_terminal(&generation, input_epoch, &target, failure("late"), cx);
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
    });
    let notes = notifications.borrow();
    assert_eq!(notes.len(), 1);
    assert!(matches!(
        &notes[0],
        crate::notify::Notification::AiTurnIssue {
            outcome: crate::ai_hook::AiTurnOutcome::Failed,
            ..
        }
    ));
}

#[cfg(windows)]
fn codex_terminal_fixture(
    view: &mut TerminalView,
    cx: &mut Context<'_, TerminalView>,
    session_id: &str,
) -> (String, crate::ai_hook::lifecycle::CodexFailureTarget) {
    use crate::platform::wsl_hooks::channel::{Channel, PIPE_ENV, TOKEN_ENV};
    let mut options = nebula_terminal::tty::Options::default();
    options.shell = Some(nebula_terminal::tty::Shell::new(
        "wsl.exe".into(),
        vec!["--distribution".into(), "fixture".into()],
    ));
    let generation = format!(
        r"\\.\pipe\pebrel-wsl-hook-terminal-{}",
        crate::ai_hook::remote::new_token().unwrap()
    );
    options.env.extend([
        (PIPE_ENV.into(), generation.clone()),
        (TOKEN_ENV.into(), "a".repeat(32)),
        (crate::ai_hook::PANE_ENV.into(), "42".into()),
        (crate::agent_env::PROCESS_ENV.into(), "900".into()),
    ]);
    let (mut session, _) = session::test_session();
    session.wsl_hook = Channel::start(&options, |_, _| true).unwrap();
    view.session = Some(session);
    let wire = format!(
        "nebula-hook/1 source=codex process=20:200 codex_hooks=full\n{{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"{session_id}\",\"turn_id\":\"first\",\"bridge_sequence\":1}}"
    );
    let start = crate::ai_hook::parse_remote_envelope(wire.as_bytes(), Some(42)).unwrap();
    assert!(view.handle_ai_hook(&start, cx));
    (generation, view.agent_activity.codex_failure_target().unwrap())
}

#[cfg(windows)]
#[gpui::test]
fn wsl_hook_codex_structured_idle_is_silent_and_real_stop_notifies_once(cx: &mut TestAppContext) {
    use crate::ai_hook::lifecycle::CodexTerminalEvidence;
    use crate::runtime_api::RuntimeTaskState;
    let (view, window, _) = open(cx);
    let notifications = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let observed = notifications.clone();
    let _subscription = view.update(window, |_, cx| {
        cx.subscribe(&view, move |_, _, event, _| {
            if let TerminalViewEvent::Notification(note) = event {
                observed.borrow_mut().push(note.clone());
            }
        })
    });
    view.update(window, |view, cx| {
        let (generation, target) = codex_terminal_fixture(view, cx, "terminal-idle");
        let epoch = view.prompt_input_epoch;
        view.apply_wsl_codex_terminal(&generation, epoch, &target, CodexTerminalEvidence::Finished {}, cx);
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Idle);
        assert!(notifications.borrow().is_empty());
        let stop = |sequence| crate::ai_hook::parse_remote_envelope(
            format!("nebula-hook/1 source=codex process=20:200 codex_hooks=full\n{{\"hook_event_name\":\"Stop\",\"session_id\":\"terminal-idle\",\"turn_id\":\"first\",\"bridge_sequence\":{sequence}}}").as_bytes(),
            Some(42),
        ).unwrap();
        view.handle_ai_hook(&stop(2), cx);
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Finished);
        view.handle_ai_hook(&stop(3), cx);
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Finished);
        view.shutdown();
    });
    assert_eq!(notifications.borrow().len(), 1);
}

#[cfg(windows)]
#[gpui::test]
fn wsl_hook_codex_terminal_result_requires_input_epoch_channel_and_current_turn(
    cx: &mut TestAppContext,
) {
    use crate::ai_hook::lifecycle::CodexTerminalEvidence;
    use crate::runtime_api::RuntimeTaskState;
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        let (generation, target) = codex_terminal_fixture(view, cx, "terminal-guard");
        let old_epoch = view.prompt_input_epoch;
        view.prompt_input_epoch = view.prompt_input_epoch.wrapping_add(1);
        view.apply_wsl_codex_terminal(
            &generation,
            old_epoch,
            &target,
            CodexTerminalEvidence::Finished {},
            cx,
        );
        assert_eq!(
            view.runtime_task_state(),
            RuntimeTaskState::Running,
            "input invalidates old probe even without a Hook"
        );
        let epoch = view.prompt_input_epoch;
        view.apply_wsl_codex_terminal(
            "retired-generation",
            epoch,
            &target,
            CodexTerminalEvidence::Finished {},
            cx,
        );
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
        let mut wrong = target.clone();
        wrong.turn = "old-turn".into();
        view.apply_wsl_codex_terminal(
            &generation,
            epoch,
            &wrong,
            CodexTerminalEvidence::Finished {},
            cx,
        );
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
        view.agent_activity.input_sent();
        view.apply_wsl_codex_terminal(
            &generation,
            epoch,
            &target,
            CodexTerminalEvidence::Cancelled {},
            cx,
        );
        assert_eq!(
            view.runtime_task_state(),
            RuntimeTaskState::Running,
            "new input cannot re-probe the old bound turn"
        );
        view.shutdown();
    });
}

#[cfg(windows)]
#[gpui::test]
fn wsl_hook_uses_authenticated_events_and_rejects_osc_forgery_and_shutdown(
    cx: &mut TestAppContext,
) {
    use super::super::event_mailbox::SessionEvent;
    use crate::platform::wsl_hooks::channel::{Channel, PIPE_ENV, TOKEN_ENV};
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        let mut options = nebula_terminal::tty::Options::default();
        options.shell = Some(nebula_terminal::tty::Shell::new("wsl.exe".into(), vec!["--distribution".into(), "fixture".into()]));
        let generation = format!(r"\\.\pipe\pebrel-wsl-hook-test-{}", crate::ai_hook::remote::new_token().unwrap());
        options.env.extend([(PIPE_ENV.into(), generation.clone()), (TOKEN_ENV.into(), "a".repeat(32)),
            (crate::ai_hook::PANE_ENV.into(), "42".into()), (crate::agent_env::PROCESS_ENV.into(), "900".into())]);
        let (mut session, _) = session::test_session();
        session.wsl_hook = Channel::start(&options, |_, _| true).unwrap();
        view.session = Some(session);
        let wire = |event: &str, sequence: u64| format!("nebula-hook/1 source=claude process=20:200\n{{\"hook_event_name\":\"{event}\",\"session_id\":\"authenticated\",\"bridge_sequence\":{sequence}}}").into_bytes();
        view.process_event(TermEvent::AiHookEnvelope(wire("UserPromptSubmit", 999)), cx);
        assert!(!view.agent_activity.hook_seen(), "OSC cannot bypass host owner validation");
        let session_start_sequence = crate::ai_hook::next_receive_sequence();
        view.process_event_with_sequence(
            SessionEvent::WslHook {
                generation: generation.clone(),
                envelope: wire("SessionStart", 1),
            },
            Some(session_start_sequence),
            cx,
        );
        assert!(view.agent_activity.hook_seen());
        assert_eq!(view.agent_activity.status(), AgentStatus::Idle);

        let queued_before_input = crate::ai_hook::next_receive_sequence();
        view.agent_activity.input_sent();
        view.process_event_with_sequence(
            SessionEvent::WslHook {
                generation: generation.clone(),
                envelope: wire("UserPromptSubmit", 2),
            },
            Some(queued_before_input),
            cx,
        );
        assert_eq!(
            view.agent_activity.status(),
            AgentStatus::Idle,
            "a Hook queued before input cannot start the newly submitted turn"
        );

        let current_prompt_sequence = crate::ai_hook::next_receive_sequence();
        view.process_event_with_sequence(
            SessionEvent::WslHook {
                generation: generation.clone(),
                envelope: wire("UserPromptSubmit", 2),
            },
            Some(current_prompt_sequence),
            cx,
        );
        assert_eq!(view.agent_activity.status(), AgentStatus::Working);

        let stop_sequence = crate::ai_hook::next_receive_sequence();
        view.process_event_with_sequence(
            SessionEvent::WslHook {
                generation: generation.clone(),
                envelope: wire("Stop", 3),
            },
            Some(stop_sequence),
            cx,
        );
        assert_eq!(view.agent_activity.status(), AgentStatus::Done);
        view.shutdown();
        view.process_event(SessionEvent::WslHook { generation, envelope: wire("UserPromptSubmit", 4) }, cx);
        assert_eq!(view.agent_activity.status(), AgentStatus::Done, "shutdown revokes queued events");
    });
}

#[gpui::test]
fn claude_resume_relocated_transcript_accepts_authenticated_hook_chain(cx: &mut TestAppContext) {
    use crate::runtime_api::RuntimeTaskState;
    use base64::Engine as _;

    let (view, window, _) = open(cx);
    let notifications = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let observed = notifications.clone();
    let _subscription = view.update(window, |_, cx| {
        cx.subscribe(&view, move |_, _, event, _| {
            if let TerminalViewEvent::Notification(notification) = event {
                observed.borrow_mut().push(notification.clone());
            }
        })
    });
    view.update(window, |view, cx| {
        let id = "832ce477-f0e1-42d7-a438-32693bb51d6b";
        let current_file = format!("/home/test/.claude/projects/-project/{id}.jsonl");
        view.recovery.target = Some(crate::session::AgentSession {
            wsl_launch: None,
            source: "claude".into(),
            session_id: Some(id.into()),
            session_file: Some(format!("/home/test/.claude/projects/-old-worktree/{id}.jsonl")),
        });
        view.recovery.awaiting_confirmation = true;
        view.agent_activity.begin_command(true);
        let (session, _input, mut events, proxy) = session::test_session_with_events();
        view.session = Some(session);
        let mut stream = nebula_terminal::event_loop::StreamProcessor::default();
        let token = "0123456789abcdef0123456789abcdef";
        stream.set_remote_hook_token(token.into());
        for (index, (name, state, sidebar)) in [
            ("SessionStart", RuntimeTaskState::Idle, SidebarActivity::Idle),
            ("UserPromptSubmit", RuntimeTaskState::Running, SidebarActivity::Running),
            ("PreToolUse", RuntimeTaskState::Running, SidebarActivity::Running),
            ("Stop", RuntimeTaskState::Finished, SidebarActivity::Done),
        ]
        .into_iter()
        .enumerate()
        {
            let payload = serde_json::json!({
                "hook_event_name": name, "session_id": id, "transcript_path": current_file,
                "source": "resume", "bridge_sequence": index + 1,
            });
            let wire = format!("nebula-hook/1 source=claude process=4100:70 pane=999\n{payload}");
            let encoded = base64::engine::general_purpose::STANDARD.encode(wire);
            let frame = format!("\x1b]777;nebula-hook;{token};{encoded}\x07");
            for chunk in frame.as_bytes().chunks(11) {
                stream.feed(&mut view.session.as_ref().unwrap().term.lock(), &proxy, chunk);
            }
            while let Ok(event) = events.try_recv() {
                view.process_event(event, cx);
            }
            assert!(view.agent_activity.hook_seen(), "{name} must survive the recovery gate");
            assert_eq!(view.agent_activity.source(), AgentStatusSource::Hook);
            assert_eq!(view.runtime_task_state(), state, "{name}");
            assert_eq!(view.sidebar_activity(), sidebar, "{name}");
            assert_eq!(view.ai_session.as_ref().unwrap().session_id, id);
            assert_eq!(
                view.recovery.target.as_ref().unwrap().session_file.as_ref(),
                Some(&current_file)
            );
            assert!(!view.recovery_pending());
        }
    });
    assert_eq!(notifications.borrow().len(), 1, "only Stop completes the turn");
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
fn vanished_claude_form_restores_running_with_a_custom_status_footer(cx: &mut TestAppContext) {
    use crate::runtime_api::RuntimeTaskState;

    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        view.running_program = Some("claude".into());
        view.agent_activity.begin_command(true);
        view.agent_activity.submitted();
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);

        screen(view, "Do you want to proceed?\n❯ 1. Yes\n2. No\nEsc to cancel · Tab to amend");
        for _ in 0..6 {
            view.refresh_agent_screen_state(cx);
            assert_eq!(view.agent_activity.status(), AgentStatus::Blocked);
            assert_eq!(view.agent_activity.source(), AgentStatusSource::Screen);
            assert_eq!(view.agent_activity.rule(), Some("live_form_blocked"));
            assert_eq!(view.sidebar_activity(), SidebarActivity::Attention);
            assert_eq!(view.runtime_task_state(), RuntimeTaskState::Attention);
        }

        // An unboxed, unclassified layout cannot replace the last current form.
        let running = "* Ideating… (1m 20s · 2.3k tokens)\n\n❯ \n\n状态 运行中\n模型 Claude Sonnet\n目录 /project\n上下文 12%";
        assert!(crate::ai_agents::detect("claude", running).is_none());
        screen(view, running);
        view.refresh_agent_screen_state(cx);
        assert_eq!(view.agent_activity.status(), AgentStatus::Blocked);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Attention);
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Attention);

        for _ in 0..8 {
            view.refresh_agent_screen_state(cx);
            assert_eq!(view.agent_activity.status(), AgentStatus::Blocked);
        }
        // A complete current composer with real activity can resolve attention.
        screen(view, "* Ideating… (1m 20s · 2.3k tokens)\n────────\n❯ \n────────\n状态 运行中\n模型 Claude Sonnet\n目录 /project\n上下文 12%");
        view.refresh_agent_screen_state(cx);
        assert_eq!(view.agent_activity.status(), AgentStatus::Working);
        assert_eq!(view.agent_activity.source(), AgentStatusSource::Screen);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
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
            assert_eq!(view.sidebar_activity(), SidebarActivity::Unconfirmed);
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

/// 无集成的嵌套交互 shell（Cygwin fish 等）没有 133;D 完成边；OSC 7 随它
/// 每次提示符渲染到达，必须充当「命令结束、回到提示符」的边，否则进入
/// fish 后每个 Enter 的弱推断标记都永远等不到完成（用户 10-10 报障）。
#[gpui::test]
fn cwd_report_finishes_a_command_inside_an_unintegrated_nested_shell(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        // fish 提示符上的提交：commit_line 的弱推断路径（无 133;C 参与）。
        view.suggest.last_committed = "cd /cygdrive/d/project".into();
        view.mark_command_running();
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
        // fish 画下一个提示符时的 OSC 7 上报：cwd 信号本身不是完成证据，
        // 只排程进程树探测，运行态当场必须维持。
        view.process_event(TermEvent::CwdReport("/cygdrive/d/project".into()), cx);
        assert!(view.command_running, "OSC 7 alone is a cwd signal, not completion");
        assert_eq!(view.cwd, "/cygdrive/d/project", "the report still updates the cwd");
        // 进程树只剩交互 shell（bash 根 + 接管的 fish）→ 回到提示符，清运行态。
        let at_prompt = vec![
            crate::process_tree::ProcessEntry {
                pid: 1,
                parent_pid: 0,
                depth: 0,
                executable: "bash.exe".into(),
            },
            crate::process_tree::ProcessEntry {
                pid: 2,
                parent_pid: 1,
                depth: 1,
                executable: "fish.exe".into(),
            },
        ];
        let started = view.command_started;
        view.apply_osc7_prompt_evidence(started, Ok(at_prompt), cx);
        assert!(!view.command_running);
        assert!(view.command_started.is_none());
        assert_eq!(view.sidebar_activity(), SidebarActivity::Idle);
        // 已被进程反证/完成的命令不受影响；hook 拥有的 agent 回合不走这条弱边。
        view.handle_ai_hook(&hook("osc7-not-mine", "UserPromptSubmit", 1), cx);
        view.mark_command_running();
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
        view.process_event(TermEvent::CwdReport("/cygdrive/d/other".into()), cx);
        let started = view.command_started;
        view.apply_osc7_prompt_evidence(
            started,
            Ok(vec![crate::process_tree::ProcessEntry {
                pid: 1,
                parent_pid: 0,
                depth: 0,
                executable: "bash.exe".into(),
            }]),
            cx,
        );
        assert_eq!(
            view.sidebar_activity(),
            SidebarActivity::Running,
            "a hook-owned turn ignores the weak OSC 7 edge"
        );
    });
}

/// 运行中的命令自己发出的 OSC 7 不是完成证据（评审反例：
/// `bash -c 'printf "]7;file://localhost/tmp"; sleep 60'`）——
/// 进程树里 sleep 还在，弱推断的运行态必须保持。
#[gpui::test]
fn a_running_command_emitting_osc7_does_not_finish_itself(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        view.suggest.last_committed = "bash -c 'printf osc7; sleep 60'".into();
        view.mark_command_running();
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
        // 命令运行中上报了一个目录。
        view.process_event(TermEvent::CwdReport("/tmp".into()), cx);
        assert!(view.command_running, "the cwd report alone changes nothing");
        // 进程树：根 shell → bash -c → sleep —— 存在非 shell 子进程，否决。
        let command_running_tree = vec![
            crate::process_tree::ProcessEntry {
                pid: 1,
                parent_pid: 0,
                depth: 0,
                executable: "bash.exe".into(),
            },
            crate::process_tree::ProcessEntry {
                pid: 2,
                parent_pid: 1,
                depth: 1,
                executable: "bash.exe".into(),
            },
            crate::process_tree::ProcessEntry {
                pid: 3,
                parent_pid: 2,
                depth: 2,
                executable: "sleep.exe".into(),
            },
        ];
        let started = view.command_started;
        view.apply_osc7_prompt_evidence(started, Ok(command_running_tree), cx);
        assert!(view.command_running, "a live command child vetoes the prompt inference");
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
        // sleep 退出后同样的上报才构成回到提示符的证据。
        let at_prompt = vec![
            crate::process_tree::ProcessEntry {
                pid: 1,
                parent_pid: 0,
                depth: 0,
                executable: "bash.exe".into(),
            },
            crate::process_tree::ProcessEntry {
                pid: 2,
                parent_pid: 1,
                depth: 1,
                executable: "fish.exe".into(),
            },
        ];
        let started = view.command_started;
        view.apply_osc7_prompt_evidence(started, Ok(at_prompt), cx);
        assert!(!view.command_running);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Idle);
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

/// SSH pane 里 agent 的两个会话周期：退出后再次启动，新 CLI 进程的
/// SessionStart 必须重新建立身份（图标/状态/摘要的输入），无论上一个会话的
/// 收尾事件是否完整到达（SessionEnd 在 bridge 通道上有真实丢失路径）。
#[gpui::test]
fn ssh_agent_second_launch_reestablishes_identity(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        view.ssh_destination = Some("build@example.test".into());
        view.suggest.suggest_env =
            crate::display::SuggestEnv::Ssh { destination: "build@example.test".into() };

        // ---- 会话 1：用户键入 claude 回车（133;C + bridge SessionStart）----
        view.suggest.last_committed = "claude".into();
        view.process_event(TermEvent::CommandStart, cx);
        assert!(view.handle_ai_hook(&remote_hook("s1", "SessionStart", "100:1", 1), cx));
        assert!(view.handle_ai_hook(&remote_hook("s1", "UserPromptSubmit", "100:1", 2), cx));
        assert_eq!(view.running_program.as_deref(), Some("claude"));
        view.process_event(TermEvent::Title("✳ 修复登录跳转".into()), cx);
        assert_eq!(view.title, "✳ 修复登录跳转", "first session's title is live");
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);

        // A queued shell end cannot retire a still-owned remote Agent.
        view.process_event(TermEvent::CommandDone { exit_code: Some(0) }, cx);
        assert_eq!(view.running_program.as_deref(), Some("claude"));
        assert!(view.handle_ai_hook(&remote_hook("s1", "SessionEnd", "100:1", 3), cx));
        assert_eq!(view.running_program, None, "verified SessionEnd clears identity");

        // ---- 会话 2：新 CLI 进程（process 换新、序号从 1 重来）----
        view.suggest.last_committed = "claude".into();
        view.process_event(TermEvent::CommandStart, cx);
        assert!(
            view.handle_ai_hook(&remote_hook("s2", "SessionStart", "200:9", 1), cx),
            "the second session's SessionStart must be accepted"
        );
        assert_eq!(view.running_program.as_deref(), Some("claude"));
        view.process_event(TermEvent::Title("✳ 继续修分页".into()), cx);
        assert_eq!(view.title, "✳ 继续修分页", "second session's title is live");
        assert!(
            view.handle_ai_hook(&remote_hook("s2", "UserPromptSubmit", "200:9", 2), cx),
            "the second session's turn hooks must be accepted"
        );
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);

        // ---- 变体：SessionEnd 完整到达的收尾同样不得卡住下一次进入 ----
        assert!(view.handle_ai_hook(&remote_hook("s2", "SessionEnd", "200:9", 3), cx));
        view.suggest.last_committed = "claude".into();
        view.process_event(TermEvent::CommandStart, cx);
        assert!(view.handle_ai_hook(&remote_hook("s3", "SessionStart", "300:4", 1), cx));
        assert_eq!(view.running_program.as_deref(), Some("claude"));
    });
}

/// CLI 2.x 底部 chrome 长到 5 个非空行的 idle 屏（ai_agents.rs 同源 fixture）：
/// ❯ 在自底向上第 6 行，prompt_idle 命中；❯ 后是 NBSP。
const CLAUDE_IDLE_SCREEN: &str = "╭─── Claude Code v2.1.286 ───╮\n\
                                   ────────────────────────────────────────────────────\n\
                                   ❯\u{a0}\n\
                                   ────────────────────────────────────────────────────\n\
                                   \x20 ⚠ Transcript saving is off — inherited session\n\
                                   \x20 🧠glm-5.3 | 📁Fleet | 🌿feature/left_tree*\n\
                                   \x20 ⏳5h 🟢 20% 2h7m | 📅MCP 13/1000\n\
                                   \x20 ⏵⏵ auto mode on (shift+tab to cycle)";

/// 同款加宽 chrome 的工作中屏幕：转圈 + esc to interrupt 同屏，spinner_working
/// 判 Working——停滞对账不得在干活时收边。
const CLAUDE_WORKING_SCREEN: &str = "  ⎿  Running…\n\
                                     ✶ Moseying… (8m 42s · ↓ 18.3k tokens)\n\
                                     ────────────────────────────────────────\n\
                                     ❯ \n\
                                     ────────────────────────────────────────\n\
                                     \x20 ⏵⏵ bypass permissions on (shift+tab to cycle)\n\
                                     \x20 ↳ queued\n\
                                     \x20 ⏸ esc to interrupt · ? for shortcuts · ← for agents";

/// 远端 Stop hook 丢失（bridge 通道锁/超时/超限都会丢）时，hook 权威把回合钉在
/// Working。远端缺少进程树证据时，重复空闲屏幕仍不能证明完成；
/// 迟到的权威 Stop 才能结束回合，CLI 身份保留给命令边界。
#[gpui::test]
fn remote_lost_stop_is_not_completed_by_stalled_idle_screen(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        view.ssh_destination = Some("build@example.test".into());
        view.suggest.suggest_env =
            crate::display::SuggestEnv::Ssh { destination: "build@example.test".into() };
        // fixture 先对着 detect() 钉一遍，防止规则演进让这组断言静默失效。
        assert_eq!(
            crate::ai_agents::detect("claude", CLAUDE_IDLE_SCREEN).unwrap().status,
            AgentStatus::Idle
        );
        assert_eq!(
            crate::ai_agents::detect("claude", CLAUDE_WORKING_SCREEN).unwrap().status,
            AgentStatus::Working
        );
        // 回合起跑；Stop 在远端丢失。
        view.handle_ai_hook(&hook("lost-stop", "UserPromptSubmit", 1), cx);
        assert_eq!(view.agent_activity.status(), AgentStatus::Working);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
        // agent 已答完：屏幕是 idle 提示符，但 hook 静默未超阈值——不得收边。
        screen(view, CLAUDE_IDLE_SCREEN);
        for _ in 0..4 {
            view.refresh_agent_screen_state(cx);
        }
        assert_eq!(
            view.agent_activity.status(),
            AgentStatus::Working,
            "a live hook timeline outranks the idle screen"
        );
        // Even a stalled timeline needs an authoritative completion.
        view.agent_activity.backdate_activity_for_test(
            super::REMOTE_TURN_STALL + std::time::Duration::from_secs(1),
        );
        view.refresh_agent_screen_state(cx);
        assert_eq!(view.agent_activity.status(), AgentStatus::Working, "one sample is not enough");
        view.refresh_agent_screen_state(cx);
        assert_eq!(view.agent_activity.status(), AgentStatus::Working);
        assert_eq!(view.runtime_task_state(), crate::runtime_api::RuntimeTaskState::Running);
        assert_eq!(view.running_program.as_deref(), Some("claude"), "identity survives the turn");
        assert!(view.handle_ai_hook(&hook("lost-stop", "Stop", 2), cx));
        assert_eq!(view.runtime_task_state(), crate::runtime_api::RuntimeTaskState::Finished);
    });
}

/// 工作中的屏幕（转圈/中断提示）反证「回合已结束」：连续 idle 计数归零，
/// 之后即使停滞依旧，也要重新攒满两拍。
#[gpui::test]
fn a_working_screen_resets_the_stall_reconciliation_samples(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        view.ssh_destination = Some("build@example.test".into());
        view.suggest.suggest_env =
            crate::display::SuggestEnv::Ssh { destination: "build@example.test".into() };
        view.handle_ai_hook(&hook("working-screen", "UserPromptSubmit", 1), cx);
        view.agent_activity.backdate_activity_for_test(
            super::REMOTE_TURN_STALL + std::time::Duration::from_secs(1),
        );
        screen(view, CLAUDE_IDLE_SCREEN);
        view.refresh_agent_screen_state(cx);
        // 攒到第 2 拍之前屏幕翻回转圈：计数归零，回合继续。
        screen(view, CLAUDE_WORKING_SCREEN);
        view.refresh_agent_screen_state(cx);
        assert_eq!(view.agent_activity.status(), AgentStatus::Working);
        screen(view, CLAUDE_IDLE_SCREEN);
        view.refresh_agent_screen_state(cx);
        assert_eq!(
            view.agent_activity.status(),
            AgentStatus::Working,
            "the counter restarted from the working frame"
        );
        view.refresh_agent_screen_state(cx);
        assert_eq!(view.agent_activity.status(), AgentStatus::Working);
    });
}

/// 停滞对账只属于远端：本地 pane 有进程树这第二重证据，hook 权威不被屏幕
/// 侵蚀（等价加固 `hook_lifecycle_cannot_be_rewritten_by_screen_words_or_idle_samples`）。
#[gpui::test]
fn local_hook_authority_is_never_eroded_by_stall_reconciliation(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        view.suggest.suggest_env = crate::display::SuggestEnv::Local;
        view.handle_ai_hook(&hook("local-authority", "UserPromptSubmit", 1), cx);
        view.agent_activity.backdate_activity_for_test(super::REMOTE_TURN_STALL * 2);
        screen(view, CLAUDE_IDLE_SCREEN);
        for _ in 0..4 {
            view.refresh_agent_screen_state(cx);
        }
        assert_eq!(
            view.agent_activity.status(),
            AgentStatus::Working,
            "local panes keep the process tree as their second witness"
        );
    });
}

/// 远端 UserPromptSubmit 丢失（bridge 通道锁 busy/超时/超限都会丢）时，上一
/// 回合的 Stop 把回合钉在 Done（侧栏 ready）；停滞超阈值 + 连续两拍转圈屏
/// 重新点火 Running；身份保留给命令边界。
#[gpui::test]
fn remote_lost_prompt_submit_is_reignited_by_stalled_working_screen(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        view.ssh_destination = Some("build@example.test".into());
        view.suggest.suggest_env =
            crate::display::SuggestEnv::Ssh { destination: "build@example.test".into() };
        // 第一回合完整：UserPromptSubmit → Stop（Done，侧栏读作 ready）。
        view.handle_ai_hook(&hook("lost-prompt", "UserPromptSubmit", 1), cx);
        view.handle_ai_hook(&hook("lost-prompt", "Stop", 2), cx);
        assert_eq!(view.agent_activity.status(), AgentStatus::Done);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Done);
        // 第二回合的 UserPromptSubmit 在远端丢失；屏幕转圈但 hook 静默未超
        // 阈值——hook 时间线仍然在场，屏幕不得点火。
        screen(view, CLAUDE_WORKING_SCREEN);
        for _ in 0..4 {
            view.refresh_agent_screen_state(cx);
        }
        assert_eq!(
            view.agent_activity.status(),
            AgentStatus::Done,
            "a live hook timeline outranks the working screen"
        );
        // 停滞超阈值：连续两拍转圈重新点火 Running；CLI 还活着，身份不动。
        view.agent_activity.backdate_activity_for_test(
            super::REMOTE_TURN_STALL + std::time::Duration::from_secs(1),
        );
        view.refresh_agent_screen_state(cx);
        assert_eq!(view.agent_activity.status(), AgentStatus::Done, "one sample is not enough");
        view.refresh_agent_screen_state(cx);
        assert_eq!(view.agent_activity.status(), AgentStatus::Working);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
        assert_eq!(view.running_program.as_deref(), Some("claude"), "identity survives the turn");
    });
}

#[gpui::test]
fn output_rule_source_does_not_turn_a_working_codex_into_attention(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        view.running_program = Some("codex".into());
        screen(view, "  { contains = [\"(y/n)\"] },\n  { contains = [\"[y/n]\"] },\n]\n\n◦ Working (12m 36s • esc to interrupt)\n\n› Ask Codex to do anything\ngpt-6-astra medium · Context 4% used\n? for shortcuts");
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
        ("Completed answer.", AgentStatus::Working),
        ("■ stream disconnected before completion: failed to send request", AgentStatus::Failed),
        ("■ Rate limit exceeded", AgentStatus::Failed),
        ("■ Turn interrupted", AgentStatus::Working),
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
                RuntimeTaskState::Running
            };
            let expected_sidebar = if expected == AgentStatus::Failed {
                SidebarActivity::Failed
            } else {
                SidebarActivity::Unconfirmed
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
fn claude_activity_chrome_projects_work_and_idle_without_success(cx: &mut TestAppContext) {
    use crate::runtime_api::RuntimeTaskState;
    let (view, window, _) = open(cx);
    let notifications = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let observed = notifications.clone();
    let _subscription = view.update(window, |_, cx| {
        cx.subscribe(&view, move |_, _, event, _| {
            if let TerminalViewEvent::Notification(notification) = event {
                observed.borrow_mut().push(notification.clone());
            }
        })
    });
    view.update(window, |view, cx| {
        view.running_program = Some("claude".into());
        view.command_running = true;
        view.agent_activity.begin_command(true);
        let composer = "────────\n❯ \n────────\n? for shortcuts";
        for activity in ["✻ Thinking… (3m 11s · ↓ 122 tokens)", "✢ Compacting conversation… (1s)"]
        {
            screen(view, &format!("{activity}\n\n{composer}"));
            for _ in 0..4 {
                view.refresh_agent_screen_state(cx);
            }
            assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
            assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
            screen(view, &format!("✻ Baked for 15m 24s\n{composer}"));
            for _ in 0..4 {
                view.refresh_agent_screen_state(cx);
            }
            assert_eq!(view.runtime_task_state(), RuntimeTaskState::Idle);
            assert_eq!(view.sidebar_activity(), SidebarActivity::Idle);
            assert!(!view.agent_activity.hook_seen());
        }
    });
    assert!(notifications.borrow().is_empty());
}

#[gpui::test]
fn labeled_claude_form_and_task_chrome_reach_sidebar_activity(cx: &mut TestAppContext) {
    use crate::runtime_api::RuntimeTaskState;

    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        view.running_program = Some("claude".into());
        view.command_running = true;
        view.agent_activity.begin_command(true);
        let tasks = "\n\n  3 tasks (2 done, 1 in progress, 0 open)\n  ✔ classify\n  ◼ sync";
        let form = format!(
            "  Thought for 5m 35s, ran 4 shell commands\n────\n ☐ scope\n\n❯ 1. portable\n  2. all\n────\nEnter to select · Esc to cancel\n──── magpie ─{tasks}"
        );
        screen(view, &form);
        for _ in 0..4 {
            view.refresh_agent_screen_state(cx);
        }
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Attention);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Attention);

        let working = format!(
            "✢ Compacting conversation… (2s)\n────\n❯ \n──── magpie ─{tasks}"
        );
        screen(view, &working);
        for _ in 0..4 {
            view.refresh_agent_screen_state(cx);
        }
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);

        let idle = format!(
            "✻ Baked for 15m 24s\n────\n❯ \n──── magpie ─{tasks}"
        );
        screen(view, &idle);
        for _ in 0..4 {
            view.refresh_agent_screen_state(cx);
        }
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Idle);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Idle);
    });
}

#[gpui::test]
fn agent_scroll_history_viewport_preserves_activity_and_recovers_at_bottom(
    cx: &mut TestAppContext,
) {
    use crate::runtime_api::RuntimeTaskState;
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        view.running_program = Some("claude".into());
        view.command_running = true;
        view.agent_activity.begin_command(true);
        let composer = "────────\n❯ \n────────\n? for shortcuts";
        screen(view, &format!("✻ Thinking… (3m 11s · ↓ 122 tokens)\n\n{composer}"));
        view.refresh_agent_screen_state(cx);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
        for banner in ["Jump to bottom (ctrl+End) ↓", "1 new message (ctrl+End) ↓"] {
            screen(view, &format!("Old completed answer.\n{banner}\n{composer}"));
            for _ in 0..4 {
                view.refresh_agent_screen_state(cx);
            }
            assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running, "{banner}");
            assert_eq!(view.sidebar_activity(), SidebarActivity::Running, "{banner}");
            assert_eq!(view.running_program.as_deref(), Some("claude"));
        }
        screen(view, &format!("✻ Baked for 15m 24s\n{composer}"));
        for _ in 0..4 {
            view.refresh_agent_screen_state(cx);
        }
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Idle);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Idle);
    });
}

#[gpui::test]
fn terminal_and_composerless_history_freeze_the_last_current_observation(cx: &mut TestAppContext) {
    use crate::runtime_api::RuntimeTaskState;
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        view.running_program = Some("claude".into());
        view.command_running = true;
        view.agent_activity.begin_command(true);
        let composer = "────────\n❯ \n────────\n? for shortcuts";
        screen(view, &format!("✻ Thinking… (3m 11s)\n{composer}"));
        for _ in 0..4 {
            view.refresh_agent_screen_state(cx);
        }
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);

        screen(view, &format!("✻ Baked for 15m 24s\n{composer}"));
        for row in 0..48 {
            feed(view, format!("\r\nhistory row {row}").as_bytes());
        }
        {
            let mut term = view.session.as_ref().unwrap().term.lock();
            term.scroll_display(Scroll::Top);
            assert!(term.grid().display_offset() > 0);
        }
        for _ in 0..4 {
            view.refresh_agent_screen_state(cx);
        }
        assert_eq!(
            view.runtime_task_state(),
            RuntimeTaskState::Running,
            "terminal scrollback is not current-turn evidence"
        );
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);

        view.session.as_ref().unwrap().term.lock().scroll_display(Scroll::Bottom);
        screen(view, "Old completed answer without current composer chrome.");
        for _ in 0..4 {
            view.refresh_agent_screen_state(cx);
        }
        assert_eq!(
            view.runtime_task_state(),
            RuntimeTaskState::Running,
            "a viewport without the current composer must not erase live state"
        );
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
    });
}

#[gpui::test]
fn wsl_hook_claude_turn_cancel_requires_own_escape_and_current_idle(cx: &mut TestAppContext) {
    use crate::runtime_api::RuntimeTaskState;
    let (view, window, input) = open(cx);
    let notifications = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let observed = notifications.clone();
    let _subscription = view.update(window, |_, cx| {
        cx.subscribe(&view, move |_, _, event, _| {
            if let TerminalViewEvent::Notification(note) = event {
                observed.borrow_mut().push(note.clone());
            }
        })
    });
    view.update(window, |view, cx| {
        let event = |name: &str, prompt: &str, sequence: u64| {
            let payload = serde_json::json!({"hook_event_name": name, "session_id": "turn-cancel",
                "prompt_id": prompt, "bridge_sequence": sequence});
            crate::ai_hook::parse_remote_envelope(
                format!("nebula-hook/1 source=claude process=4100:70\n{payload}").as_bytes(),
                Some(42),
            )
            .unwrap()
        };
        assert!(view.handle_ai_hook(&event("UserPromptSubmit", "first", 1), cx));
        let composer = "────────\n❯ Please wait.\n────────\n? for shortcuts";
        screen(view, composer);
        for _ in 0..4 {
            view.refresh_agent_screen_state(cx);
        }
        assert_eq!(
            view.runtime_task_state(),
            RuntimeTaskState::Running,
            "idle alone cannot cancel"
        );
        view.write_input(b"\x1b[A".to_vec(), cx);
        for _ in 0..4 {
            view.refresh_agent_screen_state(cx);
        }
        assert_eq!(
            view.runtime_task_state(),
            RuntimeTaskState::Running,
            "arrow is not cancellation"
        );
        screen(view, &format!("✻ Thinking… (2s)\n\n{composer}"));
        view.write_input(b"\x1b".to_vec(), cx);
        for _ in 0..4 {
            view.refresh_agent_screen_state(cx);
        }
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running, "wait for CLI idle");
        screen(view, composer);
        for _ in 0..4 {
            view.refresh_agent_screen_state(cx);
        }
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Idle);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Idle);
        assert!(!view.agent_activity.allows_screen());
        for (index, name) in ["Stop", "PostToolUse", "PermissionRequest"].into_iter().enumerate() {
            let late = event(name, "first", index as u64 + 2);
            assert!(!view.agent_activity.accepts_hook(&late));
            view.handle_ai_hook(&late, cx);
            assert_eq!(view.runtime_task_state(), RuntimeTaskState::Idle);
        }
        view.agent_activity.submitted();
        assert!(
            !view.agent_activity.accepts_hook(&event("Stop", "first", 5)),
            "local submission cannot revive a cancelled turn"
        );
        assert!(view.handle_ai_hook(&event("UserPromptSubmit", "second", 5), cx));
        view.write_input(b"\x1b".to_vec(), cx);
        assert!(view.handle_ai_hook(&event("UserPromptSubmit", "third", 6), cx));
        for _ in 0..4 {
            view.refresh_agent_screen_state(cx);
        }
        assert_eq!(
            view.runtime_task_state(),
            RuntimeTaskState::Running,
            "new prompt retires cancel intent"
        );
        assert!(
            input
                .try_iter()
                .any(|message| matches!(message, Msg::Input(bytes) if bytes.as_ref() == b"\x1b"))
        );
    });
    assert!(notifications.borrow().is_empty(), "cancellation is not success or failure");
}

#[gpui::test]
fn wsl_hook_claude_cancel_preserves_chrome_rows_after_a_full_width_redraw(cx: &mut TestAppContext) {
    use crate::runtime_api::RuntimeTaskState;
    use nebula_terminal::grid::Dimensions as _;
    use nebula_terminal::index::{Column, Line};
    use nebula_terminal::term::cell::Flags;
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        let payload = serde_json::json!({"hook_event_name":"PreCompact", "session_id":"wrapped-cancel",
            "prompt_id":"compact", "trigger":"manual", "bridge_sequence":1});
        let event = crate::ai_hook::parse_remote_envelope(
            format!("nebula-hook/1 source=claude process=4100:70\n{payload}").as_bytes(), Some(42)
        ).unwrap();
        assert!(view.handle_ai_hook(&event, cx));
        let columns = view.session.as_ref().unwrap().term.lock().columns();
        let rule = "─".repeat(columns);
        screen(view, &format!("{rule}\n❯ /compact\n{rule}\n  ⏵⏵ auto mode on"));
        {
            let mut term = view.session.as_ref().unwrap().term.lock();
            for row in [0, 2] {
                term.grid_mut()[Line(row)][Column(columns - 1)].flags.insert(Flags::WRAPLINE);
            }
        }
        view.write_input(b"\x1b".to_vec(), cx);
        for _ in 0..4 { view.refresh_agent_screen_state(cx); }
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Idle, "visual rows remain separate even when copy text joins wrapped lines");
    });
}

#[gpui::test]
fn claude_compaction_cancel_requires_own_input_and_current_idle_chrome(cx: &mut TestAppContext) {
    use crate::runtime_api::RuntimeTaskState;
    for (index, keyboard_mode) in ["", "\x1b[>1u", "\x1b[?9001h"].into_iter().enumerate() {
        let (view, window, input) = open(cx);
        view.update(window, |view, cx| {
            let event = |name: &str, prompt: &str, sequence: u64| {
                let payload = serde_json::json!({
                    "hook_event_name": name, "session_id": format!("compact-cancel-{index}"),
                    "prompt_id": prompt, "bridge_sequence": sequence, "trigger": "manual",
                });
                crate::ai_hook::parse_remote_envelope(
                    format!("nebula-hook/1 source=claude process=4100:70\n{payload}").as_bytes(),
                    Some(42),
                )
                .unwrap()
            };
            assert!(view.handle_ai_hook(&event("SessionStart", "", 1), cx));
            assert!(view.handle_ai_hook(&event("PreCompact", "compact", 2), cx));
            let composer = "────────\n❯ /compact\n────────\n? for shortcuts";
            screen(view, composer);
            for _ in 0..4 {
                view.refresh_agent_screen_state(cx);
            }
            assert_eq!(
                view.runtime_task_state(),
                RuntimeTaskState::Running,
                "idle text alone has no authority"
            );
            view.write_input(b"\x1b[A".to_vec(), cx);
            for _ in 0..4 {
                view.refresh_agent_screen_state(cx);
            }
            assert_eq!(
                view.runtime_task_state(),
                RuntimeTaskState::Running,
                "arrow is not cancellation"
            );
            screen(view, &format!("✢ Compacting conversation… (2s)\n\n{composer}"));
            feed(view, keyboard_mode.as_bytes());
            let escape = super::super::keymap::encode(
                &gpui::Keystroke::parse("escape").unwrap(),
                &view.term_mode(),
            )
            .unwrap();
            view.write_input(escape.clone(), cx);
            for _ in 0..4 {
                view.refresh_agent_screen_state(cx);
            }
            assert_eq!(
                view.runtime_task_state(),
                RuntimeTaskState::Running,
                "input is intent until the CLI leaves work"
            );
            screen(view, composer);
            for _ in 0..4 {
                view.refresh_agent_screen_state(cx);
            }
            assert_eq!(view.runtime_task_state(), RuntimeTaskState::Idle);
            assert_eq!(view.sidebar_activity(), SidebarActivity::Idle);
            assert!(view.agent_activity.hook_seen());
            assert!(!view.agent_activity.allows_screen(), "no general screen override");
            assert!(
                input.try_iter().any(
                    |message| matches!(message, Msg::Input(bytes) if bytes.as_ref() == escape)
                )
            );
            assert!(view.handle_ai_hook(&event("UserPromptSubmit", "continue", 3), cx));
            view.handle_ai_hook(&event("PostCompact", "compact", 4), cx);
            view.handle_ai_hook(&event("Stop", "compact", 5), cx);
            assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
        });
    }
}

#[gpui::test]
fn live_shell_owner_survives_a_late_command_boundary_without_session_end(cx: &mut TestAppContext) {
    use crate::runtime_api::RuntimeTaskState;

    let (view, window, _) = open(cx);
    let event = |name: &str, sequence: u64| {
        let payload = serde_json::json!({
            "hook_event_name": name,
            "session_id": "stale-command-boundary",
            "turn_id": "live-turn",
            "bridge_sequence": sequence,
        });
        crate::ai_hook::parse_remote_envelope(
            format!(
                "nebula-hook/1 source=codex process=1316:7632 codex_hooks=full pane=42\n{payload}"
            )
            .as_bytes(),
            Some(42),
        )
        .unwrap()
    };

    view.update(window, |view, cx| {
        view.running_program = Some("codex".into());
        view.command_running = true;
        assert!(view.handle_ai_hook(&event("UserPromptSubmit", 1), cx));
        assert!(view.agent_activity.hook_seen());
        assert_eq!(view.agent_activity.status(), AgentStatus::Working);
        assert_eq!(
            view.session_agent().unwrap().session_id.as_deref(),
            Some("stale-command-boundary")
        );

        let owner = view.agent_activity.shell_boundary_owner().unwrap();
        view.shell_boundary_probe = Some(super::agent_activity::PendingShellBoundary {
            owner: owner.clone(),
            exit_code: Some(0),
            attempts: 0,
        });
        view.resolve_probed_shell_boundary(
            &owner,
            crate::ai_hook::lifecycle::ShellOwnerStatus::Alive,
            cx,
        );

        assert!(
            view.agent_activity.hook_seen(),
            "a late shell edge must retain the live Hook owner"
        );
        assert_eq!(view.agent_activity.status(), AgentStatus::Working);
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
        assert_eq!(view.running_program.as_deref(), Some("codex"));
        assert_eq!(
            view.session_agent().unwrap().session_id.as_deref(),
            Some("stale-command-boundary")
        );

        assert!(view.handle_ai_hook(&event("PostToolUse", 2), cx));
        assert_eq!(view.agent_activity.status(), AgentStatus::Working);
    });
}

#[gpui::test]
fn dead_shell_owner_finishes_the_command_without_waiting_for_session_end(cx: &mut TestAppContext) {
    use crate::runtime_api::RuntimeTaskState;

    let (view, window, _) = open(cx);
    let event = crate::ai_hook::parse_remote_envelope(
        br#"nebula-hook/1 source=codex process=1316:7632 codex_hooks=full pane=42
{"hook_event_name":"UserPromptSubmit","session_id":"real-command-end","turn_id":"live-turn","bridge_sequence":1}"#,
        Some(42),
    )
    .unwrap();

    view.update(window, |view, cx| {
        view.running_program = Some("codex".into());
        view.command_running = true;
        assert!(view.handle_ai_hook(&event, cx));
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);

        let owner = view.agent_activity.shell_boundary_owner().unwrap();
        view.shell_boundary_probe = Some(super::agent_activity::PendingShellBoundary {
            owner: owner.clone(),
            exit_code: Some(0),
            attempts: 0,
        });
        view.resolve_probed_shell_boundary(
            &owner,
            crate::ai_hook::lifecycle::ShellOwnerStatus::Ended,
            cx,
        );

        assert!(!view.agent_activity.hook_seen());
        assert!(view.running_program.is_none());
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Idle);
    });
}

#[gpui::test]
fn next_command_releases_a_pending_shell_probe_before_the_new_hook(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    let old = crate::ai_hook::parse_remote_envelope(
        b"nebula-hook/1 source=codex process=1316:7632 codex_hooks=full pane=42\n{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"old-codex\",\"turn_id\":\"old-turn\",\"bridge_sequence\":1}",
        Some(42),
    )
    .unwrap();
    let next = crate::ai_hook::parse_remote_envelope(
        b"nebula-hook/1 source=claude process=9523:900 pane=42\n{\"hook_event_name\":\"SessionStart\",\"session_id\":\"new-claude\",\"bridge_sequence\":1}",
        Some(42),
    )
    .unwrap();

    view.update(window, |view, cx| {
        view.running_program = Some("codex".into());
        view.command_running = true;
        assert!(view.handle_ai_hook(&old, cx));
        let owner = view.agent_activity.shell_boundary_owner().unwrap();
        view.shell_boundary_probe = Some(super::agent_activity::PendingShellBoundary {
            owner,
            exit_code: Some(0),
            attempts: 0,
        });
        view.suggest.last_committed = "claude".into();

        view.on_command_start(cx);

        assert!(!view.agent_activity.hook_seen());
        assert!(view.shell_boundary_probe.is_none());
        assert_eq!(view.running_program.as_deref(), Some("claude"));
        assert!(view.handle_ai_hook(&next, cx));
        assert_eq!(
            view.session_agent().and_then(|agent| agent.session_id),
            Some("new-claude".into())
        );
    });
}

#[gpui::test]
fn unavailable_shell_owner_probe_preserves_the_live_hook(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    let event = crate::ai_hook::parse_remote_envelope(
        b"nebula-hook/1 source=codex process=1316:7632 codex_hooks=full pane=42\n{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"unavailable-boundary\",\"turn_id\":\"live-turn\",\"bridge_sequence\":1}",
        Some(42),
    )
    .unwrap();

    view.update(window, |view, cx| {
        view.running_program = Some("codex".into());
        view.command_running = true;
        assert!(view.handle_ai_hook(&event, cx));
        let owner = view.agent_activity.shell_boundary_owner().unwrap();
        view.shell_boundary_probe = Some(super::agent_activity::PendingShellBoundary {
            owner: owner.clone(),
            exit_code: Some(0),
            attempts: 4,
        });

        view.resolve_probed_shell_boundary(
            &owner,
            crate::ai_hook::lifecycle::ShellOwnerStatus::Unavailable,
            cx,
        );

        assert!(view.agent_activity.hook_seen());
        assert_eq!(view.agent_activity.shell_boundary_owner().as_ref(), Some(&owner));
        assert_eq!(
            view.pending_shell_boundary().map(|pending| pending.attempts),
            Some(5),
            "the exhausted retry state stays unresolved instead of clearing evidence"
        );
    });
}

#[gpui::test]
fn agent_scroll_success_retires_screen_error_and_next_prompt_baseline(cx: &mut TestAppContext) {
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
    view.update(window, |view, cx| {
        let error = "API Error: stream error: stream disconnected before completion";
        let composer = "────────\n❯ \n────────\n? for shortcuts";
        assert!(view.handle_ai_hook(&hook("badge-lifecycle", "UserPromptSubmit", 1), cx));
        screen(view, &format!("{error}\n{composer}"));
        view.refresh_agent_screen_state(cx);
        assert!(view.screen_error_indicator());
        assert!(view.handle_ai_hook(&hook("badge-lifecycle", "Stop", 2), cx));
        assert!(!view.screen_error_indicator(), "successful completion clears the badge");
        for _ in 0..4 {
            view.refresh_agent_screen_state(cx);
        }
        assert!(!view.screen_error_indicator(), "old error cannot relight after success");
        assert!(view.handle_ai_hook(&hook("badge-lifecycle", "UserPromptSubmit", 3), cx));
        view.refresh_agent_screen_state(cx);
        assert!(!view.screen_error_indicator(), "new prompt does not inherit old error");
        screen(view, &format!("{error}: another attempt failed\n{composer}"));
        view.refresh_agent_screen_state(cx);
        assert!(view.screen_error_indicator(), "a new current error remains visible");
        assert!(view.handle_ai_hook(&hook("badge-lifecycle", "StopFailure", 4), cx));
        view.refresh_agent_screen_state(cx);
        assert!(view.screen_error_indicator(), "failure remains visible");
    });
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
            "■ We’re currently experiencing high demand, which may cause temporary errors.\n› Ask Codex to do anything\ngpt-6 medium · Context 2% used",
        );
        view.refresh_agent_screen_state(cx);
        assert!(view.screen_error_indicator());
        assert_eq!(view.agent_activity.status(), hook_status);
        assert_eq!(view.agent_activity.status(), AgentStatus::Working);
        assert_eq!(view.agent_activity.source(), AgentStatusSource::Hook);

        screen(
            view,
            "Working on the request...\n› Ask Codex to do anything\ngpt-6 medium · Context 2% used",
        );
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
fn rejected_hook_does_not_report_an_applied_event_to_the_dispatcher(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    let event = |name: &str, turn: &str| {
        let payload = serde_json::json!({
            "hook_event_name": name, "session_id": "rejected-hook-result", "turn_id": turn,
        });
        crate::ai_hook::parse_remote_envelope(
            format!("nebula-hook/1 source=codex codex_hooks=full pane=42\n{payload}").as_bytes(),
            Some(42),
        )
        .unwrap()
    };
    view.update(window, |view, cx| {
        assert!(view.handle_ai_hook(&event("UserPromptSubmit", "new"), cx));
        let old = event("Stop", "old");
        assert!(!view.agent_activity.accepts_hook(&old));
        assert!(!view.handle_ai_hook(&old, cx), "routing is not evidence of applying a Hook");
        assert_eq!(view.agent_activity.status(), AgentStatus::Working);
        assert!(view.handle_ai_hook(&event("Stop", "new"), cx));
        assert!(!view.handle_ai_hook(&event("Stop", "new"), cx), "duplicate is not applied");
    });
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
fn agent_shift_enter_reaches_pty_without_committing_shell_history(cx: &mut TestAppContext) {
    for (program, expected) in [("claude", b"\n".as_slice()), ("codebuddy", b"\x1b\r")] {
        let (view, window, receiver) = open(cx);
        window.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.running_program = Some(program.into());
                view.suggest.last_committed = program.into();
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
                assert_eq!(view.suggest.last_committed, program);
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
        assert_eq!(bytes, expected);
    }
}

#[gpui::test]
fn fullscreen_tui_keeps_running_state_without_spinning_the_badge(cx: &mut TestAppContext) {
    use crate::runtime_api::RuntimeTaskState;

    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        view.mark_command_running();
        view.running_program = Some("lazygit".into());
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);

        // lazygit/btop 占据 alternate screen：在等人操作，不转圈（#805）。
        feed(view, b"\x1b[?1049h");
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Idle);

        // 自报进度的全屏程序仍由 OSC 9;4 驱动 badge。
        view.progress = crate::taskbar::TaskProgress::Indeterminate;
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
        view.progress = crate::taskbar::TaskProgress::None;

        // 未就绪的 SSH 连接是真正的等待。
        view.ssh_stage = Some(crate::ssh_session::SshStage::Authenticate);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
        view.ssh_stage = Some(crate::ssh_session::SshStage::Ready);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Idle);

        // 全屏形态的 Agent 照常转圈。
        view.running_program = Some("opencode".into());
        assert_eq!(view.sidebar_activity(), SidebarActivity::Unconfirmed);
        view.running_program = Some("lazygit".into());

        // 回到主屏（程序退出前的长尾输出）恢复 spinner。
        feed(view, b"\x1b[?1049l");
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
    });
}

#[gpui::test]
fn fullscreen_multiplexer_keeps_spinning_for_hook_owned_work(cx: &mut TestAppContext) {
    for program in ["tmux", "screen"] {
        let (view, window, _) = open(cx);
        view.update(window, |view, cx| {
            view.mark_command_running();
            view.running_program = Some(program.into());
            feed(view, b"\x1b[?1049h");
            assert_eq!(view.sidebar_activity(), SidebarActivity::Idle, "{program}");

            let session = format!("inside-{program}");
            assert!(view.handle_ai_hook(&hook(&session, "UserPromptSubmit", 1), cx));
            assert_eq!(view.sidebar_activity(), SidebarActivity::Running, "{program}");

            // Hook ownership still wins when a process probe reports the multiplexer.
            view.running_program = Some(program.into());
            assert_eq!(view.sidebar_activity(), SidebarActivity::Running, "{program}");
        });
    }
}
