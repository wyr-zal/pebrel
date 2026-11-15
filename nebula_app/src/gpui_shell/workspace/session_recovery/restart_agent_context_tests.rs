use super::*;
use crate::ai_agents::AgentKind;
use crate::session::{LaunchSession, LayoutSession, Session, TabSession};
use gpui::TestAppContext;

#[gpui::test]
fn observed_agent_launch_is_saved_and_restored_after_the_original_window_is_closed(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::gpui_shell::math_view::register(cx);
        crate::gpui_shell::file_editor::init(cx);
        super::super::init(cx);
        cx.set_reduce_motion(true);
        windowing::initialize(cx, crate::runtime_api::RuntimeHub::new());
    });
    let directory = tempfile::tempdir().unwrap();
    let host = directory.path().join("host A");
    let other = directory.path().join("other tab");
    let mounted = directory.path().join("guest C's 目录");
    for path in [&host, &other, &mounted] {
        std::fs::create_dir(path).unwrap();
    }
    let mounted_path = mounted.to_string_lossy().replace('\\', "/");
    let mounted_guest =
        format!("/mnt/{}{}", mounted_path[..1].to_ascii_lowercase(), &mounted_path[2..]);
    let guest = "/mnt/f/guest B's 项目";
    let id = "01a1aedc-d6f0-7db0-8875-882c249ef4a2";
    let launch = LaunchSession::Shell {
        name: "fixture".into(),
        program: "pebrel-test-missing-shell-executable".into(),
        args: vec![],
    };
    let tabs = [&host, &other]
        .into_iter()
        .map(|path| TabSession {
            cwd: path.to_string_lossy().into_owned(),
            custom_name: None,
            color: None,
            launch: Some(launch.clone()),
            layout: None,
            active_pane: 0,
        })
        .collect();
    let initial = Session::new(0, tabs);
    let mut workspace = None;
    let (_, window) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NebulaWorkspace::new(
                window,
                None,
                None,
                1,
                crate::runtime_api::RuntimeHub::new(),
                windowing::WorkspaceStartup::Empty,
                windowing::WindowRole::Regular,
                cx,
            )
        });
        workspace = Some(view.clone());
        Root::new(view, window, cx)
    });
    let workspace = workspace.unwrap();
    let snapshot = window.update(|window, cx| {
        workspace.update(cx, |workspace, cx| {
            assert!(workspace.restore_update_session(&initial, false, window, cx));
            workspace.tabs[0].focused_view().unwrap().update(cx, |view, cx| {
                view.install_wsl_restart_context_test_fixture(AgentKind::Codex, id, guest, cx);
                view.seed_restored_cwd(host.to_string_lossy().into_owned(), cx);
                assert_eq!(view.tab_label(), "guest B's 项目");
            });
            let snapshot = workspace.snapshot_session(cx);
            assert_eq!(snapshot.tabs[0].cwd, host.to_string_lossy());
            assert_eq!(snapshot.tabs[1].cwd, other.to_string_lossy());
            let Some(LayoutSession::Pane { agent: Some(agent), .. }) = &snapshot.tabs[0].layout
            else {
                panic!("the observed Agent launch must be persisted");
            };
            assert_eq!(agent.session_id.as_deref(), Some(id));
            let context = agent.wsl_launch.as_ref().unwrap();
            assert_eq!(context.distribution, "Ubuntu-22.04");
            assert_eq!(context.cwd, guest);
            assert!(agent.resume_command().unwrap().contains("-e sh -lc"));
            let file = directory.path().join("session.json");
            crate::session::save_to(&file, &snapshot).unwrap();
            let saved = crate::session::load_from(&file).unwrap();
            let first = workspace.tabs[0].focused_view().unwrap().clone();
            let located =
                crate::shell_detect::WslCwd { distro: "Ubuntu-22.04".into(), guest: guest.into() };
            assert_eq!(workspace.active_wsl_cwd(cx), Some(located.clone()));
            workspace.side_panel.open = true;
            workspace.on_terminal_event(&first, &TerminalViewEvent::TitleChanged, window, cx);
            assert_eq!(workspace.side_panel.file_wsl_root(), Some(&located));
            first.update(cx, |view, cx| {
                view.install_wsl_restart_context_test_fixture(
                    AgentKind::Codex,
                    id,
                    &mounted_guest,
                    cx,
                );
                view.seed_restored_cwd(host.to_string_lossy().into_owned(), cx);
            });
            workspace.side_panel.view = crate::display::side_panel::PanelView::Git;
            workspace.on_terminal_event(&first, &TerminalViewEvent::TitleChanged, window, cx);
            assert_eq!(workspace.active_wsl_cwd(cx).unwrap().guest, mounted_guest);
            assert_eq!(workspace.side_panel.root(), Some(mounted.as_path()));
            assert_eq!(workspace.side_panel.file_wsl_root(), None);
            assert_eq!(first.read(cx).local_cwd(), Some(host.clone()));
            workspace.active = 1;
            workspace.sync_side_panel_to_active(true, cx);
            workspace.on_terminal_event(&first, &TerminalViewEvent::TitleChanged, window, cx);
            assert_eq!(workspace.side_panel.root(), Some(other.as_path()));
            assert_eq!(workspace.active_wsl_cwd(cx), None);
            assert_eq!(workspace.snapshot_session(cx).tabs[1].cwd, other.to_string_lossy());
            workspace.active = 0;
            workspace.side_panel.view = crate::display::side_panel::PanelView::Files;
            workspace.sync_side_panel_to_active(true, cx);
            assert_eq!(workspace.side_panel.root(), Some(mounted.as_path()));
            first.update(cx, |view, _| view.set_observed_wsl_location_for_test(None));
            workspace.on_terminal_event(&first, &TerminalViewEvent::TitleChanged, window, cx);
            assert_eq!(workspace.active_wsl_cwd(cx), None);
            assert_eq!(workspace.side_panel.root(), Some(host.as_path()));
            first.update(cx, |view, cx| {
                let shell = nebula_terminal::tty::Shell::new(
                    "wsl.exe".into(),
                    vec!["-d".into(), "FrozenDistro".into()],
                );
                view.exec_context = Some(crate::runtime_exec::PaneExecContext::from_pty_options(
                    &nebula_terminal::tty::Options { shell: Some(shell), ..Default::default() },
                ));
                view.seed_restored_cwd("/home/worker/native".into(), cx);
            });
            let native = crate::shell_detect::WslCwd {
                distro: "FrozenDistro".into(),
                guest: "/home/worker/native".into(),
            };
            assert_eq!(workspace.active_wsl_cwd(cx), Some(native.clone()));
            workspace.sync_side_panel_to_active(false, cx);
            assert_eq!(workspace.side_panel.file_wsl_root(), Some(&native));
            first.update(cx, |view, _| view.ssh_destination = Some("fixture-host".into()));
            assert_eq!(workspace.active_wsl_cwd(cx), None);
            saved
        })
    });
    window.update(|window, _| window.remove_window());
    drop(workspace);
    let mut restored = None;
    let (_, window) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            NebulaWorkspace::new(
                window,
                None,
                None,
                2,
                crate::runtime_api::RuntimeHub::new(),
                windowing::WorkspaceStartup::Empty,
                windowing::WindowRole::Regular,
                cx,
            )
        });
        restored = Some(view.clone());
        Root::new(view, window, cx)
    });
    window.update(|window, cx| {
        restored.unwrap().update(cx, |workspace, cx| {
            assert!(workspace.restore_update_session(&snapshot, true, window, cx));
            let recovered = workspace.snapshot_session(cx);
            assert_eq!(recovered.tabs.len(), 2);
            assert_eq!(recovered.tabs[0].cwd, host.to_string_lossy());
            assert_eq!(recovered.tabs[1].cwd, other.to_string_lossy());
            assert_eq!(recovered.tabs[0].layout, snapshot.tabs[0].layout);
            let Some(LayoutSession::Pane { agent: Some(agent), .. }) = &recovered.tabs[0].layout
            else {
                panic!("the new workspace must retain the exact resume context");
            };
            assert!(agent.resume_command().unwrap().contains(id));
            assert!(agent.resume_command().unwrap().contains("--cd '/mnt/f/guest B''s 项目'"));
        })
    });
}
