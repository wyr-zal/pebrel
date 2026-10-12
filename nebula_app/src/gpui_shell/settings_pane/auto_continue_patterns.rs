use super::*;
use gpui_component::FocusTrapElement as _;

const MAX_PATTERNS: usize = 16;
const MAX_PATTERN_CHARS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PatternValidation {
    TooMany,
    InvalidEntry,
}

fn split_patterns(raw: &str) -> Vec<String> {
    raw.split(';').map(str::trim).filter(|pattern| !pattern.is_empty()).map(str::to_owned).collect()
}

fn serialize_patterns(patterns: &[String]) -> Result<String, PatternValidation> {
    if patterns
        .iter()
        .any(|pattern| pattern.contains('\n') || pattern.contains('\r'))
    {
        return Err(PatternValidation::InvalidEntry);
    }
    let patterns = patterns
        .iter()
        .map(|pattern| pattern.trim())
        .filter(|pattern| !pattern.is_empty())
        .collect::<Vec<_>>();
    if patterns.len() > MAX_PATTERNS {
        return Err(PatternValidation::TooMany);
    }
    if patterns.iter().any(|pattern| {
        pattern.contains(';') || pattern.chars().count() > MAX_PATTERN_CHARS
    }) {
        return Err(PatternValidation::InvalidEntry);
    }
    Ok(patterns.join(";"))
}

pub(super) struct PatternEditor {
    pub(super) rows: Vec<Entity<InputState>>,
    pub(super) error: Option<SharedString>,
    focus: FocusHandle,
}

impl SettingsPane {
    pub(super) fn auto_continue_patterns_row(
        &self,
        language: crate::display::UiLanguage,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.row(
            language.text(crate::i18n::Message::SettingsNotificationsAutoContinuePatterns),
            help("ai_auto_continue_error_patterns", language),
            NebulaButton::new("manage-ai-auto-continue-error-patterns")
                .label(
                    language.text(
                        crate::i18n::Message::SettingsNotificationsAutoContinuePatternsManage,
                    ),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_auto_continue_patterns_editor(window, cx);
                })),
            cx,
        )
    }

    fn open_auto_continue_patterns_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_font_picker(window, false, cx);
        self.close_background_picker(cx);
        let language = crate::gpui_shell::config::ui_language(cx);
        let placeholder = language
            .text(crate::i18n::Message::SettingsNotificationsAutoContinuePatternsPlaceholder);
        let rows = split_patterns(&self.runtime.ai_auto_continue_error_patterns)
            .into_iter()
            .map(|pattern| {
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(placeholder)
                        .default_value(pattern.to_owned())
                })
            })
            .collect();
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.auto_continue_patterns_editor = Some(PatternEditor { rows, error: None, focus });
        cx.notify();
    }

    fn add_auto_continue_pattern(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let language = crate::gpui_shell::config::ui_language(cx);
        let placeholder = language
            .text(crate::i18n::Message::SettingsNotificationsAutoContinuePatternsPlaceholder);
        let Some(editor) = self.auto_continue_patterns_editor.as_mut() else { return };
        if editor.rows.len() >= MAX_PATTERNS {
            editor.error = Some(
                language
                    .text(crate::i18n::Message::SettingsNotificationsAutoContinuePatternsLimit)
                    .into(),
            );
            cx.notify();
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        editor.rows.push(input.clone());
        editor.error = None;
        let focus = input.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn remove_auto_continue_pattern(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(editor) = self.auto_continue_patterns_editor.as_mut() else { return };
        if index < editor.rows.len() {
            editor.rows.remove(index);
            editor.error = None;
            cx.notify();
        }
    }

    fn close_auto_continue_patterns_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.auto_continue_patterns_editor.take().is_some() {
            window.focus(&self.focus_handle, cx);
            cx.notify();
        }
    }

    fn save_auto_continue_patterns_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.auto_continue_patterns_editor.as_ref() else { return };
        let patterns =
            editor.rows.iter().map(|input| input.read(cx).value().to_string()).collect::<Vec<_>>();
        let language = crate::gpui_shell::config::ui_language(cx);
        let value = match serialize_patterns(&patterns) {
            Ok(value) => value,
            Err(PatternValidation::TooMany) => {
                if let Some(editor) = self.auto_continue_patterns_editor.as_mut() {
                    editor.error = Some(language.text(
                        crate::i18n::Message::SettingsNotificationsAutoContinuePatternsLimit,
                    ).into());
                }
                cx.notify();
                return;
            },
            Err(PatternValidation::InvalidEntry) => {
                if let Some(editor) = self.auto_continue_patterns_editor.as_mut() {
                    editor.error = Some(language.text(
                        crate::i18n::Message::SettingsNotificationsAutoContinuePatternsValidation,
                    ).into());
                }
                cx.notify();
                return;
            },
        };
        match self.try_persist(&[("ai_auto_continue_error_patterns", value)], cx) {
            Ok(()) => self.close_auto_continue_patterns_editor(window, cx),
            Err(error) => {
                let error = error.to_string();
                let message = language.format(
                    crate::i18n::Message::SettingsNotificationsSaveFailed,
                    &[("error", &error)],
                );
                if let Some(editor) = self.auto_continue_patterns_editor.as_mut() {
                    editor.error = Some(message.into());
                }
                cx.notify();
            },
        }
    }

    pub(super) fn auto_continue_patterns_modal(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let editor = self.auto_continue_patterns_editor.as_ref()?;
        let rows = editor.rows.clone();
        let error = editor.error.clone();
        let focus = editor.focus.clone();
        let language = crate::gpui_shell::config::ui_language(cx);
        let colors = appearance_picker::AppearanceColors::current(cx);
        let viewport = window.viewport_size();
        let compact = f32::from(viewport.width) <= 720.0;
        let padding = if compact { 18.0 } else { 26.0 };
        let width =
            (f32::from(viewport.width) - if compact { 24.0 } else { 40.0 }).min(620.0).max(1.0);
        let height = (f32::from(viewport.height) - 48.0).max(1.0);
        let body_min_height = (height - 180.0).clamp(1.0, 120.0);
        let title = language
            .text(crate::i18n::Message::SettingsNotificationsAutoContinuePatternsDialogTitle);
        let description = language
            .text(crate::i18n::Message::SettingsNotificationsAutoContinuePatternsDialogDescription);
        let count = rows.len().to_string();
        let count = language.format(
            crate::i18n::Message::SettingsNotificationsAutoContinuePatternsCount,
            &[("count", &count), ("max", "16")],
        );
        let pane = cx.entity();
        let row_items = rows
            .iter()
            .enumerate()
            .map(|(index, input)| {
                let number = (index + 1).to_string();
                let input_label = language.format(
                    crate::i18n::Message::SettingsNotificationsAutoContinuePatternsEntryLabel,
                    &[("number", &number)],
                );
                h_flex()
                    .id(("auto-continue-pattern-row", index))
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .debug_selector(move || format!("auto-continue-pattern-input-{index}"))
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(input).aria_label(input_label)),
                    )
                    .child(
                        div()
                            .debug_selector(move || {
                                format!("auto-continue-pattern-remove-{index}")
                            })
                            .flex_shrink_0()
                            .child(
                                NebulaButton::new(format!("remove-auto-continue-pattern-{index}"))
                                    .label(language.text(
                                        crate::i18n::Message::SettingsNotificationsAutoContinuePatternsRemove,
                                    ))
                                    .on_click({
                                        let pane = pane.clone();
                                        move |_, _, cx| {
                                            pane.update(cx, |this, cx| {
                                                this.remove_auto_continue_pattern(index, cx);
                                            });
                                        }
                                    }),
                            ),
                    )
            })
            .collect::<Vec<_>>();
        let dialog = v_flex()
            .id("auto-continue-patterns-dialog")
            .debug_selector(|| "auto-continue-patterns-dialog".to_owned())
            .role(gpui::accesskit::Role::Dialog)
            .aria_label(title)
            .w(px(width))
            .max_h(px(height))
            .flex_shrink_0()
            .rounded(px(14.0))
            .border_1()
            .border_color(colors.control)
            .bg(colors.surface)
            .text_color(colors.ink)
            .shadow_2xl()
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                v_flex()
                    .px(px(padding))
                    .pt(px(24.0))
                    .pb(px(18.0))
                    .gap(px(7.0))
                    .flex_shrink_0()
                    .child(div().text_size(px(19.0)).font_semibold().child(title))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(colors.secondary)
                            .child(description),
                    ),
            )
            .child(
                div()
                    .id("auto-continue-patterns-scroll")
                    .min_h(px(body_min_height))
                    .flex_1()
                    .overflow_y_scrollbar()
                    .px(px(padding))
                    .pb(px(12.0))
                    .child(
                        v_flex()
                            .w_full()
                            .gap(px(8.0))
                            .when(rows.is_empty(), |body| {
                                body.child(
                                    div()
                                        .debug_selector(|| "auto-continue-patterns-empty".to_owned())
                                        .py(px(12.0))
                                        .text_color(colors.secondary)
                                        .child(language.text(
                                            crate::i18n::Message::SettingsNotificationsAutoContinuePatternsEmpty,
                                        )),
                                )
                            })
                            .children(row_items),
                    ),
            )
            .when_some(error, |dialog, error| {
                dialog.child(
                    div()
                        .debug_selector(|| "auto-continue-patterns-error".to_owned())
                        .px(px(padding))
                        .pb_2()
                        .text_size(px(11.0))
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .child(
                h_flex()
                    .px(px(padding))
                    .py(px(15.0))
                    .gap(px(8.0))
                    .justify_between()
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(colors.line)
                    .child(
                        h_flex()
                            .min_w_0()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                Button::new("add-auto-continue-pattern")
                                    .debug_selector(|| "add-auto-continue-pattern".to_owned())
                                    .icon(IconName::Plus)
                                    .label(language.text(
                                        crate::i18n::Message::SettingsNotificationsAutoContinuePatternsAdd,
                                    ))
                                    .ghost()
                                    .h(px(33.0))
                                    .disabled(rows.len() >= MAX_PATTERNS)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.add_auto_continue_pattern(window, cx);
                                    })),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(colors.secondary)
                                    .child(count),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(px(8.0))
                            .flex_shrink_0()
                            .child(
                                Button::new("cancel-auto-continue-patterns")
                                    .debug_selector(|| "cancel-auto-continue-patterns".to_owned())
                                    .label(language.text(
                                        crate::i18n::Message::SettingsNotificationsAutoContinuePatternsCancel,
                                    ))
                                    .h(px(33.0))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.close_auto_continue_patterns_editor(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("save-auto-continue-patterns")
                                    .debug_selector(|| "save-auto-continue-patterns".to_owned())
                                    .label(language.text(
                                        crate::i18n::Message::SettingsNotificationsAutoContinuePatternsSave,
                                    ))
                                    .h(px(33.0))
                                    .bg(colors.primary)
                                    .border_color(colors.primary)
                                    .text_color(colors.on_primary)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save_auto_continue_patterns_editor(window, cx);
                                    })),
                            ),
                    ),
            )
            .focus_trap("auto-continue-patterns-focus-trap", &focus);
        Some(
            deferred(
                anchored()
                    .anchor(gpui::Anchor::TopLeft)
                    .position(gpui::point(px(0.0), px(0.0)))
                    .child(
                        div()
                            .id("auto-continue-patterns-overlay")
                            .w(viewport.width)
                            .h(viewport.height)
                            .flex()
                            .items_center()
                            .justify_center()
                            .occlude()
                            .bg(colors.scrim)
                            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                                if event.keystroke.key.eq_ignore_ascii_case("escape") {
                                    cx.stop_propagation();
                                    this.close_auto_continue_patterns_editor(window, cx);
                                }
                            }))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                            .child(dialog),
                    ),
            )
            .with_priority(4)
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{PatternValidation, serialize_patterns, split_patterns};

    #[test]
    fn legacy_delimited_values_round_trip_as_individual_entries() {
        let patterns = split_patterns(" FIRST ; SECOND  ");
        assert_eq!(patterns, ["FIRST", "SECOND"]);
        assert_eq!(serialize_patterns(&patterns), Ok("FIRST;SECOND".to_owned()));
        assert_eq!(serialize_patterns(&[]), Ok(String::new()));
    }

    #[test]
    fn pattern_editor_enforces_existing_limits_and_delimiter_contract() {
        let too_many = vec!["phrase".to_owned(); 17];
        assert_eq!(serialize_patterns(&too_many), Err(PatternValidation::TooMany));
        assert_eq!(
            serialize_patterns(&["phrase;other".to_owned()]),
            Err(PatternValidation::InvalidEntry)
        );
        assert_eq!(
            serialize_patterns(&["phrase\nother".to_owned()]),
            Err(PatternValidation::InvalidEntry)
        );
        for phrase in ["\nphrase", "phrase\n", "\rphrase", "phrase\r"] {
            assert_eq!(
                serialize_patterns(&[phrase.to_owned()]),
                Err(PatternValidation::InvalidEntry),
                "line breaks must not be silently trimmed"
            );
        }
        assert_eq!(serialize_patterns(&["a".repeat(257)]), Err(PatternValidation::InvalidEntry));
        assert_eq!(serialize_patterns(&["  phrase  ".to_owned()]), Ok("phrase".to_owned()));
    }
}
