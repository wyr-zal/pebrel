//! Retry policy for accepted lifecycle facts and bounded screen evidence.
use std::time::{Duration, Instant};

use super::{AiHookEvent, AiHookKind, AiTurnOutcome};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RetryEvidence {
    HookFailure,
    ScreenFailure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScreenRetryReason {
    UnsupportedProvider,
    NoError,
    Disabled,
    MissingSession,
    PromptNotArmed,
    InactiveTurn,
    AttemptLimit,
    RetryPending,
    ExistingError,
    UnsafeScreen,
    Scheduled,
}

impl ScreenRetryReason {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::UnsupportedProvider => "unsupported_provider",
            Self::NoError => "no_match",
            Self::Disabled => "disabled",
            Self::MissingSession => "missing_session",
            Self::PromptNotArmed => "prompt_not_armed",
            Self::InactiveTurn => "inactive_turn",
            Self::AttemptLimit => "attempt_limit",
            Self::RetryPending => "retry_pending",
            Self::ExistingError => "existing_error",
            Self::UnsafeScreen => "unsafe_screen",
            Self::Scheduled => "scheduled",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScreenRetryObservation {
    pub(crate) delay: Option<Duration>,
    pub(crate) reason: ScreenRetryReason,
    pub(crate) should_log: bool,
}

#[derive(Default)]
pub(crate) struct AutoContinue {
    attempts: u8,
    last_sent: Option<Instant>,
    armed: bool,
    screen_turn_active: bool,
    screen_failure_latched: Option<String>,
    last_screen_retry_log: Option<(u64, ScreenRetryReason)>,
    pending: Option<RetryEvidence>,
}

impl AutoContinue {
    pub fn cancel(&mut self) {
        self.armed = false;
        self.screen_turn_active = false;
        self.pending = None;
    }

    pub fn reset(&mut self) {
        self.cancel();
        self.attempts = 0;
        self.screen_failure_latched = None;
        self.last_screen_retry_log = None;
    }

    pub fn pending(&self) -> Option<RetryEvidence> {
        self.pending
    }

    pub fn pending_screen_failure(&self) -> Option<&str> {
        (self.pending == Some(RetryEvidence::ScreenFailure))
            .then_some(self.screen_failure_latched.as_deref())
            .flatten()
    }

    pub fn take_pending(&mut self, max_attempts: u8) -> Option<RetryEvidence> {
        self.pending.take().filter(|_| self.attempts < max_attempts)
    }

    /// A newly accepted prompt cannot authorize an error already on its screen.
    pub fn remember_screen_at_prompt(&mut self, error: Option<String>) {
        self.screen_failure_latched = error;
        self.last_screen_retry_log = None;
    }

    /// Both evidence sources share one pending retry and never restart its deadline.
    pub fn observe(
        &mut self,
        event: &AiHookEvent,
        now: Instant,
        max_attempts: u8,
        interval: Duration,
    ) -> Option<Duration> {
        if !matches!(event.source.as_str(), "claude" | "codex") {
            self.cancel();
            return None;
        }
        match event.kind {
            AiHookKind::SessionStart if !event.session_compacted => {
                self.reset();
                self.armed = true;
            },
            AiHookKind::SessionEnd => self.reset(),
            AiHookKind::PromptSubmit => {
                self.pending = None;
                self.armed = true;
                self.screen_turn_active = true;
            },
            AiHookKind::TurnDone if event.active_background_tasks() != 0 => {
                if event.turn_outcome == AiTurnOutcome::Succeeded {
                    self.attempts = 0;
                }
                self.cancel();
            },
            AiHookKind::TurnDone if event.source == "codex" => match event.turn_outcome {
                AiTurnOutcome::Succeeded => {
                    self.attempts = 0;
                    self.cancel();
                },
                AiTurnOutcome::Cancelled => self.cancel(),
                _ => {},
            },
            AiHookKind::TurnDone => {
                if event.turn_outcome == AiTurnOutcome::Succeeded {
                    self.attempts = 0;
                }
                if event.turn_outcome != AiTurnOutcome::Failed
                    || event.session_id.is_none()
                    || !matches!(
                        event.message.as_deref(),
                        Some("server_error" | "overloaded" | "rate_limit" | "unknown")
                    )
                {
                    self.cancel();
                    return None;
                }
                self.screen_turn_active = false;
                if self.pending.is_some() {
                    // Upgrade screen evidence without replacing the pane's timer.
                    self.armed = false;
                    self.pending = Some(RetryEvidence::HookFailure);
                    return None;
                }
                let armed = std::mem::replace(&mut self.armed, false);
                if !armed || self.attempts >= max_attempts {
                    return None;
                }
                self.pending = Some(RetryEvidence::HookFailure);
                return Some(self.next_delay(now, interval));
            },
            _ => self.pending = None,
        }
        None
    }

    pub fn observe_screen_failure(
        &mut self,
        program: &str,
        session_id: Option<&str>,
        failure: Option<&str>,
        eligible: bool,
        now: Instant,
        max_attempts: u8,
        interval: Duration,
    ) -> Option<Duration> {
        self.observe_screen_failure_diagnostic(
            program,
            session_id,
            failure,
            eligible,
            true,
            now,
            max_attempts,
            interval,
        )
        .delay
    }

    pub fn observe_screen_failure_diagnostic(
        &mut self,
        program: &str,
        session_id: Option<&str>,
        failure: Option<&str>,
        eligible: bool,
        enabled: bool,
        now: Instant,
        max_attempts: u8,
        interval: Duration,
    ) -> ScreenRetryObservation {
        if !matches!(program, "claude" | "codex") {
            return ScreenRetryObservation {
                delay: None,
                reason: ScreenRetryReason::UnsupportedProvider,
                should_log: false,
            };
        }
        let Some(failure) = failure else {
            self.screen_failure_latched = None;
            self.last_screen_retry_log = None;
            if self.pending == Some(RetryEvidence::ScreenFailure) {
                self.pending = None;
            }
            return ScreenRetryObservation {
                delay: None,
                reason: ScreenRetryReason::NoError,
                should_log: false,
            };
        };
        let reason = if !enabled {
            ScreenRetryReason::Disabled
        } else if session_id.is_none() {
            ScreenRetryReason::MissingSession
        } else if !self.armed {
            ScreenRetryReason::PromptNotArmed
        } else if !self.screen_turn_active {
            ScreenRetryReason::InactiveTurn
        } else if self.attempts >= max_attempts {
            ScreenRetryReason::AttemptLimit
        } else if self.pending.is_some() {
            ScreenRetryReason::RetryPending
        } else if self.screen_failure_latched.as_deref() == Some(failure) {
            ScreenRetryReason::ExistingError
        } else if !eligible {
            ScreenRetryReason::UnsafeScreen
        } else {
            ScreenRetryReason::Scheduled
        };
        let delay = if reason == ScreenRetryReason::Scheduled {
            // A revocable screen candidate has not consumed the accepted Hook opportunity.
            self.screen_failure_latched = Some(failure.to_owned());
            self.pending = Some(RetryEvidence::ScreenFailure);
            Some(self.next_delay(now, interval))
        } else {
            None
        };
        let should_log = self.should_log_screen_retry(failure, reason);
        ScreenRetryObservation { delay, reason, should_log }
    }

    fn should_log_screen_retry(&mut self, failure: &str, reason: ScreenRetryReason) -> bool {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        failure.hash(&mut hasher);
        let key = (hasher.finish(), reason);
        if self.last_screen_retry_log == Some(key) {
            return false;
        }
        self.last_screen_retry_log = Some(key);
        true
    }

    fn next_delay(&self, now: Instant, interval: Duration) -> Duration {
        self.last_sent
            .map_or(interval, |last| interval.saturating_sub(now.saturating_duration_since(last)))
    }

    pub fn sent(&mut self, now: Instant) {
        self.armed = false;
        self.pending = None;
        self.attempts += 1;
        self.last_sent = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(name: &str, error: &str) -> AiHookEvent {
        super::super::parse_remote_envelope(
            format!("nebula-hook/1 source=claude\n{{\"hook_event_name\":\"{name}\",\"session_id\":\"retry\",\"error\":\"{error}\"}}").as_bytes(), Some(42),
        ).unwrap()
    }

    #[test]
    fn screen_retry_diagnostics_are_reason_only_and_deduplicated() {
        let now = Instant::now();
        let interval = Duration::from_secs(60);
        let mut state = AutoContinue::default();
        let rejected = state.observe_screen_failure_diagnostic(
            "codex",
            Some("session"),
            Some("high-demand error identity"),
            true,
            true,
            now,
            3,
            interval,
        );
        assert_eq!(rejected.reason, ScreenRetryReason::PromptNotArmed);
        assert_eq!(rejected.reason.code(), "prompt_not_armed");
        assert!(rejected.should_log);
        assert!(!state
            .observe_screen_failure_diagnostic(
                "codex",
                Some("session"),
                Some("high-demand error identity"),
                true,
                true,
                now,
                3,
                interval,
            )
            .should_log);

        let scheduled = state
            .observe(&event("SessionStart", ""), now, 3, interval);
        assert_eq!(scheduled, None);
        state.observe(&event("UserPromptSubmit", ""), now, 3, interval);
        let scheduled = state.observe_screen_failure_diagnostic(
            "codex",
            Some("session"),
            Some("high-demand error identity"),
            true,
            true,
            now,
            3,
            interval,
        );
        assert_eq!(scheduled.reason, ScreenRetryReason::Scheduled);
        assert_eq!(scheduled.delay, Some(interval));
        assert!(scheduled.should_log);

        let pending = state.observe_screen_failure_diagnostic(
            "codex",
            Some("session"),
            Some("high-demand error identity"),
            true,
            true,
            now,
            3,
            interval,
        );
        assert_eq!(pending.reason, ScreenRetryReason::RetryPending);
        assert!(pending.should_log);
        assert!(!state
            .observe_screen_failure_diagnostic(
                "codex",
                Some("session"),
                Some("high-demand error identity"),
                true,
                true,
                now,
                3,
                interval,
            )
            .should_log);

        let cleared = state.observe_screen_failure_diagnostic(
            "codex", Some("session"), None, true, true, now, 3, interval,
        );
        assert_eq!(cleared.reason, ScreenRetryReason::NoError);
        assert!(!cleared.should_log);
    }

    #[test]
    fn retries_are_bounded_spaced_and_success_resets_the_chain() {
        let mut state = AutoContinue::default();
        let now = Instant::now();
        let max = 3;
        let interval = Duration::from_secs(180);
        for _ in 0..max {
            state.observe(&event("UserPromptSubmit", ""), now, max, interval);
            let delay =
                state.observe(&event("StopFailure", "unknown"), now, max, interval).unwrap();
            assert_eq!(delay, interval);
            assert!(state.observe(&event("StopFailure", "unknown"), now, max, interval).is_none());
            state.sent(now);
        }
        state.observe(&event("UserPromptSubmit", ""), now, max, interval);
        assert!(state.observe(&event("StopFailure", "rate_limit"), now, max, interval).is_none());
        state.observe(&event("Stop", ""), now, max, interval);
        state.observe(&event("UserPromptSubmit", ""), now, max, interval);
        assert_eq!(
            state.observe(&event("StopFailure", "rate_limit"), now, max, interval),
            Some(interval)
        );
    }

    #[test]
    fn configured_bound_and_interval_drive_the_schedule() {
        let mut state = AutoContinue::default();
        let now = Instant::now();
        let interval = Duration::from_secs(600);
        // A five-attempt budget keeps scheduling past the old hard-coded three.
        for _ in 0..5 {
            state.observe(&event("UserPromptSubmit", ""), now, 5, interval);
            assert_eq!(
                state.observe(&event("StopFailure", "unknown"), now, 5, interval),
                Some(interval)
            );
            state.sent(now);
        }
        state.observe(&event("UserPromptSubmit", ""), now, 5, interval);
        assert!(state.observe(&event("StopFailure", "unknown"), now, 5, interval).is_none());
        // Once the interval has fully elapsed, the next send is due immediately.
        let mut fresh = AutoContinue::default();
        fresh.observe(&event("UserPromptSubmit", ""), now, 3, interval);
        assert_eq!(
            fresh.observe(&event("StopFailure", "unknown"), now, 3, interval),
            Some(interval)
        );
        fresh.sent(now);
        fresh.observe(&event("UserPromptSubmit", ""), now, 3, interval);
        assert_eq!(
            fresh.observe(&event("StopFailure", "unknown"), now + interval, 3, interval),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn codex_screen_and_claude_hook_evidence_share_one_budget_and_latch() {
        let now = Instant::now();
        let interval = Duration::from_secs(60);
        let mut state = AutoContinue::default();
        let mut codex_start = event("SessionStart", "");
        codex_start.source = "codex".into();
        state.observe(&codex_start, now, 3, interval);
        let mut codex_prompt = event("UserPromptSubmit", "");
        codex_prompt.source = "codex".into();
        state.observe(&codex_prompt, now, 3, interval);
        let mut codex_stop = event("Stop", "");
        codex_stop.source = "codex".into();
        codex_stop.turn_outcome = AiTurnOutcome::Unknown;
        state.observe(&codex_stop, now, 3, interval);

        assert_eq!(
            state.observe_screen_failure(
                "codex",
                Some("codex-session"),
                Some("failure"),
                true,
                now,
                3,
                interval
            ),
            Some(interval)
        );
        assert!(
            state
                .observe_screen_failure(
                    "codex",
                    Some("codex-session"),
                    Some("failure"),
                    true,
                    now,
                    3,
                    interval
                )
                .is_none()
        );
        state.sent(now);
        state.observe_screen_failure("codex", Some("codex-session"), None, true, now, 3, interval);
        state.observe(&codex_prompt, now, 3, interval);
        assert_eq!(
            state.observe_screen_failure(
                "codex",
                Some("codex-session"),
                Some("failure"),
                true,
                now,
                3,
                interval
            ),
            Some(interval)
        );

        let mut claude = AutoContinue::default();
        claude.observe(&event("SessionStart", ""), now, 3, interval);
        claude.observe(&event("UserPromptSubmit", ""), now, 3, interval);
        assert_eq!(
            claude.observe(&event("StopFailure", "unknown"), now, 3, interval),
            Some(interval)
        );
        assert!(
            claude
                .observe_screen_failure(
                    "claude",
                    Some("retry"),
                    Some("failure"),
                    true,
                    now,
                    3,
                    interval
                )
                .is_none()
        );
    }

    #[test]
    fn unsafe_frames_preserve_old_identity_but_do_not_consume_a_new_failure() {
        let now = Instant::now();
        let interval = Duration::from_secs(60);
        let mut state = AutoContinue::default();
        state.remember_screen_at_prompt(Some("old failure".into()));
        state.observe(&event("UserPromptSubmit", ""), now, 3, interval);
        for eligible in [false, true] {
            assert!(
                state
                    .observe_screen_failure(
                        "claude",
                        Some("retry"),
                        Some("old failure"),
                        eligible,
                        now,
                        3,
                        interval
                    )
                    .is_none()
            );
        }
        assert!(
            state
                .observe_screen_failure(
                    "claude",
                    Some("retry"),
                    Some("new failure"),
                    false,
                    now,
                    3,
                    interval
                )
                .is_none()
        );
        assert_eq!(
            state.observe_screen_failure(
                "claude",
                Some("retry"),
                Some("new failure"),
                true,
                now,
                3,
                interval
            ),
            Some(interval)
        );
        state.sent(now);
        assert!(
            state.observe(&event("StopFailure", "unknown"), now, 3, interval).is_none(),
            "a late Hook after actual dispatch must not send twice"
        );
    }

    #[test]
    fn background_work_cancels_pending_screen_retry_without_spending_an_attempt() {
        let now = Instant::now();
        let interval = Duration::from_secs(60);
        let mut state = AutoContinue::default();
        state.observe(&event("UserPromptSubmit", ""), now, 3, interval);
        assert!(
            state
                .observe_screen_failure(
                    "claude",
                    Some("retry"),
                    Some("failure"),
                    true,
                    now,
                    3,
                    interval
                )
                .is_some()
        );
        let mut failure = event("StopFailure", "unknown");
        failure.background_tasks = Some(super::super::AiBackgroundTasks { active: 1, total: 1 });
        assert!(state.observe(&failure, now, 3, interval).is_none());
        assert!(state.pending().is_none());
        assert!(
            state
                .observe_screen_failure(
                    "claude",
                    Some("retry"),
                    Some("new failure"),
                    true,
                    now,
                    3,
                    interval
                )
                .is_none()
        );
        assert_eq!(state.attempts, 0);
    }

    #[test]
    fn non_retryable_claude_hook_failure_blocks_screen_override_until_new_prompt() {
        let now = Instant::now();
        let interval = Duration::from_secs(60);
        let mut state = AutoContinue::default();
        state.observe(&event("SessionStart", ""), now, 3, interval);
        state.observe(&event("UserPromptSubmit", ""), now, 3, interval);
        assert!(state.observe(&event("StopFailure", "billing_error"), now, 3, interval).is_none());
        state.observe_screen_failure("claude", Some("retry"), None, true, now, 3, interval);
        assert!(
            state
                .observe_screen_failure(
                    "claude",
                    Some("retry"),
                    Some("failure"),
                    true,
                    now,
                    3,
                    interval
                )
                .is_none()
        );
        state.observe(&event("UserPromptSubmit", ""), now, 3, interval);
        state.observe_screen_failure("claude", Some("retry"), None, true, now, 3, interval);
        assert_eq!(
            state.observe_screen_failure(
                "claude",
                Some("retry"),
                Some("failure"),
                true,
                now,
                3,
                interval
            ),
            Some(interval)
        );
    }

    #[test]
    fn claude_prompt_id_rejects_late_failures_from_an_earlier_turn() {
        let mut activity = super::super::lifecycle::AgentActivity::default();
        let parse = |name: &str, prompt: &str| {
            super::super::parse_remote_envelope(
                format!("nebula-hook/1 source=claude\n{{\"hook_event_name\":\"{name}\",\"session_id\":\"prompt-scope\",\"prompt_id\":\"{prompt}\"}}").as_bytes(), Some(42),
            ).unwrap()
        };
        let first = parse("UserPromptSubmit", "first");
        assert_eq!(first.turn_id.as_deref(), Some("first"));
        activity.apply_hook(&first);
        activity.apply_hook(&parse("UserPromptSubmit", "second"));
        assert!(!activity.accepts_hook(&parse("StopFailure", "first")));
        assert!(activity.accepts_hook(&parse("StopFailure", "second")));
        activity.apply_hook(&parse("StopFailure", "second"));
        assert!(!activity.accepts_hook(&parse("StopFailure", "second")));
        assert!(activity.accepts_hook(&parse("SessionEnd", "exit-command")));
    }

    #[test]
    fn only_explicit_retryable_main_turn_errors_are_eligible() {
        let now = Instant::now();
        let interval = Duration::from_secs(180);
        for error in [
            "server_error",
            "overloaded",
            "rate_limit",
            "unknown",
            "authentication_failed",
            "billing_error",
            "",
            "some unknown text",
        ] {
            let mut state = AutoContinue::default();
            state.observe(&event("SessionStart", ""), now, 3, interval);
            assert_eq!(
                state.observe(&event("StopFailure", error), now, 3, interval).is_some(),
                matches!(error, "server_error" | "overloaded" | "rate_limit" | "unknown")
            );
        }
        let mut state = AutoContinue::default();
        state.observe(&event("SessionStart", ""), now, 3, interval);
        state.cancel();
        assert!(state.observe(&event("StopFailure", "unknown"), now, 3, interval).is_none());
        state.observe(&event("UserPromptSubmit", ""), now, 3, interval);
        let mut failure = event("StopFailure", "unknown");
        failure.source = "codex".into();
        assert!(state.observe(&failure, now, 3, interval).is_none());
        state.observe(&event("UserPromptSubmit", ""), now, 3, interval);
        failure.source = "claude".into();
        failure.background_tasks = Some(super::super::AiBackgroundTasks { active: 1, total: 1 });
        assert!(state.observe(&failure, now, 3, interval).is_none());
    }
}
