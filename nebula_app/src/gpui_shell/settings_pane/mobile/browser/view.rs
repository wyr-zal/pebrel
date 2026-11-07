//! Browser remote rendering; actions and lifecycle stay in the parent module.

use super::*;
use crate::gpui_shell::widgets::NebulaSwitch;

impl SettingsPane {
    pub(in crate::gpui_shell::settings_pane) fn browser_remote(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.browser_remote_initialize(window, cx);
        let language = crate::gpui_shell::config::ui_language(cx);
        let busy = self.mobile.browser.loading
            || self.mobile.browser.operation
            || self.mobile.browser.project_picker_open;
        let snapshot = self.mobile.browser.snapshot.as_ref();
        let paired = snapshot.is_some_and(|snapshot| snapshot.paired);
        let pairing = snapshot.and_then(|snapshot| snapshot.pairing.as_ref());
        let status = snapshot.map(|snapshot| browser_status(snapshot, language));
        let mut card = v_flex()
            .id("browser-remote")
            .debug_selector(|| "browser-remote".into())
            .w_full()
            .p(px(24.0))
            .gap(px(16.0))
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(10.0))
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap_3()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(18.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(language.text(Message::MobileBrowserTitle)),
                            )
                            .child(super::super::view::description(
                                language.text(Message::MobileBrowserHint),
                                cx,
                            )),
                    )
                    .child(
                        NebulaSwitch::new("browser-remote-enabled")
                            .checked(self.mobile.browser.enabled)
                            .disabled(busy)
                            .on_click(cx.listener(|this, enabled: &bool, window, cx| {
                                this.mobile.browser.enabled = *enabled;
                                this.browser_remote_save(*enabled, window, cx);
                            })),
                    ),
            );
        if let Some(status) = status {
            card = card.child(
                div().text_size(px(12.0)).text_color(cx.theme().muted_foreground).child(status),
            );
        }
        card = card
            .child(browser_field(
                language.text(Message::MobileBrowserRelay),
                Input::new(&self.mobile.browser.relay_input).appearance(false).disabled(busy),
                cx,
            ))
            .child(browser_field(
                language.text(Message::MobileBrowserDeviceName),
                Input::new(&self.mobile.browser.device_name_input).appearance(false).disabled(busy),
                cx,
            ))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Button::new("browser-remote-save")
                            .outline()
                            .label(language.text(Message::MobileBrowserSave))
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                let enabled = this.mobile.browser.enabled;
                                this.browser_remote_save(enabled, window, cx);
                            })),
                    )
                    .when(!paired, |actions| {
                        actions
                            .child(
                                Button::new("browser-remote-pair")
                                    .primary()
                                    .label(language.text(Message::MobileBrowserPair))
                                    .disabled(
                                        busy || pairing.is_some() || !self.mobile.browser.enabled,
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.browser_remote_pair(window, cx)
                                    })),
                            )
                            .when(pairing.is_some(), |actions| {
                                actions.child(
                                    Button::new("browser-remote-cancel-pair")
                                        .ghost()
                                        .label(language.text(Message::MobileBrowserCancelPair))
                                        .disabled(busy)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.browser_remote_cancel_pair(window, cx)
                                        })),
                                )
                            })
                    })
                    .when(paired, |actions| {
                        actions.child(
                            Button::new("browser-remote-revoke")
                                .danger()
                                .label(language.text(if self.mobile.browser.revoke_armed {
                                    Message::MobileBrowserConfirmRevoke
                                } else {
                                    Message::MobileBrowserRevoke
                                }))
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.browser_remote_revoke(window, cx)
                                })),
                        )
                    }),
            );
        if let Some(pairing) = pairing {
            card = card.child(browser_qr_panel(
                self.mobile.browser.qr.clone(),
                pairing.qr_payload.clone(),
                language.format(Message::MobileBrowserPairCode, &[("code", &pairing.code)]),
                language.text(Message::MobileBrowserPairing),
                cx,
            ));
        }
        if let Some(approval) = snapshot.and_then(|snapshot| snapshot.browser_approval.as_ref()) {
            let approve_id = approval.id.clone();
            let reject_id = approval.id.clone();
            card = card.child(
                v_flex()
                    .gap_2()
                    .child(
                        language
                            .format(Message::MobileBrowserApproval, &[("code", &approval.code)]),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("browser-approve")
                                    .primary()
                                    .label(language.text(Message::MobileBrowserApprove))
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.browser_remote_decide(
                                            approve_id.clone(),
                                            true,
                                            window,
                                            cx,
                                        )
                                    })),
                            )
                            .child(
                                Button::new("browser-reject")
                                    .outline()
                                    .label(language.text(Message::MobileBrowserReject))
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.browser_remote_decide(
                                            reject_id.clone(),
                                            false,
                                            window,
                                            cx,
                                        )
                                    })),
                            ),
                    ),
            );
        }
        if let Some(message) = self.mobile.browser.feedback {
            card = card.child(
                div().text_size(px(12.0)).text_color(cx.theme().link).child(language.text(message)),
            );
        }
        if let Some(error) = self.mobile.browser.failure.clone() {
            card = card.child(
                div()
                    .text_size(px(12.0))
                    .text_color(cx.theme().danger)
                    .child(language.format(Message::MobileBrowserError, &[("message", &error)])),
            );
        }
        v_flex()
            .id("browser-remote-layout")
            .debug_selector(|| "browser-remote-layout".into())
            .w_full()
            .mt(px(28.0))
            .gap(px(12.0))
            .child(card)
            .child(self.browser_remote_projects(window, cx))
    }

    fn browser_remote_projects(
        &self,
        _window: &mut Window,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let language = crate::gpui_shell::config::ui_language(cx);
        let busy = self.mobile.browser.operation || self.mobile.browser.project_picker_open;
        let mut projects = v_flex()
            .id("browser-projects")
            .debug_selector(|| "browser-projects".into())
            .w_full()
            .p(px(24.0))
            .gap(px(12.0))
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(10.0))
            .child(
                div()
                    .text_size(px(16.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(language.text(Message::MobileBrowserProjects)),
            )
            .child(super::super::view::description(
                language.text(Message::MobileBrowserLocalOnly),
                cx,
            ));
        let grants = self
            .mobile
            .browser
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.preferences.projects.as_slice())
            .unwrap_or_default();
        if grants.is_empty() {
            projects = projects.child(super::super::view::description(
                language.text(Message::MobileBrowserNoProjects),
                cx,
            ));
        }
        for grant in grants {
            let id = grant.id.clone();
            let detail = grant
                .shell_id
                .as_ref()
                .map_or_else(|| grant.path.clone(), |shell| format!("{} · {shell}", grant.path));
            projects = projects.child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .p(px(10.0))
                    .rounded(px(8.0))
                    .bg(crate::gpui_shell::theme::settings_hover_bg(cx, false))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(grant.name.clone()),
                            )
                            .child(
                                super::super::view::description(detail, cx)
                                    .text_size(px(11.0))
                                    .line_height(px(17.0)),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!("browser-remove-project-{id}")))
                            .ghost()
                            .small()
                            .label(language.text(Message::MobileRemove))
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.browser_remote_remove_project(id.clone(), window, cx)
                            })),
                    ),
            );
        }
        projects = projects.child(
            h_flex()
                .w_full()
                .items_end()
                .gap_2()
                .child(
                    browser_field(
                        language.text(Message::MobileBrowserProjectFolder),
                        browser_project_path(
                            self.mobile.browser.project_path_input.read(cx).value(),
                            cx,
                        ),
                        cx,
                    )
                    .flex_1()
                    .min_w_0(),
                )
                .child(
                    Button::new("browser-pick-project")
                        .outline()
                        .label(language.text(Message::MobileBrowserChooseFolder))
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.browser_remote_pick_project(window, cx)
                        })),
                ),
        );
        projects = projects.child(
            Button::new("browser-project-shell-options")
                .ghost()
                .small()
                .label(language.text(if self.mobile.browser.project_shell_options_open {
                    Message::MobileBrowserHideAdvancedOptions
                } else {
                    Message::MobileBrowserAdvancedOptions
                }))
                .disabled(busy)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.mobile.browser.project_shell_options_open =
                        !this.mobile.browser.project_shell_options_open;
                    cx.notify();
                })),
        );
        if self.mobile.browser.project_shell_options_open {
            projects = projects
                .child(browser_field(
                    language.text(Message::MobileBrowserProjectShell),
                    Input::new(&self.mobile.browser.project_shell_input)
                        .appearance(false)
                        .disabled(busy),
                    cx,
                ))
                .child(
                    super::super::view::description(
                        language.text(Message::MobileBrowserProjectShellHint),
                        cx,
                    )
                    .text_size(px(11.0)),
                );
        }
        projects.child(
            Button::new("browser-add-project")
                .outline()
                .label(language.text(Message::MobileBrowserAddProject))
                .disabled(
                    busy || self
                        .mobile
                        .browser
                        .project_path_input
                        .read(cx)
                        .value()
                        .trim()
                        .is_empty(),
                )
                .on_click(
                    cx.listener(|this, _, window, cx| this.browser_remote_add_project(window, cx)),
                ),
        )
    }
}

fn browser_field(label: &'static str, input: impl IntoElement, cx: &App) -> gpui::Div {
    v_flex()
        .w_full()
        .gap_1()
        .child(div().text_size(px(12.0)).text_color(cx.theme().muted_foreground).child(label))
        .child(input)
}

fn browser_project_path(path: SharedString, cx: &App) -> gpui::Div {
    let language = crate::gpui_shell::config::ui_language(cx);
    let (value, color) = if path.is_empty() {
        (
            SharedString::from(language.text(Message::MobileBrowserProjectFolderPlaceholder)),
            cx.theme().muted_foreground,
        )
    } else {
        (path, cx.theme().foreground)
    };
    div()
        .w_full()
        .h(px(36.0))
        .px(px(10.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().secondary)
        .flex()
        .items_center()
        .child(
            div()
                .w_full()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .text_size(px(13.0))
                .text_color(color)
                .child(value),
        )
}

fn browser_qr_panel(
    qr: Option<Arc<RenderImage>>,
    payload: String,
    code: String,
    caption: &'static str,
    cx: &App,
) -> gpui::Div {
    let mut panel =
        v_flex().w_full().gap_2().pt(px(12.0)).child(super::super::view::description(caption, cx));
    if !code.is_empty() {
        panel = panel
            .child(div().text_size(px(15.0)).font_weight(gpui::FontWeight::SEMIBOLD).child(code));
    }
    panel = panel.child(
        div()
            .size(px(196.0))
            .p(px(12.0))
            .rounded(px(10.0))
            .bg(gpui::rgb(0xffffff))
            .flex()
            .items_center()
            .justify_center()
            .child(match qr {
                Some(qr) => img(qr).size_full().into_any_element(),
                None => div()
                    .text_size(px(12.0))
                    .text_color(gpui::rgb(0x333333))
                    .child(
                        crate::gpui_shell::config::ui_language(cx)
                            .text(Message::MobileQrUnavailable),
                    )
                    .into_any_element(),
            }),
    );
    panel.child(
        div()
            .max_w_full()
            .overflow_hidden()
            .text_ellipsis()
            .text_size(px(10.0))
            .text_color(cx.theme().muted_foreground)
            .child(payload),
    )
}

fn browser_status(snapshot: &BrowserSnapshot, language: crate::display::UiLanguage) -> String {
    let status = match &snapshot.status {
        mobile_web::Status::Stopped => language.text(Message::MobileStopped),
        mobile_web::Status::Pairing => language.text(Message::MobileWaiting),
        mobile_web::Status::Connecting => language.text(Message::MobileReconnecting),
        mobile_web::Status::Connected => language.text(Message::MobileConnected),
        mobile_web::Status::Offline => language.text(Message::MobileOffline),
        mobile_web::Status::AuthFailed => language.text(Message::MobileConnectionFailed),
        mobile_web::Status::Failed(message) => message,
    };
    if snapshot.paired {
        String::from(status)
    } else if snapshot.preferences.enabled {
        format!("{} · {}", language.text(Message::MobileBrowserTitle), status)
    } else {
        language.text(Message::MobileStopped).to_owned()
    }
}
