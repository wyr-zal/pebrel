//! Structural limits shared by every bundled and user-supplied screen rule.
//! Words in assistant prose are never sufficient evidence of a live input form.

use regex::Regex;
use std::sync::LazyLock;

use super::{AgentKind, from_last_empty_prompt};

static CONTROLS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
    r"(?i)^(?:(?:press|use)\s+)?(?:esc(?:ape)?|enter|tab|ctrl[+\-]\w|↑/?↓|↑↓|↵|[yn])(?:\s|:|\().*(?:cancel|confirm|submit|select|choose|navigate|amend|allow|deny|reject|skip|close|interrupt|stop)"
).unwrap()
});
static BINARY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
    r"(?i)^[\p{L}][^\n]*[?:]\s*(?:\(\s*y(?:es)?\s*/\s*n(?:o)?\s*\)|\[\s*y(?:es)?\s*/\s*n(?:o)?\s*\])\s*[:?]?\s*$"
).unwrap()
});

pub(super) fn attention_region(agent: AgentKind, screen: &str) -> &str {
    let screen = from_last_empty_prompt(screen);
    if agent != AgentKind::Codex {
        return screen;
    }
    // Codex's unboxed composer contains a draft or rotating placeholder. Its
    // numbered selection rows use the same glyph but are part of a form.
    let mut start = 0;
    let mut offset = 0;
    for line in screen.split_inclusive('\n') {
        if let Some(rest) = line.trim().strip_prefix(['›', '»'])
            && rest.starts_with(char::is_whitespace)
            && !rest.trim_start().starts_with(|c: char| c.is_ascii_digit() || c == '[')
        {
            start = offset;
        }
        offset += line.len();
    }
    &screen[start..]
}

static CODEX_FOOTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?s)^[^\r\n]+(?:\r?\n[^\r\n]+)?[ \t]+(?:default|minimal|low|medium|high|xhigh|max|ultra)[ \t]*[·•][^\r\n]*(?:\r?\n[ \t]*Context [^\r\n]+)?(?:\r?\n[ \t]*\?[ \t]+for shortcuts[^\r\n]*)?\s*\z",
    )
    .unwrap()
});

/// A composer is a frame boundary, not evidence that an assistant has stopped.
/// Only the adjacent activity block may supply busy/error observations.
pub(super) fn codex_activity_region(screen: &str) -> Option<&str> {
    let mut composer = None;
    let mut offset = 0;
    for line in screen.split_inclusive('\n') {
        if let Some(rest) = line.trim().strip_prefix(['›', '»'])
            && (rest.is_empty() || rest.starts_with(char::is_whitespace))
            && !rest.trim_start().starts_with(|c: char| c.is_ascii_digit() || c == '[')
        {
            composer = Some((offset, offset + line.len()));
        }
        offset += line.len();
    }
    let (start, end) = composer?;
    let footer = screen[end..].trim();
    if footer.len() > 2048 || !CODEX_FOOTER.is_match(footer) {
        return None;
    }
    let above = screen[..start].trim_end();
    let mut end = above.len();
    for row in above.split_inclusive('\n').rev().take(16) {
        end -= row.len();
        let row = row.trim();
        if row.starts_with(['•', '◦', '■']) {
            return Some(&above[end..]);
        }
        if !row.starts_with(['└', '│', '↳']) {
            break;
        }
    }
    Some("")
}

/// Only the final activity block above the current boxed Claude composer.
/// Drafts, old prompt frames and quoted status rows are not activity chrome.
pub(super) fn claude_activity_region(screen: &str) -> &str {
    let horizontal = |line: &str| {
        let row = line.trim();
        row.chars().count() >= 3 && row.chars().all(|c| c == '─')
    };
    let (mut offset, mut previous_start, mut previous_rule, mut end) = (0, 0, false, None);
    for line in screen.split_inclusive('\n') {
        if previous_rule
            && line
                .trim_start()
                .strip_prefix('❯')
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
            && screen[offset + line.len()..].lines().any(horizontal)
        {
            end = Some(previous_start);
        }
        previous_rule = horizontal(line);
        previous_start = offset;
        offset += line.len();
    }
    let Some(end) = end else { return "" };
    let activity = screen[..end].trim_end();
    let (mut offset, mut start) = (0, 0);
    for line in activity.split_inclusive('\n') {
        offset += line.len();
        if line.trim().is_empty() {
            start = offset;
        }
    }
    &activity[start..]
}

fn content_row(row: &str) -> &str {
    row.trim().trim_matches(['│', '┃']).trim()
}

pub(super) fn has_live_input_controls(screen: &str) -> bool {
    // The latest control footer wins. A quoted old form above an interrupt
    // footer or a new composer cannot turn current work into an input request.
    for row in screen
        .lines()
        .rev()
        .map(content_row)
        .filter(|row| !row.is_empty() && !row.chars().all(|c| "─━╰╯└┘+".contains(c)))
        .take(3)
    {
        if BINARY.is_match(row) {
            return true;
        }
        if CONTROLS.is_match(row) {
            let lower = row.to_ascii_lowercase();
            return !lower.contains("interrupt") && !lower.contains("to stop");
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use crate::ai_agents::{AgentKind, AgentStatus, detect};

    #[test]
    fn codex_default_effort_chrome_can_wake_a_probe_without_proving_idle() {
        let footer = "› Ask Codex to do anything\n\n  gpt-5.4 default · /work/project  ⚠ 4 warnings · f2 to view";
        assert!(crate::ai_agents::codex_idle_chrome_complete(footer));
        assert!(detect("codex", footer).is_none());
        let working = format!("• Working (12s • esc to interrupt)\n\n{footer}");
        assert_eq!(detect("codex", &working).unwrap().status, AgentStatus::Working);
        let quoted = format!("```text\n{working}\n```\nuser@host:~$ ");
        assert!(!crate::ai_agents::codex_idle_chrome_complete(&quoted));
    }

    #[test]
    fn codex_composer_alone_has_no_idle_authority() {
        for prompt in ["›", "»"] {
            for text in ["", "Ask Codex to do anything", "Review my changes"] {
                let frame = format!(
                    "{prompt} {text}\nGPT-6 Astra · Agent1 ultra · Context 30% used\n? for shortcuts"
                );
                assert!(
                    detect("codex", &frame).is_none(),
                    "an input composer is present during work too: {frame}"
                );
            }
        }
    }

    #[test]
    fn codex_status_requires_the_current_activity_block() {
        for prompt in ["›", "»"] {
            let footer = format!(
                "{prompt} Ask Codex to do anything\nGPT-6 Astra · Agent1 max · Context 30% used\n? for shortcuts"
            );
            let working = "• Working (2m 15s • esc to interrupt)";
            let frame = format!("{working}\n  └ Reading files\n\n{footer}");
            assert_eq!(detect("codex", &frame).unwrap().status, AgentStatus::Working);
            for stale in [
                format!("```text\n{working}\n```\n{footer}"),
                format!("{working}\nThe answer is finished.\n{footer}"),
                format!("{working}\n› old draft\ngpt-6 high · /old\n\n{footer}"),
                format!("{frame}\nuser@host:~$ "),
                working.to_owned(),
            ] {
                assert!(
                    !detect("codex", &stale)
                        .is_some_and(|detection| detection.status == AgentStatus::Working),
                    "old, partial, or quoted activity is not current work: {stale}"
                );
            }
        }
    }

    #[test]
    fn codex_alternate_prompt_retires_old_approval_controls() {
        let frame = "Allow command?\n› 1. Yes\n2. No\nPress enter to confirm or esc to cancel\n» Ask Codex to do anything\nGPT-6 Astra · Agent1 ultra · Context 30% used\n? for shortcuts";
        assert!(detect("codex", frame).is_none());
        assert_eq!(
            super::attention_region(AgentKind::Codex, frame),
            "» Ask Codex to do anything\nGPT-6 Astra · Agent1 ultra · Context 30% used\n? for shortcuts"
        );
    }

    #[test]
    fn all_agents_reject_quoted_keywords_and_expired_forms() {
        for agent in AgentKind::ALL {
            for phrase in [
                "{ contains = [\"(y/n)\"] },\n{ contains = [\"[y/n]\"] },",
                "The tool requires approval. Use enter to confirm or tab to amend.",
                "waiting for permission\nDo you want to proceed?",
                "Permission required\nPress enter to confirm or esc to cancel",
            ] {
                for footer in ["? for shortcuts", "esc to interrupt"] {
                    let screen = format!("{phrase}\n────────\n>\n────────\n{footer}");
                    assert!(
                        !detect(agent.slug(), &screen)
                            .is_some_and(|d| d.status == AgentStatus::Blocked),
                        "{agent:?}: {screen}"
                    );
                }
            }
        }
    }

    #[test]
    fn codex_source_listing_and_idle_answer_do_not_become_questions() {
        let listing = "  { contains = [\"(y/n)\"] },\n  { contains = [\"[y/n]\"] },\n]\n\n◦ Working (12m 36s • esc to interrupt)\n\n› Ask Codex to do anything\ngpt-6 high · /project";
        assert_eq!(detect("codex", listing).unwrap().status, AgentStatus::Working);
        let old_form = "Allow command?\n› 1. Yes\n2. No\nPress enter to confirm or esc to cancel\n› Improve documentation\ngpt-6 high · /project";
        assert!(detect("codex", old_form).is_none());
        assert!(crate::ai_agents::codex_idle_chrome_complete(old_form));
    }

    #[test]
    fn real_current_forms_still_require_attention() {
        for (agent, form) in [
            ("codex", "Allow command?\n› 1. Yes\n2. No\nPress enter to confirm or esc to cancel"),
            ("claude", "Do you want to proceed?\n❯ 1. Yes\n2. No\nEsc to cancel · Tab to amend"),
            ("pi", "Continue? [y/n]"),
            ("gemini", "Allow execution?\n1. Yes\n2. No\nEnter to confirm · Esc to cancel"),
        ] {
            assert_eq!(
                detect(agent, form).unwrap().status,
                AgentStatus::Blocked,
                "{agent}: {form}"
            );
        }
        for text in [
            "Use [y/n] in your code",
            "contains = ['[y/n]']",
            "Explain (y/n)",
            "Do you want to proceed?",
        ] {
            assert!(
                !detect("codex", text).is_some_and(|d| d.status == AgentStatus::Blocked),
                "{text}"
            );
        }
    }
}
