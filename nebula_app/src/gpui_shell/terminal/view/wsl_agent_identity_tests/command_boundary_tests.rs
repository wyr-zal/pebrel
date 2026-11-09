use super::*;

fn report_command(
    view: &mut TerminalView,
    id: char,
    command: &str,
    cx: &mut Context<TerminalView>,
) {
    for (name, value) in [
        ("pebrel_command_id", id.to_string().repeat(32)),
        ("pebrel_command", format!("pwsh:fixture\n{command}")),
    ] {
        view.process_event(Event::UserVar { name: name.into(), value }, cx);
    }
    view.process_event(Event::CommandStart, cx);
}

#[gpui::test]
fn completed_shell_boundary_preserves_the_next_wsl_identity_and_working_state(
    cx: &mut TestAppContext,
) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        view.process_event(Event::CommandDone { exit_code: Some(0) }, cx);
        assert!(view.agent_activity.command_ended());
        let ticket = start_host_wsl(view, 'b', cx);
        view.suggest.completion_shell_report(crate::completion_context::SHELL_VAR, "pwsh:fixture");
        view.process_event(
            Event::UserVar {
                name: crate::completion_context::ARGV_VAR.into(),
                value: "pwsh:fixture\nwsl.exe\0-d\0Ubuntu-22.04\0-e\0claude".into(),
            },
            cx,
        );
        assert!(view.wsl_cwd_observer.expects_agent());
        assert_eq!(view.sidebar_activity(), SidebarActivity::Unconfirmed);
        view.apply_wsl_cwd_probe_result(&ticket, WslLocationProbe::Found(location()), true, cx);
        assert_process_identity(view);
        super::super::startup_tests::feed(
            view,
            "\x1b[2J\x1b[H* Swirling… (4m 29s · ↓ 16.7k tokens)\r\n\r\n────────\r\n❯ \r\n────────\r\n? for shortcuts".as_bytes(),
        );
        for _ in 0..4 {
            view.refresh_agent_screen_state(cx);
        }
        assert_eq!(view.presentation_program(), Some("claude"));
        assert_eq!(view.sidebar_activity(), SidebarActivity::Running);
    });
}

#[gpui::test]
fn pending_shell_boundary_preserves_new_wsl_submission_and_rejects_the_old_probe(
    cx: &mut TestAppContext,
) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let old_ticket = start_host_wsl(view, 'a', cx);
        let hook = crate::ai_hook::parse_remote_envelope(
            b"nebula-hook/1 source=codex process=1316:7632 codex_hooks=full pane=42\n{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"old-codex\",\"turn_id\":\"old-turn\",\"bridge_sequence\":1}",
            Some(42),
        ).unwrap();
        assert!(view.handle_ai_hook(&hook, cx));
        view.shell_boundary_probe = Some(super::super::agent_activity::PendingShellBoundary {
            owner: view.agent_activity.shell_boundary_owner().unwrap(),
            exit_code: Some(0),
            attempts: 0,
        });
        let ticket = start_host_wsl(view, 'b', cx);
        assert!(!view.agent_activity.hook_seen());
        assert!(view.shell_boundary_probe.is_none());
        view.apply_wsl_cwd_probe_result(&old_ticket, WslLocationProbe::Found(location()), true, cx);
        assert!(view.runtime_agent().is_none());
        view.apply_wsl_cwd_probe_result(&ticket, WslLocationProbe::Found(location()), true, cx);
        assert_process_identity(view);
        assert_eq!(view.wsl_session_probe_binding().unwrap().0.command_id, "b".repeat(32));
    });
}

#[gpui::test]
fn pasted_wsl_submission_survives_previous_command_cleanup(cx: &mut TestAppContext) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let _ = start_host_wsl(view, 'a', cx);
        view.process_event(Event::CommandDone { exit_code: Some(0) }, cx);
        view.wsl_cwd_observer.prepare_submission(Some(NativeLaunchBoundary {
            time: 101,
            shell: ProcessStamp { pid: 10, start_time: 90 },
            existing_branches: Default::default(),
        }));
        let command = "wsl.exe -d Ubuntu-22.04 -e claude";
        view.accept_pasted_wsl_command(command);
        report_command(view, 'c', command, cx);
        assert!(view.wsl_cwd_observer.expects_agent());
        let ticket = view.wsl_cwd_observer.begin_probe(Instant::now()).unwrap();
        view.apply_wsl_cwd_probe_result(&ticket, WslLocationProbe::Found(location()), true, cx);
        assert_process_identity(view);
        let (scope, _) = view.wsl_session_probe_binding().unwrap();
        assert_eq!(scope.command_id, "c".repeat(32));
        assert_eq!(scope.boundary.time, 101);
        view.process_event(Event::CommandDone { exit_code: Some(0) }, cx);
        assert!(view.runtime_agent().is_none());
        assert!(view.wsl_cwd_observer.begin_probe(Instant::now()).is_none());
        assert_eq!(view.sidebar_activity(), SidebarActivity::Idle);
    });
}

#[gpui::test]
fn command_start_without_a_new_submission_cannot_revive_the_previous_wsl_command(
    cx: &mut TestAppContext,
) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let ticket = start_host_wsl(view, 'a', cx);
        view.agent_activity.command_finished();
        view.process_event(Event::CommandStart, cx);
        assert!(view.wsl_cwd_observer.begin_probe(Instant::now()).is_none());
        assert!(!view.wsl_cwd_observer.expects_agent());
        view.apply_wsl_cwd_probe_result(&ticket, WslLocationProbe::Found(location()), true, cx);
        assert!(view.runtime_agent().is_none());
    });
}

#[gpui::test]
fn ordinary_command_after_wsl_exit_cannot_inherit_agent_identity(cx: &mut TestAppContext) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let ticket = start_host_wsl(view, 'a', cx);
        view.apply_wsl_cwd_probe_result(&ticket, WslLocationProbe::Found(location()), true, cx);
        let boundary = view.wsl_session_probe_binding().unwrap().0.boundary;
        view.process_event(Event::CommandDone { exit_code: Some(0) }, cx);
        view.wsl_cwd_observer.prepare_submission(Some(boundary));
        view.suggest.last_committed = "Get-ChildItem".into();
        report_command(view, 'd', "Get-ChildItem", cx);
        assert!(!view.wsl_cwd_observer.expects_agent());
        assert!(view.wsl_cwd_observer.begin_probe(Instant::now()).is_none());
        assert!(view.runtime_agent().is_none());
        assert_ne!(view.presentation_program(), Some("claude"));
    });
}
