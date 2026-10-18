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
        resolve_process_records("S\t100\t1\nP\t200\t100\tnode\tnode\t/home/claude/server.js\n"),
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
    assert_eq!(resolve_process_records("S\t100\t1\nP\t200\t900\tclaude\tclaude\t\n"), None);
    assert_eq!(resolve_process_records("P\t200\t100\tclaude\tclaude\t\n"), None);
}

#[test]
fn different_same_kind_owners_are_ambiguous() {
    assert_eq!(
        resolve_process_records(
            "S\t100\t1\nP\t200\t100\tcodex\tcodex\t\nP\t201\t100\tcodex\tcodex\t\n"
        ),
        None,
        "an AgentKind is not enough to choose between two process owners"
    );
}

#[cfg(unix)]
#[test]
fn probe_rechecks_identity_after_reading_argv() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let proc = root.path().join("proc");
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let stat = |pid: u32, parent: u32, group: u32, start: u64| {
        format!("{pid} (test) S {parent} {group} 100 34823 200 0 0 0 0 0 0 0 0 0 0 0 0 0 {start}\n")
    };
    for (pid, parent, group, start, name) in
        [(100, 1, 100, 1000, "bash"), (200, 100, 200, 2000, "codex")]
    {
        let dir = proc.join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stat"), stat(pid, parent, group, start)).unwrap();
        std::fs::write(dir.join("comm"), format!("{name}\n")).unwrap();
        std::fs::write(dir.join("cmdline"), format!("{name}\0")).unwrap();
        std::fs::write(dir.join("environ"), b"PEBREL_PANE_ID=4\0PEBREL_PROCESS_ID=10404\0")
            .unwrap();
    }
    let next_stat = root.path().join("next-stat");
    std::fs::write(&next_stat, stat(200, 100, 200, 2001)).unwrap();
    let wrapper = bin.join("tr");
    std::fs::write(&wrapper, "#!/bin/sh\ncat > \"$PROBE_INPUT\"\nif /usr/bin/grep -aq '^codex' \"$PROBE_INPUT\"; then cp \"$NEXT_STAT\" \"$CHANGE_STAT\"; fi\n/usr/bin/tr \"$@\" < \"$PROBE_INPUT\"\n").unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let script = PROCESS_PROBE_SCRIPT.replace("/proc/", &format!("{}/", proc.display()));
    let output = Command::new("/bin/sh")
        .args(["-c", &script, "fixture", "4", "10404"])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("PROBE_INPUT", root.path().join("input"))
        .env("NEXT_STAT", next_stat)
        .env("CHANGE_STAT", proc.join("200/stat"))
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        !output.status.success() || !text.lines().any(|line| line.starts_with("P\t200\t")),
        "a PID replaced during argv read was published as a current owner: {text}"
    );
}

#[test]
fn foreground_receipt_keeps_owner_and_anchor_stamps() {
    let snapshot = "S\t100\t1\nM\t100\t1\t1000\t34823\t100\t200\nP\t200\t100\tcodex\tcodex\t\nM\t200\t100\t2000\t34823\t200\t200\nE\n";
    assert_eq!(
        parse_probe(snapshot),
        AgentProbe::Found(ForegroundAgent {
            kind: AgentKind::Codex,
            process: ProcessStamp { pid: 200, start_time: 2000 },
            shell: ProcessStamp { pid: 100, start_time: 1000 },
            tty_nr: 34823,
            process_group: 200,
        })
    );
    for invalid in [
        snapshot.replace("M\t200\t100\t2000\t34823", "M\t200\t100\t2000\t999"),
        snapshot.replace("M\t200\t100\t2000", "M\t200\t999\t2000"),
        snapshot.replace("\t200\t200\nE", "\t200\t201\nE"),
        snapshot.replace("M\t200\t100\t2000\t34823\t200\t200\n", ""),
        snapshot.replace("E\n", ""),
        format!("{snapshot}E\n"),
    ] {
        assert_eq!(parse_probe(&invalid), AgentProbe::Unavailable, "{invalid}");
    }
}

#[test]
fn incomplete_scan_does_not_disprove_an_agent() {
    assert_eq!(
        parse_probe("S\t100\t1\nM\t100\t1\t1000\t34823\t100\t100\nE\n"),
        AgentProbe::NoAgent
    );
    assert_eq!(
        parse_probe("S\t100\t1\nM\t100\t1\t1000\t34823\t100\t200\nE\n"),
        AgentProbe::Unavailable
    );
    assert_eq!(parse_probe("E\n"), AgentProbe::Unavailable);
    assert_eq!(parse_probe(""), AgentProbe::Unavailable);
    let ordinary = "S\t100\t1\nM\t100\t1\t1000\t34823\t100\t200\nP\t200\t100\tpython\tpython\tserver.py\nM\t200\t100\t2000\t34823\t200\t200\nE\n";
    assert_eq!(parse_probe(ordinary), AgentProbe::NoAgent);
    assert_eq!(
        parse_probe(&ordinary.replace("E\n", "U\t34823\t200\nE\n")),
        AgentProbe::Unavailable
    );
    assert_eq!(parse_probe(&ordinary.replace("E\n", "U\t999\t200\nE\n")), AgentProbe::NoAgent);
    let oversized = format!("{}E\n", "S\t100\t1\n".repeat(MAX_PROBE_RECORDS));
    assert_eq!(parse_probe(&oversized), AgentProbe::Unavailable);
}

#[test]
fn recheck_distinguishes_positive_change_from_unavailable() {
    assert_eq!(parse_recheck("C\tcodex\tcodex\t--yolo\n", AgentKind::Codex), AgentRecheck::Current);
    assert_eq!(
        parse_recheck("C\tnode\tnode\t/opt/bin/codex\n", AgentKind::Codex),
        AgentRecheck::Current
    );
    assert_eq!(
        parse_recheck("C\tpython\tpython\tserver.py\n", AgentKind::Codex),
        AgentRecheck::Changed
    );
    assert_eq!(parse_recheck("changed\n", AgentKind::Codex), AgentRecheck::Changed);
    for unavailable in
        ["", "unavailable\n", "changed", "changed\nC\tcodex\tcodex\t\n", "C\tcodex\n"]
    {
        assert_eq!(parse_recheck(unavailable, AgentKind::Codex), AgentRecheck::Unavailable);
    }
}

#[test]
fn execed_provider_can_be_its_own_pty_anchor() {
    let snapshot =
        "R\t200\t1\nP\t200\t1\tcodex\tcodex\t--yolo\nM\t200\t1\t2000\t34823\t200\t200\nE\n";
    assert_eq!(
        parse_probe(snapshot),
        AgentProbe::Found(ForegroundAgent {
            kind: AgentKind::Codex,
            process: ProcessStamp { pid: 200, start_time: 2000 },
            shell: ProcessStamp { pid: 200, start_time: 2000 },
            tty_nr: 34823,
            process_group: 200,
        })
    );
}

#[test]
fn execed_root_does_not_relax_owner_uniqueness() {
    let two_roots = "R\t200\t1\nP\t200\t1\tcodex\tcodex\t\nM\t200\t1\t2000\t34823\t200\t200\nR\t300\t1\nP\t300\t1\tcodex\tcodex\t\nM\t300\t1\t3000\t34824\t300\t300\nE\n";
    assert_eq!(parse_probe(two_roots), AgentProbe::Unavailable);
    let fake_root = "S\t100\t1\nM\t100\t1\t1000\t34823\t100\t200\nR\t200\t100\nP\t200\t100\tcodex\tcodex\t\nM\t200\t100\t2000\t34823\t200\t200\nE\n";
    assert_eq!(parse_probe(fake_root), AgentProbe::Unavailable);
}
