//! Helper capability inspection. Public reads never perform I/O.
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HelperHealthState {
    #[default]
    Checking,
    Compatible,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HelperHealthReason {
    MissingFile,
    UnsupportedProtocol,
    MissingNativeCapability,
    MissingWslCapability,
    ProbeFailed,
    ProbeTimeout,
    OutputTooLarge,
    ChangedDuringProbe,
    UninspectedPath,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HelperHealth {
    pub state: HelperHealthState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<HelperHealthReason>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Capabilities {
    native: bool,
    wsl: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Fingerprint {
    path: PathBuf,
    digest: [u8; 32],
}

#[derive(Default)]
struct Cache {
    selected_path: Option<PathBuf>,
    identity: Option<Fingerprint>,
    revision: u64,
    in_flight: bool,
    checked_at: Option<Instant>,
    result: Option<Result<Capabilities, HelperHealthReason>>,
    native: HelperHealth,
    wsl: HelperHealth,
}

impl Cache {
    fn wsl_for_path(&self, expected: &Path) -> HelperHealth {
        if self.selected_path.as_deref() == Some(expected) {
            self.wsl
        } else {
            unavailable(HelperHealthReason::UninspectedPath)
        }
    }

    fn begin(&mut self, fingerprint: Fingerprint, now: Instant) -> Option<u64> {
        let retry = matches!(
            self.result,
            Some(Err(HelperHealthReason::ProbeFailed
                | HelperHealthReason::ProbeTimeout
                | HelperHealthReason::ChangedDuringProbe))
        ) && self
            .checked_at
            .is_some_and(|at| now.saturating_duration_since(at) >= Duration::from_secs(5));
        if self.identity.as_ref() == Some(&fingerprint)
            && (self.in_flight || self.result.is_some() && !retry)
        {
            return None;
        }
        self.revision = self.revision.wrapping_add(1);
        self.selected_path = Some(fingerprint.path.clone());
        self.identity = Some(fingerprint);
        self.in_flight = true;
        self.checked_at = Some(now);
        self.result = None;
        self.native = HelperHealth::default();
        self.wsl = HelperHealth::default();
        Some(self.revision)
    }

    fn finish(
        &mut self,
        ticket: u64,
        current: Option<&Fingerprint>,
        result: Result<Capabilities, HelperHealthReason>,
    ) {
        if ticket != self.revision || !self.in_flight {
            return;
        }
        self.in_flight = false;
        let result = if current == self.identity.as_ref() {
            result
        } else {
            Err(HelperHealthReason::ChangedDuringProbe)
        };
        self.result = Some(result);
        match result {
            Ok(capabilities) => {
                self.native = if capabilities.native {
                    compatible()
                } else {
                    unavailable(HelperHealthReason::MissingNativeCapability)
                };
                self.wsl = if capabilities.wsl {
                    compatible()
                } else {
                    unavailable(HelperHealthReason::MissingWslCapability)
                };
            },
            Err(reason) => {
                self.native = unavailable(reason);
                self.wsl = self.native;
            },
        }
    }

    fn fail(&mut self, path: Option<PathBuf>, reason: HelperHealthReason) {
        self.revision = self.revision.wrapping_add(1);
        self.selected_path = path;
        self.identity = None;
        self.in_flight = false;
        self.result = Some(Err(reason));
        self.native = unavailable(reason);
        self.wsl = self.native;
    }
}

fn compatible() -> HelperHealth {
    HelperHealth { state: HelperHealthState::Compatible, reason: None }
}

fn unavailable(reason: HelperHealthReason) -> HelperHealth {
    HelperHealth { state: HelperHealthState::Unavailable, reason: Some(reason) }
}

fn wsl_helper_path(current: Option<std::ffi::OsString>) -> Option<PathBuf> {
    current.filter(|value| !value.is_empty()).map(PathBuf::from)
}

fn parse_capabilities(bytes: &[u8]) -> Option<Capabilities> {
    if bytes.len() > 64 || !bytes.is_ascii() {
        return None;
    }
    let line = bytes.strip_suffix(b"\n")?;
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if line.contains(&b'\n') || line.contains(&b'\r') {
        return None;
    }
    let mut fields = std::str::from_utf8(line).ok()?.split_ascii_whitespace();
    if fields.next()? != "pebrel-hook-capabilities/1" {
        return None;
    }
    let mut result = Capabilities { native: false, wsl: false };
    for field in fields {
        match field {
            "native-hook/1" if !result.native => result.native = true,
            "wsl-hook/1" if !result.wsl => result.wsl = true,
            _ => return None,
        }
    }
    (result.native || result.wsl).then_some(result)
}

fn fingerprint(path: &Path) -> Result<Fingerprint, HelperHealthReason> {
    let digest = || -> Result<[u8; 32], HelperHealthReason> {
        let mut file = std::fs::File::open(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                HelperHealthReason::MissingFile
            } else {
                HelperHealthReason::ProbeFailed
            }
        })?;
        let metadata = file.metadata().map_err(|_| HelperHealthReason::ProbeFailed)?;
        if !metadata.is_file() || metadata.len() > 32 * 1024 * 1024 {
            return Err(HelperHealthReason::ProbeFailed);
        }
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 16 * 1024];
        let mut total = 0;
        loop {
            let count = file.read(&mut buffer).map_err(|_| HelperHealthReason::ProbeFailed)?;
            if count == 0 {
                break;
            }
            total += count;
            if total > 32 * 1024 * 1024 {
                return Err(HelperHealthReason::ChangedDuringProbe);
            }
            hash.update(&buffer[..count]);
        }
        if total as u64 != metadata.len() {
            return Err(HelperHealthReason::ChangedDuringProbe);
        }
        Ok(hash.finalize().into())
    };
    let before = digest()?;
    // Reopen by path: an atomic same-size replacement must not inherit the old receipt.
    if digest()? != before {
        return Err(HelperHealthReason::ChangedDuringProbe);
    }
    Ok(Fingerprint { path: path.to_owned(), digest: before })
}

fn probe_command(command: &mut Command) -> Result<Capabilities, HelperHealthReason> {
    let mut execute = || -> Result<Vec<u8>, HelperHealthReason> {
        let mut output = tempfile::tempfile().map_err(|_| HelperHealthReason::ProbeFailed)?;
        command
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(output.try_clone().map_err(|_| HelperHealthReason::ProbeFailed)?);
        crate::platform::process::hidden_command(command);
        crate::platform::process::configure_process_group(command);
        let mut child = command.spawn().map_err(|_| HelperHealthReason::ProbeFailed)?;
        let group = match crate::platform::process::ProcessGroup::attach(&child) {
            Ok(group) => group,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(HelperHealthReason::ProbeFailed);
            },
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        let status = loop {
            let poll = child.try_wait().map_err(|_| HelperHealthReason::ProbeFailed);
            let length = output
                .metadata()
                .map(|meta| meta.len())
                .map_err(|_| HelperHealthReason::ProbeFailed);
            let reason = match (&poll, length) {
                (_, Ok(size)) if size > 64 => Some(HelperHealthReason::OutputTooLarge),
                (Err(reason), _) => Some(*reason),
                (_, Err(reason)) => Some(reason),
                (Ok(None), _) if Instant::now() >= deadline => {
                    Some(HelperHealthReason::ProbeTimeout)
                },
                _ => None,
            };
            if let Some(reason) = reason {
                group.terminate(&mut child);
                let _ = child.wait();
                return Err(reason);
            }
            if let Ok(Some(status)) = poll {
                break status;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        group.finish();
        if !status.success() {
            return Err(HelperHealthReason::ProbeFailed);
        }
        output.rewind().map_err(|_| HelperHealthReason::ProbeFailed)?;
        let mut bytes = Vec::new();
        output.take(65).read_to_end(&mut bytes).map_err(|_| HelperHealthReason::ProbeFailed)?;
        if bytes.len() > 64 {
            return Err(HelperHealthReason::OutputTooLarge);
        }
        Ok(bytes)
    };
    parse_capabilities(&execute()?).ok_or(HelperHealthReason::UnsupportedProtocol)
}

static NATIVE: LazyLock<Mutex<Cache>> = LazyLock::new(Mutex::default);
static WSL: LazyLock<Mutex<Cache>> = LazyLock::new(Mutex::default);

pub(crate) fn cached_native_helper_health() -> HelperHealth {
    NATIVE
        .lock()
        .map(|cache| cache.native)
        .unwrap_or_else(|_| unavailable(HelperHealthReason::ProbeFailed))
}

pub(crate) fn cached_wsl_helper_health() -> HelperHealth {
    WSL.lock()
        .map(|cache| cache.wsl)
        .unwrap_or_else(|_| unavailable(HelperHealthReason::ProbeFailed))
}

pub(crate) fn cached_wsl_helper_health_for_path(expected: &Path) -> HelperHealth {
    WSL.lock()
        .map(|cache| cache.wsl_for_path(expected))
        .unwrap_or_else(|_| unavailable(HelperHealthReason::ProbeFailed))
}

fn refresh(cache: &Mutex<Cache>, path: Option<PathBuf>) {
    let checked = path.as_deref().ok_or(HelperHealthReason::MissingFile).and_then(fingerprint);
    let key = match checked {
        Ok(key) => key,
        Err(reason) => {
            if let Ok(mut cache) = cache.lock() {
                cache.fail(path, reason);
            }
            return;
        },
    };
    let Some(ticket) =
        cache.lock().ok().and_then(|mut cache| cache.begin(key.clone(), Instant::now()))
    else {
        return;
    };
    let mut command = Command::new(&key.path);
    command.arg("--capabilities");
    let result = probe_command(&mut command);
    let current = fingerprint(&key.path).ok();
    if let Ok(mut cache) = cache.lock() {
        cache.finish(ticket, current.as_ref(), result);
    }
}

/// Called only by existing background setup/inspection workers.
pub(crate) fn refresh_helper_health() {
    static REFRESH: Mutex<()> = Mutex::new(());
    let Ok(_refresh) = REFRESH.try_lock() else { return };
    refresh(&NATIVE, super::helper_path());
    refresh(&WSL, wsl_helper_path(std::env::var_os(crate::ai_hook::HOOK_EXE_ENV)));
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTH: &[u8] = b"pebrel-hook-capabilities/1 native-hook/1 wsl-hook/1\n";

    fn key(value: u8) -> Fingerprint {
        Fingerprint { path: PathBuf::from("fixture/helper"), digest: [value; 32] }
    }

    fn fixture_command(root: &Path, output: &str, exit: i32) -> Command {
        #[cfg(windows)]
        {
            let script = root.join("helper.ps1");
            std::fs::write(&script, format!("[Console]::Out.Write('{output}'); exit {exit}\n"))
                .unwrap();
            let mut command = Command::new("powershell.exe");
            command.args(["-NoProfile", "-NonInteractive", "-File"]).arg(script);
            command
        }
        #[cfg(not(windows))]
        {
            let script = root.join("helper.sh");
            std::fs::write(&script, format!("printf '%s' '{output}'\nexit {exit}\n")).unwrap();
            let mut command = Command::new("sh");
            command.arg(script);
            command
        }
    }

    #[test]
    fn wsl_checks_only_its_actual_exported_path() {
        let old = PathBuf::from("fixture/old-helper");
        let newly_discovered_native = PathBuf::from("fixture/runtime/new-helper");
        let selected = wsl_helper_path(Some(old.clone().into_os_string()));
        assert_eq!(selected, Some(old));
        assert_ne!(selected, Some(newly_discovered_native));
        assert_eq!(wsl_helper_path(None), None);
        assert_eq!(wsl_helper_path(Some("".into())), None);
    }

    #[test]
    fn a_different_pane_helper_path_cannot_reuse_compatibility() {
        let mut cache = Cache::default();
        let checked = key(1);
        let ticket = cache.begin(checked.clone(), Instant::now()).unwrap();
        cache.finish(ticket, Some(&checked), Ok(Capabilities { native: true, wsl: true }));
        assert_eq!(cache.wsl_for_path(&checked.path).state, HelperHealthState::Compatible);
        let other = cache.wsl_for_path(Path::new("fixture/uninspected-helper"));
        assert_eq!(other.state, HelperHealthState::Unavailable);
        assert_eq!(other.reason, Some(HelperHealthReason::UninspectedPath));
    }

    #[test]
    fn explicit_capabilities_keep_native_and_wsl_separate() {
        assert_eq!(parse_capabilities(BOTH), Some(Capabilities { native: true, wsl: true }));
        assert_eq!(
            parse_capabilities(b"pebrel-hook-capabilities/1 native-hook/1\r\n"),
            Some(Capabilities { native: true, wsl: false })
        );
    }

    #[test]
    fn missing_or_ambiguous_protocol_is_not_a_capability() {
        for bytes in [
            b"".as_slice(),
            b"native-hook/1 wsl-hook/1\n",
            b"banner\npebrel-hook-capabilities/1 native-hook/1 wsl-hook/1\n",
            b"pebrel-hook-capabilities/2 native-hook/1 wsl-hook/1\n",
            b"pebrel-hook-capabilities/1 native-hook/1 native-hook/1\n",
            b"pebrel-hook-capabilities/1 native-hook/1\nextra\n",
        ] {
            assert_eq!(parse_capabilities(bytes), None, "{bytes:?}");
        }
        assert_eq!(parse_capabilities(&[b'x'; 65]), None);
    }

    #[test]
    fn actual_helper_process_must_report_its_capability() {
        let root = tempfile::tempdir().unwrap();
        let mut command = fixture_command(root.path(), std::str::from_utf8(BOTH).unwrap(), 0);
        assert_eq!(probe_command(&mut command), Ok(Capabilities { native: true, wsl: true }));
        let mut command =
            fixture_command(root.path(), "pebrel-hook-capabilities/1 native-hook/1\n", 0);
        assert_eq!(probe_command(&mut command), Ok(Capabilities { native: true, wsl: false }));
    }

    #[test]
    fn actual_helper_process_rejects_empty_nonzero_and_oversized_output() {
        let root = tempfile::tempdir().unwrap();
        for (output, exit, expected) in [
            ("".to_owned(), 0, HelperHealthReason::UnsupportedProtocol),
            (String::from_utf8(BOTH.to_vec()).unwrap(), 7, HelperHealthReason::ProbeFailed),
            ("x".repeat(65), 0, HelperHealthReason::OutputTooLarge),
        ] {
            let mut command = fixture_command(root.path(), &output, exit);
            assert_eq!(probe_command(&mut command), Err(expected));
        }
    }

    #[test]
    fn actual_helper_process_times_out_instead_of_leaving_a_check_pending() {
        let root = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let mut command = {
            let mut command = Command::new("powershell.exe");
            command.args(["-NoProfile", "-NonInteractive", "-Command", "Start-Sleep -Seconds 30"]);
            command
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut command = Command::new("sh");
            command.args(["-c", "exec sleep 30"]);
            command
        };
        let _ = root;
        let started = Instant::now();
        assert_eq!(probe_command(&mut command), Err(HelperHealthReason::ProbeTimeout));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn fingerprint_changes_when_same_length_helper_bytes_change() {
        let root = tempfile::tempdir().unwrap();
        let helper = root.path().join("helper");
        std::fs::write(&helper, b"old").unwrap();
        let previous = fingerprint(&helper).unwrap();
        std::fs::write(&helper, b"new").unwrap();
        assert_ne!(fingerprint(&helper).unwrap(), previous);
        assert_eq!(fingerprint(&root.path().join("missing")), Err(HelperHealthReason::MissingFile));
    }

    #[test]
    fn independent_cache_retains_native_only_and_invalidates_changed_helper() {
        let now = Instant::now();
        let mut cache = Cache::default();
        let ticket = cache.begin(key(1), now).unwrap();
        cache.finish(ticket, Some(&key(1)), Ok(Capabilities { native: true, wsl: false }));
        assert_eq!(cache.native.state, HelperHealthState::Compatible);
        assert_eq!(cache.wsl.state, HelperHealthState::Unavailable);
        assert_eq!(cache.wsl.reason, Some(HelperHealthReason::MissingWslCapability));
        assert_eq!(cache.begin(key(1), now), None);
        assert!(cache.begin(key(2), now).is_some());
        assert_eq!(cache.wsl.state, HelperHealthState::Checking);
    }

    #[test]
    fn late_probe_cannot_overwrite_the_new_helper_check() {
        let mut cache = Cache::default();
        let old = cache.begin(key(1), Instant::now()).unwrap();
        let current = cache.begin(key(2), Instant::now()).unwrap();
        cache.finish(old, Some(&key(1)), Ok(Capabilities { native: true, wsl: true }));
        assert_eq!(cache.wsl.state, HelperHealthState::Checking);
        cache.finish(current, Some(&key(2)), Err(HelperHealthReason::UnsupportedProtocol));
        assert_eq!(cache.native.state, HelperHealthState::Unavailable);
        assert_eq!(cache.native.reason, Some(HelperHealthReason::UnsupportedProtocol));
        assert_eq!(cache.wsl.reason, Some(HelperHealthReason::UnsupportedProtocol));
    }

    #[test]
    fn replacement_during_probe_cannot_receive_old_compatibility() {
        let mut cache = Cache::default();
        let ticket = cache.begin(key(1), Instant::now()).unwrap();
        cache.finish(ticket, Some(&key(2)), Ok(Capabilities { native: true, wsl: true }));
        assert_eq!(cache.wsl.state, HelperHealthState::Unavailable);
        assert_eq!(cache.wsl.reason, Some(HelperHealthReason::ChangedDuringProbe));
        assert!(cache.begin(key(2), Instant::now()).is_some());
    }

    #[test]
    fn health_projection_contains_no_path_or_credentials() {
        let health = HelperHealth {
            state: HelperHealthState::Unavailable,
            reason: Some(HelperHealthReason::MissingWslCapability),
        };
        assert_eq!(
            serde_json::to_value(health).unwrap(),
            serde_json::json!({"state":"unavailable", "reason":"missing_wsl_capability"})
        );
    }
}
