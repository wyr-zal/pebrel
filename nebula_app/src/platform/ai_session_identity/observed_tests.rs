use super::*;
use std::os::unix::fs::{PermissionsExt as _, symlink};

const CURRENT: &str = "0199a213-c2a4-7cf5-8f6b-d746fbb6e86c";
const OTHER: &str = "01a1aedc-d6f0-7db0-8875-882c249ef4a2";
const TOKEN: &str = "0123456789abcdef0123456789abcdef";

fn stat(pid: u32, start: u64) -> String {
    let mut fields = vec!["0".to_owned(); 20];
    fields[0] = "S".into();
    fields[19] = start.to_string();
    format!("{pid} (codex) {}\n", fields.join(" "))
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let sessions = root.path().join("data O'Brien/sessions");
    let dated_sessions = sessions.join("2026/11/06");
    std::fs::create_dir_all(&dated_sessions).unwrap();
    for (pid, id) in [(200, CURRENT), (201, OTHER)] {
        let proc = root.path().join(format!("proc/{pid}"));
        std::fs::create_dir_all(proc.join("fd")).unwrap();
        std::fs::write(proc.join("stat"), stat(pid, 2000)).unwrap();
        std::fs::write(proc.join("comm"), "codex\n").unwrap();
        std::fs::write(
            proc.join("environ"),
            format!(
                "PEBREL_PANE_ID=42\0PEBREL_PROCESS_ID=fixture\0PEBREL_COMMAND_ID={TOKEN}\0\
             CODEX_HOME={}\0",
                sessions.parent().unwrap().display(),
            ),
        )
        .unwrap();
        let rollout = dated_sessions.join(format!("rollout-date-{id}.jsonl"));
        std::fs::write(&rollout, format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"source\":\"cli\",\"thread_source\":\"user\"}}}}\n"
        )).unwrap();
        symlink(&rollout, proc.join("fd/7")).unwrap();
    }
    root
}

fn query(root: &Path, pid: &str, start: &str, token: &str) -> Command {
    let script = PROBE_SCRIPT.replace("/proc/", &format!("{}/proc/", root.display()));
    let mut command = Command::new("sh");
    command.args(["-c", &script, "probe", "42", "fixture", pid, start, token]);
    command
}

#[test]
fn observed_lookup_selects_the_exact_receipt_instead_of_another_same_pane_process() {
    let root = fixture();
    let session = run_probe_command(query(root.path(), "200", "2000", TOKEN)).unwrap();
    assert_eq!(session.session_id, CURRENT);
    assert!(
        run_probe_command(query(root.path(), "", "", "")).is_none(),
        "the unscoped legacy lookup sees two distinct user threads and is ambiguous"
    );
    for (pid, start, token) in
        [("200", "2001", TOKEN), ("200", "2000", "wrong-command"), ("999", "2000", TOKEN)]
    {
        assert!(run_probe_command(query(root.path(), pid, start, token)).is_none());
    }
}

#[test]
fn a_reused_pid_during_metadata_read_discards_even_an_already_printed_record() {
    let root = fixture();
    let tools = root.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let next_stat = root.path().join("next-stat");
    std::fs::write(&next_stat, stat(200, 2001)).unwrap();
    let head = tools.join("head");
    std::fs::write(
        &head,
        "#!/bin/sh\n\
        if [ \"$1\" = '-c' ]; then cp \"$NEXT_STAT\" \"$OWNER_STAT\"; fi\n\
        exec /usr/bin/head \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&head, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut command = query(root.path(), "200", "2000", TOKEN);
    command
        .env("PATH", format!("{}:{}", tools.display(), std::env::var("PATH").unwrap_or_default()))
        .env("NEXT_STAT", next_stat)
        .env("OWNER_STAT", root.path().join("proc/200/stat"));
    assert!(run_probe_command(command).is_none());
}
