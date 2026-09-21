use super::*;
use crate::message::{ContentBlock, Message, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, Provider};
use async_trait::async_trait;
use futures::{stream, Stream};
use std::pin::Pin;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::task::{Context, Poll};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

struct IsolatedRuntimeDir {
    _prev_runtime: Option<std::ffi::OsString>,
    _temp: tempfile::TempDir,
}

struct IsolatedReloadRecoveryEnv {
    prev_home: Option<std::ffi::OsString>,
    prev_runtime: Option<std::ffi::OsString>,
    _home: tempfile::TempDir,
    _runtime: tempfile::TempDir,
}

struct AstraIngressTestEnv {
    previous_home: Option<std::ffi::OsString>,
    _home: tempfile::TempDir,
}

impl AstraIngressTestEnv {
    fn new(model: &str) -> Self {
        Self::new_with_effort(model, "high")
    }

    fn new_with_effort(model: &str, effort: &str) -> Self {
        let home = tempfile::TempDir::new().expect("create Astra ingress test home");
        std::fs::write(
            home.path().join("config.toml"),
            format!("[agents.astra_first]\nmodel = \"{model}\"\neffort = \"{effort}\"\n"),
        )
        .expect("write Astra ingress test config");
        let previous_home = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", home.path());
        crate::config::invalidate_config_cache();
        Self {
            previous_home,
            _home: home,
        }
    }
}

impl Drop for AstraIngressTestEnv {
    fn drop(&mut self) {
        match self.previous_home.take() {
            Some(value) => crate::env::set_var("JCODE_HOME", value),
            None => crate::env::remove_var("JCODE_HOME"),
        }
        crate::config::invalidate_config_cache();
    }
}

struct FastMatrixTestEnv {
    previous_home: Option<std::ffi::OsString>,
    _home: tempfile::TempDir,
}

impl FastMatrixTestEnv {
    fn new() -> Self {
        let home = tempfile::TempDir::new().expect("create fast matrix test home");
        std::fs::write(
            home.path().join("config.toml"),
            "[provider.openai_model_service_tiers]\n\"gpt-6-astra\" = \"priority\"\n\"gpt-5.6-luna\" = \"priority\"\n",
        )
            .expect("write fast matrix test config");
        let previous_home = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", home.path());
        crate::config::invalidate_config_cache();
        Self {
            previous_home,
            _home: home,
        }
    }
}

impl Drop for FastMatrixTestEnv {
    fn drop(&mut self) {
        match self.previous_home.take() {
            Some(value) => crate::env::set_var("JCODE_HOME", value),
            None => crate::env::remove_var("JCODE_HOME"),
        }
        crate::config::invalidate_config_cache();
    }
}

#[tokio::test]
async fn service_tier_snapshot_publication_is_limited_to_owning_main_entries() {
    let provider: Arc<dyn Provider> = Arc::new(CompleteImmediatelyProvider);
    let root_id = "fast-root";
    let worker_id = "fast-worker";
    let sibling_id = "fast-sibling";
    let root_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&provider),
        Registry::empty(),
        crate::session::Session::create_with_id(root_id.to_string(), None, None),
        None,
    )));
    let worker_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&provider),
        Registry::empty(),
        crate::session::Session::create_with_id(worker_id.to_string(), None, None),
        None,
    )));
    let sibling_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&provider),
        Registry::empty(),
        crate::session::Session::create_with_id(sibling_id.to_string(), None, None),
        None,
    )));
    let root_state = crate::server::RuntimeFastState::from_provider(root_id, provider.as_ref());
    let sibling_state =
        crate::server::RuntimeFastState::from_provider(sibling_id, provider.as_ref());
    let sessions: crate::server::SessionAgents = Arc::new(RwLock::new(HashMap::from([
        (
            root_id.to_string(),
            crate::server::SessionAgentEntry::new(root_agent, Arc::clone(&root_state)),
        ),
        (
            worker_id.to_string(),
            crate::server::SessionAgentEntry::new(worker_agent, Arc::clone(&root_state)),
        ),
        (
            sibling_id.to_string(),
            crate::server::SessionAgentEntry::new(sibling_agent, sibling_state),
        ),
    ])));

    assert!(
        super::owning_main_fast_state(&sessions, root_id)
            .await
            .is_some(),
        "the owning main may publish its own snapshot"
    );
    assert!(
        super::owning_main_fast_state(&sessions, worker_id)
            .await
            .is_none(),
        "a worker sharing the root handle must not publish root state"
    );
    assert!(
        super::owning_main_fast_state(&sessions, sibling_id)
            .await
            .is_some(),
        "a sibling main retains an independent publication handle"
    );
}

#[tokio::test]
async fn service_tier_request_path_publishes_only_for_owning_main() {
    let root_provider = Arc::new(RecordingServiceTierProvider::new("gpt-5.6-luna"));
    let root_provider_dyn: Arc<dyn Provider> = root_provider.clone();
    let root_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&root_provider_dyn),
        Registry::empty(),
        crate::session::Session::create_with_id("request-fast-root".to_string(), None, None),
        None,
    )));

    let worker_provider = Arc::new(RecordingServiceTierProvider::new("gpt-5.6-luna"));
    let worker_provider_dyn: Arc<dyn Provider> = worker_provider.clone();
    let mut worker_session = crate::session::Session::create_with_origin(
        Some("request-fast-root".to_string()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    worker_session.model = Some("gpt-5.6-luna".to_string());
    let worker_id = worker_session.id.clone();
    let worker_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&worker_provider_dyn),
        Registry::empty(),
        worker_session,
        None,
    )));

    let deferred_provider = Arc::new(RecordingServiceTierProvider::new("gpt-5.6-luna"));
    let deferred_provider_dyn: Arc<dyn Provider> = deferred_provider.clone();
    let mut deferred_session = crate::session::Session::create_with_origin(
        Some("request-fast-root".to_string()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    deferred_session.model = Some("gpt-5.6-luna".to_string());
    let deferred_id = deferred_session.id.clone();
    let deferred_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&deferred_provider_dyn),
        Registry::empty(),
        deferred_session,
        None,
    )));

    let sibling_provider = Arc::new(RecordingServiceTierProvider::new("gpt-5.6-luna"));
    let sibling_provider_dyn: Arc<dyn Provider> = sibling_provider.clone();
    let sibling_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&sibling_provider_dyn),
        Registry::empty(),
        crate::session::Session::create_with_id(
            "request-fast-sibling".to_string(),
            None,
            None,
        ),
        None,
    )));

    let root_state = crate::server::RuntimeFastState::from_provider(
        "request-fast-root",
        root_provider.as_ref(),
    );
    let sibling_state = crate::server::RuntimeFastState::from_provider(
        "request-fast-sibling",
        sibling_provider.as_ref(),
    );
    let sessions: crate::server::SessionAgents = Arc::new(RwLock::new(HashMap::from([
        (
            "request-fast-root".to_string(),
            crate::server::SessionAgentEntry::new(Arc::clone(&root_agent), Arc::clone(&root_state)),
        ),
        (
            worker_id.clone(),
            crate::server::SessionAgentEntry::new(Arc::clone(&worker_agent), Arc::clone(&root_state)),
        ),
        (
            deferred_id.clone(),
            crate::server::SessionAgentEntry::new(
                Arc::clone(&deferred_agent),
                Arc::clone(&root_state),
            ),
        ),
        (
            "request-fast-sibling".to_string(),
            crate::server::SessionAgentEntry::new(Arc::clone(&sibling_agent), Arc::clone(&sibling_state)),
        ),
    ])));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    let root_fast_state = super::owning_main_fast_state(&sessions, "request-fast-root").await;
    assert!(root_fast_state.is_some());
    crate::server::provider_control::handle_set_service_tier(
        910,
        "priority".to_string(),
        &root_agent,
        root_fast_state,
        &client_event_tx,
    )
    .await;
    expect_successful_service_tier_ack(&mut client_event_rx, 910).await;
    assert_eq!(root_provider.service_tier().as_deref(), Some("priority"));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Priority);

    let root_fast_state = super::owning_main_fast_state(&sessions, "request-fast-root").await;
    assert!(root_fast_state.is_some());
    crate::server::provider_control::handle_set_service_tier(
        911,
        "off".to_string(),
        &root_agent,
        root_fast_state,
        &client_event_tx,
    )
    .await;
    expect_successful_service_tier_ack(&mut client_event_rx, 911).await;
    assert_eq!(root_provider.service_tier().as_deref(), Some("off"));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);

    let worker_fast_state = super::owning_main_fast_state(&sessions, &worker_id).await;
    assert!(worker_fast_state.is_none());
    crate::server::provider_control::handle_set_service_tier(
        912,
        "priority".to_string(),
        &worker_agent,
        worker_fast_state,
        &client_event_tx,
    )
    .await;
    expect_successful_service_tier_ack(&mut client_event_rx, 912).await;
    assert_eq!(worker_provider.service_tier().as_deref(), Some("priority"));
    assert_eq!(root_provider.service_tier().as_deref(), Some("off"));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);
    assert_eq!(sibling_provider.service_tier(), None);
    assert_eq!(sibling_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);

    let deferred_fast_state = super::owning_main_fast_state(&sessions, &deferred_id).await;
    assert!(deferred_fast_state.is_none());
    let busy_deferred_agent = deferred_agent.lock().await;
    crate::server::provider_control::handle_set_service_tier(
        913,
        "priority".to_string(),
        &deferred_agent,
        deferred_fast_state,
        &client_event_tx,
    )
    .await;
    assert!(client_event_rx.try_recv().is_err());
    assert_eq!(root_provider.service_tier().as_deref(), Some("off"));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);
    drop(busy_deferred_agent);

    expect_successful_service_tier_ack(&mut client_event_rx, 913).await;
    assert_eq!(deferred_provider.service_tier().as_deref(), Some("priority"));
    assert_eq!(root_provider.service_tier().as_deref(), Some("off"));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);
    assert_eq!(sibling_provider.service_tier(), None);
    assert_eq!(sibling_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);

    let astra_provider = RecordingServiceTierProvider::new("gpt-6-astra");
    let mut astra_child = crate::session::Session::create_with_origin(
        Some("request-fast-root".to_string()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    astra_child.model = Some("gpt-6-astra".to_string());
    assert!(crate::server::apply_runtime_fast_policy(
        &astra_provider,
        &astra_child,
        Some(&root_state),
    )
    .expect("Astra child should apply the root snapshot"));
    assert_eq!(astra_provider.scoped_override(), Some(None));
}

async fn expect_successful_service_tier_ack(
    client_event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<ServerEvent>,
    id: u64,
) {
    let error = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match client_event_rx.recv().await {
                Some(ServerEvent::ServiceTierChanged {
                    id: event_id, error, ..
                }) if event_id == id => break error,
                Some(_) => continue,
                None => panic!("service-tier event channel closed before request {id}"),
            }
        }
    })
    .await
    .expect("service-tier request acknowledgement timeout");
    assert!(error.is_none(), "service-tier request {id} failed: {error:?}");
}

#[tokio::test]
async fn closed_session_rejects_no_reply_context_message() {
    let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
        forked: Arc::new(AtomicBool::new(false)),
    });
    let registry = Registry::new(Arc::clone(&provider)).await;
    let mut session =
        crate::session::Session::create_with_id("session_closed_context".to_string(), None, None);
    session.mark_closed();
    let agent = Arc::new(Mutex::new(Agent::new_with_session(
        provider, registry, session, None,
    )));
    agent.lock().await.mark_closed();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();

    append_context_message(
        41,
        "must not be appended",
        Vec::new(),
        "session_closed_context",
        false,
        &agent,
        &event_tx,
    )
    .await;

    assert!(matches!(
        event_rx.recv().await,
        Some(ServerEvent::Error { id: 41, message, .. }) if message == "Session is closed"
    ));
    assert_eq!(agent.lock().await.visible_conversation_message_count(), 0);
}

#[tokio::test]
async fn resume_source_working_dir_does_not_wait_for_busy_agent_lock() {
    let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
        forked: Arc::new(AtomicBool::new(false)),
    });
    let registry = Registry::new(Arc::clone(&provider)).await;
    let mut session = crate::session::Session::create_with_id(
        "session_busy_resume_source".to_string(),
        None,
        None,
    );
    session.working_dir = Some("/workspace/busy-resume-source".to_string());
    let agent = Arc::new(Mutex::new(Agent::new_with_session(
        provider, registry, session, None,
    )));
    let _busy_agent_lock = agent.lock().await;

    let working_dir = tokio::time::timeout(Duration::from_millis(100), async {
        resume_source_working_dir(&agent, Some("/workspace/busy-resume-source".to_string()))
    })
    .await
    .expect("switching away from a busy session must not wait for its Agent mutex");

    assert_eq!(
        working_dir.as_deref(),
        Some("/workspace/busy-resume-source")
    );
}

#[tokio::test]
async fn socket_request_set_service_tier_uses_worker_identity_without_publishing_root() {
    let root_provider = Arc::new(RecordingServiceTierProvider::new("gpt-5.6-luna"));
    let provider_template: Arc<dyn Provider> = root_provider.clone();
    let (client_stream, server_task, sessions) =
        start_astra_ingress_probe_client(provider_template).await;
    let (client_reader, mut client_writer) = client_stream.into_split();
    let mut client_reader = BufReader::new(client_reader);
    let working_dir = std::env::current_dir()
        .expect("test working directory")
        .to_string_lossy()
        .into_owned();

    let subscribe = Request::Subscribe {
        id: 1,
        working_dir: Some(working_dir.clone()),
        selfdev: None,
        target_session_id: None,
        client_instance_id: None,
        client_has_local_history: false,
        allow_session_takeover: false,
        crash_on_disconnect: false,
        continue_on_disconnect: false,
        terminal_env: Vec::new(),
    };
    client_writer
        .write_all((serde_json::to_string(&subscribe).expect("serialize root Subscribe") + "\n").as_bytes())
        .await
        .expect("write root Subscribe");

    let mut line = String::new();
    loop {
        line.clear();
        let event = tokio::time::timeout(Duration::from_secs(2), client_reader.read_line(&mut line))
            .await
            .expect("root Subscribe response timeout")
            .expect("read root Subscribe response");
        assert!(event > 0, "server closed before root Subscribe completed");
        if matches!(decode_request_or_event(&line), ServerEvent::Done { id: 1 }) {
            break;
        }
    }

    let root_id = {
        let sessions_guard = sessions.read().await;
        assert_eq!(sessions_guard.len(), 1, "root Subscribe should create one session");
        sessions_guard
            .keys()
            .next()
            .cloned()
            .expect("root session id")
    };
    let root_state = sessions
        .read()
        .await
        .get(&root_id)
        .expect("root session entry")
        .fast_state();

    let root_off = Request::SetServiceTier {
        id: 2,
        service_tier: "off".to_string(),
    };
    client_writer
        .write_all((serde_json::to_string(&root_off).expect("serialize root service tier") + "\n").as_bytes())
        .await
        .expect("write root service tier");
    loop {
        line.clear();
        let event = tokio::time::timeout(Duration::from_secs(2), client_reader.read_line(&mut line))
            .await
            .expect("root service-tier response timeout")
            .expect("read root service-tier response");
        assert!(event > 0, "server closed before root service-tier response");
        match decode_request_or_event(&line) {
            ServerEvent::ServiceTierChanged { id: 2, error, .. } => {
                assert!(error.is_none(), "root service-tier request failed: {error:?}");
                break;
            }
            _ => {}
        }
    }
    assert_eq!(root_provider.service_tier().as_deref(), Some("off"));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);

    let sibling_provider = Arc::new(RecordingServiceTierProvider::new("gpt-5.6-luna"));
    let sibling_provider_dyn: Arc<dyn Provider> = sibling_provider.clone();
    let sibling_id = "socket-request-sibling".to_string();
    let sibling_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&sibling_provider_dyn),
        Registry::empty(),
        crate::session::Session::create_with_id(sibling_id.clone(), None, None),
        None,
    )));
    let sibling_state = crate::server::RuntimeFastState::from_provider(
        sibling_id.as_str(),
        sibling_provider.as_ref(),
    );

    let worker_provider = Arc::new(RecordingServiceTierProvider::new("gpt-5.6-luna"));
    let worker_provider_dyn: Arc<dyn Provider> = worker_provider.clone();
    let worker_id = "socket-request-worker".to_string();
    let mut worker_session = crate::session::Session::create_with_origin(
        Some(root_id.clone()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    worker_session.id = worker_id.clone();
    worker_session.model = Some("gpt-5.6-luna".to_string());
    let worker_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&worker_provider_dyn),
        Registry::empty(),
        worker_session,
        None,
    )));
    {
        let mut sessions_guard = sessions.write().await;
        sessions_guard.insert(
            worker_id.clone(),
            crate::server::SessionAgentEntry::new(worker_agent, Arc::clone(&root_state)),
        );
        sessions_guard.insert(
            sibling_id,
            crate::server::SessionAgentEntry::new(sibling_agent, Arc::clone(&sibling_state)),
        );
    }

    let worker_subscribe = Request::Subscribe {
        id: 3,
        working_dir: Some(working_dir),
        selfdev: None,
        target_session_id: Some(worker_id.clone()),
        client_instance_id: None,
        client_has_local_history: false,
        allow_session_takeover: false,
        crash_on_disconnect: false,
        continue_on_disconnect: false,
        terminal_env: Vec::new(),
    };
    client_writer
        .write_all(
            (serde_json::to_string(&worker_subscribe).expect("serialize worker Subscribe") + "\n")
                .as_bytes(),
        )
        .await
        .expect("write worker Subscribe");
    loop {
        line.clear();
        let event = tokio::time::timeout(Duration::from_secs(2), client_reader.read_line(&mut line))
            .await
            .expect("worker Subscribe response timeout")
            .expect("read worker Subscribe response");
        assert!(event > 0, "server closed before worker Subscribe completed");
        if matches!(decode_request_or_event(&line), ServerEvent::Done { id: 3 }) {
            break;
        }
    }

    let worker_priority = Request::SetServiceTier {
        id: 4,
        service_tier: "priority".to_string(),
    };
    client_writer
        .write_all(
            (serde_json::to_string(&worker_priority).expect("serialize worker service tier") + "\n")
                .as_bytes(),
        )
        .await
        .expect("write worker service tier");
    loop {
        line.clear();
        let event = tokio::time::timeout(Duration::from_secs(2), client_reader.read_line(&mut line))
            .await
            .expect("worker service-tier response timeout")
            .expect("read worker service-tier response");
        assert!(event > 0, "server closed before worker service-tier response");
        match decode_request_or_event(&line) {
            ServerEvent::ServiceTierChanged { id: 4, error, .. } => {
                assert!(error.is_none(), "worker service-tier request failed: {error:?}");
                break;
            }
            _ => {}
        }
    }
    assert_eq!(worker_provider.service_tier().as_deref(), Some("priority"));
    assert_eq!(root_provider.service_tier().as_deref(), Some("off"));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);
    assert_eq!(sibling_provider.service_tier(), None);
    assert_eq!(sibling_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);

    drop(client_writer);
    server_task
        .await
        .expect("socket request server task join")
        .expect("socket request server task result");
}

fn fast_matrix_subscribe(id: u64, target_session_id: &str, working_dir: &str) -> Request {
    Request::Subscribe {
        id,
        working_dir: Some(working_dir.to_string()),
        selfdev: None,
        target_session_id: Some(target_session_id.to_string()),
        client_instance_id: None,
        client_has_local_history: false,
        allow_session_takeover: false,
        crash_on_disconnect: false,
        continue_on_disconnect: false,
        terminal_env: Vec::new(),
    }
}

async fn send_fast_matrix_request<W, R>(
    writer: &mut W,
    reader: &mut BufReader<R>,
    request: Request,
    expected_id: u64,
) where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    writer
        .write_all((serde_json::to_string(&request).expect("serialize fast matrix request") + "\n").as_bytes())
        .await
        .expect("write fast matrix request");
    let mut line = String::new();
    loop {
        line.clear();
        let read = tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
            .await
            .expect("fast matrix response timeout")
            .expect("read fast matrix response");
        assert!(read > 0, "server closed during fast matrix request {expected_id}");
        match decode_request_or_event(&line) {
            ServerEvent::Done { id } if id == expected_id => return,
            ServerEvent::Error { id, message, .. } if id == expected_id => {
                panic!("fast matrix request {expected_id} failed: {message}")
            }
            _ => {}
        }
    }
}

async fn send_fast_matrix_service_tier_request<W, R>(
    writer: &mut W,
    reader: &mut BufReader<R>,
    request: Request,
    expected_id: u64,
    expected_tier: &str,
) where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    writer
        .write_all((serde_json::to_string(&request).expect("serialize fast matrix request") + "\n").as_bytes())
        .await
        .expect("write fast matrix request");
    let mut line = String::new();
    loop {
        line.clear();
        let read = tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
            .await
            .expect("fast matrix response timeout")
            .expect("read fast matrix response");
        assert!(read > 0, "server closed during fast matrix request {expected_id}");
        match decode_request_or_event(&line) {
            ServerEvent::ServiceTierChanged {
                id,
                service_tier,
                error,
            } if id == expected_id => {
                assert!(error.is_none(), "service tier request {expected_id} failed: {error:?}");
                assert_eq!(service_tier.as_deref(), Some(expected_tier));
                return;
            }
            ServerEvent::Error { id, message, .. } if id == expected_id => {
                panic!("fast matrix request {expected_id} failed: {message}")
            }
            _ => {}
        }
    }
}

async fn send_fast_matrix_reasoning_effort_request<W, R>(
    writer: &mut W,
    reader: &mut BufReader<R>,
    request: Request,
    expected_id: u64,
    expected_effort: &str,
) where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    writer
        .write_all((serde_json::to_string(&request).expect("serialize effort request") + "\n").as_bytes())
        .await
        .expect("write effort request");
    let mut line = String::new();
    loop {
        line.clear();
        let read = tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
            .await
            .expect("effort response timeout")
            .expect("read effort response");
        assert!(read > 0, "server closed during effort request {expected_id}");
        match decode_request_or_event(&line) {
            ServerEvent::ReasoningEffortChanged { id, effort, error } if id == expected_id => {
                assert!(error.is_none(), "effort request {expected_id} failed: {error:?}");
                assert_eq!(effort.as_deref(), Some(expected_effort));
                return;
            }
            ServerEvent::Error { id, message, .. } if id == expected_id => {
                panic!("effort request {expected_id} failed: {message}")
            }
            _ => {}
        }
    }
}

async fn send_fast_matrix_model_request<W, R>(
    writer: &mut W,
    reader: &mut BufReader<R>,
    request: Request,
    expected_id: u64,
    expected_model: &str,
) where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    writer
        .write_all((serde_json::to_string(&request).expect("serialize model request") + "\n").as_bytes())
        .await
        .expect("write model request");
    let mut line = String::new();
    loop {
        line.clear();
        let read = tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
            .await
            .expect("model response timeout")
            .expect("read model response");
        assert!(read > 0, "server closed during model request {expected_id}");
        match decode_request_or_event(&line) {
            ServerEvent::ModelChanged {
                id,
                model,
                error,
                ..
            } if id == expected_id => {
                assert!(error.is_none(), "model request {expected_id} failed: {error:?}");
                assert_eq!(model, expected_model);
                return;
            }
            ServerEvent::Error { id, message, .. } if id == expected_id => {
                panic!("model request {expected_id} failed: {message}")
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn fast_matrix_off_on_off_reuses_existing_root_astra_and_luna_workers() {
    let _storage = crate::storage::lock_test_env();
    let _env = FastMatrixTestEnv::new();
    let root_provider = Arc::new(RecordingServiceTierProvider::new_with_service_tier(
        "gpt-5.6-luna",
        Some("off"),
    ));
    let provider_template: Arc<dyn Provider> = root_provider.clone();
    let (client_stream, server_task, sessions) =
        start_astra_ingress_probe_client(provider_template).await;
    let (client_reader, mut client_writer) = client_stream.into_split();
    let mut client_reader = BufReader::new(client_reader);
    let working_dir = std::env::current_dir()
        .expect("test working directory")
        .to_string_lossy()
        .into_owned();

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Subscribe {
            id: 1,
            working_dir: Some(working_dir.clone()),
            selfdev: None,
            target_session_id: None,
            client_instance_id: None,
            client_has_local_history: false,
            allow_session_takeover: false,
            crash_on_disconnect: false,
            continue_on_disconnect: false,
            terminal_env: Vec::new(),
        },
        1,
    )
    .await;

    let root_id = {
        let sessions_guard = sessions.read().await;
        assert_eq!(sessions_guard.len(), 1, "root Subscribe should create one session");
        sessions_guard
            .keys()
            .next()
            .cloned()
            .expect("root session id")
    };
    let root_state = sessions
        .read()
        .await
        .get(&root_id)
        .expect("root session entry")
        .fast_state();

    let astra_provider = Arc::new(RecordingServiceTierProvider::new_with_service_tier(
        "gpt-6-astra",
        Some("priority"),
    ));
    let astra_provider_dyn: Arc<dyn Provider> = astra_provider.clone();
    let mut astra_session = crate::session::Session::create_with_origin(
        Some(root_id.clone()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    astra_session.model = Some("gpt-6-astra".to_string());
    astra_session.working_dir = Some(working_dir.clone());
    let astra_id = astra_session.id.clone();
    let astra_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&astra_provider_dyn),
        Registry::empty(),
        astra_session,
        None,
    )));

    let luna_provider = Arc::new(RecordingServiceTierProvider::new_with_service_tier(
        "gpt-5.6-luna",
        Some("priority"),
    ));
    let luna_provider_dyn: Arc<dyn Provider> = luna_provider.clone();
    let mut luna_session = crate::session::Session::create_with_origin(
        Some(root_id.clone()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    luna_session.model = Some("gpt-5.6-luna".to_string());
    luna_session.working_dir = Some(working_dir.clone());
    let luna_id = luna_session.id.clone();
    let luna_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&luna_provider_dyn),
        Registry::empty(),
        luna_session,
        None,
    )));

    {
        let mut sessions_guard = sessions.write().await;
        sessions_guard.insert(
            astra_id.clone(),
            crate::server::SessionAgentEntry::new(Arc::clone(&astra_agent), Arc::clone(&root_state)),
        );
        sessions_guard.insert(
            luna_id.clone(),
            crate::server::SessionAgentEntry::new(Arc::clone(&luna_agent), Arc::clone(&root_state)),
        );
    }

    send_fast_matrix_service_tier_request(
        &mut client_writer,
        &mut client_reader,
        Request::SetServiceTier {
            id: 2,
            service_tier: "off".to_string(),
        },
        2,
        "off",
    )
    .await;
    assert_eq!(root_provider.service_tier().as_deref(), Some("off"));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Message {
            id: 3,
            content: "root off admission".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
        3,
    )
    .await;
    assert_eq!(root_provider.scoped_override(), Some(None));

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        fast_matrix_subscribe(4, &astra_id, &working_dir),
        4,
    )
    .await;
    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Message {
            id: 5,
            content: "astra off admission".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
        5,
    )
    .await;
    assert_eq!(astra_provider.scoped_override(), Some(None));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        fast_matrix_subscribe(6, &luna_id, &working_dir),
        6,
    )
    .await;
    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Message {
            id: 7,
            content: "luna off admission".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
        7,
    )
    .await;
    assert_eq!(luna_provider.scoped_override(), Some(Some("priority".to_string())));

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        fast_matrix_subscribe(8, &root_id, &working_dir),
        8,
    )
    .await;
    send_fast_matrix_service_tier_request(
        &mut client_writer,
        &mut client_reader,
        Request::SetServiceTier {
            id: 9,
            service_tier: "priority".to_string(),
        },
        9,
        "priority",
    )
    .await;
    assert_eq!(root_provider.service_tier().as_deref(), Some("priority"));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Priority);

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        fast_matrix_subscribe(10, &astra_id, &working_dir),
        10,
    )
    .await;
    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Message {
            id: 11,
            content: "astra on admission".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
        11,
    )
    .await;
    assert_eq!(astra_provider.scoped_override(), Some(Some("priority".to_string())));

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        fast_matrix_subscribe(12, &luna_id, &working_dir),
        12,
    )
    .await;
    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Message {
            id: 13,
            content: "luna on admission".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
        13,
    )
    .await;
    assert_eq!(luna_provider.scoped_override(), Some(Some("priority".to_string())));

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        fast_matrix_subscribe(14, &root_id, &working_dir),
        14,
    )
    .await;
    send_fast_matrix_service_tier_request(
        &mut client_writer,
        &mut client_reader,
        Request::SetServiceTier {
            id: 15,
            service_tier: "off".to_string(),
        },
        15,
        "off",
    )
    .await;
    assert_eq!(root_provider.service_tier().as_deref(), Some("off"));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        fast_matrix_subscribe(16, &astra_id, &working_dir),
        16,
    )
    .await;
    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Message {
            id: 17,
            content: "astra off again admission".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
        17,
    )
    .await;
    assert_eq!(astra_provider.scoped_override(), Some(None));

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        fast_matrix_subscribe(18, &luna_id, &working_dir),
        18,
    )
    .await;
    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Message {
            id: 19,
            content: "luna off again admission".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
        19,
    )
    .await;
    assert_eq!(luna_provider.scoped_override(), Some(Some("priority".to_string())));
    root_provider.assert_dispatch_sequence(&[("root off admission", Some(None))]);
    astra_provider.assert_dispatch_sequence(&[
        ("astra off admission", Some(None)),
        ("astra on admission", Some(Some("priority".to_string()))),
        ("astra off again admission", Some(None)),
    ]);
    luna_provider.assert_dispatch_sequence(&[
        ("luna off admission", Some(Some("priority".to_string()))),
        ("luna on admission", Some(Some("priority".to_string()))),
        ("luna off again admission", Some(Some("priority".to_string()))),
    ]);

    let sessions_guard = sessions.read().await;
    assert_eq!(sessions_guard.len(), 3, "the same root and two workers must remain resident");
    let root_agent = sessions_guard
        .get(&root_id)
        .expect("root remains resident")
        .agent();
    assert_ne!(
        root_agent.lock().await.session_for_split().origin(),
        crate::session::SessionOrigin::SwarmWorker,
        "root must never be classified as a worker"
    );
    assert!(Arc::ptr_eq(
        &sessions_guard
            .get(&astra_id)
            .expect("Astra worker remains resident")
            .agent(),
        &astra_agent
    ));
    assert!(Arc::ptr_eq(
        &sessions_guard
            .get(&luna_id)
            .expect("Luna worker remains resident")
            .agent(),
        &luna_agent
    ));

    drop(sessions_guard);
    drop(client_writer);
    server_task
        .await
        .expect("fast matrix server task join")
        .expect("fast matrix server task result");
}

#[tokio::test]
async fn cold_restore_rebinds_a_fresh_astra_state_for_the_new_root_session() {
    let _storage_guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("test JCODE_HOME");
    let previous_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", home.path());

    let provider: Arc<dyn Provider> = Arc::new(CompleteImmediatelyProvider);
    let registry = Registry::new(Arc::clone(&provider)).await;
    let old_session = crate::session::Session::create_with_id(
        "session_cold_restore_old_root".to_string(),
        None,
        None,
    );
    let mut agent = Agent::new_with_session(provider, registry, old_session, None);
    let old_state = agent.astra_first_state_handle();
    {
        let mut state = old_state.lock().await;
        state.generation = 17;
        state.analyst_id = Some("stale-analyst".to_string());
        state.analyst_model = Some("stale-model".to_string());
    }

    let mut restored_session = crate::session::Session::create_with_id(
        "session_cold_restore_new_root".to_string(),
        None,
        None,
    );
    restored_session.title = Some("cold restore fixture".to_string());
    restored_session.save().expect("save cold restore session");
    agent
        .restore_session(&restored_session.id)
        .expect("cold restore should succeed");

    let fresh_state = agent.astra_first_state_handle();
    assert!(
        !Arc::ptr_eq(&old_state, &fresh_state),
        "a restored runtime session must receive a fresh Astra state handle"
    );
    let state = fresh_state.lock().await;
    assert_eq!(state.session_id, restored_session.id);
    assert_eq!(state.generation, 0);
    assert!(state.analyst_id.is_none());
    assert!(state.analyst_model.is_none());
    assert!(state.analyst_route.is_none());
    assert!(state.analyst_effort.is_none());
    assert!(!state.busy);
    assert!(!state.cancelled);
    assert_eq!(
        state.enabled,
        crate::config::config().agents.astra_first.is_some(),
        "the restored root keeps the configured root-only Astra enablement"
    );
    drop(state);

    let same_provider: Arc<dyn Provider> = Arc::new(CompleteImmediatelyProvider);
    let same_registry = Registry::new(Arc::clone(&same_provider)).await;
    let same_session_id = "session_cold_restore_same_root";
    let same_session =
        crate::session::Session::create_with_id(same_session_id.to_string(), None, None);
    let mut same_agent = Agent::new_with_session(same_provider, same_registry, same_session, None);
    let same_state = same_agent.astra_first_state_handle();
    {
        let mut state = same_state.lock().await;
        state.generation = 11;
        state.analyst_id = Some("same-id-analyst".to_string());
        state.analyst_model = Some("same-id-model".to_string());
        state.analyst_route = Some("same-id-route".to_string());
        state.analyst_effort = Some("high".to_string());
        state.busy = false;
        state.cancelled = false;
    }
    let mut same_restored_session =
        crate::session::Session::create_with_id(same_session_id.to_string(), None, None);
    same_restored_session.title = Some("same-id cold restore fixture".to_string());
    same_restored_session
        .save()
        .expect("save same-id cold restore session");
    same_agent
        .restore_session(&same_restored_session.id)
        .expect("same-id cold restore should succeed");

    let same_state_after_restore = same_agent.astra_first_state_handle();
    assert!(
        Arc::ptr_eq(&same_state, &same_state_after_restore),
        "same-id restore must preserve one shared Astra state authority"
    );
    {
        let state = same_state_after_restore.lock().await;
        assert_eq!(state.session_id, same_session_id);
        assert_eq!(state.generation, 11);
        assert_eq!(state.analyst_id.as_deref(), Some("same-id-analyst"));
        assert_eq!(state.analyst_model.as_deref(), Some("same-id-model"));
        assert_eq!(state.analyst_route.as_deref(), Some("same-id-route"));
        assert_eq!(state.analyst_effort.as_deref(), Some("high"));
        assert!(!state.busy);
        assert!(!state.cancelled);
    }
    {
        let mut state = same_state.lock().await;
        state.generation = 12;
        state.busy = true;
        state.cancelled = true;
    }
    let observed_state_handle = same_agent.astra_first_state_handle();
    let observed_shared_state = observed_state_handle.lock().await;
    assert_eq!(observed_shared_state.generation, 12);
    assert!(observed_shared_state.busy);
    assert!(observed_shared_state.cancelled);
    drop(observed_shared_state);

    if let Some(previous_home) = previous_home {
        crate::env::set_var("JCODE_HOME", previous_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[tokio::test]
async fn session_control_handle_does_not_wait_for_busy_agent_lock() {
    let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
        forked: Arc::new(AtomicBool::new(false)),
    });
    let registry = Registry::new(Arc::clone(&provider)).await;
    let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));

    let queue = Arc::new(std::sync::Mutex::new(Vec::new()));
    let background_signal = InterruptSignal::new();
    let stop_signal = InterruptSignal::new();
    let control = SessionControlHandle::new(
        "session_control_test",
        Arc::clone(&queue),
        background_signal.clone(),
        stop_signal.clone(),
    );

    let _busy_agent_lock = agent.lock().await;

    tokio::time::timeout(Duration::from_millis(100), async {
        assert!(control.queue_soft_interrupt(
            "please stop".to_string(),
            Vec::new(),
            true,
            SoftInterruptSource::User,
        ));
        control.request_cancel();
        assert!(control.request_background_current_tool());
        control.clear_soft_interrupts();
    })
    .await
    .expect("lock-free control operations should not wait for the agent mutex");

    assert!(stop_signal.is_set());
    assert!(background_signal.is_set());
    assert!(queue.lock().expect("queue lock").is_empty());
}

#[tokio::test]
async fn refreshed_session_control_handle_does_not_wait_for_busy_agent_lock() {
    let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
        forked: Arc::new(AtomicBool::new(false)),
    });
    let registry = Registry::new(Arc::clone(&provider)).await;
    let mut session = crate::session::Session::create_with_id(
        "session_busy_control_refresh".to_string(),
        None,
        None,
    );
    session.model = Some("panic-on-fork".to_string());
    let agent = Arc::new(Mutex::new(Agent::new_with_session(
        provider, registry, session, None,
    )));

    let stop_signal = InterruptSignal::new();
    let soft_interrupt_queue = Arc::new(std::sync::Mutex::new(Vec::new()));
    let shutdown_signals = Arc::new(RwLock::new(HashMap::from([(
        "session_busy_control_refresh".to_string(),
        stop_signal.clone(),
    )])));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::from([(
        "session_busy_control_refresh".to_string(),
        soft_interrupt_queue,
    )])));

    let _busy_agent_lock = agent.lock().await;

    tokio::time::timeout(Duration::from_millis(100), async {
        let control = refresh_session_control_handle(
            "session_busy_control_refresh",
            &agent,
            &shutdown_signals,
            &soft_interrupt_queues,
        )
        .await;
        control.request_cancel();
    })
    .await
    .expect("refreshing a session control handle must not wait for the busy agent mutex");

    assert!(stop_signal.is_set());
}

#[tokio::test]
async fn busy_session_background_tool_signal_fires_via_registry_fallback() {
    // Regression: pressing Alt+B/Ctrl+B while a turn owns the agent mutex (e.g.
    // running `await_members`) used to silently no-op because the lock-free
    // `cancel_only` control handle dropped the background-tool signal
    // (BACKGROUND_TOOL_SIGNAL_FIRE result=no_signal_handle). Building a full
    // SessionControlHandle now registers the signal in a process-global registry
    // so the cancel-only fallback can still fire it without the agent lock.
    let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
        forked: Arc::new(AtomicBool::new(false)),
    });
    let registry = Registry::new(Arc::clone(&provider)).await;
    let session_id = "session_busy_background_signal_registry";
    let mut session = crate::session::Session::create_with_id(session_id.to_string(), None, None);
    session.model = Some("panic-on-fork".to_string());
    let agent = Arc::new(Mutex::new(Agent::new_with_session(
        provider, registry, session, None,
    )));

    let background_signal = {
        let agent_guard = agent.lock().await;
        agent_guard.background_tool_signal()
    };

    // Build a full control handle once (registers the background signal), then
    // simulate the busy-turn reconnect path which yields a cancel-only handle.
    let stop_signal = InterruptSignal::new();
    let soft_interrupt_queue = Arc::new(std::sync::Mutex::new(Vec::new()));
    let _full = SessionControlHandle::new(
        session_id,
        Arc::clone(&soft_interrupt_queue),
        background_signal.clone(),
        stop_signal.clone(),
    );

    let cancel_only =
        SessionControlHandle::cancel_only(session_id, soft_interrupt_queue, stop_signal);

    // The cancel-only handle has no directly-held background signal, yet it must
    // still fire the registered one.
    assert!(cancel_only.request_background_current_tool());
    assert!(background_signal.is_set());

    // Cleanup so the global registry does not leak across tests.
    crate::server::state::remove_background_tool_signal(session_id);
}

#[tokio::test]
async fn busy_agent_request_rejection_does_not_wait_for_agent_lock() {
    let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
        forked: Arc::new(AtomicBool::new(false)),
    });
    let registry = Registry::new(Arc::clone(&provider)).await;
    let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();

    let busy_agent_lock = agent.lock().await;
    let rejected = tokio::time::timeout(Duration::from_millis(100), async {
        reject_if_agent_busy_for_request(
            17,
            "rename_session",
            "session_busy_reject",
            true,
            &agent,
            &client_event_tx,
        )
    })
    .await
    .expect("busy-agent request rejection must not wait for the agent mutex");
    assert!(rejected);
    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::Error {
            id: 17,
            retry_after_secs: Some(1),
            ..
        })
    ));

    drop(busy_agent_lock);
    assert!(!reject_if_agent_busy_for_request(
        18,
        "rename_session",
        "session_busy_reject",
        false,
        &agent,
        &client_event_tx,
    ));
    assert!(client_event_rx.try_recv().is_err());
}

#[tokio::test]
async fn context_message_persists_without_starting_turn() {
    let _guard = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    let session_id = "session_context_only_no_reply";
    let forked = Arc::new(AtomicBool::new(false));
    let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
        forked: Arc::clone(&forked),
    });
    let registry = Registry::new(Arc::clone(&provider)).await;
    let mut session = crate::session::Session::create_with_id(session_id.to_string(), None, None);
    session.model = Some("panic-on-fork".to_string());
    let agent = Arc::new(Mutex::new(Agent::new_with_session(
        provider, registry, session, None,
    )));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let before = agent.lock().await.message_count();

    append_context_message(
        77,
        "remember this context",
        vec![("image/png".to_string(), "AAA".to_string())],
        session_id,
        false,
        &agent,
        &client_event_tx,
    )
    .await;

    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::ContextMessageAdded { id: 77 })
    ));
    assert!(client_event_rx.try_recv().is_err());
    assert!(!forked.load(Ordering::SeqCst));

    let persisted = crate::session::Session::load(session_id).expect("persisted session");
    assert_eq!(persisted.messages.len(), before + 1);
    let message = persisted.messages.last().unwrap();
    assert_eq!(format!("{:?}", message.role), "User");
    assert!(matches!(
        &message.content[0],
        ContentBlock::Image { media_type, data }
            if media_type == "image/png" && data == "AAA"
    ));
    assert!(matches!(
        &message.content[1],
        ContentBlock::Text { text, .. } if text == "remember this context"
    ));
}

#[tokio::test]
async fn context_message_rejects_while_busy_without_waiting_for_agent_lock() {
    let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
        forked: Arc::new(AtomicBool::new(false)),
    });
    let registry = Registry::new(Arc::clone(&provider)).await;
    let agent = Arc::new(Mutex::new(Agent::new(provider, registry)));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let _busy_agent_lock = agent.lock().await;

    tokio::time::timeout(Duration::from_millis(100), async {
        append_context_message(
            78,
            "too busy",
            Vec::new(),
            "session_context_busy",
            true,
            &agent,
            &client_event_tx,
        )
        .await;
    })
    .await
    .expect("busy rejection must not wait for the agent mutex");

    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::Error {
            id: 78,
            retry_after_secs: Some(1),
            ..
        })
    ));
}

#[tokio::test]
async fn cancel_without_local_task_still_signals_session_control() {
    let soft_interrupt_queue = Arc::new(std::sync::Mutex::new(Vec::new()));
    let stop_signal = InterruptSignal::new();
    let control = SessionControlHandle::cancel_only(
        "session_detached_cancel",
        soft_interrupt_queue,
        stop_signal.clone(),
    );
    // The point of this path is a turn this connection does not own (attach
    // after reload, server-initiated turn). Without a registered active turn
    // the cancel is a deliberate no-op, because arming the signal with nothing
    // running only kills the *next* message.
    let _active_turn = crate::turn_cancel_registry::register_active_turn(
        "session_detached_cancel",
        InterruptSignal::new(),
    );
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _) = broadcast::channel(8);
    let mut client_is_processing = true;
    let mut message_id = Some(99);
    let mut session_id = Some("session_detached_cancel".to_string());
    let mut task = None;

    cancel_processing_message(
        &mut ProcessingState {
            client_is_processing: &mut client_is_processing,
            message_id: &mut message_id,
            session_id: &mut session_id,
            task: &mut task,
        },
        &control,
        &client_event_tx,
        &SwarmStatusRefs {
            members: &swarm_members,
            swarms_by_id: &swarms_by_id,
            event_history: &event_history,
            event_counter: &event_counter,
            event_tx: &swarm_event_tx,
        },
        Some(99),
        None,
    )
    .await;

    assert!(stop_signal.is_set());
    assert!(!client_is_processing);
    assert!(message_id.is_none());
    assert!(session_id.is_none());
    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::Interrupted)
    ));
    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::Done { id: 99 })
    ));
}

/// Regression for issue #428: the detached-turn cancel path schedules a
/// deferred reset of the shared stop signal. That reset must be epoch-guarded:
/// if a newer cancel fires during the reset window (rapid repeated Esc), the
/// stale timer must not clear it, otherwise the running turn never observes
/// the interrupt and keeps generating.
#[tokio::test]
async fn deferred_cancel_reset_does_not_erase_newer_cancel() {
    let soft_interrupt_queue = Arc::new(std::sync::Mutex::new(Vec::new()));
    let stop_signal = InterruptSignal::new();
    let control = SessionControlHandle::cancel_only(
        "session_detached_cancel_race",
        Arc::clone(&soft_interrupt_queue),
        stop_signal.clone(),
    );
    // A turn owned by another connection is what makes this the signalling
    // path rather than the idle no-op; see the sibling test.
    let _active_turn = crate::turn_cancel_registry::register_active_turn(
        "session_detached_cancel_race",
        InterruptSignal::new(),
    );
    let (client_event_tx, _client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _) = broadcast::channel(8);

    let cancel_via_no_task_path = async |request_id: u64| {
        let mut client_is_processing = true;
        let mut message_id = Some(request_id);
        let mut session_id = Some("session_detached_cancel_race".to_string());
        let mut task = None;
        cancel_processing_message(
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut message_id,
                session_id: &mut session_id,
                task: &mut task,
            },
            &control,
            &client_event_tx,
            &SwarmStatusRefs {
                members: &swarm_members,
                swarms_by_id: &swarms_by_id,
                event_history: &event_history,
                event_counter: &event_counter,
                event_tx: &swarm_event_tx,
            },
            Some(request_id),
            None,
        )
        .await;
    };

    // First Esc: fires the signal and schedules a 500ms deferred reset.
    cancel_via_no_task_path(1).await;
    assert!(stop_signal.is_set());

    // 400ms later the user presses Esc again (turn still hasn't stopped).
    tokio::time::sleep(Duration::from_millis(400)).await;
    cancel_via_no_task_path(2).await;
    assert!(stop_signal.is_set());

    // The first press's timer expires now. It must NOT clear the second
    // press's still-unobserved cancel.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        stop_signal.is_set(),
        "stale deferred reset erased a newer cancel (issue #428)"
    );

    // The second press's own timer may still clear it afterwards.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        !stop_signal.is_set(),
        "the newest cancel's deferred reset should eventually clear the flag"
    );
}

impl IsolatedRuntimeDir {
    fn new() -> Self {
        let temp = tempfile::TempDir::new().expect("runtime dir");
        let prev_runtime = std::env::var_os("JCODE_RUNTIME_DIR");
        crate::env::set_var("JCODE_RUNTIME_DIR", temp.path());
        crate::server::clear_reload_marker();
        Self {
            _prev_runtime: prev_runtime,
            _temp: temp,
        }
    }
}

impl IsolatedReloadRecoveryEnv {
    fn new() -> Self {
        let home = tempfile::TempDir::new().expect("jcode home");
        let runtime = tempfile::TempDir::new().expect("runtime dir");
        let prev_home = std::env::var_os("JCODE_HOME");
        let prev_runtime = std::env::var_os("JCODE_RUNTIME_DIR");
        crate::env::set_var("JCODE_HOME", home.path());
        crate::env::set_var("JCODE_RUNTIME_DIR", runtime.path());
        crate::server::clear_reload_marker();
        Self {
            prev_home,
            prev_runtime,
            _home: home,
            _runtime: runtime,
        }
    }
}

impl Drop for IsolatedReloadRecoveryEnv {
    fn drop(&mut self) {
        crate::server::clear_reload_marker();
        if let Some(prev_home) = self.prev_home.take() {
            crate::env::set_var("JCODE_HOME", prev_home);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
        if let Some(prev_runtime) = self.prev_runtime.take() {
            crate::env::set_var("JCODE_RUNTIME_DIR", prev_runtime);
        } else {
            crate::env::remove_var("JCODE_RUNTIME_DIR");
        }
    }
}

impl Drop for IsolatedRuntimeDir {
    fn drop(&mut self) {
        crate::server::clear_reload_marker();
        if let Some(prev_runtime) = self._prev_runtime.take() {
            crate::env::set_var("JCODE_RUNTIME_DIR", prev_runtime);
        } else {
            crate::env::remove_var("JCODE_RUNTIME_DIR");
        }
    }
}

/// Regression for issue #428: a turn actively streaming in this session but
/// NOT owned by the cancelling connection (no local task handle: post-reload
/// reattach, server-initiated wake turns, headless recovery) must abort
/// promptly even when the control handle's stop signal is a *different
/// instance* from the streaming agent's own `graceful_shutdown` signal.
///
/// Before the fix, `cancel_processing_message` hit the NO_LOCAL_TASK branch,
/// fired the stale handle-local signal (which nothing was listening to),
/// emitted `Interrupted` immediately, and the provider stream kept generating
/// for minutes ("Interrupting..." disappears, model keeps going, eventually
/// "Interrupted [x66]").
#[test]
fn cancel_aborts_detached_streaming_turn_with_stale_stop_signal() -> anyhow::Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    let session_id = "session_detached_streaming_cancel_428";

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let provider: Arc<dyn Provider> = Arc::new(NeverEndingStreamProvider);
        let registry = Registry::new(Arc::clone(&provider)).await;
        let mut session =
            crate::session::Session::create_with_id(session_id.to_string(), None, None);
        session.model = Some("never-ending-stream".to_string());
        let agent = Arc::new(Mutex::new(Agent::new_with_session(
            provider, registry, session, None,
        )));

        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<ServerEvent>();

        // Start the turn the way server-initiated paths do: no entry in any
        // connection's processing-task map.
        let turn_agent = Arc::clone(&agent);
        let turn = tokio::spawn(async move {
            process_message_streaming_mpsc(turn_agent, "stream forever", Vec::new(), None, event_tx)
                .await
        });

        // Wait until the provider stream is actively producing output.
        loop {
            match tokio::time::timeout(Duration::from_secs(5), event_rx.recv()).await {
                Ok(Some(ServerEvent::TextDelta { .. })) => break,
                Ok(Some(_)) => continue,
                Ok(None) => panic!("event channel closed before streaming started"),
                Err(_) => panic!("turn never started streaming"),
            }
        }

        // Esc arrives on a connection that does not own the task. Its control
        // handle holds a stop signal instance that is NOT the streaming
        // agent's graceful_shutdown signal (stale/lost registration).
        let stale_stop_signal = InterruptSignal::new();
        let control = SessionControlHandle::cancel_only(
            session_id,
            Arc::new(std::sync::Mutex::new(Vec::new())),
            stale_stop_signal.clone(),
        );
        let (client_event_tx, _client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
        let swarm_members = Arc::new(RwLock::new(HashMap::new()));
        let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
        let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
        let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (swarm_event_tx, _) = broadcast::channel(8);
        let mut client_is_processing = false;
        let mut message_id = None;
        let mut cancel_session_id = None;
        let mut task = None;

        cancel_processing_message(
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut message_id,
                session_id: &mut cancel_session_id,
                task: &mut task,
            },
            &control,
            &client_event_tx,
            &SwarmStatusRefs {
                members: &swarm_members,
                swarms_by_id: &swarms_by_id,
                event_history: &event_history,
                event_counter: &event_counter,
                event_tx: &swarm_event_tx,
            },
            Some(1),
            None,
        )
        .await;

        // The streaming turn must observe the cancel and stop promptly, not
        // minutes later when the provider happens to finish (issue #428).
        let result = tokio::time::timeout(Duration::from_secs(2), turn)
            .await
            .expect("streaming turn must abort promptly after cancel (issue #428)")
            .expect("turn task join");
        result.expect("cancelled turn should checkpoint cleanly");

        // The turn is over, so its cancel registration must be gone and the
        // agent's own signal must be reset so the *next* turn is not aborted
        // by the consumed cancel.
        assert!(
            crate::turn_cancel_registry::active_turn_signals(session_id).is_empty(),
            "finished turn must unregister its cancel signal"
        );
        let agent_signal = {
            let agent_guard = agent.lock().await;
            agent_guard.graceful_shutdown_signal()
        };
        assert!(
            !agent_signal.is_set(),
            "consumed cancel must not leak into the next turn"
        );
    });
    Ok(())
}

/// A cancel that arrives while the session is idle must not arm the cancel
/// signal at all.
///
/// The no-local-task branch cannot tell an idle session from one whose turn
/// another connection owns, so it used to fire the signal and clear it on a
/// 500ms timer. Any message sent inside that window began with the flag
/// already set and was aborted the instant it started: no reply, no error,
/// just a message that vanished. Pressing Esc on an idle prompt and typing
/// immediately is an ordinary thing to do, so this must be a true no-op.
#[test]
fn idle_cancel_does_not_arm_the_signal_for_the_next_turn() -> anyhow::Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    let session_id = "session_idle_cancel_noop";

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let stop_signal = InterruptSignal::new();
        let control = SessionControlHandle::cancel_only(
            session_id,
            Arc::new(std::sync::Mutex::new(Vec::new())),
            stop_signal.clone(),
        );
        let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
        let swarm_members = Arc::new(RwLock::new(HashMap::new()));
        let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
        let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
        let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (swarm_event_tx, _) = broadcast::channel(8);
        let mut client_is_processing = false;
        let mut message_id = None;
        let mut cancel_session_id = None;
        let mut task = None;

        assert!(
            !crate::turn_cancel_registry::has_active_turn(session_id),
            "test precondition: the session must be idle"
        );

        cancel_processing_message(
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut message_id,
                session_id: &mut cancel_session_id,
                task: &mut task,
            },
            &control,
            &client_event_tx,
            &SwarmStatusRefs {
                members: &swarm_members,
                swarms_by_id: &swarms_by_id,
                event_history: &event_history,
                event_counter: &event_counter,
                event_tx: &swarm_event_tx,
            },
            Some(1),
            None,
        )
        .await;

        assert!(
            !stop_signal.is_set(),
            "an idle cancel must not arm the stop signal; the next turn would die instantly"
        );
        // The client still learns the cancel was handled, so a UI showing
        // "Interrupting..." resolves rather than hanging.
        match client_event_rx.try_recv() {
            Ok(ServerEvent::Interrupted) => {}
            other => panic!("idle cancel must still report Interrupted, got {other:?}"),
        }
    });
    Ok(())
}

struct PanicOnForkProvider {
    forked: Arc<AtomicBool>,
}

#[derive(Clone)]
struct AstraIngressProbeProvider {
    complete_calls: Arc<std::sync::atomic::AtomicUsize>,
    fork_calls: Arc<std::sync::atomic::AtomicUsize>,
    calls: Arc<std::sync::Mutex<Vec<String>>>,
    model: Arc<std::sync::Mutex<String>>,
    effort: Arc<std::sync::Mutex<Option<String>>>,
    responses: Arc<std::sync::Mutex<VecDeque<std::result::Result<String, String>>>>,
}

#[async_trait]
impl Provider for AstraIngressProbeProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        self.complete_calls.fetch_add(1, Ordering::SeqCst);
        let prompt = _messages
            .iter()
            .flat_map(|message| message.content.iter())
            .filter_map(|content| match content {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.calls
            .lock()
            .expect("probe call lock")
            .push(format!("model={} prompt={prompt}", self.model()));

        let response = self
            .responses
            .lock()
            .expect("probe response lock")
            .pop_front()
            .unwrap_or_else(|| Ok("probe response".to_string()));
        let response = match response {
            Ok(response) => response,
            Err(message) => return Err(anyhow::anyhow!(message)),
        };
        Ok(Box::pin(stream::iter([
            Ok(StreamEvent::TextDelta(response)),
            Ok(StreamEvent::MessageEnd { stop_reason: None }),
        ])))
    }

    fn name(&self) -> &str {
        "astra-ingress-probe"
    }

    fn model(&self) -> String {
        self.model.lock().expect("probe model lock").clone()
    }

    fn set_model(&self, model: &str) -> Result<()> {
        *self.model.lock().expect("probe model lock") = model
            .strip_prefix("openai-api:")
            .or_else(|| model.strip_prefix("openai-oauth:"))
            .unwrap_or(model)
            .to_string();
        Ok(())
    }

    fn reasoning_effort(&self) -> Option<String> {
        self.effort.lock().expect("probe effort lock").clone()
    }

    fn set_reasoning_effort(&self, effort: &str) -> Result<()> {
        *self.effort.lock().expect("probe effort lock") = Some(effort.to_string());
        Ok(())
    }

    fn fork(&self) -> Arc<dyn Provider> {
        self.fork_calls.fetch_add(1, Ordering::SeqCst);
        Arc::new(Self {
            complete_calls: Arc::clone(&self.complete_calls),
            fork_calls: Arc::clone(&self.fork_calls),
            calls: Arc::clone(&self.calls),
            model: Arc::new(std::sync::Mutex::new(self.model())),
            effort: Arc::new(std::sync::Mutex::new(self.reasoning_effort())),
            responses: Arc::clone(&self.responses),
        })
    }
}

/// Streams text deltas forever (one every 20ms) until dropped. Stands in for
/// a live provider stream that only stops when the turn observes a cancel.
struct NeverEndingStreamProvider;

#[async_trait]
impl Provider for NeverEndingStreamProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(stream::unfold(0u64, |n| async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Some((Ok(StreamEvent::TextDelta(format!("token{} ", n))), n + 1))
        })))
    }

    fn name(&self) -> &str {
        "never-ending-stream"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

#[derive(Clone, Default)]
struct CompleteImmediatelyProvider;

#[async_trait]
impl Provider for CompleteImmediatelyProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(stream::iter(vec![Ok(StreamEvent::MessageEnd {
            stop_reason: None,
        })])))
    }

    fn name(&self) -> &str {
        "complete-immediately"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

#[tokio::test]
async fn direct_handle_set_service_tier_defers_until_busy_root_released() {
    let _storage = crate::storage::lock_test_env();
    let _env = FastMatrixTestEnv::new();
    let root_provider = Arc::new(RecordingServiceTierProvider::new_with_service_tier(
        "gpt-5.6-luna",
        Some("off"),
    ));
    let root_provider_dyn: Arc<dyn Provider> = root_provider.clone();
    let root_id = "matrix-d-r2-root";
    let root_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&root_provider_dyn),
        Registry::empty(),
        crate::session::Session::create_with_id(root_id.to_string(), None, None),
        None,
    )));
    let root_state = crate::server::RuntimeFastState::from_provider(root_id, root_provider.as_ref());
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    let busy_root = root_agent.lock().await;
    tokio::time::timeout(
        Duration::from_millis(100),
        crate::server::provider_control::handle_set_service_tier(
            1_001,
            "priority".to_string(),
            &root_agent,
            Some(Arc::clone(&root_state)),
            &client_event_tx,
        ),
    )
    .await
    .expect("the direct setter must return after queuing deferred work");

    assert!(
        client_event_rx.try_recv().is_err(),
        "deferred setter must not publish an ACK while the root mutex is held"
    );
    assert_eq!(root_provider.service_tier().as_deref(), Some("off"));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);

    drop(busy_root);

    let event = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match client_event_rx.recv().await {
                Some(ServerEvent::ServiceTierChanged { id, service_tier, error }) if id == 1_001 => {
                    break (service_tier, error);
                }
                Some(_) => continue,
                None => panic!("service-tier event channel closed before deferred ACK"),
            }
        }
    })
    .await
    .expect("deferred service-tier ACK timeout");
    assert_eq!(event.0.as_deref(), Some("priority"));
    assert!(event.1.is_none(), "deferred service-tier ACK failed: {:?}", event.1);
    assert_eq!(root_provider.service_tier().as_deref(), Some("priority"));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Priority);
}

#[tokio::test]
async fn socket_deferred_main_service_tier_success_and_failure_preserve_snapshot() {
    let _storage = crate::storage::lock_test_env();
    let _env = FastMatrixTestEnv::new();
    let root_provider = Arc::new(RecordingServiceTierProvider::new_with_service_tier(
        "gpt-5.6-luna",
        Some("off"),
    ));
    let provider_template: Arc<dyn Provider> = root_provider.clone();
    let (client_stream, server_task, sessions) =
        start_astra_ingress_probe_client(provider_template).await;
    let (client_reader, mut client_writer) = client_stream.into_split();
    let mut client_reader = BufReader::new(client_reader);
    let working_dir = std::env::current_dir()
        .expect("test working directory")
        .to_string_lossy()
        .into_owned();

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Subscribe {
            id: 1,
            working_dir: Some(working_dir.clone()),
            selfdev: None,
            target_session_id: None,
            client_instance_id: None,
            client_has_local_history: false,
            allow_session_takeover: false,
            crash_on_disconnect: false,
            continue_on_disconnect: false,
            terminal_env: Vec::new(),
        },
        1,
    )
    .await;
    let root_id = sessions
        .read()
        .await
        .keys()
        .next()
        .cloned()
        .expect("root session id");
    let root_state = sessions
        .read()
        .await
        .get(&root_id)
        .expect("root session entry")
        .fast_state();
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);

    let root_agent = sessions
        .read()
        .await
        .get(&root_id)
        .expect("root agent entry")
        .agent();
    let busy_root = root_agent.lock().await;
    client_writer
        .write_all(
            (serde_json::to_string(&Request::SetServiceTier {
                id: 2,
                service_tier: "priority".to_string(),
            })
            .expect("serialize deferred priority request")
                + "\n")
                .as_bytes(),
        )
        .await
        .expect("write deferred priority request");
    let deferred_success_ack = tokio::time::timeout(Duration::from_millis(100), async {
        let mut line = String::new();
        loop {
            line.clear();
            let read = client_reader.read_line(&mut line).await.expect("read deferred success barrier");
            assert!(read > 0, "server closed before deferred success application");
            if matches!(decode_request_or_event(&line), ServerEvent::ServiceTierChanged { id: 2, .. }) {
                return true;
            }
        }
    })
    .await;
    assert!(
        deferred_success_ack.is_err(),
        "deferred success must not ACK before provider application"
    );
    let mut pending_line = String::new();
    assert_eq!(root_provider.service_tier().as_deref(), Some("off"));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);
    drop(busy_root);

    let success = tokio::time::timeout(Duration::from_secs(2), async {
        let mut line = String::new();
        loop {
            line.clear();
            let read = client_reader
                .read_line(&mut line)
                .await
                .expect("read deferred success ACK");
            assert!(read > 0, "server closed before deferred success ACK");
            if let ServerEvent::ServiceTierChanged {
                id,
                service_tier,
                error,
            } = decode_request_or_event(&line)
                && id == 2
            {
                break (service_tier, error);
            }
        }
    })
    .await
    .expect("deferred success ACK timeout");
    assert_eq!(success.0.as_deref(), Some("priority"));
    assert!(success.1.is_none(), "deferred success ACK failed: {:?}", success.1);
    assert_eq!(root_provider.service_tier().as_deref(), Some("priority"));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Priority);

    let astra_provider = Arc::new(RecordingServiceTierProvider::new("gpt-6-astra"));
    let astra_provider_dyn: Arc<dyn Provider> = astra_provider.clone();
    let mut astra_session = crate::session::Session::create_with_origin(
        Some(root_id.clone()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    astra_session.model = Some("gpt-6-astra".to_string());
    astra_session.working_dir = Some(working_dir.clone());
    let astra_id = astra_session.id.clone();
    let astra_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&astra_provider_dyn),
        Registry::empty(),
        astra_session,
        None,
    )));
    sessions.write().await.insert(
        astra_id.clone(),
        crate::server::SessionAgentEntry::new(Arc::clone(&astra_agent), Arc::clone(&root_state)),
    );
    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        fast_matrix_subscribe(3, &astra_id, &working_dir),
        3,
    )
    .await;
    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Message {
            id: 4,
            content: "matrix-d success native Astra".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
        4,
    )
    .await;
    assert_eq!(
        astra_provider.scoped_override(),
        Some(Some("priority".to_string()))
    );

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        fast_matrix_subscribe(5, &root_id, &working_dir),
        5,
    )
    .await;
    root_provider.fail_next_service_tier("matrix-d injected failure");
    let root_agent = sessions
        .read()
        .await
        .get(&root_id)
        .expect("root agent entry")
        .agent();
    let busy_root = root_agent.lock().await;
    client_writer
        .write_all(
            (serde_json::to_string(&Request::SetServiceTier {
                id: 6,
                service_tier: "off".to_string(),
            })
            .expect("serialize deferred failure request")
                + "\n")
                .as_bytes(),
        )
        .await
        .expect("write deferred failure request");
    pending_line.clear();
    let deferred_failure_ack = tokio::time::timeout(Duration::from_millis(100), async {
        loop {
            pending_line.clear();
            let read = client_reader.read_line(&mut pending_line).await.expect("read deferred failure barrier");
            assert!(read > 0, "server closed before deferred failure application");
            if matches!(decode_request_or_event(&pending_line), ServerEvent::ServiceTierChanged { id: 6, .. }) {
                return true;
            }
        }
    })
    .await;
    assert!(
        deferred_failure_ack.is_err(),
        "deferred failure must not ACK before provider application"
    );
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Priority);
    drop(busy_root);

    let failure = tokio::time::timeout(Duration::from_secs(2), async {
        let mut line = String::new();
        loop {
            line.clear();
            let read = client_reader
                .read_line(&mut line)
                .await
                .expect("read failed service-tier ACK");
            assert!(read > 0, "server closed before failed service-tier ACK");
            if let ServerEvent::ServiceTierChanged { id, error, .. } = decode_request_or_event(&line)
                && id == 6
            {
                break error;
            }
        }
    })
    .await
    .expect("failed service-tier ACK timeout");
    assert_eq!(failure.as_deref(), Some("matrix-d injected failure"));
    assert_eq!(root_provider.service_tier().as_deref(), Some("priority"));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Priority);

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        fast_matrix_subscribe(7, &astra_id, &working_dir),
        7,
    )
    .await;
    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Message {
            id: 8,
            content: "matrix-d failed setter retains native Astra".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
        8,
    )
    .await;
    assert_eq!(
        astra_provider.scoped_override(),
        Some(Some("priority".to_string()))
    );
    astra_provider.assert_dispatch_sequence(&[
        (
            "matrix-d success native Astra",
            Some(Some("priority".to_string())),
        ),
        (
            "matrix-d failed setter retains native Astra",
            Some(Some("priority".to_string())),
        ),
    ]);

    drop(client_writer);
    server_task
        .await
        .expect("Matrix D server task join")
        .expect("Matrix D server task result");
}

#[tokio::test]
async fn socket_same_resident_native_unrelated_web_native_refreshes_policy() {
    let _storage = crate::storage::lock_test_env();
    let _env = FastMatrixTestEnv::new();
    let root_provider = Arc::new(RecordingServiceTierProvider::new_with_service_tier(
        "gpt-5.6-luna",
        Some("priority"),
    ));
    let provider_template: Arc<dyn Provider> = root_provider.clone();
    let (client_stream, server_task, sessions) =
        start_astra_ingress_probe_client(provider_template).await;
    let (client_reader, mut client_writer) = client_stream.into_split();
    let mut client_reader = BufReader::new(client_reader);
    let working_dir = std::env::current_dir()
        .expect("test working directory")
        .to_string_lossy()
        .into_owned();

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Subscribe {
            id: 1,
            working_dir: Some(working_dir.clone()),
            selfdev: None,
            target_session_id: None,
            client_instance_id: None,
            client_has_local_history: false,
            allow_session_takeover: false,
            crash_on_disconnect: false,
            continue_on_disconnect: false,
            terminal_env: Vec::new(),
        },
        1,
    )
    .await;
    let root_id = sessions
        .read()
        .await
        .keys()
        .next()
        .cloned()
        .expect("root session id");
    let root_state = sessions
        .read()
        .await
        .get(&root_id)
        .expect("root session entry")
        .fast_state();
    let initial_root_agent = sessions
        .read()
        .await
        .get(&root_id)
        .expect("initial root session entry")
        .agent();
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Priority);

    let astra_provider = Arc::new(RecordingServiceTierProvider::new("gpt-6-astra"));
    let astra_provider_dyn: Arc<dyn Provider> = astra_provider.clone();
    let mut astra_session = crate::session::Session::create_with_origin(
        Some(root_id.clone()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    astra_session.model = Some("gpt-6-astra".to_string());
    astra_session.working_dir = Some(working_dir.clone());
    let astra_id = astra_session.id.clone();
    let astra_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&astra_provider_dyn),
        Registry::empty(),
        astra_session,
        None,
    )));
    sessions.write().await.insert(
        astra_id.clone(),
        crate::server::SessionAgentEntry::new(Arc::clone(&astra_agent), Arc::clone(&root_state)),
    );
    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        fast_matrix_subscribe(2, &astra_id, &working_dir),
        2,
    )
    .await;
    assert_eq!(astra_provider.model(), "gpt-6-astra");
    assert_eq!(
        sessions
            .read()
            .await
            .get(&astra_id)
            .expect("resident Astra entry after Subscribe")
            .agent()
            .lock()
            .await
            .provider_handle()
            .model(),
        "gpt-6-astra"
    );
    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Message {
            id: 3,
            content: "C native priority".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
        3,
    )
    .await;
    assert_eq!(
        astra_provider.scoped_override(),
        Some(Some("priority".to_string()))
    );

    send_fast_matrix_model_request(
        &mut client_writer,
        &mut client_reader,
        Request::SetModel {
            id: 13,
            model: "gpt-6-astra[web]".to_string(),
        },
        13,
        "gpt-6-astra[web]",
    )
    .await;
    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Message {
            id: 14,
            content: "C Web direct clear".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
        14,
    )
    .await;
    assert_eq!(
        astra_provider.scoped_override(),
        None,
        "direct native-to-Web admission must clear the native priority override"
    );

    send_fast_matrix_model_request(
        &mut client_writer,
        &mut client_reader,
        Request::SetModel {
            id: 4,
            model: "offline-analyst".to_string(),
        },
        4,
        "offline-analyst",
    )
    .await;
    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Message {
            id: 5,
            content: "C unrelated clear".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
        5,
    )
    .await;
    assert_eq!(astra_provider.scoped_override(), None);

    send_fast_matrix_model_request(
        &mut client_writer,
        &mut client_reader,
        Request::SetModel {
            id: 6,
            model: "gpt-6-astra[web]".to_string(),
        },
        6,
        "gpt-6-astra[web]",
    )
    .await;
    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Message {
            id: 7,
            content: "C Web clear".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
        7,
    )
    .await;
    assert_eq!(astra_provider.scoped_override(), None);

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        fast_matrix_subscribe(8, &root_id, &working_dir),
        8,
    )
    .await;
    send_fast_matrix_service_tier_request(
        &mut client_writer,
        &mut client_reader,
        Request::SetServiceTier {
            id: 9,
            service_tier: "off".to_string(),
        },
        9,
        "off",
    )
    .await;
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        fast_matrix_subscribe(10, &astra_id, &working_dir),
        10,
    )
    .await;
    send_fast_matrix_model_request(
        &mut client_writer,
        &mut client_reader,
        Request::SetModel {
            id: 11,
            model: "gpt-6-astra".to_string(),
        },
        11,
        "gpt-6-astra",
    )
    .await;
    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Message {
            id: 12,
            content: "C native ordinary after root off".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
        12,
    )
    .await;
    assert_eq!(astra_provider.scoped_override(), Some(None));
    astra_provider.assert_dispatch_sequence(&[
        ("C native priority", Some(Some("priority".to_string()))),
        ("C Web direct clear", None),
        ("C unrelated clear", None),
        ("C Web clear", None),
        ("C native ordinary after root off", Some(None)),
    ]);

    let final_sessions = sessions.read().await;
    let final_root = final_sessions.get(&root_id).expect("final root entry");
    let final_astra = final_sessions.get(&astra_id).expect("final Astra entry");
    assert!(Arc::ptr_eq(&final_root.agent(), &initial_root_agent));
    assert!(Arc::ptr_eq(&final_root.fast_state(), &root_state));
    assert!(Arc::ptr_eq(&final_astra.agent(), &astra_agent));
    assert!(Arc::ptr_eq(&final_astra.fast_state(), &root_state));

    drop(client_writer);
    server_task
        .await
        .expect("Matrix C server task join")
        .expect("Matrix C server task result");
}

#[tokio::test]
async fn socket_wake_reuses_resident_native_astra_for_admitted_turn() {
    let _storage = crate::storage::lock_test_env();
    let _env = FastMatrixTestEnv::new();
    let runtime_dir = tempfile::tempdir().expect("isolated wake runtime");
    struct WakeEnvGuard {
        previous_mode: Option<std::ffi::OsString>,
        previous_runtime: Option<std::ffi::OsString>,
        _runtime: tempfile::TempDir,
    }
    impl Drop for WakeEnvGuard {
        fn drop(&mut self) {
            match self.previous_mode.take() {
                Some(value) => crate::env::set_var("JCODE_WAKE_MODE", value),
                None => crate::env::remove_var("JCODE_WAKE_MODE"),
            }
            match self.previous_runtime.take() {
                Some(value) => crate::env::set_var("JCODE_RUNTIME_DIR", value),
                None => crate::env::remove_var("JCODE_RUNTIME_DIR"),
            }
            crate::config::invalidate_config_cache();
        }
    }
    let _wake_env = WakeEnvGuard {
        previous_mode: std::env::var_os("JCODE_WAKE_MODE"),
        previous_runtime: std::env::var_os("JCODE_RUNTIME_DIR"),
        _runtime: runtime_dir,
    };
    crate::env::set_var("JCODE_WAKE_MODE", "internal");
    crate::env::set_var("JCODE_RUNTIME_DIR", _wake_env._runtime.path());
    crate::config::invalidate_config_cache();

    let root_provider = Arc::new(RecordingServiceTierProvider::new_with_service_tier(
        "gpt-5.6-luna",
        Some("priority"),
    ));
    let provider_template: Arc<dyn Provider> = root_provider.clone();
    let (client_stream, server_task, sessions) =
        start_astra_ingress_probe_client(provider_template).await;
    let (client_reader, mut client_writer) = client_stream.into_split();
    let mut client_reader = BufReader::new(client_reader);
    let working_dir = std::env::current_dir()
        .expect("test working directory")
        .to_string_lossy()
        .into_owned();

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Subscribe {
            id: 1,
            working_dir: Some(working_dir.clone()),
            selfdev: None,
            target_session_id: None,
            client_instance_id: None,
            client_has_local_history: false,
            allow_session_takeover: false,
            crash_on_disconnect: false,
            continue_on_disconnect: false,
            terminal_env: Vec::new(),
        },
        1,
    )
    .await;
    let root_id = sessions
        .read()
        .await
        .keys()
        .next()
        .cloned()
        .expect("root session id");
    let root_state = sessions
        .read()
        .await
        .get(&root_id)
        .expect("root session entry")
        .fast_state();
    let initial_root_agent = sessions
        .read()
        .await
        .get(&root_id)
        .expect("initial root session entry")
        .agent();

    let astra_provider = Arc::new(RecordingServiceTierProvider::new("gpt-6-astra"));
    astra_provider.enable_wake_fixture_stream();
    let astra_provider_dyn: Arc<dyn Provider> = astra_provider.clone();
    let mut astra_session = crate::session::Session::create_with_origin(
        Some(root_id.clone()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    astra_session.model = Some("gpt-6-astra".to_string());
    astra_session.working_dir = Some(working_dir.clone());
    let astra_id = astra_session.id.clone();
    let astra_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&astra_provider_dyn),
        Registry::empty(),
        astra_session,
        None,
    )));
    sessions.write().await.insert(
        astra_id.clone(),
        crate::server::SessionAgentEntry::new(Arc::clone(&astra_agent), Arc::clone(&root_state)),
    );
    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        fast_matrix_subscribe(2, &astra_id, &working_dir),
        2,
    )
    .await;
    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Message {
            id: 3,
            content: "C-WAKE-1 initial priority".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
        3,
    )
    .await;
    assert_eq!(
        astra_provider.scoped_override(),
        Some(Some("priority".to_string()))
    );
    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        fast_matrix_subscribe(4, &root_id, &working_dir),
        4,
    )
    .await;
    send_fast_matrix_service_tier_request(
        &mut client_writer,
        &mut client_reader,
        Request::SetServiceTier {
            id: 5,
            service_tier: "off".to_string(),
        },
        5,
        "off",
    )
    .await;
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);
    assert_eq!(root_provider.service_tier().as_deref(), Some("off"));

    let swarm_id = "c-wake-1-swarm".to_string();
    let (root_member_event_tx, _root_member_event_rx) = mpsc::unbounded_channel();
    let (member_event_tx, mut member_event_rx) = mpsc::unbounded_channel();
    let swarm_members = Arc::new(RwLock::new(HashMap::from([
        (
            root_id.clone(),
            crate::server::SwarmMember::from_record(
                jcode_swarm_core::SwarmMemberRecord {
                    session_id: root_id.clone(),
                    working_dir: Some(std::path::PathBuf::from(&working_dir)),
                    swarm_id: Some(swarm_id.clone()),
                    swarm_enabled: true,
                    status: jcode_swarm_core::SwarmLifecycleStatus::Ready,
                    detail: None,
                    task_label: Some("coordinator".to_string()),
                    friendly_name: Some("wake-root".to_string()),
                    report_back_to_session_id: None,
                    latest_completion_report: None,
                    role: jcode_swarm_core::SwarmRole::Coordinator,
                    is_headless: false,
                },
                root_member_event_tx,
            ),
        ),
        (
            astra_id.clone(),
            crate::server::SwarmMember::from_record(
                jcode_swarm_core::SwarmMemberRecord {
                    session_id: astra_id.clone(),
                    working_dir: Some(std::path::PathBuf::from(&working_dir)),
                    swarm_id: Some(swarm_id.clone()),
                    swarm_enabled: true,
                    status: jcode_swarm_core::SwarmLifecycleStatus::Ready,
                    detail: None,
                    task_label: Some("native Astra worker".to_string()),
                    friendly_name: Some("native-astra".to_string()),
                    report_back_to_session_id: Some(root_id.clone()),
                    latest_completion_report: None,
                    role: jcode_swarm_core::SwarmRole::Agent,
                    is_headless: false,
                },
                member_event_tx.clone(),
            ),
        ),
    ])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        swarm_id.clone(),
        HashSet::from([root_id.clone(), astra_id.clone()]),
    )])));
    let channel_subscriptions = Arc::new(RwLock::new(HashMap::new()));
    let event_history = Arc::new(RwLock::new(VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _swarm_event_rx) = broadcast::channel(8);
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let client_connections = Arc::new(RwLock::new(HashMap::new()));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    let initial_idle = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(guard) =
                crate::server::live_turn::idle_live_agent(&astra_id, &sessions, &swarm_members).await
            {
                let same_agent = sessions
                    .read()
                    .await
                    .get(&astra_id)
                    .map(|entry| Arc::ptr_eq(&entry.agent(), &astra_agent))
                    .unwrap_or(false);
                assert!(same_agent, "idle reservation was not for the resident Astra agent");
                return guard;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial resident Astra turn should become idle");
    drop(initial_idle);

    let mut wake_entry_rx = astra_provider.arm_wake_entry();
    super::handle_comm_message(
        6,
        root_id.clone(),
        "C-WAKE-1 real internal wake admission".to_string(),
        Some(astra_id.clone()),
        None,
        Some(crate::protocol::CommDeliveryMode::Wake),
        None,
        None,
        &client_event_tx,
        &sessions,
        &soft_interrupt_queues,
        &swarm_members,
        &swarms_by_id,
        &channel_subscriptions,
        &event_history,
        &event_counter,
        &swarm_event_tx,
        &client_connections,
    )
    .await;

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match client_event_rx.recv().await {
                Some(ServerEvent::Done { id }) if id == 6 => break,
                Some(_) => continue,
                None => panic!("Wake client event channel closed"),
            }
        }
    })
    .await
    .expect("Wake delivery ACK timeout");

    let wake_dispatch = match tokio::time::timeout(Duration::from_secs(2), &mut wake_entry_rx).await
    {
        Ok(result) => result.expect("Wake provider entry channel closed"),
        Err(_) => panic!(
            "Wake provider entry timeout; recorded dispatches: {:?}",
            astra_provider.dispatches()
        ),
    };
    assert!(wake_dispatch.prompt.contains("C-WAKE-1 real internal wake admission"));
    assert_eq!(wake_dispatch.effective_override, Some(None));

    let mut saw_wake_notification = false;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match member_event_rx.recv().await {
                Some(ServerEvent::Notification { message, .. }) => {
                    if message.contains("C-WAKE-1 real internal wake admission") {
                        saw_wake_notification = true;
                    }
                }
                Some(ServerEvent::Done { id: 0 }) => break,
                Some(_) => continue,
                None => panic!("Wake worker event channel closed"),
            }
        }
    })
    .await
    .expect("Wake worker completion timeout");
    assert!(saw_wake_notification, "Wake worker notification was not observed");

    let wake_idle = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(guard) =
                crate::server::live_turn::idle_live_agent(&astra_id, &sessions, &swarm_members).await
            {
                return guard;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Wake worker reservation should be released after Done");
    assert_eq!(
        swarm_members
            .read()
            .await
            .get(&astra_id)
            .map(|member| member.status.clone())
            .as_deref(),
        Some("ready")
    );
    drop(wake_idle);
    astra_provider.assert_dispatch_sequence(&[
        ("C-WAKE-1 initial priority", Some(Some("priority".to_string()))),
        ("C-WAKE-1 real internal wake admission", Some(None)),
    ]);
    let final_sessions = sessions.read().await;
    let final_root = final_sessions.get(&root_id).expect("final root entry");
    let final_astra = final_sessions.get(&astra_id).expect("final Astra entry");
    assert!(Arc::ptr_eq(&final_root.agent(), &initial_root_agent));
    assert!(Arc::ptr_eq(&final_root.fast_state(), &root_state));
    assert!(Arc::ptr_eq(&final_astra.agent(), &astra_agent));
    assert!(Arc::ptr_eq(&final_astra.fast_state(), &root_state));

    drop(client_writer);
    server_task
        .await
        .expect("C-WAKE-1 server task join")
        .expect("C-WAKE-1 server task result");
}

#[tokio::test]
async fn socket_assign_next_reuses_resident_native_astra_for_next_message() {
    let _storage = crate::storage::lock_test_env();
    let _env = FastMatrixTestEnv::new();
    let root_provider = Arc::new(RecordingServiceTierProvider::new_with_service_tier(
        "gpt-5.6-luna",
        Some("priority"),
    ));
    let provider_template: Arc<dyn Provider> = root_provider.clone();
    let (client_stream, server_task, sessions) =
        start_astra_ingress_probe_client(provider_template.clone()).await;
    let (client_reader, mut client_writer) = client_stream.into_split();
    let mut client_reader = BufReader::new(client_reader);
    let working_dir = std::env::current_dir()
        .expect("test working directory")
        .to_string_lossy()
        .into_owned();

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Subscribe {
            id: 1,
            working_dir: Some(working_dir.clone()),
            selfdev: None,
            target_session_id: None,
            client_instance_id: None,
            client_has_local_history: false,
            allow_session_takeover: false,
            crash_on_disconnect: false,
            continue_on_disconnect: false,
            terminal_env: Vec::new(),
        },
        1,
    )
    .await;
    let root_id = sessions
        .read()
        .await
        .keys()
        .next()
        .cloned()
        .expect("root session id");
    let root_state = sessions
        .read()
        .await
        .get(&root_id)
        .expect("root session entry")
        .fast_state();

    let astra_provider = Arc::new(RecordingServiceTierProvider::new("gpt-6-astra"));
    let astra_provider_dyn: Arc<dyn Provider> = astra_provider.clone();
    let mut astra_session = crate::session::Session::create_with_origin(
        Some(root_id.clone()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    astra_session.model = Some("gpt-6-astra".to_string());
    astra_session.working_dir = Some(working_dir.clone());
    let astra_id = astra_session.id.clone();
    let astra_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&astra_provider_dyn),
        Registry::empty(),
        astra_session,
        None,
    )));
    sessions.write().await.insert(
        astra_id.clone(),
        crate::server::SessionAgentEntry::new(Arc::clone(&astra_agent), Arc::clone(&root_state)),
    );

    send_fast_matrix_service_tier_request(
        &mut client_writer,
        &mut client_reader,
        Request::SetServiceTier {
            id: 2,
            service_tier: "off".to_string(),
        },
        2,
        "off",
    )
    .await;
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);
    assert_eq!(root_provider.service_tier().as_deref(), Some("off"));

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        fast_matrix_subscribe(3, &astra_id, &working_dir),
        3,
    )
    .await;

    let swarm_id = "c-assign-1-swarm".to_string();
    let (member_event_tx, _member_event_rx) = mpsc::unbounded_channel();
    let swarm_members = Arc::new(RwLock::new(HashMap::from([
        (
            root_id.clone(),
            crate::server::SwarmMember::from_record(
                jcode_swarm_core::SwarmMemberRecord {
                    session_id: root_id.clone(),
                    working_dir: Some(std::path::PathBuf::from(&working_dir)),
                    swarm_id: Some(swarm_id.clone()),
                    swarm_enabled: true,
                    status: jcode_swarm_core::SwarmLifecycleStatus::Ready,
                    detail: None,
                    task_label: Some("coordinator".to_string()),
                    friendly_name: None,
                    report_back_to_session_id: None,
                    latest_completion_report: None,
                    role: jcode_swarm_core::SwarmRole::Coordinator,
                    is_headless: false,
                },
                member_event_tx.clone(),
            ),
        ),
        (
            astra_id.clone(),
            crate::server::SwarmMember::from_record(
                jcode_swarm_core::SwarmMemberRecord {
                    session_id: astra_id.clone(),
                    working_dir: Some(std::path::PathBuf::from(&working_dir)),
                    swarm_id: Some(swarm_id.clone()),
                    swarm_enabled: true,
                    status: jcode_swarm_core::SwarmLifecycleStatus::Ready,
                    detail: None,
                    task_label: Some("native Astra worker".to_string()),
                    friendly_name: None,
                    report_back_to_session_id: Some(root_id.clone()),
                    latest_completion_report: None,
                    role: jcode_swarm_core::SwarmRole::Agent,
                    is_headless: false,
                },
                member_event_tx.clone(),
            ),
        ),
    ])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        swarm_id.clone(),
        HashSet::from([root_id.clone(), astra_id.clone()]),
    )])));
    let swarm_plans = Arc::new(RwLock::new(HashMap::from([(
        swarm_id.clone(),
        jcode_plan::VersionedPlan {
            items: vec![jcode_plan::PlanItem {
                content: "C-ASSIGN-1 plan assignment".to_string(),
                status: "queued".to_string(),
                priority: "high".to_string(),
                id: "c-assign-next-task".to_string(),
                subsystem: None,
                file_scope: Vec::new(),
                blocked_by: Vec::new(),
                assigned_to: None,
            }],
            version: 1,
            participants: HashSet::from([root_id.clone(), astra_id.clone()]),
            task_progress: HashMap::new(),
            mode: "light".to_string(),
            node_meta: HashMap::new(),
        },
    )])));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::from([(
        swarm_id.clone(),
        root_id.clone(),
    )])));
    let event_history = Arc::new(RwLock::new(VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _swarm_event_rx) = broadcast::channel(8);
    // The resident Astra is represented by the real socket harness above. Keep
    // the standalone mutation maps consistent with that connected client so
    // handle_comm_assign_next exercises assignment only. Without this
    // test-local connection record, handle_comm_assign_task correctly treats
    // the independently constructed map as headless and starts its fallback
    // assigned-task run before the documented next Message.
    let (disconnect_tx, _disconnect_rx) = mpsc::unbounded_channel();
    let client_connections = Arc::new(RwLock::new(HashMap::from([(
        "c-assign-astra-client".to_string(),
        crate::server::ClientConnectionInfo {
            client_id: "c-assign-astra-client".to_string(),
            session_id: astra_id.clone(),
            client_instance_id: None,
            debug_client_id: None,
            connected_at: std::time::Instant::now(),
            last_seen: std::time::Instant::now(),
            is_processing: false,
            current_tool_name: None,
            terminal_env: Vec::new(),
            disconnect_tx,
        },
    )])));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let global_session_id = Arc::new(RwLock::new(root_id.clone()));
    let mutation_runtime = SwarmMutationRuntime::default();
    let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());
    let (assign_event_tx, mut assign_event_rx) = mpsc::unbounded_channel();

    crate::server::comm_control::handle_comm_assign_next(
        4,
        root_id.clone(),
        None,
        Some(working_dir.clone()),
        Some(false),
        Some(false),
        Some("assignment metadata only; await the documented next Message".to_string()),
        None,
        None,
        &assign_event_tx,
        &sessions,
        &global_session_id,
        &provider_template,
        &soft_interrupt_queues,
        &client_connections,
        &swarm_members,
        &swarms_by_id,
        &swarm_plans,
        &swarm_coordinators,
        &event_history,
        &event_counter,
        &swarm_event_tx,
        &mcp_pool,
        &mutation_runtime,
    )
    .await;

    let assignment = tokio::time::timeout(Duration::from_secs(1), assign_event_rx.recv())
        .await
        .expect("assign_next response timeout")
        .expect("assign_next response channel closed");
    assert!(matches!(
        assignment,
        ServerEvent::CommAssignTaskResponse {
            id: 4,
            task_id,
            target_session,
        } if task_id == "c-assign-next-task" && target_session == astra_id
    ));
    let assigned_plan = swarm_plans
        .read()
        .await
        .get(&swarm_id)
        .expect("assigned plan")
        .items
        .first()
        .expect("assigned task")
        .clone();
    assert_eq!(assigned_plan.assigned_to.as_deref(), Some(astra_id.as_str()));
    assert_eq!(assigned_plan.status, "queued");
    assert!(astra_provider.dispatches().is_empty(), "assign_next must not start a turn");

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Message {
            id: 5,
            content: "C-ASSIGN-1 next admitted turn".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
        5,
    )
    .await;
    astra_provider.assert_dispatch_sequence(&[(
        "C-ASSIGN-1 next admitted turn",
        Some(None),
    )]);
    assert!(Arc::ptr_eq(
        &sessions
            .read()
            .await
            .get(&astra_id)
            .expect("resident assigned Astra entry")
            .agent(),
        &astra_agent
    ));
    assert!(Arc::ptr_eq(
        &sessions
            .read()
            .await
            .get(&astra_id)
            .expect("resident assigned Astra state entry")
            .fast_state(),
        &root_state
    ));

    drop(client_writer);
    server_task
        .await
        .expect("C-ASSIGN-1 server task join")
        .expect("C-ASSIGN-1 server task result");
}

#[derive(Clone, Copy, Debug)]
enum WakeFixCovCase {
    MissingResidentEntry,
    ReservedAgentArcMismatch,
    InvalidOwnerState,
    RetiredOwnerState,
    UnsupportedWithoutOwnership,
}

#[tokio::test]
async fn wake_fix_1_cov_02_live_turn_native_tier_while_root_busy() {
    let _storage = crate::storage::lock_test_env();
    let _env = FastMatrixTestEnv::new();

    run_wake_fix_cov02_branch(false).await;
    run_wake_fix_cov02_branch(true).await;
}

async fn run_wake_fix_cov02_branch(display_role: bool) {
    let root_provider = Arc::new(RecordingServiceTierProvider::new_with_service_tier(
        "gpt-5.6-luna",
        Some("priority"),
    ));
    let root_provider_dyn: Arc<dyn Provider> = root_provider.clone();
    let root_id = format!("wf1-cov02-root-{display_role}");
    let root_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&root_provider_dyn),
        Registry::empty(),
        crate::session::Session::create_with_id(root_id.clone(), None, None),
        None,
    )));
    let root_state = crate::server::RuntimeFastState::from_provider(
        root_id.clone(),
        root_provider.as_ref(),
    );
    let initial_root_agent = Arc::clone(&root_agent);

    let worker_provider = Arc::new(RecordingServiceTierProvider::new("gpt-6-astra"));
    worker_provider.enable_wake_fixture_stream();
    let worker_provider_dyn: Arc<dyn Provider> = worker_provider.clone();
    let mut worker_session = crate::session::Session::create_with_origin(
        Some(root_id.clone()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    worker_session.model = Some("gpt-6-astra".to_string());
    let worker_id = worker_session.id.clone();
    let worker_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&worker_provider_dyn),
        Registry::empty(),
        worker_session,
        None,
    )));
    let initial_worker_agent = Arc::clone(&worker_agent);
    let sessions: crate::server::SessionAgents = Arc::new(RwLock::new(HashMap::from([
        (
            root_id.clone(),
            crate::server::SessionAgentEntry::new(
                Arc::clone(&root_agent),
                Arc::clone(&root_state),
            ),
        ),
        (
            worker_id.clone(),
            crate::server::SessionAgentEntry::new(
                Arc::clone(&worker_agent),
                Arc::clone(&root_state),
            ),
        ),
    ])));

    let (member_event_tx, mut member_event_rx) = mpsc::unbounded_channel();
    let swarm_id = format!("wf1-cov02-swarm-{display_role}");
    let members = Arc::new(RwLock::new(HashMap::from([(
        worker_id.clone(),
        crate::server::SwarmMember::from_record(
            jcode_swarm_core::SwarmMemberRecord {
                session_id: worker_id.clone(),
                working_dir: None,
                swarm_id: Some(swarm_id.clone()),
                swarm_enabled: true,
                status: jcode_swarm_core::SwarmLifecycleStatus::Ready,
                detail: None,
                task_label: Some("WF1-COV-02".to_string()),
                friendly_name: Some("wf1-cov02-worker".to_string()),
                report_back_to_session_id: Some(root_id.clone()),
                latest_completion_report: None,
                role: jcode_swarm_core::SwarmRole::Agent,
                is_headless: false,
            },
            member_event_tx,
        ),
    )])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        swarm_id,
        HashSet::from([worker_id.clone()]),
    )])));
    let event_history = Arc::new(RwLock::new(VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (event_tx, _) = broadcast::channel(8);
    let context = || {
        crate::server::live_turn::LiveTurnSwarmContext::new(
            &members,
            &swarms_by_id,
            &event_history,
            &event_counter,
            &event_tx,
        )
    };

    let mut prior_entry = worker_provider.arm_wake_entry();
    let started = crate::server::live_turn::run_live_turn_if_idle(
        &worker_id,
        &format!("WF1-COV-02 prior priority {display_role}"),
        None,
        &sessions,
        context(),
    )
    .await;
    assert!(started, "prior worker turn must be admitted");
    let prior_dispatch = tokio::time::timeout(Duration::from_secs(2), &mut prior_entry)
        .await
        .expect("prior provider entry timeout")
        .expect("prior provider entry channel closed");
    assert_eq!(prior_dispatch.effective_override, Some(Some("priority".to_string())));
    await_cov02_worker_done(&mut member_event_rx).await;
    await_cov02_worker_idle(&worker_id, &sessions, &members).await;

    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();
    crate::server::provider_control::handle_set_service_tier(
        9_902,
        "off".to_string(),
        &root_agent,
        Some(Arc::clone(&root_state)),
        &client_event_tx,
    )
    .await;
    let setter_event = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match client_event_rx.recv().await {
                Some(ServerEvent::ServiceTierChanged {
                    id,
                    service_tier,
                    error,
                }) if id == 9_902 => break (service_tier, error),
                Some(_) => continue,
                None => panic!("WF1-COV-02 service-tier event channel closed"),
            }
        }
    })
    .await
    .expect("root off setter ACK timeout");
    assert_eq!(setter_event.0.as_deref(), Some("off"));
    assert!(setter_event.1.is_none(), "root off setter failed: {:?}", setter_event.1);
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);

    let root_guard = root_agent.lock().await;
    assert!(root_agent.try_lock().is_err(), "root Agent mutex must be held");
    let mut ordinary_entry = worker_provider.arm_wake_entry();
    let message = format!(
        "WF1-COV-02 {} ordinary admitted turn",
        if display_role { "display" } else { "normal" }
    );
    let started = if display_role {
        crate::server::live_turn::run_live_system_turn_if_idle(
            &worker_id,
            &message,
            &sessions,
            context(),
        )
        .await
    } else {
        crate::server::live_turn::run_live_turn_if_idle(
            &worker_id,
            &message,
            None,
            &sessions,
            context(),
        )
        .await
    };
    assert!(started, "busy-root worker turn must be admitted");
    let ordinary_dispatch = tokio::time::timeout(Duration::from_secs(2), &mut ordinary_entry)
        .await
        .expect("ordinary provider entry timeout")
        .expect("ordinary provider entry channel closed");
    assert!(ordinary_dispatch.prompt.contains(&message));
    assert_eq!(ordinary_dispatch.effective_override, Some(None));
    assert!(root_agent.try_lock().is_err(), "root guard must survive provider entry");
    await_cov02_worker_done(&mut member_event_rx).await;
    assert!(root_agent.try_lock().is_err(), "root guard must survive worker Done0");
    assert_eq!(members.read().await.get(&worker_id).unwrap().status, "ready");
    await_cov02_worker_idle(&worker_id, &sessions, &members).await;
    assert!(root_agent.try_lock().is_err(), "root guard must survive worker release");
    drop(root_guard);

    let final_sessions = sessions.read().await;
    assert!(Arc::ptr_eq(
        &final_sessions.get(&root_id).unwrap().agent(),
        &initial_root_agent
    ));
    assert!(Arc::ptr_eq(
        &final_sessions.get(&worker_id).unwrap().agent(),
        &initial_worker_agent
    ));
    assert!(Arc::ptr_eq(
        &final_sessions.get(&root_id).unwrap().fast_state(),
        &root_state
    ));
    assert!(Arc::ptr_eq(
        &final_sessions.get(&worker_id).unwrap().fast_state(),
        &root_state
    ));
    worker_provider.assert_dispatch_sequence(&[
        (
            &format!("WF1-COV-02 prior priority {display_role}"),
            Some(Some("priority".to_string())),
        ),
        (&message, Some(None)),
    ]);
}

#[tokio::test]
async fn socket_c_resume_1_cold_reloads_worker_with_parent_fast_state() {
    let _storage = crate::storage::lock_test_env();
    let _env = FastMatrixTestEnv::new();
    let root_provider = Arc::new(RecordingServiceTierProvider::new_with_service_tier(
        "gpt-5.6-luna",
        Some("priority"),
    ));
    let provider_template: Arc<dyn Provider> = root_provider.clone();
    let (client_stream, server_task, sessions) =
        start_astra_ingress_probe_client(provider_template).await;
    let (client_reader, mut client_writer) = client_stream.into_split();
    let mut client_reader = BufReader::new(client_reader);
    let working_dir = std::env::current_dir()
        .expect("C-RESUME-1 working directory")
        .to_string_lossy()
        .into_owned();

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Subscribe {
            id: 1,
            working_dir: Some(working_dir.clone()),
            selfdev: None,
            target_session_id: None,
            client_instance_id: None,
            client_has_local_history: false,
            allow_session_takeover: false,
            crash_on_disconnect: false,
            continue_on_disconnect: false,
            terminal_env: Vec::new(),
        },
        1,
    )
    .await;
    let (root_id, root_agent, root_state) = {
        let sessions_guard = sessions.read().await;
        let (root_id, root_entry) = sessions_guard
            .iter()
            .next()
            .expect("root Subscribe must register a session");
        (
            root_id.clone(),
            root_entry.agent(),
            root_entry.fast_state(),
        )
    };
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Priority);

    let worker_id = "c-resume-1-worker".to_string();
    let mut saved_worker_session = crate::session::Session::create_with_origin(
        Some(root_id.clone()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    saved_worker_session.id = worker_id.clone();
    saved_worker_session.model = Some("gpt-6-astra".to_string());
    saved_worker_session.working_dir = Some(working_dir.clone());
    saved_worker_session
        .save()
        .expect("save C-RESUME-1 worker snapshot");
    assert!(crate::session::Session::load(&worker_id).is_ok());

    let worker_provider = root_provider.fork();
    worker_provider
        .set_model("gpt-6-astra")
        .expect("set worker model");
    let worker_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&worker_provider),
        Registry::empty(),
        saved_worker_session,
        None,
    )));
    sessions.write().await.insert(
        worker_id.clone(),
        crate::server::SessionAgentEntry::new(Arc::clone(&worker_agent), Arc::clone(&root_state)),
    );

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        fast_matrix_subscribe(2, &worker_id, &working_dir),
        2,
    )
    .await;
    let initial_prompt = "C-RESUME-1 initial priority admission";
    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::Message {
            id: 3,
            content: initial_prompt.to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
        3,
    )
    .await;
    root_provider.assert_dispatch_sequence(&[(
        initial_prompt,
        Some(Some("priority".to_string())),
    )]);

    send_fast_matrix_request(
        &mut client_writer,
        &mut client_reader,
        Request::ResumeSession {
            id: 4,
            session_id: root_id.clone(),
            client_instance_id: None,
            client_has_local_history: false,
            allow_session_takeover: false,
        },
        4,
    )
    .await;
    send_fast_matrix_service_tier_request(
        &mut client_writer,
        &mut client_reader,
        Request::SetServiceTier {
            id: 5,
            service_tier: "off".to_string(),
        },
        5,
        "off",
    )
    .await;
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);

    let old_worker_agent = crate::server::remove_session_agent_entry(&sessions, &worker_id)
        .await
        .expect("existing worker must be removed through the server removal path")
        .agent();
    assert!(sessions.read().await.contains_key(&root_id));
    assert!(crate::session::Session::load(&worker_id).is_ok());
    drop(client_writer);
    server_task
        .await
        .expect("initial C-RESUME-1 server join")
        .expect("initial C-RESUME-1 server result");

    let (resumed_stream, resumed_server_task, resumed_sessions) =
        start_astra_ingress_probe_client_with_sessions(
            root_provider.clone(),
            Arc::clone(&sessions),
        )
        .await;
    let (resumed_reader, mut resumed_writer) = resumed_stream.into_split();
    let mut resumed_reader = BufReader::new(resumed_reader);
    send_fast_matrix_request(
        &mut resumed_writer,
        &mut resumed_reader,
        Request::Subscribe {
            id: 1,
            working_dir: Some(working_dir),
            selfdev: None,
            target_session_id: None,
            client_instance_id: None,
            client_has_local_history: false,
            allow_session_takeover: false,
            crash_on_disconnect: false,
            continue_on_disconnect: false,
            terminal_env: Vec::new(),
        },
        1,
    )
    .await;
    send_fast_matrix_request(
        &mut resumed_writer,
        &mut resumed_reader,
        Request::ResumeSession {
            id: 2,
            session_id: worker_id.clone(),
            client_instance_id: None,
            client_has_local_history: true,
            allow_session_takeover: false,
        },
        2,
    )
    .await;

    let (resumed_agent, resumed_state) = {
        let sessions_guard = resumed_sessions.read().await;
        let resumed_entry = sessions_guard
            .get(&worker_id)
            .expect("ResumeSession must register the worker again");
        (resumed_entry.agent(), resumed_entry.fast_state())
    };
    assert!(!Arc::ptr_eq(&old_worker_agent, &resumed_agent));
    assert!(Arc::ptr_eq(&resumed_state, &root_state));
    assert_eq!(resumed_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);
    {
        let resumed_guard = resumed_agent.lock().await;
        let resumed_session = resumed_guard.session_for_split();
        assert_eq!(resumed_session.parent_id.as_deref(), Some(root_id.as_str()));
        assert_eq!(resumed_session.origin(), crate::session::SessionOrigin::SwarmWorker);
        assert_eq!(resumed_session.model.as_deref(), Some("gpt-6-astra"));
    }

    let resumed_prompt = "C-RESUME-1 resumed ordinary admission";
    let mut resumed_entry = root_provider.arm_wake_entry();
    send_fast_matrix_request(
        &mut resumed_writer,
        &mut resumed_reader,
        Request::Message {
            id: 3,
            content: resumed_prompt.to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
        3,
    )
    .await;
    let resumed_dispatch = tokio::time::timeout(Duration::from_secs(2), &mut resumed_entry)
        .await
        .expect("resumed provider entry timeout")
        .expect("resumed provider entry channel closed");
    assert!(resumed_dispatch.prompt.contains(resumed_prompt));
    assert_eq!(resumed_dispatch.effective_override, Some(None));
    root_provider.assert_dispatch_sequence(&[
        (
            initial_prompt,
            Some(Some("priority".to_string())),
        ),
        (resumed_prompt, Some(None)),
    ]);

    let final_sessions = resumed_sessions.read().await;
    let current_root_state = final_sessions
        .get(&root_id)
        .expect("root remains resident")
        .fast_state();
    assert!(Arc::ptr_eq(&resumed_state, &current_root_state));
    assert!(Arc::ptr_eq(&current_root_state, &root_state));
    assert!(Arc::ptr_eq(
        &final_sessions.get(&root_id).expect("root remains resident").agent(),
        &root_agent
    ));
    assert!(Arc::ptr_eq(
        &final_sessions.get(&worker_id).expect("resumed worker exists").fast_state(),
        &root_state
    ));
    drop(final_sessions);
    drop(resumed_writer);
    resumed_server_task
        .await
        .expect("resumed C-RESUME-1 server join")
        .expect("resumed C-RESUME-1 server result");
}

#[tokio::test]
async fn socket_matrix_e_1_inflight_fast_off_preserves_effort_and_next_turn() {
    let _storage = crate::storage::lock_test_env();
    let _env = FastMatrixTestEnv::new();
    let root_provider = Arc::new(RecordingServiceTierProvider::new_with_service_tier(
        "gpt-5.6-luna",
        Some("priority"),
    ));
    let provider_template: Arc<dyn Provider> = root_provider.clone();
    let (root_stream, root_server_task, sessions) =
        start_astra_ingress_probe_client(provider_template).await;
    let (root_reader, mut root_writer) = root_stream.into_split();
    let mut root_reader = BufReader::new(root_reader);
    let working_dir = std::env::current_dir()
        .expect("Matrix E-1 working directory")
        .to_string_lossy()
        .into_owned();

    send_fast_matrix_request(
        &mut root_writer,
        &mut root_reader,
        Request::Subscribe {
            id: 1,
            working_dir: Some(working_dir.clone()),
            selfdev: None,
            target_session_id: None,
            client_instance_id: None,
            client_has_local_history: false,
            allow_session_takeover: false,
            crash_on_disconnect: false,
            continue_on_disconnect: false,
            terminal_env: Vec::new(),
        },
        1,
    )
    .await;
    let root_id = sessions
        .read()
        .await
        .keys()
        .next()
        .cloned()
        .expect("Matrix E-1 root session id");
    let root_state = sessions
        .read()
        .await
        .get(&root_id)
        .expect("Matrix E-1 root entry")
        .fast_state();

    send_fast_matrix_reasoning_effort_request(
        &mut root_writer,
        &mut root_reader,
        Request::SetReasoningEffort {
            id: 2,
            effort: "high".to_string(),
            target_session_id: None,
        },
        2,
        "high",
    )
    .await;

    let worker_provider = root_provider.fork_recording();
    let worker_provider_dyn: Arc<dyn Provider> = worker_provider.clone();
    let mut worker_session = crate::session::Session::create_with_origin(
        Some(root_id.clone()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    worker_session.id = "matrix-e-1-worker".to_string();
    worker_session.model = Some("gpt-6-astra".to_string());
    worker_session.working_dir = Some(working_dir.clone());
    let worker_id = worker_session.id.clone();
    let worker_agent = Arc::new(Mutex::new(Agent::new_with_session(
        worker_provider_dyn,
        Registry::empty(),
        worker_session,
        None,
    )));
    sessions.write().await.insert(
        worker_id.clone(),
        crate::server::SessionAgentEntry::new(Arc::clone(&worker_agent), Arc::clone(&root_state)),
    );

    let (worker_stream, worker_server_task, _worker_sessions) =
        start_astra_ingress_probe_client_with_sessions(
            root_provider.clone(),
            Arc::clone(&sessions),
        )
        .await;
    let (worker_reader, mut worker_writer) = worker_stream.into_split();
    let mut worker_reader = BufReader::new(worker_reader);
    send_fast_matrix_request(
        &mut worker_writer,
        &mut worker_reader,
        fast_matrix_subscribe(1, &worker_id, &working_dir),
        1,
    )
    .await;
    send_fast_matrix_reasoning_effort_request(
        &mut worker_writer,
        &mut worker_reader,
        Request::SetReasoningEffort {
            id: 2,
            effort: "medium".to_string(),
            target_session_id: None,
        },
        2,
        "medium",
    )
    .await;
    assert_eq!(
        sessions
            .read()
            .await
            .get(&root_id)
            .expect("Matrix E-1 root remains resident")
            .agent()
            .lock()
            .await
            .provider_reasoning_effort()
            .as_deref(),
        Some("high")
    );
    assert_eq!(
        worker_agent
            .lock()
            .await
            .provider_reasoning_effort()
            .as_deref(),
        Some("medium")
    );

    let (mut lifecycle, release_tx) = worker_provider.arm_inflight();
    let first_prompt = "MATRIX-E-1 in-flight priority effort";
    worker_writer
        .write_all(
            (serde_json::to_string(&Request::Message {
                id: 3,
                content: first_prompt.to_string(),
                images: Vec::new(),
                system_reminder: None,
                active_skill: None,
                no_reply: false,
            })
            .expect("serialize Matrix E-1 first Message")
                + "\n")
                .as_bytes(),
        )
        .await
        .expect("write Matrix E-1 first Message");
    let polled_tag = tokio::time::timeout(Duration::from_secs(2), lifecycle.wait_polled())
        .await
        .expect("Matrix E-1 provider stream poll timeout");
    assert_eq!(polled_tag, "matrix-e-1-inflight-polled");
    let first_dispatch = lifecycle.captured();
    assert!(first_dispatch.prompt.contains(first_prompt));
    assert_eq!(
        first_dispatch.effective_override,
        Some(Some("priority".to_string()))
    );
    assert_eq!(first_dispatch.effort.as_deref(), Some("medium"));
    lifecycle.assert_before_release();

    send_fast_matrix_service_tier_request(
        &mut root_writer,
        &mut root_reader,
        Request::SetServiceTier {
            id: 3,
            service_tier: "off".to_string(),
        },
        3,
        "off",
    )
    .await;
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);
    assert_eq!(root_provider.service_tier().as_deref(), Some("off"));
    assert_eq!(
        sessions
            .read()
            .await
            .get(&root_id)
            .expect("Matrix E-1 root remains resident")
            .agent()
            .lock()
            .await
            .provider_reasoning_effort()
            .as_deref(),
        Some("high")
    );
    assert_eq!(worker_provider.reasoning_effort().as_deref(), Some("medium"));

    let mut line = String::new();
    let ack_read = tokio::time::timeout(
        Duration::from_millis(100),
        worker_reader.read_line(&mut line),
    )
    .await
    .expect("Matrix E-1 request ACK timeout")
    .expect("read Matrix E-1 request ACK");
    assert!(ack_read > 0, "worker socket closed before Matrix E-1 request ACK");
    assert!(matches!(decode_request_or_event(&line), ServerEvent::Ack { id: 3 }));

    let no_terminal_before_release = tokio::time::timeout(Duration::from_millis(100), async {
        loop {
            let mut event_line = String::new();
            let read = worker_reader
                .read_line(&mut event_line)
                .await
                .expect("read Matrix E-1 pre-release event");
            assert!(read > 0, "worker socket closed before Matrix E-1 release");
            match decode_request_or_event(&event_line) {
                ServerEvent::Done { id: 3 } => {
                    panic!("Matrix E-1 request completed before the provider barrier was released")
                }
                ServerEvent::Error { id: 3, message, .. } => {
                    panic!("Matrix E-1 request failed before release: {message}")
                }
                _ => {}
            }
        }
    })
    .await;
    assert!(
        no_terminal_before_release.is_err(),
        "in-flight request reached a terminal event before the provider barrier was released"
    );
    lifecycle.assert_before_release();
    assert_eq!(lifecycle.captured(), first_dispatch);

    release_tx
        .send(())
        .expect("release Matrix E-1 provider barrier");
    loop {
        line.clear();
        let read = tokio::time::timeout(Duration::from_secs(2), worker_reader.read_line(&mut line))
            .await
            .expect("Matrix E-1 first completion timeout")
            .expect("read Matrix E-1 first completion");
        assert!(read > 0, "worker socket closed before Matrix E-1 completion");
        match decode_request_or_event(&line) {
            ServerEvent::Done { id: 3 } => break,
            ServerEvent::Error { id: 3, message, .. } => {
                panic!("Matrix E-1 first request failed: {message}")
            }
            _ => {}
        }
    }

    lifecycle.assert_completed();

    let next_prompt = "MATRIX-E-1 next ordinary effort";
    send_fast_matrix_request(
        &mut worker_writer,
        &mut worker_reader,
        Request::Message {
            id: 4,
            content: next_prompt.to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
        4,
    )
    .await;
    let dispatches = worker_provider.dispatches();
    let matching: Vec<_> = dispatches
        .iter()
        .filter(|dispatch| {
            dispatch.prompt.contains(first_prompt) || dispatch.prompt.contains(next_prompt)
        })
        .collect();
    assert_eq!(matching.len(), 2, "Matrix E-1 must record both real worker admissions");
    assert_eq!(matching[0].effective_override, Some(Some("priority".to_string())));
    assert_eq!(matching[0].effort.as_deref(), Some("medium"));
    assert!(matching[1].prompt.contains(next_prompt));
    assert_eq!(matching[1].effective_override, Some(None));
    assert_eq!(matching[1].effort.as_deref(), Some("medium"));
    assert_eq!(
        worker_agent
            .lock()
            .await
            .provider_reasoning_effort()
            .as_deref(),
        Some("medium")
    );
    assert!(Arc::ptr_eq(
        &sessions
            .read()
            .await
            .get(&worker_id)
            .expect("Matrix E-1 worker remains resident")
            .agent(),
        &worker_agent
    ));

    drop(worker_writer);
    worker_server_task
        .await
        .expect("Matrix E-1 worker server join")
        .expect("Matrix E-1 worker server result");
    drop(root_writer);
    root_server_task
        .await
        .expect("Matrix E-1 root server join")
        .expect("Matrix E-1 root server result");
}

async fn await_cov02_worker_done(
    member_event_rx: &mut mpsc::UnboundedReceiver<ServerEvent>,
) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match member_event_rx.recv().await {
                Some(ServerEvent::Done { id: 0 }) => break,
                Some(ServerEvent::Error { id: 0, message, .. }) => {
                    panic!("WF1-COV-02 worker failed: {message}")
                }
                Some(_) => continue,
                None => panic!("WF1-COV-02 worker event channel closed"),
            }
        }
    })
    .await
    .expect("WF1-COV-02 worker Done0 timeout");
}

async fn await_cov02_worker_idle(
    worker_id: &str,
    sessions: &crate::server::SessionAgents,
    members: &Arc<RwLock<HashMap<String, crate::server::SwarmMember>>>,
) {
    let guard = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(guard) =
                crate::server::live_turn::idle_live_agent(worker_id, sessions, members).await
            {
                break guard;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("WF1-COV-02 worker reservation release timeout");
    drop(guard);
}

struct WakeFixCovObservation {
    dispatches: Vec<RecordedProviderDispatch>,
    terminal_error: Option<String>,
    member_status: String,
    reservation_same_agent: bool,
}

#[tokio::test]
async fn wake_fix_1_cov_01_live_turn_admission_ownership_cases() {
    let _storage = crate::storage::lock_test_env();
    let _env = FastMatrixTestEnv::new();

    for case in [
        WakeFixCovCase::MissingResidentEntry,
        WakeFixCovCase::ReservedAgentArcMismatch,
        WakeFixCovCase::InvalidOwnerState,
        WakeFixCovCase::RetiredOwnerState,
    ] {
        let observation = run_wake_fix_cov_case(case).await;
        assert!(
            observation.dispatches.is_empty(),
            "native ownership failure {:?} must not enter Provider::complete: {:?}",
            case,
            observation.dispatches
        );
        assert!(
            observation.terminal_error.is_some(),
            "native ownership failure {:?} must emit worker Error{{id:0}}",
            case
        );
        assert_eq!(observation.member_status, "failed", "case {case:?}");
        assert!(
            observation.reservation_same_agent,
            "case {case:?} must release the original Agent reservation"
        );
    }

    let unsupported = run_wake_fix_cov_case(WakeFixCovCase::UnsupportedWithoutOwnership).await;
    assert_eq!(unsupported.terminal_error, None);
    assert_eq!(unsupported.member_status, "ready");
    assert!(unsupported.reservation_same_agent);
    assert_eq!(unsupported.dispatches.len(), 1);
    assert_eq!(unsupported.dispatches[0].effective_override, None);
}

async fn run_wake_fix_cov_case(case: WakeFixCovCase) -> WakeFixCovObservation {
    let unsupported = matches!(case, WakeFixCovCase::UnsupportedWithoutOwnership);
    let model = if unsupported {
        "gpt-6-astra[web]"
    } else {
        "gpt-6-astra"
    };
    let provider = Arc::new(RecordingServiceTierProvider::new(model));
    if unsupported {
        provider.enable_wake_fixture_stream();
        provider
            .set_scoped_service_tier_override(Some(Some("priority".to_string())))
            .expect("seed stale unsupported scoped override");
    }
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let parent_id = (!unsupported).then(|| "wf1-cov-root".to_string());
    let mut session = crate::session::Session::create_with_origin(
        parent_id.clone(),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    session.model = Some(model.to_string());
    let worker_id = session.id.clone();
    let agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&provider_dyn),
        Registry::empty(),
        session,
        None,
    )));
    let fast_state = match case {
        WakeFixCovCase::InvalidOwnerState => {
            let invalid_state_provider: Arc<dyn Provider> = Arc::new(CompleteImmediatelyProvider);
            let state = crate::server::RuntimeFastState::from_provider(
                parent_id.clone().expect("native parent"),
                invalid_state_provider.as_ref(),
            );
            let snapshot = state.snapshot();
            assert!(!snapshot.valid, "invalid-owner setup must have valid=false");
            assert!(!snapshot.retired, "invalid-owner setup must have retired=false");
            state
        }
        WakeFixCovCase::RetiredOwnerState => {
            let state = crate::server::RuntimeFastState::from_provider(
                parent_id.clone().expect("native parent"),
                provider.as_ref(),
            );
            state.invalidate();
            state
        }
        _ => crate::server::RuntimeFastState::from_provider(
            parent_id.clone().unwrap_or_else(|| worker_id.clone()),
            provider.as_ref(),
        ),
    };
    let original_entry = crate::server::SessionAgentEntry::new(
        Arc::clone(&agent),
        Arc::clone(&fast_state),
    );
    let sessions: crate::server::SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        worker_id.clone(),
        original_entry.clone(),
    )])));
    let (member_event_tx, mut member_event_rx) = mpsc::unbounded_channel();
    let swarm_id = "wf1-cov-swarm".to_string();
    let members = Arc::new(RwLock::new(HashMap::from([(
        worker_id.clone(),
        crate::server::SwarmMember::from_record(
            jcode_swarm_core::SwarmMemberRecord {
                session_id: worker_id.clone(),
                working_dir: None,
                swarm_id: Some(swarm_id.clone()),
                swarm_enabled: true,
                status: jcode_swarm_core::SwarmLifecycleStatus::Ready,
                detail: None,
                task_label: Some("WF1-COV-01".to_string()),
                friendly_name: Some("wf1-cov-worker".to_string()),
                report_back_to_session_id: parent_id.clone(),
                latest_completion_report: None,
                role: jcode_swarm_core::SwarmRole::Agent,
                is_headless: false,
            },
            member_event_tx,
        ),
    )])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        swarm_id,
        HashSet::from([worker_id.clone()]),
    )])));
    let event_history = Arc::new(RwLock::new(VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (event_tx, _) = broadcast::channel(8);

    let reservation = crate::server::live_turn::idle_live_agent(&worker_id, &sessions, &members)
        .await
        .expect("test worker must be reservable before spawning the real live turn");
    match case {
        WakeFixCovCase::MissingResidentEntry => {
            sessions.write().await.remove(&worker_id);
        }
        WakeFixCovCase::ReservedAgentArcMismatch => {
            let mut mismatch_session = crate::session::Session::create_with_origin(
                parent_id.clone(),
                None,
                crate::session::SessionOrigin::SwarmWorker,
            );
            mismatch_session.model = Some(model.to_string());
            let mismatch_provider: Arc<dyn Provider> = Arc::new(CompleteImmediatelyProvider);
            let mismatch_agent = Arc::new(Mutex::new(Agent::new_with_session(
                mismatch_provider,
                Registry::empty(),
                mismatch_session,
                None,
            )));
            sessions.write().await.insert(
                worker_id.clone(),
                crate::server::SessionAgentEntry::new(mismatch_agent, Arc::clone(&fast_state)),
            );
        }
        _ => {}
    }

    let context = crate::server::live_turn::LiveTurnSwarmContext::new(
        &members,
        &swarms_by_id,
        &event_history,
        &event_counter,
        &event_tx,
    );
    crate::server::live_turn::spawn_tracked_live_turn(
        &worker_id,
        &sessions,
        reservation,
        format!("WF1-COV-01 {:?}", case),
        None,
        None,
        Some(format!("WF1-COV-01 {:?}", case)),
        context,
    )
    .await;

    let terminal_error = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match member_event_rx.recv().await {
                Some(ServerEvent::Error { id: 0, message, .. }) => break Some(message),
                Some(ServerEvent::Done { id: 0 }) => break None,
                Some(_) => continue,
                None => panic!("WF1-COV-01 member event channel closed"),
            }
        }
    })
    .await
    .expect("WF1-COV-01 worker terminal event timeout");

    if matches!(
        case,
        WakeFixCovCase::MissingResidentEntry | WakeFixCovCase::ReservedAgentArcMismatch
    ) {
        sessions
            .write()
            .await
            .insert(worker_id.clone(), original_entry);
    }
    let released = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(guard) =
                crate::server::live_turn::idle_live_agent(&worker_id, &sessions, &members).await
            {
                break guard;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("WF1-COV-01 reservation was not released after terminal event");
    let reservation_same_agent =
        Arc::ptr_eq(tokio::sync::OwnedMutexGuard::mutex(&released), &agent);
    drop(released);
    let member_status = members
        .read()
        .await
        .get(&worker_id)
        .expect("WF1-COV-01 member remains registered")
        .status
        .clone();
    WakeFixCovObservation {
        dispatches: provider.dispatches(),
        terminal_error,
        member_status,
        reservation_same_agent,
    }
}

#[derive(Clone)]
struct RecordingServiceTierProvider {
    model: Arc<StdMutex<String>>,
    effort: Arc<StdMutex<Option<String>>>,
    service_tier: Arc<StdMutex<Option<String>>>,
    scoped_override: Arc<StdMutex<Option<Option<String>>>>,
    dispatches: Arc<StdMutex<Vec<RecordedProviderDispatch>>>,
    service_tier_error: Arc<StdMutex<Option<String>>>,
    wake_fixture_stream: Arc<StdMutex<bool>>,
    wake_record_full_messages: Arc<StdMutex<bool>>,
    wake_entry_tx:
        Arc<StdMutex<Option<tokio::sync::oneshot::Sender<RecordedProviderDispatch>>>>,
    inflight_lifecycle: Arc<StdMutex<Option<Arc<InflightLifecycleState>>>>,
    inflight_release_rx: Arc<StdMutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RecordedProviderDispatch {
    ordinal: usize,
    prompt: String,
    effective_override: Option<Option<String>>,
    effort: Option<String>,
}

struct InflightLifecycleState {
    polled: AtomicBool,
    released: AtomicBool,
    terminal_emitted: AtomicBool,
    dropped_before_release: AtomicBool,
    polled_tx: StdMutex<Option<tokio::sync::oneshot::Sender<&'static str>>>,
    captured: StdMutex<Option<RecordedProviderDispatch>>,
}

struct InflightLifecycle {
    state: Arc<InflightLifecycleState>,
    polled_rx: Option<tokio::sync::oneshot::Receiver<&'static str>>,
}

impl InflightLifecycle {
    async fn wait_polled(&mut self) -> &'static str {
        self.polled_rx
            .take()
            .expect("Matrix E-1 polled receiver already consumed")
            .await
            .expect("Matrix E-1 polled signal channel closed")
    }

    fn captured(&self) -> RecordedProviderDispatch {
        self.state
            .captured
            .lock()
            .expect("Matrix E-1 captured lifecycle lock")
            .clone()
            .expect("Matrix E-1 provider stream was not captured")
    }

    fn assert_before_release(&self) {
        assert!(self.state.polled.load(Ordering::SeqCst));
        assert!(!self.state.released.load(Ordering::SeqCst));
        assert!(!self.state.terminal_emitted.load(Ordering::SeqCst));
        assert!(!self.state.dropped_before_release.load(Ordering::SeqCst));
    }

    fn assert_completed(&self) {
        assert!(self.state.polled.load(Ordering::SeqCst));
        assert!(self.state.released.load(Ordering::SeqCst));
        assert!(self.state.terminal_emitted.load(Ordering::SeqCst));
        assert!(!self.state.dropped_before_release.load(Ordering::SeqCst));
    }
}

struct InflightLifecycleStream {
    state: Arc<InflightLifecycleState>,
    release_rx: Option<tokio::sync::oneshot::Receiver<()>>,
    next_event: u8,
}

impl Stream for InflightLifecycleStream {
    type Item = anyhow::Result<StreamEvent>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if !this.state.polled.swap(true, Ordering::SeqCst) {
            let _ = this
                .state
                .polled_tx
                .lock()
                .expect("Matrix E-1 polled signal lock")
                .take()
                .map(|sender| sender.send("matrix-e-1-inflight-polled"));
        }
        if !this.state.released.load(Ordering::SeqCst) {
            let release_rx = this
                .release_rx
                .as_mut()
                .expect("Matrix E-1 release receiver missing");
            match Pin::new(release_rx).poll(cx) {
                Poll::Ready(Ok(())) => {
                    this.state.released.store(true, Ordering::SeqCst);
                }
                Poll::Ready(Err(_)) => {
                    return Poll::Ready(Some(Err(anyhow::anyhow!(
                        "Matrix E-1 release barrier dropped before completion"
                    ))));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
        match this.next_event {
            0 => {
                this.next_event = 1;
                Poll::Ready(Some(Ok(StreamEvent::TextDelta(
                    "matrix-e-1 lifecycle complete".to_string(),
                ))))
            }
            1 => {
                this.next_event = 2;
                this.state.terminal_emitted.store(true, Ordering::SeqCst);
                Poll::Ready(Some(Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                })))
            }
            _ => Poll::Ready(None),
        }
    }
}

impl Drop for InflightLifecycleStream {
    fn drop(&mut self) {
        if !self.state.released.load(Ordering::SeqCst)
            && !self.state.terminal_emitted.load(Ordering::SeqCst)
        {
            self.state
                .dropped_before_release
                .store(true, Ordering::SeqCst);
        }
    }
}

impl RecordingServiceTierProvider {
    fn new(model: &str) -> Self {
        Self::new_with_service_tier(model, None)
    }

    fn new_with_service_tier(model: &str, service_tier: Option<&str>) -> Self {
        Self {
            model: Arc::new(StdMutex::new(model.to_string())),
            effort: Arc::new(StdMutex::new(None)),
            service_tier: Arc::new(StdMutex::new(service_tier.map(str::to_string))),
            scoped_override: Arc::new(StdMutex::new(None)),
            dispatches: Arc::new(StdMutex::new(Vec::new())),
            service_tier_error: Arc::new(StdMutex::new(None)),
            wake_fixture_stream: Arc::new(StdMutex::new(false)),
            wake_record_full_messages: Arc::new(StdMutex::new(false)),
            wake_entry_tx: Arc::new(StdMutex::new(None)),
            inflight_lifecycle: Arc::new(StdMutex::new(None)),
            inflight_release_rx: Arc::new(StdMutex::new(None)),
        }
    }

    fn enable_wake_fixture_stream(&self) {
        *self
            .wake_fixture_stream
            .lock()
            .expect("wake fixture stream lock") = true;
        *self
            .wake_record_full_messages
            .lock()
            .expect("wake full-message recording lock") = true;
    }

    fn arm_wake_entry(&self) -> tokio::sync::oneshot::Receiver<RecordedProviderDispatch> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        *self.wake_entry_tx.lock().expect("wake entry sender lock") = Some(sender);
        receiver
    }

    fn arm_inflight(&self) -> (InflightLifecycle, tokio::sync::oneshot::Sender<()>) {
        let (polled_sender, polled_receiver) = tokio::sync::oneshot::channel();
        let (release_sender, release_receiver) = tokio::sync::oneshot::channel();
        let state = Arc::new(InflightLifecycleState {
            polled: AtomicBool::new(false),
            released: AtomicBool::new(false),
            terminal_emitted: AtomicBool::new(false),
            dropped_before_release: AtomicBool::new(false),
            polled_tx: StdMutex::new(Some(polled_sender)),
            captured: StdMutex::new(None),
        });
        *self
            .inflight_lifecycle
            .lock()
            .expect("in-flight lifecycle lock") = Some(Arc::clone(&state));
        *self
            .inflight_release_rx
            .lock()
            .expect("in-flight release receiver lock") = Some(release_receiver);
        (
            InflightLifecycle {
                state,
                polled_rx: Some(polled_receiver),
            },
            release_sender,
        )
    }

    fn fail_next_service_tier(&self, message: &str) {
        *self
            .service_tier_error
            .lock()
            .expect("service tier error lock") = Some(message.to_string());
    }

    fn scoped_override(&self) -> Option<Option<String>> {
        self.scoped_override
            .lock()
            .expect("scoped override lock")
            .clone()
    }

    fn dispatches(&self) -> Vec<RecordedProviderDispatch> {
        self.dispatches
            .lock()
            .expect("dispatch lock")
            .clone()
    }

    fn assert_dispatch_sequence(
        &self,
        expected: &[(&str, Option<Option<String>>)],
    ) {
        let matching: Vec<_> = self
            .dispatches()
            .into_iter()
            .filter(|dispatch| {
                expected
                    .iter()
                    .any(|(prompt, _)| dispatch.prompt.contains(prompt))
            })
            .collect();
        assert_eq!(
            matching.len(),
            expected.len(),
            "recorded provider dispatch count did not match admitted messages"
        );
        for (index, (dispatch, (prompt, expected_override))) in
            matching.iter().zip(expected.iter()).enumerate()
        {
            assert!(
                dispatch.prompt.contains(prompt),
                "dispatch {} did not correlate to admitted message {:?}: {:?}",
                index + 1,
                prompt,
                dispatch.prompt
            );
            assert_eq!(&dispatch.effective_override, expected_override);
            if index > 0 {
                let previous = &matching[index - 1];
                assert!(
                    dispatch.ordinal > previous.ordinal,
                    "recorded dispatch order regressed: {} then {}",
                    previous.ordinal,
                    dispatch.ordinal
                );
            }
        }
    }

    fn fork_recording(&self) -> Arc<Self> {
        Arc::new(Self {
            model: Arc::new(StdMutex::new(self.model())),
            effort: Arc::new(StdMutex::new(self.reasoning_effort())),
            service_tier: Arc::clone(&self.service_tier),
            scoped_override: Arc::clone(&self.scoped_override),
            dispatches: Arc::clone(&self.dispatches),
            service_tier_error: Arc::clone(&self.service_tier_error),
            wake_fixture_stream: Arc::clone(&self.wake_fixture_stream),
            wake_record_full_messages: Arc::clone(&self.wake_record_full_messages),
            wake_entry_tx: Arc::clone(&self.wake_entry_tx),
            inflight_lifecycle: Arc::clone(&self.inflight_lifecycle),
            inflight_release_rx: Arc::clone(&self.inflight_release_rx),
        })
    }
}

#[async_trait]
impl Provider for RecordingServiceTierProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let effective_override = self.scoped_override();
        let record_full_messages = *self
            .wake_record_full_messages
            .lock()
            .expect("wake full-message recording lock");
        let prompt = if record_full_messages {
            messages
                .iter()
                .flat_map(|message| message.content.iter())
                .filter_map(|block| match block {
                    ContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            messages
                .last()
                .map(|message| match message.content.first() {
                    Some(ContentBlock::Text { text, .. }) => text.clone(),
                    _ => String::new(),
                })
                .unwrap_or_default()
        };
        let recorded = {
            let mut dispatches = self.dispatches.lock().expect("dispatch lock");
            let ordinal = dispatches.len() + 1;
            dispatches.push(RecordedProviderDispatch {
                ordinal,
                prompt,
                effective_override,
                effort: self.reasoning_effort(),
            });
            dispatches.last().expect("recorded provider dispatch").clone()
        };
        if let Some(sender) = self.wake_entry_tx.lock().expect("wake entry sender lock").take() {
            let _ = sender.send(recorded.clone());
        }
        if let Some(state) = self
            .inflight_lifecycle
            .lock()
            .expect("in-flight lifecycle lock")
            .take()
        {
            *state
                .captured
                .lock()
                .expect("Matrix E-1 captured lifecycle lock") = Some(recorded.clone());
            let release_rx = self
                .inflight_release_rx
                .lock()
                .expect("in-flight release receiver lock")
                .take()
                .expect("Matrix E-1 release receiver missing");
            return Ok(Box::pin(InflightLifecycleStream {
                state,
                release_rx: Some(release_rx),
                next_event: 0,
            }));
        }
        if *self
            .wake_fixture_stream
            .lock()
            .expect("wake fixture stream lock")
        {
            Ok(Box::pin(stream::iter(vec![
                Ok(StreamEvent::TextDelta("wake fixture complete".to_string())),
                Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }),
            ])))
        } else {
            Ok(Box::pin(stream::iter(vec![Ok(StreamEvent::MessageEnd {
                stop_reason: None,
            })])))
        }
    }

    fn name(&self) -> &str {
        "recording-service-tier"
    }

    fn model(&self) -> String {
        self.model.lock().expect("model lock").clone()
    }

    fn reasoning_effort(&self) -> Option<String> {
        self.effort.lock().expect("effort lock").clone()
    }

    fn set_reasoning_effort(&self, effort: &str) -> anyhow::Result<()> {
        *self.effort.lock().expect("effort lock") = Some(effort.to_string());
        Ok(())
    }

    fn available_efforts(&self) -> Vec<&'static str> {
        vec!["low", "medium", "high"]
    }

    fn available_models_for_switching(&self) -> Vec<String> {
        vec![
            "gpt-6-astra".to_string(),
            "gpt-6-astra[web]".to_string(),
            "offline-analyst".to_string(),
        ]
    }

    fn set_model(&self, model: &str) -> anyhow::Result<()> {
        *self.model.lock().expect("model lock") = model
            .strip_prefix("openai-api:")
            .or_else(|| model.strip_prefix("openai-oauth:"))
            .or_else(|| model.strip_prefix("openai:"))
            .unwrap_or(model)
            .to_string();
        Ok(())
    }

    fn service_tier(&self) -> Option<String> {
        self.service_tier
            .lock()
            .expect("service tier lock")
            .clone()
    }

    fn set_service_tier(&self, service_tier: &str) -> anyhow::Result<()> {
        if let Some(error) = self
            .service_tier_error
            .lock()
            .expect("service tier error lock")
            .take()
        {
            return Err(anyhow::anyhow!(error));
        }
        *self.service_tier.lock().expect("service tier lock") = Some(service_tier.to_string());
        Ok(())
    }

    fn supports_scoped_service_tier_override(&self) -> bool {
        true
    }

    fn set_scoped_service_tier_override(
        &self,
        override_tier: Option<Option<String>>,
    ) -> anyhow::Result<()> {
        *self
            .scoped_override
            .lock()
            .expect("scoped override lock") = override_tier;
        Ok(())
    }

    fn fork(&self) -> Arc<dyn Provider> {
        self.fork_recording()
    }
}

#[derive(Clone, Default)]
struct FanoutStreamProvider;

#[async_trait]
impl Provider for FanoutStreamProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(stream::unfold(0_u8, |step| async move {
            match step {
                0 => Some((Ok(StreamEvent::TextDelta("before attach".to_string())), 1)),
                1 => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Some((Ok(StreamEvent::TextDelta("after attach".to_string())), 2))
                }
                2 => Some((
                    Ok(StreamEvent::MessageEnd {
                        stop_reason: Some("end_turn".to_string()),
                    }),
                    3,
                )),
                _ => None,
            }
        })))
    }

    fn name(&self) -> &str {
        "fanout-stream"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

#[async_trait]
impl Provider for PanicOnForkProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        panic!("complete should never run in lightweight control test")
    }

    fn name(&self) -> &str {
        "panic-on-fork"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        self.forked.store(true, Ordering::SeqCst);
        panic!("fork should not run for lightweight control requests")
    }
}

#[tokio::test]
async fn astra_busy_admission_rejects_before_waiting_for_root_agent_lock() {
    let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
        forked: Arc::new(AtomicBool::new(false)),
    });
    let registry = Registry::new(Arc::clone(&provider)).await;
    let mut session = crate::session::Session::create_with_id(
        "session_astra_busy_admission".to_string(),
        None,
        None,
    );
    session.model = Some("panic-on-fork".to_string());
    let agent = Arc::new(Mutex::new(Agent::new_with_session(
        provider, registry, session, None,
    )));
    let astra_state = Arc::new(Mutex::new(crate::agent::AstraFirstState {
        session_id: "session_astra_busy_admission".to_string(),
        generation: 4,
        analyst_id: Some("analyst".to_string()),
        analyst_model: Some("gpt-6-astra[web]".to_string()),
        analyst_route: Some("chatgpt-web".to_string()),
        analyst_effort: Some("high".to_string()),
        busy: true,
        cancelled: false,
        active_child: None,
        cancel_notify: Arc::new(tokio::sync::Notify::new()),
        enabled: true,
    }));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();
    let (processing_done_tx, _processing_done_rx) = mpsc::unbounded_channel();
    let mut client_is_processing = false;
    let mut processing_message_id = None;
    let mut processing_session_id = None;
    let mut processing_task = None;
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _) = broadcast::channel(8);
    let client_connections = Arc::new(RwLock::new(HashMap::new()));

    let _root_agent_lock = agent.lock().await;
    tokio::time::timeout(
        Duration::from_millis(100),
        start_processing_message(
            ProcessingMessage {
                id: 901,
                content: "must be rejected while Astra is busy".to_string(),
                images: Vec::new(),
                system_reminder: None,
                active_skill: None,
            },
            "session_astra_busy_admission",
            "connection-astra-busy",
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut processing_message_id,
                session_id: &mut processing_session_id,
                task: &mut processing_task,
            },
            &agent,
            None,
            &client_event_tx,
            &processing_done_tx,
            Vec::new(),
            &client_connections,
            &SwarmStatusRefs {
                members: &swarm_members,
                swarms_by_id: &swarms_by_id,
                event_history: &event_history,
                event_counter: &event_counter,
                event_tx: &swarm_event_tx,
            },
            Some(&astra_state),
            None,
            None,
        ),
    )
    .await
    .expect("busy Astra admission must not wait for the root Agent lock");

    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::Error { id: 901, message, .. })
            if message.contains("Astra-first turn is busy")
    ));
    assert!(!client_is_processing);
    assert!(processing_task.is_none());
    assert!(astra_first::is_busy(&astra_state).await);
}

#[test]
fn ping_request_is_lightweight_control_request() {
    assert!((Request::Ping { id: 1 }).is_lightweight_control_request());
}

fn subscribe_request(working_dir: Option<&str>) -> Request {
    Request::Subscribe {
        id: 1,
        working_dir: working_dir.map(str::to_string),
        selfdev: None,
        target_session_id: None,
        client_instance_id: None,
        client_has_local_history: false,
        allow_session_takeover: false,
        crash_on_disconnect: false,
        continue_on_disconnect: false,
        terminal_env: Vec::new(),
    }
}

#[test]
fn initial_subscribe_requires_an_absolute_client_working_dir() {
    for invalid in [None, Some(""), Some("relative/project")] {
        let error = initial_subscribe_working_dir(&subscribe_request(invalid))
            .expect_err("invalid client cwd must be rejected before session creation");
        assert!(error.contains("working_dir") || error.contains("working directory"));
    }

    let absolute = std::env::temp_dir().join("jcode-client-project");
    assert_eq!(
        initial_subscribe_working_dir(&subscribe_request(absolute.to_str()))
            .expect("absolute client cwd"),
        absolute.to_string_lossy()
    );

    let error = initial_subscribe_working_dir(&Request::GetState { id: 2 })
        .expect_err("stateful requests must not create an unbound session");
    assert!(error.contains("must Subscribe"));
}

#[test]
fn remote_subscribe_requires_an_existing_server_directory() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let file = directory.path().join("not-a-directory");
    std::fs::write(&file, "file")?;
    let missing = directory.path().join("missing");
    for path in [&file, &missing] {
        let mut request = subscribe_request(path.to_str());
        assert!(
            initial_subscribe_working_dir(&request).is_ok(),
            "local subscription behavior remains unchanged"
        );
        if let Request::Subscribe {
            continue_on_disconnect,
            ..
        } = &mut request
        {
            *continue_on_disconnect = true;
        }
        assert!(initial_subscribe_working_dir(&request)
            .unwrap_err()
            .contains("must exist and be a directory on the server"));
    }
    assert_eq!(
        validated_subscribe_working_dir(directory.path().to_str(), true)
            .expect("existing directory"),
        directory.path().to_str().unwrap()
    );
    Ok(())
}

#[tokio::test]
async fn new_client_agent_stamps_client_cwd_into_initial_context() {
    let provider: Arc<dyn Provider> = Arc::new(CompleteImmediatelyProvider);
    let registry = Registry::new(Arc::clone(&provider)).await;
    let client_cwd = std::env::temp_dir().join("jcode-authoritative-client-project");
    let client_cwd = client_cwd.to_string_lossy();
    let agent = Agent::new_with_initial_working_dir(provider, registry, Some(&client_cwd));

    assert_eq!(agent.working_dir(), Some(client_cwd.as_ref()));
    let context = agent.messages()[0].content_preview();
    assert!(
        context.contains(&format!("Working directory: {client_cwd}")),
        "initial context must be created from the client cwd: {context}"
    );
}

#[test]
fn server_reload_starting_is_true_only_for_recent_starting_marker() {
    let _guard = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();

    assert!(!server_reload_starting());

    crate::server::write_reload_state(
        "reload-lifecycle-test",
        "test-hash",
        crate::server::ReloadPhase::Starting,
        Some("session_test_reload".to_string()),
    );
    assert!(server_reload_starting());

    crate::server::write_reload_state(
        "reload-lifecycle-test",
        "test-hash",
        crate::server::ReloadPhase::SocketReady,
        Some("session_test_reload".to_string()),
    );
    assert!(!server_reload_starting());
}

#[test]
fn reload_starting_rejects_new_turn_without_spawning_processing_task() {
    let _guard = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();
    crate::server::write_reload_state(
        "reload-lifecycle-starting",
        "test-hash",
        crate::server::ReloadPhase::Starting,
        Some("session_guard".to_string()),
    );

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let forked = Arc::new(AtomicBool::new(false));
        let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
            forked: Arc::clone(&forked),
        });
        let registry = Registry::new(Arc::clone(&provider)).await;
        let mut session =
            crate::session::Session::create_with_id("session_guard".to_string(), None, None);
        session.model = Some("panic-on-fork".to_string());
        let agent = Arc::new(Mutex::new(Agent::new_with_session(
            provider, registry, session, None,
        )));

        let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
        let (processing_done_tx, mut processing_done_rx) = mpsc::unbounded_channel();
        let mut client_is_processing = false;
        let mut processing_message_id = None;
        let mut processing_session_id = None;
        let mut processing_task = None;
        let swarm_members = Arc::new(RwLock::new(HashMap::new()));
        let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
        let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
        let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (swarm_event_tx, _) = broadcast::channel(8);

        start_processing_message(
            ProcessingMessage {
                id: 42,
                content: "do not start during reload".to_string(),
                images: Vec::new(),
                system_reminder: None,
                active_skill: None,
            },
            "session_guard",
            "test-connection",
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut processing_message_id,
                session_id: &mut processing_session_id,
                task: &mut processing_task,
            },
            &agent,
            None,
            &client_event_tx,
            &processing_done_tx,
            Vec::new(),
            &Arc::new(RwLock::new(HashMap::new())),
            &SwarmStatusRefs {
                members: &swarm_members,
                swarms_by_id: &swarms_by_id,
                event_history: &event_history,
                event_counter: &event_counter,
                event_tx: &swarm_event_tx,
            },
            None,
            None,
            None,
        )
        .await;

        let event = client_event_rx
            .recv()
            .await
            .expect("reload event should be sent to client");
        assert!(matches!(event, ServerEvent::Reloading { new_socket: None }));
        assert!(
            client_event_rx.try_recv().is_err(),
            "reload guard should only emit the reload notification"
        );
        assert!(!client_is_processing);
        assert_eq!(processing_message_id, None);
        assert_eq!(processing_session_id, None);
        assert!(processing_task.is_none());
        assert!(processing_done_rx.try_recv().is_err());
        assert!(
            !forked.load(Ordering::SeqCst),
            "rejecting during reload should not fork or invoke provider work"
        );
    });
}

#[tokio::test]
async fn client_initiated_turn_fans_out_stream_and_terminal_events_to_live_attachments() {
    let _guard = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();
    let session_id = "session_live_attachment_fanout";

    let provider: Arc<dyn Provider> = Arc::new(FanoutStreamProvider);
    let registry = Registry::new(Arc::clone(&provider)).await;
    let mut session = crate::session::Session::create_with_id(session_id.to_string(), None, None);
    session.model = Some("fanout-stream".to_string());
    let agent = Arc::new(Mutex::new(Agent::new_with_session(
        provider, registry, session, None,
    )));

    let (origin_tx, mut origin_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let (attached_tx, mut attached_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let swarm_members = Arc::new(RwLock::new(HashMap::from([(
        session_id.to_string(),
        SwarmMember {
            session_id: session_id.to_string(),
            event_tx: origin_tx.clone(),
            event_txs: HashMap::from([("origin".to_string(), origin_tx.clone())]),
            working_dir: None,
            swarm_id: None,
            swarm_enabled: false,
            status: "ready".to_string(),
            detail: None,
            task_label: None,
            friendly_name: None,
            report_back_to_session_id: None,
            latest_completion_report: None,
            role: "agent".to_string(),
            joined_at: Instant::now(),
            last_status_change: Instant::now(),
            is_headless: false,
            output_tail: None,
            todo_progress: None,
            todo_items: Vec::new(),
            runtime: crate::protocol::SwarmMemberRuntime::default(),
        },
    )])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _) = broadcast::channel(8);
    let (processing_done_tx, mut processing_done_rx) = mpsc::unbounded_channel();
    let mut client_is_processing = false;
    let mut processing_message_id = None;
    let mut processing_session_id = None;
    let mut processing_task = None;

    start_processing_message(
        ProcessingMessage {
            id: 479,
            content: "stream to every attachment".to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
        },
        session_id,
        "test-connection",
        &mut ProcessingState {
            client_is_processing: &mut client_is_processing,
            message_id: &mut processing_message_id,
            session_id: &mut processing_session_id,
            task: &mut processing_task,
        },
        &agent,
        None,
        &origin_tx,
        &processing_done_tx,
        Vec::new(),
        &Arc::new(RwLock::new(HashMap::new())),
        &SwarmStatusRefs {
            members: &swarm_members,
            swarms_by_id: &swarms_by_id,
            event_history: &event_history,
            event_counter: &event_counter,
            event_tx: &swarm_event_tx,
        },
        None,
        None,
        None,
    )
    .await;

    loop {
        let event = tokio::time::timeout(Duration::from_secs(2), origin_rx.recv())
            .await
            .expect("origin should receive the initial stream event promptly")
            .expect("origin event channel should remain open");
        if matches!(event, ServerEvent::TextDelta { ref text } if text == "before attach") {
            break;
        }
    }

    crate::server::register_session_event_sender(
        &swarm_members,
        session_id,
        "attached",
        attached_tx,
    )
    .await;

    for rx in [&mut origin_rx, &mut attached_rx] {
        let mut saw_post_attach_delta = false;
        let mut saw_message_end = false;
        let mut saw_done = false;
        while !saw_post_attach_delta || !saw_message_end || !saw_done {
            let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("attachment should receive streamed event promptly")
                .expect("attachment event channel should remain open");
            saw_post_attach_delta |= matches!(
                event,
                ServerEvent::TextDelta { ref text } if text == "after attach"
            );
            if matches!(event, ServerEvent::MessageEnd { .. }) {
                assert!(!saw_done, "MessageEnd must precede the terminal Done event");
                saw_message_end = true;
            }
            saw_done |= matches!(event, ServerEvent::Done { id: 479 });
        }
    }

    let (done_id, result, _) =
        tokio::time::timeout(Duration::from_secs(2), processing_done_rx.recv())
            .await
            .expect("processing should complete promptly")
            .expect("processing completion channel should remain open");
    assert_eq!(done_id, 479);
    result.expect("turn should complete successfully");

    if let Some(handle) = processing_task.take() {
        handle.await.expect("processing task join");
    }
}

#[test]
fn accepted_reload_recovery_continuation_marks_intent_delivered() -> anyhow::Result<()> {
    let _lock = crate::storage::lock_test_env();
    let _env = IsolatedReloadRecoveryEnv::new();
    let session_id = "session_accepted_reload_recovery";
    let continuation = "stored continuation accepted by server";

    super::super::reload_recovery::persist_intent(
        "reload-accepted-continuation",
        session_id,
        super::super::reload_recovery::ReloadRecoveryRole::InterruptedPeer,
        crate::tool::selfdev::ReloadRecoveryDirective {
            reconnect_notice: Some("stored notice".to_string()),
            continuation_message: continuation.to_string(),
        },
        "synthetic accepted continuation test",
    )?;
    assert!(super::super::reload_recovery::has_pending_for_session(
        session_id
    ));

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let provider: Arc<dyn Provider> = Arc::new(CompleteImmediatelyProvider);
        let registry = Registry::new(Arc::clone(&provider)).await;
        let mut session =
            crate::session::Session::create_with_id(session_id.to_string(), None, None);
        session.model = Some("complete-immediately".to_string());
        let agent = Arc::new(Mutex::new(Agent::new_with_session(
            provider, registry, session, None,
        )));

        let (client_event_tx, _client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
        let (processing_done_tx, mut processing_done_rx) = mpsc::unbounded_channel();
        let mut client_is_processing = false;
        let mut processing_message_id = None;
        let mut processing_session_id = None;
        let mut processing_task = None;
        let swarm_members = Arc::new(RwLock::new(HashMap::new()));
        let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
        let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
        let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (swarm_event_tx, _) = broadcast::channel(8);

        start_processing_message(
            ProcessingMessage {
                id: 77,
                content: "continue after reload".to_string(),
                images: Vec::new(),
                system_reminder: Some(continuation.to_string()),
                active_skill: None,
            },
            session_id,
            "test-connection",
            &mut ProcessingState {
                client_is_processing: &mut client_is_processing,
                message_id: &mut processing_message_id,
                session_id: &mut processing_session_id,
                task: &mut processing_task,
            },
            &agent,
            None,
            &client_event_tx,
            &processing_done_tx,
            Vec::new(),
            &Arc::new(RwLock::new(HashMap::new())),
            &SwarmStatusRefs {
                members: &swarm_members,
                swarms_by_id: &swarms_by_id,
                event_history: &event_history,
                event_counter: &event_counter,
                event_tx: &swarm_event_tx,
            },
            None,
            None,
            None,
        )
        .await;

        assert!(client_is_processing);
        assert_eq!(processing_message_id, Some(77));
        assert_eq!(processing_session_id.as_deref(), Some(session_id));
        assert!(processing_task.is_some());
        assert!(
            !super::super::reload_recovery::has_pending_for_session(session_id),
            "server acceptance of the exact hidden continuation should consume the durable intent"
        );

        let (done_id, result, _report) =
            tokio::time::timeout(std::time::Duration::from_secs(5), processing_done_rx.recv())
                .await
                .expect("processing task should finish")
                .expect("processing task should report completion");
        assert_eq!(done_id, 77);
        result?;
        if let Some(handle) = processing_task.take() {
            handle.await.expect("processing task join");
        }
        Ok::<(), anyhow::Error>(())
    })?;

    Ok(())
}

#[test]
fn reload_starting_rejects_new_turns_for_multiple_sessions() {
    let _guard = crate::storage::lock_test_env();
    let _runtime = IsolatedRuntimeDir::new();
    crate::server::write_reload_state(
        "reload-lifecycle-multi-starting",
        "test-hash",
        crate::server::ReloadPhase::Starting,
        Some("session_alpha".to_string()),
    );

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let forked = Arc::new(AtomicBool::new(false));
        let provider: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
            forked: Arc::clone(&forked),
        });
        let registry = Registry::new(Arc::clone(&provider)).await;
        let swarm_members = Arc::new(RwLock::new(HashMap::new()));
        let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
        let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
        let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (swarm_event_tx, _) = broadcast::channel(8);

        for (message_id, session_id) in [
            (101, "session_alpha"),
            (102, "session_beta"),
            (103, "session_gamma"),
        ] {
            let mut session =
                crate::session::Session::create_with_id(session_id.to_string(), None, None);
            session.model = Some("panic-on-fork".to_string());
            let agent = Arc::new(Mutex::new(Agent::new_with_session(
                Arc::clone(&provider),
                registry.clone(),
                session,
                None,
            )));

            let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
            let (processing_done_tx, mut processing_done_rx) = mpsc::unbounded_channel();
            let mut client_is_processing = false;
            let mut processing_message_id = None;
            let mut processing_session_id = None;
            let mut processing_task = None;

            start_processing_message(
                ProcessingMessage {
                    id: message_id,
                    content: format!("do not start {session_id} during reload"),
                    images: Vec::new(),
                    system_reminder: None,
                    active_skill: None,
                },
                session_id,
                "test-connection",
                &mut ProcessingState {
                    client_is_processing: &mut client_is_processing,
                    message_id: &mut processing_message_id,
                    session_id: &mut processing_session_id,
                    task: &mut processing_task,
                },
                &agent,
                None,
                &client_event_tx,
                &processing_done_tx,
                Vec::new(),
                &Arc::new(RwLock::new(HashMap::new())),
                &SwarmStatusRefs {
                    members: &swarm_members,
                    swarms_by_id: &swarms_by_id,
                    event_history: &event_history,
                    event_counter: &event_counter,
                    event_tx: &swarm_event_tx,
                },
                None,
                None,
                None,
            )
            .await;

            let event = tokio::time::timeout(
                std::time::Duration::from_millis(250),
                client_event_rx.recv(),
            )
            .await
            .expect("reload guard should emit promptly for every session")
            .expect("reload event should be sent to client");
            assert!(
                matches!(event, ServerEvent::Reloading { new_socket: None }),
                "expected Reloading event for {session_id}, got {event:?}"
            );
            assert!(
                client_event_rx.try_recv().is_err(),
                "reload guard should only emit one reload notification for {session_id}"
            );
            assert!(
                !client_is_processing,
                "{session_id} should not enter processing during reload"
            );
            assert_eq!(processing_message_id, None);
            assert_eq!(processing_session_id, None);
            assert!(
                processing_task.is_none(),
                "{session_id} should not spawn a processing task during reload"
            );
            assert!(processing_done_rx.try_recv().is_err());
        }

        assert!(
            !forked.load(Ordering::SeqCst),
            "rejecting multiple sessions during reload should not fork or invoke provider work"
        );
    });
}

#[tokio::test]
async fn lightweight_comm_request_skips_full_session_initialization() {
    let (server_stream, client_stream) = crate::transport::Stream::pair().expect("socket pair");
    let forked = Arc::new(AtomicBool::new(false));
    let provider_template: Arc<dyn Provider> = Arc::new(PanicOnForkProvider {
        forked: Arc::clone(&forked),
    });

    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::new()));
    let global_session_id = Arc::new(RwLock::new(String::new()));
    let client_count = Arc::new(RwLock::new(0usize));
    let client_connections = Arc::new(RwLock::new(HashMap::new()));
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let shared_context = Arc::new(RwLock::new(HashMap::new()));
    let swarm_plans = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::new()));
    let file_touch = FileTouchService::new();
    let channel_subscriptions = Arc::new(RwLock::new(HashMap::new()));
    let channel_subscriptions_by_session = Arc::new(RwLock::new(HashMap::new()));
    let client_debug_state = Arc::new(RwLock::new(ClientDebugState::default()));
    let (_debug_response_tx, _) = broadcast::channel(8);
    let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _) = broadcast::channel(8);
    let (_global_event_tx, _) = broadcast::channel(8);
    let global_is_processing = Arc::new(RwLock::new(false));
    let shutdown_signals = Arc::new(RwLock::new(HashMap::new()));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());

    let server_task = tokio::spawn(handle_client(
        server_stream,
        Arc::clone(&sessions),
        _global_event_tx,
        provider_template,
        global_is_processing,
        global_session_id,
        client_count,
        Arc::clone(&client_connections),
        swarm_members,
        swarms_by_id,
        shared_context,
        swarm_plans,
        swarm_coordinators,
        file_touch,
        channel_subscriptions,
        channel_subscriptions_by_session,
        client_debug_state,
        _debug_response_tx,
        event_history,
        event_counter,
        swarm_event_tx,
        "jcode-test".to_string(),
        "🧪".to_string(),
        mcp_pool,
        shutdown_signals,
        soft_interrupt_queues,
        AwaitMembersRuntime::default(),
        SwarmMutationRuntime::default(),
    ));

    let (client_reader, mut client_writer) = client_stream.into_split();
    let mut client_reader = BufReader::new(client_reader);
    let ping = Request::Ping { id: 6 };
    let payload = serde_json::to_string(&ping).expect("serialize Ping") + "\n";
    client_writer
        .write_all(payload.as_bytes())
        .await
        .expect("write Ping");

    let mut line = String::new();
    client_reader
        .read_line(&mut line)
        .await
        .expect("read Pong bytes");
    let pong = decode_request_or_event(&line);
    assert!(matches!(
        pong,
        ServerEvent::Pong {
            id: 6,
            session_preview_protocol: Some(1),
            ..
        }
    ));

    let request = Request::CommList {
        id: 7,
        session_id: "not-in-swarm".to_string(),
    };
    let payload = serde_json::to_string(&request).expect("serialize request") + "\n";
    client_writer
        .write_all(payload.as_bytes())
        .await
        .expect("write request after Ping");

    line.clear();
    client_reader
        .read_line(&mut line)
        .await
        .expect("read ack bytes after Ping");
    let ack = decode_request_or_event(&line);
    assert!(matches!(ack, ServerEvent::Ack { id: 7 }));

    line.clear();
    client_reader
        .read_line(&mut line)
        .await
        .expect("read terminal response");
    let response = decode_request_or_event(&line);
    match response {
        ServerEvent::Error { id, message, .. } => {
            assert_eq!(id, 7);
            assert!(message.contains("Not in a swarm"));
        }
        other => panic!("expected error response, got {other:?}"),
    }

    line.clear();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), client_reader.read_line(&mut line))
            .await
            .expect("non-Ping lightweight command must close its one-shot connection")
            .expect("read EOF"),
        0,
    );
    drop(client_writer);
    server_task
        .await
        .expect("server task join")
        .expect("server task result");

    assert!(
        !forked.load(Ordering::SeqCst),
        "lightweight control request should not fork a provider"
    );
    assert!(
        client_connections.read().await.is_empty(),
        "lightweight control request should not register a live client session"
    );
    assert!(
        sessions.read().await.is_empty(),
        "lightweight control request should not allocate a live agent session"
    );
}

#[tokio::test]
async fn real_ingress_astra_no_reply_does_not_append_or_run_provider() {
    let _storage = crate::storage::lock_test_env();
    let _env = AstraIngressTestEnv::new("openai-api:mock-analyst");
    let complete_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider_template: Arc<dyn Provider> = Arc::new(AstraIngressProbeProvider {
        complete_calls: Arc::clone(&complete_calls),
        fork_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        calls: Arc::new(std::sync::Mutex::new(Vec::new())),
        model: Arc::new(std::sync::Mutex::new("luna-root".to_string())),
        effort: Arc::new(std::sync::Mutex::new(Some("medium".to_string()))),
        responses: Arc::new(std::sync::Mutex::new(VecDeque::new())),
    });

    let (server_stream, client_stream) = crate::transport::Stream::pair().expect("socket pair");
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::new()));
    let global_session_id = Arc::new(RwLock::new(String::new()));
    let client_count = Arc::new(RwLock::new(0usize));
    let client_connections = Arc::new(RwLock::new(HashMap::new()));
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let shared_context = Arc::new(RwLock::new(HashMap::new()));
    let swarm_plans = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::new()));
    let file_touch = FileTouchService::new();
    let channel_subscriptions = Arc::new(RwLock::new(HashMap::new()));
    let channel_subscriptions_by_session = Arc::new(RwLock::new(HashMap::new()));
    let client_debug_state = Arc::new(RwLock::new(ClientDebugState::default()));
    let (_debug_response_tx, _) = broadcast::channel(8);
    let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _) = broadcast::channel(8);
    let (_global_event_tx, _) = broadcast::channel(8);
    let global_is_processing = Arc::new(RwLock::new(false));
    let shutdown_signals = Arc::new(RwLock::new(HashMap::new()));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());

    let server_task = tokio::spawn(handle_client(
        server_stream,
        Arc::clone(&sessions),
        _global_event_tx,
        Arc::clone(&provider_template),
        global_is_processing,
        global_session_id,
        client_count,
        Arc::clone(&client_connections),
        swarm_members,
        swarms_by_id,
        shared_context,
        swarm_plans,
        swarm_coordinators,
        file_touch,
        channel_subscriptions,
        channel_subscriptions_by_session,
        client_debug_state,
        _debug_response_tx,
        event_history,
        event_counter,
        swarm_event_tx,
        "jcode-test".to_string(),
        "🧪".to_string(),
        mcp_pool,
        shutdown_signals,
        soft_interrupt_queues,
        AwaitMembersRuntime::default(),
        SwarmMutationRuntime::default(),
    ));

    let (client_reader, mut client_writer) = client_stream.into_split();
    let mut client_reader = BufReader::new(client_reader);
    let working_dir = std::env::current_dir()
        .expect("test working directory")
        .to_string_lossy()
        .into_owned();
    let subscribe = Request::Subscribe {
        id: 1,
        working_dir: Some(working_dir),
        selfdev: None,
        target_session_id: None,
        client_instance_id: None,
        client_has_local_history: false,
        allow_session_takeover: false,
        crash_on_disconnect: false,
        continue_on_disconnect: false,
        terminal_env: Vec::new(),
    };
    client_writer
        .write_all(
            (serde_json::to_string(&subscribe).expect("serialize Subscribe") + "\n").as_bytes(),
        )
        .await
        .expect("write Subscribe");

    let mut line = String::new();
    loop {
        line.clear();
        let event =
            tokio::time::timeout(Duration::from_secs(2), client_reader.read_line(&mut line))
                .await
                .expect("Subscribe response timeout")
                .expect("read Subscribe response");
        if event == 0 {
            let server_result = server_task
                .await
                .expect("server task join after early Subscribe EOF");
            panic!("server closed before Subscribe completed: {server_result:?}");
        }
        if matches!(decode_request_or_event(&line), ServerEvent::Done { id: 1 }) {
            break;
        }
    }

    let no_reply = Request::Message {
        id: 2,
        content: "must not append while Astra-first is enabled".to_string(),
        images: Vec::new(),
        system_reminder: None,
        active_skill: None,
        no_reply: true,
    };
    client_writer
        .write_all(
            (serde_json::to_string(&no_reply).expect("serialize no_reply") + "\n").as_bytes(),
        )
        .await
        .expect("write no_reply");
    let mut no_reply_error = None;
    while no_reply_error.is_none() {
        line.clear();
        let event =
            tokio::time::timeout(Duration::from_secs(2), client_reader.read_line(&mut line))
                .await
                .expect("no_reply response timeout")
                .expect("read no_reply response");
        assert!(event > 0, "server closed before no_reply rejection");
        match decode_request_or_event(&line) {
            ServerEvent::Ack { id: 2 } => {}
            ServerEvent::Error { id: 2, message, .. } => no_reply_error = Some(message),
            ServerEvent::SwarmStatus { .. } => {}
            other => panic!("unexpected no_reply event: {other:?}"),
        }
    }
    assert!(no_reply_error
        .as_deref()
        .is_some_and(|message| message.contains("does not support no_reply")));
    assert_eq!(complete_calls.load(Ordering::SeqCst), 0);

    drop(client_writer);
    server_task
        .await
        .expect("server task join")
        .expect("server task result");
    let live_sessions = sessions.read().await;
    for agent in live_sessions.values() {
        assert_eq!(
            agent.lock().await.visible_conversation_message_count(),
            0,
            "denied and no_reply ingress must not append a user turn"
        );
    }
}

async fn start_astra_ingress_probe_client(
    provider_template: Arc<dyn Provider>,
) -> (
    crate::transport::Stream,
    tokio::task::JoinHandle<anyhow::Result<()>>,
    SessionAgents,
) {
    start_astra_ingress_probe_client_with_sessions(
        provider_template,
        Arc::new(RwLock::new(HashMap::new())),
    )
    .await
}

async fn start_astra_ingress_probe_client_with_sessions(
    provider_template: Arc<dyn Provider>,
    sessions: SessionAgents,
) -> (
    crate::transport::Stream,
    tokio::task::JoinHandle<anyhow::Result<()>>,
    SessionAgents,
) {
    let (server_stream, client_stream) = crate::transport::Stream::pair().expect("socket pair");
    let global_session_id = Arc::new(RwLock::new(String::new()));
    let client_count = Arc::new(RwLock::new(0usize));
    let client_connections = Arc::new(RwLock::new(HashMap::new()));
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let shared_context = Arc::new(RwLock::new(HashMap::new()));
    let swarm_plans = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::new()));
    let file_touch = FileTouchService::new();
    let channel_subscriptions = Arc::new(RwLock::new(HashMap::new()));
    let channel_subscriptions_by_session = Arc::new(RwLock::new(HashMap::new()));
    let client_debug_state = Arc::new(RwLock::new(ClientDebugState::default()));
    let (_debug_response_tx, _) = broadcast::channel(8);
    let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _) = broadcast::channel(8);
    let (_global_event_tx, _) = broadcast::channel(8);
    let global_is_processing = Arc::new(RwLock::new(false));
    let shutdown_signals = Arc::new(RwLock::new(HashMap::new()));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());

    let server_task = tokio::spawn(handle_client(
        server_stream,
        Arc::clone(&sessions),
        _global_event_tx,
        provider_template,
        global_is_processing,
        global_session_id,
        client_count,
        Arc::clone(&client_connections),
        swarm_members,
        swarms_by_id,
        shared_context,
        swarm_plans,
        swarm_coordinators,
        file_touch,
        channel_subscriptions,
        channel_subscriptions_by_session,
        client_debug_state,
        _debug_response_tx,
        event_history,
        event_counter,
        swarm_event_tx,
        "jcode-test".to_string(),
        "🧪".to_string(),
        mcp_pool,
        shutdown_signals,
        soft_interrupt_queues,
        AwaitMembersRuntime::default(),
        SwarmMutationRuntime::default(),
    ));
    (client_stream, server_task, sessions)
}

async fn run_scripted_astra_ingress(
    responses: Vec<std::result::Result<String, String>>,
) -> (
    ServerEvent,
    Vec<ServerEvent>,
    Vec<String>,
    SessionAgents,
    usize,
) {
    let complete_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider_template: Arc<dyn Provider> = Arc::new(AstraIngressProbeProvider {
        complete_calls: Arc::clone(&complete_calls),
        fork_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        calls: Arc::clone(&calls),
        model: Arc::new(std::sync::Mutex::new("luna-root".to_string())),
        effort: Arc::new(std::sync::Mutex::new(Some("medium".to_string()))),
        responses: Arc::new(std::sync::Mutex::new(VecDeque::from(responses))),
    });
    let (client_stream, server_task, sessions) =
        start_astra_ingress_probe_client(provider_template).await;
    let (client_reader, mut client_writer) = client_stream.into_split();
    let mut client_reader = BufReader::new(client_reader);
    let working_dir = std::env::current_dir()
        .expect("test working directory")
        .to_string_lossy()
        .into_owned();
    let subscribe = Request::Subscribe {
        id: 1,
        working_dir: Some(working_dir),
        selfdev: None,
        target_session_id: None,
        client_instance_id: None,
        client_has_local_history: false,
        allow_session_takeover: false,
        crash_on_disconnect: false,
        continue_on_disconnect: false,
        terminal_env: Vec::new(),
    };
    client_writer
        .write_all(
            (serde_json::to_string(&subscribe).expect("serialize Subscribe") + "\n").as_bytes(),
        )
        .await
        .expect("write Subscribe");

    let mut line = String::new();
    loop {
        line.clear();
        let event =
            tokio::time::timeout(Duration::from_secs(2), client_reader.read_line(&mut line))
                .await
                .expect("Subscribe response timeout")
                .expect("read Subscribe response");
        assert!(event > 0, "server closed before Subscribe completed");
        if matches!(decode_request_or_event(&line), ServerEvent::Done { id: 1 }) {
            break;
        }
    }

    let message = Request::Message {
        id: 2,
        content: "scripted actual Astra ingress".to_string(),
        images: Vec::new(),
        system_reminder: None,
        active_skill: None,
        no_reply: false,
    };
    client_writer
        .write_all((serde_json::to_string(&message).expect("serialize Message") + "\n").as_bytes())
        .await
        .expect("write Message");

    let mut terminal = None;
    let mut observed_events = Vec::new();
    while terminal.is_none() {
        line.clear();
        let event =
            tokio::time::timeout(Duration::from_secs(2), client_reader.read_line(&mut line))
                .await
                .expect("scripted Astra response timeout")
                .expect("read scripted Astra response");
        assert!(event > 0, "server closed during scripted Astra request");
        let event = decode_request_or_event(&line);
        observed_events.push(event.clone());
        match event {
            ServerEvent::StdinRequest { .. } => {
                panic!("Astra ingress must not request specialized consent")
            }
            event @ (ServerEvent::Done { id: 2 } | ServerEvent::Error { id: 2, .. }) => {
                terminal = Some(event);
            }
            _ => {}
        }
    }
    drop(client_writer);
    server_task
        .await
        .expect("scripted Astra server task join")
        .expect("scripted Astra server task result");

    (
        terminal.expect("scripted Astra path must produce a terminal event"),
        observed_events,
        calls.lock().expect("probe call lock").clone(),
        sessions,
        complete_calls.load(Ordering::SeqCst),
    )
}

async fn root_agent_from_sessions(sessions: &SessionAgents) -> Arc<Mutex<Agent>> {
    let live_sessions = sessions.read().await;
    for agent in live_sessions.values() {
        if agent.lock().await.provider_model() == "luna-root" {
            return Arc::clone(agent);
        }
    }
    panic!("root session must remain live");
}

#[tokio::test]
async fn real_ingress_astra_native_oauth_medium_profile_uses_existing_route_resolution() {
    let _storage = crate::storage::lock_test_env();
    let _env = AstraIngressTestEnv::new_with_effort("openai-oauth:gpt-6-astra", "medium");
    let (terminal, _events, calls, sessions, complete_calls) = run_scripted_astra_ingress(vec![
        Ok(
            r#"{"instruction":"use the ordinary coordinator","final_review":"required","review_reason":""}"#
                .to_string(),
        ),
        Ok("native root draft".to_string()),
        Ok("native final answer".to_string()),
    ])
    .await;

    assert!(matches!(terminal, ServerEvent::Done { id: 2 }));
    assert_eq!(complete_calls, 3);
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls
            .iter()
            .filter(|call| {
                call.contains("Astra admission analyst")
                    || call.contains("same Astra analyst completing")
            })
            .count(),
        2,
        "required review must invoke the analyst for admission and final review only"
    );
    assert!(calls[0].contains("Astra admission analyst"));
    assert!(calls[2].contains("same Astra analyst completing"));

    let mut found_native_profile = false;
    for agent in sessions.read().await.values() {
        let agent = agent.lock().await;
        if agent.session_route_api_method().as_deref() == Some("openai-oauth") {
            found_native_profile = true;
            assert_eq!(agent.provider_reasoning_effort().as_deref(), Some("medium"));
            assert_ne!(
                agent.session_route_api_method().as_deref(),
                Some("chatgpt-web")
            );
        }
    }
    assert!(
        found_native_profile,
        "native OAuth profile must resolve through the existing non-Web route"
    );
}

#[tokio::test]
async fn real_ingress_astra_valid_waiver_releases_captured_root_once() {
    let _storage = crate::storage::lock_test_env();
    let _env = AstraIngressTestEnv::new("openai-api:mock-analyst");
    let (terminal, events, calls, sessions, complete_calls) = run_scripted_astra_ingress(vec![
        Ok(
            r#"{"instruction":"answer from the ordinary coordinator","final_review":"not_required","review_reason":"No open questions or material risk remain."}"#
                .to_string(),
        ),
        Ok("captured root answer".to_string()),
    ])
    .await;

    assert!(matches!(terminal, ServerEvent::Done { id: 2 }));
    assert_eq!(
        complete_calls, 2,
        "a valid waiver must skip the final analyst turn"
    );
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.contains("Astra admission analyst"))
            .count(),
        1,
        "a valid waiver must perform exactly one admission analyst invocation"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.contains("same Astra analyst completing"))
            .count(),
        0,
        "a valid waiver must not invoke a second analyst for final review"
    );
    assert!(calls[0].contains("Astra admission analyst"));
    assert!(calls[1].contains("answer from the ordinary coordinator"));
    assert!(!calls
        .iter()
        .any(|call| call.contains("same Astra analyst completing")));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                ServerEvent::TextDelta { text } if text == "captured root answer"
            ))
            .count(),
        1,
        "the captured root text must be released exactly once"
    );

    let root = root_agent_from_sessions(&sessions).await;
    assert_eq!(
        root.lock().await.visible_conversation_message_count(),
        2,
        "the direct-release path must retain one user and one assistant record"
    );
}

#[tokio::test]
async fn real_ingress_astra_root_error_revokes_waiver_and_keeps_same_analyst_final_path() {
    let _storage = crate::storage::lock_test_env();
    let _env = AstraIngressTestEnv::new("openai-api:mock-analyst");
    let (terminal, events, calls, _sessions, complete_calls) = run_scripted_astra_ingress(vec![
        Ok(
            r#"{"instruction":"answer from the ordinary coordinator","final_review":"not_required","review_reason":"Low-risk direct release."}"#
                .to_string(),
        ),
        Err("root provider failure".to_string()),
        Ok("final diagnosis after root failure".to_string()),
    ])
    .await;

    assert!(matches!(terminal, ServerEvent::Done { id: 2 }));
    assert_eq!(
        complete_calls, 3,
        "root failure must revoke the waiver and run final review"
    );
    assert_eq!(calls.len(), 3);
    assert!(calls[2].contains("same Astra analyst completing"));
    assert!(calls[2].contains("root provider failure"));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                ServerEvent::TextDelta { text } if text == "final diagnosis after root failure"
            ))
            .count(),
        1
    );
    assert!(!events.iter().any(|event| matches!(
        event,
        ServerEvent::TextDelta { text } if text == "root provider failure"
    )));
}

#[tokio::test]
async fn real_ingress_astra_malformed_waiver_falls_back_to_required_review() {
    let _storage = crate::storage::lock_test_env();
    let _env = AstraIngressTestEnv::new("openai-api:mock-analyst");
    let (terminal, _events, calls, _sessions, complete_calls) = run_scripted_astra_ingress(vec![
        Ok(
            r#"{"instruction":"answer","final_review":"not_required","review_reason":""}"#
                .to_string(),
        ),
        Ok("root draft".to_string()),
        Ok("required final".to_string()),
    ])
    .await;

    assert!(matches!(terminal, ServerEvent::Done { id: 2 }));
    assert_eq!(complete_calls, 3);
    assert_eq!(
        calls.len(),
        3,
        "malformed waiver must not skip final review"
    );
    assert!(calls[2].contains("same Astra analyst completing"));
}

#[tokio::test]
async fn real_ingress_astra_final_failure_never_releases_root_draft() {
    let _storage = crate::storage::lock_test_env();
    let _env = AstraIngressTestEnv::new("openai-api:mock-analyst");
    let (terminal, events, calls, sessions, complete_calls) = run_scripted_astra_ingress(vec![
        Ok(
            r#"{"instruction":"ordinary coordinator work","final_review":"required","review_reason":""}"#
                .to_string(),
        ),
        Ok("hidden root draft".to_string()),
        Err("final analyst failure".to_string()),
    ])
    .await;

    match terminal {
        ServerEvent::Error { id: 2, message, .. } => {
            assert!(message.contains("final analyst failure"))
        }
        other => panic!("final failure must remain an error, got {other:?}"),
    }
    assert_eq!(complete_calls, 3);
    assert!(calls[2].contains("same Astra analyst completing"));
    assert!(!events.iter().any(|event| matches!(
        event,
        ServerEvent::TextDelta { text } if text == "hidden root draft"
    )));
    let root = root_agent_from_sessions(&sessions).await;
    assert_eq!(root.lock().await.visible_conversation_message_count(), 2);
}

#[tokio::test]
async fn real_ingress_astra_happy_path_admission_root_final_order() {
    let _storage = crate::storage::lock_test_env();
    let _env = AstraIngressTestEnv::new("openai-api:mock-analyst");
    let complete_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider_template: Arc<dyn Provider> = Arc::new(AstraIngressProbeProvider {
        complete_calls: Arc::clone(&complete_calls),
        fork_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        calls: Arc::clone(&calls),
        model: Arc::new(std::sync::Mutex::new("luna-root".to_string())),
        effort: Arc::new(std::sync::Mutex::new(Some("medium".to_string()))),
        responses: Arc::new(std::sync::Mutex::new(VecDeque::from([
            Ok("first analyst admission".to_string()),
            Ok("first root handoff".to_string()),
            Ok("first analyst final".to_string()),
            Ok("second analyst admission".to_string()),
            Ok("second root handoff".to_string()),
            Ok("second analyst final".to_string()),
        ]))),
    });
    let (client_stream, server_task, sessions) =
        start_astra_ingress_probe_client(provider_template).await;
    let (client_reader, mut client_writer) = client_stream.into_split();
    let mut client_reader = BufReader::new(client_reader);
    let working_dir = std::env::current_dir()
        .expect("test working directory")
        .to_string_lossy()
        .into_owned();
    let subscribe = Request::Subscribe {
        id: 1,
        working_dir: Some(working_dir),
        selfdev: None,
        target_session_id: None,
        client_instance_id: None,
        client_has_local_history: false,
        allow_session_takeover: false,
        crash_on_disconnect: false,
        continue_on_disconnect: false,
        terminal_env: Vec::new(),
    };
    client_writer
        .write_all(
            (serde_json::to_string(&subscribe).expect("serialize Subscribe") + "\n").as_bytes(),
        )
        .await
        .expect("write Subscribe");

    let mut line = String::new();
    loop {
        line.clear();
        let event =
            tokio::time::timeout(Duration::from_secs(2), client_reader.read_line(&mut line))
                .await
                .expect("Subscribe response timeout")
                .expect("read Subscribe response");
        assert!(event > 0, "server closed before Subscribe completed");
        if matches!(decode_request_or_event(&line), ServerEvent::Done { id: 1 }) {
            break;
        }
    }

    let message = Request::Message {
        id: 2,
        content: "happy path admission root final oracle".to_string(),
        images: Vec::new(),
        system_reminder: None,
        active_skill: None,
        no_reply: false,
    };
    client_writer
        .write_all((serde_json::to_string(&message).expect("serialize Message") + "\n").as_bytes())
        .await
        .expect("write Message");

    let mut terminal = None;
    let mut observed_events = Vec::new();
    while terminal.is_none() {
        line.clear();
        let event =
            tokio::time::timeout(Duration::from_secs(2), client_reader.read_line(&mut line))
                .await
                .expect("happy-path response timeout")
                .expect("read happy-path response");
        assert!(event > 0, "server closed during happy-path request");
        let event = decode_request_or_event(&line);
        observed_events.push(format!("{event:?}"));
        match event {
            ServerEvent::StdinRequest { .. } => {
                panic!("Astra ingress must not request specialized consent")
            }
            event @ (ServerEvent::Done { id: 2 } | ServerEvent::Error { id: 2, .. }) => {
                terminal = Some(event);
            }
            _ => {}
        }
    }

    let (analyst_session_id, analyst_agent) = {
        let live_sessions = sessions.read().await;
        let mut found = None;
        for (session_id, agent) in live_sessions.iter() {
            if agent.lock().await.provider_model() == "mock-analyst" {
                found = Some((session_id.clone(), Arc::clone(agent)));
                break;
            }
        }
        found.expect("first message must create one resident analyst")
    };

    let second_message = Request::Message {
        id: 3,
        content: "second happy path admission root final oracle".to_string(),
        images: Vec::new(),
        system_reminder: None,
        active_skill: None,
        no_reply: false,
    };
    client_writer
        .write_all(
            (serde_json::to_string(&second_message).expect("serialize second Message") + "\n")
                .as_bytes(),
        )
        .await
        .expect("write second Message");

    let mut second_terminal = None;
    while second_terminal.is_none() {
        line.clear();
        let event =
            tokio::time::timeout(Duration::from_secs(2), client_reader.read_line(&mut line))
                .await
                .expect("second happy-path response timeout")
                .expect("read second happy-path response");
        assert!(event > 0, "server closed during second happy-path request");
        let event = decode_request_or_event(&line);
        observed_events.push(format!("{event:?}"));
        match event {
            ServerEvent::StdinRequest { .. } => {
                panic!("Astra ingress must not request specialized consent")
            }
            event @ (ServerEvent::Done { id: 3 } | ServerEvent::Error { id: 3, .. }) => {
                second_terminal = Some(event);
            }
            _ => {}
        }
    }

    let current_analyst = sessions
        .read()
        .await
        .get(&analyst_session_id)
        .cloned()
        .expect("resident analyst must remain after second message");
    assert!(
        Arc::ptr_eq(&current_analyst, &analyst_agent),
        "second message must reuse the resident analyst Agent Arc"
    );
    assert!(matches!(
        second_terminal,
        Some(ServerEvent::Done { id: 3 })
    ));

    drop(client_writer);
    server_task
        .await
        .expect("server task join")
        .expect("server task result");

    let recorded_calls = calls.lock().expect("probe call lock").clone();
    let identities = {
        let live_sessions = sessions.read().await;
        let mut identities = Vec::new();
        for (session_id, agent) in live_sessions.iter() {
            let agent = agent.lock().await;
            let session = agent.session_for_split();
            if agent.provider_model() == "mock-analyst" {
                let persisted = crate::session::Session::load(session_id)
                    .expect("analyst session route must remain durable");
                assert_eq!(
                    persisted.route_api_method.as_deref(),
                    Some("openai-api-key"),
                    "persisted analyst session must retain the resolved API route"
                );
            }
            identities.push(format!(
                "session={session_id} model={:?} route={:?} effort={:?} origin={:?} parent={:?}",
                agent.provider_model(),
                agent.session_route_api_method(),
                agent.provider_reasoning_effort(),
                session.origin(),
                session.parent_id,
            ));
        }
        identities
    };

    match terminal.expect("happy path must produce a terminal event") {
        ServerEvent::Done { id: 2 } => {
            assert_eq!(complete_calls.load(Ordering::SeqCst), 6);
            assert_eq!(recorded_calls.len(), 6);
            assert!(recorded_calls[0].contains("Astra admission analyst"));
            assert!(recorded_calls[1].contains("Astra admission handoff"));
            assert!(recorded_calls[2].contains("same Astra analyst completing"));
            assert!(recorded_calls[3].contains("Astra admission analyst"));
            assert!(recorded_calls[4].contains("Astra admission handoff"));
            assert!(recorded_calls[5].contains("same Astra analyst completing"));
            assert!(
                identities.iter().any(|identity| {
                    identity.contains(
                        "model=\"mock-analyst\" route=Some(\"openai-api-key\") effort=Some(\"high\")",
                    )
                }),
                "successful analyst identity must preserve the resolved API route: {identities:?}"
            );
            let public_texts = observed_events
                .iter()
                .filter(|event| event.starts_with("TextDelta {"))
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(
                public_texts,
                vec![
                    r#"TextDelta { text: "first analyst final" }"#.to_string(),
                    r#"TextDelta { text: "second analyst final" }"#.to_string(),
                ],
                "only the ordered final analyst answers may be public: {observed_events:?}"
            );
        }
        ServerEvent::Error { id: 2, message, .. } => panic!(
            "happy-path regression: expected admission -> root -> final success, got error {message:?}; expected analyst identity model=mock-analyst route=Some(\"openai-api-key\") effort=Some(\"high\"); observed identities={identities:?}; provider_calls={recorded_calls:?}; events={observed_events:?}"
        ),
        other => panic!("unexpected happy-path terminal event: {other:?}"),
    }
}

#[tokio::test]
async fn real_ingress_astra_web_route_admission_preserves_host_dispatch_and_metadata() {
    let _storage = crate::storage::lock_test_env();
    let _env = AstraIngressTestEnv::new("gpt-6-astra[web]");
    let complete_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fork_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider_template: Arc<dyn Provider> = Arc::new(AstraIngressProbeProvider {
        complete_calls: Arc::clone(&complete_calls),
        fork_calls: Arc::clone(&fork_calls),
        calls: Arc::clone(&calls),
        model: Arc::new(std::sync::Mutex::new("luna-root".to_string())),
        effort: Arc::new(std::sync::Mutex::new(Some("medium".to_string()))),
        responses: Arc::new(std::sync::Mutex::new(VecDeque::new())),
    });
    let (client_stream, server_task, sessions) =
        start_astra_ingress_probe_client(provider_template).await;
    let (client_reader, mut client_writer) = client_stream.into_split();
    let mut client_reader = BufReader::new(client_reader);
    let working_dir = std::env::current_dir()
        .expect("test working directory")
        .to_string_lossy()
        .into_owned();
    let subscribe = Request::Subscribe {
        id: 1,
        working_dir: Some(working_dir),
        selfdev: None,
        target_session_id: None,
        client_instance_id: None,
        client_has_local_history: false,
        allow_session_takeover: false,
        crash_on_disconnect: false,
        continue_on_disconnect: false,
        terminal_env: Vec::new(),
    };
    client_writer
        .write_all(
            (serde_json::to_string(&subscribe).expect("serialize Subscribe") + "\n").as_bytes(),
        )
        .await
        .expect("write Subscribe");

    let mut line = String::new();
    loop {
        line.clear();
        let event =
            tokio::time::timeout(Duration::from_secs(2), client_reader.read_line(&mut line))
                .await
                .expect("Subscribe response timeout")
                .expect("read Subscribe response");
        assert!(event > 0, "server closed before Subscribe completed");
        if matches!(decode_request_or_event(&line), ServerEvent::Done { id: 1 }) {
            break;
        }
    }

    let message = Request::Message {
        id: 2,
        content: "real Web route admission oracle".to_string(),
        images: Vec::new(),
        system_reminder: None,
        active_skill: None,
        no_reply: false,
    };
    client_writer
        .write_all((serde_json::to_string(&message).expect("serialize Message") + "\n").as_bytes())
        .await
        .expect("write Message");

    let mut terminal = None;
    while terminal.is_none() {
        line.clear();
        let event =
            tokio::time::timeout(Duration::from_secs(2), client_reader.read_line(&mut line))
                .await
                .expect("Web response timeout")
                .expect("read Web response");
        assert!(event > 0, "server closed during Web request");
        match decode_request_or_event(&line) {
            ServerEvent::StdinRequest { .. } => {
                panic!("Astra ingress must not request specialized consent")
            }
            event @ (ServerEvent::Done { id: 2 } | ServerEvent::Error { id: 2, .. }) => {
                terminal = Some(event);
            }
            _ => {}
        }
    }
    drop(client_writer);
    server_task
        .await
        .expect("server task join")
        .expect("server task result");

    let recorded_calls = calls.lock().expect("probe call lock").clone();
    let mut found_web_identity = false;
    {
        let live_sessions = sessions.read().await;
        for (session_id, agent) in live_sessions.iter() {
            let agent = agent.lock().await;
            if agent.session_route_api_method().as_deref() == Some("chatgpt-web") {
                found_web_identity = true;
                assert_eq!(
                    agent.session_route_api_method().as_deref(),
                    Some("chatgpt-web"),
                    "live analyst session must retain the resolved Web route"
                );
                assert_eq!(agent.provider_reasoning_effort().as_deref(), Some("high"));
                let session = agent.session_for_split();
                assert_eq!(session.origin(), crate::session::SessionOrigin::SwarmWorker);
                assert!(
                    session
                        .parent_id
                        .as_deref()
                        .is_some_and(|parent| !parent.is_empty()),
                    "Web analyst must retain a generated requesting-root parent"
                );
                let persisted = crate::session::Session::load(session_id)
                    .expect("Web analyst session route must remain durable");
                assert_eq!(persisted.route_api_method.as_deref(), Some("chatgpt-web"));
            }
        }
    }

    match terminal.expect("Web path must produce a terminal event") {
        ServerEvent::Done { id: 2 } => {
            assert!(
                fork_calls.load(Ordering::SeqCst) > 0,
                "host fixture must fork a provider"
            );
            assert_eq!(complete_calls.load(Ordering::SeqCst), 3);
            assert_eq!(recorded_calls.len(), 3);
            assert!(
                recorded_calls
                    .iter()
                    .any(|call| call.contains("model=openai:gpt-6-astra[web]")),
                "host fixture must dispatch the selected Web model, not only store desired metadata: {recorded_calls:?}"
            );
            assert!(
                found_web_identity,
                "successful Web admission must expose live model and chatgpt-web route"
            );
        }
        ServerEvent::Error { id: 2, message, .. } => {
            panic!(
                "Web route regression expected success, got {message:?}; calls={recorded_calls:?}"
            )
        }
        other => panic!("unexpected Web terminal event: {other:?}"),
    }
}

fn decode_request_or_event(line: &str) -> ServerEvent {
    serde_json::from_str(line.trim()).expect("decode server event")
}

#[test]
fn soft_interrupt_dispatch_starts_idle_session_and_queues_busy_session() {
    assert!(should_start_idle_soft_interrupt(false, false, false));
    assert!(!should_start_idle_soft_interrupt(true, false, false));
    assert!(!should_start_idle_soft_interrupt(false, true, false));
    assert!(!should_start_idle_soft_interrupt(false, false, true));
}
