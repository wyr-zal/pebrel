use super::{cwd_copy_tests::open_source, file_manager_directory, open_working_directory_item};
use crate::ai_agents::AgentKind;
use crate::gpui_shell::terminal::view::TerminalView;
use crate::platform::agent_process_identity::{ForegroundAgent, ProcessStamp};
use crate::platform::wsl_location::ObservedWslLocation;
use gpui::TestAppContext;
use gpui_component::menu::PopupMenuItem;
use std::path::Path;

fn observe(view: &mut TerminalView, host_path: &Path) {
    let host = host_path.to_string_lossy().replace('\\', "/");
    let guest = format!("/mnt/{}{}", host[..1].to_ascii_lowercase(), &host[2..]);
    view.set_observed_wsl_location_for_test(Some(ObservedWslLocation {
        distro: "pebrel-test-nested-distro".into(),
        user: None,
        cwd: guest,
        launcher: ProcessStamp { pid: 10, start_time: 100 },
        agent: ForegroundAgent {
            kind: AgentKind::Claude,
            process: ProcessStamp { pid: 20, start_time: 200 },
            shell: ProcessStamp { pid: 30, start_time: 300 },
            tty_nr: 4,
            process_group: 20,
        },
    }));
}

#[gpui::test]
fn observed_wsl_directory_overrides_both_host_and_wsl_panes(cx: &mut TestAppContext) {
    let (source, window) = open_source(cx);
    let host = tempfile::tempdir().unwrap();
    let guest = tempfile::Builder::new().prefix("wsl directory with spaces ").tempdir().unwrap();
    source.update(window, |view, _| {
        for program in ["pwsh.exe", "wsl.exe"] {
            let mut options = nebula_terminal::tty::Options::default();
            options.shell = Some(nebula_terminal::tty::Shell::new(
                program.into(),
                vec!["-d".into(), "pebrel-test-host-distro".into()],
            ));
            view.exec_context =
                Some(crate::runtime_exec::PaneExecContext::from_pty_options(&options));
            view.cwd = host.path().to_string_lossy().into_owned();
            observe(view, guest.path());
            assert_eq!(view.observed_wsl_cwd().unwrap().0, "pebrel-test-nested-distro");
            assert_eq!(view.tab_label(), guest.path().file_name().unwrap().to_str().unwrap());
            assert_eq!(file_manager_directory(view), Some(guest.path().to_path_buf()), "{program}");
            assert_eq!(view.local_cwd(), Some(host.path().to_path_buf()));
        }
    });
}

#[gpui::test]
fn observed_wsl_directory_tracks_the_latest_location(cx: &mut TestAppContext) {
    let (source, window) = open_source(cx);
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    source.update(window, |view, _| {
        view.cwd.clear();
        observe(view, first.path());
        assert_eq!(file_manager_directory(view), Some(first.path().to_path_buf()));
        observe(view, second.path());
        assert_eq!(file_manager_directory(view), Some(second.path().to_path_buf()));
    });
}

#[gpui::test]
fn observed_wsl_missing_directory_disables_menu_without_host_fallback(cx: &mut TestAppContext) {
    let (source, window) = open_source(cx);
    let host = tempfile::tempdir().unwrap();
    source.update(window, |view, _| {
        view.cwd = host.path().to_string_lossy().into_owned();
        observe(view, &host.path().join("missing-wsl-directory"));
        assert_eq!(file_manager_directory(view), None);
        assert_eq!(view.local_cwd(), Some(host.path().to_path_buf()));
    });
    window.update(|_, cx| {
        let PopupMenuItem::Item { disabled, .. } = open_working_directory_item(&source, cx) else {
            panic!("expected a standard menu item")
        };
        assert!(disabled);
    });
}

#[gpui::test]
fn cleared_wsl_observation_restores_the_host_directory(cx: &mut TestAppContext) {
    let (source, window) = open_source(cx);
    let host = tempfile::tempdir().unwrap();
    let guest = tempfile::tempdir().unwrap();
    source.update(window, |view, _| {
        view.cwd = host.path().to_string_lossy().into_owned();
        observe(view, guest.path());
        assert_eq!(file_manager_directory(view), Some(guest.path().to_path_buf()));
        view.set_observed_wsl_location_for_test(None);
        assert_eq!(file_manager_directory(view), Some(host.path().to_path_buf()));
    });
}

#[gpui::test]
fn observed_wsl_directory_does_not_enable_ssh_file_manager(cx: &mut TestAppContext) {
    let (source, window) = open_source(cx);
    let guest = tempfile::tempdir().unwrap();
    source.update(window, |view, _| {
        observe(view, guest.path());
        view.ssh_destination = Some("remote".into());
        assert_eq!(file_manager_directory(view), None);
    });
    window.update(|_, cx| {
        let PopupMenuItem::Item { disabled, .. } = open_working_directory_item(&source, cx) else {
            panic!("expected a standard menu item")
        };
        assert!(disabled);
    });
}
