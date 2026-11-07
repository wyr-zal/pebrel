use super::*;
use crate::mobile_web::{self, Preferences as BrowserPreferences, Snapshot as BrowserSnapshot};
use pebrel_mobile_link::qr::PairingQr;

mod view;

pub(super) struct BrowserRemoteState {
    initialized: bool,
    loading: bool,
    operation: bool,
    enabled: bool,
    snapshot: Option<BrowserSnapshot>,
    relay_input: Entity<InputState>,
    device_name_input: Entity<InputState>,
    project_path_input: Entity<InputState>,
    project_shell_input: Entity<InputState>,
    project_picker_open: bool,
    project_shell_options_open: bool,
    inputs_loaded: bool,
    failure: Option<String>,
    feedback: Option<Message>,
    qr_payload: Option<String>,
    qr: Option<Arc<RenderImage>>,
    revoke_armed: bool,
    sequence: u64,
    monitor: Option<Task<()>>,
}

impl BrowserRemoteState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsPane>) -> Self {
        let language = crate::gpui_shell::config::ui_language(cx);
        Self {
            initialized: false,
            loading: false,
            operation: false,
            enabled: false,
            snapshot: None,
            relay_input: cx
                .new(|cx| InputState::new(window, cx).placeholder("https://relay.example.com")),
            device_name_input: cx.new(|cx| {
                InputState::new(window, cx).placeholder(language.text(Message::MobileTitle))
            }),
            project_path_input: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(language.text(Message::MobileBrowserProjectFolderPlaceholder))
            }),
            project_shell_input: cx.new(|cx| {
                InputState::new(window, cx).placeholder(language.text(Message::MobileAutomatic))
            }),
            project_picker_open: false,
            project_shell_options_open: false,
            inputs_loaded: false,
            failure: None,
            feedback: None,
            qr_payload: None,
            qr: None,
            revoke_armed: false,
            sequence: 0,
            monitor: None,
        }
    }

    fn apply_snapshot(
        &mut self,
        snapshot: BrowserSnapshot,
        window: &mut Window,
        cx: &mut Context<SettingsPane>,
    ) {
        self.enabled = snapshot.preferences.enabled;
        let qr_payload = snapshot.pairing.as_ref().map(|pairing| pairing.qr_payload.clone());
        if self.qr_payload != qr_payload {
            self.qr = qr_payload.as_deref().and_then(qr_image);
            self.qr_payload = qr_payload;
        }
        if !self.inputs_loaded {
            self.set_input(
                self.relay_input.clone(),
                snapshot.preferences.relay_url.clone(),
                window,
                cx,
            );
            self.set_input(
                self.device_name_input.clone(),
                snapshot.preferences.device_name.clone(),
                window,
                cx,
            );
            self.inputs_loaded = true;
        }
        self.snapshot = Some(snapshot);
    }

    fn set_input(
        &self,
        input: Entity<InputState>,
        value: String,
        window: &mut Window,
        cx: &mut Context<SettingsPane>,
    ) {
        input.update(cx, |input, cx| input.set_value(value, window, cx));
    }
}

fn qr_image(payload: &str) -> Option<Arc<RenderImage>> {
    let qr = PairingQr::encode(payload.as_bytes()).ok()?;
    let scale = (512 / qr.width()).clamp(1, 8);
    let side = (qr.width() * scale) as u32;
    let pixels = image::RgbaImage::from_raw(side, side, qr.rgba(scale).ok()?)?;
    Some(Arc::new(RenderImage::new([image::Frame::new(pixels)])))
}

impl SettingsPane {
    fn browser_remote_initialize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mobile.browser.initialized {
            return;
        }
        self.mobile.browser.initialized = true;
        self.mobile.browser.loading = true;
        let load = cx.background_executor().spawn(async { mobile_web::snapshot() });
        cx.spawn_in(window, async move |this, cx| {
            let result = load.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.mobile.browser.loading = false;
                match result {
                    Ok(snapshot) => {
                        this.mobile.browser.apply_snapshot(snapshot, window, cx);
                    },
                    Err(error) => this.mobile.browser.failure = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        self.mobile.browser.monitor = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let should_refresh = this
                    .update(cx, |this, _| {
                        this.active_section == MOBILE_SECTION
                            && !this.mobile.browser.loading
                            && !this.mobile.browser.operation
                    })
                    .unwrap_or(false);
                if !should_refresh {
                    continue;
                }
                let Ok(sequence) = this.update(cx, |this, _| this.mobile.browser.sequence) else {
                    break;
                };
                let result = cx.background_executor().spawn(async { mobile_web::snapshot() }).await;
                if this
                    .update(cx, |this, cx| {
                        if this.mobile.browser.sequence != sequence || this.mobile.browser.operation
                        {
                            return;
                        }
                        if let Ok(snapshot) = result {
                            this.mobile.browser.enabled = snapshot.preferences.enabled;
                            this.mobile.browser.snapshot = Some(snapshot);
                            let payload =
                                this.mobile.browser.snapshot.as_ref().and_then(|snapshot| {
                                    snapshot
                                        .pairing
                                        .as_ref()
                                        .map(|pairing| pairing.qr_payload.clone())
                                });
                            if this.mobile.browser.qr_payload != payload {
                                this.mobile.browser.qr = payload.as_deref().and_then(qr_image);
                                this.mobile.browser.qr_payload = payload;
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn browser_remote_run(
        &mut self,
        feedback: Option<Message>,
        work: impl FnOnce() -> Result<BrowserSnapshot, String> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.mobile.browser.sequence = self.mobile.browser.sequence.wrapping_add(1);
        let sequence = self.mobile.browser.sequence;
        self.mobile.browser.operation = true;
        self.mobile.browser.failure = None;
        self.mobile.browser.feedback = None;
        let task = cx.background_executor().spawn(async move { work() });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.mobile.browser.sequence != sequence {
                    return;
                }
                this.mobile.browser.operation = false;
                match result {
                    Ok(snapshot) => {
                        this.mobile.browser.apply_snapshot(snapshot, window, cx);
                        this.mobile.browser.feedback = feedback;
                    },
                    Err(error) => {
                        this.mobile.browser.enabled = this
                            .mobile
                            .browser
                            .snapshot
                            .as_ref()
                            .is_some_and(|snapshot| snapshot.preferences.enabled);
                        this.mobile.browser.failure = Some(error);
                    },
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn browser_remote_preferences(&self, cx: &App, enabled: bool) -> BrowserPreferences {
        let current = self
            .mobile
            .browser
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.preferences.clone())
            .unwrap_or_default();
        BrowserPreferences {
            enabled,
            relay_url: String::from(self.mobile.browser.relay_input.read(cx).value().trim()),
            device_name: String::from(
                self.mobile.browser.device_name_input.read(cx).value().trim(),
            ),
            projects: current.projects,
        }
    }

    fn browser_remote_save(&mut self, enabled: bool, window: &mut Window, cx: &mut Context<Self>) {
        let preferences = self.browser_remote_preferences(cx, enabled);
        self.browser_remote_run(
            Some(Message::MobileBrowserSaved),
            move || mobile_web::save_preferences(preferences),
            window,
            cx,
        );
    }

    fn browser_remote_pair(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.browser_remote_run(None, mobile_web::start_pairing, window, cx);
    }

    fn browser_remote_decide(
        &mut self,
        id: String,
        approve: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.browser_remote_run(None, move || mobile_web::decide_browser(&id, approve), window, cx);
    }

    fn browser_remote_cancel_pair(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.browser_remote_run(None, mobile_web::cancel_pairing_request, window, cx);
    }

    fn browser_remote_revoke(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.mobile.browser.revoke_armed {
            self.mobile.browser.revoke_armed = true;
            let sequence = self.mobile.browser.sequence;
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(4)).await;
                let _ = this.update(cx, |this, cx| {
                    if this.mobile.browser.sequence == sequence {
                        this.mobile.browser.revoke_armed = false;
                        cx.notify();
                    }
                });
            })
            .detach();
            cx.notify();
            return;
        }
        self.mobile.browser.revoke_armed = false;
        self.browser_remote_run(None, mobile_web::revoke_device, window, cx);
    }

    fn browser_remote_pick_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mobile.browser.operation || self.mobile.browser.project_picker_open {
            return;
        }
        let language = crate::gpui_shell::config::ui_language(cx);
        #[cfg(windows)]
        let picked = super::super::shell_picker::pick_folder_with_wsl_places(
            window,
            language.text(Message::MobileBrowserProjectFolder),
        );
        #[cfg(not(windows))]
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(language.text(Message::MobileBrowserProjectFolder).into()),
        });
        self.mobile.browser.project_picker_open = true;
        self.mobile.browser.failure = None;
        let input = self.mobile.browser.project_path_input.clone();
        cx.spawn_in(window, async move |this, cx| {
            #[cfg(windows)]
            let selected = picked.await.ok().flatten();
            #[cfg(not(windows))]
            let selected = match picked.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                _ => None,
            };
            let selected = if let Some(path) = selected {
                let display = path.to_string_lossy().into_owned();
                cx.background_executor()
                    .spawn(async move { path.is_dir() })
                    .await
                    .then_some(display)
            } else {
                None
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.mobile.browser.project_picker_open = false;
                if let Some(path) = selected {
                    input.update(cx, |input, cx| input.set_value(path, window, cx));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn browser_remote_add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = String::from(self.mobile.browser.project_path_input.read(cx).value().trim());
        let shell = String::from(self.mobile.browser.project_shell_input.read(cx).value().trim());
        if path.is_empty() {
            self.mobile.browser.failure =
                Some(String::from("Choose the exact project path on this computer"));
            cx.notify();
            return;
        }
        self.browser_remote_run(
            None,
            move || mobile_web::add_project(&path, (!shell.is_empty()).then_some(shell)),
            window,
            cx,
        );
    }

    fn browser_remote_remove_project(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.browser_remote_run(None, move || mobile_web::remove_project(&id), window, cx);
    }
}
