//! Logging for Pebrel.
//!
//! The main executable is supposed to call `initialize()` exactly once during
//! startup. All logging messages are written to stdout, given that their
//! log-level is sufficient for the level configured in `cli::Options`.

use std::fs::{File, OpenOptions};
use std::io::{self, LineWriter, Read, Seek, SeekFrom, Stdout, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use std::{env, process};

use log::{Level, LevelFilter};
#[cfg(feature = "legacy-shell")]
use winit::event_loop::EventLoopProxy;

use crate::cli::Options;
#[cfg(feature = "legacy-shell")]
use crate::event::{Event, EventType};
#[cfg(feature = "legacy-shell")]
use crate::message_bar::{Message, MessageType};

/// Logging target for IPC config error messages.
pub const LOG_TARGET_IPC_CONFIG: &str = "nebula_log_window_config";

/// Name for the environment variable containing the log file's path.
const LOG_PATH_ENV: &str = "PEBREL_LOG";

/// Logging target for config error messages.
pub const LOG_TARGET_CONFIG: &str = "nebula_config_derive";

/// Logging target for winit events.
pub const LOG_TARGET_WINIT: &str = "nebula_winit_event";

/// Name for the environment variable containing extra logging targets.
///
/// The targets are semicolon separated.
const EXTRA_LOG_TARGETS_ENV: &str = "PEBREL_EXTRA_LOG_TARGETS";

const MAX_LOG_FILE_BYTES: u64 = 8 * 1024 * 1024;
const RETAIN_LOG_FILE_BYTES: u64 = 6 * 1024 * 1024;

static APPEND_LOG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub(crate) fn append_bounded_log(path: &Path, message: &[u8]) {
    let Ok(_lock) = APPEND_LOG_LOCK.get_or_init(|| Mutex::new(())).lock() else { return };
    let Ok(mut file) = OpenOptions::new().create(true).read(true).write(true).open(path) else {
        return;
    };
    if file.seek(SeekFrom::End(0)).is_err() || file.write_all(message).is_err() {
        return;
    }
    let _ = trim_log_file_tail(&mut file, MAX_LOG_FILE_BYTES, RETAIN_LOG_FILE_BYTES);
}

#[cfg(windows)]
fn cleanup_inactive_temp_logs(temp_dir: &Path, current_log: &Path) {
    use std::os::windows::fs::OpenOptionsExt;

    let Ok(entries) = std::fs::read_dir(temp_dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else { continue };
        if &path == current_log || !name.starts_with("Pebrel-") || !name.ends_with(".log") {
            continue;
        }

        // Skip files that are currently open by a Pebrel process.
        let Ok(file) = OpenOptions::new().read(true).write(true).share_mode(0).open(&path) else {
            continue;
        };
        drop(file);
        let _ = std::fs::remove_file(path);
    }
}

fn trim_log_file_tail(file: &mut File, max_bytes: u64, retain_bytes: u64) -> io::Result<()> {
    if retain_bytes == 0 || retain_bytes >= max_bytes {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid log retention limits"));
    }

    let length = file.metadata()?.len();
    if length <= max_bytes {
        file.seek(SeekFrom::End(0))?;
        return Ok(());
    }

    file.seek(SeekFrom::Start(length - retain_bytes))?;
    let mut tail = Vec::with_capacity(retain_bytes as usize);
    file.read_to_end(&mut tail)?;
    if let Some(line_end) = tail.iter().position(|byte| *byte == b'\n') {
        tail.drain(..=line_end);
    }

    file.seek(SeekFrom::Start(0))?;
    file.write_all(&tail)?;
    file.set_len(tail.len() as u64)?;
    file.seek(SeekFrom::End(0))?;
    Ok(())
}

pub(crate) fn debug_log(message: impl AsRef<str>) {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    if !*ENABLED.get_or_init(|| {
        env::var("PEBREL_DEBUG_LOG").or_else(|_| env::var("NEBULA_DEBUG_LOG")).is_ok_and(|value| {
            let value = value.trim();
            !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
        })
    }) {
        return;
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| format!("{}.{:03}", duration.as_secs(), duration.subsec_millis()))
        .unwrap_or_else(|_| "0.000".to_owned());
    let path = crate::platform::dirs::data_dir().join("pebrel_debug.log");
    let record = format!("[{timestamp} pid={}] {}\n", process::id(), message.as_ref());
    append_bounded_log(&path, record.as_bytes());
}

/// Write sanitized startup events even when the ordinary log level is Warn.
pub(crate) fn startup_diagnostic(message: impl AsRef<str>) {
    let path = env::temp_dir().join(format!("Pebrel-startup-{}.log", process::id()));
    let record = startup_diagnostic_record(process::id(), message.as_ref());
    append_bounded_log(&path, record.as_bytes());
}

/// User configurable extra log targets to include.
fn extra_log_targets() -> &'static [String] {
    static EXTRA_LOG_TARGETS: OnceLock<Vec<String>> = OnceLock::new();

    EXTRA_LOG_TARGETS.get_or_init(|| {
        env::var(EXTRA_LOG_TARGETS_ENV)
            .or_else(|_| env::var("NEBULA_EXTRA_LOG_TARGETS"))
            .map_or(Vec::new(), |targets| targets.split(';').map(ToString::to_string).collect())
    })
}

/// List of targets which will be logged by Pebrel.
const ALLOWED_TARGETS: &[&str] = &[
    "pebrel",
    LOG_TARGET_IPC_CONFIG,
    LOG_TARGET_CONFIG,
    LOG_TARGET_WINIT,
    "nebula_config_derive",
    "nebula_terminal",
    "nebula",
    "crossfont",
];

/// Initialize the logger to its defaults.
pub fn initialize(options: &Options) -> Result<Option<PathBuf>, log::SetLoggerError> {
    initialize_logger(options, Logger::new())
}

/// Initialize the logger with the legacy shell's message-bar event sink.
#[cfg(feature = "legacy-shell")]
pub fn initialize_legacy(
    options: &Options,
    event_proxy: EventLoopProxy<Event>,
) -> Result<Option<PathBuf>, log::SetLoggerError> {
    initialize_logger(options, Logger::new_legacy(event_proxy))
}

fn initialize_logger(
    options: &Options,
    logger: Logger,
) -> Result<Option<PathBuf>, log::SetLoggerError> {
    log::set_max_level(options.log_level());

    let path = logger.file_path();
    log::set_boxed_logger(Box::new(logger))?;

    Ok(path)
}

pub struct Logger {
    logfile: Mutex<OnDemandLogFile>,
    stdout: Mutex<LineWriter<Stdout>>,
    #[cfg(feature = "legacy-shell")]
    event_proxy: Option<Mutex<EventLoopProxy<Event>>>,
    start: Instant,
}

impl Logger {
    fn new() -> Self {
        let logfile = Mutex::new(OnDemandLogFile::new());
        let stdout = Mutex::new(LineWriter::new(io::stdout()));

        Logger {
            logfile,
            stdout,
            #[cfg(feature = "legacy-shell")]
            event_proxy: None,
            start: Instant::now(),
        }
    }

    #[cfg(feature = "legacy-shell")]
    fn new_legacy(event_proxy: EventLoopProxy<Event>) -> Self {
        let mut logger = Self::new();
        logger.event_proxy = Some(Mutex::new(event_proxy));
        logger
    }

    fn file_path(&self) -> Option<PathBuf> {
        let logfile_lock = self.logfile.lock().ok()?;
        Some(logfile_lock.path().clone())
    }

    /// Log a record to the message bar.
    #[cfg(feature = "legacy-shell")]
    fn message_bar_log(&self, record: &log::Record<'_>, logfile_path: &str) {
        let message_type = match record.level() {
            Level::Error => MessageType::Error,
            Level::Warn => MessageType::Warning,
            _ => return,
        };

        let Some(event_proxy) = self.event_proxy.as_ref() else { return };
        let event_proxy = match event_proxy.lock() {
            Ok(event_proxy) => event_proxy,
            Err(_) => return,
        };

        // Show the variable in the reader's own shell syntax. On Windows the
        // default shell is PowerShell, where cmd-style `%PEBREL_LOG%` does not
        // expand (issue #36); use `$env:PEBREL_LOG` so a copy-paste resolves.
        #[cfg(not(windows))]
        let env_var = format!("${LOG_PATH_ENV}");
        #[cfg(windows)]
        let env_var = format!("$env:{LOG_PATH_ENV}");

        let message = format!(
            "[{}] {}\nSee log at {} ({})",
            record.level(),
            record.args(),
            logfile_path,
            env_var,
        );

        let mut message = Message::new(message, message_type);
        message.set_target(record.target().to_owned());

        let _ = event_proxy.send_event(Event::new(EventType::Message(message), None));
    }
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record<'_>) {
        // Get target crate.
        let index = record.target().find(':').unwrap_or_else(|| record.target().len());
        let target = &record.target()[..index];

        // Only log our own crates, except when logging at Level::Trace.
        if !self.enabled(record.metadata()) || !is_allowed_target(record.level(), target) {
            return;
        }

        // Create log message for the given `record` and `target`.
        let message = create_log_message(record, target, self.start);

        if let Ok(mut logfile) = self.logfile.lock() {
            // Write to logfile.
            let _ = logfile.write_all(message.as_ref());

            // Log relevant entries to message bar.
            #[cfg(feature = "legacy-shell")]
            self.message_bar_log(record, &logfile.path.to_string_lossy());
        }

        // Write to stdout.
        if let Ok(mut stdout) = self.stdout.lock() {
            let _ = stdout.write_all(message.as_ref());
        }
    }

    fn flush(&self) {}
}

fn create_log_message(record: &log::Record<'_>, target: &str, start: Instant) -> String {
    let runtime = start.elapsed();
    let secs = runtime.as_secs();
    let nanos = runtime.subsec_nanos();
    let mut message = format!("[{}.{:0>9}s] [{:<5}] [{}] ", secs, nanos, record.level(), target);

    // Alignment for the lines after the first new line character in the payload. We don't deal
    // with fullwidth/unicode chars here, so just `message.len()` is sufficient.
    let alignment = message.len();

    // Push lines with added extra padding on the next line, which is trimmed later.
    let lines = record.args().to_string();
    for line in lines.split('\n') {
        let line = format!("{}\n{:width$}", line, "", width = alignment);
        message.push_str(&line);
    }

    // Drop extra trailing alignment.
    message.truncate(message.len() - alignment);
    message
}

/// Check if log messages from a crate should be logged.
fn is_allowed_target(level: Level, target: &str) -> bool {
    match (level, log::max_level()) {
        (Level::Error, LevelFilter::Trace) | (Level::Warn, LevelFilter::Trace) => true,
        _ => ALLOWED_TARGETS.contains(&target) || extra_log_targets().iter().any(|t| t == target),
    }
}

struct OnDemandLogFile {
    file: Option<LineWriter<File>>,
    created: Arc<AtomicBool>,
    path: PathBuf,
}

impl OnDemandLogFile {
    fn new() -> Self {
        let mut path = env::temp_dir();
        path.push(format!("Pebrel-{}.log", process::id()));

        #[cfg(windows)]
        cleanup_inactive_temp_logs(&env::temp_dir(), &path);

        // Set log path as an environment variable.
        unsafe {
            env::set_var(LOG_PATH_ENV, path.as_os_str());
            env::set_var("NEBULA_LOG", path.as_os_str());
        }

        OnDemandLogFile { path, file: None, created: Arc::new(AtomicBool::new(false)) }
    }

    fn file(&mut self) -> Result<&mut LineWriter<File>, io::Error> {
        // Allow to recreate the file if it has been deleted at runtime.
        if self.file.is_some() && !self.path.as_path().exists() {
            self.file = None;
        }

        // Create the file if it doesn't exist yet.
        if self.file.is_none() {
            let file = OpenOptions::new().read(true).write(true).create_new(true).open(&self.path);

            match file {
                Ok(file) => {
                    self.file = Some(io::LineWriter::new(file));
                    self.created.store(true, Ordering::Relaxed);
                    let _ =
                        writeln!(io::stdout(), "Created log file at \"{}\"", self.path.display());
                },
                Err(e) => {
                    let _ = writeln!(io::stdout(), "Unable to create log file: {e}");
                    return Err(e);
                },
            }
        }

        Ok(self.file.as_mut().unwrap())
    }

    fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Write for OnDemandLogFile {
    fn write(&mut self, buf: &[u8]) -> Result<usize, io::Error> {
        let file = self.file()?;
        let written = file.write(buf)?;
        file.flush()?;
        trim_log_file_tail(file.get_mut(), MAX_LOG_FILE_BYTES, RETAIN_LOG_FILE_BYTES)?;
        Ok(written)
    }

    fn flush(&mut self) -> Result<(), io::Error> {
        self.file()?.flush()
    }
}

fn startup_diagnostic_record(pid: u32, message: &str) -> String {
    format!("[STARTUP pid={pid}] {message}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, OpenOptions};

    #[test]
    fn trim_log_file_tail_keeps_recent_complete_lines() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pebrel.log");
        fs::write(&path, b"one\ntwo\nthree\nfour\n").unwrap();
        let mut file = OpenOptions::new().read(true).write(true).open(&path).unwrap();

        trim_log_file_tail(&mut file, 12, 8).unwrap();

        assert_eq!(fs::read(path).unwrap(), b"four\n");
    }

    #[cfg(windows)]
    #[test]
    fn cleanup_removes_inactive_pebrel_temp_logs_but_preserves_active_and_current() {
        let directory = tempfile::tempdir().unwrap();
        let old_log = directory.path().join("Pebrel-101.log");
        let active_log = directory.path().join("Pebrel-202.log");
        let current_log = directory.path().join("Pebrel-303.log");
        let unrelated = directory.path().join("other.log");
        fs::write(&old_log, b"old\n").unwrap();
        fs::write(&active_log, b"active\n").unwrap();
        fs::write(&current_log, b"current\n").unwrap();
        fs::write(&unrelated, b"other\n").unwrap();
        let active_file = OpenOptions::new().write(true).open(&active_log).unwrap();

        cleanup_inactive_temp_logs(directory.path(), &current_log);

        assert!(!old_log.exists());
        assert!(active_log.exists());
        assert!(current_log.exists());
        assert!(unrelated.exists());

        drop(active_file);
        cleanup_inactive_temp_logs(directory.path(), &current_log);
        assert!(!active_log.exists());
    }

    #[test]
    fn product_binary_log_target_is_allowed_by_default() {
        let target = "pebrel";
        assert!(ALLOWED_TARGETS.contains(&target), "missing product log target: {target}");
        assert!(is_allowed_target(Level::Info, target));
    }

    #[test]
    fn startup_diagnostic_record_is_marked_separately_from_filtered_logs() {
        assert_eq!(
            startup_diagnostic_record(42, "event=launch_begin"),
            "[STARTUP pid=42] event=launch_begin\n",
        );
    }

    #[test]
    fn log_allowlist_preserves_legacy_targets_without_matching_prefixes() {
        for target in ["nebula", "nebula_terminal", "nebula_config_derive"] {
            assert!(ALLOWED_TARGETS.contains(&target));
        }
        assert!(!ALLOWED_TARGETS.contains(&"pebrel_unrelated"));
        assert!(!ALLOWED_TARGETS.contains(&"nebula_unrelated"));
    }
}
