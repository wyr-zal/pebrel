//! GPUI adapter for the shared Agent lifecycle: route evidence, project effects.

use gpui::{Context, EventEmitter as _};
use nebula_terminal::grid::Dimensions as _;
use nebula_terminal::index::{Column, Line, Point};

use crate::ai_agents::{AgentStatus, AgentStatusSource};
#[cfg(windows)]
use crate::ai_hook::lifecycle::CodexTerminalEvidence;
use crate::ai_hook::{AiHookEvent, AiHookKind};

use super::{TerminalView, TerminalViewEvent, notifications};

impl TerminalView {
    /// True means the selected pane applied the Hook, not merely that it was routed here.
    pub fn handle_ai_hook(&mut self, event: &AiHookEvent, cx: &mut Context<Self>) -> bool {
        if let Some(client_pid) = event.client_pid
            && event.pane == Some(self.pane_id)
            && let Some(session) = &self.session
            && crate::process_tree::is_within_tree(client_pid, session.shell_pid) == Some(false)
        {
            log::warn!("ai_hook: pane={} rejected foreign client pid={client_pid}", self.pane_id);
            return false;
        }
        if !self.agent_activity.accepts_hook(event) {
            log::debug!("ai_hook: pane={} ignored hook outside the active lifecycle", self.pane_id);
            return false;
        }
        if !self.recovery.accepts_choice_event(event) {
            return false;
        }
        // A lifecycle-only Hook ID cannot downgrade the independently verified
        // open rollout. Keep consuming lifecycle events; the next process probe
        // can discover a new conversation selected inside the same CLI process.
        let keep_native_identity =
            event.source == "codex" && event.session_file.is_none() && self.ai_session_from_probe;
        let target = event.session_id.as_ref().filter(|_| !keep_native_identity).map(|id| {
            crate::session::AgentSession {
                source: event.source.clone(),
                session_id: Some(id.clone()),
                session_file: event.session_file.clone(),
            }
        });
        if target.as_ref().is_some_and(|target| !self.recovery.accepts(target)) {
            return false;
        }
        let verdict = crate::ai_hook::accept_for_pane(event, self.pane_id);
        if !verdict.accepted() {
            log::debug!(
                "ai_hook: pane={} source={} dropped {verdict:?}",
                self.pane_id,
                event.source
            );
            return false;
        }
        self.agent_activity.apply_hook(event);
        self.invalidate_agent_identity_probe();
        if matches!(
            event.kind,
            AiHookKind::SessionStart
                | AiHookKind::NeedsAttention
                | AiHookKind::PromptSubmit
                | AiHookKind::ToolComplete
                | AiHookKind::TurnDone
        ) {
            self.last_command_failed = event.kind == AiHookKind::TurnDone
                && event.active_background_tasks() == 0
                && matches!(
                    event.turn_outcome,
                    crate::ai_hook::AiTurnOutcome::Failed
                        | crate::ai_hook::AiTurnOutcome::Incomplete
                );
        }
        if let Some(session) = &self.session {
            session.term.lock().set_redraw_anchor_enabled(
                event.source == "pi" && event.kind != AiHookKind::SessionEnd,
            );
        }
        if let Some(target) = target {
            let previous = self.recovery.target.clone();
            self.recovery.confirm(target);
            if previous != self.recovery.target {
                self.set_screen_error_indicator(false, cx);
                cx.emit(TerminalViewEvent::SessionIdentityChanged);
            }
        }

        // Only the owner may publish answers or replace the resumable identity.
        if self.ssh_destination.is_none()
            && self.exited.is_none()
            && self.answers.observe(event, self.pane_id)
            && let Some(reader) = &self.answer_reader
        {
            reader.update(cx, |reader, cx| reader.answer_arrived(cx));
        }
        if event.kind == AiHookKind::NeedsAttention
            && let Some(reader) = &self.answer_reader
        {
            reader.update(cx, |reader, cx| reader.needs_attention(cx));
        }
        if let Some(id) = event.session_id.as_deref()
            && let Err(error) =
                crate::ai_sessions::record_hook_session(&event.source, id, &self.cwd, None)
        {
            log::warn!("agent session index: could not record {} {id}: {error}", event.source);
        }
        if event.kind == AiHookKind::SessionEnd {
            self.set_screen_error_indicator(false, cx);
            self.clear_foreground_agent_state(cx);
        } else {
            self.running_program = Some(event.source.clone());
            if let Some(id) = event.session_id.as_deref().filter(|_| !keep_native_identity) {
                self.ai_session_from_probe = false;
                self.ai_session = Some(crate::display::AiSessionIdentity {
                    source: event.source.clone(),
                    session_id: id.to_owned(),
                });
            }
        }
        let status = self.agent_activity.status();
        self.confirmation.observe_waiting(status == AgentStatus::Blocked);
        if event.kind == AiHookKind::NeedsAttention {
            let request = event
                .event_id
                .clone()
                .unwrap_or_else(|| format!("{}:{}", event.source, event.received_sequence));
            self.confirmation.set_provider_request(Some(&request));
            if let Some(mut attention) = event.attention.clone() {
                attention.pane_id = Some(self.pane_id);
                cx.emit(TerminalViewEvent::AiAttention(attention));
            } else {
                cx.emit(TerminalViewEvent::Notification(crate::notify::Notification::AiTurn {
                    program: event.source.clone(),
                    message: event.message.clone(),
                    attention: true,
                }));
            }
        } else if event.kind == AiHookKind::TurnDone
            && let Some(notification) =
                crate::notify::Notification::from_ai_hook(event, event.message.clone(), false)
        {
            cx.emit(TerminalViewEvent::Notification(notification));
        }
        self.schedule_auto_continue(event, cx);
        cx.emit(TerminalViewEvent::TitleChanged);
        cx.notify();
        true
    }

    pub(crate) fn screen_error_indicator(&self) -> bool {
        self.screen_error_indicator
    }

    #[cfg(windows)]
    fn probe_wsl_codex_terminal(&mut self, screen: &str, cx: &mut Context<Self>) {
        // Chrome only wakes a bounded probe; its text cannot end a hook-owned
        // turn. The process's open rollout must prove the exact terminal record.
        let Some(target) = self.agent_activity.codex_failure_target() else { return };
        let idle_chrome = crate::ai_agents::codex_idle_chrome_complete(screen);
        if idle_chrome
            && self.agent_activity.mark_status_unconfirmed(
                crate::ai_agents::AgentStatusUncertainty::AwaitingTerminalEvidence,
            )
        {
            cx.emit(TerminalViewEvent::TitleChanged);
            cx.notify();
        }
        if self.codex_failure_probe.is_some()
            || self
                .last_codex_failure_probe
                .is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(2))
            || !(screen.lines().any(|line| line.trim_start().starts_with('■')) || idle_chrome)
        {
            return;
        }
        let Some(probe) = self
            .session
            .as_ref()
            .and_then(|session| session.wsl_hook.as_ref())
            .and_then(|channel| channel.failure_probe())
        else {
            return;
        };
        self.last_codex_failure_probe = Some(std::time::Instant::now());
        let input_epoch = self.prompt_input_epoch;
        let generation = probe.generation.clone();
        let expected = target.clone();
        let work = cx.background_executor().spawn(async move { probe.run(&expected) });
        self.codex_failure_probe = Some(cx.spawn(async move |this, cx| {
            let terminal = work.await;
            let _ = this.update(cx, |view, cx| {
                view.codex_failure_probe = None;
                if let Some(terminal) = terminal {
                    view.apply_wsl_codex_terminal(&generation, input_epoch, &target, terminal, cx);
                }
            });
        }));
    }

    #[cfg(windows)]
    pub(super) fn apply_wsl_codex_terminal(
        &mut self,
        generation: &str,
        input_epoch: u64,
        target: &crate::ai_hook::lifecycle::CodexFailureTarget,
        terminal: CodexTerminalEvidence,
        cx: &mut Context<Self>,
    ) {
        if self.exited.is_some()
            || self.prompt_input_epoch != input_epoch
            || self.running_program.as_deref() != Some("codex")
            || !self
                .session
                .as_ref()
                .and_then(|session| session.wsl_hook.as_ref())
                .is_some_and(|channel| channel.accepts(generation))
            || !self.agent_activity.observe_codex_terminal(target, &terminal)
        {
            return;
        }
        self.confirmation.observe_waiting(false);
        if let CodexTerminalEvidence::Failed { error } = terminal {
            cx.emit(TerminalViewEvent::Notification(crate::notify::Notification::AiTurnIssue {
                program: "codex".into(),
                message: Some(error),
                outcome: crate::ai_hook::AiTurnOutcome::Failed,
            }));
        }
        cx.emit(TerminalViewEvent::TitleChanged);
        cx.notify();
    }

    pub(super) fn set_screen_error_indicator(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.screen_error_indicator == visible {
            return;
        }
        self.screen_error_indicator = visible;
        log::debug!(
            "auto_continue: screen_error_indicator={}",
            if visible { "visible" } else { "cleared" }
        );
        cx.emit(TerminalViewEvent::TitleChanged);
        cx.notify();
    }

    /// Check shell boundaries first; only an integration lacking lifecycle
    /// authority may then consume a screen observation. Silence has no meaning.
    pub fn refresh_agent_screen_state(&mut self, cx: &mut Context<Self>) {
        if self.exited.is_some()
            || matches!(self.ssh_stage, Some(crate::ssh_session::SshStage::Failed(_)))
        {
            self.set_screen_error_indicator(false, cx);
            return;
        }
        self.flush_pending_runtime_submit(cx);
        self.flush_pending_shell_command(cx);
        self.reconcile_shell_activity(cx);
        self.on_native_cmd_prompt(cx);
        self.probe_missing_codex_session(cx);
        let retry_source = self
            .running_program
            .as_deref()
            .filter(|program| matches!(*program, "claude" | "codex"))
            .map(str::to_owned);
        let screen_observation_enabled = retry_source.is_some();
        let allows_screen = self.agent_activity.allows_screen();
        let Some(session) = &self.session else {
            self.set_screen_error_indicator(false, cx);
            return;
        };
        let (prompt_restored, screen) = {
            let term = session.term.lock();
            let lines = term.screen_lines();
            let columns = term.columns();
            if lines == 0 || columns == 0 {
                (false, None)
            } else {
                let pending = self.command_running || self.running_program.is_some();
                let prompt_restored = pending
                    && self.suggest.pending_command_prompt.as_deref().is_some_and(|expected| {
                        crate::display::nebula_shell_prompt_restored_from_raw_grid(
                            &term,
                            expected,
                            &self.suggest.suggest_env,
                        )
                    });
                let screen = (allows_screen || screen_observation_enabled).then(|| {
                    let start = Point::new(Line((lines - lines.min(24)) as i32), Column(0));
                    let end = Point::new(Line(lines as i32 - 1), Column(columns.saturating_sub(1)));
                    if self.agent_activity.awaiting_cancel() {
                        // Copy text joins soft wraps. Cancellation needs the
                        // visual composer rows after a full-width TUI redraw.
                        let mut screen = String::new();
                        for row in start.line.0..=end.line.0 {
                            if row > start.line.0 {
                                screen.push('\n');
                            }
                            screen.push_str(&term.bounds_to_string(
                                Point::new(Line(row), Column(0)),
                                Point::new(Line(row), end.column),
                            ));
                        }
                        screen
                    } else {
                        term.bounds_to_string(start, end)
                    }
                });
                (prompt_restored, screen)
            }
        };
        if prompt_restored
            && !self.native_prompt_seen
            && self.pending_runtime_submit.is_none()
            && self.pending_shell_command.is_none()
            && !self.recovery.preparing()
        {
            self.set_screen_error_indicator(false, cx);
            if self.suggest.suggest_env.is_this_machine() {
                self.probe_restored_prompt(cx);
            } else {
                self.finish_foreground_command(None, cx);
            }
            return;
        }
        let Some(screen) = screen else {
            self.set_screen_error_indicator(false, cx);
            return;
        };
        let settings = cx.try_global::<crate::gpui_shell::config::Settings>();
        let screen_error_visible = retry_source.as_deref().is_some_and(|program| {
            settings.is_some_and(|settings| {
                crate::ai_agents::auto_continue_error_text(
                    program,
                    &screen,
                    &settings.ai_auto_continue_error_patterns,
                )
                .is_some()
            })
        });
        self.set_screen_error_indicator(screen_error_visible, cx);
        #[cfg(windows)]
        self.probe_wsl_codex_terminal(&screen, cx);
        if let Some(program) = retry_source.as_deref() {
            self.schedule_auto_continue_screen_failure(program, &screen, cx);
        }
        if !allows_screen {
            if self.agent_activity.awaiting_cancel()
                && self.agent_activity.observe_cancel(crate::ai_agents::detect("claude", &screen))
            {
                self.confirmation.observe_waiting(false);
                cx.emit(TerminalViewEvent::TitleChanged);
                cx.notify();
            }
            return;
        }
        let detected = notifications::screen_identity_allowed(self.running_program.as_deref())
            .then(|| crate::ai_agents::identify(&screen))
            .flatten();
        let Some(program) =
            notifications::screen_program(self.running_program.as_deref(), detected)
        else {
            self.agent_activity.observe_screen(None);
            return;
        };
        if self.running_program.as_deref() != Some(program.as_str()) {
            self.running_program = Some(program.clone());
            self.agent_activity.identify(AgentStatusSource::Screen);
            cx.emit(TerminalViewEvent::TitleChanged);
            cx.notify();
        }
        let previous = self.agent_activity.status();
        if !self.agent_activity.observe_screen(crate::ai_agents::detect(&program, &screen)) {
            return;
        }
        let next = self.agent_activity.status();
        self.confirmation.observe_waiting(next == AgentStatus::Blocked);
        if let Some(attention) =
            notifications::screen_notification(previous, next, self.agent_activity.hook_seen())
        {
            let notification = if next == AgentStatus::Failed {
                crate::notify::Notification::AiTurnIssue {
                    program: program.clone(),
                    message: None,
                    outcome: crate::ai_hook::AiTurnOutcome::Failed,
                }
            } else {
                crate::notify::Notification::AiTurn {
                    program: program.clone(),
                    message: None,
                    attention,
                }
            };
            cx.emit(TerminalViewEvent::Notification(notification));
        }
        log::debug!(
            "agent screen: pane={} {previous:?}->{next:?} rule={:?}",
            self.pane_id,
            self.agent_activity.rule()
        );
        cx.emit(TerminalViewEvent::TitleChanged);
        cx.notify();
    }
}
