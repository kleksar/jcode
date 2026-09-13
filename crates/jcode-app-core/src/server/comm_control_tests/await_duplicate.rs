#[tokio::test]
async fn await_members_repeated_request_registers_one_waiter() {
    let (_env, _runtime) = RuntimeEnvGuard::new();
    let swarm_id = "swarm-await-duplicate";
    let requester = "req";
    let peer = "peer";
    let await_runtime = AwaitMembersRuntime::default();
    let (client_tx, mut client_rx) = mpsc::unbounded_channel();
    let swarm_members = Arc::new(RwLock::new(HashMap::from([
        (requester.to_string(), member(requester, swarm_id, "ready")),
        (peer.to_string(), member(peer, swarm_id, "running")),
    ])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        swarm_id.to_string(),
        HashSet::from([requester.to_string(), peer.to_string()]),
    )])));
    let (swarm_event_tx, _swarm_event_rx) = broadcast::channel(32);

    for _ in 0..2 {
        handle_comm_await_members(
            7,
            requester.to_string(),
            vec!["completed".to_string()],
            vec![peer.to_string()],
            None,
            Some(60),
            false,
            false,
            false,
            CommAwaitMembersContext {
                client_event_tx: &client_tx,
                swarm_members: &swarm_members,
                swarms_by_id: &swarms_by_id,
                swarm_event_tx: &swarm_event_tx,
                await_members_runtime: &await_runtime,
            },
        )
        .await;
    }

    swarm_members
        .write()
        .await
        .get_mut(peer)
        .expect("peer exists")
        .status = "completed".to_string();
    let _ = swarm_event_tx.send(swarm_event(
        peer,
        swarm_id,
        SwarmEventType::StatusChange {
            old_status: "running".to_string(),
            new_status: "completed".to_string(),
        },
    ));

    let response = tokio::time::timeout(Duration::from_secs(1), client_rx.recv())
        .await
        .expect("one response should arrive")
        .expect("channel should stay open");
    assert!(matches!(
        response,
        ServerEvent::CommAwaitMembersResponse {
            id: 7,
            completed: true,
            ..
        }
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), client_rx.recv())
            .await
            .is_err(),
        "duplicate request must not produce a second response"
    );
}

#[tokio::test]
async fn await_members_reports_removed_explicit_worker_as_stopped() {
    let requester = "req";
    let swarm_id = "swarm-await-terminal";
    let swarm_members = Arc::new(RwLock::new(HashMap::from([(
        requester.to_string(),
        member(requester, swarm_id, "ready"),
    )])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        swarm_id.to_string(),
        HashSet::from([requester.to_string(), "removed-worker".to_string()]),
    )])));

    let statuses = crate::server::comm_await::awaited_member_statuses(
        requester,
        swarm_id,
        &["removed-worker".to_string()],
        &[
            "completed".to_string(),
            "stopped".to_string(),
            "failed".to_string(),
        ],
        &swarm_members,
        &swarms_by_id,
    )
    .await;

    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].status, "stopped");
    assert!(statuses[0].done);
}

#[tokio::test]
async fn await_members_duplicate_background_request_emits_one_terminal_wake() {
    let (_env, _runtime) = RuntimeEnvGuard::new();
    let swarm_id = "swarm-await-duplicate-terminal";
    let requester = "req-terminal";
    let peer = "peer-terminal";
    let await_runtime = AwaitMembersRuntime::default();
    let (client_tx, _client_rx) = mpsc::unbounded_channel();
    let swarm_members = Arc::new(RwLock::new(HashMap::from([
        (requester.to_string(), member(requester, swarm_id, "ready")),
        (peer.to_string(), member(peer, swarm_id, "running")),
    ])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        swarm_id.to_string(),
        HashSet::from([requester.to_string(), peer.to_string()]),
    )])));
    let (swarm_event_tx, _swarm_event_rx) = broadcast::channel(32);
    let mut bus_rx = crate::bus::Bus::global().subscribe();

    for _ in 0..2 {
        handle_comm_await_members(
            9,
            requester.to_string(),
            vec!["completed".to_string()],
            vec![peer.to_string()],
            None,
            Some(60),
            true,
            true,
            true,
            CommAwaitMembersContext {
                client_event_tx: &client_tx,
                swarm_members: &swarm_members,
                swarms_by_id: &swarms_by_id,
                swarm_event_tx: &swarm_event_tx,
                await_members_runtime: &await_runtime,
            },
        )
        .await;
    }

    swarm_members
        .write()
        .await
        .get_mut(peer)
        .expect("peer exists")
        .status = "completed".to_string();
    let _ = swarm_event_tx.send(swarm_event(
        peer,
        swarm_id,
        SwarmEventType::StatusChange {
            old_status: "running".to_string(),
            new_status: "completed".to_string(),
        },
    ));

    let terminal = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match bus_rx.recv().await {
                Ok(crate::bus::BusEvent::SwarmAwaitCompleted(event))
                    if event.session_id == requester =>
                {
                    return event;
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    panic!("bus closed before terminal await delivery")
                }
            }
        }
    })
    .await
    .expect("one terminal await wake should arrive");
    assert!(terminal.completed);
    assert!(terminal.wake);
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
        "duplicate background request must not emit a second terminal wake"
    );
}
