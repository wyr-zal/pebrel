use super::*;

#[test]
fn real_antigravity_working_chrome_reads_working() {
    let screen = "  ⠋ Thinking... (8s)\n\
                  ────────────────────────────────────────\n\
                  › \n\
                  ────────────────────────────────────────\n\
                  \x20 Plan mode: research & plan only (shift+tab to cycle) · Press esc to interrupt generation";
    let detection = detect("antigravity", screen).unwrap();
    assert_eq!(detection.status, AgentStatus::Working, "rule={}", detection.rule_id);
}

#[test]
fn real_antigravity_idle_chrome_reads_idle() {
    let screen = "  I have completed the task and updated the configuration.\n\
                  ────────────────────────────────────────\n\
                  › \n\
                  ────────────────────────────────────────\n\
                  \x20 Plan mode: research & plan only (shift+tab to cycle) · ? for shortcuts";
    let detection = detect("antigravity", screen).unwrap();
    assert_eq!(detection.status, AgentStatus::Idle, "rule={}", detection.rule_id);
}

#[test]
fn antigravity_old_scrollback_spinner_does_not_prevent_idle() {
    // 历史滚屏中留有上一轮的思考转圈符号；region 限制在底部后不能把已结束的回合误判为 Working。
    let screen = "  ⠋ Thinking... (from previous turn 5m ago)\n\
                  \x20 Result: success\n\
                  \x20 Line 1\n\
                  \x20 Line 2\n\
                  \x20 Line 3\n\
                  \x20 Line 4\n\
                  \x20 Line 5\n\
                  \x20 Line 6\n\
                  \x20 Line 7\n\
                  \x20 Line 8\n\
                  \x20 Line 9\n\
                  \x20 Line 10\n\
                  ────────────────────────────────────────\n\
                  › \n\
                  ────────────────────────────────────────\n\
                  \x20 Accept-edits mode: file edits auto-approved (shift+tab to cycle)";
    let detection = detect("antigravity", screen).unwrap();
    assert_eq!(detection.status, AgentStatus::Idle, "rule={}", detection.rule_id);
}

#[test]
fn real_antigravity_permission_form_reads_blocked() {
    let screen = " requesting permission for:\n\
                  \x20 Run command: cargo test\n\
                  \x20 Do you want to proceed?\n\
                  \x20 › 1. Yes, allow\n\
                  \x20   2. No, deny access\n\
                  \x20 esc to cancel · tab amend";
    let detection = detect("antigravity", screen).unwrap();
    assert_eq!(detection.status, AgentStatus::Blocked, "rule={}", detection.rule_id);
}

#[test]
fn antigravity_previous_confirmation_does_not_override_current_prompt() {
    for phrase in [
        "The tool requires approval before running (y/n).",
        "Use enter to confirm or tab to amend the command.",
        "Do you want to proceed?",
        "waiting for permission",
        "requesting permission for: cargo test\n[y] Yes\nesc to cancel · enter to confirm",
    ] {
        for (activity, footer, expected) in [
            ("", "? for shortcuts", AgentStatus::Idle),
            ("", "esc to interrupt", AgentStatus::Working),
            ("⠋ Thinking... (8s)\n", "esc to interrupt", AgentStatus::Working),
        ] {
            let screen = format!("{phrase}\n{activity}────────\n› \n────────\n{footer}");
            let detection = detect("antigravity-cli", &screen).unwrap();
            assert_eq!(detection.status, expected, "{screen}: rule={}", detection.rule_id);
        }
    }
}

#[test]
fn antigravity_working_footer_excludes_quoted_form_hints() {
    for footer in
        ["↑/↓ Navigate · enter Select · esc Skip", "↑/↓ Navigate · tab Amend · f full diff"]
    {
        let working = format!(
            "The footer reads:\n{footer}\nand that means a choice is pending.\n⠋ Thinking... (2s)\nesc to interrupt"
        );
        assert_eq!(detect("agy", &working).unwrap().status, AgentStatus::Working);
        let approval = format!("Previous output: esc to interrupt\n{footer}\nesc to cancel");
        assert_eq!(detect("agy", &approval).unwrap().status, AgentStatus::Blocked);
    }
}

#[test]
fn prompt_drafts_need_input_box_context() {
    for row in ["> fix the tests please", "│ › fix the tests │", "› Ask anything…"] {
        let screen = format!("Do you want to proceed?\n────────\n{row}\n────────\n? for shortcuts");
        assert_eq!(detect("agy", &screen).unwrap().status, AgentStatus::Idle);
    }
    for row in ["> Yes, I trust this folder", "> Allow once", "> [ ] 中文", "> 1) Yes", "> 1. Yes"]
    {
        let screen = format!("Do you want to proceed?\n{row}\nesc to cancel");
        assert_eq!(from_last_empty_prompt(&screen), screen);
    }
}

#[test]
fn malformed_prompt_regions_are_rejected_at_load() {
    for region in [
        "after_last_prompt_row()",
        "after_last_prompt_row(",
        "after_last_prompt_row(unknown)",
        "after_last_prompt_row(bottom_non_empty_lines(0))",
    ] {
        let source = format!(
            "id = 'custom'\n[[rules]]\nid = 'test'\nstate = 'idle'\nregion = '{region}'\ncontains = ['ready']"
        );
        assert!(compile_manifest(&source, None).unwrap_err().contains("invalid prompt region"));
    }
}

#[test]
fn prompt_regions_are_explicit_and_preserve_unknown_input() {
    let screen = "Do you want to proceed?\n│ ›   │\n? for shortcuts";
    assert_eq!(
        region(screen, "after_last_prompt_row(bottom_non_empty_lines(12))"),
        "│ ›   │\n? for shortcuts"
    );
    for row in ["› Ask anything…", "> 1. Yes", "› unfinished draft"] {
        let screen = format!("Do you want to proceed?\n{row}");
        assert_eq!(from_last_empty_prompt(&screen), screen);
    }
    let source = "id = 'custom'\nshared_rule_region = 'after_last_prompt_row'\n\
        [[rules]]\nid = 'shared_custom'\nstate = 'blocked'\n\
        region = 'whole_recent'\ncontains = ['approval']";
    let compiled = compile_manifest(source, None).unwrap();
    assert_eq!(compiled.manifest.rules[0].region, "whole_recent");
    assert_eq!(
        compiled.manifest.rules[1].region,
        "after_last_prompt_row(bottom_non_empty_lines(1))"
    );
    assert!(compile_manifest(&source.replace("after_last_prompt_row", "unknown"), None).is_err());
}

#[test]
fn antigravity_long_forms_and_extended_footers_still_block() {
    for (footer, rule) in [
        ("↑/↓ Navigate · enter Select · esc Skip", "question_selection"),
        ("↑/↓ Navigate · tab Amend · f full diff", "file_review"),
    ] {
        let screen = format!(
            "Question 1/1: Choose\n{}\n{footer} · ctrl+g expand\nesc to cancel",
            "long wrapped option\n".repeat(20)
        );
        let found = detect("agy", &screen).unwrap();
        assert_eq!(found.status, AgentStatus::Blocked);
        assert_eq!(found.rule_id, rule);
        // Even an exact standalone footer quoted in an answer is history
        // once the current input prompt is visible.
        for prompt in [">", "│ ›   │"] {
            let answered = format!("{screen}\n{prompt}\n? for shortcuts");
            assert_eq!(detect("agy", &answered).unwrap().status, AgentStatus::Idle);
        }
    }
}

#[test]
fn antigravity_real_command_permission_requires_attention() {
    let screen = "Command\n────────\nRequesting permission for:\n\
        cmd /c echo agy-command-probe\nDo you want to proceed?\n\
        > 1. Yes\n\
        2. Yes, and always allow in this conversation for commands that start with 'cmd'\n\
        3. Yes, and always allow for commands that start with 'cmd' (Persist to settings.json)\n\
        4. No\n↑/↓ Navigate · tab Amend · ctrl+g edit/expand command\n\
        esc to cancel                         Gemini 3.8 Flash · high";
    assert_eq!(detect("agy", screen).unwrap().status, AgentStatus::Blocked);
}

#[test]
fn antigravity_real_file_creation_review_requires_attention() {
    let review = "Create file\n────────\nagy-permission-check.txt +1\n\
        1 + permission probe\nAllow creation of this file?\n\
        > 1. Yes, allow creation\n  2. No, deny creation\n\
        ↑/↓ Navigate · tab Amend · f full diff\n\
        esc to cancel                         Gemini 3.8 Flash · high";
    let detection = detect("agy", review).unwrap();
    assert_eq!(detection.status, AgentStatus::Blocked);
    assert_eq!(detection.rule_id, "file_review");
    let edit = review
        .replace("Allow creation of this file?", "Accept this file edit?")
        .replace("Yes, allow creation", "Yes, accept this change")
        .replace("No, deny creation", "No, reject this change");
    assert_eq!(detect("agy", &edit).unwrap().status, AgentStatus::Blocked);
    let accepted = format!("{review}\nCreated file.\n────────\n>\n────────\n? for shortcuts");
    assert_eq!(detect("agy", &accepted).unwrap().status, AgentStatus::Idle);
    let automatic = "Created agy-permission-check.txt\n────────\n>\n────────\n? for shortcuts";
    assert_eq!(detect("agy", automatic).unwrap().status, AgentStatus::Idle);
}

#[test]
fn antigravity_real_ask_question_overrides_cancel_working_hint() {
    // Captured from agy 1.1.27 in a Windows ConPTY, before choosing Chinese.
    let question = "Question\n────────\n\
        Question 1/1: 请选择接下来交流使用的语言 / Please select the language to use:\n\
        > 1. 中文\n  2. English\n  3. Write-in...\n\
        ↑/↓ Navigate · enter Select · esc Skip\n\
        esc to cancel                         Gemini 3.8 Flash · high";
    let detection = detect("agy", question).unwrap();
    assert_eq!(detection.status, AgentStatus::Blocked);
    assert_eq!(detection.rule_id, "question_selection");

    let answered = format!("{question}\n已选择中文。\n────────\n>\n────────\n? for shortcuts");
    assert_eq!(detect("agy", &answered).unwrap().status, AgentStatus::Idle);
    let prose = "The shortcuts are ↑/↓ Navigate · enter Select · esc Skip\n>\n? for shortcuts";
    assert_eq!(detect("agy", prose).unwrap().status, AgentStatus::Idle);
}

#[test]
fn antigravity_active_confirmation_after_prompt_still_blocks() {
    let screen = "› \nrequesting permission for: cargo test\n› 1. Yes, allow\n\
                  2. No\nesc to cancel · enter to confirm";
    let detection = detect("antigravity-cli", screen).unwrap();
    assert_eq!(detection.status, AgentStatus::Blocked, "rule={}", detection.rule_id);
}

#[test]
fn antigravity_prose_mentioning_approval_while_idle_reads_idle() {
    // 回答正文中提及 "needs approval for ..."，但此时已完成并显示空闲提示符，
    // 不得误判为 Blocked。
    for phrase in ["needs approval for", "requesting approval for", "requesting permission for:"] {
        let screen = format!(
            "│ The tool {phrase} operations outside the workspace.\n\
             │ Please confirm your settings if you plan to enable this.\n\
             ────────────────────────────────────────\n\
             › \n\
             ────────────────────────────────────────\n\
             \x20 Plan mode: research & plan only (shift+tab to cycle) · ? for shortcuts"
        );
        let detection = detect("antigravity", &screen).unwrap();
        assert_eq!(detection.status, AgentStatus::Idle, "{phrase}: rule={}", detection.rule_id);
    }
}
