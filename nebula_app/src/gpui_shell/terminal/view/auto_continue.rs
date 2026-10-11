//! Pane-owned retry timer. Never bypass hook arbitration or broadcast input.
use std::time::Instant;

use gpui::{Context, EventEmitter as _};
use nebula_terminal::event_loop::Msg;
use nebula_terminal::grid::Dimensions as _;

use crate::ai_agents::AgentStatus;
use crate::ai_hook::auto_continue::RetryEvidence;
use crate::ai_hook::{AiHookEvent, AiHookKind};
use crate::gpui_shell::config::Settings;

use super::{TerminalView, TerminalViewEvent};

impl TerminalView {
    pub(super) fn cancel_auto_continue(&mut self) {
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
            let current_error = self.auto_continue_screen_snapshot().and_then(|screen| {
                settings.and_then(|settings| {
                    self.auto_continue_screen_error(&event.source, &screen, settings)
                })
            });
            self.auto_continue.remember_screen_at_prompt(current_error);
        }
        let delay = self.auto_continue.observe(event, Instant::now(), max_attempts, interval);
        if !cx.try_global::<Settings>().is_some_and(|settings| settings.ai_auto_continue) {
            self.cancel_auto_continue();
            return;
        }
        if self.auto_continue.pending().is_none() {
            self.auto_continue_task = None;
        }
        let Some(delay) = delay else { return };
        let session_id = event.session_id.clone().expect("policy requires identity");
        self.schedule_auto_continue_send(event.source.clone(), session_id, delay, cx);
    }

    pub(super) fn schedule_auto_continue_screen_failure(
        &mut self,
        program: &str,
        screen: &str,
        cx: &mut Context<Self>,
    ) {
        let settings = cx.try_global::<Settings>();
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
        let eligible = self.auto_continue_screen_can_retry(program, screen);
        let delay = self.auto_continue.observe_screen_failure(
            program,
            session_id.as_deref(),
            failure.as_deref(),
            eligible,
            Instant::now(),
            max_attempts,
            interval,
        );
        if !cx.try_global::<Settings>().is_some_and(|settings| settings.ai_auto_continue) {
            self.cancel_auto_continue();
            return;
        }
        if self.auto_continue.pending().is_none() {
            self.auto_continue_task = None;
        }
        let Some(delay) = delay else { return };
        let Some(session_id) = session_id else { return };
        self.schedule_auto_continue_send(program.to_owned(), session_id, delay, cx);
    }

    fn schedule_auto_continue_send(
        &mut self,
        program: String,
        session_id: String,
        delay: std::time::Duration,
        cx: &mut Context<Self>,
    ) {
        let input_epoch = self.prompt_input_epoch;
        let executor = cx.background_executor().clone();
        self.auto_continue_task = Some(cx.spawn(async move |this, cx| {
            executor.timer(delay).await;
            let _ = this.update(cx, |view, cx| {
                let Some(settings) = cx.try_global::<Settings>() else { return };
                let Some(evidence) =
                    view.auto_continue.take_pending(settings.ai_auto_continue_attempts)
                else {
                    return;
                };
                if !settings.ai_auto_continue
                    || view.prompt_input_epoch != input_epoch
                    || view.exited.is_some()
                    || view.error.is_some()
                    || view.marked_text.is_some()
                    || view.pending_runtime_submit.is_some()
                    || view.running_program.as_deref() != Some(program.as_str())
                    || !view.ai_session.as_ref().is_some_and(|identity| {
                        identity.source == program && identity.session_id == session_id
                    })
                {
                    return;
                }
                let status = view.agent_activity.status();
                let Some(screen) = view.auto_continue_screen_snapshot() else { return };
                match evidence {
                    RetryEvidence::HookFailure
                        if !matches!(status, AgentStatus::Idle | AgentStatus::Failed)
                            || !view.last_command_failed
                            || !crate::ai_agents::auto_continue_screen_is_safe(
                                &program, &screen,
                            ) =>
                    {
                        return;
                    },
                    RetryEvidence::ScreenFailure
                        if matches!(status, AgentStatus::Blocked | AgentStatus::Unknown)
                            || !view.auto_continue_screen_can_retry(&program, &screen)
                            || view
                                .auto_continue_screen_error(&program, &screen, settings)
                                .is_none() =>
                    {
                        return;
                    },
                    _ => {},
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
                        return;
                    };
                    let mut bytes = b"continue".to_vec();
                    bytes.extend(enter);
                    bytes
                };
                let Some(session) = &view.session else { return };
                if session.notifier.0.send(Msg::Input(bytes.into())).is_err() {
                    return;
                }
                view.auto_continue.sent(Instant::now());
                view.agent_activity.submitted();
                view.last_command_failed = false;
                cx.emit(TerminalViewEvent::TitleChanged);
                cx.notify();
            });
        }));
    }

    fn auto_continue_screen_snapshot(&self) -> Option<String> {
        let session = self.session.as_ref()?;
        let term = session.term.lock();
        let lines = term.screen_lines();
        let columns = term.columns();
        if lines == 0 || columns == 0 {
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

    fn auto_continue_screen_can_retry(&self, program: &str, screen: &str) -> bool {
        let status = self.agent_activity.status();
        if matches!(status, AgentStatus::Blocked | AgentStatus::Unknown) {
            return false;
        }
        if status == AgentStatus::Working && crate::ai_agents::detect(program, screen).is_none() {
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
