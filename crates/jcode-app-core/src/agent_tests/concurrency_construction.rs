use super::*;

struct IsolatedTelemetryEnv {
    _home: tempfile::TempDir,
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl IsolatedTelemetryEnv {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let previous = ["JCODE_HOME", "JCODE_NO_TELEMETRY"]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
        crate::env::set_var("JCODE_HOME", home.path());
        crate::env::set_var("JCODE_NO_TELEMETRY", "1");
        Self {
            _home: home,
            previous,
        }
    }
}

impl Drop for IsolatedTelemetryEnv {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..) {
            match value {
                Some(value) => crate::env::set_var(key, value),
                None => crate::env::remove_var(key),
            }
        }
    }
}

#[tokio::test]
async fn provisional_connection_does_not_track_until_logical_ownership_commits() {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedTelemetryEnv::new();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new_provisional_with_initial_working_dir(provider, registry, None);
    assert!(
        !agent.has_concurrency_tracking(),
        "a viewer placeholder is not a live logical session"
    );
    agent.activate_concurrency_tracking();
    assert!(
        agent.has_concurrency_tracking(),
        "idle committed sessions must count before their first turn"
    );
    let first_guard = format!("{:?}", agent.concurrency_session);
    agent.activate_concurrency_tracking();
    assert_eq!(
        format!("{:?}", agent.concurrency_session),
        first_guard,
        "repeated subscribe must not create another incarnation"
    );
    agent.mark_closed();
    assert!(!agent.has_concurrency_tracking());
}

#[tokio::test]
async fn headless_parent_is_set_before_concurrency_tracking_begins() {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedTelemetryEnv::new();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let child = Agent::new_with_parent_and_initial_working_dir(
        provider.clone(),
        registry,
        None,
        Some("coordinator-session".to_owned()),
    );
    assert_eq!(
        child.session.parent_id.as_deref(),
        Some("coordinator-session")
    );
    assert!(format!("{:?}", child.concurrency_session).contains("child: true"));
    let registry = Registry::new(provider.clone()).await;
    let root = Agent::new_with_parent_and_initial_working_dir(provider, registry, None, None);
    assert!(root.session.parent_id.is_none());
    assert!(format!("{:?}", root.concurrency_session).contains("child: false"));
}

struct OriginOrderingProvider {
    inspect: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait::async_trait]
impl Provider for OriginOrderingProvider {
    async fn complete(
        &self,
        _: &[crate::message::Message],
        _: &[crate::message::ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> anyhow::Result<crate::provider::EventStream> {
        panic!("no real provider calls")
    }
    fn name(&self) -> &str {
        "origin-ordering-test"
    }
    fn model(&self) -> String {
        if self
            .inspect
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            let sessions = std::fs::read_dir(crate::storage::jcode_dir().unwrap().join("sessions"))
                .expect("initial snapshot must precede build_base");
            let paths: Vec<_> = sessions
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                .collect();
            assert_eq!(paths.len(), 1);
            let snapshot: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&paths[0]).unwrap()).unwrap();
            assert_eq!(snapshot["origin"], "swarm_worker");
            let active = crate::storage::jcode_dir().unwrap().join("active_pids");
            assert!(
                !active.exists() || std::fs::read_dir(active).unwrap().next().is_none(),
                "snapshot precedes mark_active publication"
            );
            assert_eq!(snapshot["working_dir"], "/worker-origin-cwd");
            assert_eq!(snapshot["parent_id"], "origin-coordinator");
            assert!(
                snapshot["messages"].as_array().unwrap().is_empty(),
                "snapshot precedes context and environment hooks"
            );
        }
        "test-model".to_string()
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            inspect: self.inspect.clone(),
        })
    }
}

#[tokio::test]
async fn worker_origin_snapshot_precedes_agent_initialization() {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedTelemetryEnv::new();
    let inspect = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let provider: Arc<dyn Provider> = Arc::new(OriginOrderingProvider {
        inspect: inspect.clone(),
    });
    let registry = Registry::new(provider.clone()).await;
    inspect.store(true, std::sync::atomic::Ordering::SeqCst);
    let agent = Agent::new_with_parent_and_initial_working_dir_and_origin(
        provider,
        registry,
        Some("/worker-origin-cwd"),
        Some("origin-coordinator".to_string()),
        crate::session::SessionOrigin::SwarmWorker,
    )
    .unwrap();
    assert!(!inspect.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        agent.session.origin(),
        crate::session::SessionOrigin::SwarmWorker
    );
    assert!(agent.has_concurrency_tracking());
}

#[tokio::test]
async fn worker_origin_save_failure_prevents_agent_initialization() {
    let _lock = crate::storage::lock_test_env();
    let env = IsolatedTelemetryEnv::new();
    let inspect = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let provider: Arc<dyn Provider> = Arc::new(OriginOrderingProvider {
        inspect: inspect.clone(),
    });
    let registry = Registry::new(provider.clone()).await;
    std::fs::write(env._home.path().join("sessions"), "block snapshots").unwrap();
    inspect.store(true, std::sync::atomic::Ordering::SeqCst);
    let result = Agent::new_with_parent_and_initial_working_dir_and_origin(
        provider,
        registry,
        None,
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    assert!(result.is_err());
    assert!(
        inspect.load(std::sync::atomic::Ordering::SeqCst),
        "build_base must not run"
    );
    let active = env._home.path().join("active_pids");
    assert!(!active.exists() || std::fs::read_dir(active).unwrap().next().is_none());
}

#[tokio::test]
async fn worker_origin_ordinary_wrappers_and_resumed_sessions_remain_unknown() {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedTelemetryEnv::new();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let ordinary = Agent::new(provider.clone(), Registry::new(provider.clone()).await);
    assert_eq!(
        ordinary.session.origin(),
        crate::session::SessionOrigin::Unknown
    );
    let provisional = Agent::new_provisional_with_initial_working_dir(
        provider.clone(),
        Registry::new(provider.clone()).await,
        None,
    );
    assert_eq!(
        provisional.session.origin(),
        crate::session::SessionOrigin::Unknown
    );
    let child = Agent::new_with_parent_and_initial_working_dir(
        provider.clone(),
        Registry::new(provider.clone()).await,
        None,
        Some(ordinary.session_id().to_string()),
    );
    assert_eq!(
        child.session.origin(),
        crate::session::SessionOrigin::Unknown
    );
    // A normal fork, even of a worker, is not itself a swarm-created worker.
    let worker = crate::session::Session::create_with_origin(
        None,
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    let mut fork = crate::session::Session::create(Some(worker.id.clone()), None);
    fork.append_fork_notice(&worker.id, "worker");
    fork.save().unwrap();
    let fork = Agent::new_with_session(
        provider.clone(),
        Registry::new(provider.clone()).await,
        fork,
        None,
    );
    assert_eq!(
        fork.session.origin(),
        crate::session::SessionOrigin::Unknown
    );
    assert_eq!(
        crate::session::Session::load(fork.session_id())
            .unwrap()
            .origin(),
        crate::session::SessionOrigin::Unknown
    );
    let old = crate::session::Session::create_with_id("legacy-reused".to_string(), None, None);
    let resumed = Agent::new_with_session(
        provider.clone(),
        Registry::new(provider.clone()).await,
        old,
        None,
    );
    assert_eq!(
        resumed.session.origin(),
        crate::session::SessionOrigin::Unknown
    );
}

#[tokio::test]
async fn worker_origin_same_id_resume_preserves_worker_origin() {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedTelemetryEnv::new();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let mut worker = crate::session::Session::create_with_origin(
        None,
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    worker.save().unwrap();
    let worker_id = worker.id.clone();

    let resumed = Agent::new_with_session(
        provider.clone(),
        Registry::new(provider).await,
        crate::session::Session::load(&worker_id).unwrap(),
        None,
    );

    assert_eq!(resumed.session_id(), worker_id);
    assert_eq!(resumed.session.origin(), crate::session::SessionOrigin::SwarmWorker);
}
