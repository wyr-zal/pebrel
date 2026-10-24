use super::*;

#[test]
fn process_probe_command_passes_identity_arguments_and_removes_identity_environment() {
    let mut command = Command::new("wsl.exe");
    for name in [
        crate::agent_env::PANE_ENV,
        crate::ai_hook::LEGACY_PANE_ENV,
        crate::agent_env::PROCESS_ENV,
        crate::agent_env::COMMAND_ID_ENV,
    ] {
        command.env(name, "probe-self");
    }
    let operation_args = ["check", "70", "700", "80", "800", "34820", "70"]
        .map(str::to_owned);
    let mut expected_args = ["7", "process-instance", "abcdef0123456789abcdef0123456789"]
        .map(str::to_owned)
        .to_vec();
    configure_process_probe(
        &mut command,
        7,
        "process-instance",
        Some("abcdef0123456789abcdef0123456789"),
        &operation_args,
    );

    let actual_args: Vec<_> = command
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    assert_eq!(actual_args[0], "-c");
    assert_eq!(actual_args[1], PROCESS_PROBE_SCRIPT);
    assert_eq!(actual_args[2], "pebrel-agent-process-probe");
    expected_args.extend(operation_args);
    assert_eq!(&actual_args[3..], expected_args.as_slice());

    for name in [
        crate::agent_env::PANE_ENV,
        crate::ai_hook::LEGACY_PANE_ENV,
        crate::agent_env::PROCESS_ENV,
        crate::agent_env::COMMAND_ID_ENV,
    ] {
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == std::ffi::OsStr::new(name) && value.is_none()),
            "probe child must not inherit {name}"
        );
    }
}

#[cfg(windows)]
fn native_boundary(time: u64) -> NativeLaunchBoundary {
    NativeLaunchBoundary {
        time, shell: ProcessStamp { pid: 10, start_time: 100 },
        existing_branches: HashSet::new(),
    }
}

#[cfg(windows)]
#[test]
fn native_receipts_reject_ambiguous_or_unstamped_provider_trees() {
    use crate::platform::process_snapshot::ProcessRow;
    let row = |pid, parent, executable: &str, created| ProcessRow {
        pid, parent, executable: executable.into(), created,
    };
    let base = || vec![row(10, 1, "pwsh.exe", 100), row(20, 10, "codex.exe", 200)];
    let AgentProbe::Found(found) = native_receipt_from_rows(10, &native_boundary(150), base()) else {
        panic!("unique stamped provider must yield a receipt");
    };
    assert_eq!(found.process, ProcessStamp { pid: 20, start_time: 200 });
    assert_eq!(found.shell, ProcessStamp { pid: 10, start_time: 100 });
    let mut ambiguous = base();
    ambiguous.push(row(21, 10, "claude.exe", 210));
    assert_eq!(native_receipt_from_rows(10, &native_boundary(150), ambiguous), AgentProbe::Unavailable);
    let mut unknown_stamp = base();
    unknown_stamp[1].created = 0;
    assert_eq!(native_receipt_from_rows(10, &native_boundary(150), unknown_stamp), AgentProbe::Unavailable);
    let reused_parent = vec![row(10, 1, "pwsh.exe", 300), row(20, 10, "codex.exe", 200)];
    assert_eq!(native_receipt_from_rows(10, &native_boundary(150), reused_parent), AgentProbe::Unavailable);
    let npm_codex = vec![row(10, 1, "pwsh.exe", 100), row(20, 10, "node.exe", 200), row(30, 20, "codex.exe", 300)];
    let AgentProbe::Found(receipt) = native_receipt_from_rows(10, &native_boundary(150), npm_codex) else {
        panic!("the native Codex child must own the npm launcher's receipt");
    };
    assert_eq!(receipt.process.pid, 30);
}

#[cfg(windows)]
#[test]
fn native_takeover_excludes_old_branches_even_when_the_new_agent_is_deeper() {
    use crate::platform::process_snapshot::ProcessRow;
    let row = |pid, parent, executable: &str, created| ProcessRow {
        pid, parent, executable: executable.into(), created,
    };
    let tree = || vec![
        row(10, 1, "pwsh.exe", 100),
        row(20, 10, "codex.exe", 140), // old background Agent
        row(30, 10, "cmd.exe", 200),   // current command wrapper
        row(40, 30, "claude.exe", 300),
        row(50, 20, "codex.exe", 400), // old branch's newly spawned Agent
    ];
    let AgentProbe::Found(found) = native_receipt_from_rows(10, &native_boundary(150), tree()) else {
        panic!("the command's new branch must own the receipt");
    };
    assert_eq!(found.process.pid, 40);
    assert_eq!(found.kind, AgentKind::Claude);
    assert_eq!(probe_native_receipt(10, None), AgentProbe::Unavailable);
    assert_eq!(native_receipt_from_rows(10, &native_boundary(500), tree()), AgentProbe::NoAgent);
    let mut ambiguous = tree();
    ambiguous.push(row(60, 10, "codex.exe", 250));
    assert_eq!(native_receipt_from_rows(10, &native_boundary(150), ambiguous), AgentProbe::Unavailable,
        "depth cannot decide ownership between two current branches");
    let mut clock_rewound = native_boundary(110);
    clock_rewound.existing_branches.insert(ProcessStamp { pid: 20, start_time: 140 });
    let AgentProbe::Found(found) = native_receipt_from_rows(10, &clock_rewound, tree()) else {
        panic!("preexisting branches must stay excluded after clock rollback");
    };
    assert_eq!(found.process.pid, 40);
    let mut old_wrapper = tree();
    old_wrapper[2].created = 145;
    assert_eq!(native_receipt_from_rows(10, &native_boundary(150), old_wrapper), AgentProbe::NoAgent,
        "an old wrapper's new child cannot establish current command ownership");
}

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
        .args(["-c", &script, "fixture", "4", "10404", "", "discover"])
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

#[cfg(unix)]
#[test]
fn process_scan_parses_each_tty_candidate_environment_once() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let proc = root.path().join("proc");
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let stat = |pid: u32, parent: u32, group: u32, tty: u32, foreground: u32, start: u64| {
        format!("{pid} (fixture) S {parent} {group} 100 {tty} {foreground} 0 0 0 0 0 0 0 0 0 0 0 0 {start}\n")
    };
    let environment = b"PEBREL_PANE_ID=4\0PEBREL_PROCESS_ID=10404\0PEBREL_COMMAND_ID=0123456789abcdef0123456789abcdef\0";
    for (pid, parent, group, tty, foreground, start, name, owned) in [
        (100, 1, 100, 34823, 200, 1000, "bash", true),
        (200, 100, 200, 34823, 200, 2000, "claude", true),
        (300, 1, 300, 999, 300, 3000, "sleep", false),
        (301, 1, 301, 999, 301, 3001, "sleep", false),
        (302, 1, 302, 999, 302, 3002, "sleep", false),
    ] {
        let directory = proc.join(pid.to_string());
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("stat"),
            stat(pid, parent, group, tty, foreground, start),
        )
        .unwrap();
        std::fs::write(directory.join("comm"), format!("{name}\n")).unwrap();
        std::fs::write(directory.join("cmdline"), format!("{name}\0--resume\0")).unwrap();
        std::fs::write(
            directory.join("environ"),
            if owned { environment.as_slice() } else { b"OTHER=1\0" },
        )
        .unwrap();
    }
    for pid in 400..500 {
        let directory = proc.join(pid.to_string());
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("stat"), stat(pid, 1, pid, 0, 0, u64::from(pid))).unwrap();
        std::fs::write(directory.join("comm"), "sleep\n").unwrap();
        std::fs::write(directory.join("cmdline"), b"sleep\0").unwrap();
        std::fs::write(directory.join("environ"), b"OTHER=1\0").unwrap();
    }
    let cwd = root.path().join("working-directory");
    std::fs::create_dir_all(&cwd).unwrap();
    std::os::unix::fs::symlink(cwd, proc.join("200/cwd")).unwrap();
    let grep_count = root.path().join("grep-count");
    let grep_wrapper = bin.join("grep");
    std::fs::write(
        &grep_wrapper,
        "#!/bin/sh\nprintf x >> \"$GREP_COUNT\"\nexec /usr/bin/grep \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&grep_wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();

    let script = PROCESS_PROBE_SCRIPT.replace("/proc/", &format!("{}/", proc.display()));
    let output = Command::new("/bin/sh")
        .args([
            "-c",
            &script,
            "fixture",
            "4",
            "10404",
            "0123456789abcdef0123456789abcdef",
            "discover",
        ])
        .env(crate::agent_env::PANE_ENV, "999")
        .env(crate::agent_env::PROCESS_ENV, "99999")
        .env(crate::agent_env::COMMAND_ID_ENV, "wrong-command-id")
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("GREP_COUNT", &grep_count)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(std::fs::read(grep_count).unwrap().len(), 8);
}

#[cfg(unix)]
#[test]
fn process_scan_rejects_a_cwd_whose_final_character_is_a_newline() {
    let root = tempfile::tempdir().unwrap();
    let proc = root.path().join("proc");
    let stat = |pid: u32, parent: u32, group: u32, start: u64| {
        format!("{pid} (fixture) S {parent} {group} 100 34823 200 0 0 0 0 0 0 0 0 0 0 0 0 {start}\n")
    };
    for (pid, parent, group, start, name) in
        [(100, 1, 100, 1000, "bash"), (200, 100, 200, 2000, "claude")]
    {
        let directory = proc.join(pid.to_string());
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("stat"), stat(pid, parent, group, start)).unwrap();
        std::fs::write(directory.join("comm"), format!("{name}\n")).unwrap();
        std::fs::write(directory.join("cmdline"), format!("{name}\0--resume\0")).unwrap();
        std::fs::write(
            directory.join("environ"),
            b"PEBREL_PANE_ID=4\0PEBREL_PROCESS_ID=10404\0PEBREL_COMMAND_ID=0123456789abcdef0123456789abcdef\0",
        )
        .unwrap();
    }
    let cwd = root.path().join("directory\n");
    std::fs::create_dir_all(&cwd).unwrap();
    std::os::unix::fs::symlink(cwd, proc.join("200/cwd")).unwrap();
    let script = PROCESS_PROBE_SCRIPT.replace("/proc/", &format!("{}/", proc.display()));
    let output = Command::new("/bin/sh")
        .args([
            "-c",
            &script,
            "fixture",
            "4",
            "10404",
            "0123456789abcdef0123456789abcdef",
            "discover",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
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
fn location_probe_returns_only_the_selected_agent_cwd() {
    use base64::Engine as _;
    let cwd = base64::engine::general_purpose::STANDARD.encode("/mnt/f/Project Learn");
    let snapshot = format!(
        "S\t100\t1\nP\t200\t100\tclaude\tclaude\t--resume\nD\t200\t2000\t{cwd}\nM\t100\t1\t1000\t34823\t100\t200\nM\t200\t100\t2000\t34823\t200\t200\nE\n"
    );
    let parsed = parse_probe_snapshot(&snapshot).expect("valid location snapshot");
    assert_eq!(
        parsed,
        (
            AgentProbe::Found(ForegroundAgent {
                kind: AgentKind::Claude,
                process: ProcessStamp { pid: 200, start_time: 2000 },
                shell: ProcessStamp { pid: 100, start_time: 1000 },
                tty_nr: 34823,
                process_group: 200,
            }),
            Some("/mnt/f/Project Learn".into()),
        )
    );

    let wrong_owner = snapshot.replace("D\t200\t2000\t", "D\t201\t2000\t");
    assert_eq!(parse_probe_snapshot(&wrong_owner).unwrap().1, None);

    let reused_pid = snapshot.replace("M\t200\t100\t2000\t", "M\t200\t100\t2001\t");
    let (probe, cwd) = parse_probe_snapshot(&reused_pid).expect("reused process snapshot parses");
    let AgentProbe::Found(agent) = probe else { panic!("foreground Agent identity remains observable") };
    assert_eq!(agent.process, ProcessStamp { pid: 200, start_time: 2001 });
    assert!(cwd.is_none(), "cwd from the old PID instance must not be reused");
}

#[test]
fn expired_location_probe_deadline_returns_unavailable_without_starting_a_child() {
    let mut options = nebula_terminal::tty::Options::default();
    options.env.insert(crate::agent_env::PROCESS_ENV.into(), "10404".into());
    let context = PaneExecContext::from_pty_options(&options);
    let mut remaining = MAX_PROBE_OUTPUT;
    assert!(read_probe(
        &context,
        4,
        &["discover".into()],
        Instant::now() - Duration::from_millis(1),
        &mut remaining,
        &|| false,
    )
    .is_none());
}

#[test]
fn command_token_without_a_matching_guest_process_is_a_complete_no_agent_result() {
    assert_eq!(parse_probe_snapshot("N\nE\n"), Some((AgentProbe::NoAgent, None)));
    assert_eq!(parse_probe_snapshot("N\nS\t100\t1\nE\n"), None);
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
