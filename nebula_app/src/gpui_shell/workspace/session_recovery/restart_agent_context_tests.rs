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
    for path in [&host, &other] {
        std::fs::create_dir(path).unwrap();
    }
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
            crate::session::load_from(&file).unwrap()
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
