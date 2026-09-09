use super::*;
use crate::protocol::ServerEvent;
use crate::provider::{EventStream, Provider};
use crate::session::{Session, SessionStatus};
use crate::tool::Registry;
use async_trait::async_trait;

struct NoRequests;

#[async_trait]
impl Provider for NoRequests {
    async fn complete(
        &self,
        _: &[crate::message::Message],
        _: &[crate::message::ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        anyhow::bail!("close tests must not request a provider")
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

struct Home {
    _dir: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", dir.path());
        Self {
            _dir: dir,
            previous,
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => crate::env::set_var("JCODE_HOME", value),
            None => crate::env::remove_var("JCODE_HOME"),
        }
    }
}

async fn live_session() -> (String, Arc<Mutex<Agent>>, SessionAgents) {
    let provider: Arc<dyn Provider> = Arc::new(NoRequests);
    let registry = Registry::new(provider.clone()).await;
    let mut session = Session::create(None, None);
    session.title = Some("Close but retain transcript metadata".to_string());
    session.save().unwrap();
    let id = session.id.clone();
    let agent = Arc::new(Mutex::new(Agent::new_with_session(
        provider, registry, session, None,
    )));
    let sessions = Arc::new(RwLock::new(HashMap::from([(id.clone(), agent.clone())])));
    (id, agent, sessions)
}

fn server_state() -> (
    Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    Arc<RwLock<HashMap<String, SwarmMember>>>,
    mpsc::UnboundedSender<ServerEvent>,
    mpsc::UnboundedReceiver<ServerEvent>,
) {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    (
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(HashMap::new())),
        event_tx,
        event_rx,
    )
}

#[tokio::test]
async fn close_idle_persisted_session_marks_closed_retains_metadata_and_emits_event() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let (id, _agent, sessions) = live_session().await;
    let (connections, members, event_tx, mut event_rx) = server_state();

    super::client_session_close::handle_close_session(
        7,
        &id,
        "requester",
        &sessions,
        &connections,
        &members,
        &event_tx,
    )
    .await
    .unwrap();

    assert!(sessions.read().await.get(&id).is_none());
    let persisted = Session::load(&id).expect("closed session remains resumable from storage");
    assert_eq!(persisted.status, SessionStatus::Closed);
    assert_eq!(
        persisted.title.as_deref(),
        Some("Close but retain transcript metadata")
    );
    assert!(matches!(
        event_rx.recv().await,
        Some(ServerEvent::SessionClosed { id: 7, session_id }) if session_id == id
    ));
}

#[tokio::test]
async fn close_busy_session_is_refused_without_cancellation_or_state_change() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let (id, agent, sessions) = live_session().await;
    let (connections, members, event_tx, mut event_rx) = server_state();
    let _busy = agent.lock().await;

    super::client_session_close::handle_close_session(
        8,
        &id,
        "requester",
        &sessions,
        &connections,
        &members,
        &event_tx,
    )
    .await
    .unwrap();

    assert!(sessions.read().await.contains_key(&id));
    assert!(matches!(
        event_rx.recv().await,
        Some(ServerEvent::Error { id: 8, message, .. }) if message == "Session is working"
    ));
}

#[tokio::test]
async fn close_current_session_is_refused() {
    let (connections, members, event_tx, mut event_rx) = server_state();
    let sessions = Arc::new(RwLock::new(HashMap::new()));

    super::client_session_close::handle_close_session(
        9,
        "current",
        "current",
        &sessions,
        &connections,
        &members,
        &event_tx,
    )
    .await
    .unwrap();

    assert!(matches!(
        event_rx.recv().await,
        Some(ServerEvent::Error { id: 9, message, .. }) if message == "Cannot close the current session"
    ));
}

#[tokio::test]
async fn close_already_closed_session_is_idempotent() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let mut session = Session::create(None, None);
    session.title = Some("Already closed session".to_string());
    session.mark_closed();
    session.save().unwrap();
    let (connections, members, event_tx, mut event_rx) = server_state();
    let sessions = Arc::new(RwLock::new(HashMap::new()));

    super::client_session_close::handle_close_session(
        10,
        &session.id,
        "requester",
        &sessions,
        &connections,
        &members,
        &event_tx,
    )
    .await
    .unwrap();

    assert!(matches!(
        event_rx.recv().await,
        Some(ServerEvent::SessionClosed { id: 10, session_id }) if session_id == session.id
    ));
}

#[tokio::test]
async fn close_unknown_session_is_refused() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let (connections, members, event_tx, mut event_rx) = server_state();
    let sessions = Arc::new(RwLock::new(HashMap::new()));

    super::client_session_close::handle_close_session(
        11,
        "unknown",
        "requester",
        &sessions,
        &connections,
        &members,
        &event_tx,
    )
    .await
    .unwrap();

    assert!(matches!(
        event_rx.recv().await,
        Some(ServerEvent::Error { id: 11, message, .. }) if message == "Unknown session"
    ));
}

#[tokio::test]
async fn close_attached_idle_session_disconnects_its_client() {
    let _lock = crate::storage::lock_test_env();
    let _home = Home::new();
    let (id, _agent, sessions) = live_session().await;
    let (connections, members, event_tx, mut event_rx) = server_state();
    let (disconnect_tx, mut disconnect_rx) = mpsc::unbounded_channel();
    connections.write().await.insert(
        "attached-target".to_string(),
        ClientConnectionInfo {
            client_id: "attached-target".to_string(),
            session_id: id.clone(),
            client_instance_id: None,
            debug_client_id: None,
            connected_at: std::time::Instant::now(),
            last_seen: std::time::Instant::now(),
            is_processing: false,
            current_tool_name: None,
            terminal_env: Vec::new(),
            disconnect_tx,
        },
    );

    super::client_session_close::handle_close_session(
        12,
        &id,
        "requester",
        &sessions,
        &connections,
        &members,
        &event_tx,
    )
    .await
    .unwrap();

    assert!(disconnect_rx.recv().await.is_some());
    assert!(matches!(
        event_rx.recv().await,
        Some(ServerEvent::SessionClosed { id: 12, session_id }) if session_id == id
    ));
}
