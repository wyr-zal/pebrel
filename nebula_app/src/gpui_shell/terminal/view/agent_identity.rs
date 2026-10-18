use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use gpui::{Context, EventEmitter as _};

use super::{TerminalView, TerminalViewEvent};
use crate::platform::agent_process_identity::{self, AgentProbe, AgentRecheck, ForegroundAgent};

const PROBE_INTERVAL: Duration = Duration::from_secs(2);
const RECHECK_INTERVAL: Duration = Duration::from_secs(5);
const MAX_IDLE_INTERVAL: Duration = Duration::from_secs(30);

enum ProbeOutcome {
    Discovery(AgentProbe),
    Recheck(AgentRecheck),
}

fn probe_interval(receipt: bool, misses: u8, wake: bool) -> Duration {
    if wake {
        return PROBE_INTERVAL;
    }
    let base = if receipt { RECHECK_INTERVAL } else { PROBE_INTERVAL };
    base.saturating_mul(1u32 << misses.min(4)).min(MAX_IDLE_INTERVAL)
}

fn process_scope(
    context: &crate::runtime_exec::PaneExecContext,
) -> (Option<String>, Option<String>, Option<String>) {
    (
        context.wsl_distribution().flatten().map(str::to_owned),
        context.wsl_user().map(str::to_owned),
        context.process_instance().map(str::to_owned),
    )
}

fn additional_binding_is_current(
    current_input: u64,
    request_input: u64,
    same_pty: bool,
    same_scope: bool,
    closed: bool,
    cancelled: bool,
) -> bool {
    current_input == request_input && same_pty && same_scope && !closed && !cancelled
}

impl TerminalView {
    /// Invalidation requests cancellation; only the completed worker releases its slot.
    pub(super) fn invalidate_agent_identity_probe(&mut self) {
        self.agent_identity_probe_epoch = self.agent_identity_probe_epoch.wrapping_add(1);
        self.last_agent_identity_probe = None;
        self.agent_identity_probe_misses = 0;
        self.agent_identity_probe_wake = true;
        self.agent_identity_receipt = None;
        if let Some(cancelled) = &self.agent_identity_probe_cancel {
            cancelled.store(true, Ordering::Release);
        }
    }

    pub(super) fn wake_agent_identity_probe(&mut self, input: bool) {
        self.agent_identity_probe_wake = true;
        self.agent_identity_probe_misses = 0;
        if input && let Some(cancelled) = &self.agent_identity_probe_cancel {
            cancelled.store(true, Ordering::Release);
        }
    }

    pub(super) fn stop_agent_identity_probe(&self) {
        self.agent_identity_probe_closed.store(true, Ordering::Release);
        if let Some(cancelled) = &self.agent_identity_probe_cancel {
            cancelled.store(true, Ordering::Release);
        }
    }

    fn finish_agent_identity_probe(&mut self, worker: &Arc<AtomicBool>) -> bool {
        if !self
            .agent_identity_probe_cancel
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, worker))
        {
            return false;
        }
        self.agent_identity_probe_cancel = None;
        self.agent_identity_probe_pending = false;
        true
    }

    fn agent_identity_context(&self) -> Option<(crate::runtime_exec::PaneExecContext, String)> {
        let crate::display::SuggestEnv::Wsl { distro } = &self.suggest.suggest_env else {
            return None;
        };
        let context = self.exec_context.as_ref()?;
        context.wsl_distribution()?;
        context.process_instance()?;
        let context = if distro.is_empty() {
            context.clone()
        } else {
            context.for_wsl_distribution(distro)?
        };
        Some((context, distro.clone()))
    }

    fn owns_agent_identity_receipt(&self, expected: &ForegroundAgent) -> bool {
        !self.agent_activity.hook_seen()
            && self.agent_identity_receipt.as_ref() == Some(expected)
            && self.running_program.as_deref() == Some(expected.kind.slug())
    }

    fn apply_agent_identity_probe(
        &mut self,
        expected: Option<&ForegroundAgent>,
        result: ProbeOutcome,
        cx: &mut Context<Self>,
    ) {
        match (expected, result) {
            (None, ProbeOutcome::Discovery(result))
                if may_refine_process_identity(
                    self.agent_activity.hook_seen(),
                    self.running_program.as_deref(),
                ) =>
            {
                match result {
                    AgentProbe::Found(receipt) => {
                        self.running_program = Some(receipt.kind.slug().to_owned());
                        self.agent_identity_receipt = Some(receipt);
                        self.agent_identity_probe_misses = 0;
                        self.agent_activity.identify_foreground_process();
                        // Identity is not activity; the shared lifecycle owns all task-state changes.
                        cx.emit(TerminalViewEvent::TitleChanged);
                        cx.notify();
                    },
                    AgentProbe::NoAgent | AgentProbe::Unavailable => {
                        self.agent_identity_probe_misses =
                            self.agent_identity_probe_misses.saturating_add(1);
                    },
                }
            },
            (Some(expected), ProbeOutcome::Recheck(result))
                if self.owns_agent_identity_receipt(expected) =>
            {
                match result {
                    AgentRecheck::Current => self.agent_identity_probe_misses = 0,
                    AgentRecheck::Unavailable => {
                        self.agent_identity_probe_misses =
                            self.agent_identity_probe_misses.saturating_add(1);
                    },
                    AgentRecheck::Changed => {
                        self.agent_identity_receipt = None;
                        self.running_program = None;
                        self.agent_activity.forget_unhooked_identity();
                        self.invalidate_ai_session_probe();
                        if self.ai_session_from_probe
                            && self
                                .ai_session
                                .as_ref()
                                .is_some_and(|identity| identity.source == expected.kind.slug())
                        {
                            self.ai_session = None;
                            self.ai_session_from_probe = false;
                            cx.emit(TerminalViewEvent::SessionIdentityChanged);
                        }
                        self.agent_identity_probe_misses = 0;
                        self.agent_identity_probe_wake = true;
                        cx.emit(TerminalViewEvent::TitleChanged);
                        cx.notify();
                    },
                }
            },
            _ => {},
        }
    }
}

pub(super) fn may_refine_process_identity(hook_seen: bool, running_program: Option<&str>) -> bool {
    !hook_seen && running_program.and_then(crate::ai_agents::AgentKind::parse).is_none()
}

fn process_probe_result_is_current(
    current_epoch: u64,
    result_epoch: u64,
    _command_running: bool,
    hook_seen: bool,
    exited: bool,
    current_shell_pid: Option<u32>,
    result_shell_pid: u32,
    running_program: Option<&str>,
    current_distribution: Option<&str>,
    result_distribution: &str,
) -> bool {
    current_epoch == result_epoch
        && !hook_seen
        && !exited
        && current_shell_pid == Some(result_shell_pid)
        && current_distribution == Some(result_distribution)
        && may_refine_process_identity(hook_seen, running_program)
}

impl TerminalView {
    pub(super) fn probe_agent_identity(&mut self, cx: &mut Context<'_, Self>) {
        if self.exited.is_some()
            || self.error.is_some()
            || self.agent_activity.hook_seen()
            || self.agent_identity_probe_closed.load(Ordering::Acquire)
            || self.agent_identity_probe_pending
        {
            return;
        }
        if self
            .agent_identity_receipt
            .as_ref()
            .is_some_and(|receipt| !self.owns_agent_identity_receipt(receipt))
        {
            self.agent_identity_receipt = None;
        }
        let expected = self.agent_identity_receipt.clone();
        if expected.is_none()
            && !may_refine_process_identity(false, self.running_program.as_deref())
        {
            return;
        }
        let interval = probe_interval(
            expected.is_some(),
            self.agent_identity_probe_misses,
            self.agent_identity_probe_wake,
        );
        if self.last_agent_identity_probe.is_some_and(|at| at.elapsed() < interval) {
            return;
        }
        let Some((context, result_distribution)) = self.agent_identity_context() else { return };
        let Some(session) = &self.session else { return };
        let shell_pid = session.shell_pid;
        if shell_pid == 0 {
            return;
        }
        let pty = Arc::downgrade(&session.term);
        let scope = process_scope(&context);
        let pane_id = self.pane_id;
        let epoch = self.agent_identity_probe_epoch;
        let input_epoch = self.prompt_input_epoch;
        let cancelled = Arc::new(AtomicBool::new(false));
        self.agent_identity_probe_cancel = Some(cancelled.clone());
        self.agent_identity_probe_pending = true;
        self.agent_identity_probe_wake = false;
        self.last_agent_identity_probe = Some(Instant::now());

        let worker_cancelled = cancelled.clone();
        let worker_expected = expected.clone();
        let work = cx.background_executor().spawn(async move {
            let is_cancelled = || worker_cancelled.load(Ordering::Acquire);
            match worker_expected {
                Some(receipt) => {
                    ProbeOutcome::Recheck(agent_process_identity::recheck_foreground_agent(
                        pane_id,
                        &context,
                        &receipt,
                        &is_cancelled,
                    ))
                },
                None => ProbeOutcome::Discovery(agent_process_identity::probe_foreground_agent(
                    pane_id,
                    &context,
                    &is_cancelled,
                )),
            }
        });
        cx.spawn(async move |this, cx| {
            let detected = work.await;
            let _ = this.update(cx, |view, cx| {
                // Epoch changes do not free a still-running worker; its own Arc releases the slot.
                if !view.finish_agent_identity_probe(&cancelled) {
                    return;
                }
                let same_pty = view
                    .session
                    .as_ref()
                    .is_some_and(|session| pty.ptr_eq(&Arc::downgrade(&session.term)));
                let current_context = view.agent_identity_context();
                let same_scope = current_context
                    .as_ref()
                    .is_some_and(|(context, _)| process_scope(context) == scope);
                let program =
                    if expected.is_some() { None } else { view.running_program.as_deref() };
                if !additional_binding_is_current(
                    view.prompt_input_epoch,
                    input_epoch,
                    same_pty,
                    same_scope,
                    view.agent_identity_probe_closed.load(Ordering::Acquire),
                    cancelled.load(Ordering::Acquire),
                ) || !process_probe_result_is_current(
                    view.agent_identity_probe_epoch,
                    epoch,
                    false,
                    view.agent_activity.hook_seen(),
                    view.exited.is_some() || view.error.is_some(),
                    view.session.as_ref().map(|session| session.shell_pid),
                    shell_pid,
                    program,
                    current_context.as_ref().map(|(_, distro)| distro.as_str()),
                    &result_distribution,
                ) {
                    view.agent_identity_probe_wake = true;
                    return;
                }
                view.apply_agent_identity_probe(expected.as_ref(), detected, cx);
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::{may_refine_process_identity, process_probe_result_is_current};

    fn current(
        current_epoch: u64,
        result_epoch: u64,
        command_running: bool,
        hook_seen: bool,
        shell_pid: Option<u32>,
        running_program: Option<&str>,
    ) -> bool {
        process_probe_result_is_current(
            current_epoch,
            result_epoch,
            command_running,
            hook_seen,
            false,
            shell_pid,
            10,
            running_program,
            Some("Debian"),
            "Debian",
        )
    }

    #[test]
    fn only_unknown_wrapper_names_can_be_refined() {
        assert!(may_refine_process_identity(false, Some("cc")));
        assert!(may_refine_process_identity(false, None));
        assert!(!may_refine_process_identity(false, Some("codex")));
        assert!(!may_refine_process_identity(true, Some("cc")));
    }

    #[test]
    fn unknown_wrapper_can_be_refined_but_authoritative_identity_cannot() {
        assert!(current(4, 4, true, false, Some(10), Some("cc")));
        assert!(!current(4, 4, true, true, Some(10), Some("cc")));
        assert!(!current(4, 4, true, false, Some(10), Some("codex")));
        assert!(current(4, 4, false, false, Some(10), Some("cc")));
        assert!(!current(5, 4, true, false, Some(10), Some("cc")));
        assert!(!current(4, 4, true, false, Some(11), Some("cc")));
        assert!(!process_probe_result_is_current(
            4,
            4,
            true,
            false,
            false,
            Some(10),
            10,
            Some("cc"),
            Some("Ubuntu"),
            "Debian",
        ));
    }
}

#[cfg(test)]
mod independent_probe_regressions {
    use super::*;

    #[test]
    fn missing_command_start_does_not_reject_a_current_identity_result() {
        assert!(process_probe_result_is_current(
            4,
            4,
            false,
            false,
            false,
            Some(10),
            10,
            None,
            Some("Debian"),
            "Debian",
        ));
        assert!(!process_probe_result_is_current(
            5,
            4,
            false,
            false,
            false,
            Some(10),
            10,
            None,
            Some("Debian"),
            "Debian",
        ));
        assert!(!process_probe_result_is_current(
            4,
            4,
            false,
            true,
            false,
            Some(10),
            10,
            None,
            Some("Debian"),
            "Debian",
        ));
    }

    #[cfg(feature = "gpui-test-support")]
    #[gpui::test]
    fn invalidation_keeps_single_flight_until_worker_finishes(cx: &mut gpui::TestAppContext) {
        let (view, window, _) = super::super::startup_tests::open(cx);
        view.update(window, |view, _| {
            let cancellation = Arc::new(AtomicBool::new(false));
            view.agent_identity_probe_pending = true;
            view.agent_identity_probe_cancel = Some(cancellation.clone());
            view.invalidate_agent_identity_probe();
            assert!(cancellation.load(Ordering::Acquire));
            assert!(view.agent_identity_probe_pending, "cancellation does not mean process exit");
            assert!(
                view.agent_identity_probe_cancel
                    .as_ref()
                    .is_some_and(|current| { Arc::ptr_eq(current, &cancellation) })
            );
        });
    }
}

#[cfg(test)]
mod proof_contract_tests {
    use super::*;

    #[test]
    fn result_requires_unchanged_pty_input_scope_and_live_request() {
        assert!(additional_binding_is_current(3, 3, true, true, false, false));
        assert!(!additional_binding_is_current(4, 3, true, true, false, false));
        assert!(!additional_binding_is_current(3, 3, false, true, false, false));
        assert!(!additional_binding_is_current(3, 3, true, false, false, false));
        assert!(!additional_binding_is_current(3, 3, true, true, true, false));
        assert!(!additional_binding_is_current(3, 3, true, true, false, true));
    }

    #[test]
    fn empty_shell_backoff_is_capped_and_real_activity_wakes_it() {
        assert_eq!(probe_interval(false, 0, false), Duration::from_secs(2));
        assert_eq!(probe_interval(false, 1, false), Duration::from_secs(4));
        assert_eq!(probe_interval(false, 255, false), MAX_IDLE_INTERVAL);
        assert_eq!(probe_interval(false, 255, true), PROBE_INTERVAL);
        assert_eq!(probe_interval(true, 0, false), RECHECK_INTERVAL);
    }

    #[cfg(feature = "gpui-test-support")]
    fn receipt(pid: u32) -> ForegroundAgent {
        ForegroundAgent {
            kind: crate::ai_agents::AgentKind::Codex,
            process: agent_process_identity::ProcessStamp { pid, start_time: u64::from(pid) },
            shell: agent_process_identity::ProcessStamp { pid: 10, start_time: 10 },
            tty_nr: 34823,
            process_group: pid,
        }
    }

    #[cfg(feature = "gpui-test-support")]
    #[gpui::test]
    fn only_the_finished_worker_releases_its_slot(cx: &mut gpui::TestAppContext) {
        let (view, window, _) = super::super::startup_tests::open(cx);
        view.update(window, |view, _| {
            let old = Arc::new(AtomicBool::new(false));
            let new = Arc::new(AtomicBool::new(false));
            view.agent_identity_probe_cancel = Some(old.clone());
            view.agent_identity_probe_pending = true;
            view.invalidate_agent_identity_probe();
            assert!(view.agent_identity_probe_pending);
            assert!(view.finish_agent_identity_probe(&old));
            assert!(!view.agent_identity_probe_pending);
            view.agent_identity_probe_cancel = Some(new.clone());
            view.agent_identity_probe_pending = true;
            assert!(!view.finish_agent_identity_probe(&old));
            assert!(view.agent_identity_probe_pending);
            view.stop_agent_identity_probe();
            assert!(new.load(Ordering::Acquire));
            assert!(view.agent_identity_probe_closed.load(Ordering::Acquire));
            assert!(view.finish_agent_identity_probe(&new));
        });
    }

    #[cfg(feature = "gpui-test-support")]
    #[gpui::test]
    fn receipt_discovery_is_not_activity_and_changed_only_revokes_itself(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, window, _) = super::super::startup_tests::open(cx);
        view.update(window, |view, cx| {
            let old = receipt(200);
            let new = receipt(201);
            let status = view.agent_activity.status();
            assert!(!view.command_running);
            assert!(view.command_started.is_none());
            view.apply_agent_identity_probe(
                None,
                ProbeOutcome::Discovery(AgentProbe::Found(old.clone())),
                cx,
            );
            assert_eq!(view.running_program.as_deref(), Some("codex"));
            assert_eq!(view.agent_activity.status(), status, "process existence is not a turn");
            view.apply_agent_identity_probe(
                Some(&old),
                ProbeOutcome::Recheck(AgentRecheck::Unavailable),
                cx,
            );
            assert_eq!(view.agent_identity_receipt.as_ref(), Some(&old));
            view.agent_identity_receipt = Some(new.clone());
            view.apply_agent_identity_probe(
                Some(&old),
                ProbeOutcome::Recheck(AgentRecheck::Changed),
                cx,
            );
            assert_eq!(view.agent_identity_receipt.as_ref(), Some(&new));

            view.agent_identity_receipt = Some(old.clone());
            view.ai_session = Some(crate::display::AiSessionIdentity {
                source: "codex".into(),
                session_id: "derived-old".into(),
            });
            view.ai_session_from_probe = true;
            view.recovery.target = Some(crate::session::AgentSession {
                source: "codex".into(),
                session_id: Some("derived-old".into()),
                session_file: None,
            });
            let epoch = view.ai_session_probe_epoch;
            view.apply_agent_identity_probe(
                Some(&old),
                ProbeOutcome::Recheck(AgentRecheck::Changed),
                cx,
            );
            assert!(view.running_program.is_none());
            assert!(view.agent_identity_receipt.is_none());
            assert!(view.ai_session.is_none());
            assert!(!view.ai_session_from_probe);
            assert_ne!(
                view.ai_session_probe_epoch, epoch,
                "old session probes must not refill identity"
            );
            assert!(view.recovery.target.is_some(), "keep the durable recovery target");
            assert_ne!(view.agent_activity.status(), crate::ai_agents::AgentStatus::Done);
        });
    }

    #[cfg(feature = "gpui-test-support")]
    #[gpui::test]
    fn verified_foreground_identity_reopens_observation_after_a_shell_boundary(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::ai_agents::{AgentStatus, AgentStatusSource, Detection};
        let (view, window, _) = super::super::startup_tests::open(cx);
        view.update(window, |view, cx| {
            view.agent_activity.command_finished();
            assert!(!view.agent_activity.allows_screen());
            view.apply_agent_identity_probe(None,
                ProbeOutcome::Discovery(AgentProbe::Found(receipt(202))), cx);
            assert!(view.agent_activity.allows_screen(), "the verified new process owns current chrome");
            assert_eq!(view.agent_activity.status(), AgentStatus::Unknown);
            assert_eq!(view.agent_activity.source(), AgentStatusSource::Process);
            assert!(view.agent_activity.uncertainty().is_some());
            assert!(view.agent_activity.observe_screen(Some(Detection {
                agent: crate::ai_agents::AgentKind::Codex,
                status: AgentStatus::Working, rule_id: "test.current_chrome".into(),
            })));
            assert_eq!(view.agent_activity.status(), AgentStatus::Working);
            assert!(view.agent_activity.uncertainty().is_none());
            let old_stop = crate::ai_hook::parse_remote_envelope(
                b"nebula-hook/1 source=codex process=10:20 codex_hooks=full\n{\"hook_event_name\":\"Stop\",\"session_id\":\"old-command\"}",
                Some(view.pane_id),
            ).unwrap();
            assert!(!view.agent_activity.accepts_hook(&old_stop));
            assert!(!view.agent_activity.apply_hook(&old_stop));
            assert_eq!(view.agent_activity.status(), AgentStatus::Working);
        });
    }

    #[cfg(feature = "gpui-test-support")]
    #[gpui::test]
    fn changed_does_not_erase_foreign_or_hook_session_identity(cx: &mut gpui::TestAppContext) {
        let (view, window, _) = super::super::startup_tests::open(cx);
        view.update(window, |view, cx| {
            let old = receipt(200);
            for (source, from_probe) in [("claude", true), ("codex", false)] {
                view.running_program = Some("codex".into());
                view.agent_identity_receipt = Some(old.clone());
                view.ai_session = Some(crate::display::AiSessionIdentity { source: source.into(), session_id: "keep".into() });
                view.ai_session_from_probe = from_probe;
                view.apply_agent_identity_probe(Some(&old), ProbeOutcome::Recheck(AgentRecheck::Changed), cx);
                assert_eq!(view.ai_session.as_ref().unwrap().session_id, "keep");
                assert_eq!(view.ai_session_from_probe, from_probe);
            }
            let event = crate::ai_hook::parse_remote_envelope(
                b"nebula-hook/1 source=codex process=10:20 codex_hooks=full\n{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"hook-new\"}",
                Some(view.pane_id),
            ).unwrap();
            assert!(view.agent_activity.apply_hook(&event));
            view.invalidate_agent_identity_probe();
            view.running_program = Some("codex".into());
            view.ai_session = Some(crate::display::AiSessionIdentity { source: "codex".into(), session_id: "hook-new".into() });
            view.ai_session_from_probe = false;
            view.apply_agent_identity_probe(Some(&old), ProbeOutcome::Recheck(AgentRecheck::Changed), cx);
            assert_eq!(view.running_program.as_deref(), Some("codex"));
            assert_eq!(view.ai_session.as_ref().unwrap().session_id, "hook-new");
            assert!(view.agent_activity.hook_seen());
        });
    }
}
