use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::time::Duration;

use crate::ai_agents::AgentKind;
use crate::runtime_exec::PaneExecContext;

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PROBE_OUTPUT: usize = 64 * 1024;
const MAX_PROBE_RECORDS: usize = 128;
const MAX_PROCESS_FIELD: usize = 4096;

const PROCESS_PROBE_SCRIPT: &str = r###"
pane_id=$1
process_instance=$2
records=0
tab=$(printf '\t')

echo_record() {
    records=$((records + 1))
    [ "$records" -le 128 ] || exit 2
    printf '%s\n' "$1"
}

owned_process() {
    [ -r "$1/environ" ] || return 1
    environment=$(tr '\000' '\n' < "$1/environ" 2>/dev/null) || return 1
    printf '%s\n' "$environment" | grep -F -x -e "PEBREL_PANE_ID=$pane_id" -e "NEBULA_PANE_ID=$pane_id" >/dev/null 2>&1 || return 1
    printf '%s\n' "$environment" | grep -F -x -e "PEBREL_PROCESS_ID=$process_instance" >/dev/null 2>&1
}

process_stat() {
    stat_line=
    IFS= read -r stat_line < "$1/stat" || return 1
    stat_line=${stat_line##*) }
    set -- $stat_line
    [ "$#" -ge 6 ] || return 1
    printf '%s %s %s %s' "$2" "$3" "$5" "$6"
}

for proc in /proc/[0-9]*; do
    [ -d "$proc" ] || continue
    pid=${proc##*/}
    fields=$(process_stat "$proc") || continue
    set -- $fields
    ppid=$1
    pgrp=$2
    tty_nr=$3
    tpgid=$4
    IFS= read -r comm < "$proc/comm" 2>/dev/null || continue
    if owned_process "$proc"; then
        case "$comm" in
            bash|sh|dash|zsh|fish|-bash|-sh|-dash|-zsh|-fish)
                echo_record "S${tab}${pid}${tab}${ppid}"
                ;;
        esac
        if [ "$tty_nr" != 0 ] && [ "$pgrp" = "$tpgid" ] && [ -r "$proc/cmdline" ]; then
            command_line=$(tr '\000' '\n' < "$proc/cmdline" 2>/dev/null) || continue
            newline='
'
            argv0=${command_line%%"$newline"*}
            argv1=
            if [ "$argv0" != "$command_line" ]; then
                rest=${command_line#*"$newline"}
                argv1=${rest%%"$newline"*}
            fi
            echo_record "P${tab}${pid}${tab}${ppid}${tab}${comm}${tab}${argv0}${tab}${argv1}"
        fi
    fi
done
"###;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessRecord {
    pid: u32,
    ppid: u32,
    executable: String,
    argv0: String,
    argv1: String,
}

/// Read identity from the current pane's process tree or its confirmed WSL guest.
pub(crate) fn probe_agent(
    pane_id: u64,
    context: &PaneExecContext,
    shell_pid: Option<u32>,
    cancelled: &dyn Fn() -> bool,
) -> Option<AgentKind> {
    let pane_id = pane_id.to_string();
    let instance = context.process_instance()?;
    if let Some(distribution) = context.wsl_distribution() {
        #[cfg(windows)]
        {
            return probe_wsl(distribution, context.wsl_user(), &pane_id, instance, cancelled);
        }
        #[cfg(target_os = "linux")]
        {
            let _ = (distribution, context.wsl_user());
            return probe_local_proc(&pane_id, instance, cancelled);
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = (distribution, context.wsl_user());
            return None;
        }
    }

    #[cfg(target_os = "linux")]
    {
        let _ = shell_pid;
        return probe_local_proc(&pane_id, instance, cancelled);
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        let _ = (pane_id, instance);
        if cancelled() {
            return None;
        }
        probe_native_process_tree(shell_pid?)
    }
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    {
        let _ = (pane_id, instance, shell_pid);
        None
    }
}

#[cfg(target_os = "linux")]
fn probe_local_proc(
    pane_id: &str,
    instance: &str,
    cancelled: &dyn Fn() -> bool,
) -> Option<AgentKind> {
    let mut command = Command::new("/bin/sh");
    command.args([
        "-c",
        PROCESS_PROBE_SCRIPT,
        "pebrel-agent-process-probe",
        pane_id,
        instance,
    ]);
    crate::platform::process::hidden_command(&mut command);
    run_probe(command, cancelled)
}

#[cfg(windows)]
fn probe_wsl(
    distribution: Option<&str>,
    user: Option<&str>,
    pane_id: &str,
    instance: &str,
    cancelled: &dyn Fn() -> bool,
) -> Option<AgentKind> {
    let mut command = Command::new("wsl.exe");
    if let Some(distribution) = distribution {
        command.args(["--distribution", distribution]);
    }
    if let Some(user) = user {
        command.args(["--user", user]);
    }
    command.args([
        "--exec",
        "sh",
        "-c",
        PROCESS_PROBE_SCRIPT,
        "pebrel-agent-process-probe",
        pane_id,
        instance,
    ]);
    crate::platform::process::hidden_command(&mut command);
    run_probe(command, cancelled)
}

#[cfg(any(windows, target_os = "linux"))]
fn run_probe(command: Command, cancelled: &dyn Fn() -> bool) -> Option<AgentKind> {
    let output = crate::platform::process_output::read_cancellable(
        command,
        PROBE_TIMEOUT,
        MAX_PROBE_OUTPUT,
        cancelled,
    )
    .ok()?;
    resolve_process_records(&String::from_utf8_lossy(&output))
}

#[cfg(any(windows, target_os = "macos"))]
fn probe_native_process_tree(shell_pid: u32) -> Option<AgentKind> {
    let descendants = crate::process_tree::descendants(shell_pid).ok()?;
    nearest_agent(descendants.iter().filter_map(|process| {
        (process.depth > 0)
            .then(|| AgentKind::parse(&process.executable).map(|agent| (process.depth, agent)))
            .flatten()
    }))
}

fn resolve_process_records(output: &str) -> Option<AgentKind> {
    let mut shells = HashSet::new();
    let mut processes = HashMap::new();
    let mut records = 0;
    for line in output.lines() {
        records += 1;
        if records > MAX_PROBE_RECORDS {
            return None;
        }
        let mut fields = line.split('\t');
        match fields.next()? {
            "S" => {
                let pid = fields.next()?.parse::<u32>().ok()?;
                let _ppid = fields.next()?.parse::<u32>().ok()?;
                if fields.next().is_some() || pid == 0 {
                    return None;
                }
                shells.insert(pid);
            },
            "P" => {
                let record = ProcessRecord {
                    pid: fields.next()?.parse::<u32>().ok()?,
                    ppid: fields.next()?.parse::<u32>().ok()?,
                    executable: field(fields.next()?)?,
                    argv0: field(fields.next()?)?,
                    argv1: field(fields.next()?)?,
                };
                if fields.next().is_some()
                    || record.pid == 0
                    || record.ppid == 0
                    || processes.insert(record.pid, record).is_some()
                {
                    return None;
                }
            },
            _ => return None,
        }
    }

    let candidates = processes.values().filter_map(|process| {
        let depth = depth_to_shell(process, &processes, &shells)?;
        let agent = AgentKind::parse_process_argv(
            &process.executable,
            &process.argv0,
            Some(&process.argv1),
        )?;
        Some((depth, agent))
    });
    nearest_agent(candidates)
}

fn field(value: &str) -> Option<String> {
    (value.len() <= MAX_PROCESS_FIELD && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}

fn depth_to_shell(
    process: &ProcessRecord,
    processes: &HashMap<u32, ProcessRecord>,
    shells: &HashSet<u32>,
) -> Option<u32> {
    let mut parent = process.ppid;
    let mut depth = 1;
    let mut visited = HashSet::new();
    visited.insert(process.pid);
    loop {
        if shells.contains(&parent) {
            return Some(depth);
        }
        if depth >= 64 || !visited.insert(parent) {
            return None;
        }
        parent = processes.get(&parent)?.ppid;
        depth += 1;
    }
}

fn nearest_agent(candidates: impl IntoIterator<Item = (u32, AgentKind)>) -> Option<AgentKind> {
    let mut depth = u32::MAX;
    let mut selected = None;
    for (candidate_depth, candidate) in candidates {
        if candidate_depth < depth {
            depth = candidate_depth;
            selected = Some(candidate);
        } else if candidate_depth == depth && selected != Some(candidate) {
            return None;
        }
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versioned_claude_argv_is_bound_to_the_current_shell() {
        assert_eq!(
            resolve_process_records(
                "S\t100\t1\nP\t200\t100\t2.1.294\tclaude\t--dangerously-skip-permissions\n"
            ),
            Some(AgentKind::Claude)
        );
    }

    #[test]
    fn node_entrypoint_requires_the_known_claude_package_path() {
        assert_eq!(
            resolve_process_records(
                "S\t100\t1\nP\t200\t100\tnode\t/usr/bin/node\t/opt/node_modules/@anthropic-ai/claude-code/cli.js\n"
            ),
            Some(AgentKind::Claude)
        );
        assert_eq!(
            resolve_process_records(
                "S\t100\t1\nP\t200\t100\tnode\tnode\t/home/claude/server.js\n"
            ),
            None
        );
    }

    #[test]
    fn nested_agents_do_not_replace_the_shells_nearest_agent() {
        assert_eq!(
            resolve_process_records(
                "S\t100\t1\nP\t200\t100\tclaude\tclaude\t--resume\nP\t201\t200\tcodex.exe\tcodex\tresume\n"
            ),
            Some(AgentKind::Claude)
        );
    }

    #[test]
    fn ambiguous_or_unowned_processes_do_not_claim_an_identity() {
        assert_eq!(
            resolve_process_records(
                "S\t100\t1\nP\t200\t100\tclaude\tclaude\t\nP\t201\t100\tcodex.exe\tcodex\t\n"
            ),
            None
        );
        assert_eq!(
            resolve_process_records("S\t100\t1\nP\t200\t900\tclaude\tclaude\t\n"),
            None
        );
        assert_eq!(resolve_process_records("P\t200\t100\tclaude\tclaude\t\n"), None);
    }
}
