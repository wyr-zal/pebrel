use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::Duration;

use gpui::Context;

use super::{TerminalView, TerminalViewEvent};

const STARTUP_GRACE: Duration = Duration::from_secs(3);
const PROBE_INTERVAL: Duration = Duration::from_secs(2);

impl TerminalView {
    pub(super) fn invalidate_agent_identity_probe(&mut self) {
        self.agent_identity_probe_epoch = self.agent_identity_probe_epoch.wrapping_add(1);
        self.agent_identity_probe_pending = false;
        self.last_agent_identity_probe = None;
        if let Some(cancelled) = self.agent_identity_probe_cancel.take() {
            cancelled.store(true, Ordering::Release);
        }
    }

    pub(super) fn probe_agent_identity(&mut self, cx: &mut Context<'_, Self>) {
        if self.exited.is_some()
            || !self.command_running
            || self.agent_activity.hook_seen()
            || self.running_program.as_deref().and_then(crate::ai_agents::AgentKind::parse).is_some()
            || !matches!(&self.suggest.suggest_env, crate::display::SuggestEnv::Wsl { .. })
            || self.agent_identity_probe_pending
        {
            return;
        }
        if self.command_started.is_none_or(|started| started.elapsed() < STARTUP_GRACE)
            || self.last_agent_identity_probe.is_some_and(|at| at.elapsed() < PROBE_INTERVAL)
        {
            return;
        }
        let crate::display::SuggestEnv::Wsl { distro } = &self.suggest.suggest_env else {
            return;
        };
        let Some(exec_context) = self.exec_context.as_ref() else { return };
        if exec_context.wsl_distribution().is_none() {
            return;
        }
        let context = if distro.is_empty() {
            exec_context.clone()
        } else {
            let Some(context) = exec_context.for_wsl_distribution(distro) else { return };
            context
        };
        let result_distribution = distro.clone();
        let Some(shell_pid) = self
            .session
            .as_ref()
            .map(|session| session.shell_pid)
            .filter(|pid| *pid != 0)
        else {
            return;
        };
        let pane_id = self.pane_id;
        let epoch = self.agent_identity_probe_epoch;
        let cancelled = Arc::new(AtomicBool::new(false));
        self.agent_identity_probe_cancel = Some(cancelled.clone());
        self.agent_identity_probe_pending = true;
        self.last_agent_identity_probe = Some(std::time::Instant::now());

        let work = cx.background_executor().spawn(async move {
            let is_cancelled = || cancelled.load(Ordering::Acquire);
            crate::platform::agent_process_identity::probe_agent(
                pane_id,
                &context,
                Some(shell_pid),
                &is_cancelled,
            )
        });
        cx.spawn(async move |this, cx| {
            let detected = work.await;
            let _ = this.update(cx, |view, cx| {
                if view.agent_identity_probe_epoch == epoch {
                    view.agent_identity_probe_pending = false;
                    view.agent_identity_probe_cancel = None;
                }
                let current_shell_pid = view.session.as_ref().map(|session| session.shell_pid);
                if !process_probe_result_is_current(
                    view.agent_identity_probe_epoch,
                    epoch,
                    view.command_running,
                    view.agent_activity.hook_seen(),
                    view.exited.is_some(),
                    current_shell_pid,
                    shell_pid,
                    view.running_program.as_deref(),
                    match &view.suggest.suggest_env {
                        crate::display::SuggestEnv::Wsl { distro } => Some(distro.as_str()),
                        _ => None,
                    },
                    &result_distribution,
                ) {
                    return;
                }
                let Some(agent) = detected else { return };
                view.running_program = Some(agent.slug().to_owned());
                view.agent_activity.identify(crate::ai_agents::AgentStatusSource::Process);
                cx.emit(TerminalViewEvent::TitleChanged);
                cx.notify();
            });
        })
        .detach();
    }
}

pub(super) fn may_refine_process_identity(hook_seen: bool, running_program: Option<&str>) -> bool {
    !hook_seen
        && running_program
            .and_then(crate::ai_agents::AgentKind::parse)
            .is_none()
}

fn process_probe_result_is_current(
    current_epoch: u64,
    result_epoch: u64,
    command_running: bool,
    hook_seen: bool,
    exited: bool,
    current_shell_pid: Option<u32>,
    result_shell_pid: u32,
    running_program: Option<&str>,
    current_distribution: Option<&str>,
    result_distribution: &str,
) -> bool {
    current_epoch == result_epoch
        && command_running
        && !hook_seen
        && !exited
        && current_shell_pid == Some(result_shell_pid)
        && current_distribution == Some(result_distribution)
        && may_refine_process_identity(hook_seen, running_program)
}

#[cfg(test)]
mod tests {
    use super::{may_refine_process_identity, process_probe_result_is_current};

    fn current(
        current_epoch: u64,
        result_epoch: u64,
        command_running: bool,
        hook_seen: bool,
        shell_pid: Option<u32>,
        running_program: Option<&str>,
    ) -> bool {
        process_probe_result_is_current(
            current_epoch,
            result_epoch,
            command_running,
            hook_seen,
            false,
            shell_pid,
            10,
            running_program,
            Some("Debian"),
            "Debian",
        )
    }

    #[test]
    fn only_unknown_wrapper_names_can_be_refined() {
        assert!(may_refine_process_identity(false, Some("cc")));
        assert!(may_refine_process_identity(false, None));
        assert!(!may_refine_process_identity(false, Some("codex")));
        assert!(!may_refine_process_identity(true, Some("cc")));
    }

    #[test]
    fn unknown_wrapper_can_be_refined_but_authoritative_identity_cannot() {
        assert!(current(4, 4, true, false, Some(10), Some("cc")));
        assert!(!current(4, 4, true, true, Some(10), Some("cc")));
        assert!(!current(4, 4, true, false, Some(10), Some("codex")));
        assert!(!current(4, 4, false, false, Some(10), Some("cc")));
        assert!(!current(5, 4, true, false, Some(10), Some("cc")));
        assert!(!current(4, 4, true, false, Some(11), Some("cc")));
        assert!(!process_probe_result_is_current(
            4,
            4,
            true,
            false,
            false,
            Some(10),
            10,
            Some("cc"),
            Some("Ubuntu"),
            "Debian",
        ));
    }
}
