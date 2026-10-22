//! Kernel-stamped native identities and bounded interpreter entrypoint reads.
use super::{AgentKind, ProcessStamp};
use std::{mem, ptr};
use windows_sys::Wdk::System::Threading::{
    NtQueryInformationProcess, ProcessCommandLineInformation,
};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree, UNICODE_STRING};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

/// Same kernel clock and units as process creation stamps (100 ns FILETIME).
fn launch_boundary() -> u64 {
    let mut time = unsafe { mem::zeroed() };
    unsafe { windows_sys::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime(&mut time) };
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeLaunchBoundary {
    pub time: u64,
    pub shell: ProcessStamp,
    pub existing_branches: std::collections::HashSet<ProcessStamp>,
}

impl NativeLaunchBoundary {
    /// Capture before PTY submission. The snapshot also excludes old branches
    /// when system-clock correction makes their FILETIME newer than `time`.
    pub(crate) fn capture(shell_pid: u32) -> Option<Self> {
        let time = launch_boundary();
        let rows = crate::platform::process_snapshot::snapshot().ok()?;
        let shell = rows.iter().find(|row| row.pid == shell_pid && row.created != 0)?;
        if rows.iter().any(|row| row.parent == shell_pid && row.created == 0) { return None; }
        let existing_branches = rows.iter().filter(|row| row.parent == shell_pid)
            .map(|row| ProcessStamp { pid: row.pid, start_time: row.created }).collect();
        Some(Self {
            time,
            shell: ProcessStamp { pid: shell_pid, start_time: shell.created },
            existing_branches,
        })
    }
}

pub(crate) struct NativeProcessIdentity(HANDLE, ProcessStamp);

impl Drop for NativeProcessIdentity {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

impl NativeProcessIdentity {
    pub(crate) fn open(pid: u32) -> Option<Self> {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return None;
        }
        let mut identity = Self(handle, ProcessStamp { pid, start_time: 0 });
        identity.1.start_time = identity.creation_time()?;
        Some(identity)
    }

    pub(crate) fn stamp(&self) -> ProcessStamp {
        self.1
    }

    fn creation_time(&self) -> Option<u64> {
        let mut times = [unsafe { mem::zeroed() }; 4];
        let [created, exited, kernel, user] = &mut times;
        (unsafe { GetProcessTimes(self.0, created, exited, kernel, user) } != 0)
            .then_some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
            .filter(|created| *created != 0)
    }

    fn command_line(&self) -> Option<String> {
        const MAX_BYTES: usize = 32 * 1024;
        let mut needed = 0;
        unsafe {
            NtQueryInformationProcess(
                self.0,
                ProcessCommandLineInformation,
                ptr::null_mut(),
                0,
                &mut needed,
            )
        };
        if needed as usize > MAX_BYTES || (needed as usize) < mem::size_of::<UNICODE_STRING>() {
            return None;
        }
        let mut buffer = vec![0usize; (needed as usize).div_ceil(mem::size_of::<usize>())];
        let bytes = mem::size_of_val(buffer.as_slice());
        let status = unsafe {
            NtQueryInformationProcess(
                self.0,
                ProcessCommandLineInformation,
                buffer.as_mut_ptr().cast(),
                bytes as u32,
                &mut needed,
            )
        };
        if status < 0 || needed as usize > bytes {
            return None;
        }
        let value = unsafe { ptr::read_unaligned(buffer.as_ptr().cast::<UNICODE_STRING>()) };
        let offset = (value.Buffer as usize).checked_sub(buffer.as_ptr() as usize)?;
        let length = usize::from(value.Length);
        if length == 0
            || length % 2 != 0
            || offset % 2 != 0
            || offset < mem::size_of::<UNICODE_STRING>()
            || offset.checked_add(length)? > needed as usize
        {
            return None;
        }
        let raw = unsafe { std::slice::from_raw_parts(value.Buffer, length / 2) };
        let command = String::from_utf16(raw).ok()?;
        (self.creation_time() == Some(self.1.start_time)).then_some(command)
    }

    pub(crate) fn command_line_arguments(&self) -> Option<Vec<String>> {
        let command = self.command_line()?;
        let mut count = 0;
        let raw = unsafe { CommandLineToArgvW(wide(&command).as_ptr(), &mut count) };
        if raw.is_null() || count <= 0 || count > 256 {
            if !raw.is_null() {
                unsafe { LocalFree(raw.cast()) };
            }
            return None;
        }
        struct Arguments(*mut *mut u16);
        impl Drop for Arguments {
            fn drop(&mut self) {
                unsafe { LocalFree(self.0.cast()) };
            }
        }
        let _arguments = Arguments(raw);
        (0..count)
            .map(|index| {
                let pointer = unsafe { ptr::read(raw.add(index as usize)) };
                (!pointer.is_null()).then(|| unsafe {
                    let mut length = 0usize;
                    while *pointer.add(length) != 0 {
                        length += 1;
                    }
                    String::from_utf16(std::slice::from_raw_parts(pointer, length)).ok()
                })?
            })
            .collect()
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn entrypoint_kind(executable: &str, command: &str) -> Option<AgentKind> {
    if command.contains('\0') {
        return None;
    }
    let mut wide: Vec<_> = command.encode_utf16().collect();
    wide.push(0);
    unsafe {
        let mut count = 0;
        let argv = CommandLineToArgvW(wide.as_ptr(), &mut count);
        if argv.is_null() {
            return None;
        }
        let args = std::slice::from_raw_parts(argv, count.max(0) as usize);
        let mut values = Vec::new();
        for &arg in args.iter().take(2) {
            let mut len = 0;
            while *arg.add(len) != 0 {
                len += 1;
            }
            values.push(String::from_utf16_lossy(std::slice::from_raw_parts(arg, len)));
        }
        LocalFree(argv.cast());
        AgentKind::parse_process_argv(
            executable,
            values.first()?.as_str(),
            values.get(1).map(String::as_str),
        )
    }
}

pub(super) fn process_kind(
    pid: u32,
    created: u64,
    executable: &str,
) -> Result<Option<AgentKind>, ()> {
    let name = crate::process_tree::display_name(executable);
    if let Some(kind) = AgentKind::parse(&name) {
        return Ok(Some(kind));
    }
    if !matches!(name.to_ascii_lowercase().as_str(), "node" | "nodejs" | "bun" | "deno") {
        return Ok(None);
    }
    let process = NativeProcessIdentity::open(pid).ok_or(())?;
    if process.stamp().start_time != created {
        return Err(());
    }
    let command = process.command_line().ok_or(())?;
    Ok(entrypoint_kind(executable, &command))
}

pub(crate) fn hook_owner(client: ProcessStamp) -> Option<(ProcessStamp, AgentKind)> {
    let rows = crate::platform::process_snapshot::snapshot().ok()?;
    let rows: std::collections::HashMap<_, _> =
        rows.into_iter().map(|row| (row.pid, row)).collect();
    if rows.get(&client.pid)?.created != client.start_time {
        return None;
    }
    let mut current = client.pid;
    for _ in 0..48 {
        let row = rows.get(&current)?;
        if row.created == 0 {
            return None;
        }
        if let Some(kind) = process_kind(row.pid, row.created, &row.executable).ok()? {
            return Some((ProcessStamp { pid: row.pid, start_time: row.created }, kind));
        }
        let parent = rows.get(&row.parent)?;
        if parent.pid == current || parent.created == 0 || parent.created > row.created {
            return None;
        }
        current = parent.pid;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_entrypoints_require_the_known_cli_path() {
        assert_eq!(
            entrypoint_kind(
                "node.exe",
                r#"node.exe "C:\Program Files\node_modules\@anthropic-ai\claude-code\cli.js""#
            ),
            Some(AgentKind::Claude)
        );
        assert_eq!(entrypoint_kind("node.exe", r#"node.exe C:\work\server.js claude"#), None);
    }

    #[test]
    fn native_command_line_is_bound_to_the_open_process_instance() {
        let process = NativeProcessIdentity::open(std::process::id()).unwrap();
        assert_ne!(process.stamp().start_time, 0);
        assert!(process.command_line().is_some());
        assert!(!process.command_line_arguments().unwrap().is_empty());
        assert_eq!(
            process_kind(std::process::id(), process.stamp().start_time + 1, "node.exe"),
            Err(())
        );
    }

    #[test]
    #[ignore = "requires an explicit local PEBREL_NATIVE_IDENTITY_NODE executable"]
    fn actual_node_cli_receipt_and_hook_owner_use_the_same_kernel_instance() {
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let node =
            std::env::var_os("PEBREL_NATIVE_IDENTITY_NODE").expect("explicit Node executable");
        let helper = std::env::var_os("PEBREL_NATIVE_IDENTITY_HELPER")
            .expect("explicit local helper executable");
        let events = crate::ai_hook::spawn_gpui_server();
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("node_modules/@anthropic-ai/claude-code/cli.js");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        let marker = root.path().join("ready");
        std::fs::write(
            &script,
            r#"const fs = require('fs'), cp = require('child_process');
fs.writeFileSync(process.argv[2], 'ready');
for (let i=0; i<20; i++) cp.execFileSync(process.argv[3], ['claude'], {
  input: JSON.stringify({hook_event_name:'SessionStart',session_id:'native-pipe-test',bridge_sequence:i+1,occurred_at_ms:Date.now()}),
  windowsHide:true
});
setInterval(()=>{},1000);"#,
        )
        .unwrap();
        let mut command = std::process::Command::new(node);
        command.arg(&script).arg(&marker).arg(&helper)
            .env("PEBREL_PANE_ID", "42").env("NEBULA_PANE_ID", "42");
        crate::platform::process::hidden_command(&mut command);
        let boundary = NativeLaunchBoundary::capture(std::process::id()).unwrap();
        let child = Child(command.spawn().unwrap());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !marker.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(marker.exists(), "isolated Node CLI must reach its readiness marker");
        let process = NativeProcessIdentity::open(child.0.id()).unwrap();
        assert_eq!(
            process_kind(child.0.id(), process.stamp().start_time, "node.exe"),
            Ok(Some(AgentKind::Claude))
        );
        assert_eq!(hook_owner(process.stamp()), Some((process.stamp(), AgentKind::Claude)));
        let super::super::AgentProbe::Found(receipt) =
            super::super::probe_native_receipt(std::process::id(), Some(&boundary))
        else {
            panic!("the unique isolated Node CLI must yield a native receipt");
        };
        assert_eq!(receipt.process, process.stamp());
        assert_eq!(receipt.kind, AgentKind::Claude);
        let mut delays = Vec::new();
        for sequence in 1..=20 {
            let event = events.recv_timeout(std::time::Duration::from_secs(3))
                .expect("each real short-lived helper must deliver its event");
            assert_eq!(event.agent_pid, Some(process.stamp().pid));
            assert_eq!(event.agent_start_time, Some(process.stamp().start_time));
            assert_eq!(event.bridge_sequence, Some(sequence));
            assert_ne!(event.client_pid, event.agent_pid);
            delays.push(event.received_at_ms.saturating_sub(event.occurred_at_ms.unwrap()));
        }
        delays.sort_unstable();
        println!("real native helper pipe: n=20 p50={}ms p95={}ms max={}ms, owner stamps and sequence 20/20",
            delays[10], delays[18], delays[19]);
        let mut captures = Vec::new();
        for _ in 0..100 {
            let started = std::time::Instant::now();
            assert!(NativeLaunchBoundary::capture(std::process::id()).is_some());
            captures.push(started.elapsed().as_micros());
        }
        captures.sort_unstable();
        println!("recovery submission capture: n=100 p50={}us p95={}us max={}us",
            captures[50], captures[95], captures[99]);
        // Measure the complete snapshot + Node entrypoint hot path, without
        // provider traffic or changing any user's running process.
        let mut samples = Vec::new();
        for _ in 0..100 {
            let started = std::time::Instant::now();
            assert_eq!(hook_owner(process.stamp()), Some((process.stamp(), AgentKind::Claude)));
            samples.push(started.elapsed().as_micros());
        }
        samples.sort_unstable();
        println!("native Hook owner: n=100 p50={}us p95={}us max={}us total={}us",
            samples[50], samples[95], samples[99], samples.iter().sum::<u128>());

        // Four panes emitting at once exercise the serial accept loop: the
        // server pins each client's kernel identity before reading, and the
        // helper only retries a raced connect for about 100 ms. Sequence
        // continuity proves no event is dropped behind that queue; the delay
        // spread shows what the queueing costs. Runs last so the serial
        // numbers above stay comparable.
        std::fs::write(
            &script,
            r#"const fs = require('fs'), cp = require('child_process');
const [,, marker, helper, count] = process.argv;
fs.writeFileSync(marker, 'ready');
for (let i=0; i<Number(count); i++) cp.execFileSync(helper, ['claude'], {
  input: JSON.stringify({hook_event_name:'SessionStart',session_id:'pane-'+process.env.NEBULA_PANE_ID,bridge_sequence:i+1,occurred_at_ms:Date.now()}),
  windowsHide:true
});
setInterval(()=>{},1000);"#,
        )
        .unwrap();
        const PANES: u64 = 4;
        const PER_PANE: u64 = 5;
        let panes: Vec<_> = (1..=PANES)
            .map(|pane| {
                let pane_marker = root.path().join(format!("ready-{pane}"));
                let mut command = std::process::Command::new(
                    std::env::var_os("PEBREL_NATIVE_IDENTITY_NODE").expect("explicit Node executable"),
                );
                command
                    .arg(&script)
                    .arg(&pane_marker)
                    .arg(&helper)
                    .arg(PER_PANE.to_string())
                    .env("PEBREL_PANE_ID", pane.to_string())
                    .env("NEBULA_PANE_ID", pane.to_string());
                crate::platform::process::hidden_command(&mut command);
                (pane, pane_marker, Child(command.spawn().unwrap()))
            })
            .collect();
        let ready = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !panes.iter().all(|(_, marker, _)| marker.exists()) {
            assert!(std::time::Instant::now() < ready, "every pane CLI must reach its marker");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let stamps: Vec<_> = panes
            .iter()
            .map(|(pane, _, child)| {
                (*pane, NativeProcessIdentity::open(child.0.id()).unwrap().stamp())
            })
            .collect();
        let mut burst = Vec::new();
        for _ in 0..PANES * PER_PANE {
            burst.push(
                events
                    .recv_timeout(std::time::Duration::from_secs(3))
                    .expect("every concurrent helper must deliver its event"),
            );
        }
        for (pane, stamp) in &stamps {
            let mut mine: Vec<_> = burst.iter().filter(|event| event.pane == Some(*pane)).collect();
            mine.sort_by_key(|event| event.bridge_sequence);
            assert_eq!(mine.len() as u64, PER_PANE, "pane {pane} must deliver every event");
            for (index, event) in mine.iter().enumerate() {
                assert_eq!(event.bridge_sequence, Some(index as u64 + 1));
                assert_eq!(event.agent_pid, Some(stamp.pid));
                assert_eq!(event.agent_start_time, Some(stamp.start_time));
                assert_ne!(event.client_pid, event.agent_pid);
            }
        }
        let mut delays: Vec<_> = burst
            .iter()
            .map(|event| event.received_at_ms.saturating_sub(event.occurred_at_ms.unwrap()))
            .collect();
        delays.sort_unstable();
        let n = delays.len();
        println!("concurrent panes: panes={PANES} events={n} p50={}ms p95={}ms max={}ms, sequence and owner stamps {n}/{n}",
            delays[n / 2], delays[n * 95 / 100], delays[n - 1]);
    }
}
