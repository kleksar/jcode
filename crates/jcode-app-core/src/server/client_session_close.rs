use super::{ClientConnectionInfo, SessionAgents, SwarmMember, remove_session_entry};
use crate::protocol::ServerEvent;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};

fn refuse(id: u64, message: impl Into<String>, event_tx: &mpsc::UnboundedSender<ServerEvent>) {
    let _ = event_tx.send(ServerEvent::Error {
        id,
        message: message.into(),
        retry_after_secs: None,
    });
}

/// Close an idle session as an explicit, server-authoritative lifecycle action.
/// Transport loss never reaches this path and no cancellation signal is set here.
pub(super) async fn handle_close_session(
    id: u64,
    target_session_id: &str,
    requester_session_id: &str,
    sessions: &SessionAgents,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) -> Result<()> {
    if target_session_id == requester_session_id {
        refuse(id, "Cannot close the current session", client_event_tx);
        return Ok(());
    }

    // Attachment transitions use the connection registry as their authority.
    // Keep the same connections -> sessions lock order as live resume so no new
    // attachment can commit between our snapshot and session removal.
    let connections = client_connections.write().await;
    let target_agent = sessions.read().await.get(target_session_id).cloned();
    let Some(target_agent) = target_agent else {
        let Ok(mut persisted) = crate::session::Session::load(target_session_id) else {
            refuse(id, "Unknown session", client_event_tx);
            return Ok(());
        };
        if !matches!(persisted.status, crate::session::SessionStatus::Closed) {
            persisted.mark_closed();
            if let Err(error) = persisted.save() {
                refuse(
                    id,
                    format!("Failed to close session: {error}"),
                    client_event_tx,
                );
                return Ok(());
            }
        }
        let _ = client_event_tx.send(ServerEvent::SessionClosed {
            id,
            session_id: target_session_id.to_string(),
        });
        return Ok(());
    };

    let target_connections: Vec<_> = connections
        .values()
        .filter(|connection| connection.session_id == target_session_id)
        .cloned()
        .collect();
    if target_connections
        .iter()
        .any(|connection| connection.is_processing)
    {
        refuse(id, "Session is working", client_event_tx);
        return Ok(());
    }

    let Some(mut agent) = target_agent.try_lock().ok() else {
        refuse(id, "Session is working", client_event_tx);
        return Ok(());
    };
    agent.mark_closed();
    let mut persisted = agent.session_for_split().clone();
    persisted.mark_closed();
    if let Err(error) = persisted.save() {
        refuse(
            id,
            format!("Failed to close session: {error}"),
            client_event_tx,
        );
        return Ok(());
    }
    drop(agent);

    remove_session_entry(sessions, target_session_id).await;
    swarm_members.write().await.remove(target_session_id);
    for connection in target_connections {
        let _ = connection.disconnect_tx.send(());
    }
    drop(connections);
    let _ = client_event_tx.send(ServerEvent::SessionClosed {
        id,
        session_id: target_session_id.to_string(),
    });
    Ok(())
}
