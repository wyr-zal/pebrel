use super::*;
use crate::gpui_shell::settings_fixture::{SettingsBytesGuard, lock_theme_studio};
use crate::session::{AgentSession, LaunchSession, LayoutSession, Session, SplitAxis, TabSession};

struct SessionBytesGuard {
    files: Vec<(PathBuf, Option<Vec<u8>>)>,
}

impl SessionBytesGuard {
    fn capture() -> Self {
        let files = ["session.json", "session.crashed.json"]
            .into_iter()
            .map(|name| {
                let path = crate::display::nebula_data_dir().join(name);
                let bytes = std::fs::read(&path).ok();
                (path, bytes)
            })
            .collect();
        Self { files }
    }
}

impl Drop for SessionBytesGuard {
    fn drop(&mut self) {
        for (path, bytes) in &self.files {
            if let Some(bytes) = bytes {
                std::fs::write(path, bytes).unwrap();
            } else if path.is_file() {
                std::fs::remove_file(path).unwrap();
            }
        }
    }
}

struct BlockQuarantine {
    path: PathBuf,
    previous: Option<PathBuf>,
    _backup: tempfile::TempDir,
}

impl BlockQuarantine {
    fn new() -> Self {
        let path = crate::display::nebula_data_dir().join("session.crashed.json");
        let backup = tempfile::tempdir_in(path.parent().unwrap()).unwrap();
        let previous = path.exists().then(|| backup.path().join("previous"));
        if let Some(previous) = &previous {
            std::fs::rename(&path, previous).unwrap();
        }
        // 目录占用隔离文件名，让真实 fs::copy 稳定失败，同时 session.json 仍可写。
        std::fs::create_dir(&path).unwrap();
        Self { path, previous, _backup: backup }
    }
}

impl Drop for BlockQuarantine {
    fn drop(&mut self) {
        std::fs::remove_dir(&self.path).unwrap();
        if let Some(previous) = &self.previous {
            std::fs::rename(previous, &self.path).unwrap();
        }
    }
}

thread_local! {
    static FAILED_WINDOW: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

struct FailWindow;

impl FailWindow {
    fn new(name: &str) -> Self {
        FAILED_WINDOW.with(|failed| {
            assert!(failed.borrow().is_none());
            *failed.borrow_mut() = Some(name.into());
        });
        Self
    }
}

impl Drop for FailWindow {
    fn drop(&mut self) {
        FAILED_WINDOW.with(|failed| *failed.borrow_mut() = None);
    }
}

pub(super) fn before_window_open(session: &Session) -> gpui::Result<()> {
    let fail = FAILED_WINDOW.with(|failed| {
        failed.borrow().as_deref().is_some_and(|name| {
            session.tabs.first().is_some_and(|tab| tab.custom_name.as_deref() == Some(name))
        })
    });
    if fail {
        Err(std::io::Error::other("fixture native window creation failure").into())
    } else {
        Ok(())
    }
}

fn configure(behavior: &str, restore: bool, resume: bool, shell: &str) {
    nebula_settings::persist_keys(&[
        ("restore_session", u8::from(restore).to_string()),
        ("resume_ai", u8::from(resume).to_string()),
        ("shell", shell.into()),
        ("windowing_behavior", behavior.into()),
        ("new_tab_position", "end".into()),
        ("tray", "0".into()),
    ])
    .unwrap();
}

fn initialize_test(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::gpui_shell::math_view::register(cx);
        crate::gpui_shell::file_editor::init(cx);
        super::super::super::init(cx);
        cx.set_reduce_motion(true);
        let settings = crate::gpui_shell::config::Settings::load_current(cx);
        cx.set_global(settings);
        initialize(cx, crate::runtime_api::RuntimeHub::new());
        cx.global_mut::<WindowRegistry>().session_persistence =
            SessionPersistence::ordinary_for_test();
    });
}

fn saved_window(directory: &std::path::Path, names: &[&str], active: usize) -> Session {
    Session::new(
        active,
        names
            .iter()
            .map(|name| {
                let path = directory.join(name);
                std::fs::create_dir_all(&path).unwrap();
                let mut tab = TabSession::single(
                    path.to_string_lossy().into_owned(),
                    Some((*name).into()),
                    None,
                );
                tab.launch = Some(LaunchSession::Shell {
                    name: "隔离恢复替身".into(),
                    program: "pebrel-test-missing-shell-executable".into(),
                    args: vec![],
                });
                tab
            })
            .collect(),
    )
}

fn start_directory(cx: &mut App, folder: &std::path::Path) {
    let (_, ai_events) = std::sync::mpsc::channel();
    let (_, shell_events) = std::sync::mpsc::channel();
    if crate::platform::elevation::requires_isolation() {
        // 管理员 CI 直接测普通恢复编排；产品入口继续拒绝恢复共享普通快照。
        directory_restore::open_directory_startup(cx, ai_events, shell_events, folder.into())
            .unwrap();
    } else {
        open_initial_window(
            cx,
            ai_events,
            shell_events,
            Some(folder.into()),
            None,
            None,
            false,
            true,
        );
    }
}

fn remove_windows(cx: &mut App) {
    for entry in cx.global::<WindowRegistry>().entries.clone() {
        entry.handle.update(cx, |_, window, _| window.remove_window()).unwrap();
    }
    prune_entries(cx);
}

fn assert_old_tabs_survive(original: &Session, snapshot: &Session) {
    for old in &original.tabs {
        let restored = snapshot.tabs.iter().find(|tab| tab.cwd == old.cwd).unwrap();
        assert_eq!(restored.custom_name, old.custom_name);
        assert_eq!(restored.launch, old.launch);
        assert_eq!(restored.active_pane, old.active_pane);
        if old.layout.is_some() {
            assert_eq!(restored.layout, old.layout);
        }
    }
}

#[gpui::test]
fn explorer_cold_start_restores_every_old_window_before_opening_its_folder(
    cx: &mut gpui::TestAppContext,
) {
    let _lock = lock_theme_studio();
    let _settings = SettingsBytesGuard::capture();
    let _session = SessionBytesGuard::capture();
    configure("use_any_existing", true, true, "pwsh");
    initialize_test(cx);
    let root = tempfile::Builder::new().prefix("pebrel 冷恢复 ").tempdir().unwrap();
    let mut active_window = saved_window(root.path(), &["旧窗口 A", "旧标签 B"], 1);
    let second_pane = root.path().join("分屏 C's 目录");
    std::fs::create_dir(&second_pane).unwrap();
    let launch = active_window.tabs[0].launch.clone();
    active_window.tabs[0].layout = Some(LayoutSession::Split {
        axis: SplitAxis::LeftRight,
        ratio_permille: 370,
        first: Box::new(LayoutSession::Pane {
            cwd: active_window.tabs[0].cwd.clone(),
            custom_name: Some("左分屏".into()),
            launch: launch.clone(),
            agent: Some(AgentSession {
                source: "claude".into(),
                session_id: Some("saved-cold-start-42".into()),
                session_file: None,
                wsl_launch: None,
            }),
        }),
        second: Box::new(LayoutSession::Pane {
            cwd: second_pane.to_string_lossy().into_owned(),
            custom_name: Some("右分屏".into()),
            launch,
            agent: None,
        }),
    });
    active_window.tabs[0].active_pane = 1;
    active_window.tabs[0].cwd = second_pane.to_string_lossy().into_owned();
    // 故意先保存活动窗口，确保拆窗后仍最后建立并承接新目录。
    let original = crate::session::combine_sessions([
        (true, active_window),
        (false, saved_window(root.path(), &["旧非活动窗口 D"], 0)),
    ])
    .unwrap();
    crate::session::try_save(&original).unwrap();
    let folder = root.path().join("资源管理器当前目录");
    std::fs::create_dir(&folder).unwrap();
    cx.update(|cx| {
        start_directory(cx, &folder);
        let registry = cx.global::<WindowRegistry>();
        assert_eq!(registry.entries.len(), 2, "冷启动必须保留两扇旧窗口");
        assert!(registry.update_restore_windows.is_none(), "普通恢复不能占用更新事务");
        let first = registry.entries[0].workspace.upgrade().unwrap();
        assert_eq!(first.read(cx).tabs.len(), 1);
        let active = registry.entries[1].workspace.upgrade().unwrap();
        assert_eq!(active.read(cx).tabs.len(), 3);
        assert_eq!(active.read(cx).active, 2);
        let restored_tab = &active.read(cx).tabs[0];
        let WorkspaceTab::Terminal { panes, .. } = restored_tab else {
            panic!("分屏必须恢复为终端标签");
        };
        assert_eq!(panes.len(), 2);
        assert!(panes[0].view.read(cx).session.is_none(), "真实 PTY 启动失败仍保留旧标签");
        assert!(panes[0].view.read(cx).recovery_pending(), "追加目录不等待 Agent 确认");
        assert!(!panes[0].view.read(cx).recovery_ready());
        let snapshot = combined_session(None, cx).unwrap();
        assert_eq!(snapshot.tabs.len(), original.tabs.len() + 1);
        assert_old_tabs_survive(&original, &snapshot);
        assert_eq!(snapshot.tabs[snapshot.active_tab].cwd, folder.to_string_lossy());
        save_combined_session(cx, false).unwrap();
        save_combined_session(cx, true).unwrap();
        let saved = crate::session::load().unwrap();
        assert_eq!(saved.tabs, snapshot.tabs);
        assert!(saved.clean_exit);
        assert_eq!(saved.clone().into_update_windows().unwrap().len(), 2);
        remove_windows(cx);
        // 等价新进程：清除旧 registry 的保存状态，只从正式磁盘快照再次冷启动。
        cx.global_mut::<WindowRegistry>().session_persistence =
            SessionPersistence::ordinary_for_test();
        let next = root.path().join("next A folder");
        std::fs::create_dir(&next).unwrap();
        start_directory(cx, &next);
        assert_eq!(cx.global::<WindowRegistry>().entries.len(), 2);
        let reopened = combined_session(None, cx).unwrap();
        assert_old_tabs_survive(&original, &reopened);
        assert_eq!(reopened.tabs.len(), original.tabs.len() + 2);
        assert_eq!(
            reopened.tabs.iter().filter(|tab| tab.cwd == folder.to_string_lossy()).count(),
            1
        );
        assert_eq!(reopened.tabs[reopened.active_tab].cwd, next.to_string_lossy());
        remove_windows(cx);
    });
}

#[gpui::test]
fn new_window_policy_restores_old_active_tabs_and_keeps_the_default_wsl_launch(
    cx: &mut gpui::TestAppContext,
) {
    let _lock = lock_theme_studio();
    let _settings = SettingsBytesGuard::capture();
    let _session = SessionBytesGuard::capture();
    configure("use_new", true, false, "wsl:Pebrel-Cold-Restore-Fixture");
    initialize_test(cx);
    let root = tempfile::tempdir().unwrap();
    let original = crate::session::combine_sessions([
        (false, saved_window(root.path(), &["window A", "window B"], 1)),
        (true, saved_window(root.path(), &["window C"], 0)),
    ])
    .unwrap();
    crate::session::try_save(&original).unwrap();
    let folder = root.path().join("新建 WSL 目录");
    std::fs::create_dir(&folder).unwrap();
    cx.update(|cx| {
        start_directory(cx, &folder);
        let entries = &cx.global::<WindowRegistry>().entries;
        assert_eq!(entries.len(), 3);
        let first = entries[0].workspace.upgrade().unwrap();
        assert_eq!(first.read(cx).active, 1);
        assert_eq!(first.read(cx).tabs.len(), 2);
        let second = entries[1].workspace.upgrade().unwrap();
        assert_eq!(second.read(cx).active, 0);
        assert_eq!(second.read(cx).tabs.len(), 1);
        let new = entries[2].workspace.upgrade().unwrap();
        assert_eq!(new.read(cx).tabs.len(), 1);
        let snapshot = combined_session(None, cx).unwrap();
        assert_old_tabs_survive(&original, &snapshot);
        let current = &snapshot.tabs[snapshot.active_tab];
        assert_eq!(current.cwd, folder.to_string_lossy());
        let Some(LaunchSession::Shell { program, args, .. }) = &current.launch else {
            panic!("新终端应使用设置中的 WSL");
        };
        assert!(crate::shell_detect::is_wsl_launcher(program));
        assert!(args.iter().any(|arg| arg == "Pebrel-Cold-Restore-Fixture"));
        assert!(args.iter().any(|arg| arg == "--cd"));
        assert!(args.iter().any(|arg| arg.contains("新建 WSL 目录")));
        remove_windows(cx);
    });
}

#[gpui::test]
fn current_desktop_policy_uses_the_existing_window_selector(cx: &mut gpui::TestAppContext) {
    let _lock = lock_theme_studio();
    let _settings = SettingsBytesGuard::capture();
    let _session = SessionBytesGuard::capture();
    configure("use_existing", true, false, "pwsh");
    initialize_test(cx);
    let root = tempfile::tempdir().unwrap();
    let original = crate::session::combine_sessions([
        (false, saved_window(root.path(), &["old A"], 0)),
        (true, saved_window(root.path(), &["old B"], 0)),
    ])
    .unwrap();
    crate::session::try_save(&original).unwrap();
    let folder = root.path().join("desktop folder");
    std::fs::create_dir(&folder).unwrap();
    cx.update(|cx| {
        start_directory(cx, &folder);
        let snapshot = combined_session(None, cx).unwrap();
        assert_eq!(snapshot.tabs.len(), original.tabs.len() + 1);
        assert_old_tabs_survive(&original, &snapshot);
        assert_eq!(snapshot.tabs[snapshot.active_tab].cwd, folder.to_string_lossy());
        let selected = select_mru_window(nebula_settings::WindowingBehaviorName::UseExisting, cx)
            .expect("测试窗口的虚拟桌面查询不可用时复用 MRU");
        let workspace = selected.workspace.upgrade().unwrap();
        let selected_snapshot = workspace.read(cx).snapshot_session(cx);
        assert_eq!(
            selected_snapshot.tabs[selected_snapshot.active_tab].cwd,
            folder.to_string_lossy()
        );
        remove_windows(cx);
    });
}

#[gpui::test]
fn failed_window_creation_preserves_old_targets_and_the_single_event_receiver(
    cx: &mut gpui::TestAppContext,
) {
    let _lock = lock_theme_studio();
    let _settings = SettingsBytesGuard::capture();
    let _session = SessionBytesGuard::capture();
    configure("use_new", true, false, "pwsh");
    initialize_test(cx);
    let root = tempfile::tempdir().unwrap();
    for first_fails in [false, true] {
        let original = crate::session::combine_sessions([
            (false, saved_window(root.path(), &["first window"], 0)),
            (false, saved_window(root.path(), &["failed window"], 0)),
            (true, saved_window(root.path(), &["last window"], 0)),
        ])
        .unwrap();
        crate::session::try_save(&original).unwrap();
        let folder = root.path().join(format!("failure-{first_fails} 新目录"));
        std::fs::create_dir(&folder).unwrap();
        let _failure = FailWindow::new(if first_fails { "first window" } else { "failed window" });
        cx.update(|cx| {
            cx.global_mut::<WindowRegistry>().session_persistence =
                SessionPersistence::ordinary_for_test();
            let (_, ai_events) = std::sync::mpsc::channel();
            let (sender, shell_events) = std::sync::mpsc::channel();
            let opened = directory_restore::open_directory_startup(
                cx,
                ai_events,
                shell_events,
                folder.clone(),
            );
            assert_eq!(opened.is_err(), first_fails);
            if first_fails {
                assert!(cx.global::<WindowRegistry>().entries.is_empty());
                assert!(sender.send(GpuiShellEvent::TrayFocus(None)).is_err());
            } else {
                assert_eq!(cx.global::<WindowRegistry>().entries.len(), 3);
                assert!(sender.send(GpuiShellEvent::TrayFocus(None)).is_ok());
                let live = combined_session(None, cx).unwrap();
                assert_eq!(live.tabs.len(), 3);
                assert!(
                    !live
                        .tabs
                        .iter()
                        .any(|tab| tab.custom_name.as_deref() == Some("failed window"))
                );
            }
            save_combined_session(cx, false).unwrap();
            save_combined_session(cx, true).unwrap();
            let saved = crate::session::load().unwrap();
            assert_old_tabs_survive(&original, &saved);
            if !first_fails {
                assert_eq!(saved.tabs.len(), original.tabs.len() + 1);
                assert_eq!(saved.clone().into_update_windows().unwrap().len(), 4);
                assert_eq!(saved.tabs[saved.active_tab].cwd, folder.to_string_lossy());
            }
            remove_windows(cx);
        });
    }
}

#[gpui::test]
fn failed_quarantine_cannot_let_the_new_folder_overwrite_the_old_record(
    cx: &mut gpui::TestAppContext,
) {
    let _lock = lock_theme_studio();
    let _settings = SettingsBytesGuard::capture();
    let _session = SessionBytesGuard::capture();
    configure("use_new", true, false, "pwsh");
    initialize_test(cx);
    let root = tempfile::tempdir().unwrap();
    let _block = BlockQuarantine::new();
    for bad_boundaries in [false, true] {
        let mut original = saved_window(root.path(), &["保留旧目标"], 0);
        if bad_boundaries {
            original = crate::session::combine_sessions([(true, original)]).unwrap();
            let mut value = serde_json::to_value(&original).unwrap();
            value["window_layout"][0]["tab_count"] = serde_json::json!(0);
            original = serde_json::from_value(value).unwrap();
        } else {
            original.boot_attempts = u32::MAX;
        }
        crate::session::try_save(&original).unwrap();
        let path = crate::display::nebula_data_dir().join("session.json");
        let bytes = std::fs::read(&path).unwrap();
        let folder = root.path().join(format!("quarantine-{bad_boundaries} 新目录"));
        std::fs::create_dir(&folder).unwrap();
        cx.update(|cx| {
            cx.global_mut::<WindowRegistry>().session_persistence =
                SessionPersistence::ordinary_for_test();
            start_directory(cx, &folder);
            let live = combined_session(None, cx).unwrap();
            assert_eq!(live.tabs.len(), 1);
            assert_eq!(live.tabs[0].cwd, folder.to_string_lossy());
            save_combined_session(cx, false).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            save_combined_session(cx, true).unwrap();
            let saved = crate::session::load().unwrap();
            assert_eq!(saved.tabs, original.tabs);
            assert_eq!(saved.window_layout, original.window_layout);
            assert_eq!(saved.boot_attempts, original.boot_attempts);
            remove_windows(cx);
        });
    }
}

#[gpui::test]
fn disabled_empty_and_rejected_recovery_only_open_the_requested_folder(
    cx: &mut gpui::TestAppContext,
) {
    let _lock = lock_theme_studio();
    let _settings = SettingsBytesGuard::capture();
    let _session = SessionBytesGuard::capture();
    configure("use_new", true, false, "pwsh");
    initialize_test(cx);
    let root = tempfile::tempdir().unwrap();
    for case in ["disabled", "missing", "empty", "boot_limit", "bad_boundaries", "invalid_json"] {
        configure("use_new", case != "disabled", false, "pwsh");
        let mut original = saved_window(root.path(), &["old folder"], 0);
        if case == "empty" {
            original.tabs.clear();
        } else if case == "boot_limit" {
            original.boot_attempts = u32::MAX;
        }
        if case == "bad_boundaries" {
            original = crate::session::combine_sessions([(true, original)]).unwrap();
            let mut value = serde_json::to_value(&original).unwrap();
            value["window_layout"][0]["tab_count"] = serde_json::json!(0);
            std::fs::write(
                crate::display::nebula_data_dir().join("session.json"),
                serde_json::to_vec(&value).unwrap(),
            )
            .unwrap();
        } else if case == "invalid_json" {
            std::fs::write(crate::display::nebula_data_dir().join("session.json"), b"{invalid")
                .unwrap();
        } else if case == "missing" {
            let path = crate::display::nebula_data_dir().join("session.json");
            if path.exists() {
                std::fs::rename(path, root.path().join("before-missing.json")).unwrap();
            }
        } else {
            crate::session::try_save(&original).unwrap();
        }
        let folder = root.path().join(format!("{case} 当前目录"));
        std::fs::create_dir(&folder).unwrap();
        cx.update(|cx| {
            cx.global_mut::<WindowRegistry>().session_persistence =
                SessionPersistence::ordinary_for_test();
            start_directory(cx, &folder);
            assert_eq!(cx.global::<WindowRegistry>().entries.len(), 1, "{case}");
            let snapshot = combined_session(None, cx).unwrap();
            assert_eq!(snapshot.tabs.len(), 1, "{case} 不额外增加默认目录标签");
            assert_eq!(snapshot.tabs[0].cwd, folder.to_string_lossy(), "{case}");
            remove_windows(cx);
        });
    }
}
