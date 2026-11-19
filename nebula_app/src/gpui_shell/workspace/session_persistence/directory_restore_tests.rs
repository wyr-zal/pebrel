use super::*;
use crate::session::{AgentSession, LayoutSession, TabSession, WindowState};

fn window(cwds: &[&str], active: usize) -> Session {
    let mut session = Session::new(
        active,
        cwds.iter()
            .map(|cwd| {
                let mut tab = TabSession::single((*cwd).into(), Some((*cwd).into()), None);
                tab.layout = Some(LayoutSession::Pane {
                    cwd: (*cwd).into(),
                    custom_name: Some("保留 pane 标题".into()),
                    launch: None,
                    agent: Some(AgentSession {
                        source: "claude".into(),
                        session_id: Some(format!("saved-{}", cwd.len())),
                        session_file: None,
                        wsl_launch: None,
                    }),
                });
                tab
            })
            .collect(),
    );
    session.window = Some(WindowState { width: 950, height: 700, ..Default::default() });
    session
}

fn original() -> Session {
    let mut session = combine_sessions([
        (false, window(&["D:/旧窗口 A", "D:/旧标签 B"], 1)),
        (true, window(&["D:/旧活动窗口 C"], 0)),
    ])
    .unwrap();
    session.boot_attempts = 1;
    session
}

#[test]
fn directory_restore_checkpoints_and_tab_closures_cannot_replace_the_old_document() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.json");
    let original = original();
    crate::session::save_to(&path, &original).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let mut state = SessionPersistence::ordinary_for_test();
    state.begin_directory_restore(original.clone());
    for reason in [SaveReason::Checkpoint, SaveReason::TabsClosed] {
        for partial in [None, Some(Session::new(0, vec![])), Some(window(&["D:/新目录"], 0))] {
            state.save_with(partial, reason, |_| panic!("不能落盘半套恢复窗口")).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(state.latest.as_ref(), Some(&original));
        }
    }
}

#[test]
fn closing_or_quitting_during_construction_preserves_every_old_window_and_target() {
    for reason in [SaveReason::WindowClose, SaveReason::Quit] {
        let mut state = SessionPersistence::ordinary_for_test();
        let mut expected = original();
        state.begin_directory_restore(expected.clone());
        expected.clean_exit = true;
        state
            .save_with(Some(window(&["D:/半套窗口"], 0)), reason, |saved| {
                assert_eq!(saved, &expected);
                assert_eq!(saved.clone().into_update_windows().unwrap().len(), 2);
                Ok(())
            })
            .unwrap();
        assert_eq!(state.saved.as_ref(), Some(&expected));
    }
}

#[test]
fn system_shutdown_during_directory_restore_uses_the_complete_old_snapshot() {
    use crate::platform::session_end::test_support::set_pending;
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            set_pending(false);
        }
    }
    let _reset = Reset;
    for reason in
        [SaveReason::Checkpoint, SaveReason::TabsClosed, SaveReason::WindowClose, SaveReason::Quit]
    {
        let mut state = SessionPersistence::ordinary_for_test();
        let expected = original();
        state.begin_directory_restore(expected.clone());
        set_pending(true);
        state
            .save_with(Some(window(&["D:/半套窗口"], 0)), reason, |saved| {
                assert_eq!(saved.tabs, expected.tabs);
                assert_eq!(saved.window_layout, expected.window_layout);
                Ok(())
            })
            .unwrap();
        assert_eq!(state.saved.as_ref().unwrap().tabs, expected.tabs);
        assert!(!state.quitting);
    }
}

#[test]
fn completed_directory_restore_saves_live_windows_and_the_directory_normally() {
    let mut state = SessionPersistence::ordinary_for_test();
    state.begin_directory_restore(original());
    state.finish_directory_restore(vec![]);
    let current = combine_sessions([
        (false, window(&["D:/旧窗口 A", "D:/旧标签 B"], 1)),
        (true, window(&["D:/旧活动窗口 C", "D:/新目录"], 1)),
    ])
    .unwrap();
    state.save_with(Some(current.clone()), SaveReason::Checkpoint, |_| Ok(())).unwrap();
    assert_eq!(state.saved.as_ref(), Some(&current));
    assert!(state.restoring.is_none());
    assert!(state.unrestored.is_empty());
    state
        .save_with(Some(current), SaveReason::Checkpoint, |_| panic!("相同快照不重复写入"))
        .unwrap();
}

#[test]
fn failed_windows_are_merged_once_and_keep_the_live_active_window() {
    let mut failed = window(&["D:/未建立窗口"], 0);
    failed.boot_attempts = 1;
    let mut state = SessionPersistence::ordinary_for_test();
    state.begin_directory_restore(original());
    state.finish_directory_restore(vec![failed.clone()]);
    let current = combine_sessions([
        (false, window(&["D:/旧窗口 A"], 0)),
        (true, window(&["D:/旧活动窗口 C", "D:/新目录"], 1)),
    ])
    .unwrap();
    state.save_with(Some(current.clone()), SaveReason::Checkpoint, |_| Ok(())).unwrap();
    let saved = state.saved.as_ref().unwrap();
    let windows = saved.clone().into_update_windows().unwrap();
    failed.boot_attempts = 0;
    assert_eq!(windows[0], failed);
    assert_eq!(windows.len(), 3);
    assert_eq!(saved.tabs.len(), current.tabs.len() + 1);
    assert_eq!(saved.tabs[saved.active_tab].cwd, "D:/新目录");
    assert_eq!(saved.boot_attempts, 0, "正常存活保存不重复累计未建立窗口的启动次数");
    state
        .save_with(Some(current), SaveReason::Checkpoint, |_| panic!("不能重复合入失败窗口"))
        .unwrap();
}

#[test]
fn closing_successfully_restored_windows_does_not_resurrect_them_from_the_original() {
    let mut state = SessionPersistence::ordinary_for_test();
    state.begin_directory_restore(original());
    state.finish_directory_restore(vec![window(&["D:/未建立窗口"], 0)]);
    state
        .save_with(
            Some(window(&["D:/旧窗口 A", "D:/新目录"], 1)),
            SaveReason::Checkpoint,
            |_| Ok(()),
        )
        .unwrap();
    state.save_with(Some(window(&["D:/新目录"], 0)), SaveReason::WindowClose, |_| Ok(())).unwrap();
    let saved = state.saved.as_ref().unwrap();
    assert_eq!(saved.tabs.len(), 2);
    assert_eq!(saved.tabs[0].cwd, "D:/未建立窗口");
    assert_eq!(saved.tabs[1].cwd, "D:/新目录");
    assert!(!saved.tabs.iter().any(|tab| tab.cwd == "D:/旧窗口 A"));
    assert!(saved.clean_exit);
}

#[test]
fn closing_all_live_tabs_keeps_only_the_unconstructed_window() {
    let mut state = SessionPersistence::ordinary_for_test();
    state.begin_directory_restore(original());
    let failed = window(&["D:/未建立窗口"], 0);
    state.finish_directory_restore(vec![failed.clone()]);
    state.save_with(Some(Session::new(0, vec![])), SaveReason::TabsClosed, |_| Ok(())).unwrap();
    assert_eq!(state.saved.as_ref().unwrap().tabs, failed.tabs);
    assert_eq!(state.saved.as_ref().unwrap().boot_attempts, 0);
}

#[test]
fn failed_complete_write_retries_without_duplicating_unconstructed_windows() {
    let mut state = SessionPersistence::ordinary_for_test();
    state.begin_directory_restore(original());
    state.finish_directory_restore(vec![window(&["D:/未建立窗口"], 0)]);
    let current = window(&["D:/新目录"], 0);
    assert!(
        state
            .save_with(Some(current), SaveReason::Checkpoint, |_| {
                Err(std::io::Error::new(std::io::ErrorKind::StorageFull, "fixture disk full"))
            })
            .is_err()
    );
    assert!(state.saved.is_none());
    let expected = state.latest.clone().unwrap();
    assert_eq!(expected.tabs.len(), 2);
    state
        .save_with(None, SaveReason::Checkpoint, |saved| {
            assert_eq!(saved, &expected);
            Ok(())
        })
        .unwrap();
    assert_eq!(state.saved.as_ref(), Some(&expected));
    state.save_with(None, SaveReason::Quit, |_| Ok(())).unwrap();
    assert_eq!(state.saved.as_ref().unwrap().tabs, expected.tabs);
    assert!(state.saved.as_ref().unwrap().clean_exit);
}

#[test]
fn failed_exit_during_restore_can_retry_the_complete_snapshot() {
    let mut state = SessionPersistence::ordinary_for_test();
    let expected = original();
    state.begin_directory_restore(expected.clone());
    assert!(
        state
            .save_with(None, SaveReason::Quit, |_| Err(std::io::Error::other("fixture failure")))
            .is_err()
    );
    assert!(!state.quitting);
    state
        .save_with(Some(Session::new(0, vec![])), SaveReason::Quit, |saved| {
            assert_eq!(saved.tabs, expected.tabs);
            assert_eq!(saved.window_layout, expected.window_layout);
            Ok(())
        })
        .unwrap();
    assert!(state.saved.as_ref().unwrap().clean_exit);
}
