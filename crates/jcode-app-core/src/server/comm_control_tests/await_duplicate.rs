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
        ServerEvent::CommAwaitMembersResponse { id: 7, completed: true, .. }
    ));
    assert!(tokio::time::timeout(Duration::from_millis(50), client_rx.recv())
        .await
        .is_err(), "duplicate request must not produce a second response");
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

    let statuses = super::comm_await::awaited_member_statuses(
        requester,
        swarm_id,
        &["removed-worker".to_string()],
        &["completed".to_string(), "stopped".to_string(), "failed".to_string()],
        &swarm_members,
        &swarms_by_id,
    )
    .await;

    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].status, "stopped");
    assert!(statuses[0].done);
}
