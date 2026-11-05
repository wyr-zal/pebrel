use super::*;
use crate::config::ui_config::Profile;
use crate::gpui_shell::config::Settings;
use crate::gpui_shell::workspace::{shell_palette_rows, windowing};
use crate::shell_detect::DetectedShell;
use gpui::{AppContext as _, TestAppContext};
use gpui_component::Root;

fn check_picker(cx: &mut TestAppContext, profile: bool, explicit_directory: bool) {
    let _settings_lock = crate::gpui_shell::settings_fixture::lock_theme_studio();
    let directory = tempfile::tempdir().unwrap();
    let plain = directory.path().join("project");
    let spaced = directory.path().join("project with spaces");
    let own = directory.path().join("profile startup");
    for path in [&plain, &spaced, &own] {
        std::fs::create_dir(path).unwrap();
    }
    let guest_path = |path: &std::path::Path| {
        let native = path.to_string_lossy().replace('\\', "/");
        format!("/mnt/{}/{}", native[..1].to_ascii_lowercase(), &native[3..])
    };
    let shell = DetectedShell {
        id: "pwsh".into(),
        name: "PowerShell 7".into(),
        // Exercise the real launch preparation, but never start a user's shell.
        program: directory.path().join("missing/pwsh.exe").to_string_lossy().into_owned(),
        args: vec!["-NoLogo".into()],
    };
    let target_profile = Profile {
        name: "PowerShell profile".into(),
        command: shell.program.clone(),
        args: shell.args.clone(),
        cwd: explicit_directory.then(|| own.clone()),
        shell_id: Some("pwsh".into()),
        terminal_profile_id: Some("picker-fixture".into()),
    };
    let configured_shell = Some("wsl:PickerFixture".to_owned());
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::gpui_shell::math_view::register(cx);
        crate::gpui_shell::file_editor::init(cx);
        crate::gpui_shell::workspace::init(cx);
        windowing::initialize(cx, crate::runtime_api::RuntimeHub::new());
        cx.set_reduce_motion(true);
        let mut settings = Settings::load(nebula_settings::ThemeName::Nord);
        settings.shell_id = configured_shell.clone();
        cx.set_global(settings);
    });
    let (_, window) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| {
            let mut workspace = NebulaWorkspace::new(
                window,
                None,
                None,
                1,
                crate::runtime_api::RuntimeHub::new(),
                windowing::WorkspaceStartup::Empty,
                windowing::WindowRole::Regular,
                cx,
            );
            workspace.add_terminal_with(
                LaunchSession::Shell {
                    name: "source fixture".into(),
                    program: shell.program.clone(),
                    args: vec![],
                },
                Some(plain.clone()),
                None,
                window,
                cx,
            );
            let source = workspace.tabs[workspace.active].focused_view().unwrap().clone();
            let cases = [
                (plain.to_string_lossy().into_owned(), Some(plain.clone())),
                (guest_path(&plain), Some(plain.clone())),
                (guest_path(&spaced), Some(spaced.clone())),
                ("/home/picker-fixture".into(), None),
                ("/".into(), None),
            ];
            for (reported, inherited) in cases {
                source.update(cx, |view, _| {
                    view.cwd = reported.clone();
                    let options = nebula_terminal::tty::Options {
                        shell: Some(nebula_terminal::tty::Shell::new(
                            "wsl.exe".into(),
                            vec!["-d".into(), "PickerFixture".into()],
                        )),
                        ..Default::default()
                    };
                    view.exec_context =
                        Some(crate::runtime_exec::PaneExecContext::from_pty_options(&options));
                });
                workspace.active = workspace
                    .tabs
                    .iter()
                    .position(|tab| {
                        tab.focused_view()
                            .is_some_and(|view| view.entity_id() == source.entity_id())
                    })
                    .unwrap();
                let before = workspace.tabs.len();
                workspace.palette_override = Some(shell_palette_rows(
                    if profile { vec![] } else { vec![shell.clone()] },
                    if profile { vec![target_profile.clone()] } else { vec![] },
                    vec![],
                    configured_shell.as_deref().unwrap(),
                    crate::display::UiLanguage::EnUs,
                    1.0,
                ));
                workspace.command_palette_open = true;
                workspace.shell_picker_open = true;
                workspace.command_palette_selected = 0;
                workspace.run_selected_palette_action(window, cx);
                assert_eq!(workspace.tabs.len(), before + 1);
                let selected = workspace.tabs[workspace.active].focused_view().unwrap().read(cx);
                let expected = target_profile
                    .cwd
                    .clone()
                    .or_else(NebulaWorkspace::startup_directory)
                    .or(inherited)
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default();
                assert_eq!(selected.cwd, expected, "picker cwd for {reported:?}");
                assert_eq!(
                    selected.exec_context.as_ref().unwrap().shell_program(),
                    Some(shell.program.as_str())
                );
                assert_eq!(cx.global::<Settings>().shell_id, configured_shell);
                assert!(!workspace.command_palette_open);
                assert_eq!(source.read(cx).cwd, reported);
            }
            workspace
        });
        Root::new(workspace, window, cx)
    });
    window.update(|window, cx| window.draw(cx).clear(cx));
}

#[gpui::test]
fn picker_wsl_profile_keeps_its_explicit_cd(cx: &mut TestAppContext) {
    let _settings_lock = crate::gpui_shell::settings_fixture::lock_theme_studio();
    let configured_shell = Some("wsl:PickerFixture".to_owned());
    let own_guest = Profile {
        name: "WSL own directory".into(),
        command: "wsl.exe".into(),
        args: vec!["-d".into(), "PickerFixture".into(), "--cd".into(), "/workspace".into()],
        cwd: None,
        shell_id: Some("wsl".into()),
        terminal_profile_id: Some("picker-own-cd".into()),
    };
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::gpui_shell::math_view::register(cx);
        crate::gpui_shell::file_editor::init(cx);
        crate::gpui_shell::workspace::init(cx);
        windowing::initialize(cx, crate::runtime_api::RuntimeHub::new());
        cx.set_reduce_motion(true);
        let mut settings = Settings::load(nebula_settings::ThemeName::Nord);
        settings.shell_id = configured_shell.clone();
        cx.set_global(settings);
    });
    let (_, window) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| {
            let mut workspace = NebulaWorkspace::new(
                window,
                None,
                None,
                1,
                crate::runtime_api::RuntimeHub::new(),
                windowing::WorkspaceStartup::Empty,
                windowing::WindowRole::Regular,
                cx,
            );
            workspace.add_terminal_with(
                LaunchSession::Shell {
                    name: "source fixture".into(),
                    program: "pebrel-test-missing-shell".into(),
                    args: vec![],
                },
                None,
                None,
                window,
                cx,
            );
            let source = workspace.tabs[workspace.active].focused_view().unwrap().clone();
            source.update(cx, |view, _| {
                view.cwd = "/home/picker-origin".into();
                let options = nebula_terminal::tty::Options {
                    shell: Some(nebula_terminal::tty::Shell::new(
                        "wsl.exe".into(),
                        vec!["-d".into(), "PickerFixture".into()],
                    )),
                    ..Default::default()
                };
                view.exec_context =
                    Some(crate::runtime_exec::PaneExecContext::from_pty_options(&options));
            });
            workspace.palette_override = Some(shell_palette_rows(
                vec![],
                vec![own_guest.clone()],
                vec![],
                configured_shell.as_deref().unwrap(),
                crate::display::UiLanguage::EnUs,
                1.0,
            ));
            workspace.command_palette_open = true;
            workspace.shell_picker_open = true;
            workspace.command_palette_selected = 0;
            workspace.run_selected_palette_action(window, cx);
            let selected = workspace.tabs[workspace.active].focused_view().unwrap().read(cx);
            let LaunchSession::Shell { program, args, .. } = &selected.session_launch else {
                panic!("picker must keep the WSL profile's actual shell identity");
            };
            assert_eq!(program, &own_guest.command);
            assert_eq!(args, &own_guest.args, "the profile's --cd must survive the picker");
            assert_ne!(
                selected.cwd, "/home/picker-origin",
                "source guest cwd must not reach a target with its own --cd"
            );
            workspace
        });
        Root::new(workspace, window, cx)
    });
    window.update(|window, cx| window.draw(cx).clear(cx));
}

#[gpui::test]
fn picker_shell_inherits_wsl_host_directories(cx: &mut TestAppContext) {
    check_picker(cx, false, false);
}

#[gpui::test]
fn picker_profile_inherits_wsl_host_directories(cx: &mut TestAppContext) {
    check_picker(cx, true, false);
}

#[gpui::test]
fn picker_profile_keeps_its_explicit_directory(cx: &mut TestAppContext) {
    check_picker(cx, true, true);
}
