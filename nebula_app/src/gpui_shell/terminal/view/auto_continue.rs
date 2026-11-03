//! Pane-owned retry timer. Never bypass hook arbitration or broadcast input.
use std::time::{Duration, Instant};

use gpui::{Context, EventEmitter as _};
use nebula_terminal::event_loop::Msg;
use nebula_terminal::grid::Dimensions as _;

use crate::ai_agents::AgentStatus;
use crate::ai_hook::auto_continue::RetryEvidence;
use crate::ai_hook::{AiHookEvent, AiHookKind};
use crate::gpui_shell::config::Settings;

use super::{TerminalView, TerminalViewEvent};

impl TerminalView {
    fn log_auto_continue(&self, stage: &'static str, reason: &'static str, cx: &Context<Self>) {
        log::debug!("auto_continue: pane={} stage={stage} reason={reason}", self.pane_id);
        let pane = self.pane_id;
        cx.background_executor()
            .spawn(async move {
                crate::logging::auto_continue_diagnostic(pane, stage, reason);
            })
            .detach();
    }

    pub(super) fn cancel_auto_continue(&mut self, cx: &Context<Self>, reason: &'static str) {
        if self.auto_continue.pending().is_some() {
            self.log_auto_continue("send_cancelled", reason, cx);
        }
        self.auto_continue_task = None;
        self.auto_continue.cancel();
    }

    pub(super) fn schedule_auto_continue(&mut self, event: &AiHookEvent, cx: &mut Context<Self>) {
        let settings = cx.try_global::<Settings>();
        let max_attempts = settings
            .map(|settings| settings.ai_auto_continue_attempts)
            .unwrap_or_else(|| nebula_settings::AutoContinueAttempts::default().count());
        let interval = settings
            .map(|settings| settings.ai_auto_continue_interval)
            .unwrap_or_else(|| nebula_settings::AutoContinueInterval::default().interval());
        if event.kind == AiHookKind::PromptSubmit {
            let screen = self.auto_continue_screen_snapshot();
            if self.auto_continue_screen_pause_reason(&event.source, screen.as_deref()).is_none() {
                let current_error = screen.and_then(|screen| {
                    settings.and_then(|settings| {
                        self.auto_continue_screen_error(&event.source, &screen, settings)
                    })
                });
                self.screen_error_at_prompt = current_error.clone();
            } else {
                self.screen_error_at_prompt =
                    self.auto_continue.screen_failure_baseline().map(str::to_owned);
            }
            self.auto_continue.remember_screen_at_prompt(self.screen_error_at_prompt.clone());
            self.set_screen_error_indicator(false, cx);
        }
        let previous_pending = self.auto_continue.pending();
        let delay = self.auto_continue.observe(event, Instant::now(), max_attempts, interval);
        if !cx.try_global::<Settings>().is_some_and(|settings| settings.ai_auto_continue) {
            self.cancel_auto_continue(cx, "disabled");
            return;
        }
        if self.auto_continue.pending().is_none() {
            if previous_pending.is_some() {
                let reason = match event.kind {
                    AiHookKind::PromptSubmit => "new_prompt",
                    AiHookKind::NeedsAttention => "waiting_for_input",
                    AiHookKind::SessionEnd => "session_ended",
                    AiHookKind::TurnDone => "turn_changed",
                    _ => "lifecycle_changed",
                };
                self.log_auto_continue("send_cancelled", reason, cx);
            }
            self.auto_continue_task = None;
        }
        let Some(delay) = delay else { return };
        let session_id = event.session_id.clone().expect("policy requires identity");
        self.log_auto_continue("hook_retry", "scheduled", cx);
        self.schedule_auto_continue_send(event.source.clone(), session_id, delay, cx);
    }

    pub(super) fn schedule_auto_continue_screen_failure(
        &mut self,
        program: &str,
        screen: &str,
        cx: &mut Context<Self>,
    ) {
        let settings = cx.try_global::<Settings>();
        let enabled = settings.is_some_and(|settings| settings.ai_auto_continue);
        let max_attempts = settings
            .map(|settings| settings.ai_auto_continue_attempts)
            .unwrap_or_else(|| nebula_settings::AutoContinueAttempts::default().count());
        let interval = settings
            .map(|settings| settings.ai_auto_continue_interval)
            .unwrap_or_else(|| nebula_settings::AutoContinueInterval::default().interval());
        let session_id = self
            .ai_session
            .as_ref()
            .filter(|identity| identity.source == program)
            .map(|identity| identity.session_id.clone());
        let failure = settings
            .and_then(|settings| self.auto_continue_screen_error(program, screen, settings));
        if failure.is_none() {
            if self.auto_continue.pending() == Some(RetryEvidence::ScreenFailure) {
                self.log_auto_continue("send_cancelled", "error_disappeared", cx);
            }
            self.screen_error_at_prompt = None;
        }
        let eligible = self.auto_continue_screen_can_retry(program, screen);
        let observation = self.auto_continue.observe_screen_failure_diagnostic(
            program,
            session_id.as_deref(),
            failure.as_deref(),
            eligible,
            enabled,
            Instant::now(),
            max_attempts,
            interval,
        );
        if observation.should_log {
            self.log_auto_continue("screen_retry", observation.reason.code(), cx);
        }
        if !enabled {
            self.cancel_auto_continue(cx, "disabled");
            return;
        }
        if self.auto_continue.pending().is_none() {
            self.auto_continue_task = None;
        }
        let Some(delay) = observation.delay else { return };
        let Some(session_id) = session_id else {
            self.log_auto_continue("screen_retry", "missing_session", cx);
            return;
        };
        self.schedule_auto_continue_send(program.to_owned(), session_id, delay, cx);
    }

    fn schedule_auto_continue_send(
        &mut self,
        program: String,
        session_id: String,
        delay: Duration,
        cx: &mut Context<Self>,
    ) {
        let input_epoch = self.prompt_input_epoch;
        let executor = cx.background_executor().clone();
        self.auto_continue_task = Some(cx.spawn(async move |this, cx| {
            let mut paused_reason = None;
            executor.timer(delay).await;
            loop {
                let keep_waiting = this.update(cx, |view, cx| {
                    let Some(settings) = cx.try_global::<Settings>() else {
                        view.auto_continue.take_pending(u8::MAX);
                        view.log_auto_continue("send_cancelled", "settings_unavailable", cx);
                        return false;
                    };
                    let identity_changed = !view.ai_session.as_ref().is_some_and(|identity| {
                        identity.source == program && identity.session_id == session_id
                    });
                    let guard_reason = if !settings.ai_auto_continue {
                        Some("disabled")
                    } else if view.prompt_input_epoch != input_epoch {
                        Some("input_changed")
                    } else if view.exited.is_some() {
                        Some("pane_exited")
                    } else if view.error.is_some() {
                        Some("pane_error")
                    } else if view.session.is_none() {
                        Some("pty_unavailable")
                    } else if view.marked_text.is_some() {
                        Some("marked_text")
                    } else if view.pending_runtime_submit.is_some() {
                        Some("runtime_submit_pending")
                    } else if view.running_program.as_deref() != Some(program.as_str()) {
                        Some("program_changed")
                    } else if identity_changed {
                        Some("session_changed")
                    } else {
                        None
                    };
                    if let Some(reason) = guard_reason {
                        view.auto_continue.take_pending(settings.ai_auto_continue_attempts);
                        view.log_auto_continue("send_cancelled", reason, cx);
                        return false;
                    }
                    let status = view.agent_activity.status();
                    let Some(pending) = view.auto_continue.pending() else { return false };
                    let status_reason = match pending {
                        RetryEvidence::HookFailure
                            if !matches!(status, AgentStatus::Idle | AgentStatus::Failed) =>
                        {
                            Some("hook_status_changed")
                        },
                        RetryEvidence::HookFailure if !view.last_command_failed => {
                            Some("hook_failure_cleared")
                        },
                        RetryEvidence::ScreenFailure
                            if matches!(status, AgentStatus::Blocked | AgentStatus::Unknown) =>
                        {
                            Some("blocked_or_unknown")
                        },
                        _ => None,
                    };
                    if let Some(reason) = status_reason {
                        view.auto_continue.take_pending(settings.ai_auto_continue_attempts);
                        view.log_auto_continue("send_cancelled", reason, cx);
                        return false;
                    }
                    let screen = view.auto_continue_screen_snapshot();
                    let pause = view.auto_continue_screen_pause_reason(&program, screen.as_deref());
                    if pause == Some("composer_unavailable")
                        && screen.as_deref().is_some_and(|screen| {
                            crate::ai_agents::detect(&program, screen)
                                .is_some_and(|detected| detected.status == AgentStatus::Blocked)
                        })
                    {
                        view.cancel_auto_continue(cx, "waiting_for_input");
                        return false;
                    }
                    if let Some(reason) = pause {
                        if paused_reason != Some(reason) {
                            view.log_auto_continue("send_paused", reason, cx);
                            paused_reason = Some(reason);
                        }
                        return true;
                    }
                    let screen = screen.expect("a current composer requires a screen snapshot");
                    if paused_reason.take().is_some() {
                        view.log_auto_continue("send_resumed", "current_viewport", cx);
                    }
                    let pending_screen_failure =
                        view.auto_continue.pending_screen_failure().map(str::to_owned);
                    let Some(evidence) =
                        view.auto_continue.take_pending(settings.ai_auto_continue_attempts)
                    else {
                        view.log_auto_continue("send_cancelled", "pending_missing_or_limit", cx);
                        return false;
                    };
                    let current_screen_failure =
                        view.auto_continue_screen_error(&program, &screen, settings);
                    let evidence_reason = match evidence {
                        RetryEvidence::HookFailure
                            if !crate::ai_agents::auto_continue_screen_is_safe(
                                &program, &screen,
                            ) =>
                        {
                            Some("screen_unsafe")
                        },
                        RetryEvidence::ScreenFailure
                            if !view.auto_continue_screen_can_retry(&program, &screen) =>
                        {
                            Some("screen_not_retryable")
                        },
                        RetryEvidence::ScreenFailure if current_screen_failure.is_none() => {
                            Some("error_disappeared")
                        },
                        RetryEvidence::ScreenFailure
                            if pending_screen_failure.as_deref().is_some_and(|failure| {
                                current_screen_failure.as_deref() != Some(failure)
                            }) =>
                        {
                            Some("error_changed")
                        },
                        _ => None,
                    };
                    if let Some(reason) = evidence_reason {
                        view.log_auto_continue("send_cancelled", reason, cx);
                        return false;
                    }
                    let bytes = if program == "codex" {
                        crate::input::terminal_input::build_runtime_codex_submission(
                            "continue",
                            view.term_mode(),
                        )
                    } else {
                        let Ok(enter) = view.runtime_key_sequence(
                            crate::runtime_api::RuntimeKey::Enter,
                            crate::runtime_api::RuntimeKeyModifiers::default(),
                            1,
                        ) else {
                            view.log_auto_continue("send_cancelled", "enter_encoding_failed", cx);
                            return false;
                        };
                        let mut bytes = b"continue".to_vec();
                        bytes.extend(enter);
                        bytes
                    };
                    let Some(session) = &view.session else {
                        view.log_auto_continue("send_cancelled", "pty_unavailable", cx);
                        return false;
                    };
                    if session.notifier.0.send(Msg::Input(bytes.into())).is_err() {
                        view.log_auto_continue("send_failed", "pty_channel_closed", cx);
                        return false;
                    }
                    view.log_auto_continue("send_result", "accepted", cx);
                    view.auto_continue.sent(Instant::now());
                    view.agent_activity.submitted();
                    view.last_command_failed = false;
                    cx.emit(TerminalViewEvent::TitleChanged);
                    cx.notify();
                    false
                });
                if !matches!(keep_waiting, Ok(true)) {
                    break;
                }
                // Keep this task's original deadline and input epoch while history is visible.
                executor.timer(Duration::from_secs(1)).await;
            }
        }));
    }

    fn auto_continue_screen_snapshot(&self) -> Option<String> {
        let session = self.session.as_ref()?;
        let term = session.term.lock();
        let lines = term.screen_lines();
        let columns = term.columns();
        if lines == 0 || columns == 0 || term.grid().display_offset() > 0 {
            return None;
        }
        let start = nebula_terminal::index::Point::new(
            nebula_terminal::index::Line((lines - lines.min(24)) as i32),
            nebula_terminal::index::Column(0),
        );
        let end = nebula_terminal::index::Point::new(
            nebula_terminal::index::Line(lines as i32 - 1),
            nebula_terminal::index::Column(columns.saturating_sub(1)),
        );
        Some(term.bounds_to_string(start, end))
    }

    fn auto_continue_screen_pause_reason(
        &self,
        program: &str,
        screen: Option<&str>,
    ) -> Option<&'static str> {
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.term.lock().grid().display_offset() > 0)
        {
            return Some("terminal_history");
        }
        let Some(screen) = screen else { return Some("screen_unavailable") };
        if crate::ai_agents::screen_is_history_viewport(program, screen) {
            Some("agent_history")
        } else if !crate::ai_agents::screen_has_current_composer(program, screen) {
            Some("composer_unavailable")
        } else {
            None
        }
    }

    fn auto_continue_screen_can_retry(&self, program: &str, screen: &str) -> bool {
        let status = self.agent_activity.status();
        if matches!(status, AgentStatus::Blocked | AgentStatus::Unknown) {
            return false;
        }
        crate::ai_agents::auto_continue_screen_is_safe(program, screen)
    }

    fn auto_continue_screen_error(
        &self,
        program: &str,
        screen: &str,
        settings: &Settings,
    ) -> Option<String> {
        crate::ai_agents::auto_continue_error_text(
            program,
            screen,
            &settings.ai_auto_continue_error_patterns,
        )
    }
}

#[cfg(all(test, feature = "gpui-test-support"))]
mod tests;
