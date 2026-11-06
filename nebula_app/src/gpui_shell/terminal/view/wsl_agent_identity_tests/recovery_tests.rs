use super::super::startup_tests::feed;
use super::*;
use crate::session::AgentSession;

const REQUESTED: &str = "01a1aedc-d6f0-7db0-8875-882c249ef4a2";
const CURRENT: &str = "0199a213-c2a4-7cf5-8f6b-d746fbb6e86c";

#[gpui::test]
fn chat_input_cannot_replace_the_verified_launch_candidate(cx: &mut TestAppContext) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let ticket = start_host_wsl(view, 'a', cx);
        let location = recovery_location(AgentKind::Codex, REQUESTED, "/current");
        view.apply_wsl_cwd_probe_result(&ticket, WslLocationProbe::Found(location), true, cx);
        view.suggest.last_committed = "please fix a file in another directory".into();
        let saved = view.session_agent().unwrap();
        assert_eq!(saved.session_id.as_deref(), Some(REQUESTED));
        assert_eq!(saved.wsl_launch.as_ref().unwrap().cwd, "/current");
        assert!(saved.resume_command().unwrap().contains(REQUESTED));
        assert!(view.ai_session.is_none(), "a requested ID is not a native acknowledgement");
    });
}

#[gpui::test]
fn native_guest_identity_survives_refresh_and_keeps_host_cwd_separate(cx: &mut TestAppContext) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let ticket = start_host_wsl(view, 'a', cx);
        let location = recovery_location(AgentKind::Codex, REQUESTED, "/current");
        view.apply_wsl_cwd_probe_result(
            &ticket,
            WslLocationProbe::Found(location.clone()),
            true,
            cx,
        );
        let host_cwd = view.cwd.clone();
        view.apply_codex_session_identity(
            crate::platform::ai_session_identity::CodexSession {
                session_id: CURRENT.into(),
                session_file: format!("/sessions/rollout-date-{CURRENT}.jsonl"),
                cwd: Some("/old-metadata-directory".into()),
            },
            true,
            cx,
        );
        assert_eq!(view.cwd, host_cwd);
        let mut refreshed = location;
        refreshed.cwd = "/last-directory".into();
        refreshed.agent_launch.as_mut().unwrap().cwd = refreshed.cwd.clone();
        view.wsl_cwd_observer.set_observed_for_test(Some(refreshed));
        view.sync_wsl_agent_identity_with_context(cx);
        assert_eq!(view.ai_session.as_ref().unwrap().session_id, CURRENT);
        let saved = view.session_agent().unwrap();
        assert_eq!(saved.session_id.as_deref(), Some(CURRENT));
        assert_eq!(saved.wsl_launch.as_ref().unwrap().cwd, "/last-directory");
        assert_eq!(view.cwd, host_cwd);
        assert!(!saved.resume_command().unwrap().contains(REQUESTED));
    });
}

#[gpui::test]
fn replaced_guest_receipts_cannot_keep_a_retired_live_target(cx: &mut TestAppContext) {
    for kind in [AgentKind::Claude, AgentKind::Codex] {
        let (entity, window, _) = open(cx);
        entity.update(window, |view, cx| {
            let ticket = start_host_wsl(view, 'a', cx);
            view.apply_wsl_cwd_probe_result(
                &ticket,
                WslLocationProbe::Found(recovery_location(AgentKind::Codex, REQUESTED, "/current")),
                true,
                cx,
            );
            view.apply_codex_session_identity(
                crate::platform::ai_session_identity::CodexSession {
                    session_id: CURRENT.into(),
                    session_file: format!("/sessions/rollout-date-{CURRENT}.jsonl"),
                    cwd: None,
                },
                true,
                cx,
            );
            let mut replacement = recovery_location(kind, REQUESTED, "/next");
            replacement.agent.process.start_time += 1;
            view.wsl_cwd_observer.set_observed_for_test(Some(replacement));
            view.sync_wsl_agent_identity_with_context(cx);
            assert!(view.recovery.live.is_none());
            assert!(view.ai_session.is_none());
            let saved = view.session_agent().unwrap();
            assert_eq!(saved.source, kind.slug());
            assert!(saved.session_id.is_none());
        });
    }
}

#[gpui::test]
fn guest_session_probe_rejects_replaced_scope_but_accepts_a_cwd_refresh(cx: &mut TestAppContext) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let ticket = start_host_wsl(view, 'a', cx);
        let location = recovery_location(AgentKind::Codex, REQUESTED, "/current");
        view.apply_wsl_cwd_probe_result(
            &ticket,
            WslLocationProbe::Found(location.clone()),
            true,
            cx,
        );
        let (scope, expected) = view.wsl_session_probe_binding().unwrap();
        let mut refreshed = location;
        refreshed.cwd = "/new-directory".into();
        view.wsl_cwd_observer.set_observed_for_test(Some(refreshed.clone()));
        assert!(view.wsl_session_probe_owner_is_current(&scope, &expected));
        refreshed.agent.process.start_time += 1;
        view.wsl_cwd_observer.set_observed_for_test(Some(refreshed));
        assert!(!view.wsl_session_probe_owner_is_current(&scope, &expected));
        view.wsl_cwd_observer.accept_command_id(&"b".repeat(32));
        assert!(!view.wsl_session_probe_owner_is_current(&scope, &expected));
    });
}

#[gpui::test]
fn an_accepted_hook_updates_the_target_and_preserves_guest_directory_refresh(
    cx: &mut TestAppContext,
) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let ticket = start_host_wsl(view, 'a', cx);
        let location = recovery_location(AgentKind::Claude, REQUESTED, "/current");
        view.apply_wsl_cwd_probe_result(
            &ticket,
            WslLocationProbe::Found(location.clone()),
            true,
            cx,
        );
        let wire = format!(
            "nebula-hook/1 source=claude process=30:300\n\
            {{\"hook_event_name\":\"SessionStart\",\"session_id\":\"{CURRENT}\"}}"
        );
        let event =
            crate::ai_hook::parse_remote_envelope(wire.as_bytes(), Some(view.pane_id)).unwrap();
        assert!(view.handle_ai_hook(&event, cx));
        let mut refreshed = location;
        refreshed.cwd = "/last-directory".into();
        refreshed.agent_launch.as_mut().unwrap().cwd = refreshed.cwd.clone();
        view.wsl_cwd_observer.set_observed_for_test(Some(refreshed));
        let saved = view.session_agent().unwrap();
        assert_eq!(saved.session_id.as_deref(), Some(CURRENT));
        assert_eq!(saved.wsl_launch.as_ref().unwrap().cwd, "/last-directory");
        assert!(!saved.resume_command().unwrap().contains(REQUESTED));
        let mut end = event;
        end.kind = crate::ai_hook::AiHookKind::SessionEnd;
        end.received_sequence += 1;
        assert!(view.handle_ai_hook(&end, cx));
        assert!(view.saved_wsl_agent().is_none());
    });
}

#[gpui::test]
fn cold_resume_submits_the_long_command_and_keeps_the_same_target_for_retry(
    cx: &mut TestAppContext,
) {
    let (entity, window, receiver) = open(cx);
    entity.update(window, |view, cx| {
        let mut options = nebula_terminal::tty::Options::default();
        options.shell = Some(nebula_terminal::tty::Shell::new("pwsh.exe".into(), vec![]));
        view.exec_context = Some(crate::runtime_exec::PaneExecContext::from_pty_options(&options));
        let saved = AgentSession {
            source: "codex".into(),
            session_id: Some(REQUESTED.into()),
            session_file: None,
            wsl_launch: recovery_location(AgentKind::Codex, REQUESTED, "/current").agent_launch,
        };
        let expected = saved.resume_command().unwrap();
        view.restore_agent(saved.clone(), cx);
        feed(view, b"\x1b]133;A\x07PS C:\\fixture> ");
        view.flush_pending_shell_command(cx);
        assert!(receiver.try_iter().any(|message| {
            matches!(message, Msg::Input(bytes) if bytes.as_ref() == expected.as_bytes())
        }));
        feed(view, expected.as_bytes());
        view.flush_pending_runtime_submit(cx);
        view.process_event(Event::CommandDone { exit_code: Some(1) }, cx);
        assert!(view.can_retry_recovery());
        assert_eq!(view.session_agent(), Some(saved.clone()));
        view.retry_recovery(cx);
        assert_eq!(view.session_agent(), Some(saved));
    });
}

#[gpui::test]
fn failed_wsl_codex_chooser_runs_in_the_guest_and_retains_it_after_selection(
    cx: &mut TestAppContext,
) {
    let (entity, window, receiver) = open(cx);
    entity.update(window, |view, cx| {
        let mut options = nebula_terminal::tty::Options::default();
        options.shell = Some(nebula_terminal::tty::Shell::new("pwsh.exe".into(), vec![]));
        view.exec_context = Some(crate::runtime_exec::PaneExecContext::from_pty_options(&options));
        let saved = AgentSession {
            source: "codex".into(),
            session_id: Some(REQUESTED.into()),
            session_file: None,
            wsl_launch: recovery_location(AgentKind::Codex, REQUESTED, "/current").agent_launch,
        };
        view.restore_agent(saved.clone(), cx);
        feed(view, b"\x1b]133;A\x07PS C:\\fixture> ");
        view.flush_pending_shell_command(cx);
        feed(view, saved.resume_command().unwrap().as_bytes());
        view.flush_pending_runtime_submit(cx);
        view.process_event(Event::CommandDone { exit_code: Some(1) }, cx);
        receiver.try_iter().for_each(drop);
        assert!(view.can_choose_recovery_session());
        let selector = saved.codex_selector_command(view.path_quote()).unwrap();
        view.choose_recovery_session(cx);
        feed(view, b"\x1b]133;A\x07PS C:\\fixture> ");
        view.flush_pending_shell_command(cx);
        assert!(receiver.try_iter().any(|message| {
            matches!(message, Msg::Input(bytes) if bytes.as_ref() == selector.as_bytes())
        }));
        assert!(view.recovery_pending());
        view.apply_codex_session_identity(
            crate::platform::ai_session_identity::CodexSession {
                session_id: CURRENT.into(),
                session_file: format!("/sessions/rollout-date-{CURRENT}.jsonl"),
                cwd: None,
            },
            true,
            cx,
        );
        let selected = view.session_agent().unwrap();
        assert_eq!(selected.session_id.as_deref(), Some(CURRENT));
        assert_eq!(selected.wsl_launch, saved.wsl_launch);
        assert!(!view.recovery_pending());
    });
}

#[gpui::test]
fn missing_claude_identity_is_saved_as_failed_without_sending_continue(cx: &mut TestAppContext) {
    let (entity, window, receiver) = open(cx);
    entity.update(window, |view, cx| {
        let saved = AgentSession {
            source: "claude".into(),
            session_id: None,
            session_file: None,
            wsl_launch: recovery_location(AgentKind::Claude, REQUESTED, "/current").agent_launch,
        };
        view.restore_agent(saved.clone(), cx);
        feed(view, b"\x1b]133;A\x07user@host:~$ ");
        view.flush_pending_shell_command(cx);
        assert!(receiver.try_iter().all(|message| !matches!(message, Msg::Input(_))));
        assert_eq!(view.session_agent(), Some(saved));
        assert!(view.can_retry_recovery());
        assert!(view.ai_session.is_none());
    });
}

#[gpui::test]
fn a_new_guest_after_failed_recovery_keeps_its_own_identity_and_the_old_retry_target(
    cx: &mut TestAppContext,
) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let old = AgentSession {
            source: "codex".into(),
            session_id: Some("missing-old".into()),
            session_file: None,
            wsl_launch: recovery_location(AgentKind::Codex, "missing-old", "/old").agent_launch,
        };
        view.recovery.target = Some(old.clone());
        view.recovery.awaiting_confirmation = true;
        view.recovery.mark_failed();
        let ticket = start_host_wsl(view, 'a', cx);
        view.apply_wsl_cwd_probe_result(
            &ticket,
            WslLocationProbe::Found(recovery_location(AgentKind::Codex, REQUESTED, "/current")),
            true,
            cx,
        );
        assert_eq!(view.session_agent().unwrap().session_id.as_deref(), Some(REQUESTED));
        assert!(view.recovery_pending(), "the launch candidate cannot confirm recovery");
        view.apply_codex_session_identity(
            crate::platform::ai_session_identity::CodexSession {
                session_id: CURRENT.into(),
                session_file: format!("/sessions/rollout-date-{CURRENT}.jsonl"),
                cwd: None,
            },
            true,
            cx,
        );
        let current = view.session_agent().unwrap();
        assert_eq!(current.session_id.as_deref(), Some(CURRENT));
        assert_eq!(current.wsl_launch.as_ref().unwrap().cwd, "/current");
        assert_eq!(view.recovery.retry_candidate.as_ref(), Some(&old));
        assert!(!view.recovery_pending());
    });
}

#[gpui::test]
fn attaching_a_guest_environment_does_not_promote_a_requested_id_to_native_identity(
    cx: &mut TestAppContext,
) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let ticket = start_host_wsl(view, 'a', cx);
        view.apply_wsl_cwd_probe_result(
            &ticket,
            WslLocationProbe::Found(recovery_location(AgentKind::Claude, REQUESTED, "/current")),
            true,
            cx,
        );
        let attached = view.with_wsl_launch(AgentSession {
            source: "claude".into(),
            session_id: None,
            session_file: None,
            wsl_launch: None,
        });
        assert!(attached.session_id.is_none());
        assert!(attached.session_file.is_none());
        assert!(attached.wsl_launch.is_some());
        assert_process_identity(view);
        assert_eq!(view.session_agent().unwrap().session_id.as_deref(), Some(REQUESTED));
    });
}
