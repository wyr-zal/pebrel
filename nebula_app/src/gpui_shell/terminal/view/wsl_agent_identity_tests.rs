use super::startup_tests::open;
use super::*;
use crate::ai_agents::{AgentKind, AgentStatus, AgentStatusSource, AgentStatusUncertainty};
use crate::platform::agent_process_identity::{
    ForegroundAgent, NativeLaunchBoundary, ProcessStamp,
};
use crate::platform::wsl_location::{ObservedWslLocation, WslLocationProbe};
use gpui::TestAppContext;
use nebula_terminal::event::Event;
use std::time::Instant;

fn location() -> ObservedWslLocation {
    ObservedWslLocation {
        distro: "Ubuntu-22.04".into(),
        user: None,
        cwd: "/mnt/e/ProjectOrder/VoiceRecite".into(),
        launcher: ProcessStamp { pid: 20, start_time: 200 },
        agent: ForegroundAgent {
            kind: AgentKind::Claude,
            process: ProcessStamp { pid: 30, start_time: 300 },
            shell: ProcessStamp { pid: 40, start_time: 400 },
            tty_nr: 4,
            process_group: 30,
        },
    }
}

fn start_host_wsl(
    view: &mut TerminalView,
    command_digit: char,
    cx: &mut Context<TerminalView>,
) -> super::wsl_cwd_observer::ProbeTicket {
    view.suggest.suggest_env = crate::display::SuggestEnv::Local;
    view.session.as_mut().unwrap().shell_pid = 10;
    view.cwd = r"C:\Users\12704".into();
    view.suggest.cwd = view.cwd.clone();
    let mut options = nebula_terminal::tty::Options::default();
    options.shell = Some(nebula_terminal::tty::Shell::new("pwsh.exe".into(), vec![]));
    view.exec_context = Some(crate::runtime_exec::PaneExecContext::from_pty_options(&options));
    view.suggest.last_committed =
        "wsl.exe -d 'Ubuntu-22.04' --cd '/mnt/e/ProjectOrder/VoiceRecite' -e sh -lc \
         'exec ${SHELL:-sh} -lic ''claude --resume fixture-session'''"
            .into();
    view.wsl_cwd_observer.prepare_submission(Some(NativeLaunchBoundary {
        time: 100,
        shell: ProcessStamp { pid: 10, start_time: 90 },
        existing_branches: Default::default(),
    }));
    let command_id = command_digit.to_string().repeat(32);
    view.wsl_cwd_observer.accept_command_id(&command_id);
    view.wsl_cwd_observer.accept_command_line(&view.suggest.last_committed);
    view.process_event(Event::CommandStart, cx);
    assert_eq!(view.running_program.as_deref(), Some("wsl"));
    view.wsl_cwd_observer.begin_probe(Instant::now()).expect("current WSL command")
}

fn assert_process_identity(view: &TerminalView) {
    let agent = view.runtime_agent().expect("publish the verified guest Agent");
    assert_eq!(agent.kind, "claude");
    assert_eq!(agent.state_source, crate::runtime_api::RuntimeAgentStateSource::Process);
    assert!(!agent.hook_seen);
    assert!(agent.session_id.is_none(), "the requested resume ID is not a confirmed session");
    assert!(view.ai_session.is_none());
    assert_eq!(view.agent_activity.source(), AgentStatusSource::Process);
    assert_eq!(view.agent_activity.status(), AgentStatus::Unknown);
    assert_eq!(view.agent_activity.uncertainty(), Some(AgentStatusUncertainty::IdentityOnly));
    assert!(!view.agent_activity.hook_seen());
}

impl TerminalView {
    pub(crate) fn install_wsl_process_identity_test_fixture(&mut self, cx: &mut Context<Self>) {
        self.install_completion_test_session();
        let ticket = start_host_wsl(self, 'a', cx);
        self.apply_wsl_cwd_probe_result(&ticket, WslLocationProbe::Found(location()), true, cx);
        assert_process_identity(self);
    }
}

#[gpui::test]
fn wsl_process_identity_reaches_runtime_without_claiming_hook_or_session(cx: &mut TestAppContext) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let ticket = start_host_wsl(view, 'a', cx);
        view.apply_wsl_cwd_probe_result(&ticket, WslLocationProbe::Found(location()), true, cx);
        assert_process_identity(view);
        assert_eq!(view.tab_label(), "VoiceRecite");
        assert_eq!(
            view.cwd, r"C:\Users\12704",
            "guest location must not replace Host execution cwd"
        );
        assert_eq!(view.running_program.as_deref(), Some("claude"));
    });
}

#[gpui::test]
fn wsl_process_identity_is_published_when_the_directory_is_unchanged(cx: &mut TestAppContext) {
    let (entity, window, _) = open(cx);
    let ticket = entity.update(window, |view, cx| {
        let ticket = start_host_wsl(view, 'a', cx);
        view.wsl_cwd_observer.set_observed_for_test(Some(location()));
        assert_eq!(view.tab_label(), "VoiceRecite");
        assert!(view.runtime_agent().is_none());
        ticket
    });
    let changes = std::rc::Rc::new(std::cell::Cell::new(0));
    let _subscription = window.update(|_, cx| {
        let changes = changes.clone();
        cx.subscribe(&entity, move |_, event, _| {
            if matches!(event, TerminalViewEvent::TitleChanged) {
                changes.set(changes.get() + 1);
            }
        })
    });
    entity.update(window, |view, cx| {
        view.apply_wsl_cwd_probe_result(&ticket, WslLocationProbe::Found(location()), true, cx);
        assert_process_identity(view);
    });
    assert!(changes.get() > 0, "identity-only changes must refresh the presentation");
}

#[gpui::test]
fn wsl_process_identity_survives_shell_titles_while_title_text_updates(cx: &mut TestAppContext) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let ticket = start_host_wsl(view, 'a', cx);
        view.apply_wsl_cwd_probe_result(&ticket, WslLocationProbe::Found(location()), true, cx);
        view.process_event(Event::Title(r"NEBULA|C:\Users\12704|main|wsl".into()), cx);
        assert_process_identity(view);
        view.process_event(Event::Title("Claude task title".into()), cx);
        assert_eq!(view.title, "Claude task title");
        assert_process_identity(view);
    });
}

#[gpui::test]
fn wsl_process_identity_rejects_a_ticket_from_the_previous_command(cx: &mut TestAppContext) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let retired = start_host_wsl(view, 'a', cx);
        view.wsl_cwd_observer.accept_command_id(&"b".repeat(32));
        view.process_event(Event::CommandStart, cx);
        view.apply_wsl_cwd_probe_result(&retired, WslLocationProbe::Found(location()), true, cx);
        assert_eq!(view.running_program.as_deref(), Some("wsl"));
        assert!(view.runtime_agent().is_none());
        let current = view.wsl_cwd_observer.begin_probe(Instant::now()).unwrap();
        view.apply_wsl_cwd_probe_result(&current, WslLocationProbe::Found(location()), true, cx);
        assert_process_identity(view);
    });
}

#[gpui::test]
fn wsl_process_identity_rejects_a_receipt_from_a_different_pty(cx: &mut TestAppContext) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let ticket = start_host_wsl(view, 'a', cx);
        view.apply_wsl_cwd_probe_result(&ticket, WslLocationProbe::Found(location()), false, cx);
        assert!(view.runtime_agent().is_none());
        assert!(view.agent_identity_receipt.is_none());
        assert!(!view.agent_activity.hook_seen());
        assert_ne!(view.agent_activity.status(), AgentStatus::Done);
    });
}

#[gpui::test]
fn wsl_process_identity_replaces_a_stale_session_from_another_provider(cx: &mut TestAppContext) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let ticket = start_host_wsl(view, 'a', cx);
        view.ai_session = Some(crate::display::AiSessionIdentity {
            source: "codex".into(),
            session_id: "retired-probe-session".into(),
        });
        view.ai_session_from_probe = true;
        view.apply_wsl_cwd_probe_result(&ticket, WslLocationProbe::Found(location()), true, cx);
        assert_process_identity(view);
    });
}

#[gpui::test]
fn wsl_process_identity_cannot_overwrite_an_accepted_hook(cx: &mut TestAppContext) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let ticket = start_host_wsl(view, 'a', cx);
        view.apply_wsl_cwd_probe_result(&ticket, WslLocationProbe::Found(location()), true, cx);
        let late = view
            .wsl_cwd_observer
            .begin_probe(Instant::now() + std::time::Duration::from_secs(4))
            .unwrap();
        let event = crate::ai_hook::parse_remote_envelope(
            b"nebula-hook/1 source=claude process=30:300\n{\"hook_event_name\":\"SessionStart\",\"session_id\":\"accepted-wsl-fixture\"}",
            Some(view.pane_id),
        )
        .unwrap();
        assert!(view.handle_ai_hook(&event, cx));
        let mut different_agent = location();
        different_agent.agent.kind = AgentKind::Codex;
        view.apply_wsl_cwd_probe_result(&late, WslLocationProbe::Found(different_agent), true, cx);
        view.wsl_cwd_observer.finish_command();
        view.sync_wsl_agent_identity();
        let current = view.runtime_agent().unwrap();
        assert_eq!(current.kind, "claude");
        assert_eq!(current.session_id.as_deref(), Some("accepted-wsl-fixture"));
        assert!(current.hook_seen);
        assert_eq!(view.running_program.as_deref(), Some("claude"));
    });
}

#[gpui::test]
fn wsl_process_identity_cleanup_restores_the_wrapper_without_finishing_the_task(
    cx: &mut TestAppContext,
) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let ticket = start_host_wsl(view, 'a', cx);
        view.apply_wsl_cwd_probe_result(&ticket, WslLocationProbe::Found(location()), true, cx);
        view.wsl_cwd_observer.finish_command();
        assert!(view.sync_wsl_agent_identity());
        assert_eq!(view.running_program.as_deref(), Some("wsl"));
        assert!(view.agent_identity_receipt.is_none());
        assert!(view.runtime_agent().is_none());
        assert!(!view.agent_activity.command_ended());
        assert_ne!(view.agent_activity.status(), AgentStatus::Done);
        assert!(!view.agent_activity.hook_seen());
    });
}

#[gpui::test]
fn wsl_process_identity_does_not_probe_a_host_codex_session(cx: &mut TestAppContext) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let ticket = start_host_wsl(view, 'a', cx);
        let mut codex = location();
        codex.agent.kind = AgentKind::Codex;
        view.apply_wsl_cwd_probe_result(&ticket, WslLocationProbe::Found(codex), true, cx);
        assert_eq!(view.runtime_agent().unwrap().kind, "codex");
        view.probe_missing_codex_session(cx);
        assert!(
            !view.ai_session_probe_pending,
            "a guest Agent cannot authorize a Host rollout probe"
        );
        assert!(view.ai_session.is_none());
    });
}

#[gpui::test]
fn wsl_process_identity_owns_rechecks_until_its_command_ends(cx: &mut TestAppContext) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let ticket = start_host_wsl(view, 'a', cx);
        view.recovery.target = Some(crate::session::AgentSession {
            source: "codex".into(),
            session_id: Some("failed-restore".into()),
            session_file: None,
        });
        view.recovery.awaiting_confirmation = true;
        view.recovery.mark_failed();
        assert!(view.recovery.requires_foreground_receipt());
        let retired_epoch = view.agent_identity_probe_epoch;
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        view.agent_identity_probe_cancel = Some(cancelled.clone());
        view.agent_identity_probe_pending = true;
        view.apply_wsl_cwd_probe_result(&ticket, WslLocationProbe::Found(location()), true, cx);
        assert!(cancelled.load(std::sync::atomic::Ordering::Acquire));
        assert_ne!(view.agent_identity_probe_epoch, retired_epoch);
        view.agent_identity_probe_pending = false;
        view.agent_identity_probe_cancel = None;
        view.probe_agent_identity(cx);
        assert!(!view.agent_identity_probe_pending, "the observer owns Linux identity rechecks");
        assert_process_identity(view);
        view.wsl_cwd_observer.finish_command();
        view.sync_wsl_agent_identity();
        view.probe_agent_identity(cx);
        assert!(view.agent_identity_probe_pending, "Host recovery probing can resume afterward");
        view.stop_agent_identity_probe();
    });
}

#[gpui::test]
fn wsl_process_identity_does_not_let_a_retired_pty_overwrite_the_new_shell(
    cx: &mut TestAppContext,
) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let ticket = start_host_wsl(view, 'a', cx);
        view.apply_wsl_cwd_probe_result(&ticket, WslLocationProbe::Found(location()), true, cx);
        let retired = view
            .wsl_cwd_observer
            .begin_probe(Instant::now() + std::time::Duration::from_secs(4))
            .unwrap();
        let (mut replacement, _) = session::test_session();
        replacement.shell_pid = 10;
        view.session = Some(replacement);
        view.running_program = Some("pwsh".into());
        view.apply_wsl_cwd_probe_result(&retired, WslLocationProbe::Found(location()), false, cx);
        assert_eq!(view.running_program.as_deref(), Some("pwsh"));
        assert!(view.agent_identity_receipt.is_none());
        assert!(view.runtime_agent().is_none());
        assert_ne!(view.agent_activity.status(), AgentStatus::Done);
    });
}

#[gpui::test]
fn wsl_process_identity_survives_a_rejected_hook(cx: &mut TestAppContext) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        let ticket = start_host_wsl(view, 'a', cx);
        view.apply_wsl_cwd_probe_result(&ticket, WslLocationProbe::Found(location()), true, cx);
        view.recovery.target = Some(crate::session::AgentSession {
            source: "claude".into(),
            session_id: Some("failed-restore".into()),
            session_file: None,
        });
        view.recovery.awaiting_confirmation = true;
        view.recovery.mark_failed();
        let event = crate::ai_hook::parse_remote_envelope(
            b"nebula-hook/1 source=claude process=999:1\n{\"hook_event_name\":\"SessionStart\",\"session_id\":\"foreign-owner\"}",
            Some(view.pane_id),
        ).unwrap();
        assert!(!view.handle_ai_hook(&event, cx));
        assert_process_identity(view);
        assert!(view.wsl_identity_override_is_current());
    });
}
