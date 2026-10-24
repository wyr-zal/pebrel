//! Bounded observation of an Agent working directory in a manually launched WSL branch.

use std::collections::HashMap;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::platform::agent_process_identity::{
    ForegroundAgent, ForegroundAgentLocationProbe, NativeLaunchBoundary, NativeProcessIdentity,
    ProcessStamp,
};

const LOCATION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_DISTRO_LIST: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedWslLocation {
    pub(crate) distro: String,
    pub(crate) user: Option<String>,
    pub(crate) cwd: String,
    pub(crate) launcher: ProcessStamp,
    pub(crate) agent: ForegroundAgent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WslLocationProbe {
    Found(ObservedWslLocation),
    NoLauncher,
    NoAgent { launcher: ProcessStamp },
    Unavailable { launcher: Option<ProcessStamp> },
    Ambiguous,
    Unknown { launcher: Option<ProcessStamp> },
}

fn probe_outcome(result: &WslLocationProbe) -> &'static str {
    match result {
        WslLocationProbe::Found(_) => "found",
        WslLocationProbe::NoLauncher => "no_launcher",
        WslLocationProbe::NoAgent { .. } => "no_agent",
        WslLocationProbe::Unavailable { .. } => "unavailable",
        WslLocationProbe::Ambiguous => "ambiguous",
        WslLocationProbe::Unknown { .. } => "unknown",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WslLauncher {
    stamp: ProcessStamp,
    distro: String,
    user: Option<String>,
}

enum LauncherSelection {
    None,
    One(WslLauncher),
    Ambiguous,
    Unavailable,
    Unknown(Option<ProcessStamp>),
}

#[derive(Debug, PartialEq, Eq)]
enum CandidateRows {
    None,
    One(String, ProcessStamp),
    Ambiguous,
    Unavailable,
}

/// Probe only a new, direct WSL child of this pane's Windows shell.
pub(crate) fn probe(
    shell_pid: u32,
    pane_id: u64,
    boundary: &NativeLaunchBoundary,
    host_context: &crate::runtime_exec::PaneExecContext,
    command_id: &str,
    cancelled: &dyn Fn() -> bool,
) -> WslLocationProbe {
    let deadline = Instant::now() + LOCATION_PROBE_TIMEOUT;
    if cancelled() {
        return WslLocationProbe::Unavailable { launcher: None };
    }
    let launcher = match select_launcher(shell_pid, boundary) {
        LauncherSelection::One(launcher) => {
            crate::logging::debug_log("wsl_location stage=launcher outcome=selected");
            launcher
        },
        LauncherSelection::None => {
            crate::logging::debug_log("wsl_location stage=launcher outcome=none");
            return WslLocationProbe::NoLauncher;
        },
        LauncherSelection::Ambiguous => {
            crate::logging::debug_log("wsl_location stage=launcher outcome=ambiguous");
            return WslLocationProbe::Ambiguous;
        },
        LauncherSelection::Unavailable => {
            crate::logging::debug_log("wsl_location stage=launcher outcome=unavailable");
            return WslLocationProbe::Unavailable { launcher: None };
        },
        LauncherSelection::Unknown(launcher) => {
            crate::logging::debug_log("wsl_location stage=launcher outcome=unknown");
            return WslLocationProbe::Unknown { launcher };
        },
    };
    if Instant::now() >= deadline || cancelled() {
        return WslLocationProbe::Unavailable { launcher: Some(launcher.stamp) };
    }
    match running_distros(deadline, cancelled) {
        Some(distros) if distros.iter().any(|name| name.eq_ignore_ascii_case(&launcher.distro)) => {
            crate::logging::debug_log("wsl_location stage=distro outcome=running");
        },
        Some(_) => {
            crate::logging::debug_log("wsl_location stage=distro outcome=not_running");
            return WslLocationProbe::Unavailable { launcher: Some(launcher.stamp) };
        },
        None => {
            crate::logging::debug_log("wsl_location stage=distro outcome=unavailable");
            return WslLocationProbe::Unavailable { launcher: Some(launcher.stamp) };
        },
    }
    if let Err(result) = confirm_launcher(shell_pid, boundary, &launcher) {
        crate::logging::debug_log(format!(
            "wsl_location stage=launcher_recheck outcome={}",
            probe_outcome(&result)
        ));
        return result;
    }
    if cancelled() || Instant::now() >= deadline {
        return WslLocationProbe::Unavailable { launcher: Some(launcher.stamp) };
    }

    let context =
        host_context.for_observed_wsl(&launcher.distro, launcher.user.as_deref(), command_id);
    let result = crate::platform::agent_process_identity::probe_foreground_agent_location(
        pane_id, &context, deadline, cancelled,
    );
    if cancelled() {
        return WslLocationProbe::Unavailable { launcher: Some(launcher.stamp) };
    }
    crate::logging::debug_log(format!(
        "wsl_location stage=foreground_agent outcome={}",
        match &result {
            ForegroundAgentLocationProbe::Found(_, _) => "found",
            ForegroundAgentLocationProbe::NoAgent => "no_agent",
            ForegroundAgentLocationProbe::Unavailable => "unavailable",
        }
    ));
    if let Err(result) = confirm_launcher(shell_pid, boundary, &launcher) {
        crate::logging::debug_log(format!(
            "wsl_location stage=launcher_final_check outcome={}",
            probe_outcome(&result)
        ));
        return result;
    }
    match result {
        ForegroundAgentLocationProbe::Found(agent, cwd) => {
            WslLocationProbe::Found(ObservedWslLocation {
                distro: launcher.distro,
                user: launcher.user,
                cwd,
                launcher: launcher.stamp,
                agent,
            })
        },
        ForegroundAgentLocationProbe::NoAgent => {
            WslLocationProbe::NoAgent { launcher: launcher.stamp }
        },
        ForegroundAgentLocationProbe::Unavailable => {
            WslLocationProbe::Unavailable { launcher: Some(launcher.stamp) }
        },
    }
}

fn select_launcher(shell_pid: u32, boundary: &NativeLaunchBoundary) -> LauncherSelection {
    let Ok(rows) = crate::platform::process_snapshot::snapshot() else {
        return LauncherSelection::Unavailable;
    };
    let (executable, stamp) = match select_launcher_candidate(shell_pid, boundary, rows) {
        CandidateRows::One(executable, stamp) => (executable, stamp),
        CandidateRows::None => return LauncherSelection::None,
        CandidateRows::Ambiguous => return LauncherSelection::Ambiguous,
        CandidateRows::Unavailable => return LauncherSelection::Unavailable,
    };
    let Some(process) =
        NativeProcessIdentity::open(stamp.pid).filter(|process| process.stamp() == stamp)
    else {
        return LauncherSelection::Unavailable;
    };
    let Some(arguments) = process.command_line_arguments() else {
        return LauncherSelection::Unknown(Some(stamp));
    };
    let Some((distro, user)) = arguments
        .get(1..)
        .and_then(|args| crate::shell_detect::wsl_explicit_scope(&executable, args))
        .filter(|(distro, user)| {
            !distro.is_empty()
                && distro.len() <= 256
                && !distro.chars().any(char::is_control)
                && user.as_ref().is_none_or(|user| {
                    !user.is_empty() && user.len() <= 256 && !user.chars().any(char::is_control)
                })
        })
    else {
        return LauncherSelection::Unknown(Some(stamp));
    };
    if process.stamp() != stamp {
        return LauncherSelection::Unavailable;
    }
    LauncherSelection::One(WslLauncher { stamp, distro, user })
}

fn select_launcher_candidate(
    shell_pid: u32,
    boundary: &NativeLaunchBoundary,
    rows: Vec<crate::platform::process_snapshot::ProcessRow>,
) -> CandidateRows {
    let stamps: HashMap<_, _> = rows.iter().map(|row| (row.pid, row.created)).collect();
    if stamps.get(&shell_pid).copied() != Some(boundary.shell.start_time) {
        return CandidateRows::None;
    }
    let Ok(tree) = crate::process_tree::descendants_from_rows(shell_pid, rows) else {
        return CandidateRows::Unavailable;
    };
    let mut candidates = Vec::new();
    for child in tree.iter().filter(|entry| {
        entry.depth == 1
            && entry.parent_pid == shell_pid
            && crate::shell_detect::is_wsl_launcher(&entry.executable)
    }) {
        let Some(&created) = stamps.get(&child.pid) else {
            return CandidateRows::Unavailable;
        };
        let stamp = ProcessStamp { pid: child.pid, start_time: created };
        if created == 0 {
            return CandidateRows::Unavailable;
        }
        if created <= boundary.time || boundary.existing_branches.contains(&stamp) {
            continue;
        }
        candidates.push((child.executable.clone(), stamp));
    }
    if candidates.len() > 1 {
        return CandidateRows::Ambiguous;
    }
    match candidates.pop() {
        Some((executable, stamp)) => CandidateRows::One(executable, stamp),
        None => CandidateRows::None,
    }
}

fn confirm_launcher(
    shell_pid: u32,
    boundary: &NativeLaunchBoundary,
    expected: &WslLauncher,
) -> Result<(), WslLocationProbe> {
    match select_launcher(shell_pid, boundary) {
        LauncherSelection::One(current) if current == *expected => Ok(()),
        LauncherSelection::None => Err(WslLocationProbe::NoLauncher),
        LauncherSelection::Ambiguous => Err(WslLocationProbe::Ambiguous),
        LauncherSelection::Unavailable => {
            Err(WslLocationProbe::Unavailable { launcher: Some(expected.stamp) })
        },
        LauncherSelection::One(current) => {
            Err(WslLocationProbe::Unknown { launcher: Some(current.stamp) })
        },
        LauncherSelection::Unknown(launcher) => Err(WslLocationProbe::Unknown { launcher }),
    }
}

fn running_distros(deadline: Instant, cancelled: &dyn Fn() -> bool) -> Option<Vec<String>> {
    if cancelled() || Instant::now() >= deadline {
        return None;
    }
    let mut command = Command::new(crate::platform::shell::wsl_executable()?);
    command.args(["--list", "--running", "--quiet"]);
    crate::platform::process::hidden_command(&mut command);
    let output = crate::platform::process_output::read_cancellable(
        command,
        deadline.saturating_duration_since(Instant::now()),
        MAX_DISTRO_LIST,
        cancelled,
    )
    .ok()?;
    decode_distro_list(&output)
}

fn decode_distro_list(output: &[u8]) -> Option<Vec<String>> {
    let text = if output.len().is_multiple_of(2) && output.contains(&0) {
        let units: Vec<_> =
            output.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
        String::from_utf16(&units).ok()?
    } else {
        String::from_utf8(output.to_vec()).ok()?
    };
    Some(
        text.lines()
            .map(|line| line.trim().trim_start_matches('\u{feff}').trim())
            .filter(|line| !line.is_empty() && !line.chars().any(char::is_control))
            .map(str::to_owned)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_distro_output_accepts_utf8_and_windows_utf16() {
        assert_eq!(decode_distro_list(b"Ubuntu-22.04\r\nDebian\r\n").unwrap(), [
            "Ubuntu-22.04",
            "Debian"
        ]);
        let utf16: Vec<u8> =
            "\u{feff}Ubuntu-22.04\r\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(decode_distro_list(&utf16).unwrap(), ["Ubuntu-22.04"]);
        assert_eq!(decode_distro_list(&[0xff]), None);
    }

    #[test]
    fn launcher_candidates_must_be_unique_new_direct_children() {
        use crate::platform::process_snapshot::ProcessRow;
        let row = |pid, parent, executable: &str, created| ProcessRow {
            pid,
            parent,
            executable: executable.into(),
            created,
        };
        let boundary = NativeLaunchBoundary {
            time: 150,
            shell: ProcessStamp { pid: 10, start_time: 100 },
            existing_branches: std::collections::HashSet::from([ProcessStamp {
                pid: 20,
                start_time: 120,
            }]),
        };
        let old_and_new = vec![
            row(10, 1, "pwsh.exe", 100),
            row(20, 10, "wsl.exe", 120),
            row(30, 10, "wsl.exe", 200),
            row(40, 30, "wsl.exe", 210),
        ];
        assert!(matches!(
            select_launcher_candidate(10, &boundary, old_and_new),
            CandidateRows::One(executable, ProcessStamp { pid: 30, start_time: 200 })
                if executable == "wsl.exe"
        ));
        let mut ambiguous = vec![row(10, 1, "pwsh.exe", 100)];
        ambiguous.push(row(30, 10, "wsl.exe", 200));
        ambiguous.push(row(31, 10, "wsl.exe", 210));
        assert!(matches!(
            select_launcher_candidate(10, &boundary, ambiguous),
            CandidateRows::Ambiguous
        ));
        assert_eq!(
            select_launcher_candidate(10, &boundary, vec![row(10, 1, "pwsh.exe", 100)]),
            CandidateRows::None
        );
    }
}
