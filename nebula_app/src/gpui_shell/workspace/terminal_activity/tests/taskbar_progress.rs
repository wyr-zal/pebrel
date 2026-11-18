use super::*;
use crate::taskbar::TaskProgress;

fn progress(terminal: &Entity<TerminalView>, state: u8, window: &mut VisualTestContext) {
    terminal.update(window, |view, cx| {
        view.remote_files_test_event(Event::Progress { state, value: None }, cx);
    });
    window.run_until_parked();
}

fn change_workspace(
    workspace: &Entity<NebulaWorkspace>,
    window: &mut VisualTestContext,
    action: impl FnOnce(&mut NebulaWorkspace, &mut Window, &mut Context<NebulaWorkspace>),
) {
    window.update(|window, cx| {
        workspace.update(cx, |workspace, cx| action(workspace, window, cx));
    });
}

#[gpui::test]
fn switching_tabs_clears_old_progress_and_restores_current_progress(cx: &mut TestAppContext) {
    let (workspace, terminals, _, mut window) = open_painted_workspace(&[1, 1], cx);
    progress(&terminals[0], 3, &mut window);
    workspace.read_with(&window, |workspace, _| {
        assert_eq!(workspace.taskbar_progress.get(), TaskProgress::Indeterminate);
    });
    change_workspace(&workspace, &mut window, |workspace, window, cx| {
        workspace.activate_tab(1, window, cx);
        assert_eq!(workspace.taskbar_progress.get(), TaskProgress::None);
    });
    progress(&terminals[0], 0, &mut window);
    change_workspace(&workspace, &mut window, |workspace, window, cx| {
        workspace.activate_tab(0, window, cx);
        assert_eq!(workspace.taskbar_progress.get(), TaskProgress::None);
    });
    progress(&terminals[1], 3, &mut window);
    workspace.read_with(&window, |workspace, _| {
        assert_eq!(workspace.taskbar_progress.get(), TaskProgress::None);
    });
    change_workspace(&workspace, &mut window, |workspace, window, cx| {
        workspace.activate_tab(1, window, cx);
        assert_eq!(workspace.taskbar_progress.get(), TaskProgress::Indeterminate);
    });
}

#[gpui::test]
fn switching_and_closing_panes_uses_the_surviving_panes_progress(cx: &mut TestAppContext) {
    let (workspace, terminals, _, mut window) = open_painted_workspace(&[2], cx);
    progress(&terminals[0], 3, &mut window);
    let first = terminals[0].read_with(&window, |view, _| view.pane_id);
    let second = terminals[1].read_with(&window, |view, _| view.pane_id);
    change_workspace(&workspace, &mut window, |workspace, window, cx| {
        workspace.focus_pane(0, second, window, cx);
        assert_eq!(workspace.taskbar_progress.get(), TaskProgress::None);
        workspace.focus_pane(0, first, window, cx);
        assert_eq!(workspace.taskbar_progress.get(), TaskProgress::Indeterminate);
        workspace.close_pane(0, first, window, cx);
        assert_eq!(workspace.taskbar_progress.get(), TaskProgress::None);
    });
}

#[gpui::test]
fn settings_clear_progress_until_the_terminal_is_visible_again(cx: &mut TestAppContext) {
    let (workspace, terminals, _, mut window) = open_painted_workspace(&[1], cx);
    progress(&terminals[0], 3, &mut window);
    change_workspace(&workspace, &mut window, |workspace, window, cx| {
        workspace.open_settings(window, cx);
        assert_eq!(workspace.taskbar_progress.get(), TaskProgress::None);
    });
    progress(&terminals[0], 1, &mut window);
    change_workspace(&workspace, &mut window, |workspace, window, cx| {
        assert_eq!(workspace.taskbar_progress.get(), TaskProgress::None);
        workspace.close_settings(window, cx);
        assert_eq!(workspace.taskbar_progress.get(), TaskProgress::Value(0));
    });
}

#[gpui::test]
fn closing_the_active_tab_clears_its_progress(cx: &mut TestAppContext) {
    let (workspace, terminals, _, mut window) = open_painted_workspace(&[1, 1], cx);
    progress(&terminals[0], 3, &mut window);
    change_workspace(&workspace, &mut window, |workspace, window, cx| {
        workspace.close_tab(0, window, cx);
        assert_eq!(workspace.tabs.len(), 1);
        assert_eq!(workspace.taskbar_progress.get(), TaskProgress::None);
    });
}

#[gpui::test]
fn file_tabs_clear_progress_without_changing_the_terminal(cx: &mut TestAppContext) {
    let (workspace, terminals, _, mut window) = open_painted_workspace(&[1], cx);
    progress(&terminals[0], 3, &mut window);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fixture.txt");
    std::fs::write(&path, "fixture\n").unwrap();
    change_workspace(&workspace, &mut window, |workspace, window, cx| {
        let view = cx.new(|cx| crate::gpui_shell::code_tab::CodeTabView::new(path, window, cx));
        let subscription = cx.subscribe(&view, |_, _, _, _| {});
        workspace.insert_tab_at(
            1,
            WorkspaceTab::Code { view, _subscription: subscription },
            TabMeta::default(),
        );
        workspace.activate_tab(1, window, cx);
        assert_eq!(workspace.taskbar_progress.get(), TaskProgress::None);
        workspace.activate_tab(0, window, cx);
        assert_eq!(workspace.taskbar_progress.get(), TaskProgress::Indeterminate);
    });
}

#[gpui::test]
fn failed_transport_clears_unfinished_legacy_progress(cx: &mut TestAppContext) {
    let (workspace, terminals, _, mut window) = open_painted_workspace(&[1], cx);
    progress(&terminals[0], 3, &mut window);
    terminals[0].update(&mut window, |view, cx| {
        view.remote_files_test_event(Event::PtyFailure("fixture failure".into()), cx);
        view.remote_files_test_event(Event::Exit, cx);
    });
    window.run_until_parked();
    workspace.read_with(&window, |workspace, _| {
        assert_eq!(workspace.tabs.len(), 1);
        assert_eq!(workspace.taskbar_progress.get(), TaskProgress::None);
    });
}
