#[cfg(unix)]
#[tokio::test]
async fn communicate_await_members_public_boundary_is_backgrounded_with_defaults() {
    let _env_lock = crate::storage::lock_test_env();
    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let socket_path = runtime_dir.path().join("public-await.sock");
    let _socket = EnvGuard::set("JCODE_SOCKET", &socket_path);
    let repo_dir = std::env::current_dir().expect("repo cwd");

    // This is deliberately a scripted local socket rather than a peer/provider
    // E2E. It captures the actual public CommunicateTool wire request, which
    // catches regressions back to foreground waits or the old polling path.
    let listener = crate::transport::Listener::bind(&socket_path).expect("bind scripted socket");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept public tool request");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read request");
        let request: Request = serde_json::from_str(line.trim()).expect("deserialize request");
        let id = request.id();
        let response = ServerEvent::CommAwaitMembersResponse {
            id,
            completed: false,
            members: Vec::new(),
            summary: "Watching in background.".to_string(),
            background_started: true,
        };
        writer
            .write_all(
                format!(
                    "{}\n",
                    serde_json::to_string(&response).expect("serialize response")
                )
                .as_bytes(),
            )
            .await
            .expect("write response");
        request
    });

    let output = tokio::time::timeout(
        Duration::from_secs(1),
        CommunicateTool::new().execute(
            json!({"action": "await_members", "timeout_minutes": 7}),
            test_ctx("public-await-session", &repo_dir),
        ),
    )
    .await
    .expect("public await must return promptly")
    .expect("public await tool should accept background hand-off");
    assert!(output.output.contains("background"));

    let request = server.await.expect("scripted socket task");
    let Request::CommAwaitMembers {
        id,
        session_id,
        target_status,
        session_ids,
        timeout_secs,
        background,
        notify,
        wake,
        ..
    } = request
    else {
        panic!("public await tool must send CommAwaitMembers request");
    };
    assert_eq!(
        id, 1,
        "public tool must preserve its request correlation id"
    );
    assert_eq!(session_id, "public-await-session");
    assert!(session_ids.is_empty());
    assert_eq!(timeout_secs, Some(7 * 60));
    assert!(
        background,
        "public await must never use foreground blocking"
    );
    assert!(notify);
    assert!(wake);
    assert!(
        ["ready", "completed", "stopped", "failed"]
            .into_iter()
            .all(|status| target_status.iter().any(|actual| actual == status)),
        "public await must include its documented terminal-status defaults: {target_status:?}"
    );
}
