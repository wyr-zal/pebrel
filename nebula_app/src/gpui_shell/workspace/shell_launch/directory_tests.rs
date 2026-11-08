use super::*;
use crate::gpui_shell::terminal::view::TerminalLaunch;
use crate::gpui_shell::workspace::NebulaWorkspace;
use crate::session::LaunchSession;

#[test]
fn explicit_directory_reaches_the_terminal_without_changing_the_saved_profile() {
    let saved = tempfile::tempdir().unwrap();
    let requested = tempfile::tempdir().unwrap();
    let profile = LaunchSession::Profile {
        name: "Project shell".into(),
        command: "custom-shell".into(),
        args: vec!["--login".into()],
        cwd: Some(saved.path().to_string_lossy().into_owned()),
        shell_id: Some("custom".into()),
    };
    let snapshot = at_explicit_directory(profile.clone(), Some(requested.path()));
    let TerminalLaunch::Local { cwd, shell, .. } =
        NebulaWorkspace::terminal_launch_from_session(&snapshot, Some(requested.path().into()))
    else {
        panic!("expected a local terminal");
    };
    assert_eq!(cwd.as_deref(), Some(requested.path()));
    let shell = shell.unwrap();
    assert_eq!(shell.program(), "custom-shell");
    assert_eq!(shell.args(), &["--login"]);

    // Starting the same saved profile again without an override keeps its default.
    let unchanged = at_explicit_directory(profile.clone(), None);
    assert_eq!(unchanged, profile);
    let TerminalLaunch::Local { cwd, .. } =
        NebulaWorkspace::terminal_launch_from_session(&unchanged, None)
    else {
        panic!("expected a local terminal");
    };
    assert_eq!(cwd.as_deref(), Some(saved.path()));
}

#[test]
fn explicit_host_directory_replaces_wsl_profile_directory_and_keeps_guest_arguments() {
    let requested = tempfile::tempdir().unwrap();
    let args = [
        "-d",
        "Team Linux",
        "--cd",
        "/old",
        "--user",
        "guest",
        "--cd=/older",
        "--exec",
        "zsh",
        "--cd",
        "/guest-option",
    ]
    .map(String::from)
    .to_vec();
    for launch in [
        LaunchSession::Profile {
            name: "WSL project".into(),
            command: "wsl.exe".into(),
            args: args.clone(),
            cwd: None,
            shell_id: Some("wsl:Team Linux".into()),
        },
        LaunchSession::Shell {
            name: "WSL project".into(),
            program: "wsl.exe".into(),
            args: args.clone(),
        },
    ] {
        assert_eq!(at_explicit_directory(launch.clone(), None), launch);
        let snapshot = at_explicit_directory(launch, Some(requested.path()));
        let TerminalLaunch::Local { cwd, shell, .. } =
            NebulaWorkspace::terminal_launch_from_session(&snapshot, Some(requested.path().into()))
        else {
            panic!("expected WSL to launch through the local PTY");
        };
        assert_eq!(cwd.as_deref(), Some(requested.path()));
        assert_eq!(
            shell.unwrap().args(),
            &[
                "--cd",
                requested.path().to_str().unwrap(),
                "-d",
                "Team Linux",
                "--user",
                "guest",
                "--exec",
                "zsh",
                "--cd",
                "/guest-option",
            ]
        );
    }
}

#[test]
fn directory_override_cannot_turn_an_invalid_shell_request_into_a_default_shell() {
    let requested = tempfile::tempdir().unwrap();
    assert!(resolve_shell_at("pebrel-missing-shell-id", Some(requested.path())).is_err());
}

#[test]
fn explicit_relative_host_directory_is_absolute_before_wsl_receives_it() {
    let launch =
        LaunchSession::Shell { name: "WSL".into(), program: "wsl.exe".into(), args: Vec::new() };
    let LaunchSession::Shell { args, .. } = at_explicit_directory(launch, Some(Path::new(".")))
    else {
        panic!("expected shell")
    };
    let directory = std::path::absolute(".").unwrap();
    let value = directory.to_str().unwrap();
    let encoded =
        if value.contains([' ', '\t']) { format!("\"{value}\"") } else { value.to_owned() };
    assert_eq!(args, vec!["--cd".to_owned(), encoded]);
}

#[test]
fn explicit_guest_directory_survives_to_the_wsl_launch() {
    let requested = Path::new("/home/pebrel-fixture/project space");
    let launch = LaunchSession::Shell {
        name: "WSL".into(),
        program: "wsl.exe".into(),
        args: vec!["-d".into(), "Ubuntu".into(), "--user".into(), "guest".into()],
    };
    let snapshot = at_explicit_directory(launch, Some(requested));
    let TerminalLaunch::Local { cwd, shell, .. } =
        NebulaWorkspace::terminal_launch_from_session(&snapshot, Some(requested.into()))
    else {
        panic!("expected local WSL launch")
    };
    let mut options =
        nebula_terminal::tty::Options { shell, working_directory: cwd, ..Default::default() };
    crate::platform::launch_directory::prepare_wsl(&mut options, Some("/different"));
    assert_eq!(
        options.shell.unwrap().args(),
        &["--cd", "\"/home/pebrel-fixture/project space\"", "-d", "Ubuntu", "--user", "guest"]
    );
    assert!(options.working_directory.is_none());
}

#[test]
fn profile_directory_precedes_guest_home_but_explicit_wsl_cd_keeps_precedence() {
    let directory = tempfile::tempdir().unwrap();
    for own_directory in [false, true] {
        let profile = LaunchSession::Profile {
            name: "Guest profile".into(),
            command: "wsl.exe".into(),
            args: if own_directory { vec!["--cd".into(), "/chosen".into()] } else { Vec::new() },
            cwd: Some(directory.path().to_string_lossy().into_owned()),
            shell_id: None,
        };
        let TerminalLaunch::Local { shell, cwd, .. } =
            NebulaWorkspace::terminal_launch_from_session(&profile, None)
        else {
            panic!("expected local WSL launch")
        };
        let mut options =
            nebula_terminal::tty::Options { shell, working_directory: cwd, ..Default::default() };
        crate::platform::launch_directory::prepare_wsl(&mut options, None);
        assert_eq!(
            options.shell.unwrap().args(),
            &["--cd", if own_directory { "/chosen" } else { directory.path().to_str().unwrap() }]
        );
    }
}

#[cfg(windows)]
#[test]
#[ignore = "requires installed PowerShell 7 and Ubuntu-22.04; uses isolated folders without a GUI"]
fn explorer_directories_reach_real_default_shells() {
    use clap::Parser as _;
    use nebula_settings::{RawSettings, RuntimeSettings};
    use std::io::Write as _;
    use std::os::windows::process::CommandExt as _;
    use std::process::Command;

    const MARKER: &str = "PEBREL_CWD_FIXTURE=";

    assert_eq!(
        crate::platform::startup::inherited_launch_directory(),
        std::env::current_dir().ok(),
        "a direct launch from an external directory must capture the Windows process cwd",
    );
    let root = tempfile::Builder::new().prefix("pebrel 目录验证 ").tempdir().unwrap();
    let invocation = crate::cli::Options::try_parse_from(["pebrel"]).unwrap();
    for name in ["A folder", "B 目录"] {
        let directory = root.path().join(name);
        std::fs::create_dir(&directory).unwrap();
        let expected = directory.to_str().unwrap();
        std::fs::write(directory.join(".pebrel-cwd-fixture"), format!("{MARKER}{expected}"))
            .unwrap();
        for shell_id in ["pwsh", "wsl:Ubuntu-22.04"] {
            let settings = RuntimeSettings::from_raw(&RawSettings::from_text(&format!(
                "shell={shell_id}\nstartup_directory={}",
                root.path().display(),
            )));
            let cwd = crate::launch_directory(&invocation, &settings, Some(directory.clone()));
            let launch =
                resolve_shell_at(settings.shell.as_deref().unwrap(), cwd.as_deref()).unwrap();
            let TerminalLaunch::Local { shell, cwd, .. } =
                NebulaWorkspace::terminal_launch_from_session(&launch, cwd)
            else {
                panic!("default shell must create a local terminal")
            };
            let mut options = nebula_terminal::tty::Options {
                shell,
                working_directory: cwd,
                ..Default::default()
            };
            crate::platform::launch_directory::prepare_wsl(&mut options, None);
            let shell = options.shell.unwrap();
            let mut command = Command::new(shell.program());
            if crate::platform::launch_directory::is_wsl_program(shell.program()) {
                // Shell snapshots already quote Windows arguments for the PTY command line.
                command.raw_arg(shell.args().join(" ")).args([
                    "--exec",
                    "/bin/cat",
                    ".pebrel-cwd-fixture",
                ]);
            } else {
                assert!(shell.program().to_ascii_lowercase().ends_with("pwsh.exe"));
                let mut args = shell.args().to_vec();
                assert_eq!(args[args.len() - 2], "-Command");
                args.retain(|arg| !arg.eq_ignore_ascii_case("-NoExit"));
                let startup = args.last_mut().unwrap();
                // Keep the real integration bootstrap; append the query to its command.
                startup.insert_str(0, "$utf8=[Text.UTF8Encoding]::new($false);[Console]::OutputEncoding=$utf8;$OutputEncoding=$utf8;");
                startup.push_str(&format!(
                    ";[Console]::WriteLine(\"`n{MARKER}$((Get-Location).ProviderPath)\");exit"
                ));
                command.args(["-NoProfile", "-NonInteractive"]).args(args);
            }
            if let Some(cwd) = options.working_directory {
                command.current_dir(cwd);
            }
            let output = crate::platform::process_output::read_cancellable(
                command,
                std::time::Duration::from_secs(25),
                64 * 1024,
                &|| false,
            )
            .unwrap();
            let output = String::from_utf8(output).unwrap();
            let actual = output.lines().find_map(|line| line.strip_prefix(MARKER)).unwrap();
            assert_eq!(actual, expected, "{shell_id} must enter {name}");
            writeln!(std::io::stdout().lock(), "{shell_id} entered {expected}").unwrap();
        }
    }
}
