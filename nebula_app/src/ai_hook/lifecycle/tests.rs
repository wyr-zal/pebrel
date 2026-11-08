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

fn event_with_turn(source: &str, session: &str, name: &str, turn: &str) -> AiHookEvent {
    let mut event = event(source, session, name);
    event.turn_id = Some(turn.to_owned());
    event
}

fn screen(status: AgentStatus) -> Option<Detection> {
    Some(Detection { agent: AgentKind::Codex, status, rule_id: format!("test.{status:?}") })
}

#[test]
fn identifying_an_agent_command_does_not_prove_a_running_turn() {
    let mut activity = AgentActivity::default();
    activity.begin_command(true);
    assert_eq!(activity.status(), AgentStatus::Unknown);
    assert_eq!(activity.source(), AgentStatusSource::Process);
}

#[test]
fn native_owner_rejects_a_recycled_pid_and_missing_kernel_stamp() {
    let mut activity = AgentActivity::default();
    let mut start = event("claude", "native-owner", "UserPromptSubmit");
    start.agent_pid = Some(100);
    start.agent_start_time = Some(200);
    assert!(activity.apply_hook(&start));
    let mut next = event("claude", "native-owner", "PostToolUse");
    next.agent_pid = Some(100);
    next.agent_start_time = Some(201);
    assert!(!activity.apply_hook(&next));
    next.agent_start_time = None;
    assert!(!activity.apply_hook(&next));
    next.agent_start_time = Some(200);
    assert!(activity.apply_hook(&next));
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

fn codex_verified_turn_event(name: &str, turn: &str, sequence: u64) -> AiHookEvent {
    parse_remote_envelope(
        format!(
            "nebula-hook/1 source=codex process=20:200 codex_hooks=full\n{{\"hook_event_name\":\"{name}\",\"session_id\":\"session\",\"turn_id\":\"{turn}\",\"bridge_sequence\":{sequence}}}"
        )
        .as_bytes(),
        Some(1),
    )
    .unwrap()
}

#[test]
fn wsl_hook_codex_input_without_a_new_start_cannot_apply_old_rollout_evidence() {
    let mut activity = AgentActivity::default();
    assert!(activity.apply_hook(&codex_verified_turn_event("UserPromptSubmit", "first", 1)));
    let old_target = activity.codex_failure_target().unwrap();

    activity.input_sent();
    assert!(
        activity.codex_failure_target().is_none(),
        "physical input cannot preserve an authoritative target for the old turn"
    );
    assert!(!activity.observe_codex_failure(&old_target));
    assert_eq!(activity.status(), AgentStatus::Working);
}

#[test]
fn wsl_hook_codex_old_tool_hook_cannot_rebind_input_to_a_stale_turn() {
    let mut activity = AgentActivity::default();
    assert!(activity.apply_hook(&codex_verified_turn_event("UserPromptSubmit", "first", 1)));
    let old_target = activity.codex_failure_target().unwrap();
    activity.input_sent();
    activity.apply_hook(&codex_verified_turn_event("PostToolUse", "first", 2));
    assert!(activity.codex_failure_target().is_none());
    assert!(!activity.observe_codex_failure(&old_target));

    assert!(activity.apply_hook(&codex_verified_turn_event("UserPromptSubmit", "second", 3)));
    let current = activity.codex_failure_target().unwrap();
    assert_eq!(current.turn, "second");
    assert!(!activity.observe_codex_failure(&old_target));
    assert!(activity.observe_codex_failure(&current));
}

#[test]
fn wsl_hook_codex_structured_finish_and_cancel_are_idle_not_success() {
    for terminal in [CodexTerminalEvidence::Finished {}, CodexTerminalEvidence::Cancelled {}] {
        let mut activity = AgentActivity::default();
        assert!(activity.apply_hook(&codex_verified_turn_event("UserPromptSubmit", "first", 1)));
        let target = activity.codex_failure_target().unwrap();
        activity.mark_status_unconfirmed(
            crate::ai_agents::AgentStatusUncertainty::AwaitingTerminalEvidence,
        );
        assert!(activity.observe_codex_terminal(&target, &terminal));
        assert_eq!(activity.uncertainty(), None);
        assert_eq!(activity.status(), AgentStatus::Idle);
        assert_eq!(activity.source(), AgentStatusSource::Process);
        assert!(activity.hook_seen());
        assert!(activity.retired_codex_failure.is_none());
        assert!(!activity.observe_codex_terminal(&target, &terminal));
        for name in ["UserPromptSubmit", "PostToolUse", "PermissionRequest"] {
            assert!(!activity.apply_hook(&codex_verified_turn_event(name, "first", 2)), "{name}");
            assert_eq!(activity.status(), AgentStatus::Idle);
        }
    }
}

#[test]
fn wsl_hook_codex_structured_idle_keeps_one_authoritative_stop_and_rejects_late_work() {
    use crate::ai_hook::AiTurnOutcome;
    for outcome in [AiTurnOutcome::Succeeded, AiTurnOutcome::Failed, AiTurnOutcome::Cancelled] {
        let mut activity = AgentActivity::default();
        assert!(activity.apply_hook(&codex_verified_turn_event("UserPromptSubmit", "first", 1)));
        let target = activity.codex_failure_target().unwrap();
        assert!(activity.observe_codex_terminal(&target, &CodexTerminalEvidence::Finished {}));
        let mut stop = codex_verified_turn_event("Stop", "first", 2);
        stop.turn_outcome = outcome;
        assert!(activity.apply_hook(&stop), "a real Hook still owns its terminal outcome");
        let expected = match outcome {
            AiTurnOutcome::Succeeded => AgentStatus::Done,
            AiTurnOutcome::Failed => AgentStatus::Failed,
            _ => AgentStatus::Idle,
        };
        assert_eq!(activity.status(), expected);
        assert_eq!(activity.source(), AgentStatusSource::Hook);
        assert!(!activity.apply_hook(&codex_verified_turn_event("Stop", "first", 3)));
        assert!(!activity.apply_hook(&codex_verified_turn_event("PostToolUse", "first", 4)));
        assert!(!activity.observe_codex_terminal(&target, &CodexTerminalEvidence::Cancelled {}));
        assert_eq!(activity.status(), expected);
        assert!(activity.apply_hook(&codex_verified_turn_event("UserPromptSubmit", "second", 5)));
        assert_eq!(activity.status(), AgentStatus::Working);
        assert!(!activity.observe_codex_terminal(&target, &CodexTerminalEvidence::Finished {}));
    }
}

#[test]
fn wsl_hook_codex_terminal_evidence_rejects_foreign_or_unconfirmed_targets() {
    let mut activity = AgentActivity::default();
    assert!(activity.apply_hook(&codex_verified_turn_event("UserPromptSubmit", "first", 1)));
    let target = activity.codex_failure_target().unwrap();
    for field in ["session", "turn", "process"] {
        let mut foreign = target.clone();
        match field {
            "session" => foreign.session = "foreign".into(),
            "turn" => foreign.turn = "older".into(),
            _ => foreign.process = "20:201".into(),
        }
        assert!(!activity.observe_codex_terminal(&foreign, &CodexTerminalEvidence::Finished {}));
    }
    activity.input_sent();
    assert!(!activity.observe_codex_terminal(&target, &CodexTerminalEvidence::Finished {}));
    assert!(activity.apply_hook(&codex_verified_turn_event("UserPromptSubmit", "second", 2)));
    let current = activity.codex_failure_target().unwrap();
    assert!(activity.apply_hook(&codex_verified_turn_event("PermissionRequest", "second", 3)));
    assert!(!activity.observe_codex_terminal(&current, &CodexTerminalEvidence::Finished {}));
    activity.command_finished();
    assert!(!activity.observe_codex_terminal(&current, &CodexTerminalEvidence::Cancelled {}));
}

#[test]
fn wsl_hook_codex_structured_failure_remains_failed_and_rejects_empty_errors() {
    let mut activity = AgentActivity::default();
    assert!(activity.apply_hook(&codex_verified_turn_event("UserPromptSubmit", "first", 1)));
    let target = activity.codex_failure_target().unwrap();
    assert!(!activity.observe_codex_terminal(
        &target,
        &CodexTerminalEvidence::Failed { error: "  ".into() },
    ));
    assert_eq!(activity.status(), AgentStatus::Working);
    assert!(activity.observe_codex_terminal(
        &target,
        &CodexTerminalEvidence::Failed { error: "bounded terminal failure".into() },
    ));
    assert_eq!(activity.status(), AgentStatus::Failed);
    assert!(!activity.observe_codex_terminal(&target, &CodexTerminalEvidence::Finished {}));
    assert!(!activity.apply_hook(&codex_verified_turn_event("Stop", "first", 2)));
}

fn claude_turn_event(name: &str, prompt: &str, sequence: u64, trigger: &str) -> AiHookEvent {
    let payload = serde_json::json!({
        "hook_event_name": name, "session_id": "compact-session", "prompt_id": prompt,
        "source": "compact", "trigger": trigger, "bridge_sequence": sequence,
    });
    let wire = format!("nebula-hook/1 source=claude process=4100:70\n{payload}");
    parse_remote_envelope(wire.as_bytes(), Some(1)).expect("supported Claude lifecycle event")
}

fn claude_event_for_session(
    name: &str,
    session: &str,
    prompt: Option<&str>,
    bridge_sequence: u64,
    received_sequence: u64,
) -> AiHookEvent {
    let mut payload = serde_json::json!({
        "hook_event_name": name,
        "session_id": session,
        "bridge_sequence": bridge_sequence,
    });
    if let Some(prompt) = prompt {
        payload["prompt_id"] = serde_json::json!(prompt);
    }
    let wire = format!("nebula-hook/1 source=claude process=4100:70\n{payload}");
    let mut event =
        parse_remote_envelope(wire.as_bytes(), Some(1)).expect("supported Claude lifecycle event");
    event.received_sequence = received_sequence;
    event
}

#[test]
fn claude_hook_queued_before_input_is_rejected_by_the_ingress_fence() {
    let mut activity = AgentActivity::default();
    assert!(activity.apply_hook(&claude_event_for_session("SessionStart", "session", None, 1, 1,)));
    assert!(activity.apply_hook(&claude_event_for_session(
        "UserPromptSubmit",
        "session",
        Some("turn-a"),
        2,
        2,
    )));

    activity.input_sent_at(10);
    let stale = claude_event_for_session("Stop", "session", Some("turn-a"), 3, 9);
    assert!(!activity.accepts_hook(&stale));
    let current = claude_event_for_session("Stop", "session", Some("turn-a"), 4, 10);
    assert!(activity.accepts_hook(&current));
}

#[test]
fn claude_prompt_ids_are_deduplicated_for_one_session_and_reset_for_a_new_one() {
    let mut activity = AgentActivity::default();
    assert!(activity.apply_hook(&claude_event_for_session(
        "SessionStart",
        "session-a",
        None,
        1,
        1,
    )));
    assert!(activity.apply_hook(&claude_event_for_session(
        "UserPromptSubmit",
        "session-a",
        Some("reused-prompt"),
        2,
        2,
    )));
    assert!(activity.apply_hook(&claude_event_for_session(
        "UserPromptSubmit",
        "session-a",
        Some("second-prompt"),
        3,
        3,
    )));
    assert!(!activity.apply_hook(&claude_event_for_session(
        "UserPromptSubmit",
        "session-a",
        Some("reused-prompt"),
        4,
        4,
    )));

    assert!(activity.apply_hook(&claude_event_for_session(
        "SessionStart",
        "session-b",
        None,
        5,
        5,
    )));
    assert!(activity.apply_hook(&claude_event_for_session(
        "UserPromptSubmit",
        "session-b",
        Some("reused-prompt"),
        6,
        6,
    )));
}

#[test]
fn claude_style_agents_reject_queued_and_unacknowledged_stops_after_submission() {
    for source in ["claude", "qoder", "codebuddy", "qwen", "droid"] {
        for has_prompt_id in [false, true] {
            let parse = |name: &str, prompt: &str| {
                let mut payload = serde_json::json!({
                    "hook_event_name": name, "session_id": "style-session",
                });
                if has_prompt_id {
                    payload["prompt_id"] = serde_json::json!(prompt);
                }
                let wire = format!("nebula-hook/1 source={source}\n{payload}");
                parse_remote_envelope(wire.as_bytes(), Some(1)).unwrap()
            };
            let mut activity = AgentActivity::default();
            assert!(activity.apply_hook(&parse("SessionStart", "")));
            assert!(activity.apply_hook(&parse("UserPromptSubmit", "old")));
            let queued_stop = parse("Stop", "old");
            activity.input_sent();
            assert!(!activity.apply_hook(&queued_stop), "{source}: input fences queued Stops");
            activity.submitted();
            assert!(!activity.apply_hook(&parse("Stop", "old")), "{source}: await new prompt");
            assert_eq!(activity.status(), AgentStatus::Working);
            assert!(activity.apply_hook(&parse("UserPromptSubmit", "new")));
            assert!(activity.apply_hook(&parse("Stop", "new")));
            assert_eq!(activity.status(), AgentStatus::Done);
        }
    }
}

#[test]
fn claude_style_prompt_ids_are_session_scoped_and_reject_old_turn_results() {
    for source in ["qoder", "codebuddy", "qwen", "droid"] {
        let parse = |name: &str, session: &str, prompt: &str| {
            let payload = serde_json::json!({
                "hook_event_name": name, "session_id": session, "prompt_id": prompt,
            });
            let wire = format!("nebula-hook/1 source={source} process=4100:70\n{payload}");
            let event = parse_remote_envelope(wire.as_bytes(), Some(1)).unwrap();
            if !prompt.is_empty() {
                assert_eq!(event.turn_id.as_deref(), Some(prompt));
            }
            event
        };
        let mut activity = AgentActivity::default();
        assert!(activity.apply_hook(&parse("SessionStart", "first", "")));
        assert!(activity.apply_hook(&parse("UserPromptSubmit", "first", "one")));
        assert!(activity.apply_hook(&parse("UserPromptSubmit", "first", "two")));
        assert!(!activity.apply_hook(&parse("UserPromptSubmit", "first", "one")));
        assert!(!activity.apply_hook(&parse("Stop", "first", "one")));
        assert!(activity.apply_hook(&parse("StopFailure", "first", "two")));
        assert_eq!(activity.status(), AgentStatus::Failed);
        assert!(!activity.apply_hook(&parse("Stop", "first", "two")));
        assert!(activity.apply_hook(&parse("SessionStart", "second", "")));
        assert!(activity.apply_hook(&parse("UserPromptSubmit", "second", "one")));
    }
}

#[test]
fn unordered_native_lifecycles_fence_old_completions_and_accept_new_work() {
    for source in ["kimi", "copilot", "grok", "cursor", "antigravity"] {
        let parse = |name: &str| {
            let wire = if source == "kimi" {
                let name = match name {
                    "session-start" => "SessionStart",
                    "prompt" => "UserPromptSubmit",
                    "done" => "Stop",
                    _ => unreachable!(),
                };
                format!(
                    "nebula-hook/1 source=kimi\n{{\"hook_event_name\":\"{name}\",\"session_id\":\"fenced\"}}"
                )
            } else {
                format!(
                    "nebula-hook/1 source={source} event={name}\n{{\"conversationId\":\"fenced\",\"conversation_id\":\"fenced\",\"sessionId\":\"fenced\",\"status\":\"completed\",\"stopReason\":\"end_turn\",\"terminationReason\":\"model_stop\",\"fullyIdle\":true}}"
                )
            };
            parse_remote_envelope(wire.as_bytes(), Some(1)).unwrap()
        };
        let start_event = if source == "antigravity" { "tool-complete" } else { "prompt" };
        let mut activity = AgentActivity::default();
        assert!(activity.apply_hook(&parse("session-start")));
        assert!(activity.apply_hook(&parse(start_event)));
        let queued_stop = parse("done");
        activity.input_sent();
        assert!(!activity.apply_hook(&queued_stop), "{source}: queued completion is stale");
        assert_eq!(activity.status(), AgentStatus::Working);
        let queued_start = parse(start_event);
        activity.submitted();
        assert!(!activity.apply_hook(&queued_start), "{source}: old work cannot release input");
        assert!(!activity.apply_hook(&parse("done")), "{source}: await fresh work");
        assert!(
            activity.apply_hook(&parse(start_event)),
            "{source}: fresh work acknowledges input"
        );
        assert!(activity.apply_hook(&parse("done")));
        assert_eq!(activity.status(), AgentStatus::Done, "{source}");
    }
}

#[test]
fn legacy_codex_notify_fences_queued_completion_without_waiting_for_a_prompt_hook() {
    let notify = || {
        parse_remote_envelope(
            b"nebula-hook/1 source=codex\n{\"type\":\"agent-turn-complete\",\"thread-id\":\"legacy-session\"}",
            Some(1),
        )
        .unwrap()
    };
    let mut activity = AgentActivity::default();
    assert!(activity.apply_hook(&notify()));
    let queued = notify();
    activity.submitted_text("codex", "new task");
    assert!(!activity.apply_hook(&queued));
    assert_eq!(activity.status(), AgentStatus::Working);
    assert!(activity.pending_submit);
    assert!(!activity.pending_hook_prompt, "legacy notify has no prompt callback");
    assert!(activity.apply_hook(&notify()), "fresh completion remains authoritative");
    assert_eq!(activity.status(), AgentStatus::Done);
    assert!(!activity.pending_submit);
}

#[test]
fn sequenced_bridges_keep_their_existing_submission_acknowledgements() {
    for source in ["opencode", "pi", "omp"] {
        assert!(!super::super::capabilities_for(source).needs_input_fence());
    }
    for mode in [super::super::CodexHookMode::Turns, super::super::CodexHookMode::Full] {
        let mut event = codex_verified_turn_event("UserPromptSubmit", "next", 1);
        event.codex_hooks = Some(mode);
        assert!(event.capabilities().needs_input_fence());
    }
}

fn runtime_prompt_hook(program: &str, name: &str, turn: &str, sequence: u64) -> AiHookEvent {
    match program {
        "codex" => codex_verified_turn_event(name, turn, sequence),
        "claude" => claude_turn_event(name, turn, sequence, ""),
        _ => unreachable!("runtime prompt fixture provider"),
    }
}

#[test]
fn runtime_prompt_pending_rejects_old_stop_before_new_start() {
    for program in ["codex", "claude"] {
        let mut activity = AgentActivity::default();
        assert!(activity.apply_hook(&runtime_prompt_hook(program, "UserPromptSubmit", "A", 1)));
        activity.submitted_text(program, "new task B");
        let uncertainty = activity.uncertainty();
        assert!(activity.pending_submit);
        assert!(uncertainty.is_some());

        assert!(
            !activity.apply_hook(&runtime_prompt_hook(program, "Stop", "A", 2)),
            "{program}: old Stop A cannot acknowledge the pending runtime prompt B"
        );
        assert_eq!(activity.status(), AgentStatus::Working, "{program}");
        assert_eq!(activity.source(), AgentStatusSource::Process, "{program}");
        assert_eq!(activity.uncertainty(), uncertainty, "{program}");
        assert!(activity.pending_submit, "{program}");
    }
}

#[test]
fn runtime_prompt_pending_does_not_reopen_a_completed_turn() {
    for program in ["codex", "claude"] {
        let mut activity = AgentActivity::default();
        assert!(activity.apply_hook(&runtime_prompt_hook(program, "UserPromptSubmit", "A", 1)));
        assert!(activity.apply_hook(&runtime_prompt_hook(program, "Stop", "A", 2)));
        assert_eq!(activity.status(), AgentStatus::Done);
        assert!(activity.uncertainty().is_none());
        activity.submitted_text(program, "new task B");
        let uncertainty = activity.uncertainty();
        assert!(activity.pending_submit);
        assert!(uncertainty.is_some());

        assert!(
            !activity.apply_hook(&runtime_prompt_hook(program, "Stop", "A", 3)),
            "{program}: changing Done to pending Working must not reopen completed A"
        );
        assert_eq!(activity.status(), AgentStatus::Working, "{program}");
        assert_eq!(activity.uncertainty(), uncertainty, "{program}");
        assert!(activity.pending_submit, "{program}");
    }
}

#[test]
fn runtime_prompt_pending_accepts_the_new_turn_and_its_completion() {
    for program in ["codex", "claude"] {
        let mut activity = AgentActivity::default();
        assert!(activity.apply_hook(&runtime_prompt_hook(program, "UserPromptSubmit", "A", 1)));
        activity.submitted_text(program, "new task B");
        assert!(activity.pending_submit);
        assert!(activity.uncertainty().is_some());

        assert!(activity.apply_hook(&runtime_prompt_hook(program, "UserPromptSubmit", "B", 2)));
        assert!(!activity.pending_submit, "{program}");
        assert_eq!(activity.status(), AgentStatus::Working, "{program}");
        assert!(activity.uncertainty().is_none(), "{program}");
        assert!(!activity.apply_hook(&runtime_prompt_hook(program, "Stop", "A", 3)));
        assert!(activity.apply_hook(&runtime_prompt_hook(program, "Stop", "B", 4)));
        assert_eq!(activity.status(), AgentStatus::Done, "{program}");
        assert!(activity.uncertainty().is_none(), "{program}");
    }
}

#[test]
fn runtime_prompt_pending_keeps_current_turn_permission_and_tool_hooks_valid() {
    for program in ["codex", "claude"] {
        let mut activity = AgentActivity::default();
        assert!(activity.apply_hook(&runtime_prompt_hook(program, "UserPromptSubmit", "A", 1)));
        activity.submitted_text(program, "new task B");
        assert!(activity.apply_hook(&runtime_prompt_hook(program, "UserPromptSubmit", "B", 2)));
        assert!(activity.apply_hook(&runtime_prompt_hook(program, "PermissionRequest", "B", 3)));
        assert_eq!(activity.status(), AgentStatus::Blocked, "{program}");
        assert!(!activity.pending_submit, "{program}");

        // Confirmation buttons write their reply without declaring another submitted prompt.
        assert!(activity.apply_hook(&runtime_prompt_hook(program, "PostToolUse", "B", 4)));
        assert_eq!(activity.status(), AgentStatus::Working, "{program}");
        assert!(activity.uncertainty().is_none(), "{program}");
        assert!(activity.apply_hook(&runtime_prompt_hook(program, "Stop", "B", 5)));
        assert_eq!(activity.status(), AgentStatus::Done, "{program}");
        assert!(activity.uncertainty().is_none(), "{program}");
    }
}

#[test]
fn runtime_prompt_pending_rejects_old_or_unidentified_hooks_until_new_prompt() {
    for program in ["codex", "claude"] {
        let mut activity = AgentActivity::default();
        assert!(activity.apply_hook(&runtime_prompt_hook(program, "UserPromptSubmit", "A", 1)));
        activity.submitted_text(program, "task B");
        for name in ["Stop", "PostToolUse", "PermissionRequest", "SessionStart", "UserPromptSubmit"]
        {
            for turn in [Some("A"), None] {
                let mut old = runtime_prompt_hook(program, name, "A", 2);
                old.turn_id = turn.map(str::to_owned);
                assert!(!activity.apply_hook(&old), "{program} {name} {turn:?}");
                assert!(activity.uncertainty().is_some());
                assert!(activity.pending_submit);
            }
        }
        assert!(activity.apply_hook(&runtime_prompt_hook(program, "UserPromptSubmit", "B", 3)));
        assert!(activity.uncertainty().is_none());
    }
}

#[test]
fn codex_terminal_probe_cannot_end_authoritative_background_work() {
    let mut activity = AgentActivity::default();
    assert!(activity.apply_hook(&codex_verified_turn_event("UserPromptSubmit", "A", 1)));
    let target = activity.codex_failure_target().unwrap();
    let wire = b"nebula-hook/1 source=codex process=20:200 codex_hooks=full\n{\"hook_event_name\":\"Stop\",\"session_id\":\"session\",\"turn_id\":\"A\",\"bridge_sequence\":2,\"background_tasks\":[{\"type\":\"local_bash\",\"status\":\"running\"}]}";
    let stop = parse_remote_envelope(wire, Some(1)).unwrap();
    assert_eq!(stop.active_background_tasks(), 1, "fixture must prove background work first");
    assert!(activity.apply_hook(&stop));
    assert_eq!(activity.status(), AgentStatus::Working);
    assert!(!activity.observe_codex_terminal(&target, &CodexTerminalEvidence::Finished {}));
    assert_eq!(activity.status(), AgentStatus::Working);
    assert_eq!(activity.source(), AgentStatusSource::Hook);
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
            for _ in 0..6 {
                assert!(!activity.observe_screen(None));
                assert_eq!(activity.status(), status, "{provider}/{name}");
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
    // Empty Codex chrome never separates turns, even after repeated redraws.
    for _ in 0..6 {
        activity.observe_screen(screen(AgentStatus::Idle));
    }
    assert!(!activity.observe_screen(screen(AgentStatus::Working)));
    assert_eq!(activity.status(), AgentStatus::Done);
    activity.input_sent();
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
        if agent == AgentKind::Codex {
            assert_eq!(activity.status(), AgentStatus::Unknown);
            assert!(activity.uncertainty().is_some());
            assert!(activity.observe_screen(screen(AgentStatus::Working)));
            for _ in 0..4 {
                activity.observe_screen(screen(AgentStatus::Idle));
            }
            assert_eq!(activity.status(), AgentStatus::Working);
            assert!(activity.uncertainty().is_some());
            assert_eq!(screen_notification(AgentStatus::Working, activity.status(), false), None);
            continue;
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
fn claude_form_replaced_by_current_working_chrome_clears_attention() {
    let mut activity = AgentActivity::default();
    activity.submitted();
    let form = "Do you want to proceed?\n❯ 1. Yes\n2. No\nEsc to cancel · Tab to amend";
    assert!(activity.observe_screen(crate::ai_agents::detect("claude", form)));
    assert_eq!(activity.status(), AgentStatus::Blocked);
    let running = "* Ideating… (2m · 123 tokens)\n────────\n❯\n────────\n版本 2.1.301\n上下文 25%\n状态 运行中\n/project";
    let detection = crate::ai_agents::detect("claude", running).unwrap();
    assert_eq!(detection.status, AgentStatus::Working);
    assert!(activity.observe_screen(Some(detection)));
    assert_eq!(activity.status(), AgentStatus::Working);
    assert_eq!(activity.source(), AgentStatusSource::Screen);
    assert_eq!(screen_notification(AgentStatus::Blocked, activity.status(), false), None);
    for _ in 0..8 {
        activity.observe_screen(None);
        assert_eq!(activity.status(), AgentStatus::Working, "silence cannot finish a turn");
        assert!(activity.uncertainty().is_some(), "unclassified output is not confirmation");
    }
}

fn claude_screen(status: AgentStatus) -> Option<Detection> {
    Some(Detection { agent: AgentKind::Claude, status, rule_id: format!("test.{status:?}") })
}

#[test]
fn expired_screen_attention_restores_work_evidence_including_partial_hooks() {
    for source in [AgentStatusSource::Process, AgentStatusSource::Screen, AgentStatusSource::Hook] {
        let mut activity = AgentActivity::default();
        match source {
            AgentStatusSource::Process => activity.submitted(),
            AgentStatusSource::Screen => {
                activity.observe_screen(claude_screen(AgentStatus::Working));
            },
            AgentStatusSource::Hook => {
                activity.apply_hook(&event("pi", "partial", "UserPromptSubmit"));
            },
            _ => unreachable!(),
        }
        let rule = activity.rule().map(str::to_owned);
        activity.observe_screen(claude_screen(AgentStatus::Blocked));
        assert!(!activity.observe_screen(None));
        assert!(activity.observe_screen(None));
        assert_eq!(activity.status(), AgentStatus::Working);
        assert_eq!(activity.source(), source);
        assert_eq!(activity.rule(), rule.as_deref());
        for _ in 0..2 {
            activity.observe_screen(claude_screen(AgentStatus::Idle));
        }
        assert_eq!(
            activity.status(),
            if source == AgentStatusSource::Hook {
                AgentStatus::Working
            } else {
                AgentStatus::Idle
            },
            "real idle evidence still respects completion ownership"
        );
    }
}

#[test]
fn expired_screen_attention_without_work_evidence_becomes_unknown() {
    let mut activity = AgentActivity::default();
    activity.observe_screen(claude_screen(AgentStatus::Blocked));
    assert!(!activity.observe_screen(None));
    assert!(activity.observe_screen(None));
    assert_eq!(activity.status(), AgentStatus::Unknown);
    assert_eq!(activity.source(), AgentStatusSource::Unknown);
    assert_eq!(activity.rule(), None);
    assert!(!activity.turn_observed, "an expired form is not a submitted turn");
    assert_eq!(screen_notification(AgentStatus::Blocked, activity.status(), false), None);
}

#[test]
fn live_forms_and_other_screen_evidence_interrupt_unmatched_samples() {
    let mut activity = AgentActivity::default();
    activity.submitted();
    activity.observe_screen(claude_screen(AgentStatus::Blocked));
    for _ in 0..6 {
        assert!(!activity.observe_screen(None));
        assert!(!activity.observe_screen(claude_screen(AgentStatus::Blocked)));
        assert_eq!(activity.status(), AgentStatus::Blocked);
    }
    assert!(!activity.observe_screen(None));
    assert!(!activity.observe_screen(claude_screen(AgentStatus::Idle)));
    assert!(!activity.observe_screen(None), "the idle sample interrupted absence evidence");
    assert_eq!(activity.status(), AgentStatus::Blocked);
    assert!(activity.observe_screen(None));
    assert_eq!(activity.status(), AgentStatus::Working);
}

#[test]
fn input_and_unidentified_snapshots_interrupt_attention_expiry() {
    for input in [false, true] {
        let mut activity = AgentActivity::default();
        activity.submitted();
        activity.observe_screen(claude_screen(AgentStatus::Blocked));
        assert!(!activity.observe_screen(None));
        if input {
            activity.input_sent();
        } else {
            activity.reset_screen_samples();
        }
        assert!(!activity.observe_screen(None));
        assert_eq!(activity.status(), AgentStatus::Blocked);
        assert!(activity.observe_screen(None));
        assert_eq!(activity.status(), AgentStatus::Working);
    }
}

#[test]
fn new_work_and_command_boundaries_discard_previous_attention_evidence() {
    for boundary in ["submit", "hook", "command"] {
        let mut activity = AgentActivity::default();
        activity.observe_screen(claude_screen(AgentStatus::Working));
        activity.observe_screen(claude_screen(AgentStatus::Blocked));
        activity.observe_screen(None);
        match boundary {
            "submit" => activity.submitted(),
            "hook" => {
                assert!(activity.apply_hook(&event("pi", "new-work", "UserPromptSubmit")));
            },
            "command" => {
                activity.command_finished();
                activity.observe_screen(None);
                assert_eq!(activity.status(), AgentStatus::Unknown);
                activity.begin_command(true);
            },
            _ => unreachable!(),
        }
        activity.observe_screen(claude_screen(AgentStatus::Blocked));
        assert!(!activity.observe_screen(None));
        assert!(activity.observe_screen(None));
        assert_eq!(
            activity.status(),
            if boundary == "command" { AgentStatus::Unknown } else { AgentStatus::Working }
        );
        assert_eq!(
            activity.source(),
            match boundary {
                "hook" => AgentStatusSource::Hook,
                "command" => AgentStatusSource::Unknown,
                _ => AgentStatusSource::Process,
            }
        );
        assert_eq!(activity.rule(), None, "old screen rule cannot survive new work");
        if boundary == "command" {
            for _ in 0..2 {
                activity.observe_screen(claude_screen(AgentStatus::Idle));
            }
            assert_eq!(activity.status(), AgentStatus::Idle, "launching is not a submitted turn");
        }
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
        let expected = if agent == AgentKind::Codex {
            assert!(activity.uncertainty().is_some());
            AgentStatus::Working
        } else if agent == AgentKind::Claude {
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
            ("Completed answer.", preceding),
            (
                "■ stream disconnected before completion: failed to send request",
                AgentStatus::Failed,
            ),
            ("■ Rate limit exceeded", AgentStatus::Failed),
            ("■ 401 Unauthorized", AgentStatus::Failed),
            ("■ 503 Service Unavailable", AgentStatus::Failed),
            ("■ Turn interrupted", preceding),
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
        let status = activity.status();
        activity.submitted_text("codex", "/plan");
        assert_eq!(activity.status(), status);
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

/// A stalled remote Hook owner cannot be completed by repeated idle screens.
#[test]
fn stalled_turn_reconciliation_preserves_hook_authority_after_idle_samples() {
    let mut activity = AgentActivity::default();
    activity.apply_hook(&event("claude", "stall", "UserPromptSubmit"));
    assert_eq!(activity.status(), AgentStatus::Working);
    assert!(!activity.turn_quiet_for(std::time::Duration::from_secs(20)));
    activity.backdate_activity_for_test(std::time::Duration::from_secs(21));
    assert!(activity.turn_quiet_for(std::time::Duration::from_secs(20)));
    // 未停滞 / 非 idle 检测 / 不足两拍：全部不动。
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), false));
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Working).as_ref(), true));
    assert!(!activity.reconcile_stalled_turn(None, true));
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), true));
    assert_eq!(activity.status(), AgentStatus::Working);
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), true));
    assert_eq!(activity.status(), AgentStatus::Working);
    assert_eq!(activity.source(), AgentStatusSource::Hook);
}

/// 无 hook 权威（allows_screen 为真）时对账让位给 observe_screen；会话仅
/// SessionStart（status 已是 Idle）时无事可做，不得伪造一次迁移。
#[test]
fn stall_reconciliation_yields_to_screen_authority_and_reads_turn_observed() {
    let mut activity = AgentActivity::default();
    // 无 hook：allows_screen() 为真 → 恒 false。
    activity.begin_command(true);
    activity.backdate_activity_for_test(std::time::Duration::from_secs(60));
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), true));
    assert_eq!(activity.status(), AgentStatus::Unknown);
    // 只有 SessionStart 的会话已经在 Idle 上，没有可完成的回合。
    let mut idle_only = AgentActivity::default();
    idle_only.apply_hook(&event("claude", "fresh", "SessionStart"));
    idle_only.backdate_activity_for_test(std::time::Duration::from_secs(60));
    assert!(!idle_only.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), true));
    assert!(!idle_only.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), true));
    assert_eq!(idle_only.status(), AgentStatus::Idle);
    assert_eq!(idle_only.source(), AgentStatusSource::Hook);
}

/// 刚提交（pending_submit）的回合即使屏幕看起来 idle 也不收边——提交屏障
/// 与 observe_screen 的 idle 分支同一语义；对点火方向同样成立（屏障期间两向
/// 计数都归零，屏障解除后证据要重新攒）。
#[test]
fn stall_reconciliation_honors_the_pending_submit_barrier() {
    let mut activity = AgentActivity::default();
    activity.apply_hook(&event("claude", "pending", "UserPromptSubmit"));
    activity.submitted();
    assert_eq!(activity.status(), AgentStatus::Working);
    // hook 时间线仍在滚动（未停滞）：屏障绝对在，屏幕证据一律不算。
    for _ in 0..4 {
        assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), false));
        assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Working).as_ref(), false));
    }
    assert_eq!(activity.status(), AgentStatus::Working);
}

#[test]
fn fresh_submission_restarts_the_remote_stall_clock_after_an_old_hook() {
    let threshold = std::time::Duration::from_secs(20);
    let mut activity = AgentActivity::default();
    activity.apply_hook(&event("claude", "delayed-submit", "UserPromptSubmit"));
    activity.apply_hook(&event("claude", "delayed-submit", "Stop"));
    activity.backdate_activity_for_test(threshold * 2);
    assert!(activity.turn_quiet_for(threshold));

    activity.submitted();
    for _ in 0..4 {
        let stalled = activity.turn_quiet_for(threshold);
        assert!(!stalled, "a new submission gets its own full waiting period");
        assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), stalled));
        assert!(activity.pending_submit);
        assert_eq!(activity.status(), AgentStatus::Working);
    }

    // Time and idle text cannot release the explicit submission barrier.
    activity.backdate_activity_for_test(threshold * 2);
    let stalled = activity.turn_quiet_for(threshold);
    assert!(stalled);
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), stalled));
    assert!(activity.pending_submit);
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), stalled));
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), stalled));
    assert_eq!(activity.status(), AgentStatus::Working);

    activity.submitted_text("claude", "next task");
    assert!(!activity.turn_quiet_for(threshold), "each submission renews the boundary");
    activity.backdate_activity_for_test(threshold * 2);
    assert!(activity.apply_hook(&event("claude", "delayed-submit", "UserPromptSubmit")));
    assert!(!activity.turn_quiet_for(threshold), "accepted hooks also renew the boundary");
    activity.backdate_activity_for_test(threshold * 2);
    assert!(!activity.apply_hook(&event("claude", "foreign-session", "UserPromptSubmit")));
    assert!(activity.turn_quiet_for(threshold), "foreign hooks cannot defer recovery");
}

/// A lost remote submission retains its barrier while the Hook is silent.
#[test]
fn pending_submit_barrier_survives_hooks_staying_silent_past_the_stall() {
    let mut activity = AgentActivity::default();
    activity.apply_hook(&event("claude", "lost-submit", "UserPromptSubmit"));
    activity.submitted();
    activity.backdate_activity_for_test(std::time::Duration::from_secs(21));
    let stalled = activity.turn_quiet_for(std::time::Duration::from_secs(20));
    assert!(stalled);
    assert_eq!(activity.status(), AgentStatus::Working);
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), stalled));
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), stalled));
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Working).as_ref(), stalled));
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), stalled));
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), stalled));
    assert!(activity.pending_submit);
    assert_eq!(activity.status(), AgentStatus::Working);
    assert_eq!(activity.source(), AgentStatusSource::Process);
}

/// A working frame followed by idle text still cannot complete a lost submission.
#[test]
fn silent_submit_barrier_rejects_a_later_idle_screen_completion() {
    let mut activity = AgentActivity::default();
    activity.apply_hook(&event("claude", "lost-submit-2", "UserPromptSubmit"));
    activity.submitted();
    activity.backdate_activity_for_test(std::time::Duration::from_secs(21));
    let stalled = activity.turn_quiet_for(std::time::Duration::from_secs(20));
    assert!(stalled);
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Working).as_ref(), stalled));
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Working).as_ref(), stalled));
    assert_eq!(activity.status(), AgentStatus::Working);
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), stalled));
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), stalled));
    assert_eq!(activity.status(), AgentStatus::Working);
    assert_eq!(activity.source(), AgentStatusSource::Process);
}

/// Only the delayed authoritative TurnDone may complete the stalled turn.
#[test]
fn late_authoritative_turn_done_completes_the_stalled_turn_once() {
    let mut activity = AgentActivity::default();
    activity.apply_hook(&event_with_turn("claude", "t", "UserPromptSubmit", "t-9"));
    activity.backdate_activity_for_test(std::time::Duration::from_secs(21));
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), true));
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), true));
    assert_eq!(activity.status(), AgentStatus::Working);
    // 迟到的成功 TurnDone：被采纳，来源翻正为 hook。
    assert!(activity.apply_hook(&event_with_turn("claude", "t", "Stop", "t-9")));
    assert_eq!(activity.status(), AgentStatus::Done);
    assert_eq!(activity.source(), AgentStatusSource::Hook);
    // 权威完成后的同 turn 重放照旧被去重护栏拦下。
    assert!(!activity.apply_hook(&event_with_turn("claude", "t", "Stop", "t-9")));
}

/// The delayed failed Hook retains the explicit failure used by auto-continue.
#[test]
fn late_failed_turn_done_marks_the_stalled_turn_failed() {
    let mut activity = AgentActivity::default();
    activity.apply_hook(&event_with_turn("claude", "f", "UserPromptSubmit", "f-1"));
    activity.backdate_activity_for_test(std::time::Duration::from_secs(21));
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), true));
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), true));
    assert_eq!(activity.status(), AgentStatus::Working);
    let mut failed = event_with_turn("claude", "f", "Stop", "f-1");
    failed.turn_outcome = crate::ai_hook::AiTurnOutcome::Failed;
    assert!(activity.apply_hook(&failed));
    assert_eq!(activity.status(), AgentStatus::Failed, "a failed turn is not a ready turn");
    assert_eq!(activity.source(), AgentStatusSource::Hook);
}

/// 对账的反方向：UserPromptSubmit 在远端 bridge 丢失（通道锁 busy/超时/超限）
/// 时，上一回合的 Stop 把回合钉在 Done；hook 静默 + 连续两拍转圈重新点火
/// Working，并补记 turn_observed（迟到的 Stop 能收敛为 Done 而非 Idle）。
#[test]
fn stalled_turn_reconciliation_reignites_a_lost_prompt_submit() {
    let mut activity = AgentActivity::default();
    activity.apply_hook(&event("claude", "lost", "UserPromptSubmit"));
    activity.apply_hook(&event("claude", "lost", "Stop"));
    assert_eq!(activity.status(), AgentStatus::Done);
    activity.backdate_activity_for_test(std::time::Duration::from_secs(21));
    // 一拍转圈不点火；中途一拍 idle 归零 working 计数，证据重新攒。
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Working).as_ref(), true));
    assert_eq!(activity.status(), AgentStatus::Done);
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), true));
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Working).as_ref(), true));
    assert!(activity.reconcile_stalled_turn(screen(AgentStatus::Working).as_ref(), true));
    assert_eq!(activity.status(), AgentStatus::Working);
    assert_eq!(activity.source(), AgentStatusSource::Screen);
    assert_eq!(activity.rule(), Some("test.Working"));
    // Renewed work still requires an authoritative completion.
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), true));
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Idle).as_ref(), true));
    assert_eq!(activity.status(), AgentStatus::Working);
    assert!(activity.apply_hook(&event("claude", "lost", "Stop")));
    assert_eq!(activity.status(), AgentStatus::Done);
}

/// 权限答复（PostToolUse/PermissionResult）在远端丢失时回合钉在 Blocked；
/// 用户已答复、屏幕回到转圈，两拍 working 同样重新点火。权限屏本身
/// （Blocked 检测）不参与对账——attention 语义归 hook 与 observe_screen 管。
#[test]
fn stalled_turn_reconciliation_reignites_a_blocked_turn_after_the_answer() {
    let mut activity = AgentActivity::default();
    activity.apply_hook(&event("claude", "blocked", "UserPromptSubmit"));
    activity.apply_hook(&event("claude", "blocked", "PermissionRequest"));
    assert_eq!(activity.status(), AgentStatus::Blocked);
    activity.backdate_activity_for_test(std::time::Duration::from_secs(21));
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Blocked).as_ref(), true));
    assert!(!activity.reconcile_stalled_turn(screen(AgentStatus::Working).as_ref(), true));
    assert!(activity.reconcile_stalled_turn(screen(AgentStatus::Working).as_ref(), true));
    assert_eq!(activity.status(), AgentStatus::Working);
    assert_eq!(activity.source(), AgentStatusSource::Screen);
}
