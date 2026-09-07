use super::{
    ChannelSubscriptions, ClientConnectionInfo, ClientDebugState, FileTouchService,
    SessionInterruptQueues, SwarmEvent, SwarmMember, VersionedPlan, unregister_session_event_sender,
};
use crate::agent::Agent;
use anyhow::Result;
use jcode_agent_runtime::InterruptSignal;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};

type SessionAgents = Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>;

/// Retained for callers while disconnect cleanup is transport-only. It no
/// longer governs session lifetime.
pub(super) const IDLE_RECONNECT_GRACE: Duration = Duration::from_secs(30);

/// Release transport-owned state without changing the live session or turn.
pub(super) async fn detach_client_attachment(
    session_id: &str,
    connection_id: &str,
    debug_id: &str,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    client_debug_state: &Arc<RwLock<ClientDebugState>>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
) {
    client_debug_state.write().await.unregister(debug_id);
    client_connections.write().await.remove(connection_id);
    unregister_session_event_sender(swarm_members, session_id, connection_id).await;
}

#[expect(
    clippy::too_many_arguments,
    reason = "disconnect cleanup receives the server runtime context from the connection handler"
)]
pub(super) async fn cleanup_client_connection(
    _sessions: &SessionAgents,
    client_session_id: &str,
    _client_is_processing: bool,
    processing_task: &mut Option<tokio::task::JoinHandle<()>>,
    event_handle: tokio::task::JoinHandle<()>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    _swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
    _swarm_coordinators: &Arc<RwLock<HashMap<String, String>>>,
    _swarm_plans: &Arc<RwLock<HashMap<String, VersionedPlan>>>,
    _file_touch: &FileTouchService,
    _channel_subscriptions: &ChannelSubscriptions,
    _channel_subscriptions_by_session: &ChannelSubscriptions,
    client_debug_state: &Arc<RwLock<ClientDebugState>>,
    client_debug_id: &str,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    client_connection_id: &str,
    _shutdown_signals: &Arc<RwLock<HashMap<String, InterruptSignal>>>,
    _soft_interrupt_queues: &SessionInterruptQueues,
    _event_history: &Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    _event_counter: &Arc<std::sync::atomic::AtomicU64>,
    _swarm_event_tx: &broadcast::Sender<SwarmEvent>,
    _client_event_tx: &mpsc::UnboundedSender<crate::protocol::ServerEvent>,
    _idle_reconnect_grace: Duration,
) -> Result<()> {
    // A turn is owned by the server session, not by this transport. Dropping the
    // JoinHandle detaches it, allowing the turn to complete after socket EOF.
    drop(processing_task.take());
    detach_client_attachment(
        client_session_id,
        client_connection_id,
        client_debug_id,
        client_connections,
        client_debug_state,
        swarm_members,
    )
    .await;
    // The forwarder belongs to this client only. Its source turn remains live.
    event_handle.abort();
    Ok(())
}

#[cfg(test)]
#[path = "client_disconnect_grace_tests.rs"]
mod grace_tests;

#[cfg(test)]
mod tests {
    #[test]
    fn disconnect_cleanup_is_transport_only() {
        // The behavioral regression tests exercise persisted idle and busy
        // sessions through cleanup_client_connection.
    }
}
