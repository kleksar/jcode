// Concurrent `assign_next` regression coverage. The test-only pause in the
// handler holds the task reservation after selection and before spawning, which
// deterministically exercises the window where the durable plan is still
// unassigned.

#[derive(Clone)]
struct AssignNextReservationContext {
    requester: String,
    client_tx: mpsc::UnboundedSender<ServerEvent>,
    sessions: Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>,
    global_session_id: Arc<RwLock<String>>,
    provider: Arc<dyn Provider>,
    soft_interrupt_queues: crate::server::SessionInterruptQueues,
    client_connections: Arc<RwLock<HashMap<String, crate::server::ClientConnectionInfo>>>,
    swarm_members: Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    swarm_plans: Arc<RwLock<HashMap<String, VersionedPlan>>>,
    swarm_coordinators: Arc<RwLock<HashMap<String, String>>>,
    event_history: Arc<RwLock<VecDeque<SwarmEvent>>>,
    event_counter: Arc<AtomicU64>,
    swarm_event_tx: broadcast::Sender<SwarmEvent>,
    mcp_pool: Arc<crate::mcp::SharedMcpPool>,
    mutation_runtime: SwarmMutationRuntime,
}

impl AssignNextReservationContext {
    async fn assign_next(self, id: u64) {
        handle_comm_assign_next(
            id,
            self.requester,
            None,
            None,
            None,
            Some(true),
            None,
            None,
            None,
            &self.client_tx,
            &self.sessions,
            &self.global_session_id,
            &self.provider,
            &self.soft_interrupt_queues,
            &self.client_connections,
            &self.swarm_members,
            &self.swarms_by_id,
            &self.swarm_plans,
            &self.swarm_coordinators,
            &self.event_history,
            &self.event_counter,
            &self.swarm_event_tx,
            &self.mcp_pool,
            &self.mutation_runtime,
        )
        .await;
    }
}

async fn assign_next_reservation_context(
    swarm_id: &str,
    items: Vec<PlanItem>,
    worker_ids: &[&str],
) -> (
    AssignNextReservationContext,
    mpsc::UnboundedReceiver<ServerEvent>,
) {
    let requester = "coord".to_string();
    let (client_tx, client_rx) = mpsc::unbounded_channel();
    let sessions = Arc::new(RwLock::new(HashMap::from([(
        requester.clone(),
        test_agent().await,
    )])));
    let soft_interrupt_queues = Arc::new(RwLock::new(HashMap::new()));
    let client_connections = Arc::new(RwLock::new(HashMap::new()));
    let mut members = HashMap::from([(requester.clone(), {
        let mut member = member(&requester, swarm_id, "ready");
        member.role = "coordinator".to_string();
        member
    })]);
    for worker_id in worker_ids {
        members.insert(
            (*worker_id).to_string(),
            owned_member(worker_id, swarm_id, "ready", &requester),
        );
    }
    let swarm_members = Arc::new(RwLock::new(members));
    let mut swarm_member_ids = HashSet::from([requester.clone()]);
    swarm_member_ids.extend(worker_ids.iter().map(|worker_id| (*worker_id).to_string()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        swarm_id.to_string(),
        swarm_member_ids,
    )])));
    let node_meta = items
        .iter()
        .map(|item| {
            (
                item.id.clone(),
                crate::plan::NodeMeta {
                    kind: Some("implement".to_string()),
                    ..crate::plan::NodeMeta::default()
                },
            )
        })
        .collect();
    let swarm_plans = Arc::new(RwLock::new(HashMap::from([(
        swarm_id.to_string(),
        VersionedPlan {
            items,
            version: 1,
            participants: HashSet::from([requester.clone()]),
            task_progress: HashMap::new(),
            mode: "light".to_string(),
            node_meta,
        },
    )])));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::from([(
        swarm_id.to_string(),
        requester.clone(),
    )])));
    let event_history = Arc::new(RwLock::new(VecDeque::new()));
    let event_counter = Arc::new(AtomicU64::new(1));
    let (swarm_event_tx, _swarm_event_rx) = broadcast::channel(32);

    (
        AssignNextReservationContext {
            requester,
            client_tx,
            sessions,
            global_session_id: Arc::new(RwLock::new(String::new())),
            provider: Arc::new(TestProvider),
            soft_interrupt_queues,
            client_connections,
            swarm_members,
            swarms_by_id,
            swarm_plans,
            swarm_coordinators,
            event_history,
            event_counter,
            swarm_event_tx,
            mcp_pool: Arc::new(crate::mcp::SharedMcpPool::from_default_config()),
            mutation_runtime: SwarmMutationRuntime::default(),
        },
        client_rx,
    )
}

fn scoped_write(id: &str, scope: &str) -> PlanItem {
    let mut item = plan_item(id, "queued", "high", &[]);
    item.file_scope = vec![scope.to_string()];
    item
}

#[tokio::test]
async fn assign_next_task_reservation_never_expires_and_releases_by_claim_id() {
    let (_env, _runtime) = RuntimeEnvGuard::new();
    let swarm_id = "swarm-assign-next-no-reservation-expiry";
    let (context, _) =
        assign_next_reservation_context(swarm_id, vec![scoped_write("write", "src/lib.rs")], &[])
            .await;
    let plan = context.swarm_plans.read().await[swarm_id].clone();
    let reservation = reserve_next_unassigned_runnable_task(swarm_id, &plan)
        .expect("first request reserves the runnable task");
    let key = assign_next_task_reservation_key(swarm_id, "write");
    let first_claim_id = reservation.claim_id;

    {
        let mut reservations = assign_next_task_reservations()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reservations
            .get_mut(&key)
            .expect("reservation is live")
            .claimed_at = Instant::now() - Duration::from_secs(16);
    }
    assert!(
        reserve_next_unassigned_runnable_task(swarm_id, &plan).is_none(),
        "a live reservation older than the former 15-second TTL must still block assignment"
    );

    let replacement_claim_id = first_claim_id + 1_000_000;
    {
        let mut reservations = assign_next_task_reservations()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reservations.insert(
            key.clone(),
            AssignNextTaskReservationState {
                swarm_id: swarm_id.to_string(),
                task_id: "write".to_string(),
                claim_id: replacement_claim_id,
                claimed_at: Instant::now(),
                file_scope: vec!["src/lib.rs".to_string()],
                mutating: true,
            },
        );
    }
    release_assign_next_task_reservation(swarm_id, "write", first_claim_id);
    assert_eq!(
        assign_next_task_reservations()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
            .map(|reservation| reservation.claim_id),
        Some(replacement_claim_id),
        "an older guard must not release a newer claim"
    );

    release_assign_next_task_reservation(swarm_id, "write", replacement_claim_id);
    drop(reservation);
}

#[tokio::test]
async fn concurrent_assign_next_reserves_task_and_overlapping_scope_before_spawn() {
    let (_env, _runtime) = RuntimeEnvGuard::new();
    let swarm_id = "swarm-assign-next-overlap-reservation";
    let (context, mut client_rx) = assign_next_reservation_context(
        swarm_id,
        vec![
            scoped_write("a-write", "src"),
            scoped_write("b-overlap", "src/lib.rs"),
        ],
        &["worker"],
    )
    .await;
    let (mut entered, release, _hook_guard) = install_assign_next_reservation_test_hook();

    let first = tokio::spawn(context.clone().assign_next(501));
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), entered.recv())
            .await
            .expect("first task reaches reservation pause"),
        Some("a-write".to_string())
    );

    let second = tokio::spawn(context.clone().assign_next(502));
    match tokio::time::timeout(std::time::Duration::from_secs(2), client_rx.recv())
        .await
        .expect("overlapping request responds")
        .expect("overlapping response")
    {
        ServerEvent::Error { id, message, .. } => {
            assert_eq!(id, 502);
            assert!(
                message.contains("No runnable unassigned tasks"),
                "{message}"
            );
        }
        other => panic!("expected overlapping assign_next rejection, got {other:?}"),
    }
    second.await.expect("second handler finishes");

    release.send(()).expect("release first reservation pause");
    first.await.expect("first handler finishes");
    match client_rx.recv().await.expect("first response") {
        ServerEvent::CommAssignTaskResponse {
            id,
            task_id,
            target_session,
        } => {
            assert_eq!(id, 501);
            assert_eq!(task_id, "a-write");
            assert!(!target_session.is_empty());
        }
        other => panic!("expected first assignment response, got {other:?}"),
    }

    {
        let plans = context.swarm_plans.read().await;
        assert_eq!(
            plans[swarm_id]
                .items
                .iter()
                .filter(|item| item.assigned_to.is_some())
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a-write"]
        );
    }

    let members = context.swarm_members.read().await;
    assert_eq!(members.len(), 2, "no extra worker may be created");
    assert_eq!(
        members
            .values()
            .filter(|member| member.session_id != "coord")
            .count(),
        1,
        "the rejected overlapping request must not leave an unassigned worker"
    );
}

#[tokio::test]
async fn concurrent_assign_next_keeps_disjoint_write_scopes_parallel() {
    let (_env, _runtime) = RuntimeEnvGuard::new();
    let swarm_id = "swarm-assign-next-disjoint-reservation";
    let (context, mut client_rx) = assign_next_reservation_context(
        swarm_id,
        vec![
            scoped_write("a-left", "src/left"),
            scoped_write("b-right", "src/right"),
        ],
        &["worker-left", "worker-right"],
    )
    .await;
    let (mut entered, release, _hook_guard) = install_assign_next_reservation_test_hook();

    let first = tokio::spawn(context.clone().assign_next(511));
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), entered.recv())
            .await
            .expect("first task reaches reservation pause"),
        Some("a-left".to_string())
    );
    let second = tokio::spawn(context.clone().assign_next(512));
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), entered.recv())
            .await
            .expect("disjoint task reaches reservation pause"),
        Some("b-right".to_string())
    );

    release.send(()).expect("release first reservation pause");
    release.send(()).expect("release second reservation pause");
    first.await.expect("first handler finishes");
    second.await.expect("second handler finishes");

    let mut assigned_ids = Vec::new();
    for _ in 0..2 {
        match client_rx.recv().await.expect("assignment response") {
            ServerEvent::CommAssignTaskResponse { task_id, .. } => assigned_ids.push(task_id),
            other => panic!("expected disjoint assignment response, got {other:?}"),
        }
    }
    assigned_ids.sort();
    assert_eq!(assigned_ids, vec!["a-left", "b-right"]);

    let plan = context.swarm_plans.read().await;
    assert_eq!(
        plan[swarm_id]
            .items
            .iter()
            .filter(|item| item.assigned_to.is_some())
            .count(),
        2
    );
}
