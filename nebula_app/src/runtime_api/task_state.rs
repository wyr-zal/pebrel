//! Evidence attached to a task-state projection, shared by reads and waiters.

use serde::{Deserialize, Serialize};

use super::{ResolvedPaneRelocations, RuntimeSnapshot};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeTaskState {
    Idle,
    Running,
    WaitingInput,
    Attention,
    Finished,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeStateEvidence {
    #[serde(rename = "task_state_confirmed", default = "super::default_true")]
    pub confirmed: bool,
    #[serde(rename = "task_state_reason", default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Default for RuntimeStateEvidence {
    fn default() -> Self {
        Self { confirmed: true, reason: None }
    }
}

impl RuntimeStateEvidence {
    pub(crate) fn from_uncertainty(reason: Option<&str>) -> Self {
        Self { confirmed: reason.is_none(), reason: reason.map(str::to_owned) }
    }
}

/// Capability, authenticated transport and applied lifecycle are independent facts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeWslHookChannel {
    pub receiver_available: bool,
    pub authenticated: bool,
    pub lifecycle_seen: bool,
    pub helper: crate::ai_hook::HelperHealth,
}

/// State and certainty both advance the baseline; duplicate samples do not.
/// Pane identity includes its window and honors an explicit relocation receipt.
pub(super) fn stamp_state_change_seq(
    previous: Option<&RuntimeSnapshot>,
    next: &mut RuntimeSnapshot,
    relocations: &ResolvedPaneRelocations,
) {
    let mut before = std::collections::HashMap::new();
    if let Some(previous) = previous {
        for window in &previous.windows {
            for tab in &window.tabs {
                for pane in &tab.panes {
                    before.insert(
                        (window.id, pane.id),
                        (pane.task_state, &pane.state_evidence, pane.state_change_seq),
                    );
                }
            }
        }
    }
    for window in &mut next.windows {
        let window_id = window.id;
        for tab in &mut window.tabs {
            for pane in &mut tab.panes {
                let identity = (window_id, pane.id);
                let previous_identity = relocations.by_target.get(&identity).unwrap_or(&identity);
                pane.state_change_seq = match before.get(previous_identity) {
                    Some((state, evidence, seq))
                        if *state == pane.task_state && **evidence == pane.state_evidence =>
                    {
                        *seq
                    },
                    Some((_, _, seq)) => seq.saturating_add(1),
                    None => 1,
                };
            }
        }
    }
}
