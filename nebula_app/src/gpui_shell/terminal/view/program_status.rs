//! Presentation of the core's latest OSC 7501 records. No screen-text inference.
use gpui::{Context, EventEmitter as _};
use nebula_terminal::program_status::{Records, State};

use super::{TerminalView, TerminalViewEvent};
use crate::runtime_api::RuntimeTaskState;
use crate::taskbar::TaskProgress;

/// OSC 7501 reports Claude Code as `claude-code`; Hook sources use `claude`.
const CLAUDE_OSC_APP: &str = "claude-code";

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProgramRecord {
    id: String,
    state: State,
    app: Option<String>,
    result_acknowledged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProgramStatus {
    state: State,
    progress: Option<u8>,
    pub app: Option<String>,
    records: Vec<ProgramRecord>,
}

impl ProgramStatus {
    fn select(records: &Records) -> Option<Self> {
        // A blocked child must remain visible under a working/idle root.
        // Equal priorities use the latest update, matching the core's LRU order.
        let record =
            records.iter().filter(|record| !record.result_acknowledged).max_by_key(|record| {
                match record.state {
                    State::Blocked => 5,
                    State::Error => 4,
                    State::Working => 3,
                    State::Done => 2,
                    State::Idle => 1,
                }
            })?;
        Some(Self {
            app: records.app_for(record).map(str::to_owned),
            state: record.state,
            progress: record.progress,
            records: records
                .iter()
                .map(|record| ProgramRecord {
                    id: record.id.clone(),
                    state: record.state,
                    app: records.app_for(record).map(str::to_owned),
                    result_acknowledged: record.result_acknowledged,
                })
                .collect(),
        })
    }

    fn root_state(&self) -> Option<State> {
        self.records.iter().find(|record| record.id.is_empty()).map(|record| record.state)
    }

    pub(super) fn supports_background_shell_rest(&self, shell_ids: &[String]) -> bool {
        if shell_ids.is_empty() {
            return false;
        }
        let mut matched_shell = false;
        for record in self.records.iter().filter(|record| !record.result_acknowledged) {
            match record.state {
                State::Blocked | State::Error => return false,
                State::Working => {
                    if record.app.as_deref().is_some_and(|app| app != CLAUDE_OSC_APP) {
                        return false;
                    }
                    if !record.id.is_empty() {
                        if !shell_ids.iter().any(|id| id == &record.id) {
                            return false;
                        }
                        matched_shell = true;
                    }
                },
                // Historical results from other programs stay on screen (or in
                // the ring) but cannot veto the current answer's badge.
                State::Idle | State::Done => {},
            }
        }
        // A missing app is trusted only when an active record matches the
        // trusted Stop's shell ids exactly.
        self.app.as_deref() == Some(CLAUDE_OSC_APP) || matched_shell
    }

    /// Work that appeared after the Stop and cannot be attributed to its
    /// background shells withdraws the rest qualification for the whole turn.
    pub(super) fn revokes_background_shell_rest(&self, shell_ids: &[String]) -> bool {
        if shell_ids.is_empty() {
            return false;
        }
        for record in self.records.iter().filter(|record| !record.result_acknowledged) {
            match record.state {
                State::Blocked | State::Error => return true,
                State::Working => {
                    if record.app.as_deref().is_some_and(|app| app != CLAUDE_OSC_APP)
                        || (!record.id.is_empty() && !shell_ids.iter().any(|id| id == &record.id))
                    {
                        return true;
                    }
                },
                State::Idle | State::Done => {},
            }
        }
        false
    }

    pub fn task_state(&self) -> RuntimeTaskState {
        match self.state {
            State::Idle => RuntimeTaskState::Idle,
            State::Working => RuntimeTaskState::Running,
            State::Done => RuntimeTaskState::Finished,
            State::Blocked => RuntimeTaskState::Attention,
            State::Error => RuntimeTaskState::Failed,
        }
    }

    fn progress(&self) -> TaskProgress {
        match self.state {
            State::Working => {
                self.progress.map_or(TaskProgress::Indeterminate, TaskProgress::Value)
            },
            State::Blocked => TaskProgress::Paused(self.progress),
            State::Error => TaskProgress::Error(None),
            State::Idle | State::Done => TaskProgress::None,
        }
    }

    fn is_result(&self) -> bool {
        matches!(self.state, State::Done | State::Error)
    }
}

impl TerminalView {
    pub(in crate::gpui_shell) fn effective_taskbar_progress(&self) -> TaskProgress {
        if self.exited.is_some() || self.local_session_failed() {
            TaskProgress::None
        } else {
            self.progress
        }
    }

    pub(super) fn effective_program_status(&self) -> Option<&ProgramStatus> {
        // Explicit program state wins over Hook and screen/process fallback.
        // Hook ownership still supplies session identity, answers and requests.
        if self.error.is_some()
            || self.pty_failed
            || self.retain_failed_session
            || matches!(self.ssh_stage, Some(crate::ssh_session::SshStage::Failed(_)))
        {
            return None;
        }
        self.program_status.as_ref().filter(|status| self.exited.is_none() || status.is_result())
    }

    pub(super) fn refresh_program_status(&mut self, cx: &mut Context<Self>) {
        let (seen, status) = self.session.as_ref().map_or((false, None), |session| {
            let term = session.term.lock();
            (term.program_statuses().seen(), ProgramStatus::select(term.program_statuses()))
        });
        let root_state = status.as_ref().and_then(ProgramStatus::root_state);
        if self.agent_activity.background_shell_rest().is_some() {
            let shell_ids_revoked = self
                .agent_activity
                .background_shell_rest()
                .and_then(|rest| rest.shell_ids())
                .is_some_and(|ids| {
                    status
                        .as_ref()
                        .is_some_and(|status| status.revokes_background_shell_rest(ids))
                        // The contradiction may have arrived before the Stop and
                        // be cleared by the first refresh after it.
                        || self
                            .program_status
                            .as_ref()
                            .is_some_and(|status| status.revokes_background_shell_rest(ids))
                });
            // An unmatched active record or a Blocked/Error report is new work
            // that cannot be explained by the Stop's shells. It must not become
            // idle again without a fresh trusted Stop.
            if shell_ids_revoked
                || (matches!(self.program_root_state, Some(State::Idle | State::Done))
                    && root_state == Some(State::Working))
            {
                self.agent_activity.revoke_background_shell_rest();
            }
        }
        if root_state.is_some() {
            self.program_root_state = root_state;
        }
        if self.program_status_seen == seen && self.program_status == status {
            return;
        }
        self.program_status_seen = seen;
        self.program_status = status;
        // A reset must clear the old projection even after releasing ownership.
        self.apply_program_progress(cx);
        // State badges and Runtime API read the same projection. Free text is
        // retained in the core only; it is never interpreted or used as markup.
        // Do not add desktop notifications: hooks already own those edges.
        cx.emit(TerminalViewEvent::TitleChanged);
        cx.notify();
    }

    pub(super) fn sync_program_progress(&mut self, cx: &mut Context<Self>) {
        if self.program_status_seen {
            self.apply_program_progress(cx);
        }
    }

    fn apply_program_progress(&mut self, cx: &mut Context<Self>) {
        let progress =
            self.effective_program_status().map_or(TaskProgress::None, ProgramStatus::progress);
        if self.progress != progress {
            self.progress = progress;
            cx.emit(TerminalViewEvent::ProgressChanged(progress));
        }
    }
}

#[cfg(all(test, feature = "gpui-test-support"))]
mod tests;
