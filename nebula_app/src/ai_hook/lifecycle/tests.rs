use super::*;
use crate::ai_hook::parse_remote_envelope;

fn event(source: &str, session: &str, name: &str) -> AiHookEvent {
    let payload = if source == "codex" && name == "notify" {
        serde_json::json!({"type":"agent-turn-complete", "thread-id":session})
    } else if matches!(source, "opencode" | "pi") {
        let kind = match name {
            "SessionStart" => "session-start",
            "UserPromptSubmit" => "prompt",
            "PermissionRequest" => "attention",
            "PostToolUse" => "tool-complete",
            "Stop" => "done",
            "SessionEnd" => "session-end",
            _ => panic!("unknown fixture event {name}"),
        };
        serde_json::json!({"kind":kind, "session_id":session})
    } else {
        serde_json::json!({"hook_event_name":name, "session_id":session})
    };
    let wire = format!("nebula-hook/1 source={source}\n{payload}");
    parse_remote_envelope(wire.as_bytes(), Some(1)).unwrap()
}

fn screen(status: AgentStatus) -> Option<Detection> {
    Some(Detection { agent: AgentKind::Codex, status, rule_id: "test.chrome".into() })
}

#[test]
fn wsl_hook_codex_rollout_failure_is_bound_to_current_turn_and_cannot_become_done() {
    let event = |name: &str, turn: &str, sequence: u64| {
        parse_remote_envelope(format!("nebula-hook/1 source=codex process=20:200 codex_hooks=full\n{{\"hook_event_name\":\"{name}\",\"session_id\":\"session\",\"turn_id\":\"{turn}\",\"bridge_sequence\":{sequence}}}").as_bytes(), Some(1)).unwrap()
    };
    let mut activity = AgentActivity::default();
    assert!(activity.codex_failure_target().is_none());
    activity.apply_hook(&event("UserPromptSubmit", "first", 1));
    let target = activity.codex_failure_target().unwrap();
    for field in ["session", "turn", "process"] {
        let mut foreign = target.clone();
        match field {
            "session" => foreign.session = "foreign".into(),
            "turn" => foreign.turn = "old".into(),
            _ => foreign.process = "20:201".into(),
        }
        assert!(!activity.observe_codex_failure(&foreign));
    }
    assert!(activity.observe_codex_failure(&target));
    assert_eq!(activity.status(), AgentStatus::Failed);
    assert!(!activity.observe_codex_failure(&target));
    for name in ["Stop", "PostToolUse", "PermissionRequest"] {
        assert!(!activity.accepts_hook(&event(name, "first", 2)));
    }
    activity.submitted();
    assert!(activity.codex_failure_target().is_none(), "local input cannot bind a new turn");
    assert!(!activity.accepts_hook(&event("Stop", "first", 2)));
    activity.apply_hook(&event("UserPromptSubmit", "second", 3));
    assert!(!activity.observe_codex_failure(&target), "old async results cannot end the new turn");
    assert_eq!(activity.status(), AgentStatus::Working);
    activity.apply_hook(&event("Stop", "second", 4));
    assert_eq!(activity.status(), AgentStatus::Done);
    assert!(!activity.observe_codex_failure(&target));
    activity.command_finished();
    assert!(activity.codex_failure_target().is_none());
}

fn claude_turn_event(name: &str, prompt: &str, sequence: u64, trigger: &str) -> AiHookEvent {
    let payload = serde_json::json!({
        "hook_event_name": name, "session_id": "compact-session", "prompt_id": prompt,
        "source": "compact", "trigger": trigger, "bridge_sequence": sequence,
    });
    let wire = format!("nebula-hook/1 source=claude process=4100:70\n{payload}");
    parse_remote_envelope(wire.as_bytes(), Some(1)).expect("supported Claude lifecycle event")
}

#[test]
fn claude_compaction_is_a_phase_not_a_new_or_completed_turn() {
    for trigger in ["manual", "auto"] {
        let mut activity = AgentActivity::default();
        assert!(activity.apply_hook(&claude_turn_event("UserPromptSubmit", "task", 1, "")));
        let (prompt, restored) = if trigger == "manual" {
            assert!(activity.apply_hook(&claude_turn_event("Stop", "task", 2, "")));
            ("manual-compact", AgentStatus::Idle)
        } else {
            ("task", AgentStatus::Working)
        };
        let start = claude_turn_event("PreCompact", prompt, 3, trigger);
        assert!(activity.apply_hook(&start));
        assert_eq!(activity.status(), AgentStatus::Working);
        assert_eq!(activity.turn_id.as_deref(), Some("task"), "no synthetic prompt");
        assert!(!activity.allows_screen());
        assert!(!activity.apply_hook(&start), "duplicate start");
        if trigger == "manual" {
            assert!(!activity.apply_hook(&claude_turn_event("Stop", "task", 4, "")));
        }
        assert!(activity.apply_hook(&claude_turn_event("SessionStart", prompt, 5, "")));
        assert_eq!(activity.status(), restored);
        assert_eq!(activity.turn_id.as_deref(), Some("task"));
        assert!(!activity.apply_hook(&claude_turn_event("PostCompact", prompt, 6, trigger)));
        assert_eq!(activity.status(), restored, "two end hooks cannot complete a task");
        assert!(activity.apply_hook(&claude_turn_event("UserPromptSubmit", "continue", 7, "")));
        for event in [
            claude_turn_event("Stop", "task", 8, ""),
            claude_turn_event("SessionStart", prompt, 9, ""),
            claude_turn_event("PostCompact", prompt, 10, trigger),
            claude_turn_event("PreCompact", prompt, 11, trigger),
        ] {
            assert!(!activity.apply_hook(&event), "old event {:?}", event.kind);
            assert_eq!(activity.status(), AgentStatus::Working);
            assert_eq!(activity.turn_id.as_deref(), Some("continue"));
        }
    }
}

#[test]
fn claude_compaction_failure_and_new_turn_retire_the_old_phase() {
    for trigger in ["manual", "auto"] {
        let mut activity = AgentActivity::default();
        activity.apply_hook(&claude_turn_event("UserPromptSubmit", "task", 1, ""));
        let prompt = if trigger == "manual" {
            activity.apply_hook(&claude_turn_event("Stop", "task", 2, ""));
            "compact"
        } else {
            "task"
        };
        assert!(activity.apply_hook(&claude_turn_event("PreCompact", prompt, 3, trigger)));
        assert!(activity.apply_hook(&claude_turn_event("StopFailure", prompt, 4, "")));
        assert_eq!(activity.status(), AgentStatus::Failed);
        assert!(!activity.compacting());
        assert!(activity.apply_hook(&claude_turn_event("UserPromptSubmit", "continue", 5, "")));
        assert!(!activity.apply_hook(&claude_turn_event("StopFailure", prompt, 6, "")));
        assert_eq!(activity.status(), AgentStatus::Working);
    }
}

#[test]
fn claude_compaction_cannot_resurrect_after_shell_exit() {
    let mut activity = AgentActivity::default();
    activity.apply_hook(&claude_turn_event("UserPromptSubmit", "task", 1, ""));
    activity.apply_hook(&claude_turn_event("PreCompact", "task", 2, "auto"));
    activity.command_finished();
    assert!(!activity.apply_hook(&claude_turn_event("SessionStart", "task", 3, "")));
    assert!(!activity.hook_seen());
    assert_eq!(activity.status(), AgentStatus::Unknown);
}

#[test]
fn claude_compact_submission_waits_for_the_real_phase_without_a_synthetic_turn() {
    for command in ["/compact", " /compact keep the constraints "] {
        let mut activity = AgentActivity::default();
        activity.apply_hook(&event("claude", "compact-session", "SessionStart"));
        activity.submitted_text("claude", command);
        assert_eq!(activity.status(), AgentStatus::Idle);
        assert!(!activity.pending_submit);
        assert!(activity.apply_hook(&claude_turn_event("PreCompact", "compact", 1, "manual")));
        assert_eq!(activity.status(), AgentStatus::Working);
        assert!(activity.turn_id.is_none());
        assert!(activity.apply_hook(&claude_turn_event("PostCompact", "compact", 2, "manual")));
        assert_eq!(activity.status(), AgentStatus::Idle);
        activity.submitted_text("claude", "/compactly explain this name");
        assert_eq!(activity.status(), AgentStatus::Working);
        assert!(activity.pending_submit);
    }
}

#[test]
fn claude_compact_first_hook_does_not_treat_process_start_as_a_running_turn() {
    for trigger in ["manual", "auto"] {
        let mut activity = AgentActivity::default();
        activity.begin_command(true);
        assert!(activity.apply_hook(&claude_turn_event("PreCompact", "compact", 1, trigger)));
        assert!(activity.hook_seen());
        assert_eq!(activity.status(), AgentStatus::Working);
        assert!(activity.apply_hook(&claude_turn_event("PostCompact", "compact", 2, trigger)));
        assert_eq!(activity.status(), AgentStatus::Idle);
    }
}

#[test]
fn claude_auto_compact_cannot_reopen_a_finished_or_superseded_turn() {
    for outcome in ["Stop", "StopFailure"] {
        let mut activity = AgentActivity::default();
        assert!(activity.apply_hook(&claude_turn_event("UserPromptSubmit", "task", 1, "")));
        assert!(activity.apply_hook(&claude_turn_event(outcome, "task", 2, "")));
        let status = activity.status();
        assert!(!activity.apply_hook(&claude_turn_event("PreCompact", "task", 3, "auto")));
        assert_eq!(activity.status(), status);
        assert!(!activity.compacting());
        assert!(activity.apply_hook(&claude_turn_event("UserPromptSubmit", "continue", 4, "")));
        assert!(!activity.apply_hook(&claude_turn_event("PreCompact", "task", 5, "auto")));
        assert_eq!(activity.status(), AgentStatus::Working);
        assert!(activity.apply_hook(&claude_turn_event("PreCompact", "continue", 6, "auto")));
    }
}

#[test]
fn complete_hooks_own_every_state_even_when_screen_disagrees() {
    for provider in ["claude", "opencode"] {
        let mut activity = AgentActivity::default();
        for (name, status) in [
            ("SessionStart", AgentStatus::Idle),
            ("UserPromptSubmit", AgentStatus::Working),
            ("PermissionRequest", AgentStatus::Blocked),
            ("PostToolUse", AgentStatus::Working),
            ("Stop", AgentStatus::Done),
        ] {
            assert!(activity.apply_hook(&event(provider, "primary", name)));
            for candidate in [AgentStatus::Idle, AgentStatus::Blocked, AgentStatus::Working] {
                for _ in 0..6 {
                    assert!(!activity.observe_screen(screen(candidate)));
                }
                assert_eq!(activity.status(), status, "{provider}/{name}/{candidate:?}");
                assert_eq!(activity.source(), AgentStatusSource::Hook);
            }
        }
    }
}

#[test]
fn turn_hooks_allow_only_missing_attention_evidence_during_a_turn() {
    let mut activity = AgentActivity::default();
    activity.apply_hook(&event("pi", "session", "SessionStart"));
    assert!(!activity.observe_screen(screen(AgentStatus::Working)));
    assert!(!activity.observe_screen(screen(AgentStatus::Blocked)));
    activity.apply_hook(&event("pi", "session", "UserPromptSubmit"));
    for _ in 0..6 {
        assert!(!activity.observe_screen(screen(AgentStatus::Idle)));
    }
    assert_eq!(activity.status(), AgentStatus::Working);
    assert!(activity.observe_screen(screen(AgentStatus::Blocked)));
    activity.apply_hook(&event("pi", "session", "PostToolUse"));
    assert_eq!(activity.status(), AgentStatus::Working);
    activity.apply_hook(&event("pi", "session", "Stop"));
    activity.input_sent();
    for _ in 0..6 {
        activity.observe_screen(screen(AgentStatus::Idle));
    }
    for status in [AgentStatus::Working, AgentStatus::Blocked] {
        assert!(!activity.observe_screen(screen(status)));
        assert_eq!(activity.status(), AgentStatus::Done);
    }
}

#[test]
fn completion_only_hooks_latch_old_frames_but_can_observe_a_new_turn() {
    let mut activity = AgentActivity::default();
    activity.apply_hook(&event("codex", "thread", "notify"));
    for status in [AgentStatus::Working, AgentStatus::Blocked] {
        assert!(!activity.observe_screen(screen(status)));
        assert_eq!(activity.status(), AgentStatus::Done);
    }
    // A single idle redraw does not reopen the previous turn.
    activity.observe_screen(screen(AgentStatus::Idle));
    activity.observe_screen(screen(AgentStatus::Working));
    assert_eq!(activity.status(), AgentStatus::Done);
    for _ in 0..2 {
        activity.observe_screen(screen(AgentStatus::Idle));
    }
    assert!(activity.observe_screen(screen(AgentStatus::Working)));
    for _ in 0..6 {
        activity.observe_screen(screen(AgentStatus::Idle));
    }
    assert_eq!(activity.status(), AgentStatus::Working, "completion still belongs to notify");
    activity.apply_hook(&event("codex", "thread", "notify"));
    activity.input_sent();
    assert!(activity.observe_screen(screen(AgentStatus::Blocked)));
    assert_eq!(activity.status(), AgentStatus::Blocked);
}

#[test]
fn codex_capabilities_follow_the_installed_notify_bridge() {
    let notify = event("codex", "thread", "notify");
    assert!(!notify.capabilities().lifecycle);
    assert!(!notify.capabilities().attention_context);
    // Merely recognizing a provider's event name would not prove that our
    // installer delivers its starts, approvals and completions as a contract.
    for name in ["SessionStart", "UserPromptSubmit", "PermissionRequest", "Stop"] {
        let wire = format!(
            "nebula-hook/1 source=codex\n{{\"hook_event_name\":\"{name}\",\"session_id\":\"s\"}}"
        );
        assert!(parse_remote_envelope(wire.as_bytes(), Some(1)).is_none());
    }
}

#[test]
fn claude_screen_idle_is_not_a_success_signal() {
    for preceding in [AgentStatus::Working, AgentStatus::Blocked] {
        let mut activity = AgentActivity::default();
        activity.begin_command(true);
        let observation = |status| {
            Some(Detection { agent: AgentKind::Claude, status, rule_id: "prompt_idle".into() })
        };
        activity.observe_screen(observation(AgentStatus::Working));
        activity.observe_screen(observation(preceding));
        for _ in 0..4 {
            let previous = activity.status();
            activity.observe_screen(observation(AgentStatus::Idle));
            assert_eq!(screen_notification(previous, activity.status(), false), None);
        }
        assert_eq!(activity.status(), AgentStatus::Idle);
        assert!(!activity.hook_seen());
    }
}

#[test]
fn fallback_requires_consecutive_idle_and_work_before_a_completion() {
    for agent in AgentKind::ALL {
        let screen = |status| Some(Detection { agent, status, rule_id: "test.chrome".into() });
        let mut activity = AgentActivity::default();
        activity.begin_command(true);
        for _ in 0..2 {
            activity.observe_screen(screen(AgentStatus::Idle));
        }
        assert_eq!(activity.status(), AgentStatus::Idle, "opening a CLI did not finish a turn");
        activity.observe_screen(screen(AgentStatus::Working));
        activity.observe_screen(screen(AgentStatus::Idle));
        activity.observe_screen(None);
        activity.observe_screen(screen(AgentStatus::Idle));
        assert_eq!(activity.status(), AgentStatus::Working);
        assert!(activity.observe_screen(screen(AgentStatus::Idle)));
        let (expected, notification) = if matches!(agent, AgentKind::Claude | AgentKind::Codex) {
            (AgentStatus::Idle, None)
        } else {
            (AgentStatus::Done, Some(false))
        };
        assert_eq!(activity.status(), expected, "{agent:?}");
        assert_eq!(
            screen_notification(AgentStatus::Working, activity.status(), false),
            notification,
            "{agent:?}"
        );
        assert_eq!(screen_notification(activity.status(), activity.status(), false), None);
    }
}

#[test]
fn runtime_submission_does_not_finish_on_the_previous_idle_frame() {
    for agent in AgentKind::ALL {
        let screen = |status| Some(Detection { agent, status, rule_id: "test.chrome".into() });
        let mut activity = AgentActivity::default();
        activity.submitted();
        for _ in 0..8 {
            activity.observe_screen(screen(AgentStatus::Idle));
        }
        assert_eq!(activity.status(), AgentStatus::Working);
        activity.observe_screen(screen(AgentStatus::Working));
        for _ in 0..2 {
            activity.observe_screen(screen(AgentStatus::Idle));
        }
        let expected = if matches!(agent, AgentKind::Claude | AgentKind::Codex) {
            AgentStatus::Idle
        } else {
            AgentStatus::Done
        };
        assert_eq!(activity.status(), expected, "{agent:?}");
    }
}

#[test]
fn codex_screen_idle_cannot_certify_an_answer_or_failure() {
    let footer =
        "› Ask Codex to do anything\ngpt-6-astra medium · Context 4% used\n? for shortcuts";
    for preceding in [AgentStatus::Working, AgentStatus::Blocked] {
        for (output, expected) in [
            ("Completed answer.", AgentStatus::Idle),
            (
                "■ stream disconnected before completion: failed to send request",
                AgentStatus::Failed,
            ),
            ("■ Rate limit exceeded", AgentStatus::Failed),
            ("■ 401 Unauthorized", AgentStatus::Failed),
            ("■ 503 Service Unavailable", AgentStatus::Failed),
            ("■ Turn interrupted", AgentStatus::Idle),
        ] {
            let mut activity = AgentActivity::default();
            activity.begin_command(true);
            activity.observe_screen(screen(AgentStatus::Working));
            activity.observe_screen(screen(preceding));
            let frame = format!("{output}\n{footer}");
            for _ in 0..6 {
                let previous = activity.status();
                activity.observe_screen(crate::ai_agents::detect("codex", &frame));
                let notification = screen_notification(previous, activity.status(), false);
                assert_eq!(
                    notification,
                    (expected == AgentStatus::Failed && previous != AgentStatus::Failed)
                        .then_some(false),
                    "{output}"
                );
            }
            assert_eq!(activity.status(), expected, "{preceding:?}: {output}");
            assert_eq!(activity.source(), AgentStatusSource::Screen);
            assert!(!activity.hook_seen());
            if expected == AgentStatus::Failed {
                // The persistent composer does not erase the failure.
                assert!(!activity.observe_screen(screen(AgentStatus::Idle)));
            }
            // An idle observation does not disable the next work/permission edge.
            assert!(activity.observe_screen(screen(AgentStatus::Working)));
            assert!(activity.observe_screen(screen(AgentStatus::Blocked)));
            assert_eq!(
                screen_notification(AgentStatus::Working, activity.status(), false),
                Some(true)
            );
        }
    }
}

#[test]
fn local_mode_command_keeps_activity_and_real_prompts_keep_the_submission_barrier() {
    for native_hook in [false, true] {
        let mut activity = AgentActivity::default();
        if native_hook {
            let wire = b"nebula-hook/1 source=codex codex_hooks=full\n{\"hook_event_name\":\"SessionStart\",\"session_id\":\"s\"}";
            let event = parse_remote_envelope(wire, Some(1)).unwrap();
            activity.apply_hook(&event);
        } else {
            activity.begin_command(true);
            for _ in 0..2 {
                activity.observe_screen(screen(AgentStatus::Idle));
            }
        }
        let source = activity.source();
        activity.submitted_text("codex", "/plan");
        assert_eq!(activity.status(), AgentStatus::Idle);
        assert_eq!(activity.source(), source);
        assert!(!activity.pending_submit);
        activity.submitted_text("codex", "请问我一个选择题");
        for _ in 0..8 {
            activity.observe_screen(screen(AgentStatus::Idle));
        }
        assert_eq!(activity.status(), AgentStatus::Working);
        assert!(activity.pending_submit);
        activity.submitted_text("codex", "/plan");
        assert_eq!(activity.status(), AgentStatus::Working, "a mode switch cannot finish work");
    }
    for (program, text) in [("claude", "/plan"), ("codex", "/review"), ("codex", "/plan\nmy task")]
    {
        let mut activity = AgentActivity::default();
        activity.submitted_text(program, text);
        assert!(activity.pending_submit, "{program}: {text}");
    }
}

#[test]
fn nested_local_or_remote_sessions_cannot_replace_primary_ownership() {
    for pid in [None, Some(10)] {
        let mut activity = AgentActivity::default();
        let mut primary = event("claude", "primary", "UserPromptSubmit");
        primary.agent_pid = pid;
        activity.apply_hook(&primary);
        for name in ["SessionStart", "PermissionRequest", "Stop", "SessionEnd"] {
            let mut child = event("claude", "child", name);
            child.agent_pid = pid.map(|pid| pid + 1);
            assert!(!activity.apply_hook(&child));
            assert_eq!(activity.status(), AgentStatus::Working);
        }
        assert_eq!(activity.primary_pid(), pid);
        assert!(!activity.apply_hook(&event("codex", "foreign", "notify")));
        activity.reset();
        assert!(activity.apply_hook(&event("codex", "next-command", "notify")));
    }
}

#[test]
fn verified_process_can_change_session_and_background_tasks_keep_working() {
    let mut activity = AgentActivity::default();
    let mut start = event("claude", "first", "SessionStart");
    start.agent_pid = Some(10);
    activity.apply_hook(&start);
    start.session_id = Some("cleared".into());
    assert!(activity.apply_hook(&start));
    let mut late = event("claude", "first", "Stop");
    late.agent_pid = Some(10);
    assert!(!activity.apply_hook(&late), "the old session cannot undo a verified /clear");
    start.kind = AiHookKind::TurnDone;
    start.background_tasks = Some(super::super::AiBackgroundTasks { active: 1, total: 2 });
    activity.apply_hook(&start);
    assert_eq!(activity.status(), AgentStatus::Working);
    start.background_tasks.as_mut().unwrap().active = 0;
    activity.apply_hook(&start);
    assert_eq!(activity.status(), AgentStatus::Done);
    start.kind = AiHookKind::SessionEnd;
    activity.apply_hook(&start);
    assert_eq!(activity.status(), AgentStatus::Unknown);
    assert!(!activity.hook_seen());
    assert_eq!(activity.primary_pid(), None);
}

#[test]
fn sessions_inside_one_provider_process_do_not_share_turn_authority() {
    let mut activity = AgentActivity::default();
    let mut primary = event("opencode", "main", "UserPromptSubmit");
    primary.agent_pid = Some(20);
    activity.apply_hook(&primary);
    for name in ["UserPromptSubmit", "PermissionRequest", "Stop", "SessionEnd"] {
        let mut nested = event("opencode", "nested", name);
        nested.agent_pid = Some(20);
        assert!(!activity.apply_hook(&nested));
        assert_eq!(activity.status(), AgentStatus::Working);
    }
}

#[test]
fn ordinary_idle_reminders_are_not_blocking_requests() {
    for kind in [
        "idle",
        "idle_prompt",
        "auth_success",
        "assistant_message",
        "input_completed",
        "permission_granted",
        "approval_complete",
    ] {
        let wire = format!(
            "nebula-hook/1 source=claude\n{{\"hook_event_name\":\"Notification\",\"notification_type\":\"{kind}\"}}"
        );
        assert!(parse_remote_envelope(wire.as_bytes(), Some(1)).is_none());
    }
    assert_eq!(event("claude", "s", "PermissionRequest").kind, AiHookKind::NeedsAttention);
    let question = b"nebula-hook/1 source=claude\n{\"hook_event_name\":\"Notification\",\"notification_type\":\"elicitation_dialog\"}";
    assert_eq!(parse_remote_envelope(question, Some(1)).unwrap().kind, AiHookKind::NeedsAttention);
}

#[test]
fn untyped_legacy_notifications_cannot_reopen_an_idle_or_finished_turn() {
    let wire = b"nebula-hook/1 source=claude\n{\"hook_event_name\":\"Notification\",\"session_id\":\"s\",\"message\":\"waiting for input\"}";
    let notification = parse_remote_envelope(wire, Some(1)).unwrap();
    let mut activity = AgentActivity::default();
    activity.apply_hook(&event("claude", "s", "SessionStart"));
    assert!(!activity.apply_hook(&notification));
    assert_eq!(activity.status(), AgentStatus::Idle);
    activity.apply_hook(&event("claude", "s", "UserPromptSubmit"));
    assert!(activity.apply_hook(&notification));
    assert_eq!(activity.status(), AgentStatus::Blocked);
    activity.apply_hook(&event("claude", "s", "Stop"));
    assert!(!activity.apply_hook(&notification));
    assert_eq!(activity.status(), AgentStatus::Done);
    // An explicit permission request is actionable even without a prior turn.
    assert!(activity.apply_hook(&event("claude", "s", "PermissionRequest")));
    assert_eq!(activity.status(), AgentStatus::Blocked);
}

#[test]
fn shell_command_end_rejects_late_hooks_until_a_new_command_starts() {
    let mut activity = AgentActivity::default();
    activity.apply_hook(&event("claude", "old", "UserPromptSubmit"));
    activity.command_finished();
    for status in [AgentStatus::Working, AgentStatus::Blocked, AgentStatus::Idle] {
        assert!(!activity.observe_screen(screen(status)));
    }
    for name in ["PostToolUse", "PermissionRequest", "Stop"] {
        assert!(!activity.apply_hook(&event("claude", "old", name)));
        assert_eq!(activity.status(), AgentStatus::Unknown);
    }
    activity.begin_command(true);
    assert!(activity.apply_hook(&event("codex", "new", "notify")));
}
