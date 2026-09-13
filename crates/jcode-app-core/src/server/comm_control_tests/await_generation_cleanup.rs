async fn await_fixture(
    requester: &str,
    peer: &str,
    swarm_id: &str,
) -> (
    Arc<RwLock<HashMap<String, SwarmMember>>>,
    Arc<RwLock<HashMap<String, HashSet<String>>>>,
    broadcast::Sender<SwarmEvent>,
) {
    let members = Arc::new(RwLock::new(HashMap::from([
        (requester.to_string(), member(requester, swarm_id, "ready")),
        (peer.to_string(), member(peer, swarm_id, "running")),
    ])));
    let swarms = Arc::new(RwLock::new(HashMap::from([(
        swarm_id.to_string(),
        HashSet::from([requester.to_string(), peer.to_string()]),
    )])));
    let (events, _events_rx) = broadcast::channel(16);
    (members, swarms, events)
}

#[test]
fn await_members_generation_is_backward_compatible_and_never_wraps() {
    let (_env, _runtime_dir) = RuntimeEnvGuard::new();
    let initial = pending_await_state(
        "legacy-generation",
        "legacy-swarm",
        "legacy-peer",
        true,
        true,
        true,
    );
    let mut legacy_json = serde_json::to_value(&initial).expect("serialize state fixture");
    legacy_json
        .as_object_mut()
        .expect("state serializes as object")
        .remove("request_generation");
    let legacy: crate::server::await_members_state::PersistedAwaitMembersState =
        serde_json::from_value(legacy_json).expect("legacy state remains readable");
    assert_eq!(legacy.request_generation, 0);
    crate::server::await_members_state::save_state(&legacy);

    let upgraded = ensure_pending_state(
        &legacy.key,
        &legacy.session_id,
        &legacy.swarm_id,
        &legacy.requested_ids,
        &legacy.target_status,
        legacy.mode.as_deref(),
        legacy.deadline_unix_ms,
        legacy.background,
        legacy.notify,
        legacy.wake,
    )
    .expect("legacy retry should establish first durable generation");
    assert_eq!(upgraded.request_generation, 1);

    let audit = crate::server::await_members_state::persist_non_replayable_final_response(
        &upgraded,
        false,
        Vec::new(),
        "reload orphan audit".to_string(),
    );
    let replacement = ensure_pending_state(
        &audit.key,
        &audit.session_id,
        &audit.swarm_id,
        &audit.requested_ids,
        &audit.target_status,
        audit.mode.as_deref(),
        audit.deadline_unix_ms,
        audit.background,
        audit.notify,
        audit.wake,
    )
    .expect("non-replayable audit retry should establish a newer generation");
    assert!(replacement.is_pending());
    assert_eq!(replacement.request_generation, 2);

    let mut exhausted = replacement.clone();
    exhausted.request_generation = u64::MAX;
    crate::server::await_members_state::save_state(&exhausted);
    assert!(matches!(
        ensure_pending_state(
            &exhausted.key,
            &exhausted.session_id,
            &exhausted.swarm_id,
            &exhausted.requested_ids,
            &exhausted.target_status,
            exhausted.mode.as_deref(),
            exhausted.deadline_unix_ms,
            exhausted.background,
            exhausted.notify,
            exhausted.wake,
        ),
        Err(crate::server::await_members_state::AwaitRequestGenerationError::Exhausted)
    ));
}

#[tokio::test]
async fn await_members_reload_snapshot_skips_newer_generation_without_wake() {
    let (_env, _runtime_dir) = RuntimeEnvGuard::new();
    let requester = "reload-generation-requester";
    let peer = "reload-generation-peer";
    let swarm_id = "reload-generation-swarm";
    let runtime = AwaitMembersRuntime::default();
    let state = pending_await_state(requester, swarm_id, peer, true, true, true);
    assert_eq!(state.request_generation, 1);
    let original_deadline = state.deadline_unix_ms;
    let (members, swarms, events) = await_fixture(requester, peer, swarm_id).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    runtime
        .pause_reload_orphan_after_snapshot(entered.clone(), release.clone())
        .await;
    let mut bus_rx = crate::bus::Bus::global().subscribe();

    let resume_runtime = runtime.clone();
    let resume_members = members.clone();
    let resume_swarms = swarms.clone();
    let resume_events = events.clone();
    let resume = tokio::spawn(async move {
        resume_background_awaits(
            &resume_members,
            &resume_swarms,
            &resume_events,
            &resume_runtime,
        )
        .await;
    });
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("reload cleanup should capture the orphan snapshot");

    let (retry_tx, mut retry_rx) = mpsc::unbounded_channel();
    handle_comm_await_members(
        41,
        requester.to_string(),
        vec!["completed".to_string()],
        vec![peer.to_string()],
        None,
        Some(5),
        true,
        false,
        false,
        CommAwaitMembersContext {
            client_event_tx: &retry_tx,
            swarm_members: &members,
            swarms_by_id: &swarms,
            swarm_event_tx: &events,
            await_members_runtime: &runtime,
        },
    )
    .await;
    assert!(matches!(
        retry_rx.recv().await,
        Some(ServerEvent::CommAwaitMembersResponse {
            background_started: true,
            ..
        })
    ));
    let retry = load_state(&state.key).expect("retry remains durable");
    assert!(
        retry.is_pending(),
        "stale orphan cleanup must not cancel retry"
    );
    assert_eq!(retry.request_generation, 2);
    assert_eq!(retry.deadline_unix_ms, original_deadline);
    assert!(!retry.notify);
    assert!(!retry.wake);

    release.notify_one();
    resume.await.expect("reload cleanup task should finish");
    runtime.clear_reload_orphan_pause().await;
    let current = load_state(&state.key).expect("retry survives stale cleanup");
    assert!(current.is_pending());
    assert_eq!(current.request_generation, retry.request_generation);
    assert_no_second_terminal_await_event(&mut bus_rx, requester).await;
    assert_eq!(runtime.transaction_registry_len(), 0);

    // Finish the replacement watcher so it cannot outlive this deterministic
    // test. Its latest false/false delivery policy must still emit no wake.
    members
        .write()
        .await
        .get_mut(peer)
        .expect("peer exists")
        .status = "completed".to_string();
    let _ = events.send(swarm_event(
        peer,
        swarm_id,
        SwarmEventType::StatusChange {
            old_status: "running".to_string(),
            new_status: "completed".to_string(),
        },
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if load_state(&state.key).is_some_and(|saved| !saved.is_pending()) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("replacement watcher should finish");
    assert_no_second_terminal_await_event(&mut bus_rx, requester).await;
    assert_eq!(runtime.transaction_registry_len(), 0);
}

#[tokio::test]
async fn await_members_reload_snapshot_cancels_matching_orphan_without_delivery() {
    let (_env, _runtime_dir) = RuntimeEnvGuard::new();
    let requester = "matching-orphan-requester";
    let peer = "matching-orphan-peer";
    let swarm_id = "matching-orphan-swarm";
    let runtime = AwaitMembersRuntime::default();
    let state = pending_await_state(requester, swarm_id, peer, true, true, true);
    let (members, swarms, events) = await_fixture(requester, peer, swarm_id).await;
    let mut bus_rx = crate::bus::Bus::global().subscribe();

    resume_background_awaits(&members, &swarms, &events, &runtime).await;

    let cancelled = load_state(&state.key).expect("orphan audit remains durable");
    assert_eq!(cancelled.request_generation, state.request_generation);
    assert!(!cancelled.is_pending());
    assert!(
        !cancelled
            .final_response
            .expect("orphan cancellation final response")
            .replayable
    );
    assert_no_second_terminal_await_event(&mut bus_rx, requester).await;
    assert_eq!(runtime.transaction_registry_len(), 0);
}

#[tokio::test]
async fn await_members_transaction_registry_reclaims_terminal_immediate_timeout_removed_error_and_leases()
 {
    let (_env, _runtime_dir) = RuntimeEnvGuard::new();

    // Terminal finalization releases the sole lease.
    let terminal_runtime = AwaitMembersRuntime::default();
    let terminal = pending_await_state(
        "registry-terminal",
        "registry-swarm",
        "peer",
        true,
        false,
        false,
    );
    finalize_await(
        &terminal_runtime,
        &terminal,
        true,
        vec![completed_member("peer")],
        "done".to_string(),
    )
    .await;
    assert_eq!(terminal_runtime.transaction_registry_len(), 0);

    // Immediate success, timeout, removed explicit worker, and no-swarm error
    // each leave the registry empty even when their request control flow
    // returns early.
    for (label, peer_status, requested_ids, timeout, in_swarm) in [
        (
            "immediate",
            "completed",
            vec!["peer".to_string()],
            Some(60),
            true,
        ),
        (
            "timeout",
            "running",
            vec!["peer".to_string()],
            Some(0),
            true,
        ),
        (
            "removed",
            "running",
            vec!["removed".to_string()],
            Some(60),
            true,
        ),
        (
            "error",
            "running",
            vec!["peer".to_string()],
            Some(60),
            false,
        ),
    ] {
        let runtime = AwaitMembersRuntime::default();
        let requester = format!("registry-{label}-requester");
        let peer = format!("registry-{label}-peer");
        let swarm_id = format!("registry-{label}-swarm");
        let (members, swarms, events) = await_fixture(&requester, &peer, &swarm_id).await;
        if peer_status == "completed" {
            members
                .write()
                .await
                .get_mut(&peer)
                .expect("peer exists")
                .status = peer_status.to_string();
        }
        if !in_swarm {
            members
                .write()
                .await
                .get_mut(&requester)
                .expect("requester exists")
                .swarm_id = None;
        }
        let (tx, mut rx) = mpsc::unbounded_channel();
        handle_comm_await_members(
            51,
            requester,
            vec!["completed".to_string()],
            requested_ids,
            None,
            timeout,
            false,
            false,
            false,
            CommAwaitMembersContext {
                client_event_tx: &tx,
                swarm_members: &members,
                swarms_by_id: &swarms,
                swarm_event_tx: &events,
                await_members_runtime: &runtime,
            },
        )
        .await;
        let _ = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("representative exit responds");
        assert_eq!(
            runtime.transaction_registry_len(),
            0,
            "{label} exit leaked transaction entry"
        );
    }

    // A queued lease retains the same lock identity until every holder drops;
    // no second lock can be created between concurrent request operations.
    let runtime = AwaitMembersRuntime::default();
    let first = runtime
        .transaction_for_request("concurrent-registry-key")
        .await;
    assert_eq!(runtime.transaction_registry_len(), 1);
    let waiting_runtime = runtime.clone();
    let waiting = tokio::spawn(async move {
        waiting_runtime
            .transaction_for_request("concurrent-registry-key")
            .await
    });
    tokio::task::yield_now().await;
    assert_eq!(runtime.transaction_registry_len(), 1);
    drop(first);
    let second = tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .expect("queued lease should acquire")
        .expect("queued lease task should not panic");
    assert_eq!(runtime.transaction_registry_len(), 1);
    drop(second);
    assert_eq!(runtime.transaction_registry_len(), 0);
}
