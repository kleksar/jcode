#[test]
fn test_transcript_request_roundtrip() -> Result<()> {
    let req = Request::Transcript {
        id: 77,
        text: "hello from whisper".to_string(),
        mode: TranscriptMode::Send,
        session_id: Some("sess_abc".to_string()),
    };
    let json = serde_json::to_string(&req)?;
    assert!(json.contains("\"type\":\"transcript\""));
    let decoded = parse_request_json(&json)?;
    assert_eq!(decoded.id(), 77);
    let Request::Transcript {
        text,
        mode,
        session_id,
        ..
    } = decoded
    else {
        return Err(anyhow!("expected Transcript request"));
    };
    assert_eq!(text, "hello from whisper");
    assert_eq!(mode, TranscriptMode::Send);
    assert_eq!(session_id.as_deref(), Some("sess_abc"));
    Ok(())
}

#[test]
fn test_transcript_event_roundtrip() -> Result<()> {
    let event = ServerEvent::Transcript {
        text: "dictated text".to_string(),
        mode: TranscriptMode::Replace,
    };
    let json = encode_event(&event);
    assert!(json.contains("\"type\":\"transcript\""));
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::Transcript { text, mode } = decoded else {
        return Err(anyhow!("expected Transcript event"));
    };
    assert_eq!(text, "dictated text");
    assert_eq!(mode, TranscriptMode::Replace);
    Ok(())
}

#[test]
fn test_memory_activity_event_roundtrip() -> Result<()> {
    let event = ServerEvent::MemoryActivity {
        activity: MemoryActivitySnapshot {
            state: MemoryStateSnapshot::SidecarChecking { count: 3 },
            state_age_ms: 275,
            pipeline: Some(MemoryPipelineSnapshot {
                search: MemoryStepStatusSnapshot::Done,
                search_result: Some(MemoryStepResultSnapshot {
                    summary: "5 hits".to_string(),
                    latency_ms: 14,
                }),
                verify: MemoryStepStatusSnapshot::Running,
                verify_result: None,
                verify_progress: Some((1, 3)),
                inject: MemoryStepStatusSnapshot::Pending,
                inject_result: None,
                maintain: MemoryStepStatusSnapshot::Pending,
                maintain_result: None,
            }),
        },
    };

    let json = encode_event(&event);
    assert!(json.contains("\"type\":\"memory_activity\""));
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::MemoryActivity { activity } = decoded else {
        return Err(anyhow!("expected MemoryActivity event"));
    };
    assert_eq!(
        activity.state,
        MemoryStateSnapshot::SidecarChecking { count: 3 }
    );
    assert_eq!(activity.state_age_ms, 275);
    let pipeline = activity
        .pipeline
        .ok_or_else(|| anyhow!("pipeline snapshot"))?;
    assert_eq!(pipeline.search, MemoryStepStatusSnapshot::Done);
    assert_eq!(pipeline.verify, MemoryStepStatusSnapshot::Running);
    assert_eq!(pipeline.verify_progress, Some((1, 3)));
    Ok(())
}

#[test]
fn test_input_shell_request_roundtrip() -> Result<()> {
    let req = Request::InputShell {
        id: 88,
        command: "ls -la".to_string(),
    };
    let json = serde_json::to_string(&req)?;
    assert!(json.contains("\"type\":\"input_shell\""));
    let decoded = parse_request_json(&json)?;
    assert_eq!(decoded.id(), 88);
    let Request::InputShell { id, command } = decoded else {
        return Err(anyhow!("expected InputShell request"));
    };
    assert_eq!(id, 88);
    assert_eq!(command, "ls -la");
    Ok(())
}

#[test]
fn test_input_shell_result_event_roundtrip() -> Result<()> {
    let event = ServerEvent::InputShellResult {
        result: jcode_message_types::InputShellResult {
            command: "pwd".to_string(),
            cwd: Some("/tmp/project".to_string()),
            output: "/tmp/project\n".to_string(),
            exit_code: Some(0),
            duration_ms: 7,
            truncated: false,
            failed_to_start: false,
        },
    };
    let json = encode_event(&event);
    assert!(json.contains("\"type\":\"input_shell_result\""));
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::InputShellResult { result } = decoded else {
        return Err(anyhow!("expected InputShellResult event"));
    };
    assert_eq!(result.command, "pwd");
    assert_eq!(result.cwd.as_deref(), Some("/tmp/project"));
    assert_eq!(result.exit_code, Some(0));
    Ok(())
}

#[test]
fn test_protocol_enum_roundtrips_cover_wire_names() -> Result<()> {
    let transcript_modes = [
        (TranscriptMode::Insert, "insert"),
        (TranscriptMode::Append, "append"),
        (TranscriptMode::Replace, "replace"),
        (TranscriptMode::Send, "send"),
    ];
    for (mode, wire) in transcript_modes {
        let json = serde_json::to_string(&mode)?;
        assert_eq!(json, format!("\"{}\"", wire));
        let decoded: TranscriptMode = serde_json::from_str(&json)?;
        assert_eq!(decoded, mode);
    }

    let delivery_modes = [
        (CommDeliveryMode::Notify, "notify"),
        (CommDeliveryMode::Interrupt, "interrupt"),
        (CommDeliveryMode::Wake, "wake"),
    ];
    for (mode, wire) in delivery_modes {
        let json = serde_json::to_string(&mode)?;
        assert_eq!(json, format!("\"{}\"", wire));
        let decoded: CommDeliveryMode = serde_json::from_str(&json)?;
        assert_eq!(decoded, mode);
    }

    let feature_toggles = [
        (FeatureToggle::Memory, "memory"),
        (FeatureToggle::Swarm, "swarm"),
        (FeatureToggle::Autoreview, "autoreview"),
        (FeatureToggle::Autojudge, "autojudge"),
    ];
    for (feature, wire) in feature_toggles {
        let json = serde_json::to_string(&feature)?;
        assert_eq!(json, format!("\"{}\"", wire));
        let decoded: FeatureToggle = serde_json::from_str(&json)?;
        assert_eq!(decoded, feature);
    }

    Ok(())
}

#[test]
fn test_set_feature_roundtrip() -> Result<()> {
    let req = Request::SetFeature {
        id: 77,
        feature: FeatureToggle::Swarm,
        enabled: true,
    };
    let json = serde_json::to_string(&req)?;
    assert!(json.contains("\"type\":\"set_feature\""));
    let decoded = parse_request_json(&json)?;
    let Request::SetFeature {
        id,
        feature,
        enabled,
    } = decoded
    else {
        return Err(anyhow!("expected SetFeature"));
    };
    assert_eq!(id, 77);
    assert_eq!(feature, FeatureToggle::Swarm);
    assert!(enabled);
    Ok(())
}

#[test]
fn test_set_route_deserializes_as_set_model_compat_alias() -> Result<()> {
    // Legacy/desktop compatibility shape: a bare model string under the
    // `set_route` tag. `decode_request` (not raw serde) normalizes it.
    let decoded = decode_request(r#"{"type":"set_route","id":42,"model":"claude-opus-4-5"}"#)?;
    let Request::SetModel { id, model } = decoded else {
        return Err(anyhow!(
            "expected set_route compatibility alias to decode as SetModel"
        ));
    };
    assert_eq!(id, 42);
    assert_eq!(model, "claude-opus-4-5");
    Ok(())
}

#[test]
fn test_structured_set_route_decodes_as_set_route_not_set_model() -> Result<()> {
    // Regression for the "Invalid request: missing field `model`" bug seen when
    // switching models via the picker: a structured `set_route` request (with a
    // `selection` object, no `model` field) must decode as `Request::SetRoute`,
    // not be shadowed by the legacy `set_model` compatibility path.
    let request = Request::SetRoute {
        id: 7,
        selection: jcode_provider_core::RouteSelection {
            model: "gpt-5.5".to_string(),
            runtime_key: jcode_provider_core::RuntimeKey::OpenAIApiKey,
            api_method: "openai-api".to_string(),
            provider_label: "OpenAI".to_string(),
            detail: String::new(),
        },
    };
    let line = serde_json::to_string(&request)?;
    assert!(line.contains("\"type\":\"set_route\""));

    let decoded = decode_request(&line)?;
    let Request::SetRoute { id, selection } = decoded else {
        return Err(anyhow!(
            "expected structured set_route to decode as SetRoute, got {decoded:?}"
        ));
    };
    assert_eq!(id, 7);
    assert_eq!(selection.model, "gpt-5.5");
    Ok(())
}

#[test]
fn test_jcode_subscription_set_route_is_wire_safe() -> Result<()> {
    let request = Request::SetRoute {
        id: 8,
        selection: jcode_provider_core::RouteSelection {
            model: "gpt-5.5".to_string(),
            runtime_key: jcode_provider_core::RuntimeKey::JcodeSubscription,
            api_method: "jcode-subscription".to_string(),
            provider_label: "Jcode Subscription".to_string(),
            detail: "jcode subscription routing · managed server-side".to_string(),
        },
    };

    let line = serde_json::to_string(&request)?;
    assert!(line.contains("\"type\":\"set_route\""));
    assert!(line.contains("\"kind\":\"jcode-subscription\""));

    let decoded = decode_request(&line)?;
    let Request::SetRoute { id, selection } = decoded else {
        return Err(anyhow!("expected Jcode SetRoute, got {decoded:?}"));
    };
    assert_eq!(id, 8);
    assert_eq!(selection.model, "gpt-5.5");
    assert_eq!(
        selection.runtime_key,
        jcode_provider_core::RuntimeKey::JcodeSubscription
    );
    assert_eq!(selection.routed_model_spec(), "gpt-5.5");
    Ok(())
}

#[test]
fn test_subscribe_request_roundtrip_preserves_session_takeover_flags() -> Result<()> {
    let req = Request::Subscribe {
        id: 89,
        working_dir: Some("/tmp/project".to_string()),
        selfdev: Some(true),
        target_session_id: Some("sess_target".to_string()),
        client_instance_id: Some("client-123".to_string()),
        client_has_local_history: true,
        allow_session_takeover: true,
        crash_on_disconnect: true,
        continue_on_disconnect: true,
        terminal_env: vec![("ZELLIJ_SESSION_NAME".to_string(), "sessionB".to_string())],
    };
    let json = serde_json::to_string(&req)?;
    assert!(json.contains("\"type\":\"subscribe\""));
    let decoded = parse_request_json(&json)?;
    let Request::Subscribe {
        id,
        working_dir,
        selfdev,
        target_session_id,
        client_instance_id,
        client_has_local_history,
        allow_session_takeover,
        crash_on_disconnect,
        continue_on_disconnect,
        terminal_env,
    } = decoded
    else {
        return Err(anyhow!("expected Subscribe"));
    };
    assert_eq!(id, 89);
    assert_eq!(working_dir.as_deref(), Some("/tmp/project"));
    assert_eq!(selfdev, Some(true));
    assert_eq!(target_session_id.as_deref(), Some("sess_target"));
    assert_eq!(client_instance_id.as_deref(), Some("client-123"));
    assert!(client_has_local_history);
    assert!(allow_session_takeover);
    assert!(crash_on_disconnect);
    assert!(continue_on_disconnect);
    assert_eq!(
        terminal_env,
        vec![("ZELLIJ_SESSION_NAME".to_string(), "sessionB".to_string())]
    );
    Ok(())
}

#[test]
fn test_subscribe_request_defaults_optional_flags() -> Result<()> {
    let json = r#"{"type":"subscribe","id":91}"#;
    let decoded = parse_request_json(json)?;
    let Request::Subscribe {
        id,
        working_dir,
        selfdev,
        target_session_id,
        client_instance_id,
        client_has_local_history,
        allow_session_takeover,
        crash_on_disconnect,
        continue_on_disconnect,
        terminal_env,
    } = decoded
    else {
        return Err(anyhow!("expected Subscribe"));
    };
    assert_eq!(id, 91);
    assert_eq!(working_dir, None);
    assert_eq!(selfdev, None);
    assert_eq!(target_session_id, None);
    assert_eq!(client_instance_id, None);
    assert!(!client_has_local_history);
    assert!(!allow_session_takeover);
    assert!(!crash_on_disconnect);
    assert!(!continue_on_disconnect);
    assert!(terminal_env.is_empty());
    Ok(())
}

#[test]
fn test_resume_session_defaults_sync_flags() -> Result<()> {
    let json = r#"{"type":"resume_session","id":92,"session_id":"sess_resume"}"#;
    let decoded = parse_request_json(json)?;
    let Request::ResumeSession {
        id,
        session_id,
        client_instance_id,
        client_has_local_history,
        allow_session_takeover,
    } = decoded
    else {
        return Err(anyhow!("expected ResumeSession"));
    };
    assert_eq!(id, 92);
    assert_eq!(session_id, "sess_resume");
    assert_eq!(client_instance_id, None);
    assert!(!client_has_local_history);
    assert!(!allow_session_takeover);
    Ok(())
}

#[test]
fn test_close_session_request_roundtrip_preserves_id_and_session_id() -> Result<()> {
    let request = Request::CloseSession {
        id: 93,
        session_id: "sess_close".to_string(),
    };
    let json = serde_json::to_string(&request)?;
    assert!(json.contains("\"type\":\"close_session\""));
    let decoded = parse_request_json(&json)?;
    assert_eq!(decoded.id(), 93);
    let Request::CloseSession { id, session_id } = decoded else {
        return Err(anyhow!("expected CloseSession"));
    };
    assert_eq!(id, 93);
    assert_eq!(session_id, "sess_close");
    Ok(())
}

#[test]
fn test_session_closed_event_roundtrip_preserves_id_and_session_id() -> Result<()> {
    let event = ServerEvent::SessionClosed {
        id: 94,
        session_id: "sess_close".to_string(),
    };
    let json = serde_json::to_string(&event)?;
    assert!(json.contains("\"type\":\"session_closed\""));
    let decoded = parse_event_json(&json)?;
    let ServerEvent::SessionClosed { id, session_id } = decoded else {
        return Err(anyhow!("expected SessionClosed"));
    };
    assert_eq!(id, 94);
    assert_eq!(session_id, "sess_close");
    Ok(())
}

#[test]
fn test_resume_all_sessions_request_roundtrip() -> Result<()> {
    let req = Request::ResumeAllSessions { id: 451 };
    let json = serde_json::to_string(&req)?;
    assert!(json.contains("\"type\":\"resume_all_sessions\""));
    let decoded = parse_request_json(&json)?;
    let Request::ResumeAllSessions { id } = decoded else {
        return Err(anyhow!("expected ResumeAllSessions"));
    };
    assert_eq!(id, 451);
    Ok(())
}

#[test]
fn test_resume_all_result_event_roundtrip() -> Result<()> {
    let event = ServerEvent::ResumeAllResult {
        id: 451,
        resumed: 2,
        skipped: 1,
        resumed_sessions: vec!["fox".to_string(), "owl".to_string()],
        message: "Resuming 2 interrupted sessions: fox, owl.".to_string(),
    };
    let json = serde_json::to_string(&event)?;
    assert!(json.contains("\"type\":\"resume_all_result\""));
    let decoded = parse_event_json(&json)?;
    let ServerEvent::ResumeAllResult {
        id,
        resumed,
        skipped,
        resumed_sessions,
        message,
    } = decoded
    else {
        return Err(anyhow!("expected ResumeAllResult"));
    };
    assert_eq!(id, 451);
    assert_eq!(resumed, 2);
    assert_eq!(skipped, 1);
    assert_eq!(resumed_sessions, vec!["fox".to_string(), "owl".to_string()]);
    assert_eq!(message, "Resuming 2 interrupted sessions: fox, owl.");
    Ok(())
}

#[test]
fn test_message_request_roundtrip_preserves_images_and_system_reminder() -> Result<()> {
    let req = Request::Message {
        id: 88,
        content: "inspect this".to_string(),
        images: vec![
            ("image/png".to_string(), "AAA".to_string()),
            ("image/jpeg".to_string(), "BBB".to_string()),
        ],
        system_reminder: Some("be concise".to_string()),
        active_skill: Some("verification".to_string()),
        no_reply: true,
    };
    let json = serde_json::to_string(&req)?;
    let decoded = parse_request_json(&json)?;
    let Request::Message {
        id,
        content,
        images,
        system_reminder,
        active_skill,
        no_reply,
    } = decoded
    else {
        return Err(anyhow!("expected Message"));
    };
    assert_eq!(id, 88);
    assert_eq!(content, "inspect this");
    assert_eq!(images.len(), 2);
    assert_eq!(images[0].0, "image/png");
    assert_eq!(images[1].0, "image/jpeg");
    assert_eq!(system_reminder.as_deref(), Some("be concise"));
    assert_eq!(active_skill.as_deref(), Some("verification"));
    assert!(no_reply);
    Ok(())
}

#[test]
fn test_provider_guardrail_event_roundtrip() -> Result<()> {
    let event = ServerEvent::ProviderGuardrail {
        stop_reason: Some("refusal".to_string()),
        message: "Provider guardrail stopped the response".to_string(),
    };
    let json = encode_event(&event);
    assert!(json.contains("\"type\":\"provider_guardrail\""));
    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::ProviderGuardrail {
        stop_reason,
        message,
    } = decoded
    else {
        return Err(anyhow!("expected ProviderGuardrail event"));
    };
    assert_eq!(stop_reason.as_deref(), Some("refusal"));
    assert_eq!(message, "Provider guardrail stopped the response");

    // stop_reason is optional on the wire.
    let decoded = parse_event_json(r#"{"type":"provider_guardrail","message":"blocked"}"#)?;
    let ServerEvent::ProviderGuardrail { stop_reason, .. } = decoded else {
        return Err(anyhow!("expected ProviderGuardrail event"));
    };
    assert!(stop_reason.is_none());
    Ok(())
}

#[test]
fn test_message_end_carries_provider_stop_reason() -> Result<()> {
    // `max_tokens` is the only signal that the output budget truncated a turn.
    // Dropping it made a truncated benchmark run look like a clean one, so the
    // reason must survive the wire round trip.
    let event = ServerEvent::MessageEnd {
        stop_reason: Some("max_tokens".to_string()),
    };
    let json = encode_event(&event);
    assert!(json.contains("\"type\":\"message_end\""));
    assert!(json.contains("\"stop_reason\":\"max_tokens\""));

    let decoded = parse_event_json(json.trim())?;
    let ServerEvent::MessageEnd { stop_reason } = decoded else {
        return Err(anyhow!("expected MessageEnd event"));
    };
    assert_eq!(stop_reason.as_deref(), Some("max_tokens"));

    // Older producers omit the field entirely, which must still decode.
    let decoded = parse_event_json(r#"{"type":"message_end"}"#)?;
    let ServerEvent::MessageEnd { stop_reason } = decoded else {
        return Err(anyhow!("expected MessageEnd event"));
    };
    assert!(stop_reason.is_none());

    // A reasonless end-of-turn must not add noise to the wire.
    let json = encode_event(&ServerEvent::MessageEnd { stop_reason: None });
    assert!(!json.contains("stop_reason"), "unexpected field: {json}");
    Ok(())
}

#[test]
fn test_native_ssh_pong_capability_is_backward_compatible() -> Result<()> {
    let legacy: ServerEvent = serde_json::from_str(r#"{"type":"pong","id":7}"#)?;
    assert!(matches!(
        legacy,
        ServerEvent::Pong {
            id: 7,
            native_ssh_protocol: None,
            ..
        }
    ));
    let modern = ServerEvent::Pong {
        id: 7,
        native_ssh_protocol: Some(1),
        session_preview_protocol: None,
        clean_session_protocol: None,
        directory_completion_protocol: None,
    };
    let json = serde_json::to_value(&modern)?;
    assert_eq!(json["native_ssh_protocol"], 1);
    assert!(matches!(
        serde_json::from_value::<ServerEvent>(json)?,
        ServerEvent::Pong {
            id: 7,
            native_ssh_protocol: Some(1),
            ..
        }
    ));
    assert!(
        serde_json::to_value(&legacy)?
            .get("native_ssh_protocol")
            .is_none()
    );
    Ok(())
}

#[test]
fn test_session_preview_request_and_event_roundtrip() -> Result<()> {
    let request = Request::GetSessionPreview {
        id: 41,
        session_id: "session-preview".to_string(),
        limit: 20,
    };
    let request_json = serde_json::to_string(&request)?;
    assert!(request_json.contains("\"type\":\"get_session_preview\""));
    let decoded_request = parse_request_json(&request_json)?;
    assert_eq!(decoded_request.id(), 41);
    let Request::GetSessionPreview {
        id,
        session_id,
        limit,
    } = decoded_request
    else {
        return Err(anyhow!("expected GetSessionPreview request"));
    };
    assert_eq!(id, 41);
    assert_eq!(session_id, "session-preview");
    assert_eq!(limit, 20);

    let event = ServerEvent::SessionPreview {
        id: 41,
        session_id: "session-preview".to_string(),
        revision: 9,
        messages: vec![
            HistoryMessage {
                role: "user".to_string(),
                content: "Please inspect this.".to_string(),
                tool_calls: None,
                tool_data: None,
            },
            HistoryMessage {
                role: "tool".to_string(),
                content: "Inspection complete.".to_string(),
                tool_calls: Some(vec!["inspect".to_string()]),
                tool_data: None,
            },
        ],
        activity: SessionActivitySnapshot {
            is_processing: true,
            current_tool_name: Some("inspect".to_string()),
        },
    };
    let event_json = encode_event(&event);
    assert!(event_json.contains("\"type\":\"session_preview\""));
    let decoded_event = parse_event_json(event_json.trim())?;
    let ServerEvent::SessionPreview {
        id,
        session_id,
        revision,
        messages,
        activity,
    } = decoded_event
    else {
        return Err(anyhow!("expected SessionPreview event"));
    };
    assert_eq!(id, 41);
    assert_eq!(session_id, "session-preview");
    assert_eq!(revision, 9);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[0].content, "Please inspect this.");
    assert_eq!(messages[1].role, "tool");
    assert_eq!(
        messages[1].tool_calls.as_deref(),
        Some(&["inspect".to_string()][..])
    );
    assert!(activity.is_processing);
    assert_eq!(activity.current_tool_name.as_deref(), Some("inspect"));
    Ok(())
}

#[test]
fn test_session_preview_pong_capability_is_backward_compatible() -> Result<()> {
    let legacy: ServerEvent = serde_json::from_str(r#"{"type":"pong","id":7}"#)?;
    assert!(matches!(
        legacy,
        ServerEvent::Pong {
            id: 7,
            session_preview_protocol: None,
            ..
        }
    ));

    let modern = ServerEvent::Pong {
        id: 7,
        native_ssh_protocol: Some(1),
        session_preview_protocol: Some(1),
        clean_session_protocol: None,
        directory_completion_protocol: None,
    };
    let json = serde_json::to_value(&modern)?;
    assert_eq!(json["session_preview_protocol"], 1);
    assert!(matches!(
        serde_json::from_value::<ServerEvent>(json)?,
        ServerEvent::Pong {
            id: 7,
            session_preview_protocol: Some(1),
            ..
        }
    ));
    assert!(
        serde_json::to_value(&legacy)?
            .get("session_preview_protocol")
            .is_none()
    );
    Ok(())
}

#[test]
fn test_clean_session_protocol_pong_capability_is_backward_compatible() -> Result<()> {
    let legacy: ServerEvent = serde_json::from_str(r#"{"type":"pong","id":7}"#)?;
    assert!(matches!(
        legacy,
        ServerEvent::Pong {
            id: 7,
            clean_session_protocol: None,
            directory_completion_protocol: None,
            ..
        }
    ));

    let modern = ServerEvent::Pong {
        id: 7,
        native_ssh_protocol: Some(1),
        session_preview_protocol: Some(1),
        clean_session_protocol: Some(1),
        directory_completion_protocol: Some(1),
    };
    let json = serde_json::to_value(&modern)?;
    assert_eq!(json["clean_session_protocol"], 1);
    assert_eq!(json["directory_completion_protocol"], 1);
    assert!(matches!(
        serde_json::from_value::<ServerEvent>(json)?,
        ServerEvent::Pong {
            id: 7,
            clean_session_protocol: Some(1),
            directory_completion_protocol: Some(1),
            ..
        }
    ));
    assert!(
        serde_json::to_value(&legacy)?
            .get("clean_session_protocol")
            .is_none()
    );
    Ok(())
}

#[test]
fn test_session_creation_wire_roundtrip() -> Result<()> {
    let runtime = SessionRuntimeSelection {
        provider_key: Some("openai".to_string()),
        model: Some("gpt-5".to_string()),
        route_api_method: Some("responses".to_string()),
        reasoning_effort: Some("high".to_string()),
    };
    let runtime_json = serde_json::to_value(&runtime)?;
    assert_eq!(
        serde_json::from_value::<SessionRuntimeSelection>(runtime_json)?,
        runtime
    );
    assert_eq!(
        serde_json::to_value(SessionRuntimeSelection::default())?,
        serde_json::json!({})
    );

    let requests = [
        Request::GetSessionCreationContext { id: 51 },
        Request::ResolveWorkingDirectory {
            id: 52,
            path: "~/project".to_string(),
        },
        Request::CompleteWorkingDirectory {
            id: 53,
            path: "~/pro".to_string(),
            limit: 32,
        },
        Request::CreateSession {
            id: 54,
            working_dir: "/server/project".to_string(),
            runtime: runtime.clone(),
        },
    ];
    for request in requests {
        let id = request.id();
        let json = serde_json::to_string(&request)?;
        assert_eq!(parse_request_json(&json)?.id(), id);
    }

    let context = ServerEvent::SessionCreationContext {
        id: 51,
        home_dir: "/server/home".to_string(),
        recent_working_dirs: vec!["/server/newest".to_string(), "/server/older".to_string()],
    };
    assert!(matches!(
        parse_event_json(encode_event(&context).trim())?,
        ServerEvent::SessionCreationContext { id: 51, home_dir, recent_working_dirs }
            if home_dir == "/server/home"
                && recent_working_dirs == ["/server/newest", "/server/older"]
    ));

    let resolved = ServerEvent::WorkingDirectoryResolved {
        id: 52,
        input: "~/project".to_string(),
        absolute_path: "/server/home/project".to_string(),
    };
    assert!(matches!(
        parse_event_json(encode_event(&resolved).trim())?,
        ServerEvent::WorkingDirectoryResolved { id: 52, input, absolute_path }
            if input == "~/project" && absolute_path == "/server/home/project"
    ));

    let completions = ServerEvent::WorkingDirectoryCompletions {
        id: 53,
        input: "~/pro".to_string(),
        candidates: vec!["~/project/".to_string()],
        truncated: false,
    };
    assert!(matches!(
        parse_event_json(encode_event(&completions).trim())?,
        ServerEvent::WorkingDirectoryCompletions { id: 53, input, candidates, truncated: false }
            if input == "~/pro" && candidates == ["~/project/"]
    ));

    let created = ServerEvent::SessionCreated {
        id: 53,
        session_id: "created-session".to_string(),
        session_name: "Created session".to_string(),
        working_dir: "/server/project".to_string(),
    };
    assert!(matches!(
        parse_event_json(encode_event(&created).trim())?,
        ServerEvent::SessionCreated { id: 53, session_id, session_name, working_dir }
            if session_id == "created-session"
                && session_name == "Created session"
                && working_dir == "/server/project"
    ));
    Ok(())
}
