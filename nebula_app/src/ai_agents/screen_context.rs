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

static CLAUDE_HISTORY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:jump\s+to\s+bottom|[1-9][0-9]*\s+new\s+messages?)\s*\(ctrl\s*\+\s*end\)\s*↓?$",
    )
    .unwrap()
});

static CODEX_HISTORY_BANNER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:↓\s*)?back\s+to\s+bottom\s*[·•]\s*esc(?:ape)?$").unwrap()
});
static CODEX_HISTORY_FOOTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^earlier\s+messages\s+available\.\s+enter\s*/\s*esc\s+latest(?:\s*[·•]\s*\?\s+(?:for\s+)?shortcuts)?(?:\s+⚠\s*\d+\s+warnings?\s*[·•]\s*f2\s+to\s+view)?$",
    )
    .unwrap()
});

pub(super) fn is_horizontal_rule(line: &str) -> bool {
    let row = line.trim();
    if row.chars().filter(|character| *character == '─').count() < 3 {
        return false;
    }
    if row.chars().all(|character| character == '─') {
        return true;
    }
    let label = row.trim_matches('─').trim();
    row.starts_with('─')
        && row.ends_with('─')
        && !label.is_empty()
        && !label.contains('─')
        && label.chars().count() <= 64
}

fn is_prompt_row(line: &str) -> bool {
    let row = line.trim_start().trim_start_matches(['│', '┃']).trim_start();
    row.strip_prefix('❯')
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
}

fn claude_composer_bounds(screen: &str) -> Option<(usize, usize)> {
    const MAX_COMPOSER_LINES: usize = 32;

    let mut lines = Vec::new();
    let mut offset = 0;
    for line in screen.split_inclusive('\n') {
        lines.push((line.trim(), offset, offset + line.len()));
        offset += line.len();
    }
    let lower = lines.len().saturating_sub(MAX_COMPOSER_LINES);

    let labeled_bottom = (lower..lines.len()).rev().find(|index| {
        is_horizontal_rule(lines[*index].0) && lines[*index].0.contains(char::is_alphanumeric)
    });
    if let Some(bottom) = labeled_bottom
        && let Some(prompt) = (lower..bottom).rev().find(|index| is_prompt_row(lines[*index].0))
        && let Some(top) = (lower..prompt).rev().find(|index| is_horizontal_rule(lines[*index].0))
    {
        return Some((lines[top].1, lines[bottom].2));
    }

    let mut composer = None;
    for top in lower..lines.len().saturating_sub(2) {
        if !is_horizontal_rule(lines[top].0) || !is_prompt_row(lines[top + 1].0) {
            continue;
        }
        if let Some(bottom) =
            (top + 2..lines.len()).find(|index| is_horizontal_rule(lines[*index].0))
        {
            composer = Some((lines[top].1, lines[bottom].2));
        }
    }
    composer
}

pub(super) fn claude_input_region(screen: &str) -> &str {
    claude_composer_bounds(screen)
        .map(|(start, end)| &screen[start..end])
        .unwrap_or_else(|| from_last_empty_prompt(screen))
}

pub(super) fn has_current_composer(agent: AgentKind, screen: &str) -> bool {
    match agent {
        AgentKind::Claude => claude_composer_bounds(screen).is_some(),
        AgentKind::Codex => codex_activity_region(screen).is_some(),
        _ => true,
    }
}

pub(super) fn is_history_viewport(agent: AgentKind, screen: &str) -> bool {
    if agent == AgentKind::Codex {
        return codex_history_viewport(screen);
    }
    if agent != AgentKind::Claude {
        return false;
    }
    let rows: Vec<_> = screen.lines().map(str::trim).filter(|row| !row.is_empty()).collect();
    let start = rows.len().saturating_sub(8);
    let mut fenced = false;
    for (index, row) in rows.iter().enumerate() {
        if row.starts_with("```") || row.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced || index < start {
            continue;
        }
        let banner_end = if CLAUDE_HISTORY.is_match(row) {
            Some(index + 1)
        } else if rows
            .get(index + 1)
            .is_some_and(|next| CLAUDE_HISTORY.is_match(&format!("{row} {next}")))
        {
            Some(index + 2)
        } else {
            None
        };
        let Some(end) = banner_end else { continue };
        let following = &rows[end..];
        let boxed_prompt = rows[end..]
            .windows(2)
            .any(|pair| is_horizontal_rule(pair[0]) && is_prompt_row(pair[1]));
        if !boxed_prompt || following.first().is_some_and(|line| is_horizontal_rule(line)) {
            return true;
        }
    }
    false
}

pub(super) fn attention_region(agent: AgentKind, screen: &str) -> &str {
    if agent == AgentKind::Claude {
        return claude_input_region(screen);
    }
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
        r"(?s)^[^\r\n]+(?:\r?\n[^\r\n]+)?[ \t]+(?:default|minimal|low|medium|high|xhigh|max|ultra)[ \t]*[·•][^\r\n]*(?:\r?\n[ \t]*Context [^\r\n]+)?(?:\r?\n[ \t]*\?[ \t]+(?:for\s+)?shortcuts[^\r\n]*)?(?:\r?\n[ \t]*⚠[ \t]*\d+[ \t]+warnings?[ \t]*[·•][ \t]*f2[ \t]+to[ \t]+view)?\s*\z",
    )
    .unwrap()
});

/// A composer is a frame boundary, not evidence that an assistant has stopped.
/// Only the adjacent activity block may supply busy/error observations.
pub(super) fn codex_activity_region(screen: &str) -> Option<&str> {
    let (start, end) = codex_composer_bounds(screen)?;
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

fn codex_composer_bounds(screen: &str) -> Option<(usize, usize)> {
    let mut composer = None;
    let mut offset = 0;
    for line in screen.split_inclusive('\n') {
        let row = line.trim();
        if let Some(rest) = row.strip_prefix(['›', '»'])
            && (rest.is_empty() || rest.starts_with(char::is_whitespace))
            && !rest.trim_start().starts_with(|c: char| c.is_ascii_digit() || c == '[')
        {
            composer = Some((offset, offset + line.len()));
        }
        offset += line.len();
    }
    composer
}

fn codex_history_viewport(screen: &str) -> bool {
    let Some((start, end)) = codex_composer_bounds(screen) else { return false };
    let footer = screen[end..].trim();
    if footer.len() > 2048 {
        return false;
    }
    let rows: Vec<_> = footer.lines().map(str::trim).filter(|row| !row.is_empty()).collect();
    // History controls must terminate the actual model footer, not occur in prose.
    for count in 1..=3.min(rows.len()) {
        let split = rows.len() - count;
        if CODEX_HISTORY_FOOTER.is_match(&rows[split..].join(" "))
            && CODEX_FOOTER.is_match(&rows[..split].join("\n"))
        {
            return true;
        }
    }
    if !CODEX_FOOTER.is_match(footer) {
        return false;
    }
    let above: Vec<_> = screen[..start]
        .lines()
        .rev()
        .map(str::trim)
        .filter(|row| !row.is_empty())
        .take(3)
        .collect();
    for count in 1..=above.len() {
        let banner = above[..count].iter().rev().copied().collect::<Vec<_>>().join(" ");
        if CODEX_HISTORY_BANNER.is_match(&banner) {
            return true;
        }
    }
    false
}

/// Only the final activity block above the current boxed Claude composer.
/// Drafts, old prompt frames and quoted status rows are not activity chrome.
pub(super) fn claude_activity_region(screen: &str) -> &str {
    let Some((end, _)) = claude_composer_bounds(screen) else {
        return "";
    };
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
    fn agent_scroll_history_viewport_is_not_current_turn_evidence() {
        let patterns = crate::ai_agents::parse_auto_continue_error_patterns(
            nebula_settings::DEFAULT_AUTO_CONTINUE_ERROR_PATTERNS,
        );
        let composer = "────────\n❯ \n────────\n? for shortcuts";
        for banner in [
            "Jump to bottom (ctrl+End) ↓",
            "1 new message (ctrl+End) ↓",
            "12 new messages (CTRL+END) ↓",
            "Jump to bottom\n(ctrl+End) ↓",
        ] {
            let screen = format!(
                "API Error: stream error: stream disconnected before completion\n\
                 ✻ Thinking… (3m 11s · ↓ 122 tokens)\n\n{banner}\n{composer}"
            );
            assert!(detect("claude", &screen).is_none(), "{banner}");
            assert!(
                crate::ai_agents::auto_continue_error_text("claude", &screen, &patterns).is_none(),
                "historical errors: {banner}"
            );
            assert!(!crate::ai_agents::auto_continue_screen_is_safe("claude", &screen));
        }
        for text in [
            "The guide says Jump to bottom (ctrl+End) ↓",
            "`Jump to bottom (ctrl+End) ↓`",
            "Jump to bottom",
            "ctrl+End",
            "1 new message",
            "❯ Jump to bottom (ctrl+End) ↓",
            "```text\nJump to bottom (ctrl+End) ↓\n```",
            "Jump to bottom (ctrl+End) ↓\nThe explanation continues here.",
        ] {
            let screen = format!("{text}\n{composer}");
            assert_eq!(detect("claude", &screen).unwrap().status, AgentStatus::Idle);
        }
    }

    #[test]
    fn codex_scroll_history_chrome_never_supplies_live_evidence() {
        let patterns = crate::ai_agents::parse_auto_continue_error_patterns(
            "stream disconnected before completion:",
        );
        let error = "■ stream disconnected before completion: upstream returned HTML (502)";
        let composer = "› Ask Codex to do anything\nGPT-6.1 Sol · Agent1 max · Context 21% used";
        for (banner, footer) in [
            ("↓ Back to bottom · esc", "? for shortcuts"),
            ("↓ Back to bottom\n· esc", "? for shortcuts"),
            ("", "Earlier messages available. enter/esc latest · ? for shortcuts"),
            ("", "Earlier messages available.\nenter/esc latest · ? for shortcuts"),
            (
                "↓ Back to bottom · esc",
                "Earlier messages available. enter/esc latest · ? for shortcuts  ⚠ 2 warnings · f2 to view",
            ),
            (
                "↓ Back to bottom · esc",
                "Earlier messages available. enter/esc latest · ? shortcuts ⚠ 2 warnings · f2 to view",
            ),
            (
                "↓ Back to bottom · esc",
                "Earlier messages available. enter/esc latest · ? shortcuts",
            ),
        ] {
            let screen = format!("{error}\n{banner}\n{composer}\n{footer}");
            assert!(crate::ai_agents::screen_is_history_viewport("codex", &screen), "{screen}");
            assert!(detect("codex", &screen).is_none(), "history is not a turn outcome");
            assert!(
                crate::ai_agents::auto_continue_error_text("codex", &screen, &patterns).is_none()
            );
            assert!(!crate::ai_agents::auto_continue_screen_is_safe("codex", &screen));
        }

        let screenshot_status = "GPT-6.1 Sol · Agent1 max · 825K out · Context 21% used · No changes · main · 370K window · status\n? for shortcuts\n⚠ 2 warnings · f2 to view";
        let scrolled = format!(
            "{error}\n↓ Back to bottom · esc\n› Ask Codex to do anything\n{screenshot_status}"
        );
        assert!(crate::ai_agents::screen_is_history_viewport("codex", &scrolled));
        assert!(
            crate::ai_agents::auto_continue_error_text("codex", &scrolled, &patterns).is_none()
        );
        assert!(!crate::ai_agents::auto_continue_screen_is_safe("codex", &scrolled));

        let footer_history = format!(
            "{error}\n› Ask Codex to do anything\nGPT-6.1 Sol · Agent1 max · 825K out · Context 21% used · No changes · main · 370K window · status\nEarlier messages available.  enter/esc latest · ? shortcuts\n⚠ 2 warnings · f2 to view"
        );
        assert!(crate::ai_agents::screen_is_history_viewport("codex", &footer_history));
        assert!(
            crate::ai_agents::auto_continue_error_text("codex", &footer_history, &patterns)
                .is_none()
        );
        assert!(!crate::ai_agents::auto_continue_screen_is_safe("codex", &footer_history));
    }

    #[test]
    fn codex_scroll_history_lookalikes_do_not_hide_the_live_composer() {
        let composer =
            "› Ask Codex to do anything\ngpt-6 medium · Context 2% used\n? for shortcuts";
        for prose in [
            "The guide says ↓ Back to bottom · esc",
            "`↓ Back to bottom · esc`",
            "\"Earlier messages available. enter/esc latest · ? for shortcuts\"",
            "↓ Back to bottom · esc\nThe explanation continues here.",
            "```text\n↓ Back to bottom · esc\n```",
            "~~~text\nEarlier messages available. enter/esc latest · ? for shortcuts\n~~~",
            "Earlier messages available. enter/esc latest · ? for shortcuts",
        ] {
            let screen = format!("{prose}\n{composer}");
            assert!(!crate::ai_agents::screen_is_history_viewport("codex", &screen), "{prose}");
            assert!(crate::ai_agents::screen_has_current_composer("codex", &screen));
        }
        let draft = "› ↓ Back to bottom · esc\ngpt-6 medium · Context 2% used\n? for shortcuts";
        assert!(!crate::ai_agents::screen_is_history_viewport("codex", draft));
        let truncated_code = format!("tail of a long code block\n```\n{composer}");
        assert!(crate::ai_agents::screen_has_current_composer("codex", &truncated_code));
        assert!(!crate::ai_agents::screen_is_history_viewport("codex", &truncated_code));
    }

    #[test]
    fn claude_labeled_composer_keeps_the_live_form_above_task_chrome() {
        let screen = "\
  Thought for 5m 35s, ran 4 shell commands
────────────────────────────────
 ☐ 同步范围

│ “Win 同步到 WSL”需要决定 WSL 哪些新 skill 要启用。

❯ 1. 只启用可移植项 (Recommended)
  2. 复制全部 29 个
  3. 只修内容差异
  4. Type something.
────────────────────────────────
  5. Chat about this

Enter to select · ↑/↓ to navigate · Esc to cancel
──────────────────────────────── magpie ─

  3 tasks (2 done, 1 in progress, 0 open)
  ✔ 修复 Win skill 双 CR
  ✔ 评估 Win 独有 skills
  ◼ 分类并同步 skill 变体";
        let detection = detect("claude", screen).expect("the current form must remain visible");
        assert_eq!(detection.status, AgentStatus::Blocked);
        assert_eq!(
            detection.rule_id, "live_form_blocked",
            "the local composer rule must own the current control footer"
        );
    }

    #[test]
    fn labeled_composer_uses_the_nearest_frame_above_the_current_prompt() {
        let rule = "────────────────";
        let screen = format!(
            "old answer\n{rule}\n❯ old draft\n{rule}\n\
             continued answer\n✻ Thinking… (1s)\n{rule}\n❯ \n{rule} magpie ─"
        );
        let expected = screen.rfind(&format!("{rule}\n❯ ")).unwrap();
        assert_eq!(
            super::claude_composer_bounds(&screen).unwrap().0,
            expected,
            "a stale prompt frame must not extend the current composer upward"
        );
    }

    #[test]
    fn claude_task_chrome_does_not_hide_compaction_or_replace_idle() {
        let tasks = "\n\n  3 tasks (2 done, 1 in progress, 0 open)
  ✔ 修复 Win skill 双 CR
  ✔ 评估 Win 独有 skills
  ◼ 分类并同步 skill 变体";
        let working = format!(
            "✢ Compacting conversation… (2s)\n────────────────\n❯ \n──────────────── magpie ─{tasks}"
        );
        assert!(super::is_horizontal_rule("────────────────"));
        assert!(super::is_prompt_row("❯ "));
        assert!(super::is_horizontal_rule("──────────────── magpie ─"));
        assert!(
            super::claude_composer_bounds(&working).is_some(),
            "composer fixture was not parsed: {working:?}"
        );
        assert_eq!(super::claude_activity_region(&working), "✢ Compacting conversation… (2s)");
        let detected = detect("claude", &working).unwrap();
        assert_eq!(detected.status, AgentStatus::Working, "rule={}", detected.rule_id);

        let idle =
            format!("✻ Baked for 15m 24s\n────────────────\n❯ \n──────────────── magpie ─{tasks}");
        assert_eq!(detect("claude", &idle).unwrap().status, AgentStatus::Idle);
        assert!(detect("claude", tasks.trim()).is_none(), "task chrome alone is not a turn");
    }

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
