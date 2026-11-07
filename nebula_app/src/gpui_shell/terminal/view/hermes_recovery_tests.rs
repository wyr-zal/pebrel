use super::startup_tests::{feed, open};
use super::*;
use crate::session::AgentSession;
use gpui::TestAppContext;

const ID: &str = "20261107_091523_a1b2c3";

#[gpui::test]
fn hermes_saved_identity_queues_a_cold_resume_without_publishing_live_identity(
    cx: &mut TestAppContext,
) {
    let (entity, window, receiver) = open(cx);
    entity.update(window, |view, cx| {
        let saved: AgentSession = serde_json::from_value(serde_json::json!({
            "source": "hermes", "session_id": ID,
        }))
        .unwrap();
        view.restore_agent(saved.clone(), cx);
        assert!(!view.can_retry_recovery(), "a supported exact resume must not fail before submission");
        assert!(view.pending_shell_command.is_some());
        assert_eq!(view.session_agent(), Some(saved));
        assert!(view.ai_session.is_none(), "queued input is not a live acknowledgement");
        assert!(view.recovery_pending());
        feed(view, b"\x1b]133;A\x07fixture$ ");
        view.flush_pending_shell_command(cx);
        assert!(receiver.try_iter().any(|message| {
            matches!(message, Msg::Input(bytes) if bytes.as_ref() == format!("hermes --resume {ID}").as_bytes())
        }));
    });
}

#[gpui::test]
fn hermes_legacy_record_without_identity_preserves_the_original_retry_target(
    cx: &mut TestAppContext,
) {
    let (entity, window, receiver) = open(cx);
    entity.update(window, |view, cx| {
        let saved: AgentSession = serde_json::from_str(r#"{"source":"hermes"}"#).unwrap();
        view.restore_agent(saved.clone(), cx);
        assert!(!view.can_retry_recovery(), "a missing ID cannot be repaired by replaying a retry");
        view.retry_recovery(cx);
        assert!(view.pending_shell_command.is_none());
        assert!(receiver.try_iter().all(|message| !matches!(message, Msg::Input(_))));
        assert!(view.ai_session.is_none());
        assert_eq!(view.session_agent(), Some(saved));
        assert!(view.recovery_pending());
    });
}

#[gpui::test]
fn hermes_unsafe_saved_identity_is_preserved_without_offering_a_broken_retry(
    cx: &mut TestAppContext,
) {
    let (entity, window, receiver) = open(cx);
    entity.update(window, |view, cx| {
        let saved = AgentSession {
            source: "hermes".into(),
            session_id: Some("latest".into()),
            session_file: None,
            wsl_launch: None,
        };
        view.restore_agent(saved.clone(), cx);
        assert!(!view.can_retry_recovery());
        view.retry_recovery(cx);
        assert!(view.pending_shell_command.is_none());
        assert!(receiver.try_iter().all(|message| !matches!(message, Msg::Input(_))));
        assert_eq!(view.session_agent(), Some(saved));
    });
}

#[gpui::test]
fn hermes_failed_exact_resume_remains_retryable(cx: &mut TestAppContext) {
    let (entity, window, receiver) = open(cx);
    entity.update(window, |view, cx| {
        let saved = AgentSession {
            source: "hermes".into(),
            session_id: Some(ID.into()),
            session_file: None,
            wsl_launch: None,
        };
        view.restore_agent(saved.clone(), cx);
        feed(view, b"\x1b]133;A\x07fixture$ ");
        view.flush_pending_shell_command(cx);
        assert!(receiver.try_iter().any(|message| matches!(message, Msg::Input(_))));
        view.finish_foreground_command(Some(1), cx);
        assert!(view.can_retry_recovery());
        assert_eq!(view.session_agent(), Some(saved));
    });
}

#[gpui::test]
fn hermes_early_close_waits_for_a_native_id_before_saving(cx: &mut TestAppContext) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        view.running_program = Some("hermes".into());
        assert!(view.agent_identity_receipt.is_none());
        view.prepare_ai_session_save(cx);
        assert!(
            view.ai_session_save_pending(),
            "closing before the native receipt must not silently save only the Hermes source"
        );
    });
}

#[gpui::test]
fn hermes_confirmed_id_remains_saveable_without_a_new_process_receipt(cx: &mut TestAppContext) {
    let (entity, window, _) = open(cx);
    entity.update(window, |view, cx| {
        view.running_program = Some("hermes".into());
        let saved = AgentSession {
            source: "hermes".into(),
            session_id: Some(ID.into()),
            session_file: None,
            wsl_launch: None,
        };
        view.apply_probed_session_identity(saved.clone(), None, false, cx);
        assert!(view.agent_identity_receipt.is_none());
        view.prepare_ai_session_save(cx);
        assert!(!view.ai_session_save_pending());
        assert_eq!(view.session_agent(), Some(saved));
    });
}

#[gpui::test]
fn hermes_missing_or_unsafe_id_emits_native_history_guidance_without_retry(
    cx: &mut TestAppContext,
) {
    use crate::i18n::{Message, UiLanguage};
    use std::cell::RefCell;
    use std::rc::Rc;

    for language in [UiLanguage::ZhCn, UiLanguage::EnUs] {
        for source in ["hermes", "hermes-agent"] {
            for id in [None, Some(""), Some("latest"), Some("a title"), Some("id;exit")] {
                let (entity, window, receiver) = open(cx);
                let notes = Rc::new(RefCell::new(Vec::new()));
                let _subscription = window.update(|_, cx| {
                    cx.global_mut::<Settings>().ui_language = language;
                    let notes = notes.clone();
                    cx.subscribe(&entity, move |_, event, _| {
                        if let TerminalViewEvent::Notification(
                            crate::notify::Notification::Text { body, program },
                        ) = event
                        {
                            notes.borrow_mut().push((body.clone(), program.clone()));
                        }
                    })
                });
                let saved = AgentSession {
                    source: source.into(),
                    session_id: id.map(str::to_owned),
                    session_file: None,
                    wsl_launch: None,
                };
                entity.update(window, |view, cx| {
                    view.restore_agent(saved.clone(), cx);
                    assert_eq!(view.session_agent(), Some(saved.clone()));
                    assert!(!view.can_retry_recovery());
                    assert!(!view.can_choose_recovery_session());
                    view.retry_recovery(cx);
                    assert!(view.pending_shell_command.is_none());
                    assert!(view.ai_session.is_none());
                    assert!(receiver.try_iter().all(|message| !matches!(message, Msg::Input(_))));
                });
                assert_eq!(
                    *notes.borrow(),
                    [(
                        language.text(Message::SessionRestoreHermesMissingId).to_owned(),
                        Some(source.into())
                    )],
                    "source={source} id={id:?} language={language:?}"
                );
            }
        }
    }
}

#[gpui::test]
fn hermes_unreplayable_wsl_launch_preserves_the_exact_id_without_offering_retry(
    cx: &mut TestAppContext,
) {
    use crate::i18n::{Message, UiLanguage};
    use std::cell::RefCell;
    use std::rc::Rc;

    for language in [UiLanguage::ZhCn, UiLanguage::EnUs] {
        for source in ["hermes", "hermes-agent"] {
            let (entity, window, receiver) = open(cx);
            let notes = Rc::new(RefCell::new(Vec::new()));
            let _subscription = window.update(|_, cx| {
                cx.global_mut::<Settings>().ui_language = language;
                let notes = notes.clone();
                cx.subscribe(&entity, move |_, event, _| {
                    if let TerminalViewEvent::Notification(crate::notify::Notification::Text {
                        body,
                        program,
                    }) = event
                    {
                        notes.borrow_mut().push((body.clone(), program.clone()));
                    }
                })
            });
            let saved = AgentSession {
                source: source.into(),
                session_id: Some(ID.into()),
                session_file: None,
                wsl_launch: Some(crate::session::WslAgentLaunch::unavailable(
                    "fixture", None, "/guest",
                )),
            };
            entity.update(window, |view, cx| {
                assert!(saved.resume_command_for_quote(view.path_quote()).is_none());
                view.restore_agent(saved.clone(), cx);
                assert_eq!(view.session_agent(), Some(saved.clone()));
                assert_eq!(view.recovery.retry_candidate, Some(saved.clone()));
                assert!(!view.can_retry_recovery());
                assert!(!view.can_choose_recovery_session());
                view.retry_recovery(cx);
                assert!(view.pending_shell_command.is_none());
                assert!(view.ai_session.is_none());
                assert!(view.recovery_pending());
                assert!(receiver.try_iter().all(|message| !matches!(message, Msg::Input(_))));
                assert_eq!(view.session_agent(), Some(saved));
            });
            assert_eq!(
                *notes.borrow(),
                [(
                    language.text(Message::SessionRestoreHermesWslUnreplayable).to_owned(),
                    Some(source.into())
                )]
            );
        }
    }
}

#[gpui::test]
fn hermes_other_agents_keep_the_generic_restore_failure_notice(cx: &mut TestAppContext) {
    use crate::i18n::{Message, UiLanguage};
    use std::cell::RefCell;
    use std::rc::Rc;

    for language in [UiLanguage::ZhCn, UiLanguage::EnUs] {
        for source in ["claude", "codex"] {
            let (entity, window, receiver) = open(cx);
            let notes = Rc::new(RefCell::new(Vec::new()));
            let _subscription = window.update(|_, cx| {
                cx.global_mut::<Settings>().ui_language = language;
                let notes = notes.clone();
                cx.subscribe(&entity, move |_, event, _| {
                    if let TerminalViewEvent::Notification(crate::notify::Notification::Text {
                        body,
                        ..
                    }) = event
                    {
                        notes.borrow_mut().push(body.clone());
                    }
                })
            });
            let saved = AgentSession {
                source: source.into(),
                session_id: None,
                session_file: None,
                wsl_launch: None,
            };
            entity.update(window, |view, cx| {
                view.restore_agent(saved.clone(), cx);
                assert_eq!(view.session_agent(), Some(saved));
                assert!(receiver.try_iter().all(|message| !matches!(message, Msg::Input(_))));
            });
            assert_eq!(*notes.borrow(), [language.text(Message::SessionRestoreFailed)]);
        }
    }
}

#[test]
fn hermes_missing_id_translations_describe_the_native_history_boundary() {
    use crate::i18n::{Message, UiLanguage};
    let chinese = UiLanguage::ZhCn.text(Message::SessionRestoreHermesMissingId);
    assert_eq!(
        chinese,
        "原恢复记录已保留，但缺少可恢复的精确会话 ID。请在 Hermes 原生历史中手动定位并选择原对话。"
    );
    assert!(!chinese.contains("标签菜单"));
    assert!(UiLanguage::ZhCn.text(Message::SessionRestoreFailed).contains("标签菜单中重试恢复"));
    for language in UiLanguage::ALL {
        let text = language.text(Message::SessionRestoreHermesMissingId);
        assert!(text.contains("Hermes"));
        assert!(text.contains("ID"));
        assert_ne!(text, language.text(Message::SessionRestoreFailed));
        assert!(text.chars().count() <= crate::notify::TOAST_BODY_MAX_CHARS);
    }
}

#[test]
fn hermes_wsl_unreplayable_translations_do_not_claim_a_missing_id_or_menu_action() {
    use crate::i18n::{Message, UiLanguage};
    let chinese = UiLanguage::ZhCn.text(Message::SessionRestoreHermesWslUnreplayable);
    assert_eq!(
        chinese,
        "原恢复记录已保留，但 WSL 启动信息无法安全重放。请在 Hermes 原生历史中手动定位原对话。"
    );
    let english = UiLanguage::EnUs.text(Message::SessionRestoreHermesWslUnreplayable);
    assert_eq!(
        english,
        "Saved record kept; WSL launch data cannot be safely replayed. Manually locate the original conversation in Hermes' native history."
    );
    for language in UiLanguage::ALL {
        let text = language.text(Message::SessionRestoreHermesWslUnreplayable);
        assert!(text.contains("Hermes"));
        assert!(text.contains("WSL"));
        assert!(!text.contains("ID"));
        assert!(!text.contains("菜单"));
        assert!(!text.to_ascii_lowercase().contains("retry"));
        assert_ne!(text, language.text(Message::SessionRestoreHermesMissingId));
        assert_ne!(text, language.text(Message::SessionRestoreFailed));
        assert!(text.chars().count() <= crate::notify::TOAST_BODY_MAX_CHARS);
    }
}
