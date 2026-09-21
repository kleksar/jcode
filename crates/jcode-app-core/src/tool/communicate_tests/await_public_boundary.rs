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

#[cfg(unix)]
#[tokio::test]
async fn communicate_await_members_scoped_waits_for_terminal_response_and_fetches_reports() {
    let _env_lock = crate::storage::lock_test_env();
    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let socket_path = runtime_dir.path().join("scoped-await.sock");
    let _socket = EnvGuard::set("JCODE_SOCKET", &socket_path);
    let repo_dir = std::env::current_dir().expect("repo cwd");
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut ctx = test_ctx("scoped-await-session", &repo_dir);
    ctx.inline_swarm_await = Some(Arc::clone(&counter));

    let listener = crate::transport::Listener::bind(&socket_path).expect("bind scripted socket");
    let (request_seen_tx, request_seen_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept await request");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read await request");
        let request: Request = serde_json::from_str(line.trim()).expect("deserialize await request");
        request_seen_tx.send(request).expect("notify request receipt");
        release_rx.await.expect("release terminal response");

        let response = ServerEvent::CommAwaitMembersResponse {
            id: 1,
            completed: true,
            members: vec![AwaitedMemberStatus {
                session_id: "worker".to_string(),
                friendly_name: Some("worker".to_string()),
                status: "completed".to_string(),
                done: true,
                completion_report: None,
            }],
            summary: "worker completed".to_string(),
            background_started: false,
        };
        writer
            .write_all(
                format!(
                    "{}\n",
                    serde_json::to_string(&response).expect("serialize await response")
                )
                .as_bytes(),
            )
            .await
            .expect("write await response");
        drop(writer);

        let (stream, _) = listener.accept().await.expect("accept report request");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read report request");
        let report_request: Request =
            serde_json::from_str(line.trim()).expect("deserialize report request");
        assert!(matches!(
            report_request,
            Request::CommReadContext {
                session_id,
                target_session,
                ..
            } if session_id == "scoped-await-session" && target_session == "worker"
        ));
        let response = ServerEvent::CommContextHistory {
            id: 1,
            session_id: "worker".to_string(),
            messages: vec![HistoryMessage {
                role: "assistant".to_string(),
                content: "terminal worker report".to_string(),
                tool_calls: None,
                tool_data: None,
            }],
        };
        writer
            .write_all(
                format!(
                    "{}\n",
                    serde_json::to_string(&response).expect("serialize report response")
                )
                .as_bytes(),
            )
            .await
            .expect("write report response");
    });

    let execute_task = tokio::spawn(async move {
        CommunicateTool::new()
            .execute(
                json!({
                    "action": "await_members",
                    "session_ids": ["worker"],
                    "target_status": ["completed"],
                    "mode": "all",
                    "timeout_minutes": 1,
                    "background": true,
                    "notify": true,
                    "wake": true
                }),
                ctx,
            )
            .await
    });
    let request = tokio::time::timeout(Duration::from_secs(1), request_seen_rx)
        .await
        .expect("scoped await request should be sent")
        .expect("scoped await request notification");
    assert!(!execute_task.is_finished(), "scoped await must remain pending");
    let Request::CommAwaitMembers {
        id,
        session_id,
        target_status,
        session_ids,
        mode,
        timeout_secs,
        background,
        notify,
        wake,
    } = request
    else {
        panic!("scoped await must send CommAwaitMembers");
    };
    assert_eq!(id, 1);
    assert_eq!(session_id, "scoped-await-session");
    assert_eq!(target_status, vec!["completed"]);
    assert_eq!(session_ids, vec!["worker"]);
    assert_eq!(mode.as_deref(), Some("all"));
    assert_eq!(timeout_secs, Some(60));
    assert!(!background, "scoped await must not be backgrounded");
    assert!(!notify);
    assert!(!wake);
    assert_eq!(counter.load(std::sync::atomic::Ordering::Acquire), 1);

    release_tx.send(()).expect("release scoped await");
    let output = tokio::time::timeout(Duration::from_secs(1), execute_task)
        .await
        .expect("scoped await should finish after terminal response")
        .expect("scoped await task should not panic")
        .expect("scoped await should succeed");
    assert!(output.output.contains("terminal worker report"));
    assert_eq!(counter.load(std::sync::atomic::Ordering::Acquire), 0);
    server.await.expect("scripted socket task");
}

#[cfg(unix)]
#[tokio::test]
async fn communicate_await_members_scoped_timeout_retains_unresolved_counter() {
    let _env_lock = crate::storage::lock_test_env();
    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let socket_path = runtime_dir.path().join("scoped-await-timeout.sock");
    let _socket = EnvGuard::set("JCODE_SOCKET", &socket_path);
    let repo_dir = std::env::current_dir().expect("repo cwd");
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut ctx = test_ctx("scoped-timeout-session", &repo_dir);
    ctx.inline_swarm_await = Some(Arc::clone(&counter));

    let listener = crate::transport::Listener::bind(&socket_path).expect("bind scripted socket");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept timeout request");
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read timeout request");
        let request: Request = serde_json::from_str(line.trim()).expect("deserialize timeout request");
        let id = request.id();
        let response = ServerEvent::CommAwaitMembersResponse {
            id,
            completed: false,
            members: Vec::new(),
            summary: "worker still running".to_string(),
            background_started: false,
        };
        writer
            .write_all(
                format!(
                    "{}\n",
                    serde_json::to_string(&response).expect("serialize timeout response")
                )
                .as_bytes(),
            )
            .await
            .expect("write timeout response");
    });

    let error = CommunicateTool::new()
        .execute(
            json!({"action": "await_members", "session_ids": ["worker"]}),
            ctx,
        )
        .await
        .expect_err("incomplete scoped await must fail closed");
    assert!(error.to_string().contains("timed out"));
    assert_eq!(counter.load(std::sync::atomic::Ordering::Acquire), 1);
    server.await.expect("scripted socket task");
}

#[cfg(unix)]
#[tokio::test]
async fn communicate_await_members_scoped_cancellation_retains_unresolved_counter() {
    let _env_lock = crate::storage::lock_test_env();
    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let socket_path = runtime_dir.path().join("scoped-await-cancel.sock");
    let _socket = EnvGuard::set("JCODE_SOCKET", &socket_path);
    let repo_dir = std::env::current_dir().expect("repo cwd");
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let shutdown = jcode_agent_runtime::InterruptSignal::new();
    let mut ctx = test_ctx("scoped-cancel-session", &repo_dir);
    ctx.inline_swarm_await = Some(Arc::clone(&counter));
    ctx.graceful_shutdown_signal = Some(shutdown.clone());

    let listener = crate::transport::Listener::bind(&socket_path).expect("bind scripted socket");
    let (request_seen_tx, request_seen_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept cancellation request");
        let (reader, _writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .expect("read cancellation request");
        request_seen_tx
            .send(())
            .expect("notify cancellation request receipt");
        release_rx.await.expect("release cancellation server");
    });

    let execute_task = tokio::spawn(async move {
        CommunicateTool::new()
            .execute(
                json!({"action": "await_members", "session_ids": ["worker"]}),
                ctx,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), request_seen_rx)
        .await
        .expect("scoped cancellation request should be sent")
        .expect("scoped cancellation request notification");
    shutdown.fire();
    let error = tokio::time::timeout(Duration::from_secs(1), execute_task)
        .await
        .expect("scoped cancellation should finish promptly")
        .expect("scoped cancellation task should not panic")
        .expect_err("scoped cancellation must fail closed");
    assert!(error.to_string().contains("cancelled"));
    assert_eq!(counter.load(std::sync::atomic::Ordering::Acquire), 1);
    release_tx.send(()).expect("release cancellation server");
    server.await.expect("scripted socket task");
}
