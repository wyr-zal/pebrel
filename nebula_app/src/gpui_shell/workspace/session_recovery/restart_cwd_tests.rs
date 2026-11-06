use super::*;
use crate::session::{AgentSession, LaunchSession, LayoutSession, Session, TabSession};
use gpui::TestAppContext;

#[gpui::test]
fn reported_cwd_is_saved_and_restored_with_the_same_agent(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::gpui_shell::math_view::register(cx);
        crate::gpui_shell::file_editor::init(cx);
        super::super::init(cx);
        cx.set_reduce_motion(true);
        windowing::initialize(cx, crate::runtime_api::RuntimeHub::new());
    });
    let directory = tempfile::tempdir().unwrap();
    let a = directory.path().join("A");
    let b = directory.path().join("B team's project");
    let other = directory.path().join("other");
    for path in [&a, &b, &other] {
        std::fs::create_dir(path).unwrap();
    }
    let launch = LaunchSession::Shell {
        name: "fixture".into(),
        program: "pebrel-test-missing-shell-executable".into(),
        args: vec![],
    };
    let tabs = [&a, &other]
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
    let agent = AgentSession {
        source: "claude".into(),
        session_id: Some("fixture-session".into()),
        session_file: None,
    };
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
    let restored = window.update(|window, cx| {
        workspace.update(cx, |workspace, cx| {
            assert!(workspace.restore_update_session(&initial, false, window, cx));
            let source = workspace.tabs[0].focused_view().unwrap().clone();
            source.update(cx, |view, cx| {
                view.restore_agent(agent.clone(), cx);
                // This existing entry dispatches the real CwdReport handler.
                view.seed_restored_cwd(b.to_string_lossy().into_owned(), cx);
                assert_eq!(view.tab_label(), "B team's project");
                assert_eq!(view.local_cwd().as_deref(), Some(b.as_path()));
                assert!(
                    view.recovery_pending(),
                    "cwd must update while recovery awaits confirmation"
                );
            });
            let snapshot = workspace.snapshot_session(cx);
            assert_eq!(snapshot.tabs[0].cwd, b.to_string_lossy());
            assert_eq!(snapshot.tabs[1].cwd, other.to_string_lossy());
            let Some(LayoutSession::Pane { cwd, agent: saved_agent, .. }) =
                snapshot.tabs[0].layout.as_ref()
            else {
                panic!("the source pane must be saved");
            };
            assert_eq!(cwd, &b.to_string_lossy());
            assert_eq!(saved_agent.as_ref(), Some(&agent));
            let path = directory.path().join("session.json");
            crate::session::save_to(&path, &snapshot).unwrap();
            crate::session::load_from(&path).unwrap()
        })
    });
    window.update(|window, _| window.remove_window());
    drop(workspace);
    let mut restored_workspace = None;
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
        restored_workspace = Some(view.clone());
        Root::new(view, window, cx)
    });
    window.update(|window, cx| {
        restored_workspace.unwrap().update(cx, |workspace, cx| {
            assert!(workspace.restore_update_session(&restored, true, window, cx));
            let new_snapshot = workspace.snapshot_session(cx);
            assert_eq!(new_snapshot.tabs.len(), 2);
            assert_eq!(new_snapshot.tabs[0].cwd, b.to_string_lossy());
            assert_eq!(new_snapshot.tabs[1].cwd, other.to_string_lossy());
            let Some(LayoutSession::Pane { agent: restored_agent, .. }) =
                new_snapshot.tabs[0].layout.as_ref()
            else {
                panic!("the restored pane must be saved");
            };
            assert_eq!(restored_agent.as_ref(), Some(&agent));
            assert_eq!(restored_agent.as_ref().unwrap().resume_command(), agent.resume_command());
        });
    });
}
