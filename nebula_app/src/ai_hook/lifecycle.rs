//! One pane's Agent lifecycle, shared by both UI shells.
//!
//! Hook events are facts; screen matches are observations with limited authority.
//! Command ownership ends at the shell boundary, never because output goes quiet.

use std::collections::HashSet;

use crate::ai_agents::{
    AgentKind, AgentStatus, AgentStatusSource, AgentStatusUncertainty, Detection,
};

use super::protocol::claude_style;
use super::{AiHookEvent, AiHookKind};

/// A stop from these sources proves nothing about the answer unless it carries a result, so
/// an unclassified one returns the pane to idle instead of announcing completion.
const UNPROVEN_STOP: [&str; 5] = ["pi", "omp", "cursor", "copilot", "antigravity"];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum HookCoverage {
    #[default]
    None,
    Completion,
    Turns,
    Lifecycle,
}

#[derive(Debug, Clone)]
struct Owner {
    source: String,
    session: Option<String>,
    pid: Option<u32>,
    start_time: Option<u64>,
    remote_process: Option<String>,
    bridge_instance: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BackgroundShellRest {
    source: String,
    session: String,
    turn: String,
    pid: Option<u32>,
    start_time: Option<u64>,
    remote_process: Option<String>,
    shell_ids: Option<Vec<String>>,
}

impl BackgroundShellRest {
    pub(crate) fn shell_ids(&self) -> Option<&[String]> {
        self.shell_ids.as_deref()
    }
}

#[derive(Debug, Clone)]
struct Compaction {
    prompt_id: Option<String>,
    resume_status: AgentStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexFailureTarget {
    pub session: String,
    pub turn: String,
    pub process: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellHookOwner {
    pub source: String,
    pub process: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellOwnerStatus {
    Alive,
    Ended,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CodexTerminalEvidence {
    // Empty struct variants also enforce deny_unknown_fields.
    Finished {},
    Cancelled {},
    Failed { error: String },
}

/// Owns status and arbitration. Adapters may submit facts but cannot set fields.
#[derive(Debug, Clone)]
pub(crate) struct AgentActivity {
    status: AgentStatus,
    source: AgentStatusSource,
    rule: Option<String>,
    uncertainty: Option<AgentStatusUncertainty>,
    coverage: HookCoverage,
    owner: Option<Owner>,
    background_shell_rest: Option<BackgroundShellRest>,
    background_tasks_active: bool,
    /// Screen-only idle confirmation and remote renewed-work confirmation
    /// have separate counters; neither can complete a Hook-owned turn.
    idle_samples: u8,
    working_samples: u8,
    unmatched_samples: u8,
    /// Only a preceding Working state may survive a screen form's expiry.
    screen_blocked_fallback: Option<(AgentStatusSource, Option<String>)>,
    screen_blocked_agent: Option<AgentKind>,
    turn_observed: bool,
    pending_submit: bool,
    pending_hook_prompt: bool,
    /// A completion-only hook latches its result until an idle screen or an
    /// explicit submission separates the old frame from a new turn.
    screen_armed: bool,
    command_ended: bool,
    verified_foreground: bool,
    native_codex: bool,
    turn_id: Option<String>,
    /// Retain every Claude-style prompt identity for the current native session so
    /// a delayed Hook cannot become valid after later turns replace `turn_id`.
    claude_style_prompt_ids: HashSet<String>,
    /// Reject Claude events that entered a queue before the most recent input.
    submit_receive_sequence: u64,
    last_remote_sequence: Option<u64>,
    compaction: Option<Compaction>,
    retired_compaction: Option<String>,
    cancel_requested: bool,
    turn_cancelled: bool,
    retired_codex_failure: Option<CodexFailureTarget>,
    retired_codex_idle: Option<CodexFailureTarget>,
    codex_input_pending: bool,
    last_activity_at: Option<std::time::Instant>,
}

impl Default for AgentActivity {
    fn default() -> Self {
        Self {
            status: AgentStatus::Unknown,
            source: AgentStatusSource::Unknown,
            rule: None,
            uncertainty: None,
            coverage: HookCoverage::None,
            owner: None,
            background_shell_rest: None,
            background_tasks_active: false,
            idle_samples: 0,
            working_samples: 0,
            unmatched_samples: 0,
            screen_blocked_fallback: None,
            screen_blocked_agent: None,
            turn_observed: false,
            pending_submit: false,
            pending_hook_prompt: false,
            screen_armed: true,
            command_ended: false,
            verified_foreground: false,
            native_codex: false,
            turn_id: None,
            claude_style_prompt_ids: HashSet::new(),
            submit_receive_sequence: 0,
            last_remote_sequence: None,
            compaction: None,
            retired_compaction: None,
            cancel_requested: false,
            turn_cancelled: false,
            retired_codex_failure: None,
            retired_codex_idle: None,
            codex_input_pending: false,
            last_activity_at: None,
        }
    }
}

impl AgentActivity {
    pub fn status(&self) -> AgentStatus {
        self.status
    }
    pub fn source(&self) -> AgentStatusSource {
        self.source
    }
    pub fn rule(&self) -> Option<&str> {
        self.rule.as_deref()
    }
    pub fn uncertainty(&self) -> Option<AgentStatusUncertainty> {
        self.uncertainty.or_else(|| {
            (self.status == AgentStatus::Unknown).then_some(AgentStatusUncertainty::IdentityOnly)
        })
    }
    pub fn mark_status_unconfirmed(&mut self, reason: AgentStatusUncertainty) -> bool {
        let changed = self.uncertainty != Some(reason);
        self.uncertainty = Some(reason);
        changed
    }
    pub fn confirm_status(&mut self) {
        self.uncertainty = None;
    }
    pub fn hook_seen(&self) -> bool {
        self.coverage != HookCoverage::None
    }
    pub(crate) fn background_shell_rest(&self) -> Option<&BackgroundShellRest> {
        let rest = self.background_shell_rest.as_ref()?;
        let owner = self.owner.as_ref()?;
        (self.status == AgentStatus::Working
            && self.source == AgentStatusSource::Hook
            && self.background_tasks_active
            && !self.pending_submit
            && !self.pending_hook_prompt
            && self.uncertainty.is_none()
            && !self.cancel_requested
            && !self.turn_cancelled
            && self.compaction.is_none()
            && owner.source == rest.source
            && owner.session.as_deref() == Some(rest.session.as_str())
            && owner.pid == rest.pid
            && owner.start_time == rest.start_time
            && owner.remote_process == rest.remote_process
            && self.turn_id.as_deref() == Some(rest.turn.as_str()))
        .then_some(rest)
    }
    pub(crate) fn background_tasks_active(&self) -> bool {
        self.background_tasks_active
    }
    pub(crate) fn revoke_background_shell_rest(&mut self) {
        self.background_shell_rest = None;
    }
    pub fn primary_pid(&self) -> Option<u32> {
        self.owner.as_ref().and_then(|owner| owner.pid)
    }
    pub(crate) fn shell_boundary_owner(&self) -> Option<ShellHookOwner> {
        if !matches!(self.coverage, HookCoverage::Turns | HookCoverage::Lifecycle) {
            return None;
        }
        let owner = self.owner.as_ref()?;
        Some(ShellHookOwner {
            source: owner.source.clone(),
            process: owner.remote_process.clone()?,
        })
    }
    pub fn command_ended(&self) -> bool {
        self.command_ended
    }
    pub fn allows_screen(&self) -> bool {
        (!self.command_ended || self.verified_foreground)
            && self.coverage != HookCoverage::Lifecycle
    }

    pub fn codex_failure_target(&self) -> Option<CodexFailureTarget> {
        if self.command_ended
            || !self.native_codex
            || self.pending_submit
            || self.codex_input_pending
            || self.status != AgentStatus::Working
            || self.background_tasks_active
        {
            return None;
        }
        let owner = self.owner.as_ref().filter(|owner| owner.source == "codex")?;
        let target = CodexFailureTarget {
            session: owner.session.clone()?,
            turn: self.turn_id.clone()?,
            process: owner.remote_process.clone()?,
        };
        (self.retired_codex_failure.as_ref() != Some(&target)
            && self.retired_codex_idle.as_ref() != Some(&target))
        .then_some(target)
    }

    /// An authenticated process's structured terminal error can fill a missing
    /// Codex end hook. A screen phrase or local submission has no such authority.
    pub fn observe_codex_failure(&mut self, target: &CodexFailureTarget) -> bool {
        if self.codex_failure_target().as_ref() != Some(target) {
            return false;
        }
        self.retired_codex_failure = Some(target.clone());
        self.status = AgentStatus::Failed;
        self.source = AgentStatusSource::Process;
        self.rule = Some("codex.rollout.task_complete.error".into());
        self.screen_armed = false;
        self.confirm_status();
        true
    }

    pub fn observe_codex_terminal(
        &mut self,
        target: &CodexFailureTarget,
        terminal: &CodexTerminalEvidence,
    ) -> bool {
        if self.codex_failure_target().as_ref() != Some(target) {
            return false;
        }
        match terminal {
            CodexTerminalEvidence::Failed { error } => {
                if error.trim().is_empty() {
                    return false;
                }
                return self.observe_codex_failure(target);
            },
            CodexTerminalEvidence::Finished {} | CodexTerminalEvidence::Cancelled {} => {
                self.retired_codex_idle = Some(target.clone());
                self.status = AgentStatus::Idle;
                self.rule = Some(
                    match terminal {
                        CodexTerminalEvidence::Finished {} => {
                            "codex.rollout.task_complete.finished"
                        },
                        _ => "codex.rollout.turn_aborted",
                    }
                    .into(),
                );
            },
        }
        self.source = AgentStatusSource::Process;
        self.screen_armed = false;
        self.confirm_status();
        true
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn forget_unhooked_identity(&mut self) {
        if !self.hook_seen() {
            self.reset();
        }
    }

    /// An explicit shell boundary also rejects delayed events from the old CLI.
    pub fn command_finished(&mut self) {
        self.reset();
        self.command_ended = true;
    }

    /// A provider's lifecycle Hook proves that its process can outlive a late
    /// shell edge (for example an initialization prompt or a queued 133;D).
    /// Keep the hook-owned session and let SessionEnd or process exit close it.
    pub fn defer_shell_boundary(&mut self) -> bool {
        if self.owner.is_none()
            || !matches!(self.coverage, HookCoverage::Turns | HookCoverage::Lifecycle)
        {
            return false;
        }
        self.command_ended = true;
        self.verified_foreground = false;
        self.idle_samples = 0;
        true
    }

    /// Launching an Agent is not evidence that it has answered a prompt.
    pub fn begin_command(&mut self, agent: bool) {
        self.reset();
        if agent {
            self.source = AgentStatusSource::Process;
            self.mark_status_unconfirmed(AgentStatusUncertainty::IdentityOnly);
        }
    }

    pub fn identify(&mut self, source: AgentStatusSource) {
        if !self.hook_seen() && self.status == AgentStatus::Unknown {
            self.source = source;
            self.mark_status_unconfirmed(AgentStatusUncertainty::IdentityOnly);
        }
    }

    /// A verified foreground receipt permits chrome observations without
    /// reopening the old command's Hook gate or asserting an active turn.
    pub fn identify_foreground_process(&mut self) {
        if !self.hook_seen() {
            self.verified_foreground = true;
            self.identify(AgentStatusSource::Process);
        }
    }

    /// A concrete runtime submission or accepted confirmation starts work. The
    /// previous input frame must not manufacture a completion before work starts.
    pub fn submitted(&mut self) {
        self.last_activity_at = Some(std::time::Instant::now());
        self.end_compaction();
        self.status = AgentStatus::Working;
        self.source = AgentStatusSource::Process;
        self.rule = None;
        self.screen_blocked_fallback = None;
        self.screen_blocked_agent = None;
        self.turn_observed = true;
        self.pending_submit = true;
        self.pending_hook_prompt = self.native_codex
            || self
                .owner
                .as_ref()
                .is_some_and(|owner| super::capabilities_for(&owner.source).needs_input_fence());
        self.input_sent();
    }

    pub fn submitted_text(&mut self, program: &str, text: &str) {
        // Local commands are not assistant submissions. Claude's compact hooks
        // own the phase; ordinary prompts still need the submission barrier.
        if (program == "codex" && text.trim() == "/plan")
            || (program == "claude" && text.split_whitespace().next() == Some("/compact"))
        {
            self.input_sent();
        } else {
            self.submitted();
        }
    }

    /// Physical Enter alone can also dismiss a menu. It only permits observing a
    /// new turn; it is not itself a successful submission or a completion.
    pub fn input_sent(&mut self) {
        self.input_sent_at(super::current_receive_sequence());
    }

    fn input_sent_at(&mut self, received_sequence: u64) {
        self.background_shell_rest = None;
        self.submit_receive_sequence = received_sequence;
        self.codex_input_pending |= self.native_codex && self.turn_id.is_some();
        self.cancel_requested = false;
        self.screen_armed = true;
        self.reset_screen_samples();
    }

    /// An unavailable or unidentified snapshot interrupts consecutive evidence
    /// without revoking a state. Only a known agent's real snapshot can do that.
    pub fn reset_screen_samples(&mut self) {
        self.idle_samples = 0;
        self.unmatched_samples = 0;
        self.working_samples = 0;
        self.mark_status_unconfirmed(AgentStatusUncertainty::PendingInput);
    }

    /// Check ownership before the ordering gate records this event. An ignored
    /// legacy notification must not reopen the gate's already completed stream.
    pub fn accepts_hook(&self, event: &AiHookEvent) -> bool {
        // Unordered lifecycle callbacks queued before input cannot be
        // reinterpreted as observations about the newly submitted turn.
        if (event.capabilities().needs_input_fence() || event.source == "codex")
            && event.received_sequence < self.submit_receive_sequence
        {
            return false;
        }
        if self.command_ended
            && self.coverage == HookCoverage::None
            && (event.kind != AiHookKind::SessionStart || event.session_compacted)
        {
            return false;
        }
        if event.legacy_attention
            && !matches!(self.status, AgentStatus::Working | AgentStatus::Blocked)
        {
            return false;
        }
        // Both bridges stay installed for old clients and pending hook trust.
        // Once this session proves its native bridge is active, notify cannot
        // downgrade its coverage or emit a second completion notification.
        if self.native_codex && event.source == "codex" && event.codex_hooks.is_none() {
            return false;
        }
        if claude_style(&event.source)
            && event.kind == AiHookKind::PromptSubmit
            && event
                .turn_id
                .as_deref()
                .is_some_and(|turn_id| self.claude_style_prompt_ids.contains(turn_id))
        {
            return false;
        }
        // Codex reports a session/group ID for internal work, but its concrete
        // thread is identified by the rollout transcript. An explicit null or
        // malformed transcript cannot prove that a lifecycle edge belongs to
        // this pane; the shared Codex process is not sufficient ownership.
        if event.has_unverifiable_native_transcript() {
            return false;
        }
        // A submitted prompt has no authoritative new turn yet. Old tool/end
        // events, including a repeated SessionStart, cannot acknowledge it.
        if self.pending_hook_prompt {
            let new_session = event.kind == AiHookKind::SessionStart
                && !event.session_compacted
                && event.session_id.is_some()
                && event.session_id.as_ref()
                    != self.owner.as_ref().and_then(|owner| owner.session.as_ref());
            let new_prompt = event.kind == AiHookKind::PromptSubmit
                && (self.turn_id.is_none()
                    || event.turn_id.is_some() && event.turn_id != self.turn_id);
            // Antigravity's installed PreInvocation is normalized as a tool
            // edge: it has no prompt callback. A fresh invocation acknowledges
            // input, while an old queued edge was rejected by the ingress fence.
            let new_invocation =
                event.source == "antigravity" && event.kind == AiHookKind::ToolComplete;
            if !new_session && !new_prompt && !new_invocation {
                return false;
            }
        }
        if !self.accepts_compaction(event) {
            return false;
        }
        if event.source == "codex"
            && self.retired_codex_failure.as_ref().is_some_and(|target| {
                event.turn_id.as_ref().is_none_or(|turn| turn == &target.turn)
            })
            && matches!(
                event.kind,
                AiHookKind::TurnDone | AiHookKind::ToolComplete | AiHookKind::NeedsAttention
            )
        {
            return false;
        }
        if event.source == "codex"
            && self.retired_codex_idle.as_ref().is_some_and(|target| {
                event.turn_id.as_ref().is_none_or(|turn| turn == &target.turn)
            })
        {
            if matches!(
                event.kind,
                AiHookKind::PromptSubmit
                    | AiHookKind::ToolComplete
                    | AiHookKind::NeedsAttention
                    | AiHookKind::CompactionStart
                    | AiHookKind::CompactionEnd
            ) {
                return false;
            }
            if event.kind == AiHookKind::TurnDone
                && (self.codex_input_pending
                    || self.source == AgentStatusSource::Hook
                        && matches!(
                            self.status,
                            AgentStatus::Idle | AgentStatus::Done | AgentStatus::Failed
                        ))
            {
                return false;
            }
        }
        if self.turn_cancelled
            && matches!(
                event.kind,
                AiHookKind::TurnDone | AiHookKind::ToolComplete | AiHookKind::NeedsAttention
            )
            && (event.turn_id.is_none() || event.turn_id == self.turn_id)
        {
            return false;
        }
        let active_turn = self.compaction.as_ref().map_or(&self.turn_id, |phase| &phase.prompt_id);
        if (event.codex_hooks.is_some()
            || claude_style(&event.source)
                && matches!(
                    event.kind,
                    AiHookKind::TurnDone | AiHookKind::NeedsAttention | AiHookKind::ToolComplete
                ))
            && active_turn.is_some()
            && event.turn_id.is_some()
            && *active_turn != event.turn_id
            && !matches!(event.kind, AiHookKind::SessionStart | AiHookKind::PromptSubmit)
        {
            return false;
        }
        // A consumed turn rejects duplicates in every terminal state it can
        // hold until a new prompt: Done, claude's cancelled/legacy Idle, and
        // the explicit failure that auto-continue already observed.
        if event.kind == AiHookKind::TurnDone
            && (self.status == AgentStatus::Done
                || claude_style(&event.source)
                    && matches!(self.status, AgentStatus::Idle | AgentStatus::Failed))
            && event.turn_id.is_some()
            && event.turn_id == self.turn_id
        {
            return false;
        }
        if let Some(owner) = &self.owner {
            if owner.source != event.source {
                return false;
            }
            if owner.start_time.is_some()
                && (owner.pid != event.agent_pid || owner.start_time != event.agent_start_time)
            {
                return false;
            }
            if owner.remote_process.is_some()
                && event.remote_process.is_some()
                && owner.remote_process != event.remote_process
            {
                return false;
            }
            if owner.remote_process.is_some()
                && owner.remote_process == event.remote_process
                && event
                    .bridge_sequence
                    .zip(self.last_remote_sequence)
                    .is_some_and(|(next, previous)| next <= previous)
            {
                return false;
            }
            let same_process = owner.pid.is_some() && owner.pid == event.agent_pid
                || owner.remote_process.is_some() && owner.remote_process == event.remote_process
                || matches!(event.source.as_str(), "pi" | "omp")
                    && owner.bridge_instance.is_some()
                    && owner.bridge_instance == event.bridge_instance;
            match (owner.pid, event.agent_pid) {
                (Some(primary), Some(incoming)) if primary != incoming => return false,
                _ if owner.session.is_some()
                    && event.session_id.is_some()
                    && owner.session != event.session_id
                    && !(same_process
                        && event.kind == AiHookKind::SessionStart
                        && !event.session_compacted) =>
                {
                    return false;
                },
                _ => {},
            }
        }
        true
    }

    fn accepts_compaction(&self, event: &AiHookEvent) -> bool {
        if !claude_style(&event.source) {
            return true;
        }
        if event.kind == AiHookKind::CompactionStart {
            if event.compaction_auto
                && self.hook_seen()
                && self.turn_observed
                && (!matches!(self.status, AgentStatus::Working | AgentStatus::Blocked)
                    || event.turn_id != self.turn_id)
            {
                return false;
            }
            return self.compaction.is_none()
                && (event.compaction_auto
                    || event.turn_id.is_none()
                    || event.turn_id != self.retired_compaction)
                && (self.status != AgentStatus::Working
                    || !self.hook_seen()
                    || !self.turn_observed
                    || (event.compaction_auto && event.turn_id == self.turn_id));
        }
        if event.kind == AiHookKind::CompactionEnd || event.session_compacted {
            if let Some(phase) = &self.compaction {
                return phase.prompt_id == event.turn_id;
            }
            return event.session_compacted
                && (event.turn_id.is_none()
                    || (event.turn_id != self.retired_compaction
                        && (self.turn_id.is_none() || event.turn_id == self.turn_id)));
        }
        true
    }

    fn end_compaction(&mut self) -> Option<AgentStatus> {
        let phase = self.compaction.take()?;
        self.cancel_requested = false;
        self.retired_compaction = phase.prompt_id;
        Some(phase.resume_status)
    }

    pub fn compacting(&self) -> bool {
        self.compaction.is_some()
    }

    pub fn request_cancel(&mut self) {
        if self.background_shell_rest().is_some() {
            // Escape is new foreground evidence. The shell may still be alive,
            // so revoke only the badge projection and keep the overall work active.
            self.revoke_background_shell_rest();
            self.idle_samples = 0;
        }
        if self.compacting()
            || (self.owner.as_ref().is_some_and(|owner| owner.source == "claude")
                && self.hook_seen()
                && self.turn_observed
                && !self.pending_submit
                && self.rule.is_none()
                && matches!(self.status, AgentStatus::Working | AgentStatus::Blocked))
        {
            self.background_shell_rest = None;
            self.cancel_requested = true;
            self.idle_samples = 0;
        }
    }

    pub fn awaiting_cancel(&self) -> bool {
        self.cancel_requested
    }

    /// Claude can omit an end hook on Esc. Explicit owner input and current
    /// idle chrome may retire that phase or turn, never report success.
    pub fn observe_cancel(&mut self, detection: Option<Detection>) -> bool {
        if !self.awaiting_cancel() {
            return false;
        }
        if !detection
            .as_ref()
            .is_some_and(|d| d.agent == AgentKind::Claude && d.status == AgentStatus::Idle)
        {
            self.idle_samples = 0;
            return false;
        }
        self.idle_samples = self.idle_samples.saturating_add(1);
        if self.idle_samples < 2 {
            return false;
        }
        let compacting = self.compacting();
        self.end_compaction();
        self.cancel_requested = false;
        self.turn_cancelled |= !compacting;
        self.pending_submit = false;
        self.screen_armed = false;
        self.status = AgentStatus::Idle;
        self.confirm_status();
        self.source = AgentStatusSource::Screen;
        self.rule = Some(
            if compacting {
                "compaction_cancelled_after_input"
            } else {
                "turn_cancelled_after_input"
            }
            .into(),
        );
        true
    }

    fn record_owner(&mut self, event: &AiHookEvent) {
        if let Some(owner) = &mut self.owner {
            owner.pid = owner.pid.or(event.agent_pid);
            owner.start_time = owner.start_time.or(event.agent_start_time);
            if owner.remote_process.is_none() {
                owner.remote_process = event.remote_process.clone();
            }
            if owner.bridge_instance.is_none() {
                owner.bridge_instance = event.bridge_instance.clone();
            }
            if event.session_id.is_some() {
                owner.session = event.session_id.clone();
            }
        } else {
            self.owner = Some(Owner {
                source: event.source.clone(),
                session: event.session_id.clone(),
                pid: event.agent_pid,
                start_time: event.agent_start_time,
                remote_process: event.remote_process.clone(),
                bridge_instance: event.bridge_instance.clone(),
            });
        }
    }

    fn background_shell_rest_for(&self, event: &AiHookEvent) -> Option<BackgroundShellRest> {
        if event.source != "claude"
            || event.kind != AiHookKind::TurnDone
            || event.turn_outcome != super::AiTurnOutcome::Succeeded
        {
            return None;
        }
        let tasks = event.background_tasks.as_ref()?;
        if tasks.active == 0 || tasks.active_shells != tasks.active {
            return None;
        }
        let session = event.session_id.as_ref()?;
        let turn = event.turn_id.as_ref()?;
        let owner = self.owner.as_ref()?;
        if owner.source != event.source || owner.session.as_ref() != Some(session) {
            return None;
        }
        let shell_ids = tasks
            .shell_ids
            .as_ref()
            .filter(|ids| ids.len() == tasks.active_shells as usize)
            .cloned();
        Some(BackgroundShellRest {
            source: event.source.clone(),
            session: session.clone(),
            turn: turn.clone(),
            pid: owner.pid,
            start_time: owner.start_time,
            remote_process: owner.remote_process.clone(),
            shell_ids,
        })
    }

    /// Apply an already ordered event. A nested/foreign session has no authority
    /// over the primary pane, including its identity, attention and notifications.
    pub fn apply_hook(&mut self, event: &AiHookEvent) -> bool {
        if !self.accepts_hook(event) {
            return false;
        }
        self.background_shell_rest = None;
        self.pending_hook_prompt = false;
        let new_codex_turn = event.source == "codex"
            && event.kind == AiHookKind::PromptSubmit
            && event.turn_id.is_some()
            && event.turn_id != self.turn_id;
        let new_codex_session = event.source == "codex"
            && event.kind == AiHookKind::SessionStart
            && event.session_id.is_some()
            && event.session_id.as_ref()
                != self.owner.as_ref().and_then(|owner| owner.session.as_ref());
        if new_codex_turn || new_codex_session {
            self.codex_input_pending = false;
            self.retired_codex_idle = None;
        }
        if claude_style(&event.source)
            && event.kind == AiHookKind::SessionStart
            && !event.session_compacted
            && event.session_id.as_ref().is_some_and(|session_id| {
                self.owner.as_ref().and_then(|owner| owner.session.as_ref()) != Some(session_id)
            })
        {
            self.claude_style_prompt_ids.clear();
        }
        if claude_style(&event.source)
            && event.kind == AiHookKind::PromptSubmit
            && let Some(turn_id) = event.turn_id.as_ref()
        {
            self.claude_style_prompt_ids.insert(turn_id.clone());
        }
        self.record_owner(event);
        if matches!(
            event.kind,
            AiHookKind::SessionStart | AiHookKind::PromptSubmit | AiHookKind::TurnDone
        ) {
            self.cancel_requested = false;
        }
        if event.kind == AiHookKind::PromptSubmit
            || (event.kind == AiHookKind::SessionStart && !event.session_compacted)
        {
            self.turn_cancelled = false;
        }
        self.native_codex |= event.codex_hooks.is_some();
        if event.remote_process.is_some() && event.bridge_sequence.is_some() {
            self.last_remote_sequence = event.bridge_sequence;
        }
        let compaction_event =
            matches!(event.kind, AiHookKind::CompactionStart | AiHookKind::CompactionEnd)
                || event.session_compacted;
        if !compaction_event
            && (event.turn_id.is_some()
                || (event.kind == AiHookKind::SessionStart && !event.session_compacted))
        {
            self.turn_id = event.turn_id.clone();
        }
        if !compaction_event {
            self.end_compaction();
        }
        self.command_ended = false;
        if event.kind == AiHookKind::SessionEnd {
            self.command_finished();
            return true;
        }
        let capabilities = event.capabilities();
        let coverage = if capabilities.lifecycle {
            if capabilities.attention_events {
                HookCoverage::Lifecycle
            } else {
                HookCoverage::Turns
            }
        } else {
            HookCoverage::Completion
        };
        self.coverage = coverage;
        self.screen_blocked_fallback = None;
        self.screen_blocked_agent = None;
        self.unmatched_samples = 0;
        self.source = AgentStatusSource::Hook;
        self.confirm_status();
        self.rule = None;
        self.idle_samples = 0;
        if event.kind != AiHookKind::SessionStart && !compaction_event {
            self.pending_submit = false;
        }
        self.status = match event.kind {
            AiHookKind::SessionStart if event.session_compacted => {
                self.end_compaction().unwrap_or(self.status)
            },
            AiHookKind::CompactionStart => {
                self.cancel_requested = false;
                self.compaction = Some(Compaction {
                    prompt_id: event.turn_id.clone(),
                    resume_status: if self.status == AgentStatus::Blocked
                        || (event.compaction_auto
                            && self.turn_observed
                            && self.status == AgentStatus::Working)
                    {
                        self.status
                    } else {
                        AgentStatus::Idle
                    },
                });
                self.rule = Some("hook.compaction".into());
                AgentStatus::Working
            },
            AiHookKind::CompactionEnd => self.end_compaction().unwrap_or(self.status),
            AiHookKind::SessionStart => {
                self.turn_observed = false;
                // A fresh session (`/clear`, restart or resume) must not inherit
                // the previous session's background snapshot. Compaction
                // SessionStart is handled above and keeps it.
                self.background_tasks_active = false;
                AgentStatus::Idle
            },
            AiHookKind::PromptSubmit | AiHookKind::ToolComplete => {
                self.turn_observed = true;
                AgentStatus::Working
            },
            AiHookKind::TurnDone if event.active_background_tasks() > 0 => {
                self.turn_observed = true;
                self.rule = Some(format!(
                    "hook.background_tasks.active={}",
                    event.active_background_tasks()
                ));
                AgentStatus::Working
            },
            AiHookKind::TurnDone => {
                self.screen_armed = false;
                if matches!(
                    event.turn_outcome,
                    super::AiTurnOutcome::Failed | super::AiTurnOutcome::Incomplete
                ) {
                    AgentStatus::Failed
                } else if event.turn_outcome == super::AiTurnOutcome::Cancelled
                    || (UNPROVEN_STOP.contains(&event.source.as_str())
                        && event.turn_outcome == super::AiTurnOutcome::Unknown)
                {
                    AgentStatus::Idle
                } else {
                    AgentStatus::Done
                }
            },
            AiHookKind::NeedsAttention => AgentStatus::Blocked,
            AiHookKind::SessionEnd => unreachable!("handled above"),
        };
        if event.kind == AiHookKind::TurnDone {
            self.background_tasks_active = event.active_background_tasks() > 0;
        }
        self.background_shell_rest = self.background_shell_rest_for(event);
        self.last_activity_at = Some(std::time::Instant::now());
        true
    }

    /// Missing chrome keeps the last observation but withdraws its certainty.
    /// Full lifecycle hooks retain authority over ordinary screen observations.
    pub fn observe_screen(&mut self, detection: Option<Detection>) -> bool {
        if !self.allows_screen() {
            return false;
        }
        let Some(detection) = detection else {
            self.idle_samples = 0;
            if self.source == AgentStatusSource::Screen
                && self.status == AgentStatus::Blocked
                && self.screen_blocked_agent != Some(AgentKind::Codex)
            {
                self.unmatched_samples = self.unmatched_samples.saturating_add(1);
                if self.unmatched_samples < 2 {
                    return false;
                }
                self.unmatched_samples = 0;
                if let Some((source, rule)) = self.screen_blocked_fallback.take() {
                    self.status = AgentStatus::Working;
                    self.source = source;
                    self.rule = rule;
                } else {
                    self.status = AgentStatus::Unknown;
                    self.source = AgentStatusSource::Unknown;
                    self.rule = None;
                }
                self.confirm_status();
                return true;
            }
            if !self.hook_seen() && self.status != AgentStatus::Failed {
                return self.mark_status_unconfirmed(AgentStatusUncertainty::ScreenAmbiguous);
            }
            return false;
        };
        self.unmatched_samples = 0;
        let next = match detection.status {
            AgentStatus::Idle => {
                if detection.agent == AgentKind::Codex {
                    self.idle_samples = 0;
                    return (!self.hook_seen() && self.status != AgentStatus::Failed)
                        && self.mark_status_unconfirmed(AgentStatusUncertainty::ScreenAmbiguous);
                }
                // Keep an explicit provider failure visible until a new turn
                // starts; the error screen commonly remains above the prompt.
                if self.status == AgentStatus::Failed {
                    self.idle_samples = 0;
                    return false;
                }
                if self.pending_submit {
                    self.idle_samples = 0;
                    return false;
                }
                self.idle_samples = self.idle_samples.saturating_add(1);
                if self.idle_samples < 2 {
                    return false;
                }
                self.screen_armed = true;
                // Completion hooks own completion, even when start/permission
                // evidence has to come from the screen on older clients.
                if matches!(self.coverage, HookCoverage::Completion | HookCoverage::Turns) {
                    return false;
                }
                // Claude and Codex show the composer during work as well as
                // after success, failure or interruption. Idle is not Done.
                if self.turn_observed
                    && !matches!(detection.agent, AgentKind::Claude | AgentKind::Codex)
                {
                    AgentStatus::Done
                } else {
                    AgentStatus::Idle
                }
            },
            status @ (AgentStatus::Working | AgentStatus::Blocked) => {
                self.idle_samples = 0;
                // Pi reports starts and completions, but has no attention hook.
                // Screen evidence may fill that gap only during a hook-owned
                // turn. It cannot start a turn or override the hook's result.
                if self.coverage == HookCoverage::Turns
                    && (status != AgentStatus::Blocked
                        || !matches!(self.status, AgentStatus::Working | AgentStatus::Blocked))
                {
                    return false;
                }
                if !self.screen_armed {
                    return false;
                }
                self.pending_submit = false;
                self.turn_observed |= status == AgentStatus::Working;
                status
            },
            AgentStatus::Failed => {
                self.idle_samples = 0;
                if detection.agent != AgentKind::Codex
                    || !self.turn_observed
                    || self.coverage != HookCoverage::None
                    || !self.screen_armed
                {
                    return false;
                }
                self.pending_submit = false;
                AgentStatus::Failed
            },
            AgentStatus::Unknown | AgentStatus::Done => {
                self.idle_samples = 0;
                return false;
            },
        };
        if next == self.status && self.uncertainty.is_none() {
            return false;
        }
        self.screen_blocked_fallback =
            if next == AgentStatus::Blocked && self.status == AgentStatus::Working {
                Some((self.source, self.rule.take()))
            } else {
                None
            };
        self.screen_blocked_agent = (next == AgentStatus::Blocked).then_some(detection.agent);
        self.status = next;
        self.confirm_status();
        self.source = AgentStatusSource::Screen;
        self.rule = Some(detection.rule_id);
        true
    }

    /// Whether the latest explicit submission or accepted hook has been quiet
    /// for `threshold`. Screen samples, plain Enter and rejected hooks do not
    /// move this boundary. The caller separately requires hook ownership.
    pub fn turn_quiet_for(&self, threshold: std::time::Duration) -> bool {
        self.last_activity_at.is_none_or(|at| at.elapsed() >= threshold)
    }

    /// A stalled remote screen may reveal renewed work, but cannot prove completion.
    /// Keep the explicit submission barrier and Hook owner until authoritative evidence.
    pub fn reconcile_stalled_turn(&mut self, detection: Option<&Detection>, stalled: bool) -> bool {
        if self.allows_screen()
            || !stalled
            || self.pending_submit
            || self.cancel_requested
            || self.turn_cancelled
            || self.background_tasks_active
        {
            self.working_samples = 0;
            return false;
        }
        let Some(detection) = detection else {
            self.working_samples = 0;
            return false;
        };
        if detection.status != AgentStatus::Working {
            self.working_samples = 0;
            return false;
        }
        self.working_samples = self.working_samples.saturating_add(1);
        if self.working_samples < 2 || self.status == AgentStatus::Working {
            return false;
        }
        self.turn_observed = true;
        self.status = AgentStatus::Working;
        self.confirm_status();
        self.source = AgentStatusSource::Screen;
        self.rule = Some(detection.rule_id.clone());
        self.working_samples = 0;
        true
    }

    /// Backdate the latest submission/hook boundary without sleeping in tests.
    #[cfg(test)]
    pub(crate) fn backdate_activity_for_test(&mut self, back: std::time::Duration) {
        if let Some(at) = &mut self.last_activity_at {
            *at = std::time::Instant::now().checked_sub(back).unwrap_or(std::time::Instant::now());
        }
    }
}

/// Notification policy is an edge, not the number of polling samples.
pub(crate) fn screen_notification(
    previous: AgentStatus,
    next: AgentStatus,
    hooks: bool,
) -> Option<bool> {
    match next {
        AgentStatus::Blocked if previous != AgentStatus::Blocked => Some(true),
        AgentStatus::Done if matches!(previous, AgentStatus::Working | AgentStatus::Blocked) => {
            (!hooks).then_some(false)
        },
        AgentStatus::Failed if previous != AgentStatus::Failed => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
