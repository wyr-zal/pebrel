use super::*;
use crate::gpui_shell::terminal::view::{SidebarActivity, TermEvent, startup_tests::open};
use crate::runtime_api::RuntimeAgentStateSource;
use gpui::TestAppContext;
use nebula_terminal::event_loop::StreamProcessor;

struct Wire {
    stream: StreamProcessor,
    events: crate::gpui_shell::terminal::event_mailbox::EventReceiver,
    proxy: crate::gpui_shell::terminal::session::EventProxy,
}

#[gpui::test]
fn osc7501_identity_never_borrows_another_clients_session(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        let mut wire = Wire::install(view);
        view.running_program = Some("codex".into());
        view.ai_session = Some(crate::display::AiSessionIdentity {
            source: "codex".into(),
            session_id: "old-codex-session".into(),
        });
        wire.feed(view, cx, b"\x1b]7501;state=blocked:app=claude-code\x07");
        let agent = view.runtime_agent().unwrap();
        assert_eq!(agent.kind, "claude");
        assert_eq!(agent.session_id, None);
        wire.feed(view, cx, b"\x1b]7501;state=working:app=cargo\x07");
        assert!(view.runtime_agent().is_none());
    });
}

impl Wire {
    fn install(view: &mut TerminalView) -> Self {
        let (session, _, events, proxy) =
            crate::gpui_shell::terminal::session::test_session_with_events();
        view.session = Some(session);
        Self { stream: StreamProcessor::default(), events, proxy }
    }

    fn feed(&mut self, view: &mut TerminalView, cx: &mut Context<TerminalView>, bytes: &[u8]) {
        {
            let mut term = view.session.as_ref().unwrap().term.lock();
            self.stream.feed(&mut term, &self.proxy, bytes);
        }
        while let Ok(event) = self.events.try_recv() {
            view.process_event(event, cx);
        }
    }
}

#[gpui::test]
fn osc7501_message_only_updates_do_not_refresh_workspace_status(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    let updates = std::rc::Rc::new(std::cell::Cell::new(0));
    let count = updates.clone();
    let _subscription = window.update(|_, cx| {
        cx.subscribe(&view, move |_, event, _| {
            if matches!(event, TerminalViewEvent::TitleChanged) {
                count.set(count.get() + 1);
            }
        })
    });
    let mut wire = view.update(window, |view, _| Wire::install(view));
    view.update(window, |view, cx| wire.feed(view, cx, b"\x1b]7501;state=working:app=codex\x07"));
    let before = updates.get();
    assert!(before > 0);
    view.update(window, |view, cx| {
        wire.feed(view, cx, b"\x1b]7501;state=working:app=codex:msg=VGVzdA==\x07");
        wire.feed(view, cx, b"\x1b]7501;state=working:app=codex:title=VGVzdA==\x07");
    });
    assert_eq!(updates.get(), before);
}

#[gpui::test]
fn osc7501_drives_agent_state_child_priority_and_progress_without_notifications(
    cx: &mut TestAppContext,
) {
    let (view, window, _) = open(cx);
    let notifications = std::rc::Rc::new(std::cell::Cell::new(0));
    let count = notifications.clone();
    let _subscription = window.update(|_, cx| {
        cx.subscribe(&view, move |_, event, _| {
            if matches!(event, TerminalViewEvent::Notification(_)) {
                count.set(count.get() + 1);
            }
        })
    });
    view.update(window, |view, cx| {
        let mut wire = Wire::install(view);
        wire.feed(view, cx, b"\x1b]7501;state=working:app=codex:progress=35\x07");
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
        assert_eq!(view.progress, TaskProgress::Value(35));
        let agent = view.runtime_agent().unwrap();
        assert_eq!(agent.kind, "codex");
        assert_eq!(agent.state_source, RuntimeAgentStateSource::Osc7501);
        assert!(!agent.hook_seen);
        for _ in 0..4 {
            wire.feed(view, cx, b"\x1b]7501;state=blocked:id=worker:kind=permission\x07");
            view.refresh_agent_screen_state(cx);
            assert_eq!(view.runtime_task_state(), RuntimeTaskState::Attention);
            assert_eq!(view.sidebar_activity(), SidebarActivity::Attention);
            assert_eq!(view.runtime_agent().unwrap().kind, "codex");
        }
        assert_eq!(view.progress, TaskProgress::Paused(None));
        view.process_event(TermEvent::Progress { state: 0, value: None }, cx);
        assert_eq!(view.progress, TaskProgress::Paused(None));
        wire.feed(view, cx, b"\x1b]7501;state=clear:id=worker\x07");
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
        wire.feed(view, cx, b"\x1b]7501;state=done:app=codex\x07\x1b]133;A\x07");
        view.finish_foreground_command(Some(0), cx);
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Finished);
        assert_eq!(view.progress, TaskProgress::None);
        view.process_event(TermEvent::Exit, cx);
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Finished);
        view.process_event(TermEvent::PtyFailure("fixture broken pipe".into()), cx);
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Failed);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Failed);
    });
    assert_eq!(notifications.get(), 0);
}

#[gpui::test]
fn osc7501_overrides_conflicting_hooks_in_either_arrival_order(cx: &mut TestAppContext) {
    for hook_first in [true, false] {
        let session = if hook_first { "osc7501-hook-first" } else { "osc7501-hook-last" };
        let (view, window, _) = open(cx);
        view.update(window, |view, cx| {
            let mut wire = Wire::install(view);
            for (index, (report, state, badge, progress)) in [
                ("idle", RuntimeTaskState::Idle, SidebarActivity::Idle, TaskProgress::None),
                (
                    "working:progress=35",
                    RuntimeTaskState::Running,
                    SidebarActivity::Running,
                    TaskProgress::Value(35),
                ),
                ("done", RuntimeTaskState::Finished, SidebarActivity::Done, TaskProgress::None),
                (
                    "blocked:kind=question",
                    RuntimeTaskState::Attention,
                    SidebarActivity::Attention,
                    TaskProgress::Paused(None),
                ),
                (
                    "blocked:kind=permission:progress=40",
                    RuntimeTaskState::Attention,
                    SidebarActivity::Attention,
                    TaskProgress::Paused(Some(40)),
                ),
                (
                    "blocked:kind=auth",
                    RuntimeTaskState::Attention,
                    SidebarActivity::Attention,
                    TaskProgress::Paused(None),
                ),
                (
                    "error",
                    RuntimeTaskState::Failed,
                    SidebarActivity::CommandFailed,
                    TaskProgress::Error(None),
                ),
            ]
            .into_iter()
            .enumerate()
            {
                wire.feed(view, cx, b"\x1b]7501;state=clear\x07");
                let name = if state == RuntimeTaskState::Running {
                    "PermissionRequest"
                } else {
                    "UserPromptSubmit"
                };
                let event = hook(session, name, index as u64 + 1);
                if hook_first {
                    assert!(view.handle_ai_hook(&event, cx));
                }
                let bytes = format!("\x1b]7501;state={report}:app=claude-code\x07");
                wire.feed(view, cx, bytes.as_bytes());
                if !hook_first {
                    assert!(view.handle_ai_hook(&event, cx));
                }
                view.refresh_agent_screen_state(cx);
                assert_eq!(view.runtime_task_state(), state, "{report}, hook_first={hook_first}");
                assert_eq!(view.sidebar_activity(), badge);
                assert_eq!(view.progress, progress);
                let agent = view.runtime_agent().unwrap();
                assert_eq!(agent.state_source, RuntimeAgentStateSource::Osc7501);
                assert_eq!(agent.session_id.as_deref(), Some(session));
                assert!(agent.hook_seen);
            }
        });
    }
}

fn hook(session: &str, name: &str, sequence: u64) -> crate::ai_hook::AiHookEvent {
    let payload = serde_json::json!({
        "hook_event_name": name,
        "session_id": session,
        "bridge_sequence": sequence,
    });
    let envelope = format!("nebula-hook/1 source=claude pane=42\n{payload}");
    crate::ai_hook::parse_remote_envelope(envelope.as_bytes(), Some(42)).unwrap()
}

#[gpui::test]
fn absent_or_cleared_osc7501_falls_back_to_hooks_then_screen(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        let mut wire = Wire::install(view);
        view.running_program = Some("claude".into());
        wire.feed(
            view,
            cx,
            "Do you want to proceed?\r\n❯ 1. Yes\r\n2. No\r\nEsc to cancel".as_bytes(),
        );
        view.refresh_agent_screen_state(cx);
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Attention);
        assert_eq!(view.runtime_agent().unwrap().state_source, RuntimeAgentStateSource::Screen);
        wire.feed(view, cx, b"\x1b]7501;state=done:app=claude-code\x07");
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Finished);
        wire.feed(view, cx, b"\x1b]7501;state=clear\x07");
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Attention);
        assert_eq!(view.runtime_agent().unwrap().state_source, RuntimeAgentStateSource::Screen);

        assert!(view.handle_ai_hook(&hook("osc7501-fallback", "UserPromptSubmit", 1), cx));
        view.refresh_agent_screen_state(cx);
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
        assert_eq!(view.runtime_agent().unwrap().state_source, RuntimeAgentStateSource::Hook);
        wire.feed(view, cx, b"\x1b]7501;state=error:app=claude-code\x07");
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Failed);
        wire.feed(view, cx, b"\x1b]7501;state=clear\x07\x1b]7501;state=invalid\x07");
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
        assert_eq!(view.runtime_agent().unwrap().state_source, RuntimeAgentStateSource::Hook);
        assert_eq!(view.progress, TaskProgress::None);
        wire.feed(view, cx, b"\x1b]7501;state=idle:app=claude-code\x07");
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Idle);
        wire.feed(view, cx, b"\x1bc");
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
        assert_eq!(view.runtime_agent().unwrap().state_source, RuntimeAgentStateSource::Hook);
    });
}

#[gpui::test]
fn osc7501_cannot_override_pane_failures(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        let mut wire = Wire::install(view);
        assert!(view.handle_ai_hook(&hook("osc7501-pane-failure", "UserPromptSubmit", 1), cx));
        wire.feed(view, cx, b"\x1b]7501;state=done:app=claude-code\x07");
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Finished);
        view.error = Some("fixture transport failure".into());
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Failed);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Failed);
    });
}

#[gpui::test]
fn osc7501_working_report_keeps_spinning_in_the_alternate_screen(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        let mut wire = Wire::install(view);
        view.mark_command_running();
        view.running_program = Some("btop".into());
        wire.feed(view, cx, b"\x1b[?1049h");
        assert_eq!(view.sidebar_activity(), SidebarActivity::Idle);
        // 全屏推断只是弱证据，不能盖掉程序的显式报告（#805）。
        wire.feed(view, cx, b"\x1b]7501;state=working:app=cargo\x07");
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
    });
}

#[gpui::test]
fn osc7501_error_and_ris_do_not_leave_stale_taskbar_state(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        let mut wire = Wire::install(view);
        wire.feed(view, cx, b"\x1b]7501;state=error:app=cargo\x07");
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Failed);
        assert_eq!(view.sidebar_activity(), SidebarActivity::CommandFailed);
        assert_eq!(view.progress, TaskProgress::Error(None));
        assert!(view.runtime_agent().is_none());
        wire.feed(view, cx, b"\x1bc");
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Idle);
        assert_eq!(view.progress, TaskProgress::None);
        view.process_event(TermEvent::Progress { state: 1, value: Some(50) }, cx);
        assert_eq!(view.progress, TaskProgress::Value(50));
        wire.feed(view, cx, b"\x1b]7501;state=working:app=claude-code:msg=ZG9uZQ==\x07");
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running, "msg is not evidence");
        assert_eq!(view.runtime_agent().unwrap().kind, "claude");
        wire.feed(view, cx, b"\x1b]7501;state=clear\x07");
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Idle);
        assert_eq!(view.progress, TaskProgress::None);
    });
}

#[gpui::test]
fn retained_results_are_acknowledged_by_input_or_the_next_shell_command(cx: &mut TestAppContext) {
    for result in ["done", "error"] {
        let (view, window, _) = open(cx);
        view.update(window, |view, cx| {
            let mut wire = Wire::install(view);
            let report = format!("\x1b]7501;state={result}:app=codex\x07");
            wire.feed(view, cx, report.as_bytes());
            wire.feed(view, cx, b"\x1b]133;A\x07");
            assert!(view.effective_program_status().is_some(), "the prompt retains results");
            view.suggest.last_committed = "sleep 10".into();
            wire.feed(view, cx, b"\x1b]133;C\x07");
            assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
            assert_eq!(view.progress, TaskProgress::None);
            assert!(view.runtime_agent().is_none(), "the old app cannot identify the new command");

            wire.feed(view, cx, report.as_bytes());
            assert!(view.effective_program_status().is_some(), "a fresh report rearms the result");
            view.write_input(vec![b'x'], cx);
            assert!(view.effective_program_status().is_none());
            assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
            assert_eq!(view.progress, TaskProgress::None);
            wire.feed(view, cx, b"\x1b]7501;state=blocked:app=codex\x07");
            view.write_input(vec![b'y'], cx);
            assert_eq!(view.runtime_task_state(), RuntimeTaskState::Attention);
            assert_eq!(
                view.runtime_agent().unwrap().state_source,
                RuntimeAgentStateSource::Osc7501
            );
        });
    }
}

#[gpui::test]
fn command_start_cannot_acknowledge_a_later_report_in_the_same_read(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        let mut wire = Wire::install(view);
        wire.feed(view, cx, b"\x1b]7501;state=done:app=codex\x07\x1b]133;A\x07");
        view.suggest.last_committed = "codex".into();
        wire.feed(view, cx, b"\x1b]133;C\x07\x1b]7501;state=error:app=codex\x07");
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Failed);
        assert_eq!(view.progress, TaskProgress::Error(None));
        assert_eq!(view.runtime_agent().unwrap().state_source, RuntimeAgentStateSource::Osc7501);
    });
}

#[gpui::test]
fn legacy_progress_survives_hooks_and_exit_without_protocol_ownership(cx: &mut TestAppContext) {
    for after_reset in [false, true] {
        let (view, window, _) = open(cx);
        let received = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let progress = received.clone();
        let _subscription = window.update(|_, cx| {
            cx.subscribe(&view, move |_, event, _| {
                if let TerminalViewEvent::ProgressChanged(value) = event {
                    progress.borrow_mut().push(*value);
                }
            })
        });
        view.update(window, |view, cx| {
            let mut wire = Wire::install(view);
            if after_reset {
                wire.feed(view, cx, b"\x1b]7501;state=working\x07");
                wire.feed(view, cx, b"\x1bc");
            }
            wire.feed(view, cx, b"\x1b]7501;?\x07\x1b]7501;state=invalid\x07");
            assert!(!view.program_status_seen);
            wire.feed(view, cx, b"\x1b]9;4;1;25\x07");
            let session = format!("legacy-progress-{after_reset}");
            assert!(view.handle_ai_hook(&hook(&session, "UserPromptSubmit", 1), cx));
            view.apply_ssh_stage(crate::ssh_session::SshStage::Failed("disconnected".into()), cx);
            view.process_event(TermEvent::Exit, cx);
            assert_eq!(view.progress, TaskProgress::Value(25));
        });
        let expected = if after_reset {
            vec![TaskProgress::Indeterminate, TaskProgress::None, TaskProgress::Value(25)]
        } else {
            vec![TaskProgress::Value(25)]
        };
        assert_eq!(*received.borrow(), expected);
    }
}

#[gpui::test]
fn protocol_progress_changes_once_at_reset_or_transport_failure(cx: &mut TestAppContext) {
    for report in ["working", "blocked", "error"] {
        for boundary in ["ris", "clear", "pty", "ssh"] {
            let (view, window, _) = open(cx);
            let received = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let progress = received.clone();
            let _subscription = window.update(|_, cx| {
                cx.subscribe(&view, move |_, event, _| {
                    if let TerminalViewEvent::ProgressChanged(value) = event {
                        progress.borrow_mut().push(*value);
                    }
                })
            });
            let expected = view.update(window, |view, cx| {
                let mut wire = Wire::install(view);
                let bytes = format!("\x1b]7501;state={report}:app=claude-code\x07");
                wire.feed(view, cx, bytes.as_bytes());
                let initial = view.progress;
                let session = format!("protocol-progress-{report}-{boundary}");
                assert!(view.handle_ai_hook(&hook(&session, "SessionEnd", 1), cx));
                assert_eq!(view.progress, initial);
                for _ in 0..2 {
                    match boundary {
                        "ris" => wire.feed(view, cx, b"\x1bc"),
                        "clear" => wire.feed(view, cx, b"\x1b]7501;state=clear\x07"),
                        "pty" => {
                            view.process_event(TermEvent::PtyFailure("broken pipe".into()), cx)
                        },
                        "ssh" => view.apply_ssh_stage(
                            crate::ssh_session::SshStage::Failed("disconnected".into()),
                            cx,
                        ),
                        _ => unreachable!(),
                    }
                    assert_eq!(view.progress, TaskProgress::None);
                    if matches!(boundary, "pty" | "ssh") {
                        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Failed);
                        assert_eq!(view.sidebar_activity(), SidebarActivity::Failed);
                    }
                }
                wire.feed(view, cx, b"\x1b]9;4;1;25\x07");
                let mut expected = vec![initial, TaskProgress::None];
                if boundary == "ris" {
                    expected.push(TaskProgress::Value(25));
                }
                assert_eq!(view.progress, *expected.last().unwrap());
                expected
            });
            assert_eq!(*received.borrow(), expected, "{report}, {boundary}");
        }
    }
}

fn shell_event(session: &str, name: &str, kind: &str) -> crate::ai_hook::AiHookEvent {
    let tasks = (0..4)
        .map(|index| {
            serde_json::json!({
                "id": format!("service{index}"), "type": kind, "status": "running"
            })
        })
        .collect::<Vec<_>>();
    let payload = serde_json::json!({
        "hook_event_name": name, "session_id": session, "prompt_id": "first",
        "background_tasks": tasks,
    });
    let envelope = format!("nebula-hook/1 source=claude pane=42\n{payload}");
    crate::ai_hook::parse_remote_envelope(envelope.as_bytes(), Some(42)).unwrap()
}

fn shell_reports(root: &str) -> Vec<u8> {
    let mut reports = format!("\x1b]7501;state={root}:app=claude-code\x07");
    for index in 0..4 {
        reports.push_str(&format!("\x1b]7501;state=working:app=claude-code:id=service{index}\x07"));
    }
    reports.into_bytes()
}

#[gpui::test]
fn shell_only_stop_quiets_claude_osc_background_records_without_finishing_task(
    cx: &mut TestAppContext,
) {
    for kind in ["shell", "local_bash"] {
        for root in ["done", "working"] {
            for stop_first in [true, false] {
                let session = format!("shell-stop-{kind}-{root}-{stop_first}");
                let (view, window, _) = open(cx);
                let notifications = std::rc::Rc::new(std::cell::Cell::new(0));
                let count = notifications.clone();
                let _subscription = window.update(|_, cx| {
                    cx.subscribe(&view, move |_, event, _| {
                        if matches!(event, TerminalViewEvent::Notification(_)) {
                            count.set(count.get() + 1);
                        }
                    })
                });
                view.update(window, |view, cx| {
                    let mut wire = Wire::install(view);
                    assert!(view.handle_ai_hook(&shell_event(&session, "UserPromptSubmit", kind), cx));
                    let stop = shell_event(&session, "Stop", kind);
                    assert_eq!(stop.active_background_tasks(), 4);
                    if stop_first {
                        assert!(view.handle_ai_hook(&stop, cx));
                    }
                    wire.feed(view, cx, &shell_reports(root));
                    if !stop_first {
                        assert!(view.handle_ai_hook(&stop, cx));
                    }
                    assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
                    assert_eq!(view.runtime_agent().unwrap().state_source, RuntimeAgentStateSource::Osc7501);
                    assert_eq!(
                        view.sidebar_activity(), SidebarActivity::Idle,
                        "shell-only Stop must quiet the badge: {kind}, root={root}, stop_first={stop_first}"
                    );
                    view.sync_activity_badges(cx);
                    assert!(view.completed_at.is_none());
                    assert_eq!(view.progress, TaskProgress::Indeterminate);
                });
                assert_eq!(notifications.get(), 0, "background work must not notify completion");
            }
        }
    }
}

#[gpui::test]
fn shell_rest_stops_when_osc_reports_unmatched_or_non_shell_work(cx: &mut TestAppContext) {
    for (report, state, badge) in [
        (
            "state=working:app=claude-code:id=untracked",
            RuntimeTaskState::Running,
            SidebarActivity::Running,
        ),
        ("state=working:app=cargo:id=other", RuntimeTaskState::Running, SidebarActivity::Running),
        (
            "state=blocked:kind=permission:app=claude-code:id=service0",
            RuntimeTaskState::Attention,
            SidebarActivity::Attention,
        ),
        (
            "state=error:app=claude-code:id=service0",
            RuntimeTaskState::Failed,
            SidebarActivity::CommandFailed,
        ),
    ] {
        let session = format!("shell-rest-veto-{report}");
        let (view, window, _) = open(cx);
        view.update(window, |view, cx| {
            let mut wire = Wire::install(view);
            assert!(view.handle_ai_hook(&shell_event(&session, "UserPromptSubmit", "shell"), cx));
            assert!(view.handle_ai_hook(&shell_event(&session, "Stop", "shell"), cx));
            wire.feed(view, cx, &shell_reports("working"));
            let bytes = format!("\x1b]7501;{report}\x07");
            wire.feed(view, cx, bytes.as_bytes());
            assert_eq!(view.runtime_task_state(), state, "{report}");
            assert_eq!(view.sidebar_activity(), badge, "{report}");
            assert!(
                view.agent_activity.background_shell_rest().is_none(),
                "{report} must revoke the qualification"
            );
            // The contradiction is new work, not a transient veto: clearing the
            // OSC records must not resurrect the old Stop's rest.
            wire.feed(view, cx, b"\x1b]7501;state=clear\x07");
            assert_eq!(view.sidebar_activity(), SidebarActivity::Running, "{report}");
        });
    }
}

#[gpui::test]
fn root_done_without_active_children_keeps_runtime_and_quiets_only_the_badge(
    cx: &mut TestAppContext,
) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        let mut wire = Wire::install(view);
        let session = "shell-rest-root-done-only";
        assert!(view.handle_ai_hook(&shell_event(session, "UserPromptSubmit", "shell"), cx));
        assert!(view.handle_ai_hook(&shell_event(session, "Stop", "shell"), cx));
        wire.feed(view, cx, b"\x1b]7501;state=done:app=claude-code\x07");
        // Explicit OSC Done stays authoritative for Runtime and waits (existing
        // priority); only the shared badge projection rests for the ended answer.
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Finished);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Idle);
    });
}

#[gpui::test]
fn root_working_transition_revokes_shell_rest_even_when_selection_stays_working(
    cx: &mut TestAppContext,
) {
    for previous_root in ["idle", "done"] {
        let session = format!("shell-root-transition-{previous_root}");
        let (view, window, _) = open(cx);
        view.update(window, |view, cx| {
            let mut wire = Wire::install(view);
            assert!(view.handle_ai_hook(&shell_event(&session, "UserPromptSubmit", "shell"), cx));
            assert!(view.handle_ai_hook(&shell_event(&session, "Stop", "shell"), cx));
            wire.feed(view, cx, &shell_reports(previous_root));
            assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
            assert_eq!(view.sidebar_activity(), SidebarActivity::Idle);
            if previous_root == "done" {
                wire.feed(view, cx, b"\x1b]7501;state=clear\x07");
                assert_eq!(view.sidebar_activity(), SidebarActivity::Idle);
            }
            wire.feed(view, cx, b"\x1b]7501;state=working:app=claude-code\x07");
            assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
            assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
        });
    }
}

#[gpui::test]
fn clearing_osc_records_falls_back_to_the_trusted_shell_stop(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        let mut wire = Wire::install(view);
        let session = "shell-rest-clear";
        assert!(view.handle_ai_hook(&shell_event(session, "UserPromptSubmit", "shell"), cx));
        assert!(view.handle_ai_hook(&shell_event(session, "Stop", "shell"), cx));
        wire.feed(view, cx, &shell_reports("working"));
        assert_eq!(view.sidebar_activity(), SidebarActivity::Idle);
        wire.feed(view, cx, b"\x1b]7501;state=clear\x07");
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Idle);
    });
}

#[gpui::test]
fn a_new_prompt_revokes_shell_rest_and_restores_the_running_badge(cx: &mut TestAppContext) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        let session = "shell-rest-new-prompt";
        assert!(view.handle_ai_hook(&shell_event(session, "UserPromptSubmit", "shell"), cx));
        assert!(view.handle_ai_hook(&shell_event(session, "Stop", "shell"), cx));
        assert_eq!(view.sidebar_activity(), SidebarActivity::Idle);
        assert!(view.handle_ai_hook(&hook(session, "UserPromptSubmit", 9), cx));
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
    });
}

#[gpui::test]
fn incomplete_shell_id_mapping_blocks_osc_quieting_but_not_hook_only_rest(cx: &mut TestAppContext) {
    for with_osc in [false, true] {
        let session = format!("shell-rest-missing-id-{with_osc}");
        let (view, window, _) = open(cx);
        view.update(window, |view, cx| {
            let mut wire = Wire::install(view);
            assert!(view.handle_ai_hook(&shell_event(&session, "UserPromptSubmit", "shell"), cx));
            let mut stop = shell_event(&session, "Stop", "shell");
            stop.background_tasks.as_mut().unwrap().shell_ids = None;
            assert!(view.handle_ai_hook(&stop, cx));
            if with_osc {
                wire.feed(view, cx, &shell_reports("working"));
            }
            assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
            assert_eq!(
                view.sidebar_activity(),
                if with_osc { SidebarActivity::Running } else { SidebarActivity::Idle }
            );
        });
    }
}

#[gpui::test]
fn osc_shell_ids_qualify_without_an_explicit_app_and_children_inherit_it(cx: &mut TestAppContext) {
    // 真实 OSC 7501 可能省略 app：根和子记录都不带时，只能凭可精确对应的
    // Shell ID 认定这是同一个 Claude 回合的后台 Shell；无法对应则继续转圈。
    for (label, reports) in [
        (
            "no-app-done",
            b"\x1b]7501;state=done\x07\x1b]7501;state=working:id=service0\x07\
              \x1b]7501;state=working:id=service1\x07"
                .to_vec(),
        ),
        (
            "inherited-app",
            b"\x1b]7501;state=done:app=claude-code\x07\x1b]7501;state=working:id=service0\x07\
              \x1b]7501;state=working:id=service1\x07"
                .to_vec(),
        ),
    ] {
        let session = format!("shell-rest-{label}");
        let (view, window, _) = open(cx);
        view.update(window, |view, cx| {
            let mut wire = Wire::install(view);
            assert!(view.handle_ai_hook(&shell_event(&session, "UserPromptSubmit", "shell"), cx));
            assert!(view.handle_ai_hook(&shell_event(&session, "Stop", "shell"), cx));
            wire.feed(view, cx, &reports);
            assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running, "{label}");
            assert_eq!(view.sidebar_activity(), SidebarActivity::Idle, "{label}");
        });
    }

    let session = "shell-rest-no-app-untracked";
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        let mut wire = Wire::install(view);
        assert!(view.handle_ai_hook(&shell_event(session, "UserPromptSubmit", "shell"), cx));
        assert!(view.handle_ai_hook(&shell_event(session, "Stop", "shell"), cx));
        wire.feed(view, cx, b"\x1b]7501;state=done\x07\x1b]7501;state=working:id=untracked\x07");
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
    });
}

#[gpui::test]
fn failed_or_cancelled_shell_stop_never_establishes_badge_rest(cx: &mut TestAppContext) {
    for outcome in [
        crate::ai_hook::AiTurnOutcome::Failed,
        crate::ai_hook::AiTurnOutcome::Cancelled,
        crate::ai_hook::AiTurnOutcome::Incomplete,
    ] {
        let session = format!("shell-rest-outcome-{outcome:?}");
        let (view, window, _) = open(cx);
        view.update(window, |view, cx| {
            assert!(view.handle_ai_hook(&shell_event(&session, "UserPromptSubmit", "shell"), cx));
            let mut stop = shell_event(&session, "Stop", "shell");
            stop.turn_outcome = outcome;
            assert!(view.handle_ai_hook(&stop, cx));
            assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
            assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
        });
    }
}

#[gpui::test]
fn overall_running_state_survives_foreground_cancel_while_background_work_is_active(
    cx: &mut TestAppContext,
) {
    let (view, window, _) = open(cx);
    view.update(window, |view, cx| {
        let session = "shell-rest-cancelled-answer";
        assert!(view.handle_ai_hook(&shell_event(session, "UserPromptSubmit", "shell"), cx));
        assert!(view.handle_ai_hook(&shell_event(session, "Stop", "shell"), cx));
        assert!(view.handle_ai_hook(&hook(session, "PostToolUse", 10), cx));
        view.agent_activity.request_cancel();
        let idle = || crate::ai_agents::Detection {
            agent: crate::ai_agents::AgentKind::Claude,
            status: crate::ai_agents::AgentStatus::Idle,
            rule_id: "test.prompt_idle".into(),
        };
        assert!(!view.agent_activity.observe_cancel(Some(idle())));
        assert!(view.agent_activity.observe_cancel(Some(idle())));
        assert_eq!(view.agent_activity.status(), crate::ai_agents::AgentStatus::Idle);
        assert_eq!(view.runtime_task_state(), RuntimeTaskState::Running);
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
    });
}
