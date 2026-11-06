use super::*;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

fn guest_path(path: &Path) -> String {
    let mut command = Command::new("wsl.exe");
    command.args(["-d", "Ubuntu-22.04", "-e", "wslpath", "-u", "--"]);
    command.arg(path);
    crate::platform::process::hidden_command(&mut command);
    let output = crate::platform::process_output::read_cancellable(
        command,
        Duration::from_secs(20),
        4096,
        &|| false,
    )
    .unwrap();
    String::from_utf8(output).unwrap().trim().to_owned()
}

#[test]
#[ignore = "requires Ubuntu-22.04 and PowerShell 7; uses only temporary fake Agent scripts"]
fn powershell_replays_the_saved_wsl_command_into_the_actual_guest_directory() {
    const ID: &str = "01a1aedc-d6f0-7db0-8875-882c249ef4a2";
    let root = tempfile::tempdir().unwrap();
    let initial = root.path().join("A");
    let current = root.path().join("B's 项目");
    std::fs::create_dir(&initial).unwrap();
    std::fs::create_dir(&current).unwrap();
    let guest_root = guest_path(root.path());
    let initial = format!("{guest_root}/A");
    let current = format!("{guest_root}/B's 项目");
    let report = format!("{guest_root}/fake-agent-report");
    let history = format!("{guest_root}/shell-history");
    for kind in [AgentKind::Claude, AgentKind::Codex] {
        let program = format!("{guest_root}/{}", kind.slug());
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\" > {}\n",
            quoted(std::slice::from_ref(&report), PathQuote::Posix).unwrap(),
        );
        std::fs::write(root.path().join(kind.slug()), script).unwrap();
        let cli = vec![
            program,
            if kind == AgentKind::Codex { "resume" } else { "--resume" }.into(),
            ID.into(),
        ];
        let inner = quoted(&cli, PathQuote::Posix).unwrap();
        let args = vec![
            "-d".into(),
            "Ubuntu-22.04".into(),
            "--cd".into(),
            initial.clone(),
            "-e".into(),
            "sh".into(),
            "-lc".into(),
            format!("exec ${{SHELL:-sh}} -lic {}", quoted(&[inner], PathQuote::Posix).unwrap()),
        ];
        let agent = crate::session::AgentSession {
            source: kind.slug().into(),
            session_id: Some(ID.into()),
            session_file: None,
            wsl_launch: Some(WslAgentLaunch::capture(
                "wsl.exe",
                &args,
                "Ubuntu-22.04",
                None,
                &current,
                kind,
            )),
        };
        let file = root.path().join("saved-agent.json");
        std::fs::write(&file, serde_json::to_vec(&agent).unwrap()).unwrap();
        let restored: crate::session::AgentSession =
            serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        let command_text = restored.resume_command().unwrap();
        let mut command =
            Command::new(r"C:\Users\12704\AppData\Local\Microsoft\WindowsApps\pwsh.exe");
        command.args(["-NoProfile", "-Command", &command_text]);
        // The child keeps its shell history in the temporary fixture. The
        // executable is an absolute fake CLI path, never the real provider.
        command.env("WSLENV", "HISTFILE").env("HISTFILE", &history);
        crate::platform::process::hidden_command(&mut command);
        crate::platform::process_output::read_cancellable(
            command,
            Duration::from_secs(25),
            64 * 1024,
            &|| false,
        )
        .unwrap();
        let output = std::fs::read_to_string(root.path().join("fake-agent-report")).unwrap();
        let lines: Vec<_> = output.lines().collect();
        assert_eq!(lines, [current.as_str(), cli[1].as_str(), ID], "{kind:?}");
    }
}
