use super::*;
use crate::i18n::Message;

pub(super) fn open_directory_startup(
    cx: &mut App,
    ai_events: std::sync::mpsc::Receiver<crate::ai_hook::AiHookEvent>,
    shell_events: std::sync::mpsc::Receiver<GpuiShellEvent>,
    cwd: PathBuf,
) -> gpui::Result<()> {
    let settings = nebula_settings::RuntimeSettings::load();
    let mut skipped = None;
    let previous = settings.restore_session.then(crate::session::load).flatten();
    let prepared = match previous {
        Some(session) if crate::session::should_restore(&session) => {
            match session.clone().into_update_windows() {
                Ok(windows) => Some((session, windows)),
                Err(error) => {
                    log::warn!("Could not read saved window boundaries: {error}");
                    skipped = quarantine_or_preserve(session, cx);
                    None
                },
            }
        },
        Some(session) if !session.tabs.is_empty() => {
            skipped = quarantine_or_preserve(session, cx);
            None
        },
        _ => None,
    };
    let Some((mut session, windows)) = prepared else {
        let (id, _) = open_workspace_window(
            cx,
            WorkspaceStartup::NewTerminal { cwd: Some(cwd) },
            Some(ai_events),
            Some(shell_events),
            true,
            WindowRole::Regular,
        )?;
        if let Some(path) = skipped {
            let entry = entry_by_id(id, cx).expect("new directory window is registered");
            entry.handle.update(cx, |_, window, cx| {
                let language = crate::gpui_shell::config::ui_language(cx);
                crate::gpui_shell::toast::banner(
                    window,
                    cx,
                    crate::display::ToastKind::Warning,
                    language.format(
                        Message::WorkspaceRecoverySkipped,
                        &[("path", &path.display().to_string())],
                    ),
                );
            })?;
        }
        return Ok(());
    };

    let crashed = crate::session::was_crash(&session);
    crate::session::mark_boot_attempt(&mut session);
    cx.global_mut::<WindowRegistry>().session_persistence.begin_directory_restore(session);
    let mut ai_events = Some(ai_events);
    let mut shell_events = Some(shell_events);
    let mut unrestored = Vec::new();
    let mut restored_tabs = 0;
    for session in windows {
        if session.tabs.is_empty() {
            continue;
        }
        let count = session.tabs.len();
        let ai_events = ai_events.take();
        let shell_events = shell_events.take();
        let opened = (|| {
            #[cfg(all(test, feature = "gpui-test-support"))]
            super::directory_restore_tests::before_window_open(&session)?;
            open_workspace_window(
                cx,
                WorkspaceStartup::RestoreSnapshot {
                    session: session.clone(),
                    resume_ai: settings.resume_ai,
                },
                ai_events,
                shell_events,
                true,
                WindowRole::Regular,
            )
        })();
        match opened {
            Ok(_) => restored_tabs += count,
            Err(error) => {
                log::warn!("Could not reopen a saved window: {error}");
                if cx.global::<WindowRegistry>().entries.is_empty() {
                    // 首窗未建立时结束启动；所有退出保存仍受完整旧快照保护。
                    return Err(error);
                }
                unrestored.push(session);
            },
        }
    }
    if let Some(entry) = entries_by_mru(cx).into_iter().next() {
        let _ = entry.handle.update(cx, |_, window, cx| {
            let _ = entry.workspace.update(cx, |workspace, cx| {
                workspace.report_restored_session(restored_tabs, crashed, window, cx);
            });
        });
    }
    // 只等待窗口/标签及恢复目标建成，不等待模型连接或 Hook 确认。
    let directory_result = append_directory(cwd, settings.windowing_behavior, cx);
    cx.global_mut::<WindowRegistry>().session_persistence.finish_directory_restore(unrestored);
    let saved = save_combined_session(cx, false);
    directory_result?;
    saved?;
    Ok(())
}

fn quarantine_or_preserve(session: crate::session::Session, cx: &mut App) -> Option<PathBuf> {
    let path = crate::session::quarantine();
    if path.is_none() {
        // 隔离副本未落盘时，宁可保留旧现场，也不能让新目录自动保存覆盖它。
        log::warn!("Could not quarantine the old workspace; preserving its saved snapshot");
        cx.global_mut::<WindowRegistry>().session_persistence.begin_directory_restore(session);
    }
    path
}

fn append_directory(
    cwd: PathBuf,
    behavior: nebula_settings::WindowingBehaviorName,
    cx: &mut App,
) -> gpui::Result<()> {
    if behavior != nebula_settings::WindowingBehaviorName::UseNew
        && let Some(entry) = select_mru_window(behavior, cx)
        && let Some(workspace) = entry.workspace.upgrade()
    {
        entry.handle.update(cx, |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.add_terminal_at(Some(cwd), None, window, cx);
            });
        })?;
        focus_entry(&entry, None, cx);
        return Ok(());
    }
    let (id, _) = open_new_window(cx, Some(cwd))?;
    if let Some(entry) = entry_by_id(id, cx) {
        focus_entry(&entry, None, cx);
    }
    Ok(())
}
