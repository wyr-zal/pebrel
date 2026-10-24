use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::ai_agents::AgentKind;
use crate::runtime_exec::PaneExecContext;

#[cfg(windows)]
mod native;
#[cfg(windows)]
pub(crate) use native::{NativeLaunchBoundary, NativeProcessIdentity, hook_owner};

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PROBE_OUTPUT: usize = 64 * 1024;
const MAX_PROBE_RECORDS: usize = 128;
const MAX_PROCESS_FIELD: usize = 4096;

const PROCESS_PROBE_SCRIPT: &str = r###"
pane_id=$1
process_instance=$2
command_id=$3
mode=${4:-discover}
shift 4
records=0
owned_any=0
tab=$(printf '\t')

echo_record() {
    records=$((records + 1))
    [ "$records" -le 128 ] || exit 2
    printf '%s\n' "$1"
}

owned_process() {
    [ -r "$1/environ" ] || return 2
    if [ -n "$command_id" ]; then
        environment=$(grep -z -F -x \
            -e "PEBREL_PANE_ID=$pane_id" \
            -e "NEBULA_PANE_ID=$pane_id" \
            -e "PEBREL_PROCESS_ID=$process_instance" \
            -e "PEBREL_COMMAND_ID=$command_id" \
            "$1/environ" 2>/dev/null)
    else
        environment=$(grep -z -F -x \
            -e "PEBREL_PANE_ID=$pane_id" \
            -e "NEBULA_PANE_ID=$pane_id" \
            -e "PEBREL_PROCESS_ID=$process_instance" \
            "$1/environ" 2>/dev/null)
    fi
    grep_status=$?
    [ "$grep_status" -le 1 ] || return 2
    case "$environment" in
        *"PEBREL_PANE_ID=$pane_id"*|*"NEBULA_PANE_ID=$pane_id"*) ;;
        *) return 1;;
    esac
    case "$environment" in *"PEBREL_PROCESS_ID=$process_instance"*) ;; *) return 1;; esac
    [ -z "$command_id" ] || case "$environment" in *"PEBREL_COMMAND_ID=$command_id"*) ;; *) return 1;; esac
}

process_stat() {
    stat_line=
    IFS= read -r stat_line < "$1/stat" || return 1
    stat_line=${stat_line##*) }
    set -- $stat_line
    [ "$#" -ge 20 ] || return 1
    stat_fields="$2 $3 $5 $6 ${20} $1"
}

read_argv() {
    IFS= read -r comm < "$1/comm" || return 1
    command_line=$(tr '\000' '\n' < "$1/cmdline" 2>/dev/null) || return 1
    newline='
'
    argv0=${command_line%%"$newline"*}
    argv1=
    if [ "$argv0" != "$command_line" ]; then
        rest=${command_line#*"$newline"}
        argv1=${rest%%"$newline"*}
    fi
}

read_cwd() {
    newline=$(printf '\nx')
    newline=${newline%x}
    cwd_value=$(readlink "$1" && printf '.') || return 2
    case "$cwd_value" in *.) cwd_value=${cwd_value%.};; *) return 2;; esac
    case "$cwd_value" in *"$newline") cwd_value=${cwd_value%"$newline"};; *) return 2;; esac
}

is_pty_root() (
    directory=$1
    process_stat "$directory" || exit 2
    before=$stat_fields
    set -- $before
    parent=$1; tty=$3
    [ "$tty" != 0 ] || exit 1
    [ -d "/proc/$parent" ] || exit 2
    process_stat "/proc/$parent" || exit 2
    parent_before=$stat_fields
    set -- $parent_before
    owned=1
    if [ "$3" = "$tty" ]; then
        owned_process "/proc/$parent"
        owned=$?
    fi
    [ "$owned" != 2 ] || exit 2
    process_stat "/proc/$parent" || exit 2
    parent_after=$stat_fields
    [ "$parent_before" = "$parent_after" ] || exit 1
    process_stat "$directory" || exit 2
    after=$stat_fields
    [ "$before" = "$after" ] || exit 1
    if [ "$owned" = 0 ]; then
        set -- $parent_before
        [ "$3" != "$tty" ] || exit 1
    fi
    exit 0
)

check_owner() {
    owner=$1; owner_start=$2; shell=$3; shell_start=$4; tty=$5; group=$6
    for pid in "$owner" "$shell"; do
        [ -d "/proc/$pid" ] || return 1
        owned_process "/proc/$pid" || return $?
    done
    process_stat "/proc/$owner" || return 2
    owner_stat=$stat_fields
    process_stat "/proc/$shell" || return 2
    shell_stat=$stat_fields
    set -- $owner_stat
    case "$6" in Z|X) return 1;; esac
    [ "$5" = "$owner_start" ] && [ "$3" = "$tty" ] && [ "$2" = "$group" ] && [ "$4" = "$group" ] || return 1
    set -- $shell_stat
    case "$6" in Z|X) return 1;; esac
    [ "$5" = "$shell_start" ] && [ "$3" = "$tty" ] && [ "$4" = "$group" ] || return 1
    IFS= read -r anchor_comm < "/proc/$shell/comm" || return 2
    if [ "$owner" = "$shell" ]; then
        is_pty_root "/proc/$shell" || return $?
    else
        case "$anchor_comm" in
            bash|sh|dash|zsh|fish|-bash|-sh|-dash|-zsh|-fish) ;;
            *) is_pty_root "/proc/$shell" || return $?;;
        esac
    fi
    parent=$owner
    depth=0
    ancestry=
    while [ "$parent" != "$shell" ]; do
        depth=$((depth + 1))
        [ "$depth" -le 64 ] || return 1
        [ -d "/proc/$parent" ] || return 1
        owned_process "/proc/$parent" || return $?
        process_stat "/proc/$parent" || return 2
        before=$stat_fields
        set -- $before
        case "$6" in Z|X) return 1;; esac
        [ "$3" = "$tty" ] && [ "$2" = "$group" ] && [ "$4" = "$group" ] || return 1
        next=$1
        process_stat "/proc/$parent" || return 2
        after=$stat_fields
        [ "$before" = "$after" ] || return 1
        ancestry="$ancestry$parent${tab}$before
"
        parent=$next
    done
    read_argv "/proc/$owner" || return 2
    process_stat "/proc/$owner" || return 2
    after=$stat_fields
    [ "$after" = "$owner_stat" ] || return 1
    process_stat "/proc/$shell" || return 2
    after=$stat_fields
    [ "$after" = "$shell_stat" ] || return 1
    owned_process "/proc/$owner" || return $?
    owned_process "/proc/$shell" || return $?
    printf '%s' "$ancestry" | while IFS="$tab" read -r pid expected; do
        process_stat "/proc/$pid" || exit 2
        current=$stat_fields
        [ "$current" = "$expected" ] || exit 1
    done || return $?
    printf 'C\t%s\t%s\t%s\n' "$comm" "$argv0" "$argv1"
}

if [ "$mode" = check ]; then
    check_owner "$@"
    code=$?
    case "$code" in 0) ;; 1) printf 'changed\n';; *) printf 'unavailable\n';; esac
    exit 0
fi
[ "$mode" = discover ] || exit 2

for proc in /proc/[0-9]*; do
    [ -d "$proc" ] || continue
    if ! process_stat "$proc"; then
        [ ! -d "$proc" ] && continue
        owned_process "$proc"
        owned=$?
        case "$owned" in 0) exit 2;; 2) echo_record U;; esac
        continue
    fi
    fields=$stat_fields
    set -- $fields
    [ "$3" != 0 ] || continue
    owned_process "$proc"
    owned=$?
    if [ "$owned" != 0 ]; then
        [ "$owned" = 2 ] || continue
        # Unreadable foreign jobs on other TTYs do not invalidate this pane.
        if [ "$2" = "$4" ]; then
            echo_record "U${tab}$3${tab}$2"
        fi
        continue
    fi
    owned_any=1
    pid=${proc##*/}
    set -- $fields
    case "$6" in Z|X) continue;; esac
    ppid=$1; pgrp=$2; tty_nr=$3; tpgid=$4; start=$5
    IFS= read -r comm < "$proc/comm" 2>/dev/null || exit 2
    shell_record=
    process_record=
    location_record=
    case "$comm" in
        bash|sh|dash|zsh|fish|-bash|-sh|-dash|-zsh|-fish)
            shell_record="S${tab}${pid}${tab}${ppid}"
            ;;
    esac
    if [ -z "$shell_record" ] && [ "$tty_nr" != 0 ]; then
        is_pty_root "$proc"
        root=$?
        case "$root" in
            0) shell_record="R${tab}${pid}${tab}${ppid}";;
            1) ;;
            *) exit 2;;
        esac
    fi
    if [ "$tty_nr" != 0 ] && [ "$pgrp" = "$tpgid" ]; then
        read_argv "$proc" || exit 2
        process_record="P${tab}${pid}${tab}${ppid}${tab}${comm}${tab}${argv0}${tab}${argv1}"
        if [ -n "$command_id" ]; then
            read_cwd "$proc/cwd" || exit 2
            cwd=$cwd_value
            [ "${#cwd}" -le 4096 ] || exit 2
            case "$cwd" in /*) ;; *) exit 2;; esac
            case "$cwd" in *[[:cntrl:]]*) exit 2;; esac
            cwd_encoded=$(printf '%s' "$cwd" | base64 | tr -d '\r\n') || exit 2
            read_cwd "$proc/cwd" || exit 2
            location_after=$cwd_value
            [ "$cwd" = "$location_after" ] || exit 2
            location_record="D${tab}${pid}${tab}${start}${tab}${cwd_encoded}"
        else
            location_record=
        fi
    fi
    [ -n "$shell_record$process_record" ] || continue
    process_stat "$proc" || exit 2
    after=$stat_fields
    [ "$fields" = "$after" ] || exit 2
    owned_process "$proc" || exit 2
    [ -z "$shell_record" ] || echo_record "$shell_record"
    [ -z "$process_record" ] || echo_record "$process_record"
    [ -z "$location_record" ] || echo_record "$location_record"
    echo_record "M${tab}${pid}${tab}${ppid}${tab}${start}${tab}${tty_nr}${tab}${pgrp}${tab}${tpgid}"
done
if [ "$owned_any" = 0 ]; then
    echo_record N
    echo_record E
    exit 0
fi
echo_record E
"###;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ProcessStamp {
    pub pid: u32,
    pub start_time: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForegroundAgent {
    pub kind: AgentKind,
    pub process: ProcessStamp,
    pub shell: ProcessStamp,
    pub tty_nr: i64,
    pub process_group: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentProbe {
    Found(ForegroundAgent),
    NoAgent,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentRecheck {
    Current,
    Changed,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ForegroundAgentLocationProbe {
    Found(ForegroundAgent, String),
    NoAgent,
    Unavailable,
}

/// A native takeover needs a command launch boundary and exactly one provider
/// in a shell branch created after it. Older background branches cannot take over.
#[cfg(windows)]
pub(crate) fn probe_native_receipt(
    shell_pid: u32,
    launched_after: Option<&NativeLaunchBoundary>,
) -> AgentProbe {
    let Some(boundary) = launched_after else { return AgentProbe::Unavailable };
    let Ok(rows) = super::process_snapshot::snapshot() else { return AgentProbe::Unavailable };
    native_receipt_from_rows(shell_pid, boundary, rows)
}

#[cfg(windows)]
fn native_receipt_from_rows(
    shell_pid: u32,
    boundary: &NativeLaunchBoundary,
    rows: Vec<super::process_snapshot::ProcessRow>,
) -> AgentProbe {
    let stamps: HashMap<_, _> = rows.iter().map(|row| (row.pid, row.created)).collect();
    let Some(&shell_start) = stamps.get(&shell_pid).filter(|start| **start != 0) else {
        return AgentProbe::Unavailable;
    };
    if boundary.shell != (ProcessStamp { pid: shell_pid, start_time: shell_start }) {
        return AgentProbe::Unavailable;
    }
    let Ok(tree) = crate::process_tree::descendants_from_rows(shell_pid, rows) else {
        return AgentProbe::Unavailable;
    };
    let mut candidates = Vec::new();
    let mut current_branches = HashSet::new();
    // descendants_from_rows is breadth first and verifies parent creation times.
    for row in tree.iter().filter(|row| row.depth > 0) {
        let Some(&created) = stamps.get(&row.pid).filter(|stamp| **stamp != 0) else {
            return AgentProbe::Unavailable;
        };
        if created <= boundary.time
            || boundary.existing_branches.contains(&ProcessStamp { pid: row.pid, start_time: created })
            || (row.parent_pid != shell_pid && !current_branches.contains(&row.parent_pid))
        {
            continue;
        }
        current_branches.insert(row.pid);
        // Codex's npm launcher directly owns its native CLI. The native child
        // emits hooks, so it must also own the independently observed receipt.
        if matches!(crate::process_tree::display_name(&row.executable).to_ascii_lowercase().as_str(), "node" | "nodejs")
            && tree.iter().any(|child| {
                child.parent_pid == row.pid
                    && AgentKind::parse(&child.executable) == Some(AgentKind::Codex)
            })
        {
            continue;
        }
        let Some(&created) = stamps.get(&row.pid) else { return AgentProbe::Unavailable };
        match native::process_kind(row.pid, created, &row.executable) {
            Ok(Some(kind)) => candidates.push((row, kind)),
            Ok(None) => {},
            Err(()) => return AgentProbe::Unavailable,
        }
    }
    if candidates.is_empty() { return AgentProbe::NoAgent; }
    let [(row, kind)] = candidates.as_slice() else { return AgentProbe::Unavailable };
    let Some(&start_time) = stamps.get(&row.pid).filter(|start| **start != 0) else {
        return AgentProbe::Unavailable;
    };
    AgentProbe::Found(ForegroundAgent {
        kind: *kind,
        process: ProcessStamp { pid: row.pid, start_time },
        shell: ProcessStamp { pid: shell_pid, start_time: shell_start },
        // These fields apply only to POSIX receipts; native checks use stamps.
        tty_nr: 0,
        process_group: 0,
    })
}

#[cfg(windows)]
pub(crate) fn recheck_native_receipt(
    expected: &ForegroundAgent,
    launched_after: Option<&NativeLaunchBoundary>,
) -> AgentRecheck {
    match probe_native_receipt(expected.shell.pid, launched_after) {
        AgentProbe::Found(current) if current == *expected => AgentRecheck::Current,
        AgentProbe::Found(_) | AgentProbe::NoAgent => AgentRecheck::Changed,
        AgentProbe::Unavailable => AgentRecheck::Unavailable,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessRecord {
    pid: u32,
    ppid: u32,
    executable: String,
    argv0: String,
    argv1: String,
}

#[derive(Clone, Copy)]
struct KernelRecord {
    ppid: u32,
    start_time: u64,
    tty_nr: i64,
    group: u32,
    foreground: i64,
}

/// Compatibility entrypoint; native callers retain their existing process-tree behavior.
pub(crate) fn probe_agent(
    pane_id: u64,
    context: &PaneExecContext,
    shell_pid: Option<u32>,
    cancelled: &dyn Fn() -> bool,
) -> Option<AgentKind> {
    context.process_instance()?;
    if context.wsl_distribution().is_some() || cfg!(target_os = "linux") {
        return match probe_foreground_agent(pane_id, context, cancelled) {
            AgentProbe::Found(agent) => Some(agent.kind),
            AgentProbe::NoAgent | AgentProbe::Unavailable => None,
        };
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        if cancelled() {
            return None;
        }
        probe_native_process_tree(shell_pid?)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = shell_pid;
        None
    }
}

/// An observation, never a session identity or proof that a turn is running.
pub(crate) fn probe_foreground_agent(
    pane_id: u64,
    context: &PaneExecContext,
    cancelled: &dyn Fn() -> bool,
) -> AgentProbe {
    probe_foreground_agent_snapshot(
        pane_id,
        context,
        Instant::now() + PROBE_TIMEOUT,
        cancelled,
    ).0
}

pub(crate) fn probe_foreground_agent_location(
    pane_id: u64,
    context: &PaneExecContext,
    deadline: Instant,
    cancelled: &dyn Fn() -> bool,
) -> ForegroundAgentLocationProbe {
    if context.command_id().is_none() {
        return ForegroundAgentLocationProbe::Unavailable;
    }
    let (result, cwd) = probe_foreground_agent_snapshot(pane_id, context, deadline, cancelled);
    match (result, cwd) {
        (AgentProbe::Found(agent), Some(cwd)) => {
            ForegroundAgentLocationProbe::Found(agent, cwd)
        },
        (AgentProbe::NoAgent, _) => ForegroundAgentLocationProbe::NoAgent,
        _ => ForegroundAgentLocationProbe::Unavailable,
    }
}

fn probe_foreground_agent_snapshot(
    pane_id: u64,
    context: &PaneExecContext,
    deadline: Instant,
    cancelled: &dyn Fn() -> bool,
) -> (AgentProbe, Option<String>) {
    let mut remaining = MAX_PROBE_OUTPUT;
    let Some(output) =
        read_probe(context, pane_id, &["discover".into()], deadline, &mut remaining, cancelled)
    else {
        return (AgentProbe::Unavailable, None);
    };
    let Some((result, cwd)) = parse_probe_snapshot(&output) else {
        return (AgentProbe::Unavailable, None);
    };
    let AgentProbe::Found(ref agent) = result else { return (result, None) };
    match recheck(context, pane_id, agent, deadline, &mut remaining, cancelled) {
        AgentRecheck::Current => (result, cwd),
        // A raced scan is not a complete negative observation.
        AgentRecheck::Changed | AgentRecheck::Unavailable => (AgentProbe::Unavailable, None),
    }
}

/// Re-read only the previously observed owner, anchor and bounded ancestry.
pub(crate) fn recheck_foreground_agent(
    pane_id: u64,
    context: &PaneExecContext,
    expected: &ForegroundAgent,
    cancelled: &dyn Fn() -> bool,
) -> AgentRecheck {
    let mut remaining = MAX_PROBE_OUTPUT;
    recheck(context, pane_id, expected, Instant::now() + PROBE_TIMEOUT, &mut remaining, cancelled)
}

fn recheck(
    context: &PaneExecContext,
    pane_id: u64,
    expected: &ForegroundAgent,
    deadline: Instant,
    remaining: &mut usize,
    cancelled: &dyn Fn() -> bool,
) -> AgentRecheck {
    let args = [
        "check".into(),
        expected.process.pid.to_string(),
        expected.process.start_time.to_string(),
        expected.shell.pid.to_string(),
        expected.shell.start_time.to_string(),
        expected.tty_nr.to_string(),
        expected.process_group.to_string(),
    ];
    let Some(output) = read_probe(context, pane_id, &args, deadline, remaining, cancelled) else {
        return AgentRecheck::Unavailable;
    };
    parse_recheck(&output, expected.kind)
}

fn parse_recheck(output: &str, expected: AgentKind) -> AgentRecheck {
    if output == "changed\n" {
        return AgentRecheck::Changed;
    }
    let Some(row) = output.strip_suffix('\n').filter(|row| !row.contains('\n')) else {
        return AgentRecheck::Unavailable;
    };
    let fields: Vec<_> = row.split('\t').collect();
    if fields.len() != 4
        || fields[0] != "C"
        || fields[1..].iter().any(|value| field(value).is_none())
    {
        return AgentRecheck::Unavailable;
    }
    if AgentKind::parse_process_argv(fields[1], fields[2], Some(fields[3])) == Some(expected) {
        AgentRecheck::Current
    } else {
        AgentRecheck::Changed
    }
}

fn read_probe(
    context: &PaneExecContext,
    pane_id: u64,
    args: &[String],
    deadline: Instant,
    remaining: &mut usize,
    cancelled: &dyn Fn() -> bool,
) -> Option<String> {
    if cancelled() || Instant::now() >= deadline || *remaining == 0 {
        return None;
    }
    let command = probe_command(context, pane_id, args)?;
    let output = crate::platform::process_output::read_cancellable(
        command,
        deadline.saturating_duration_since(Instant::now()),
        *remaining,
        cancelled,
    )
    .ok()?;
    *remaining = remaining.checked_sub(output.len())?;
    String::from_utf8(output).ok()
}

fn probe_command(context: &PaneExecContext, pane_id: u64, args: &[String]) -> Option<Command> {
    #[cfg(windows)]
    let mut command = {
        let distribution = context.wsl_distribution()?;
        let mut command = Command::new("wsl.exe");
        if let Some(distribution) = distribution {
            command.args(["--distribution", distribution]);
        }
        if let Some(user) = context.wsl_user() {
            command.args(["--user", user]);
        }
        command.args(["--exec", "sh"]);
        command
    };
    #[cfg(target_os = "linux")]
    let mut command = Command::new("/bin/sh");
    #[cfg(any(windows, target_os = "linux"))]
    {
        configure_process_probe(
            &mut command,
            pane_id,
            context.process_instance()?,
            context.command_id(),
            args,
        );
        #[cfg(windows)]
        if let Some(wslenv) = context.wslenv() {
            command.env("WSLENV", wslenv);
        }
        crate::platform::process::hidden_command(&mut command);
        Some(command)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (context, pane_id, args);
        None
    }
}

fn configure_process_probe(
    command: &mut Command,
    pane_id: u64,
    process_instance: &str,
    command_id: Option<&str>,
    args: &[String],
) {
    clear_probe_identity_environment(command);
    command
        .args(["-c", PROCESS_PROBE_SCRIPT, "pebrel-agent-process-probe"])
        .arg(pane_id.to_string())
        .arg(process_instance)
        .arg(command_id.unwrap_or_default());
    command.args(args);
}

fn clear_probe_identity_environment(command: &mut Command) {
    command
        .env_remove(crate::agent_env::PANE_ENV)
        .env_remove(crate::ai_hook::LEGACY_PANE_ENV)
        .env_remove(crate::agent_env::PROCESS_ENV)
        .env_remove(crate::agent_env::COMMAND_ID_ENV);
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

fn parse_records(
    output: &str,
) -> Result<(HashMap<u32, u32>, HashMap<u32, ProcessRecord>, HashSet<u32>), ()> {
    let mut shells = HashMap::new();
    let mut processes = HashMap::new();
    let mut roots = HashSet::new();
    for (index, line) in output.lines().enumerate() {
        if index >= MAX_PROBE_RECORDS {
            return Err(());
        }
        let mut fields = line.split('\t');
        match fields.next().ok_or(())? {
            tag @ ("S" | "R") => {
                let pid = fields.next().ok_or(())?.parse::<u32>().map_err(|_| ())?;
                let ppid = fields.next().ok_or(())?.parse::<u32>().map_err(|_| ())?;
                if fields.next().is_some() || pid == 0 || shells.insert(pid, ppid).is_some() {
                    return Err(());
                }
                if tag == "R" {
                    roots.insert(pid);
                }
            },
            "P" => {
                let record = ProcessRecord {
                    pid: fields.next().ok_or(())?.parse::<u32>().map_err(|_| ())?,
                    ppid: fields.next().ok_or(())?.parse::<u32>().map_err(|_| ())?,
                    executable: field(fields.next().ok_or(())?).ok_or(())?,
                    argv0: field(fields.next().ok_or(())?).ok_or(())?,
                    argv1: field(fields.next().ok_or(())?).ok_or(())?,
                };
                if fields.next().is_some()
                    || record.pid == 0
                    || record.ppid == 0
                    || processes.insert(record.pid, record).is_some()
                {
                    return Err(());
                }
            },
            _ => return Err(()),
        }
    }
    Ok((shells, processes, roots))
}

fn resolve_owner(
    shells: &HashMap<u32, u32>,
    processes: &HashMap<u32, ProcessRecord>,
    roots: &HashSet<u32>,
) -> Result<Option<(AgentKind, u32, u32)>, ()> {
    let mut candidates = Vec::new();
    for process in processes.values() {
        if let Some(agent) =
            AgentKind::parse_process_argv(&process.executable, &process.argv0, Some(&process.argv1))
        {
            let (depth, shell) = if roots.contains(&process.pid) {
                (0, process.pid)
            } else {
                depth_to_shell(process, processes, shells).ok_or(())?
            };
            candidates.push((depth, agent, process.pid, shell));
        }
    }
    let Some(depth) = candidates.iter().map(|candidate| candidate.0).min() else { return Ok(None) };
    let mut nearest = candidates.into_iter().filter(|candidate| candidate.0 == depth);
    let (_, kind, pid, shell) = nearest.next().ok_or(())?;
    if nearest.next().is_some() {
        return Err(());
    }
    Ok(Some((kind, pid, shell)))
}

#[cfg(test)]
fn resolve_process_records(output: &str) -> Option<AgentKind> {
    let (shells, processes, roots) = parse_records(output).ok()?;
    resolve_owner(&shells, &processes, &roots).ok()?.map(|owner| owner.0)
}

fn parse_probe(output: &str) -> AgentProbe {
    parse_probe_snapshot(output)
        .map(|(probe, _)| probe)
        .unwrap_or(AgentProbe::Unavailable)
}

fn parse_probe_snapshot(output: &str) -> Option<(AgentProbe, Option<String>)> {
    if output.len() > MAX_PROBE_OUTPUT || !output.ends_with("E\n") {
        return None;
    }
    let mut records = String::new();
    let mut kernel = HashMap::new();
    let mut unreadable = Vec::new();
    let mut cwd_by_process = HashMap::new();
    let mut ended = false;
    let mut no_owner = false;
    for (index, line) in output.lines().enumerate() {
        if index >= MAX_PROBE_RECORDS || ended {
            return None;
        }
        if line == "E" {
            ended = true;
            continue;
        }
        if line == "N" {
            no_owner = true;
            continue;
        }
        if line == "U" {
            return None;
        }
        if let Some(values) = line.strip_prefix("U\t") {
            let (tty, group) = values.split_once('\t')?;
            unreadable.push((tty.parse::<i64>().ok()?, group.parse::<u32>().ok()?));
        } else if let Some(values) = line.strip_prefix("D\t") {
            use base64::Engine as _;
            let mut fields = values.split('\t');
            let stamp = ProcessStamp {
                pid: fields.next()?.parse().ok()?,
                start_time: fields.next()?.parse().ok()?,
            };
            let encoded = fields.next()?;
            if fields.next().is_some() || stamp.pid == 0 || stamp.start_time == 0 {
                return None;
            }
            let cwd = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(encoded).ok()?)
                .ok()
                .filter(|cwd| cwd.len() <= MAX_PROCESS_FIELD && cwd.starts_with('/')
                    && !cwd.chars().any(char::is_control))?;
            if cwd_by_process.insert(stamp, cwd).is_some() {
                return None;
            }
        } else if let Some(values) = line.strip_prefix("M\t") {
            let mut fields = values.split('\t');
            let pid = fields.next()?.parse::<u32>().ok()?;
            let record = KernelRecord {
                ppid: fields.next()?.parse().ok()?,
                start_time: fields.next()?.parse().ok()?,
                tty_nr: fields.next()?.parse().ok()?,
                group: fields.next()?.parse().ok()?,
                foreground: fields.next()?.parse().ok()?,
            };
            if fields.next().is_some()
                || pid == 0
                || record.start_time == 0
                || record.group == 0
                || kernel.insert(pid, record).is_some()
            {
                return None;
            }
        } else {
            records.push_str(line);
            records.push('\n');
        }
    }
    if no_owner {
        if records.is_empty()
            && kernel.is_empty()
            && unreadable.is_empty()
            && cwd_by_process.is_empty()
        {
            return Some((AgentProbe::NoAgent, None));
        }
        return None;
    }
    let (shells, processes, roots) = parse_records(&records).ok()?;
    if shells.is_empty()
        || shells
            .iter()
            .any(|(pid, ppid)| kernel.get(pid).is_none_or(|record| record.ppid != *ppid))
        || processes.values().any(|process| {
            kernel.get(&process.pid).is_none_or(|record| record.ppid != process.ppid)
        })
    {
        return None;
    }
    if unreadable.iter().any(|(tty, group)| {
        shells
            .keys()
            .filter_map(|pid| kernel.get(pid))
            .any(|shell| shell.tty_nr == *tty && shell.foreground == i64::from(*group))
    }) {
        return None;
    }
    if let Some(root) = roots.iter().next() {
        let anchor = kernel.get(root)?;
        if roots.len() != 1
            || anchor.tty_nr == 0
            || kernel.get(&anchor.ppid).is_some_and(|parent| parent.tty_nr == anchor.tty_nr)
            || shells
                .keys()
                .filter_map(|pid| kernel.get(pid))
                .any(|other| other.tty_nr != 0 && other.tty_nr != anchor.tty_nr)
        {
            return None;
        }
    }
    let Some((kind, pid, shell)) = resolve_owner(&shells, &processes, &roots).ok()? else {
        // Missing the foreground group is an incomplete observation, not absence.
        let anchors: Vec<_> = shells
            .keys()
            .filter_map(|pid| kernel.get(pid))
            .filter(|record| record.tty_nr != 0)
            .collect();
        if anchors.is_empty()
            || anchors.iter().any(|anchor| {
                anchor.foreground <= 0
                    || (i64::from(anchor.group) != anchor.foreground
                        && !processes.keys().filter_map(|pid| kernel.get(pid)).any(|process| {
                            process.tty_nr == anchor.tty_nr
                                && i64::from(process.group) == anchor.foreground
                        }))
            })
        {
            return None;
        }
        return Some((AgentProbe::NoAgent, None));
    };
    let process = kernel.get(&pid)?;
    let anchor = kernel.get(&shell)?;
    if process.tty_nr == 0
        || process.tty_nr != anchor.tty_nr
        || i64::from(process.group) != process.foreground
        || process.foreground != anchor.foreground
    {
        return None;
    }
    let agent = ForegroundAgent {
        kind,
        process: ProcessStamp { pid, start_time: process.start_time },
        shell: ProcessStamp { pid: shell, start_time: anchor.start_time },
        tty_nr: process.tty_nr,
        process_group: process.group,
    };
    let cwd = cwd_by_process.remove(&agent.process);
    Some((AgentProbe::Found(agent), cwd))
}

fn field(value: &str) -> Option<String> {
    (value.len() <= MAX_PROCESS_FIELD && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}

fn depth_to_shell(
    process: &ProcessRecord,
    processes: &HashMap<u32, ProcessRecord>,
    shells: &HashMap<u32, u32>,
) -> Option<(u32, u32)> {
    let mut parent = process.ppid;
    let mut depth = 1;
    let mut visited = HashSet::new();
    visited.insert(process.pid);
    loop {
        if shells.contains_key(&parent) {
            return Some((depth, parent));
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
#[path = "agent_process_identity/tests.rs"]
mod tests;
