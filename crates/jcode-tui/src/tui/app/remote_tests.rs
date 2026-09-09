use super::reconnect;
use super::{
    RemoteRunState, auth_provider_hint_for_login_provider, handle_post_connect,
    handle_server_event, handle_tick, process_remote_followups,
};
use crate::protocol::{
    MemoryActivitySnapshot, MemoryPipelineSnapshot, MemoryStateSnapshot, MemoryStepStatusSnapshot,
    ServerEvent,
};
use crate::provider::Provider;
use crate::tui::info_widget::{MemoryState, StepStatus};
use anyhow::Result;
use std::sync::Arc;

struct MockProvider;

#[async_trait::async_trait]
impl Provider for MockProvider {
    async fn complete(
        &self,
        _messages: &[crate::message::Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        Err(anyhow::anyhow!(
            "Mock provider should not be used for streaming completions in remote app tests"
        ))
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

fn create_test_app() -> crate::tui::app::App {
    ensure_test_jcode_home_if_unset();
    // `has_notification()` (via `unfocused_redraw_warranted`) consults a
    // process-wide ambient-info cache that another test may have populated
    // from its own JCODE_HOME (scheduled reminders read as a notification).
    // Reset it so these tests observe only their own state.
    crate::tui::app::helpers::clear_ambient_info_cache_for_tests();
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = crate::tui::app::App::new_for_test_harness(provider, registry);
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app
}

#[test]
fn clean_composer_app_keys_paste_and_server_error_preserve_one_retryable_prompt() {
    use crossterm::event::{KeyCode, KeyModifiers};
    use std::cell::RefCell;

    let mut app = create_test_app();
    let mut picker = crate::tui::session_picker::SessionPicker::new(Vec::new());
    picker.set_current_dir(Some("/server/here".into()));
    picker.set_new_session_composer_enabled(true);
    app.session_picker_overlay = Some(RefCell::new(picker));

    app.handle_session_picker_key(KeyCode::Char('d'), KeyModifiers::NONE)
        .expect("typing should reach the clean composer");
    app.handle_paste("raft".into());
    app.handle_session_picker_key(KeyCode::Enter, KeyModifiers::NONE)
        .expect("prompt enter should choose a directory");
    app.handle_session_picker_key(KeyCode::Enter, KeyModifiers::NONE)
        .expect("directory enter should queue clean creation");

    let create = app
        .pending_clean_session_create
        .take()
        .expect("app key path must queue the pasted prompt exactly once");
    assert_eq!(create.prompt, "draft");
    assert_eq!(create.working_dir, "/server/here");
    app.in_flight_clean_session_create = Some((77, create));
    app.session_picker_overlay
        .as_ref()
        .expect("picker remains available until SessionCreated")
        .borrow_mut()
        .begin_clean_session_creation(77);

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    assert!(app.handle_server_event(
        ServerEvent::Error {
            id: 77,
            message: "create rejected".into(),
            retry_after_secs: None,
        },
        &mut remote,
    ));
    assert!(app.in_flight_clean_session_create.is_none());

    app.handle_session_picker_key(KeyCode::Enter, KeyModifiers::NONE)
        .expect("retry enter should return through App key handling");
    let retry = app
        .pending_clean_session_create
        .as_ref()
        .expect("server error must preserve a retryable create request");
    assert_eq!(retry.prompt, "draft");
    assert_eq!(retry.working_dir, "/server/here");
}

#[test]
fn clean_capability_tick_only_promotes_explicit_active_sessions_manager() {
    use crossterm::event::{KeyCode, KeyModifiers};
    use std::cell::RefCell;

    for mode in [
        crate::tui::app::SessionPickerMode::Resume,
        crate::tui::app::SessionPickerMode::Onboarding,
    ] {
        // App construction initializes a short-lived Tokio runtime. Keep it
        // outside the runtime used to drive the actual tick.
        let mut app = create_test_app();
        app.session_picker_mode = mode;
        app.session_picker_overlay = Some(RefCell::new(
            crate::tui::session_picker::SessionPicker::new(Vec::new()),
        ));
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let mut remote = crate::tui::backend::RemoteConnection::dummy();
            let probe_id = remote
                .request_clean_session_capability()
                .await
                .expect("dummy control connection accepts capability probe");
            assert!(app.handle_server_event(
                ServerEvent::Pong {
                    id: probe_id,
                    native_ssh_protocol: None,
                    session_preview_protocol: None,
                    clean_session_protocol: Some(1),
                    directory_completion_protocol: None,
                },
                &mut remote,
            ));

            handle_tick(&mut app, &mut remote).await;
            let picker = app.session_picker_overlay.as_ref().expect("picker stays open").borrow();
            assert!(
                !picker.clean_composer_enabled_for_test(),
                "{mode:?} picker must retain its legacy input behavior"
            );
            drop(picker);
            app.handle_session_picker_key(KeyCode::Char('p'), KeyModifiers::NONE)
                .expect("generic picker key dispatch");
            assert!(
                !app
                    .session_picker_overlay
                    .as_ref()
                    .expect("picker stays open")
                    .borrow()
                    .clean_composer_active_for_test(),
                "capability traffic must not turn a generic picker keystroke into a new-session draft"
            );
            assert!(!app.pending_session_creation_context);
        });
    }
}

#[test]
fn active_sessions_composer_resolves_paths_and_preserves_invalid_manual_draft() {
    use crossterm::event::{KeyCode, KeyModifiers};
    use std::cell::RefCell;

    // As above, construct the app before entering this test's tick runtime.
    let mut app = create_test_app();
    app.session_picker_mode = crate::tui::app::SessionPickerMode::ActiveSessions;
    app.pending_session_creation_context = true;
    let mut picker = crate::tui::session_picker::SessionPicker::new(Vec::new());
    picker.set_current_dir(Some("/server/here".into()));
    app.session_picker_overlay = Some(RefCell::new(picker));
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        let probe_id = remote
            .request_clean_session_capability()
            .await
            .expect("probe");
        assert!(app.handle_server_event(
            ServerEvent::Pong {
                id: probe_id,
                native_ssh_protocol: None,
                session_preview_protocol: None,
                clean_session_protocol: Some(1),
                directory_completion_protocol: None,
            },
            &mut remote,
        ));

        handle_tick(&mut app, &mut remote).await;
        assert!(
            app.session_picker_overlay
                .as_ref()
                .expect("active picker")
                .borrow()
                .clean_composer_enabled_for_test()
        );
        let context_id = app
            .in_flight_session_creation_context
            .expect("active manager requests server directory context");
        assert!(app.handle_server_event(
            ServerEvent::SessionCreationContext {
                id: context_id,
                home_dir: "/server/home".into(),
                recent_working_dirs: Vec::new(),
            },
            &mut remote,
        ));

        app.handle_session_picker_key(KeyCode::Char('p'), KeyModifiers::NONE)
            .unwrap();
        app.handle_session_picker_key(KeyCode::Enter, KeyModifiers::NONE)
            .unwrap();
        // Here, Home, then Type path.
        app.handle_session_picker_key(KeyCode::Down, KeyModifiers::NONE)
            .unwrap();
        app.handle_session_picker_key(KeyCode::Down, KeyModifiers::NONE)
            .unwrap();
        app.handle_session_picker_key(KeyCode::Enter, KeyModifiers::NONE)
            .unwrap();
        for ch in "bad_path".chars() {
            app.handle_session_picker_key(KeyCode::Char(ch), KeyModifiers::NONE)
                .unwrap();
        }
        app.handle_session_picker_key(KeyCode::Enter, KeyModifiers::NONE)
            .unwrap();
        handle_tick(&mut app, &mut remote).await;
        let bad_id = app
            .in_flight_working_dir_resolution
            .as_ref()
            .map(|(id, path)| (*id, path.clone()))
            .expect("manual Enter sends ResolveWorkingDirectory through the app tick");
        assert_ne!(bad_id.0, 0);
        assert_eq!(bad_id.1, "bad_path");
        assert!(app.handle_server_event(
            ServerEvent::Error {
                id: bad_id.0,
                message: "invalid path".into(),
                retry_after_secs: None,
            },
            &mut remote,
        ));
        {
            let picker = app.session_picker_overlay.as_ref().unwrap().borrow();
            assert_eq!(
                picker.clean_composer_feedback_for_test(),
                Some("invalid path")
            );
        }

        for _ in 0.."bad_path".len() {
            app.handle_session_picker_key(KeyCode::Backspace, KeyModifiers::NONE)
                .unwrap();
        }
        for ch in "~/".chars() {
            app.handle_session_picker_key(KeyCode::Char(ch), KeyModifiers::NONE)
                .unwrap();
        }
        app.handle_session_picker_key(KeyCode::Enter, KeyModifiers::NONE)
            .unwrap();
        handle_tick(&mut app, &mut remote).await;
        let home_id = app.in_flight_working_dir_resolution.as_ref().unwrap().0;
        assert!(app.handle_server_event(
            ServerEvent::WorkingDirectoryResolved {
                id: home_id,
                input: "~/".into(),
                absolute_path: "/server/home".into(),
            },
            &mut remote,
        ));
        assert_eq!(
            app.pending_clean_session_create
                .as_ref()
                .map(|create| (create.prompt.as_str(), create.working_dir.as_str())),
            Some(("p", "/server/home")),
            "the correlated canonical reply queues exactly the Enter-intended create"
        );
        // Repeated Enter while creating cannot queue a duplicate request.
        app.handle_session_picker_key(KeyCode::Enter, KeyModifiers::NONE)
            .unwrap();
        handle_tick(&mut app, &mut remote).await;
        let create_id = app
            .in_flight_clean_session_create
            .as_ref()
            .map(|(id, create)| (*id, create.prompt.clone(), create.working_dir.clone()))
            .expect("canonical resolution creates without another key");
        assert_eq!(create_id.1, "p");
        assert_eq!(create_id.2, "/server/home");
        assert!(app.pending_clean_session_create.is_none());
        assert!(app.handle_server_event(
            ServerEvent::SessionCreated {
                id: create_id.0,
                session_id: "created-session".into(),
                session_name: "Created session".into(),
                working_dir: "/server/home".into(),
            },
            &mut remote,
        ));
        assert!(app.in_flight_clean_session_create.is_none());
        assert_eq!(
            app.pending_session_start_prompt
                .as_ref()
                .map(|prompt| (prompt.target_session_id.as_deref(), prompt.content.as_str())),
            Some((Some("created-session"), "p")),
            "SessionCreated retains the original prompt exactly once for attach/send"
        );
    });
}

#[test]
fn active_sessions_path_completion_edits_inline_and_ignores_stale_replies() {
    use crossterm::event::{KeyCode, KeyModifiers};
    use std::cell::RefCell;
    use std::time::Duration;

    let mut app = create_test_app();
    app.session_picker_mode = crate::tui::app::SessionPickerMode::ActiveSessions;
    let mut picker = crate::tui::session_picker::SessionPicker::new(Vec::new());
    picker.set_current_dir(Some("/server/here".into()));
    picker.set_new_session_composer_enabled(true);
    picker.set_session_creation_context("/server/home".into(), vec!["/recent/one".into()]);
    app.session_picker_overlay = Some(RefCell::new(picker));

    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        let probe_id = remote
            .request_clean_session_capability()
            .await
            .expect("probe");
        assert!(app.handle_server_event(
            ServerEvent::Pong {
                id: probe_id,
                native_ssh_protocol: None,
                session_preview_protocol: None,
                clean_session_protocol: Some(1),
                directory_completion_protocol: Some(1),
            },
            &mut remote,
        ));

        app.handle_session_picker_key(KeyCode::Char('p'), KeyModifiers::NONE)
            .unwrap();
        app.handle_session_picker_key(KeyCode::Enter, KeyModifiers::NONE)
            .unwrap();
        // Here is selected by default. A first printable character and a paste
        // must enter the path editor directly, without moving to Type path.
        app.handle_session_picker_key(KeyCode::Char('~'), KeyModifiers::NONE)
            .unwrap();
        app.handle_paste("/project space".into());
        assert_eq!(
            app.session_picker_overlay
                .as_ref()
                .unwrap()
                .borrow()
                .clean_composer_manual_path_for_test(),
            Some("~/project space")
        );

        std::thread::sleep(Duration::from_millis(130));
        handle_tick(&mut app, &mut remote).await;
        let first = app
            .in_flight_working_dir_completion
            .as_ref()
            .expect("debounced path edit requests server completion")
            .clone();

        app.handle_session_picker_key(KeyCode::Char('s'), KeyModifiers::NONE)
            .unwrap();
        std::thread::sleep(Duration::from_millis(130));
        handle_tick(&mut app, &mut remote).await;
        let second = app
            .in_flight_working_dir_completion
            .as_ref()
            .expect("new edit replaces completion request")
            .clone();
        assert_ne!(first.0, second.0);
        assert!(!app.handle_server_event(
            ServerEvent::WorkingDirectoryCompletions {
                id: first.0,
                input: first.1,
                candidates: vec!["~/stale/".into()],
                truncated: false,
            },
            &mut remote,
        ));
        assert!(app.handle_server_event(
            ServerEvent::WorkingDirectoryCompletions {
                id: second.0,
                input: second.1,
                candidates: vec!["~/project spaces/".into()],
                truncated: false,
            },
            &mut remote,
        ));
        app.handle_session_picker_key(KeyCode::Tab, KeyModifiers::NONE)
            .unwrap();
        app.handle_session_picker_key(KeyCode::Enter, KeyModifiers::NONE)
            .unwrap();
        assert_eq!(
            app.in_flight_working_dir_resolution
                .as_ref()
                .map(|(_, path)| path.as_str()),
            Some("~/project spaces/")
        );
    });
}

/// Point JCODE_HOME at a per-process temp dir when the environment does not
/// already pin one, so tests never read the developer's real `~/.jcode`
/// state (e.g. a populated ambient queue turns `has_notification()` on and
/// breaks the unfocused-redraw assertions). Mirrors the helper of the same
/// name used by the main app test suite.
fn ensure_test_jcode_home_if_unset() {
    use std::sync::OnceLock;

    static TEST_HOME: OnceLock<std::path::PathBuf> = OnceLock::new();

    if std::env::var_os("JCODE_HOME").is_some() {
        return;
    }

    let path = TEST_HOME.get_or_init(|| {
        let path = std::env::temp_dir().join(format!("jcode-test-home-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&path);
        path
    });
    crate::env::set_var("JCODE_HOME", path);
}

#[test]
fn reload_handoff_active_when_server_flag_is_set() {
    let state = RemoteRunState {
        server_reload_in_progress: true,
        ..RemoteRunState::default()
    };

    assert!(reconnect::reload_handoff_active(&state));
}

#[test]
fn client_focus_defaults_to_true() {
    let app = create_test_app();
    assert!(
        app.client_focused(),
        "a freshly created client should start focused so terminals that never \
         report focus events still animate/redraw normally"
    );
}

#[test]
fn idle_donut_pauses_while_unfocused() {
    let mut app = create_test_app();

    // Whether the donut runs while focused depends on the machine's perf tier
    // and `display.idle_animation` config, so we do not assert the focused case
    // absolutely. We only assert the invariant that matters for the swarm CPU
    // regression: it must never run while the terminal is unfocused.
    let redraw = app.set_client_focused(false);
    assert!(
        !redraw,
        "losing focus should not request an immediate redraw"
    );
    assert!(!app.client_focused());
    assert!(
        !crate::tui::idle_donut_active(&app),
        "idle animation must pause while the terminal is unfocused"
    );

    // Regaining focus requests a differential redraw so the window catches up
    // without clearing and retransmitting every terminal cell.
    app.force_full_redraw = false;
    let redraw = app.set_client_focused(true);
    assert!(redraw, "regaining focus should request a redraw");
    assert!(app.client_focused());
    assert!(
        !app.force_full_redraw,
        "focus changes must not force an expensive full-terminal repaint"
    );
}

#[test]
fn unfocused_redraw_warranted_tracks_live_activity() {
    let mut app = create_test_app();
    // `unfocused_redraw_warranted` is only consulted while unfocused, and the
    // decorative donut is force-disabled when unfocused, so evaluate it in that
    // state to mirror the run loop.
    app.set_client_focused(false);

    // Idle empty session: no live output to paint while unfocused.
    assert!(
        !app.unfocused_redraw_warranted(),
        "an idle unfocused session has nothing changing worth a full-rate redraw"
    );

    // A streaming/processing session keeps painting even while unfocused so a
    // visible-but-unfocused window in a tiling WM still shows live progress.
    app.is_processing = true;
    assert!(
        app.unfocused_redraw_warranted(),
        "a processing session should keep redrawing while unfocused"
    );
}

#[test]
fn client_interaction_restores_focus_so_scroll_redraws_at_full_rate() {
    // Regression for the intermittent "can't scroll" bug. If a FocusGained is
    // dropped (flaky under tiling WMs / multiplexers) the window can get stuck
    // as "unfocused idle", which the run loop throttles to ~1 Hz. Any terminal
    // input (key/mouse/scroll) is only delivered to the focused window, so it
    // must restore the focused state and full-rate redraws immediately.
    let mut app = create_test_app();

    // Simulate a stuck-unfocused window (FocusLost seen, FocusGained dropped).
    app.set_client_focused(false);
    assert!(!app.client_focused());
    assert!(
        !app.unfocused_redraw_warranted(),
        "an idle unfocused session is throttled to ~1 Hz redraws"
    );

    // A mouse-wheel / key event arrives: the terminal only routes input to the
    // focused window, so interacting proves focus and must restore it.
    app.note_client_interaction();
    assert!(
        app.client_focused(),
        "interaction must restore focus so scrolling repaints at full rate"
    );
}

#[test]
fn auth_provider_hint_maps_openai_compatible_login_providers() {
    assert_eq!(
        auth_provider_hint_for_login_provider("Azure OpenAI"),
        Some("azure-openai")
    );
    assert_eq!(
        auth_provider_hint_for_login_provider("cerebras"),
        Some("cerebras")
    );
    assert_eq!(
        auth_provider_hint_for_login_provider("Cerebras"),
        Some("cerebras")
    );
    assert_eq!(
        auth_provider_hint_for_login_provider("minimax"),
        Some("minimax")
    );
    assert_eq!(
        auth_provider_hint_for_login_provider("not-a-provider"),
        None
    );
}

#[test]
fn auth_provider_hint_maps_direct_provider_logins_by_display_label() {
    // LoginCompleted carries the descriptor display label, which must still map
    // to the canonical server provider id so the auth-change refresh is
    // attributed correctly (regression: an Anthropic API-key login used to send
    // no hint, so the server reported "OpenAI credentials are active" and
    // skipped the post-login model switch).
    assert_eq!(
        auth_provider_hint_for_login_provider("Anthropic API"),
        Some("anthropic-api")
    );
    assert_eq!(
        auth_provider_hint_for_login_provider("anthropic-api"),
        Some("anthropic-api")
    );
    assert_eq!(
        auth_provider_hint_for_login_provider("claude-api"),
        Some("anthropic-api")
    );
    assert_eq!(
        auth_provider_hint_for_login_provider("Anthropic/Claude"),
        Some("claude")
    );
    assert_eq!(
        auth_provider_hint_for_login_provider("claude"),
        Some("claude")
    );
    assert_eq!(
        auth_provider_hint_for_login_provider("OpenAI"),
        Some("openai")
    );
    assert_eq!(
        auth_provider_hint_for_login_provider("OpenAI API"),
        Some("openai-api")
    );
    assert_eq!(
        auth_provider_hint_for_login_provider("OpenRouter"),
        Some("openrouter")
    );
    assert_eq!(
        auth_provider_hint_for_login_provider("AWS Bedrock"),
        Some("bedrock")
    );
}

#[test]
fn auth_provider_hint_resolves_every_emitted_login_completed_provider() {
    // Every string published as `LoginCompleted.provider` (see the emit sites in
    // src/tui/app/auth.rs) must resolve to a canonical server provider id so the
    // auth-change refresh is attributed to the right provider and the post-login
    // model auto-select runs. Before the loose display-name resolution, only
    // Azure and OpenAI-compatible logins resolved; every direct provider sent no
    // hint, so the server fell back to the session's active provider (the
    // "OpenAI credentials are active" bug) and skipped the model switch.
    //
    // Pairs of (emitted string, expected canonical hint). `None` is only correct
    // for auto-import, which intentionally has no single runtime to attribute to.
    let cases: &[(&str, Option<&str>)] = &[
        // OAuth logins emit lowercase descriptor ids.
        ("openai", Some("openai")),
        ("claude", Some("claude")),
        ("gemini", Some("gemini")),
        ("copilot", Some("copilot")),
        ("antigravity", Some("antigravity")),
        ("cursor", Some("cursor")),
        // API-key paste logins emit descriptor display labels.
        ("Anthropic API", Some("anthropic-api")),
        ("OpenAI API", Some("openai-api")),
        ("AWS Bedrock", Some("bedrock")),
        ("OpenRouter", Some("openrouter")),
        // Azure keeps its dedicated runtime id mapping.
        ("Azure OpenAI", Some("azure-openai")),
        // Auto-import has no single runtime to attribute the refresh to.
        ("auto-import", None),
    ];

    for (emitted, expected) in cases {
        assert_eq!(
            auth_provider_hint_for_login_provider(emitted),
            *expected,
            "login provider {emitted:?} should resolve to {expected:?}"
        );
    }

    // Every login provider descriptor must resolve to the EXACT canonical hint
    // implied by its target, across its display label, id, and every alias.
    // Asserting the exact value (not just `is_some`) is what catches a
    // wrong-attribution bug like the original "OpenAI credentials are active"
    // after an Anthropic login - a weak `is_some` check would have passed even
    // while the hint pointed at the wrong provider.
    use crate::provider_catalog::LoginProviderTarget;
    for descriptor in crate::provider_catalog::login_providers() {
        // Expected hint mirrors auth_provider_hint_for_login_provider's target
        // mapping, the single source of truth for post-login attribution.
        let expected: Option<String> = match descriptor.target {
            LoginProviderTarget::AutoImport => None,
            LoginProviderTarget::Azure => Some("azure-openai".to_string()),
            LoginProviderTarget::OpenAiCompatible(profile) => Some(profile.id.to_string()),
            _ => Some(descriptor.id.to_string()),
        };

        // The emitted string can be the descriptor id, its display label, or any
        // alias (LoginCompleted.provider varies by surface/auth path).
        let mut emitted: Vec<&str> = vec![descriptor.id, descriptor.display_name];
        emitted.extend_from_slice(descriptor.aliases);
        for label in emitted {
            assert_eq!(
                auth_provider_hint_for_login_provider(label),
                expected.as_deref(),
                "login provider {:?} (id {:?}, target {:?}) emitted as {label:?} must attribute \
                 to {expected:?}; a wrong/missing hint mislabels the catalog-refresh message and \
                 skips the post-login model switch",
                descriptor.display_name,
                descriptor.id,
                descriptor.target
            );
        }
    }
}

#[test]
fn auth_changed_event_for_anthropic_api_login_targets_claude_api_route() {
    let auth = super::auth_changed_event_for_login_provider("Anthropic API")
        .expect("Anthropic API login should produce a typed auth event");
    // The server maps the descriptor id `anthropic-api` to the `claude-api`
    // route family for model selection and labelling.
    assert_eq!(auth.provider.as_str(), "anthropic-api");
    assert_eq!(
        auth.auth_method,
        Some(crate::protocol::AuthMethod::RemoteTuiPasteApiKey)
    );
    assert_eq!(
        auth.credential_source,
        Some(crate::protocol::AuthCredentialSource::ApiKeyFile)
    );
    // Direct providers must not claim the OpenAI-compatible runtime/namespace.
    assert!(auth.expected_runtime.is_none());
    assert!(auth.expected_catalog_namespace.is_none());
}

#[test]
fn auth_changed_event_for_oauth_claude_login_is_not_marked_as_api_key_paste() {
    let auth = super::auth_changed_event_for_login_provider("claude")
        .expect("Claude OAuth login should produce a typed auth event");
    assert_eq!(auth.provider.as_str(), "claude");
    // OAuth logins are not API-key pastes.
    assert!(auth.auth_method.is_none());
    assert!(auth.credential_source.is_none());
}

#[test]
fn auth_changed_event_for_cerebras_login_carries_runtime_and_catalog_identity() {
    let auth = super::auth_changed_event_for_login_provider("Cerebras")
        .expect("Cerebras login should produce typed auth event");

    assert_eq!(auth.provider.as_str(), "cerebras");
    assert_eq!(
        auth.credential_source,
        Some(crate::protocol::AuthCredentialSource::ApiKeyFile)
    );
    assert_eq!(
        auth.auth_method,
        Some(crate::protocol::AuthMethod::RemoteTuiPasteApiKey)
    );
    assert_eq!(
        auth.expected_runtime
            .as_ref()
            .map(crate::protocol::RuntimeProviderKey::as_str),
        Some("openai-compatible")
    );
    assert_eq!(
        auth.expected_catalog_namespace
            .as_ref()
            .map(crate::protocol::CatalogNamespace::as_str),
        Some("cerebras")
    );
}

#[test]
fn reload_handoff_inactive_without_flag_or_marker() {
    // `reload_handoff_active` falls back to the on-disk reload marker in the
    // runtime dir. Point the runtime dir at an empty tempdir so a real
    // `jcode.reload` left by a live self-dev reload on this machine cannot
    // leak into the assertion.
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("create temp dir");
    let prev_runtime = std::env::var_os("JCODE_RUNTIME_DIR");
    crate::env::set_var("JCODE_RUNTIME_DIR", temp.path());

    let inactive = !reconnect::reload_handoff_active(&RemoteRunState::default());

    if let Some(prev_runtime) = prev_runtime {
        crate::env::set_var("JCODE_RUNTIME_DIR", prev_runtime);
    } else {
        crate::env::remove_var("JCODE_RUNTIME_DIR");
    }

    assert!(inactive);
}

#[test]
fn reload_wait_status_message_uses_waiting_language() {
    let mut app = create_test_app();
    app.resume_session_id = Some("ses_test_reload_wait".to_string());
    let state = RemoteRunState::default();

    let message = reconnect::reload_wait_status_message(&app, &state, "server reload in progress");

    assert!(message.contains("waiting for handoff"));
    assert!(!message.contains("retrying"));
}

#[test]
fn submit_prepared_remote_input_defers_until_history_loads() {
    // Regression for the intermittent "first prompt vanishes / weird render"
    // bug: when a manual submit lands before the bootstrap History payload is
    // applied, the History handler's `session_changed` branch calls
    // `clear_display_messages()` and wipes the just-echoed user message. The
    // submit path must hold the prompt until history loads instead of echoing
    // and sending it into that race.
    let mut app = create_test_app();
    app.is_remote = true;
    app.runtime_mode = crate::tui::app::AppRuntimeMode::RemoteClient;

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    // History has NOT loaded yet (fresh connect window).
    assert!(!remote.has_loaded_history());

    let prepared = crate::tui::app::input::PreparedInput {
        raw_input: "hi".to_string(),
        expanded: "hi".to_string(),
        images: vec![],
    };
    rt.block_on(crate::tui::app::remote::submit_prepared_remote_input(
        &mut app,
        &mut remote,
        prepared,
    ))
    .expect("submit should not error while history is loading");

    // The prompt must be held, not echoed or sent.
    assert!(
        !app.is_processing,
        "submit must not begin a remote send before history loads"
    );
    assert!(
        app.display_messages().iter().all(|m| m.role != "user"),
        "user message must not be echoed before history loads (would be clobbered)"
    );
    let held = app
        .pending_prompt_before_history
        .as_ref()
        .expect("prompt should be held until history loads");
    assert_eq!(held.raw_input, "hi");

    // Once history loads, the post-connect dispatcher fires the held prompt.
    remote.mark_history_loaded();
    rt.block_on(process_remote_followups(&mut app, &mut remote));

    assert!(
        app.pending_prompt_before_history.is_none(),
        "held prompt should be consumed once history is loaded"
    );
    assert!(
        app.display_messages()
            .iter()
            .any(|m| m.role == "user" && m.content == "hi"),
        "the held prompt should be echoed as a user message after history loads"
    );
    assert!(
        app.is_processing,
        "the held prompt should be sent once history is loaded"
    );
}

#[test]
fn remote_skill_invocation_with_prompt_sends_remote_turn() {
    let mut app = create_test_app();
    app.is_remote = true;
    app.runtime_mode = crate::tui::app::AppRuntimeMode::RemoteClient;
    let temp = tempfile::tempdir().expect("create skill dir");
    let skill_dir = temp.path().join(".jcode/skills/remote-skill");
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: remote-skill\ndescription: Remote prompt regression skill\n---\nUse it.\n",
    )
    .expect("write skill");
    app.session.working_dir = Some(temp.path().to_string_lossy().to_string());

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();
    rt.block_on(crate::tui::app::remote::submit_remote_slash_input(
        &mut app,
        &mut remote,
        crate::tui::app::input::PreparedInput {
            raw_input: "/remote-skill explain the change".to_string(),
            expanded: "/remote-skill explain the change".to_string(),
            images: vec![],
        },
    ))
    .expect("remote skill prompt should send");

    assert_eq!(app.active_skill.as_deref(), Some("remote-skill"));
    assert!(app.is_processing, "remote skill prompt should start a turn");
    assert!(
        app.display_messages()
            .iter()
            .any(|message| message.role == "user"
                && message.content == "/remote-skill explain the change"),
        "remote skill prompt should be visible as the submitted user turn"
    );
}

#[test]
fn process_remote_followups_auto_submits_staged_startup_prompt() {
    // Regression for issues #267/#268/#76: a headed swarm spawn stages its
    // initial prompt into `app.input` with `submit_input_on_startup = true`
    // (not `queued_messages`). The post-connect dispatcher must still submit it;
    // otherwise the spawned agent shows its prompt but never sends it.
    let mut app = create_test_app();
    app.is_remote = true;
    app.runtime_mode = crate::tui::app::AppRuntimeMode::RemoteClient;
    app.input = "Classify the issues in /tmp/batch.txt".to_string();
    app.cursor_pos = app.input.len();
    app.submit_input_on_startup = true;

    // The gate predicate is the actual fix site: a staged startup prompt counts
    // as pending work even though no message was queued via `queued_messages`.
    assert!(
        !app.has_queued_followups(),
        "a staged startup prompt is not a queued follow-up"
    );
    assert!(
        app.has_pending_startup_submission(),
        "staged startup prompt should be recognized as pending work so the \
         post-connect dispatcher invokes process_remote_followups"
    );

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();

    rt.block_on(process_remote_followups(&mut app, &mut remote));

    assert!(
        !app.submit_input_on_startup,
        "startup submission flag should be consumed after dispatch"
    );
    assert!(
        app.input.is_empty(),
        "input should be cleared once the startup prompt is submitted"
    );
    assert!(
        app.display_messages()
            .iter()
            .any(|message| message.role == "user"
                && message.content == "Classify the issues in /tmp/batch.txt"),
        "submitting the startup prompt should record it as a user message"
    );
}

#[test]
fn process_remote_followups_sends_startup_prompt_before_history_arrives() {
    let mut app = create_test_app();
    app.is_remote = true;
    app.runtime_mode = crate::tui::app::AppRuntimeMode::RemoteClient;
    app.input = "Start the fork immediately".to_string();
    app.cursor_pos = app.input.len();
    app.submit_input_on_startup = true;

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    assert!(!remote.has_loaded_history());

    rt.block_on(process_remote_followups(&mut app, &mut remote));

    assert!(!app.submit_input_on_startup);
    assert!(app.input.is_empty());
    assert!(
        app.is_processing,
        "the ordered startup request should start immediately"
    );
    assert!(
        app.pending_startup_prompt_echo.as_deref() == Some("Start the fork immediately"),
        "the user echo must be retained until bootstrap History is applied"
    );

    // Bootstrap History replaces the visible transcript. The retained echo is
    // restored afterwards so the fork prompt does not visually disappear.
    crate::tui::app::remote::input_dispatch::restore_pending_startup_prompt_echo(&mut app);
    assert!(app.display_messages().iter().any(|message| {
        message.role == "user" && message.content == "Start the fork immediately"
    }));
    assert!(app.pending_startup_prompt_echo.is_none());
}

#[test]
fn has_pending_startup_submission_requires_input_and_flag() {
    // Guards the predicate that gates post-connect startup dispatch.
    let mut app = create_test_app();
    assert!(!app.has_pending_startup_submission());

    app.submit_input_on_startup = true;
    assert!(
        !app.has_pending_startup_submission(),
        "flag alone with empty input is not a pending submission"
    );

    app.input = "   ".to_string();
    assert!(
        !app.has_pending_startup_submission(),
        "whitespace-only input is not a pending submission"
    );

    app.input = "do the work".to_string();
    assert!(app.has_pending_startup_submission());

    app.submit_input_on_startup = false;
    assert!(
        !app.has_pending_startup_submission(),
        "input without the auto-submit flag is just editor state, not pending"
    );
}

#[test]
fn process_remote_followups_auto_reloads_server_by_default() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();

    app.pending_server_reload = true;
    app.auto_server_reload = true;

    rt.block_on(process_remote_followups(&mut app, &mut remote));

    assert!(!app.pending_server_reload);
    let last = app
        .display_messages()
        .last()
        .expect("missing reload message");
    assert_eq!(last.title.as_deref(), Some("Reload"));
    assert!(last.content.contains("Reloading server with newer binary"));
}

#[test]
fn process_remote_followups_reloads_server_even_before_history_loads() {
    // Regression guard: when the server/client binaries differ, the History
    // handler defers session state and sets `pending_server_reload = true`
    // WITHOUT marking history as loaded. The reload must still fire; otherwise
    // history stays unloaded forever and every typed prompt stalls on
    // "Loading session..." until the user restarts.
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    // Intentionally do NOT mark history loaded, mirroring the deferred path.
    assert!(!remote.has_loaded_history());

    app.pending_server_reload = true;
    app.auto_server_reload = true;

    rt.block_on(process_remote_followups(&mut app, &mut remote));

    assert!(
        !app.pending_server_reload,
        "pending server reload should be consumed even while history is unloaded"
    );
    let last = app
        .display_messages()
        .last()
        .expect("missing reload message");
    assert_eq!(last.title.as_deref(), Some("Reload"));
    assert!(last.content.contains("Reloading server with newer binary"));
}

#[test]
fn process_remote_followups_respects_disabled_auto_server_reload() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();

    app.pending_server_reload = true;
    app.auto_server_reload = false;

    rt.block_on(process_remote_followups(&mut app, &mut remote));

    assert!(!app.pending_server_reload);
    let last = app.display_messages().last().expect("missing info message");
    assert_eq!(last.role, "system");
    assert!(last.content.contains("display.auto_server_reload = false"));
}

#[test]
fn process_remote_followups_pauses_auto_reload_after_repeated_attempts() {
    // Regression guard for issue #277: a false-positive "server has update" must
    // not auto-reload forever. After the breaker threshold we stop reloading and
    // surface a message instead.
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let _guard = rt.enter();
    let mut app = create_test_app();
    app.auto_server_reload = true;

    // Simulate the server repeatedly reporting an update on every history event.
    let mut paused = false;
    for _ in 0..10 {
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        remote.mark_history_loaded();
        app.pending_server_reload = true;
        rt.block_on(process_remote_followups(&mut app, &mut remote));
        assert!(!app.pending_server_reload);
        if let Some(last) = app.display_messages().last()
            && last.content.contains("auto-reload paused")
        {
            paused = true;
            break;
        }
    }

    assert!(
        paused,
        "auto-reload should eventually pause to avoid an infinite reload loop"
    );
}

#[test]
fn handle_post_connect_dispatches_reload_followup_even_if_history_snapshot_looks_busy() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("create temp home");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp_home.path());

    let session_id = "session_reload_busy_snapshot";
    crate::tool::selfdev::ReloadContext {
        task_context: Some("Validate reload continuation after reconnect".to_string()),
        version_before: "old-build".to_string(),
        version_after: "new-build".to_string(),
        session_id: session_id.to_string(),
        timestamp: "2026-04-14T00:00:00Z".to_string(),
    }
    .save()
    .expect("save reload context");

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let mut app = crate::tui::app::App::new_for_remote(Some(session_id.to_string()));
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app.authorize_reload_recovery(session_id);
    app.is_processing = true;
    app.status = crate::tui::app::ProcessingStatus::RunningTool("batch".to_string());
    app.processing_started = Some(std::time::Instant::now());
    app.remote_resume_activity = Some(crate::tui::app::RemoteResumeActivity {
        session_id: session_id.to_string(),
        observed_at: std::time::Instant::now(),
        current_tool_name: Some("batch".to_string()),
    });

    let _enter = rt.enter();
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();
    let mut state = super::RemoteRunState {
        reconnect_attempts: 1,
        ..Default::default()
    };

    let outcome = rt
        .block_on(handle_post_connect(
            &mut app,
            &mut terminal,
            &mut remote,
            &mut state,
            Some(session_id),
        ))
        .expect("post connect should succeed");

    assert!(matches!(outcome, super::PostConnectOutcome::Ready));
    assert!(
        app.hidden_queued_system_messages.is_empty(),
        "reload continuation should dispatch instead of staying hidden"
    );
    assert!(matches!(
        app.status,
        crate::tui::app::ProcessingStatus::Sending
    ));
    assert!(app.current_message_id.is_some());
    assert!(app.rate_limit_pending_message.is_some());

    if let Ok(path) = crate::tool::selfdev::ReloadContext::path_for_session(session_id) {
        let _ = std::fs::remove_file(path);
    }
    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[test]
fn handle_server_event_applies_remote_memory_activity_snapshot() {
    crate::memory::clear_activity();

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let _guard = rt.enter();
    let mut app = create_test_app();
    app.memory_enabled = true;
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    handle_server_event(
        &mut app,
        ServerEvent::MemoryActivity {
            activity: MemoryActivitySnapshot {
                state: MemoryStateSnapshot::SidecarChecking { count: 3 },
                state_age_ms: 180,
                pipeline: Some(MemoryPipelineSnapshot {
                    search: MemoryStepStatusSnapshot::Done,
                    search_result: None,
                    verify: MemoryStepStatusSnapshot::Running,
                    verify_result: None,
                    verify_progress: Some((1, 3)),
                    inject: MemoryStepStatusSnapshot::Pending,
                    inject_result: None,
                    maintain: MemoryStepStatusSnapshot::Pending,
                    maintain_result: None,
                }),
            },
        },
        &mut remote,
    );

    let activity = crate::memory::get_activity().expect("memory activity should be populated");
    assert_eq!(activity.state, MemoryState::SidecarChecking { count: 3 });
    let pipeline = activity.pipeline.expect("pipeline should be restored");
    assert_eq!(pipeline.search, StepStatus::Done);
    assert_eq!(pipeline.verify, StepStatus::Running);
    assert_eq!(pipeline.verify_progress, Some((1, 3)));
    assert!(activity.state_since.elapsed().as_millis() >= 100);

    crate::memory::clear_activity();
}

/// Reproduces the "stuck on loading session…" bug and verifies the watchdog
/// recovers it: a remote connection that never receives the bootstrap History
/// event (so `has_loaded_history()` stays false) must re-request `GetHistory`
/// once it has waited past the recovery delay, instead of staying stuck forever.
#[test]
fn remote_history_watchdog_rerequests_history_when_stuck() {
    use std::time::{Duration, Instant};
    use tokio::io::AsyncBufReadExt;

    let mut app = create_test_app();
    app.is_remote = true;
    app.remote_session_id = Some("session_stuck".to_string());

    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut line = String::new();
    let (redraw, attempts) = rt.block_on(async {
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        // The bug condition: history never loaded after (re)connect.
        assert!(!remote.has_loaded_history());
        let peer = remote
            .take_dummy_peer()
            .expect("dummy remote should retain peer stream");
        let (reader, _writer) = peer.into_split();
        let mut reader = tokio::io::BufReader::new(reader);

        // First tick simply starts tracking the wait; no re-request yet.
        let first = super::recover_stuck_remote_history(&mut app, &mut remote).await;
        assert!(!first, "first observation should only arm the watchdog");
        assert!(app.remote_history_wait_started.is_some());
        assert_eq!(app.remote_history_recovery_attempts, 0);

        // Simulate the connection having been stuck past the recovery delay.
        app.remote_history_wait_started = Instant::now().checked_sub(Duration::from_secs(60));

        let redraw = super::recover_stuck_remote_history(&mut app, &mut remote).await;
        reader
            .read_line(&mut line)
            .await
            .expect("history re-request should be readable by peer");
        (redraw, app.remote_history_recovery_attempts)
    });

    assert!(redraw, "re-requesting history should trigger a redraw");
    assert_eq!(
        attempts, 1,
        "watchdog should have re-requested history once"
    );
    assert!(matches!(
        serde_json::from_str::<crate::protocol::Request>(&line)
            .expect("history re-request should deserialize"),
        crate::protocol::Request::GetHistory { .. }
    ));
}

/// A partial inbound frame proves that the original History response is in
/// flight. The watchdog must not queue another full response behind it.
#[test]
fn remote_history_watchdog_does_not_rerequest_while_frame_is_arriving() {
    use std::time::{Duration, Instant};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let mut app = create_test_app();
    app.is_remote = true;
    app.remote_session_id = Some("session_large".to_string());
    app.remote_history_wait_started = Instant::now().checked_sub(Duration::from_secs(60));

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        let peer = remote
            .take_dummy_peer()
            .expect("dummy remote should retain peer stream");
        let (reader, mut writer) = peer.into_split();
        let mut reader = tokio::io::BufReader::new(reader);

        // Deliberately omit the newline so next_event retains this partial
        // History-sized frame when its future is cancelled by the tick.
        writer
            .write_all(b"{\"type\":\"history\",\"messages\":[")
            .await
            .expect("partial frame should reach remote");
        assert!(
            tokio::time::timeout(Duration::from_millis(20), remote.next_event())
                .await
                .is_err(),
            "partial frame must remain incomplete"
        );
        assert!(remote.has_buffered_inbound_frame());

        let redraw = super::recover_stuck_remote_history(&mut app, &mut remote).await;
        assert!(!redraw, "in-flight history should not trigger recovery");
        assert_eq!(app.remote_history_recovery_attempts, 0);

        let mut line = String::new();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), reader.read_line(&mut line))
                .await
                .is_err(),
            "watchdog must not write a duplicate GetHistory request"
        );
    });
}

/// Once history loads, the watchdog must clear its budget and do nothing.
#[test]
fn remote_history_watchdog_clears_budget_once_history_loads() {
    use std::time::{Duration, Instant};

    let mut app = create_test_app();
    app.is_remote = true;
    app.remote_history_wait_started = Instant::now().checked_sub(Duration::from_secs(60));
    app.remote_history_recovery_attempts = 2;

    let rt = tokio::runtime::Runtime::new().unwrap();
    let redraw = rt.block_on(async {
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        remote.mark_history_loaded();
        super::recover_stuck_remote_history(&mut app, &mut remote).await
    });

    assert!(!redraw);
    assert!(app.remote_history_wait_started.is_none());
    assert_eq!(app.remote_history_recovery_attempts, 0);
    assert!(app.remote_history_recovery_last_attempt.is_none());
}

/// After exhausting re-requests the watchdog surfaces an actionable `/restart`
/// hint exactly once instead of silently leaving the user stuck.
#[test]
fn remote_history_watchdog_advises_restart_after_giving_up() {
    use std::time::{Duration, Instant};

    let mut app = create_test_app();
    app.is_remote = true;
    app.remote_history_wait_started = Instant::now().checked_sub(Duration::from_secs(60));
    app.remote_history_recovery_attempts = super::REMOTE_HISTORY_RECOVERY_MAX_ATTEMPTS;
    app.remote_history_recovery_last_attempt = Some(Instant::now());

    let before = app.display_messages().len();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let redraw = rt.block_on(async {
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        super::recover_stuck_remote_history(&mut app, &mut remote).await
    });

    assert!(redraw);
    let messages = app.display_messages();
    assert_eq!(messages.len(), before + 1, "should add exactly one hint");
    assert!(
        messages.last().unwrap().content.contains("/restart"),
        "hint should advise /restart: {}",
        messages.last().unwrap().content
    );
    // last_attempt cleared so the hint is not repeated every tick.
    assert!(app.remote_history_recovery_last_attempt.is_none());

    // A subsequent tick must not add another hint.
    let rt2 = tokio::runtime::Runtime::new().unwrap();
    let redraw2 = rt2.block_on(async {
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        super::recover_stuck_remote_history(&mut app, &mut remote).await
    });
    assert!(!redraw2);
    assert_eq!(app.display_messages().len(), before + 1);
}

/// Regression for issue #427: picking an effort-variant model row (e.g.
/// "gpt-5.5 (high)") in remote mode must forward the chosen effort to the
/// server after the model-switch request. Previously the effort was applied
/// only to the local stand-in provider, so the server kept its configured
/// default (low by default) and silently ran the new model at low effort.
#[test]
fn forward_pending_reasoning_effort_sends_effort_request_to_server() {
    use tokio::io::AsyncBufReadExt;

    let mut app = create_test_app();
    app.is_remote = true;
    app.pending_reasoning_effort = Some("high".to_string());

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let line = rt.block_on(async {
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        let peer = remote
            .take_dummy_peer()
            .expect("dummy remote should retain peer stream");
        let (reader, _writer) = peer.into_split();
        let mut reader = tokio::io::BufReader::new(reader);

        super::forward_pending_reasoning_effort(&mut app, &mut remote).await;

        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .expect("effort request should be readable by peer");
        line
    });

    match serde_json::from_str::<crate::protocol::Request>(&line)
        .expect("effort request should deserialize")
    {
        crate::protocol::Request::SetReasoningEffort { effort, .. } => {
            assert_eq!(effort, "high", "the picker-selected effort must be sent");
        }
        other => panic!("expected SetReasoningEffort request, got {:?}", other),
    }

    assert!(
        app.pending_reasoning_effort.is_none(),
        "staged effort must be consumed after dispatch"
    );
    assert_eq!(
        app.remote_reasoning_effort.as_deref(),
        Some("high"),
        "requested effort should be tracked optimistically for the UI"
    );
}

/// The dispatcher must be a no-op when no effort variant was staged (plain
/// model rows without an effort suffix).
#[test]
fn forward_pending_reasoning_effort_is_noop_without_staged_effort() {
    let mut app = create_test_app();
    app.is_remote = true;
    assert!(app.pending_reasoning_effort.is_none());

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        super::forward_pending_reasoning_effort(&mut app, &mut remote).await;
    });

    assert!(app.remote_reasoning_effort.is_none());
    assert!(app.pending_reasoning_effort.is_none());
}

#[test]
fn remote_dropped_file_path_is_sent_as_a_prompt_not_a_slash_command() {
    // Regression: a terminal file drop like `/tmp/shot.png` starts with `/`, so
    // remote submit routed it to `submit_remote_slash_input`, which fell back to
    // `App::submit_input`. That only sets `pending_turn`, which no remote run
    // loop consumes, so the client hung in "Sending" forever.
    let temp = tempfile::tempdir().expect("create temp dir");
    let file = temp.path().join("shot.png");
    std::fs::write(&file, b"x").expect("write file");
    let dropped = file.to_string_lossy().to_string();

    let mut app = create_test_app();
    app.is_remote = true;
    app.runtime_mode = crate::tui::app::AppRuntimeMode::RemoteClient;

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();
    rt.block_on(crate::tui::app::remote::submit_remote_slash_input(
        &mut app,
        &mut remote,
        crate::tui::app::input::PreparedInput {
            raw_input: dropped.clone(),
            expanded: dropped.clone(),
            images: vec![],
        },
    ))
    .expect("dropped path should send as a normal remote turn");

    assert!(
        app.active_skill.is_none(),
        "a file path must never activate a skill"
    );
    assert!(
        app.is_processing,
        "a dropped path must start a remote turn instead of stranding pending_turn"
    );
    assert!(
        !app.pending_turn,
        "remote submissions must never park on the local-only pending_turn flag"
    );
}

#[test]
fn remote_submit_input_never_strands_a_local_pending_turn() {
    // Safety net: any path that reaches `App::submit_input` while attached to a
    // remote session must queue for the remote tick loop rather than set
    // `pending_turn`, which only the local run loop consumes.
    let mut app = create_test_app();
    app.is_remote = true;
    app.runtime_mode = crate::tui::app::AppRuntimeMode::RemoteClient;
    app.input = "plain prompt".to_string();
    app.cursor_pos = app.input.len();

    app.submit_input();

    assert!(
        !app.pending_turn,
        "remote submit_input must not set the local-only pending_turn flag"
    );
    assert_eq!(
        app.queued_messages,
        vec!["plain prompt".to_string()],
        "the prompt should be queued for the remote tick loop"
    );
}

// Public-wire admission regression fixtures. Dummy uses an isolated socket pair.
fn resume_authority_history(
    session_id: &str,
    directive: bool,
    interrupted: Option<bool>,
) -> ServerEvent {
    crate::protocol::ServerEvent::History {
        id: 1,
        session_id: session_id.to_string(),
        messages: vec![crate::protocol::HistoryMessage {
            role: "assistant".to_string(),
            content: "Reconnect me from server history".to_string(),
            tool_calls: None,
            tool_data: None,
        }],
        images: vec![],
        provider_name: Some("claude".to_string()),
        provider_model: Some("claude-sonnet-4-20250514".to_string()),
        subagent_model: None,
        autoreview_enabled: None,
        autojudge_enabled: None,
        available_models: vec![],
        available_model_routes: vec![],
        mcp_servers: vec![],
        skills: vec![],
        total_tokens: None,
        token_usage_totals: None,
        all_sessions: vec![],
        client_count: None,
        is_canary: None,
        server_version: None,
        server_name: None,
        server_icon: None,
        server_has_update: None,
        was_interrupted: interrupted,
        reload_recovery: directive.then(|| crate::protocol::ReloadRecoverySnapshot {
            reconnect_notice: Some("Reloaded with build srv1234".to_string()),
            continuation_message: "Server-owned reload continuation".to_string(),
        }),
        connection_type: Some("websocket".to_string()),
        status_detail: None,
        upstream_provider: None,
        resolved_credential: None,
        reasoning_effort: None,
        service_tier: None,
        compaction_mode: crate::config::CompactionMode::Reactive,
        activity: None,
        side_panel: crate::side_panel::SidePanelSnapshot::default(),
    }
}

async fn resume_authority_drain(
    peer: &mut tokio::net::UnixStream,
) -> Vec<crate::protocol::Request> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    let mut buf = [0; 16384];
    while let Ok(Ok(n)) =
        tokio::time::timeout(std::time::Duration::from_millis(20), peer.read(&mut buf)).await
    {
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
    }
    String::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn resume_authority_turns(requests: &[crate::protocol::Request]) -> usize {
    requests
        .iter()
        .filter(|r| {
            matches!(
                r,
                crate::protocol::Request::Message { .. }
                    | crate::protocol::Request::SoftInterrupt { .. }
                    | crate::protocol::Request::ResumeAllSessions { .. }
            )
        })
        .count()
}

#[test]
fn resume_authority_passive_history_directive_is_display_only() {
    resume_authority_passive_history(true, None);
}

#[test]
fn resume_authority_passive_legacy_interruption_is_display_only() {
    for interrupted in [Some(true), Some(false), None] {
        resume_authority_passive_history(false, interrupted);
    }
}

fn resume_authority_passive_history(directive: bool, interrupted: Option<bool>) {
    let _env = crate::storage::lock_test_env();
    let _env = ResumeAuthorityEnv::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let requests = rt.block_on(async {
        let (mut app, calls) = resume_authority_app("passive");
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        let mut peer = remote.take_dummy_peer().unwrap();
        handle_server_event(
            &mut app,
            resume_authority_history("passive", directive, interrupted),
            &mut remote,
        );
        assert!(
            app.display_messages()
                .iter()
                .any(|m| m.content == "Reconnect me from server history")
        );
        handle_tick(&mut app, &mut remote).await;
        process_remote_followups(&mut app, &mut remote).await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        resume_authority_drain(&mut peer).await
    });
    assert_eq!(
        resume_authority_turns(&requests),
        0,
        "passive History originated a turn: {requests:?}"
    );
}

#[test]
fn explicit_remote_server_reload_arms_exact_session_and_write_failure_restores_prior_scope() {
    use crossterm::event::{KeyCode, KeyModifiers};

    let _lock = crate::storage::lock_test_env();
    let _env = ResumeAuthorityEnv::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let (mut app, _) = resume_authority_app("reload-session");
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        app.input = "/server-reload".into();
        app.handle_remote_key(KeyCode::Enter, KeyModifiers::NONE, &mut remote)
            .await
            .unwrap();
        assert!(app.reload_recovery_is_authorized("reload-session"));

        app.authorize_reload_recovery("prior-session");
        app.input = "/server-reload".into();
        let peer = remote.take_dummy_peer().unwrap();
        drop(peer);
        assert!(
            app.handle_remote_key(KeyCode::Enter, KeyModifiers::NONE, &mut remote)
                .await
                .is_err()
        );
        assert!(app.reload_recovery_is_authorized("prior-session"));
    });
}

#[test]
fn live_reloading_arms_only_active_turn_once() {
    let _lock = crate::storage::lock_test_env();
    let _env = ResumeAuthorityEnv::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let (mut app, _) = resume_authority_app("live-session");
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        let mut state = reconnect::RemoteRunState::default();

        super::handle_remote_event(
            &mut app,
            &mut terminal,
            &mut remote,
            &mut state,
            crate::tui::backend::RemoteRead::Event(ServerEvent::Reloading { new_socket: None }),
        )
        .await
        .unwrap();
        assert!(!app.reload_recovery_is_authorized("live-session"));

        app.is_processing = true;
        app.remote_session_id = Some("live-session".into());
        state.server_reload_in_progress = false;
        super::handle_remote_event(
            &mut app,
            &mut terminal,
            &mut remote,
            &mut state,
            crate::tui::backend::RemoteRead::Event(ServerEvent::Reloading { new_socket: None }),
        )
        .await
        .unwrap();
        assert!(app.reload_recovery_is_authorized("live-session"));
        app.reload_recovery_authorized_session = None;
        super::handle_remote_event(
            &mut app,
            &mut terminal,
            &mut remote,
            &mut state,
            crate::tui::backend::RemoteRead::Event(ServerEvent::Reloading { new_socket: None }),
        )
        .await
        .unwrap();
        assert!(!app.reload_recovery_is_authorized("live-session"));
    });
}

/// Restore every process input even if an assertion panics.
struct ResumeAuthorityEnv {
    _home: tempfile::TempDir,
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}
impl ResumeAuthorityEnv {
    fn new() -> Self {
        let home = tempfile::TempDir::new().unwrap();
        let previous = [
            "JCODE_HOME",
            "JCODE_RUNTIME_DIR",
            "JCODE_RELOAD_RECOVERY_SESSION",
            "JCODE_RELOAD_FAST_START",
            "JCODE_SSH_REMOTE",
        ]
        .into_iter()
        .map(|key| (key, std::env::var_os(key)))
        .collect();
        crate::env::set_var("JCODE_HOME", home.path());
        crate::env::set_var("JCODE_RUNTIME_DIR", home.path());
        crate::env::remove_var("JCODE_RELOAD_RECOVERY_SESSION");
        crate::env::remove_var("JCODE_RELOAD_FAST_START");
        crate::env::remove_var("JCODE_SSH_REMOTE");
        Self {
            _home: home,
            previous,
        }
    }
}
impl Drop for ResumeAuthorityEnv {
    fn drop(&mut self) {
        for (key, value) in &self.previous {
            if let Some(value) = value {
                crate::env::set_var(key, value);
            } else {
                crate::env::remove_var(key);
            }
        }
    }
}

struct ResumeAuthorityProvider(Arc<std::sync::atomic::AtomicUsize>);
#[async_trait::async_trait]
impl Provider for ResumeAuthorityProvider {
    async fn complete(
        &self,
        _: &[crate::message::Message],
        _: &[crate::message::ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(anyhow::anyhow!(
            "isolated counting provider: unexpected completion"
        ))
    }
    fn name(&self) -> &str {
        "mock"
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self(self.0.clone()))
    }
}

fn resume_authority_app(
    session: &str,
) -> (crate::tui::app::App, Arc<std::sync::atomic::AtomicUsize>) {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut app = crate::tui::app::App::new_for_remote(Some(session.into()));
    app.provider = Arc::new(ResumeAuthorityProvider(calls.clone()));
    app.auto_server_reload = false;
    (app, calls)
}

async fn resume_authority_dispatch(
    app: &mut crate::tui::app::App,
    remote: &mut crate::tui::backend::RemoteConnection,
) {
    handle_tick(app, remote).await;
    process_remote_followups(app, remote).await;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| crate::tui::ui::draw(frame, app))
        .unwrap();
}

fn resume_authority_context(session: &str) -> crate::tool::selfdev::ReloadContext {
    let ctx = crate::tool::selfdev::ReloadContext {
        task_context: Some("intentional reload work".into()),
        version_before: "old".into(),
        version_after: "new".into(),
        session_id: session.into(),
        timestamp: chrono::Utc::now().to_rfc3339(),
    };
    ctx.save().unwrap();
    ctx
}

#[test]
fn resume_authority_duplicate_history_after_done_does_not_rearm() {
    let _lock = crate::storage::lock_test_env();
    let _env = ResumeAuthorityEnv::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        for directive in [true, false] {
            let (mut app, calls) = resume_authority_app("A");
            app.authorize_reload_recovery("A");
            let mut remote = crate::tui::backend::RemoteConnection::dummy();
            let mut peer = remote.take_dummy_peer().unwrap();
            let history = resume_authority_history("A", directive, Some(true));
            handle_server_event(&mut app, history.clone(), &mut remote);
            resume_authority_dispatch(&mut app, &mut remote).await;
            let first = resume_authority_drain(&mut peer).await;
            assert_eq!(resume_authority_turns(&first), 1, "{first:?}");
            assert_eq!(remote.session_id(), Some("A"));
            let id = app.current_message_id.unwrap();
            handle_server_event(&mut app, ServerEvent::Done { id }, &mut remote);
            handle_server_event(&mut app, history, &mut remote);
            resume_authority_dispatch(&mut app, &mut remote).await;
            assert_eq!(
                resume_authority_turns(&resume_authority_drain(&mut peer).await),
                0
            );
            assert!(!app.reload_recovery_is_authorized("A"));
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        }
    });
}

#[test]
fn resume_authority_wrong_session_history_cannot_spend_token() {
    let _lock = crate::storage::lock_test_env();
    let _env = ResumeAuthorityEnv::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let (mut app, _) = resume_authority_app("A");
        app.authorize_reload_recovery("A");
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        let mut peer = remote.take_dummy_peer().unwrap();
        handle_server_event(
            &mut app,
            resume_authority_history("B", true, None),
            &mut remote,
        );
        resume_authority_dispatch(&mut app, &mut remote).await;
        assert_eq!(
            resume_authority_turns(&resume_authority_drain(&mut peer).await),
            0
        );
        assert!(app.reload_recovery_is_authorized("A"));
        handle_server_event(
            &mut app,
            resume_authority_history("A", true, None),
            &mut remote,
        );
        resume_authority_dispatch(&mut app, &mut remote).await;
        assert_eq!(
            resume_authority_turns(&resume_authority_drain(&mut peer).await),
            1
        );
        app.authorize_reload_recovery("A");
        handle_server_event(
            &mut app,
            ServerEvent::SessionId {
                session_id: "B".into(),
            },
            &mut remote,
        );
        assert!(!app.reload_recovery_is_authorized("A"));
    });
}

#[test]
fn resume_authority_passive_stale_local_context_is_display_only() {
    let _lock = crate::storage::lock_test_env();
    let _env = ResumeAuthorityEnv::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        for loaded in [false, true] {
            for marker in [false, true] {
                for reconnect in [false, true] {
                    let ctx = resume_authority_context("passive-context");
                    let (mut app, calls) = resume_authority_app("passive-context");
                    let mut remote = crate::tui::backend::RemoteConnection::dummy();
                    let mut peer = remote.take_dummy_peer().unwrap();
                    if loaded {
                        handle_server_event(
                            &mut app,
                            resume_authority_history("passive-context", true, Some(true)),
                            &mut remote,
                        );
                    }
                    app.is_processing = true;
                    app.status = crate::tui::app::ProcessingStatus::RunningTool("existing".into());
                    app.remote_resume_activity = Some(crate::tui::app::RemoteResumeActivity {
                        session_id: "passive-context".into(),
                        observed_at: std::time::Instant::now(),
                        current_tool_name: Some("existing".into()),
                    });
                    reconnect::finalize_reload_reconnect(
                        &mut app,
                        Some("passive-context"),
                        reconnect::ReloadReconnectHints {
                            reload_ctx_for_session: Some(ctx),
                            has_client_reload_marker: marker,
                        },
                        reconnect,
                    );
                    if marker {
                        std::fs::write(
                            crate::storage::jcode_dir()
                                .unwrap()
                                .join("client-reload-pending-passive-context"),
                            "stale visual marker",
                        )
                        .unwrap();
                    }
                    let mut terminal =
                        ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
                    let mut state = RemoteRunState {
                        reconnect_attempts: usize::from(reconnect) as u32,
                        ..Default::default()
                    };
                    let outcome = handle_post_connect(
                        &mut app,
                        &mut terminal,
                        &mut remote,
                        &mut state,
                        Some("passive-context"),
                    )
                    .await
                    .unwrap();
                    assert!(matches!(outcome, super::PostConnectOutcome::Ready));
                    assert!(app.is_processing);
                    assert!(app.remote_resume_activity.is_some());
                    assert!(
                        crate::tool::selfdev::ReloadContext::peek_for_session("passive-context")
                            .unwrap()
                            .is_some()
                    );
                    resume_authority_dispatch(&mut app, &mut remote).await;
                    assert_eq!(
                        resume_authority_turns(&resume_authority_drain(&mut peer).await),
                        0
                    );
                    // Existing work finishes. A hidden continuation must not spring to life.
                    app.is_processing = false;
                    app.remote_resume_activity = None;
                    remote.mark_history_loaded();
                    resume_authority_dispatch(&mut app, &mut remote).await;
                    assert_eq!(
                        resume_authority_turns(&resume_authority_drain(&mut peer).await),
                        0
                    );
                    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
                }
            }
        }
    });
}

#[test]
fn resume_authority_reexec_handoff_is_one_shot_and_session_scoped() {
    let _lock = crate::storage::lock_test_env();
    let _env = ResumeAuthorityEnv::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let (mut producer, _) = resume_authority_app("A");
        producer.authorize_reload_recovery("A");
        assert!(producer.take_reload_recovery_handoff(Some("B")).is_none());
        assert!(!producer.reload_recovery_is_authorized("A"));
        producer.authorize_reload_recovery("A");
        let handoff = producer.take_reload_recovery_handoff(Some("A")).unwrap();
        assert!(producer.take_reload_recovery_handoff(Some("A")).is_none());
        crate::env::set_var("JCODE_RELOAD_RECOVERY_SESSION", &handoff);
        crate::env::set_var("JCODE_RELOAD_FAST_START", "1");
        let (mut app, _) = resume_authority_app("A");
        assert!(app.reload_recovery_is_authorized("A"));
        assert!(std::env::var_os("JCODE_RELOAD_RECOVERY_SESSION").is_none());
        let (second, _) = resume_authority_app("A");
        assert!(!second.reload_recovery_is_authorized("A"));
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        let mut peer = remote.take_dummy_peer().unwrap();
        handle_server_event(
            &mut app,
            resume_authority_history("A", true, None),
            &mut remote,
        );
        resume_authority_dispatch(&mut app, &mut remote).await;
        assert_eq!(
            resume_authority_turns(&resume_authority_drain(&mut peer).await),
            1
        );
        crate::env::set_var("JCODE_RELOAD_RECOVERY_SESSION", &handoff);
        let (other, _) = resume_authority_app("B");
        assert!(!other.reload_recovery_is_authorized("A"));
        assert!(!other.reload_recovery_is_authorized("B"));
        assert!(std::env::var_os("JCODE_RELOAD_RECOVERY_SESSION").is_none());
    });
}

#[test]
fn resume_authority_ssh_discards_laptop_handoff_before_history() {
    let _lock = crate::storage::lock_test_env();
    let _env = ResumeAuthorityEnv::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        crate::env::set_var("JCODE_SSH_REMOTE", "isolated-host");
        crate::env::set_var("JCODE_RELOAD_RECOVERY_SESSION", "A");
        let (mut app, calls) = resume_authority_app("A");
        assert!(!app.reload_recovery_is_authorized("A"));
        assert!(std::env::var_os("JCODE_RELOAD_RECOVERY_SESSION").is_none());
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        let mut peer = remote.take_dummy_peer().unwrap();
        handle_server_event(
            &mut app,
            resume_authority_history("A", true, Some(true)),
            &mut remote,
        );
        resume_authority_dispatch(&mut app, &mut remote).await;
        assert_eq!(
            resume_authority_turns(&resume_authority_drain(&mut peer).await),
            0
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    });
}

#[test]
fn resume_authority_local_restore_is_passive_unless_authorized() {
    let _lock = crate::storage::lock_test_env();
    let _env = ResumeAuthorityEnv::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        for authorized in [false, true] {
            let mut session = crate::session::Session::create_with_id("local-A".into(), None, None);
            session.add_message(
                crate::message::Role::Assistant,
                vec![crate::message::ContentBlock::Text {
                    text: "persisted local transcript [generation interrupted - server reloading]"
                        .into(),
                    cache_control: None,
                }],
            );
            session.save().unwrap();
            resume_authority_context("local-A");
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let provider: Arc<dyn Provider> = Arc::new(ResumeAuthorityProvider(calls.clone()));
            let mut app = crate::tui::app::App::new_minimal_with_session(
                provider,
                crate::tool::Registry::empty(),
                crate::session::Session::create(None, None),
            );
            if authorized {
                app.authorize_reload_recovery("local-A");
            }
            app.restore_session("local-A");
            assert!(
                app.display_messages()
                    .iter()
                    .any(|m| m.content.contains("persisted local transcript"))
            );
            crate::tui::app::local::handle_tick(&mut app);
            assert_eq!(app.pending_turn, authorized);
            assert_eq!(
                crate::tool::selfdev::ReloadContext::peek_for_session("local-A")
                    .unwrap()
                    .is_some(),
                !authorized
            );
            assert!(!app.reload_recovery_is_authorized("local-A"));
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
            // Exercise the same hidden followup payload through the real wire
            // dispatcher without entering a user's terminal or real provider.
            let mut remote = crate::tui::backend::RemoteConnection::dummy();
            remote.mark_history_loaded();
            let mut peer = remote.take_dummy_peer().unwrap();
            resume_authority_dispatch(&mut app, &mut remote).await;
            assert_eq!(
                resume_authority_turns(&resume_authority_drain(&mut peer).await),
                usize::from(authorized)
            );
        }
    });
}

#[test]
fn resume_authority_intentional_queued_prompts_survive_passive_attach() {
    let _lock = crate::storage::lock_test_env();
    let _env = ResumeAuthorityEnv::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        for kind in [
            "queue",
            "hidden",
            "submit",
            "draft",
            "soft-interrupt",
            "retry",
        ] {
            let (mut app, calls) = resume_authority_app("A");
            match kind {
                "queue" => app.queued_messages.push("intentional user prompt".into()),
                "hidden" => app
                    .hidden_queued_system_messages
                    .push("intentional startup reminder".into()),
                "soft-interrupt" => {
                    app.pending_soft_interrupts
                        .push("intentional user prompt".into());
                    app.pending_soft_interrupt_requests
                        .push((55, "intentional user prompt".into()));
                }
                "retry" => {
                    app.rate_limit_pending_message = Some(crate::tui::app::PendingRemoteMessage {
                        content: "intentional user prompt".into(),
                        images: vec![],
                        is_system: false,
                        system_reminder: None,
                        auto_retry: true,
                        retry_attempts: 1,
                        retry_at: None,
                    });
                    app.rate_limit_reset = Some(std::time::Instant::now());
                }
                _ => {
                    app.input = "intentional composer text".into();
                    app.submit_input_on_startup = kind == "submit";
                }
            }
            if kind == "submit" {
                crate::tui::app::App::save_startup_submission_for_session(
                    "A",
                    "intentional composer text".into(),
                    vec![("image/png".into(), "aW1hZ2U=".into())],
                );
            } else {
                app.save_input_for_reload("A");
            }
            let (mut app, restored_calls) = resume_authority_app("A");
            // A scheduled retry belongs to the already attached session.
            // Bootstrap retry migration is independent of recovery admission.
            if kind == "retry" {
                app.remote_session_id = Some("A".into());
            }
            let mut remote = crate::tui::backend::RemoteConnection::dummy();
            let mut peer = remote.take_dummy_peer().unwrap();
            handle_server_event(
                &mut app,
                resume_authority_history("A", true, Some(true)),
                &mut remote,
            );
            resume_authority_dispatch(&mut app, &mut remote).await;
            let requests = resume_authority_drain(&mut peer).await;
            assert_eq!(
                resume_authority_turns(&requests),
                usize::from(kind != "draft"),
                "{kind}: {requests:?}"
            );
            for request in requests {
                if let crate::protocol::Request::Message {
                    content,
                    images,
                    system_reminder,
                    ..
                } = request
                {
                    let expected = match kind {
                        "queue" | "soft-interrupt" | "retry" => "intentional user prompt",
                        "hidden" => "",
                        _ => "intentional composer text",
                    };
                    assert_eq!(content, expected);
                    assert_eq!(images.len(), usize::from(kind == "submit"));
                    assert_eq!(
                        system_reminder.as_deref(),
                        (kind == "hidden").then_some("intentional startup reminder")
                    );
                }
            }
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert_eq!(restored_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        }
    });
}

#[test]
fn resume_authority_send_failure_preserves_authorized_work() {
    let _lock = crate::storage::lock_test_env();
    let _env = ResumeAuthorityEnv::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let (mut app, _) = resume_authority_app("A");
        app.authorize_reload_recovery("A");
        let mut broken = crate::tui::backend::RemoteConnection::dummy();
        drop(broken.take_dummy_peer().unwrap());
        handle_server_event(
            &mut app,
            resume_authority_history("A", true, None),
            &mut broken,
        );
        resume_authority_dispatch(&mut app, &mut broken).await;
        assert!(!app.reload_recovery_is_authorized("A"));
        assert_eq!(
            app.hidden_queued_system_messages,
            ["Server-owned reload continuation"]
        );
        handle_server_event(
            &mut app,
            resume_authority_history("A", true, None),
            &mut broken,
        );
        assert_eq!(
            app.hidden_queued_system_messages,
            ["Server-owned reload continuation"]
        );
        let mut retry = crate::tui::backend::RemoteConnection::dummy();
        retry.mark_history_loaded();
        let mut peer = retry.take_dummy_peer().unwrap();
        resume_authority_dispatch(&mut app, &mut retry).await;
        let frames = resume_authority_drain(&mut peer).await;
        assert_eq!(resume_authority_turns(&frames), 1, "{frames:?}");
    });
}

#[test]
fn resume_authority_intentional_reload_consumer_order_recovers_once() {
    let _lock = crate::storage::lock_test_env();
    let _env = ResumeAuthorityEnv::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        for history_first in [false, true] {
            let ctx = resume_authority_context("A");
            let (mut app, _) = resume_authority_app("A");
            app.authorize_reload_recovery("A");
            let mut remote = crate::tui::backend::RemoteConnection::dummy();
            let mut peer = remote.take_dummy_peer().unwrap();
            let history = resume_authority_history("A", true, Some(true));
            if history_first {
                handle_server_event(&mut app, history.clone(), &mut remote);
            }
            reconnect::finalize_reload_reconnect(
                &mut app,
                Some("A"),
                reconnect::ReloadReconnectHints {
                    reload_ctx_for_session: Some(ctx.clone()),
                    has_client_reload_marker: true,
                },
                true,
            );
            handle_server_event(&mut app, history.clone(), &mut remote);
            resume_authority_dispatch(&mut app, &mut remote).await;
            assert_eq!(
                resume_authority_turns(&resume_authority_drain(&mut peer).await),
                1
            );
            let id = app.current_message_id.unwrap();
            handle_server_event(&mut app, ServerEvent::Done { id }, &mut remote);
            reconnect::finalize_reload_reconnect(
                &mut app,
                Some("A"),
                reconnect::ReloadReconnectHints {
                    reload_ctx_for_session: Some(ctx),
                    has_client_reload_marker: true,
                },
                true,
            );
            handle_server_event(&mut app, history, &mut remote);
            resume_authority_dispatch(&mut app, &mut remote).await;
            assert_eq!(
                resume_authority_turns(&resume_authority_drain(&mut peer).await),
                0
            );
        }
    });
}

#[test]
fn resume_authority_admission_rejects_without_mutation_and_deduplicates_pending() {
    let _lock = crate::storage::lock_test_env();
    let _env = ResumeAuthorityEnv::new();
    let (mut app, _) = resume_authority_app("A");
    let directive = crate::protocol::ReloadRecoverySnapshot {
        reconnect_notice: Some("intentional notice".into()),
        continuation_message: "pending reminder".into(),
    };
    app.authorize_reload_recovery("A");
    app.hidden_queued_system_messages
        .push("pending reminder".into());
    app.is_processing = true;
    assert!(!app.admit_reload_recovery("B", directive.clone()));
    assert!(app.reload_recovery_is_authorized("A"));
    assert!(app.reload_info.is_empty());
    assert!(app.is_processing);
    assert!(app.admit_reload_recovery("A", directive.clone()));
    assert_eq!(app.hidden_queued_system_messages, ["pending reminder"]);
    assert!(!app.admit_reload_recovery("A", directive));
    assert!(app.is_processing);
    app.authorize_reload_recovery("A");
    assert!(app.take_reload_recovery_handoff(None).is_none());
    assert!(!app.reload_recovery_is_authorized("A"));
}
