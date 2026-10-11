//! Shared in-app notification lifetime, independent of delivery and approvals.

use std::time::Duration;

pub const DEFAULT_AUTO_CONTINUE_ERROR_PATTERNS: &str = "API Error: stream error: stream disconnected before completion;■ We’re currently experiencing high demand, which may cause temporary errors;■ We're currently experiencing high demand, which may cause temporary errors;■ Rate limit exceeded";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NotificationDuration {
    #[default]
    Default,
    FiveSeconds,
    TenSeconds,
    ThirtySeconds,
    NinetySeconds,
    Persistent,
}

impl NotificationDuration {
    pub const VALUES: &'static [&'static str] = &["default", "5", "10", "30", "90", "persistent"];

    pub fn from_settings(value: &str) -> Option<Self> {
        match value.trim() {
            value if value.eq_ignore_ascii_case("default") => Some(Self::Default),
            "5" => Some(Self::FiveSeconds),
            "10" => Some(Self::TenSeconds),
            "30" => Some(Self::ThirtySeconds),
            "90" => Some(Self::NinetySeconds),
            value if value.eq_ignore_ascii_case("persistent") => Some(Self::Persistent),
            _ => None,
        }
    }

    pub fn settings_value(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::FiveSeconds => "5",
            Self::TenSeconds => "10",
            Self::ThirtySeconds => "30",
            Self::NinetySeconds => "90",
            Self::Persistent => "persistent",
        }
    }

    /// Retain each caller's previous lifetime unless the user overrides it.
    /// `None` is an originally persistent notification, such as an update action.
    pub fn timeout(self, default: Option<Duration>) -> Option<Duration> {
        match self {
            Self::Default => default,
            Self::FiveSeconds => Some(Duration::from_secs(5)),
            Self::TenSeconds => Some(Duration::from_secs(10)),
            Self::ThirtySeconds => Some(Duration::from_secs(30)),
            Self::NinetySeconds => Some(Duration::from_secs(90)),
            Self::Persistent => None,
        }
    }
}

/// How many times the AI auto-continue may resend before giving up on a turn.
/// The default keeps the original bound; higher caps suit relays that only
/// recover after several attempts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AutoContinueAttempts {
    #[default]
    Three,
    Five,
    Ten,
    Twenty,
    Thirty,
}

impl AutoContinueAttempts {
    pub const VALUES: &'static [&'static str] = &["3", "5", "10", "20", "30"];

    pub fn from_settings(value: &str) -> Option<Self> {
        match value.trim() {
            "3" => Some(Self::Three),
            "5" => Some(Self::Five),
            "10" => Some(Self::Ten),
            "20" => Some(Self::Twenty),
            "30" => Some(Self::Thirty),
            _ => None,
        }
    }

    pub fn settings_value(self) -> &'static str {
        match self {
            Self::Three => "3",
            Self::Five => "5",
            Self::Ten => "10",
            Self::Twenty => "20",
            Self::Thirty => "30",
        }
    }

    /// Maximum number of auto-continue sends per failed turn.
    pub fn count(self) -> u8 {
        match self {
            Self::Three => 3,
            Self::Five => 5,
            Self::Ten => 10,
            Self::Twenty => 20,
            Self::Thirty => 30,
        }
    }
}

/// Minimum spacing between two consecutive AI auto-continue sends. Tuned for
/// waiting out relay rate limits; one minute is the shortest selectable choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AutoContinueInterval {
    OneMinute,
    #[default]
    ThreeMinutes,
    FiveMinutes,
    TenMinutes,
    FifteenMinutes,
    TwentyMinutes,
}

impl AutoContinueInterval {
    pub const VALUES: &'static [&'static str] = &["60", "180", "300", "600", "900", "1200"];

    pub fn from_settings(value: &str) -> Option<Self> {
        match value.trim() {
            "60" => Some(Self::OneMinute),
            "180" => Some(Self::ThreeMinutes),
            "300" => Some(Self::FiveMinutes),
            "600" => Some(Self::TenMinutes),
            "900" => Some(Self::FifteenMinutes),
            "1200" => Some(Self::TwentyMinutes),
            _ => None,
        }
    }

    pub fn settings_value(self) -> &'static str {
        match self {
            Self::OneMinute => "60",
            Self::ThreeMinutes => "180",
            Self::FiveMinutes => "300",
            Self::TenMinutes => "600",
            Self::FifteenMinutes => "900",
            Self::TwentyMinutes => "1200",
        }
    }

    /// Wait enforced before each auto-continue send.
    pub fn interval(self) -> Duration {
        match self {
            Self::OneMinute => Duration::from_secs(60),
            Self::ThreeMinutes => Duration::from_secs(180),
            Self::FiveMinutes => Duration::from_secs(300),
            Self::TenMinutes => Duration::from_secs(600),
            Self::FifteenMinutes => Duration::from_secs(900),
            Self::TwentyMinutes => Duration::from_secs(1200),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RawSettings, RuntimeSettings, apply_updates};

    #[test]
    fn missing_or_invalid_duration_preserves_each_notifications_existing_lifetime() {
        assert_eq!(
            RuntimeSettings::from_raw(&RawSettings::from_text("")).notification_duration,
            NotificationDuration::Default
        );
        for value in ["", "invalid", "-1", "0", "1", "999999999999999999999"] {
            let settings = RuntimeSettings::from_raw(&RawSettings::from_text(&format!(
                "notification_duration={value}\n"
            )));
            assert_eq!(settings.notification_duration, NotificationDuration::Default);
            for default in [Some(Duration::from_secs(5)), Some(Duration::from_secs(90)), None] {
                assert_eq!(settings.notification_duration.timeout(default), default);
            }
        }
    }

    #[test]
    fn every_duration_round_trips_without_changing_the_delivery_switch_or_other_data() {
        for value in NotificationDuration::VALUES {
            let saved = apply_updates(
                "ai_toasts=0\ncustom_data=keep\n",
                &[("notification_duration", (*value).to_owned())],
            );
            let settings = RuntimeSettings::from_raw(&RawSettings::from_text(&saved));
            assert_eq!(settings.notification_duration.settings_value(), *value);
            assert!(!settings.ai_toasts);
            assert!(saved.contains("custom_data=keep"));
        }
    }

    #[test]
    fn timed_and_persistent_modes_have_explicit_lifetimes() {
        for (value, seconds) in [("5", 5), ("10", 10), ("30", 30), ("90", 90)] {
            assert_eq!(
                NotificationDuration::from_settings(value).unwrap().timeout(None),
                Some(Duration::from_secs(seconds))
            );
        }
        assert_eq!(
            NotificationDuration::from_settings(" PERSISTENT ")
                .unwrap()
                .timeout(Some(Duration::from_secs(5))),
            None
        );
        assert_eq!(
            NotificationDuration::from_settings(" DEFAULT "),
            Some(NotificationDuration::Default)
        );
    }
}

#[cfg(test)]
mod auto_continue_tests {
    use crate::{
        DEFAULT_AUTO_CONTINUE_ERROR_PATTERNS, RawSettings, RuntimeSettings, apply_updates,
    };
    use std::time::Duration;

    #[test]
    fn auto_continue_defaults_and_roundtrips_without_erasing_other_settings() {
        assert!(RuntimeSettings::from_raw(&RawSettings::default()).ai_auto_continue);
        for value in ["0", "false", "off", "no"] {
            let saved = apply_updates(
                "custom_data=keep\nai_toasts=0\n",
                &[("ai_auto_continue", value.to_owned())],
            );
            let runtime = RuntimeSettings::from_raw(&RawSettings::from_text(&saved));
            assert!(!runtime.ai_auto_continue, "{value}");
            assert!(!runtime.ai_toasts);
            assert!(saved.contains("custom_data=keep"));
        }
        for value in ["1", "true", "on", "yes"] {
            let raw = RawSettings::from_text(&format!("ai_auto_continue={value}\n"));
            assert!(RuntimeSettings::from_raw(&raw).ai_auto_continue);
        }
    }

    #[test]
    fn auto_continue_count_interval_and_error_patterns_round_trip() {
        use crate::{AutoContinueAttempts, AutoContinueInterval};

        let defaults = RuntimeSettings::from_raw(&RawSettings::default());
        assert_eq!(defaults.ai_auto_continue_attempts, AutoContinueAttempts::Three);
        assert_eq!(defaults.ai_auto_continue_attempts.count(), 3);
        assert_eq!(defaults.ai_auto_continue_interval, AutoContinueInterval::ThreeMinutes);
        assert_eq!(defaults.ai_auto_continue_interval.interval(), Duration::from_secs(180));
        assert_eq!(defaults.ai_auto_continue_error_patterns, DEFAULT_AUTO_CONTINUE_ERROR_PATTERNS);

        for value in AutoContinueInterval::VALUES {
            assert_eq!(
                AutoContinueInterval::from_settings(value).unwrap().settings_value(),
                *value
            );
        }
        assert_eq!(
            AutoContinueInterval::from_settings("60").unwrap().interval(),
            Duration::from_secs(60)
        );
        assert_eq!(AutoContinueInterval::from_settings("45"), None);

        let phrases = "stream closed before response.completed;temporary overload";
        let saved = apply_updates(
            "custom_data=keep\n",
            &[
                ("ai_auto_continue_attempts", "10".to_owned()),
                ("ai_auto_continue_interval", "60".to_owned()),
                ("ai_auto_continue_error_patterns", phrases.to_owned()),
            ],
        );
        let runtime = RuntimeSettings::from_raw(&RawSettings::from_text(&saved));
        assert_eq!(runtime.ai_auto_continue_attempts.count(), 10);
        assert_eq!(runtime.ai_auto_continue_interval, AutoContinueInterval::OneMinute);
        assert_eq!(runtime.ai_auto_continue_interval.interval(), Duration::from_secs(60));
        assert_eq!(runtime.ai_auto_continue_error_patterns, phrases);
        assert!(saved.contains("custom_data=keep"));

        let cleared = apply_updates(&saved, &[("ai_auto_continue_error_patterns", String::new())]);
        assert!(
            RuntimeSettings::from_raw(&RawSettings::from_text(&cleared))
                .ai_auto_continue_error_patterns
                .is_empty()
        );

        let bad =
            RawSettings::from_text("ai_auto_continue_attempts=7\nai_auto_continue_interval=45\n");
        let runtime = RuntimeSettings::from_raw(&bad);
        assert_eq!(runtime.ai_auto_continue_attempts, AutoContinueAttempts::Three);
        assert_eq!(runtime.ai_auto_continue_interval, AutoContinueInterval::ThreeMinutes);
    }
}
