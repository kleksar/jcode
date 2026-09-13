fn pending_await_state(
    requester: &str,
    swarm_id: &str,
    peer: &str,
    background: bool,
    notify: bool,
    wake: bool,
) -> crate::server::await_members_state::PersistedAwaitMembersState {
    let target_status = vec!["completed".to_string()];
    let requested_ids = vec![peer.to_string()];
    let key = request_key(requester, swarm_id, &requested_ids, &target_status, None);
    let deadline = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_millis() as u64
        + Duration::from_secs(60).as_millis() as u64;
    ensure_pending_state(
        &key,
        requester,
        swarm_id,
        &requested_ids,
        &target_status,
        None,
        deadline,
        background,
        notify,
        wake,
    )
    .expect("initial await generation should be available")
}

fn completed_member(peer: &str) -> AwaitedMemberStatus {
    AwaitedMemberStatus {
        session_id: peer.to_string(),
        friendly_name: Some(peer.to_string()),
        status: "completed".to_string(),
        done: true,
        completion_report: None,
    }
}

async fn next_terminal_await_event(
    bus_rx: &mut tokio::sync::broadcast::Receiver<crate::bus::BusEvent>,
    requester: &str,
) -> crate::bus::SwarmAwaitCompleted {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match bus_rx.recv().await {
                Ok(crate::bus::BusEvent::SwarmAwaitCompleted(event))
                    if event.session_id == requester =>
                {
                    return event;
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    panic!("bus closed before terminal await event")
                }
            }
        }
    })
    .await
    .expect("terminal await event should arrive")
}

async fn assert_no_second_terminal_await_event(
    bus_rx: &mut tokio::sync::broadcast::Receiver<crate::bus::BusEvent>,
    requester: &str,
) {
    assert!(
        tokio::time::timeout(Duration::from_millis(100), async {
            loop {
                match bus_rx.recv().await {
                    Ok(crate::bus::BusEvent::SwarmAwaitCompleted(event))
                        if event.session_id == requester =>
                    {
                        return true;
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return false,
                }
            }
        })
        .await
        .is_err(),
        "a semantic await has exactly one terminal BusEvent"
    );
}

#[tokio::test]
async fn await_members_retry_preference_claimed_before_terminal_wins_race() {
    let (_env, _runtime_dir) = RuntimeEnvGuard::new();
    let requester = "request-pref-first";
    let peer = "peer-pref-first";
    let swarm_id = "swarm-pref-first";
    let target_status = vec!["completed".to_string()];
    let requested_ids = vec![peer.to_string()];
    let await_runtime = AwaitMembersRuntime::default();
    let state = pending_await_state(requester, swarm_id, peer, true, true, true);
    assert!(await_runtime.mark_active_if_new(&state.key).await);

    let swarm_members = Arc::new(RwLock::new(HashMap::from([
        (requester.to_string(), member(requester, swarm_id, "ready")),
        (peer.to_string(), member(peer, swarm_id, "running")),
    ])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        swarm_id.to_string(),
        HashSet::from([requester.to_string(), peer.to_string()]),
    )])));
    let (swarm_event_tx, _swarm_event_rx) = broadcast::channel(16);
    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    let (retry_tx, mut retry_rx) = mpsc::unbounded_channel();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    await_runtime
        .pause_transaction_after_acquire(
            AwaitTransactionOperation::Request,
            entered.clone(),
            release.clone(),
        )
        .await;
    let entered_wait = entered.notified();
    let mut bus_rx = crate::bus::Bus::global().subscribe();

    let retry_runtime = await_runtime.clone();
    let retry_members = swarm_members.clone();
    let retry_swarms = swarms_by_id.clone();
    let retry_events = swarm_event_tx.clone();
    let update = tokio::spawn(async move {
        handle_comm_await_members(
            1,
            requester.to_string(),
            target_status.clone(),
            requested_ids.clone(),
            None,
            Some(60),
            true,
            true,
            false,
            CommAwaitMembersContext {
                client_event_tx: &update_tx,
                swarm_members: &retry_members,
                swarms_by_id: &retry_swarms,
                swarm_event_tx: &retry_events,
                await_members_runtime: &retry_runtime,
            },
        )
        .await;
    });

    tokio::time::timeout(Duration::from_secs(1), entered_wait)
        .await
        .expect("retry should hold the request transaction");
    let finalize_runtime = await_runtime.clone();
    let finalize_state = state.clone();
    let finalizer = tokio::spawn(async move {
        finalize_await(
            &finalize_runtime,
            &finalize_state,
            true,
            vec![completed_member(peer)],
            "done".to_string(),
        )
        .await;
    });
    release.notify_one();
    update.await.expect("preference retry task should finish");
    finalizer.await.expect("terminal claim task should finish");
    await_runtime.clear_transaction_pause().await;

    assert!(matches!(
        update_rx.recv().await,
        Some(ServerEvent::CommAwaitMembersResponse {
            background_started: true,
            ..
        })
    ));
    let terminal = next_terminal_await_event(&mut bus_rx, requester).await;
    assert!(terminal.notify);
    assert!(
        !terminal.wake,
        "the committed true/true -> true/false retry must win before the terminal claim"
    );
    assert_eq!(
        load_state(&state.key)
            .and_then(|state| state.final_response)
            .expect("terminal state")
            .replayable,
        true
    );

    swarm_members
        .write()
        .await
        .get_mut(peer)
        .expect("peer exists")
        .status = "completed".to_string();
    handle_comm_await_members(
        2,
        requester.to_string(),
        vec!["completed".to_string()],
        vec![peer.to_string()],
        None,
        Some(60),
        true,
        true,
        true,
        CommAwaitMembersContext {
            client_event_tx: &retry_tx,
            swarm_members: &swarm_members,
            swarms_by_id: &swarms_by_id,
            swarm_event_tx: &swarm_event_tx,
            await_members_runtime: &await_runtime,
        },
    )
    .await;
    assert!(matches!(
        retry_rx.recv().await,
        Some(ServerEvent::CommAwaitMembersResponse {
            completed: true,
            background_started: false,
            ..
        })
    ));
    assert_no_second_terminal_await_event(&mut bus_rx, requester).await;
    assert!(
        await_runtime.mark_active_if_new(&state.key).await,
        "terminal finalization must clear the active semantic key"
    );
    await_runtime.clear_active(&state.key).await;
}

#[tokio::test]
async fn await_members_terminal_claim_wins_retry_without_second_wake() {
    let (_env, _runtime_dir) = RuntimeEnvGuard::new();
    let requester = "request-final-first";
    let peer = "peer-final-first";
    let swarm_id = "swarm-final-first";
    let await_runtime = AwaitMembersRuntime::default();
    let state = pending_await_state(requester, swarm_id, peer, true, true, true);
    assert!(await_runtime.mark_active_if_new(&state.key).await);
    let swarm_members = Arc::new(RwLock::new(HashMap::from([
        (requester.to_string(), member(requester, swarm_id, "ready")),
        (peer.to_string(), member(peer, swarm_id, "completed")),
    ])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        swarm_id.to_string(),
        HashSet::from([requester.to_string(), peer.to_string()]),
    )])));
    let (swarm_event_tx, _swarm_event_rx) = broadcast::channel(16);
    let (retry_tx, mut retry_rx) = mpsc::unbounded_channel();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    await_runtime
        .pause_transaction_after_acquire(
            AwaitTransactionOperation::Finalize,
            entered.clone(),
            release.clone(),
        )
        .await;
    let entered_wait = entered.notified();
    let mut bus_rx = crate::bus::Bus::global().subscribe();

    let finalize_runtime = await_runtime.clone();
    let finalize_state = state.clone();
    let finalizer = tokio::spawn(async move {
        finalize_await(
            &finalize_runtime,
            &finalize_state,
            true,
            vec![completed_member(peer)],
            "done".to_string(),
        )
        .await;
    });
    tokio::time::timeout(Duration::from_secs(1), entered_wait)
        .await
        .expect("finalizer should hold the terminal transaction");

    let retry_runtime = await_runtime.clone();
    let retry_members = swarm_members.clone();
    let retry_swarms = swarms_by_id.clone();
    let retry_events = swarm_event_tx.clone();
    let retry = tokio::spawn(async move {
        handle_comm_await_members(
            3,
            requester.to_string(),
            vec!["completed".to_string()],
            vec![peer.to_string()],
            None,
            Some(60),
            true,
            true,
            false,
            CommAwaitMembersContext {
                client_event_tx: &retry_tx,
                swarm_members: &retry_members,
                swarms_by_id: &retry_swarms,
                swarm_event_tx: &retry_events,
                await_members_runtime: &retry_runtime,
            },
        )
        .await;
    });
    release.notify_one();
    finalizer.await.expect("terminal claim task should finish");
    retry.await.expect("retry task should finish");
    await_runtime.clear_transaction_pause().await;

    let terminal = next_terminal_await_event(&mut bus_rx, requester).await;
    assert!(terminal.notify);
    assert!(
        terminal.wake,
        "a retry queued behind the terminal claim must not overwrite committed wake=true"
    );
    assert!(matches!(
        retry_rx.recv().await,
        Some(ServerEvent::CommAwaitMembersResponse {
            completed: true,
            background_started: false,
            ..
        })
    ));
    assert_no_second_terminal_await_event(&mut bus_rx, requester).await;
    assert!(
        await_runtime.mark_active_if_new(&state.key).await,
        "the sole terminal owner must clean its active key"
    );
    await_runtime.clear_active(&state.key).await;
}

#[tokio::test]
async fn await_members_background_to_blocking_disconnect_cleans_active_state() {
    let (_env, _runtime_dir) = RuntimeEnvGuard::new();
    let requester = "request-background-to-blocking";
    let peer = "peer-background-to-blocking";
    let swarm_id = "swarm-background-to-blocking";
    let await_runtime = AwaitMembersRuntime::default();
    let swarm_members = Arc::new(RwLock::new(HashMap::from([
        (requester.to_string(), member(requester, swarm_id, "ready")),
        (peer.to_string(), member(peer, swarm_id, "running")),
    ])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        swarm_id.to_string(),
        HashSet::from([requester.to_string(), peer.to_string()]),
    )])));
    let (swarm_event_tx, swarm_event_rx) = broadcast::channel(16);
    drop(swarm_event_rx);
    let baseline_receivers = swarm_event_tx.receiver_count();
    let (background_tx, mut background_rx) = mpsc::unbounded_channel();

    handle_comm_await_members(
        1,
        requester.to_string(),
        vec!["completed".to_string()],
        vec![peer.to_string()],
        None,
        Some(60),
        true,
        true,
        true,
        CommAwaitMembersContext {
            client_event_tx: &background_tx,
            swarm_members: &swarm_members,
            swarms_by_id: &swarms_by_id,
            swarm_event_tx: &swarm_event_tx,
            await_members_runtime: &await_runtime,
        },
    )
    .await;
    assert!(matches!(
        background_rx.recv().await,
        Some(ServerEvent::CommAwaitMembersResponse {
            background_started: true,
            ..
        })
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        while swarm_event_tx.receiver_count() != baseline_receivers + 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("background watcher should register");

    // The same key can be downgraded to blocking. Its only new requester then
    // disconnects, which must cancel the active watcher instead of leaving the
    // old background task or lock entry behind.
    let (blocking_tx, blocking_rx) = mpsc::unbounded_channel();
    handle_comm_await_members(
        2,
        requester.to_string(),
        vec!["completed".to_string()],
        vec![peer.to_string()],
        None,
        Some(60),
        false,
        false,
        false,
        CommAwaitMembersContext {
            client_event_tx: &blocking_tx,
            swarm_members: &swarm_members,
            swarms_by_id: &swarms_by_id,
            swarm_event_tx: &swarm_event_tx,
            await_members_runtime: &await_runtime,
        },
    )
    .await;
    drop(blocking_rx);
    drop(blocking_tx);
    let _ = swarm_event_tx.send(swarm_event(
        peer,
        swarm_id,
        SwarmEventType::StatusChange {
            old_status: "running".to_string(),
            new_status: "working".to_string(),
        },
    ));

    tokio::time::timeout(Duration::from_secs(1), async {
        while swarm_event_tx.receiver_count() != baseline_receivers {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("downgraded blocking await should unsubscribe after disconnect");

    let key = request_key(
        requester,
        swarm_id,
        &[peer.to_string()],
        &["completed".to_string()],
        None,
    );
    let final_response = load_state(&key)
        .and_then(|state| state.final_response)
        .expect("disconnect cancellation should remain durable for audit");
    assert!(!final_response.replayable);
    assert!(
        await_runtime.mark_active_if_new(&key).await,
        "disconnect must clean the active semantic key after background-to-blocking downgrade"
    );
    await_runtime.clear_active(&key).await;
    assert_eq!(
        await_runtime.transaction_registry_len(),
        0,
        "disconnected cancellation must release its transaction registry entry"
    );
}
