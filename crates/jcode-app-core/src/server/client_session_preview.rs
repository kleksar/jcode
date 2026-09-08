use super::client_state::{preview_history_messages, session_activity_snapshot};
use super::{ClientConnectionInfo, SessionAgents};
use crate::protocol::{ServerEvent, SessionActivitySnapshot};
use crate::session::Session;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};

pub(super) const MAX_SESSION_PREVIEW_MESSAGES: usize = 20;

/// Serve a small, server-authoritative transcript tail for a target session.
///
/// The target agent is deliberately acquired with `try_lock`: a picker refresh
/// must not queue behind an active turn. When that lock is contended, the only
/// fallback is the server-owned persisted remote-startup snapshot. An absent or
/// unreadable fallback is reported as temporary unavailability, never as an
/// authoritative empty transcript.
pub(super) async fn handle_get_session_preview(
    id: u64,
    target_session_id: &str,
    requested_limit: u16,
    sessions: &SessionAgents,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) -> Result<()> {
    let limit = usize::from(requested_limit).min(MAX_SESSION_PREVIEW_MESSAGES);
    let target_agent = {
        let sessions = sessions.read().await;
        sessions.get(target_session_id).cloned()
    };

    let activity = session_activity_snapshot(client_connections, target_session_id, false)
        .await
        .unwrap_or(SessionActivitySnapshot {
            is_processing: false,
            current_tool_name: None,
        });

    let (messages, revision) = match target_agent {
        Some(agent) => match agent.try_lock() {
            Ok(agent) => {
                let session = agent.session_for_split();
                (
                    preview_history_messages(session, limit),
                    preview_revision(session),
                )
            }
            Err(_) => match load_persisted_preview_session(target_session_id.to_string()).await {
                Ok(session) => {
                    // A zero-message disk snapshot cannot distinguish a
                    // genuinely empty live session from a stale/partial
                    // startup artifact while its Agent is busy. Only a live
                    // lock may authoritatively establish empty.
                    if session.messages.is_empty() {
                        send_error(
                            id,
                            "Session preview is temporarily unavailable while the session is busy",
                            Some(1),
                            client_event_tx,
                        );
                        return Ok(());
                    }
                    let revision = preview_revision(&session);
                    (preview_history_messages(&session, limit), revision)
                }
                Err(err) => {
                    send_error(
                        id,
                        "Session preview is temporarily unavailable while the session is busy",
                        Some(1),
                        client_event_tx,
                    );
                    crate::logging::warn(&format!(
                        "session preview fallback unavailable for busy session {}: {err}",
                        target_session_id
                    ));
                    return Ok(());
                }
            },
        },
        None => {
            // A non-live persisted session is not an Active Sessions target. Do
            // not read arbitrary disk state and accidentally advertise it as a
            // live authoritative preview.
            send_error(id, "Unknown session", None, client_event_tx);
            return Ok(());
        }
    };

    let _ = client_event_tx.send(ServerEvent::SessionPreview {
        id,
        session_id: target_session_id.to_string(),
        revision,
        messages,
        activity,
    });
    Ok(())
}

fn preview_revision(session: &Session) -> u64 {
    let timestamp = session.updated_at.timestamp_millis().max(0) as u64;
    timestamp
        .wrapping_mul(31)
        .wrapping_add(session.messages.len() as u64)
}

async fn load_persisted_preview_session(session_id: String) -> Result<Session> {
    // Session loading is synchronous filesystem I/O. Keep it off the request
    // reactor so a slow disk cannot prevent other clients from being serviced.
    tokio::task::spawn_blocking(move || {
        Session::load_for_remote_startup(&session_id)
            .or_else(|_| Session::load_startup_stub(&session_id))
    })
    .await?
    .map_err(Into::into)
}

fn send_error(
    id: u64,
    message: &str,
    retry_after_secs: Option<u64>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let _ = client_event_tx.send(ServerEvent::Error {
        id,
        message: message.to_string(),
        retry_after_secs,
    });
}

#[cfg(test)]
#[path = "client_session_preview_tests.rs"]
mod tests;
