use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{Context, EventEmitter as _};

use super::{TerminalView, TerminalViewEvent};
use crate::platform::agent_process_identity::{NativeLaunchBoundary, ProcessStamp};
use crate::platform::wsl_location::{ObservedWslLocation, WslLocationProbe};

const MAX_STALE_LOCATION: Duration = Duration::from_secs(5);
const LOCATION_REFRESH_INTERVAL: Duration = Duration::from_secs(3);
const MAX_START_MISSES: u8 = 4;
const MAX_NO_AGENT_MISSES: u8 = 8;
const MAX_UNCERTAIN_MISSES: u8 = 8;

pub(super) struct WslCwdObserver {
    boundary: Option<NativeLaunchBoundary>,
    command_id: Option<String>,
    command_candidate: bool,
    active: bool,
    epoch: u64,
    pending: Option<Arc<AtomicBool>>,
    pending_title_changed: bool,
    last_probe: Option<Instant>,
    last_success: Option<Instant>,
    misses: u8,
    launcher: Option<ProcessStamp>,
    observed: Option<ObservedWslLocation>,
    closed: AtomicBool,
}

pub(super) struct ProbeTicket {
    epoch: u64,
    boundary: NativeLaunchBoundary,
    command_id: String,
    cancelled: Arc<AtomicBool>,
}

impl Default for WslCwdObserver {
    fn default() -> Self {
        Self {
            boundary: None,
            command_id: None,
            command_candidate: false,
            active: false,
            epoch: 0,
            pending: None,
            pending_title_changed: false,
            last_probe: None,
            last_success: None,
            misses: 0,
            launcher: None,
            observed: None,
            closed: AtomicBool::new(false),
        }
    }
}

impl WslCwdObserver {
    pub(super) fn prepare_submission(&mut self, boundary: Option<NativeLaunchBoundary>) {
        self.invalidate();
        self.boundary = boundary;
        self.command_id = None;
        self.command_candidate = false;
        self.active = false;
        self.reset_probe_state();
        self.pending_title_changed |= self.clear_observed();
    }

    pub(super) fn accept_command_id(&mut self, command_id: &str) {
        self.invalidate();
        self.command_id = valid_command_id(command_id).then(|| command_id.to_owned());
        self.active = false;
        self.reset_probe_state();
        self.pending_title_changed |= self.clear_observed();
    }

    pub(super) fn accept_command_line(&mut self, value: &str) {
        self.command_candidate = command_line_contains_wsl(value);
        if !self.command_candidate {
            self.invalidate();
            self.active = false;
            self.pending_title_changed |= self.clear_observed();
        }
    }

    pub(super) fn start_command(&mut self) -> bool {
        self.active = self.boundary.is_some()
            && self.command_id.is_some()
            && self.command_candidate
            && !self.closed.load(Ordering::Acquire);
        self.take_title_change()
    }

    pub(super) fn finish_command(&mut self) -> bool {
        self.invalidate();
        self.active = false;
        self.boundary = None;
        self.command_id = None;
        self.command_candidate = false;
        self.reset_probe_state();
        let cleared = self.clear_observed();
        self.take_title_change() || cleared
    }

    pub(super) fn stop(&self) {
        self.closed.store(true, Ordering::Release);
        if let Some(cancelled) = &self.pending {
            cancelled.store(true, Ordering::Release);
        }
    }

    pub(super) fn observed(&self) -> Option<&ObservedWslLocation> {
        self.observed.as_ref()
    }

    #[cfg(test)]
    pub(super) fn set_observed_for_test(&mut self, location: Option<ObservedWslLocation>) {
        self.observed = location;
    }

    pub(super) fn take_title_change(&mut self) -> bool {
        std::mem::take(&mut self.pending_title_changed)
    }

    fn invalidate(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        if let Some(cancelled) = &self.pending {
            cancelled.store(true, Ordering::Release);
        }
    }

    fn reset_probe_state(&mut self) {
        self.last_probe = None;
        self.last_success = None;
        self.misses = 0;
        self.launcher = None;
    }

    fn clear_observed(&mut self) -> bool {
        self.observed.take().is_some()
    }

    fn due(&self, now: Instant) -> bool {
        if !self.active
            || self.closed.load(Ordering::Acquire)
            || self.pending.is_some()
            || self.boundary.is_none()
            || self.command_id.is_none()
        {
            return false;
        }
        let interval = if self.observed.is_some() {
            LOCATION_REFRESH_INTERVAL
        } else {
            Duration::from_secs(1_u64 << self.misses.min(4))
        };
        self.last_probe
            .is_none_or(|last| now.saturating_duration_since(last) >= interval)
    }

    pub(super) fn begin_probe(&mut self, now: Instant) -> Option<ProbeTicket> {
        if !self.due(now) {
            return None;
        }
        let ticket = ProbeTicket {
            epoch: self.epoch,
            boundary: self.boundary.clone()?,
            command_id: self.command_id.clone()?,
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        self.last_probe = Some(now);
        self.pending = Some(ticket.cancelled.clone());
        Some(ticket)
    }

    fn finish_probe(&mut self, ticket: &ProbeTicket) -> bool {
        if !self.pending.as_ref().is_some_and(|pending| Arc::ptr_eq(pending, &ticket.cancelled)) {
            return false;
        }
        self.pending = None;
        self.epoch == ticket.epoch
            && self.command_id.as_deref() == Some(ticket.command_id.as_str())
            && self.boundary.as_ref() == Some(&ticket.boundary)
            && !self.closed.load(Ordering::Acquire)
            && !ticket.cancelled.load(Ordering::Acquire)
    }

    pub(super) fn complete_probe(
        &mut self,
        ticket: &ProbeTicket,
        result: WslLocationProbe,
        binding_is_current: bool,
        now: Instant,
    ) -> bool {
        if !self.finish_probe(ticket) {
            crate::logging::debug_log(
                "wsl_cwd_observer event=probe_complete accepted=false reason=stale_or_cancelled",
            );
            return false;
        }
        let outcome = match &result {
            WslLocationProbe::Found(_) => "found",
            WslLocationProbe::NoLauncher => "no_launcher",
            WslLocationProbe::NoAgent { .. } => "no_agent",
            WslLocationProbe::Unavailable { .. } => "unavailable",
            WslLocationProbe::Ambiguous => "ambiguous",
            WslLocationProbe::Unknown { .. } => "unknown",
        };
        crate::logging::debug_log(format!(
            "wsl_cwd_observer event=probe_complete outcome={outcome} binding_current={binding_is_current}"
        ));
        if !binding_is_current {
            return self.finish_command();
        }
        self.apply_probe(result, now)
    }

    fn apply_probe(&mut self, result: WslLocationProbe, now: Instant) -> bool {
        let before = self.visible_key();
        match result {
            WslLocationProbe::Found(location) => {
                self.launcher = Some(location.launcher);
                self.observed = Some(location);
                self.last_success = Some(now);
                self.misses = 0;
            },
            WslLocationProbe::NoAgent { launcher } => {
                self.replace_launcher(Some(launcher));
                self.clear_observed();
                self.last_success = None;
                self.record_miss(MAX_NO_AGENT_MISSES);
            },
            WslLocationProbe::Unavailable { launcher } => {
                if self.launcher != launcher {
                    self.clear_observed();
                    self.last_success = None;
                    self.launcher = launcher;
                }
                self.record_miss(MAX_UNCERTAIN_MISSES);
            },
            WslLocationProbe::Unknown { launcher } => {
                self.replace_launcher(launcher);
                self.record_miss(MAX_UNCERTAIN_MISSES);
            },
            WslLocationProbe::Ambiguous => {
                self.clear_observed();
                self.launcher = None;
                self.last_success = None;
                self.record_miss(MAX_UNCERTAIN_MISSES);
            },
            WslLocationProbe::NoLauncher => {
                self.clear_observed();
                self.launcher = None;
                self.last_success = None;
                self.record_miss(MAX_START_MISSES);
            },
        }
        before != self.visible_key()
    }

    fn record_miss(&mut self, limit: u8) {
        self.misses = self.misses.saturating_add(1);
        if self.misses >= limit {
            self.active = false;
        }
    }

    fn replace_launcher(&mut self, launcher: Option<ProcessStamp>) {
        if self.launcher != launcher {
            self.clear_observed();
            self.last_success = None;
            self.launcher = launcher;
        }
    }

    fn expire_stale(&mut self, now: Instant) -> bool {
        let before = self.visible_key();
        if self.observed.is_some()
            && self
                .last_success
                .is_none_or(|last| now.saturating_duration_since(last) > MAX_STALE_LOCATION)
        {
            self.clear_observed();
        }
        before != self.visible_key()
    }

    fn visible_key(&self) -> Option<(String, String)> {
        self.observed.as_ref().map(|location| (location.distro.clone(), location.cwd.clone()))
    }
}

fn valid_command_id(command_id: &str) -> bool {
    command_id.len() == 32 && command_id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn command_line_contains_wsl(value: &str) -> bool {
    // PowerShell prefixes its text with the shell owner; Bash reports raw text.
    let command = value.split_once('\n')
        .filter(|(owner, _)| owner.starts_with("pwsh:"))
        .map_or(value, |(_, command)| command);
    let executable =
        if let Some(quote) = command.chars().next().filter(|quote| matches!(quote, '\'' | '"')) {
            let rest = &command[quote.len_utf8()..];
            let Some(end) = rest.find(quote) else { return false };
            &rest[..end]
        } else {
            command.split_ascii_whitespace().next().unwrap_or_default()
        };
    crate::shell_detect::is_wsl_launcher(executable)
}

impl TerminalView {
    pub(crate) fn refresh_wsl_cwd_observation(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        if self.wsl_cwd_observer.expire_stale(now) {
            cx.emit(TerminalViewEvent::TitleChanged);
            cx.notify();
        }
        if self.exited.is_some()
            || self.error.is_some()
            || self.ssh_destination.is_some()
            || self.session.is_none()
        {
            return;
        }
        let Some(context) = self.exec_context.as_ref().cloned() else {
            return;
        };
        let Some(ticket) = self.wsl_cwd_observer.begin_probe(now) else { return };
        crate::logging::debug_log("wsl_cwd_observer event=probe_start");
        let Some(session) = &self.session else { return };
        let shell_pid = session.shell_pid;
        if shell_pid == 0 {
            self.wsl_cwd_observer.pending = None;
            return;
        }
        let pty = Arc::downgrade(&session.term);
        let pane_id = self.pane_id;
        let work_cancelled = ticket.cancelled.clone();
        let boundary = ticket.boundary.clone();
        let command_id = ticket.command_id.clone();
        let work = cx.background_executor().spawn(async move {
            let is_cancelled = || work_cancelled.load(Ordering::Acquire);
            crate::platform::wsl_location::probe(
                shell_pid,
                pane_id,
                &boundary,
                &context,
                &command_id,
                &is_cancelled,
            )
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |view, cx| {
                let same_pty = view
                    .session
                    .as_ref()
                    .is_some_and(|session| pty.ptr_eq(&Arc::downgrade(&session.term)));
                let same_shell =
                    view.session.as_ref().is_some_and(|session| session.shell_pid == shell_pid);
                let current_binding = same_pty
                    && same_shell
                    && view.exited.is_none()
                    && view.error.is_none();
                if view.wsl_cwd_observer.complete_probe(
                    &ticket,
                    result,
                    current_binding,
                    Instant::now(),
                ) {
                    cx.emit(TerminalViewEvent::TitleChanged);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn apply_wsl_cwd_probe_result(
        &mut self,
        ticket: &ProbeTicket,
        result: WslLocationProbe,
        same_pty_and_shell: bool,
        cx: &mut Context<Self>,
    ) {
        if self.wsl_cwd_observer.complete_probe(
            ticket,
            result,
            same_pty_and_shell && self.exited.is_none() && self.error.is_none(),
            Instant::now(),
        ) {
            cx.emit(TerminalViewEvent::TitleChanged);
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_agents::AgentKind;
    use crate::platform::agent_process_identity::{ForegroundAgent, ProcessStamp};

    fn boundary() -> NativeLaunchBoundary {
        NativeLaunchBoundary {
            time: 100,
            shell: ProcessStamp { pid: 10, start_time: 90 },
            existing_branches: Default::default(),
        }
    }

    fn location(launcher_pid: u32, agent_pid: u32, cwd: &str) -> ObservedWslLocation {
        ObservedWslLocation {
            distro: "Ubuntu-22.04".into(),
            user: None,
            cwd: cwd.into(),
            launcher: ProcessStamp { pid: launcher_pid, start_time: u64::from(launcher_pid) * 10 },
            agent: ForegroundAgent {
                kind: AgentKind::Claude,
                process: ProcessStamp { pid: agent_pid, start_time: u64::from(agent_pid) * 10 },
                shell: ProcessStamp { pid: 1, start_time: 10 },
                tty_nr: 4,
                process_group: agent_pid,
            },
        }
    }

    #[test]
    fn command_metadata_accepts_only_a_guid_and_a_wsl_launcher() {
        let mut observer = WslCwdObserver::default();
        observer.prepare_submission(Some(boundary()));
        observer.accept_command_id("0123456789abcdef0123456789abcdef");
        observer.accept_command_line("pwsh:fixture\nwsl.exe -d Ubuntu-22.04");
        observer.start_command();
        assert!(observer.active);

        observer.prepare_submission(Some(boundary()));
        observer.accept_command_id("not-a-guid");
        observer.accept_command_line("pwsh:fixture\nwsl.exe -d Ubuntu-22.04");
        observer.start_command();
        assert!(!observer.active);
        observer.accept_command_id("0123456789abcdef0123456789abcdef");
        observer.accept_command_line("pwsh:fixture\nGet-ChildItem");
        observer.start_command();
        assert!(!observer.active);
    }

    #[test]
    fn pasted_wsl_command_is_retained_until_the_shell_command_id_arrives() {
        let mut observer = WslCwdObserver::default();
        observer.prepare_submission(Some(boundary()));
        observer.accept_command_line("wsl.exe -d Ubuntu-22.04 --cd /mnt/e/ProjectOrder/VoiceRecite");
        observer.start_command();
        assert!(!observer.active);

        observer.accept_command_id("abcdef0123456789abcdef0123456789");
        observer.start_command();
        assert!(observer.active);
    }

    #[test]
    fn multiline_bash_command_keeps_its_first_line() {
        assert!(command_line_contains_wsl("wsl.exe -d Ubuntu-22.04 \\\n  --cd /mnt/f/ProjectLearn"));
        assert!(command_line_contains_wsl("pwsh:fixture\nwsl.exe -d Ubuntu-22.04"));
        assert!(!command_line_contains_wsl("printf 'first\nwsl.exe -d Ubuntu-22.04'"));
    }

    #[test]
    fn non_wsl_metadata_cancels_a_probe_started_after_the_command_id() {
        let mut observer = WslCwdObserver::default();
        observer.prepare_submission(Some(boundary()));
        observer.accept_command_line("wsl.exe -d Ubuntu-22.04");
        observer.accept_command_id("0123456789abcdef0123456789abcdef");
        observer.start_command();
        let now = Instant::now();
        let ticket = observer.begin_probe(now).unwrap();

        observer.accept_command_line("printf next");
        assert!(ticket.cancelled.load(Ordering::Acquire));
        assert!(!observer.complete_probe(
            &ticket, WslLocationProbe::Found(location(20, 30, "/stale")), true, now,
        ));
        assert!(!observer.active);
        assert!(observer.observed().is_none());
    }

    #[test]
    fn owner_change_clears_old_location_and_unavailable_same_owner_expires() {
        let mut observer = WslCwdObserver::default();
        observer.prepare_submission(Some(boundary()));
        observer.accept_command_id("0123456789abcdef0123456789abcdef");
        observer.accept_command_line("pwsh:fixture\nwsl.exe -d Ubuntu-22.04");
        observer.start_command();
        let now = Instant::now();
        assert!(observer.apply_probe(WslLocationProbe::Found(location(20, 30, "/one")), now));
        assert!(observer.apply_probe(WslLocationProbe::Found(location(21, 31, "/two")), now));
        assert_eq!(observer.observed().unwrap().cwd, "/two");
        let owner = observer.launcher.unwrap();
        assert!(
            !observer.apply_probe(WslLocationProbe::Unavailable { launcher: Some(owner) }, now)
        );
        assert!(observer.expire_stale(now + MAX_STALE_LOCATION + Duration::from_millis(1)));
        assert!(observer.observed().is_none());
    }

    #[test]
    fn canceled_old_epoch_and_closed_probe_results_are_discarded() {
        let mut observer = WslCwdObserver::default();
        observer.prepare_submission(Some(boundary()));
        observer.accept_command_id("0123456789abcdef0123456789abcdef");
        observer.accept_command_line("pwsh:fixture\nwsl.exe -d Ubuntu-22.04");
        observer.start_command();
        let now = Instant::now();
        let old_ticket = observer.begin_probe(now).unwrap();

        observer.prepare_submission(Some(boundary()));
        observer.accept_command_id("abcdef0123456789abcdef0123456789");
        observer.accept_command_line("pwsh:fixture\nwsl.exe -d Ubuntu-22.04");
        observer.start_command();
        assert!(old_ticket.cancelled.load(Ordering::Acquire));
        assert!(!observer.complete_probe(
            &old_ticket,
            WslLocationProbe::Found(location(20, 30, "/stale")),
            true,
            now,
        ));
        assert!(observer.active);
        assert!(observer.observed().is_none());

        let ticket = observer.begin_probe(now + LOCATION_REFRESH_INTERVAL).unwrap();
        observer.stop();
        assert!(ticket.cancelled.load(Ordering::Acquire));
        assert!(!observer.complete_probe(
            &ticket,
            WslLocationProbe::Found(location(20, 30, "/closed")),
            true,
            now,
        ));
        assert!(observer.observed().is_none());
    }

    #[test]
    fn a_reused_guest_pid_with_a_new_start_stamp_updates_the_observation() {
        let mut observer = WslCwdObserver::default();
        observer.prepare_submission(Some(boundary()));
        observer.accept_command_id("0123456789abcdef0123456789abcdef");
        observer.accept_command_line("pwsh:fixture\nwsl.exe -d Ubuntu-22.04");
        observer.start_command();
        let now = Instant::now();
        observer.apply_probe(WslLocationProbe::Found(location(20, 30, "/one")), now);
        let mut reused = location(20, 30, "/two");
        reused.agent.process.start_time += 1;
        assert!(observer.apply_probe(WslLocationProbe::Found(reused.clone()), now));
        assert_eq!(observer.observed().unwrap().agent.process, reused.agent.process);
        assert_eq!(observer.observed().unwrap().cwd, "/two");
    }

    #[test]
    fn a_live_location_is_not_scanned_again_every_watchdog_tick() {
        let mut observer = WslCwdObserver::default();
        observer.prepare_submission(Some(boundary()));
        observer.accept_command_id("0123456789abcdef0123456789abcdef");
        observer.accept_command_line("pwsh:fixture\nwsl.exe -d Ubuntu-22.04");
        observer.start_command();
        let now = Instant::now();
        let ticket = observer.begin_probe(now).unwrap();
        observer.complete_probe(
            &ticket,
            WslLocationProbe::Found(location(20, 30, "/one")),
            true,
            now,
        );

        assert!(!observer.due(now + LOCATION_REFRESH_INTERVAL - Duration::from_millis(1)));
        assert!(observer.due(now + LOCATION_REFRESH_INTERVAL));
    }

    #[test]
    fn no_agent_probe_budget_is_finite() {
        let mut observer = WslCwdObserver::default();
        observer.prepare_submission(Some(boundary()));
        observer.accept_command_id("0123456789abcdef0123456789abcdef");
        observer.accept_command_line("pwsh:fixture\nwsl.exe -d Ubuntu-22.04");
        observer.start_command();
        let launcher = ProcessStamp { pid: 20, start_time: 200 };

        for _ in 0..MAX_NO_AGENT_MISSES {
            observer.apply_probe(WslLocationProbe::NoAgent { launcher }, Instant::now());
        }

        assert!(!observer.active);
        assert!(observer.observed().is_none());
        assert!(observer.begin_probe(Instant::now() + Duration::from_secs(600)).is_none());
    }

    #[test]
    fn uncertain_probe_budget_is_finite() {
        let launcher = ProcessStamp { pid: 20, start_time: 200 };
        for result in [
            WslLocationProbe::Unavailable { launcher: Some(launcher) },
            WslLocationProbe::Unknown { launcher: Some(launcher) },
            WslLocationProbe::Ambiguous,
        ] {
            let mut observer = WslCwdObserver::default();
            observer.prepare_submission(Some(boundary()));
            observer.accept_command_id("0123456789abcdef0123456789abcdef");
            observer.accept_command_line("pwsh:fixture\nwsl.exe -d Ubuntu-22.04");
            observer.start_command();

            for _ in 0..MAX_UNCERTAIN_MISSES {
                observer.apply_probe(result.clone(), Instant::now());
            }

            assert!(!observer.active);
            assert!(observer.begin_probe(Instant::now() + Duration::from_secs(600)).is_none());
        }
    }

    #[test]
    fn no_agent_or_ambiguous_branch_never_keeps_the_old_label() {
        let mut observer = WslCwdObserver::default();
        observer.prepare_submission(Some(boundary()));
        observer.accept_command_id("0123456789abcdef0123456789abcdef");
        observer.accept_command_line("pwsh:fixture\nwsl.exe -d Ubuntu-22.04");
        observer.start_command();
        let now = Instant::now();
        observer.apply_probe(WslLocationProbe::Found(location(20, 30, "/one")), now);
        assert!(observer.apply_probe(
            WslLocationProbe::NoAgent { launcher: ProcessStamp { pid: 20, start_time: 200 } },
            now,
        ));
        assert!(observer.observed().is_none());
        observer.apply_probe(WslLocationProbe::Found(location(20, 30, "/one")), now);
        assert!(observer.apply_probe(WslLocationProbe::Ambiguous, now));
        assert!(observer.observed().is_none());
    }
}
