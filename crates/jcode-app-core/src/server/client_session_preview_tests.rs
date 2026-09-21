use super::{MAX_SESSION_PREVIEW_MESSAGES, handle_get_session_preview};
use crate::agent::Agent;
use crate::message::{ContentBlock, Message, ToolDefinition};
use crate::protocol::ServerEvent;
use crate::provider::{EventStream, Provider};
use crate::server::{ClientConnectionInfo, RuntimeFastState, SessionAgentEntry, SessionAgents};
use crate::tool::Registry;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock, mpsc};

struct PreviewProvider;

#[async_trait]
impl Provider for PreviewProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        unreachable!("preview tests do not call the provider")
    }

    fn name(&self) -> &str {
        "preview-test"
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
    fn model(&self) -> String {
        "preview-test".to_string()
    }
}

fn stored_text(id: usize, text: impl Into<String>) -> crate::session::StoredMessage {
    crate::session::StoredMessage {
        id: format!("message-{id}"),
        role: crate::message::Role::User,
        content: vec![ContentBlock::Text {
            text: text.into(),
            cache_control: None,
        }],
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
    }
}

fn live_agent(session: crate::session::Session) -> Arc<Mutex<Agent>> {
    Arc::new(Mutex::new(Agent::new_with_session(
        Arc::new(PreviewProvider),
        Registry::empty(),
        session,
        None,
    )))
}

fn empty_connections() -> Arc<RwLock<HashMap<String, ClientConnectionInfo>>> {
    Arc::new(RwLock::new(HashMap::new()))
}

async fn request_preview(
    id: u64,
    session_id: &str,
    limit: u16,
    sessions: &SessionAgents,
    connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
) -> ServerEvent {
    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_get_session_preview(id, session_id, limit, sessions, connections, &tx)
        .await
        .expect("handler succeeds");
    rx.recv().await.expect("preview event")
}

#[tokio::test]
async fn session_preview_bounds_the_rendered_tail_and_wire_payload() {
    let session_id = "preview-bounded";
    let mut session = crate::session::Session::create_with_id(
        session_id.to_string(),
        None,
        Some("preview".to_string()),
    );
    for index in 0..25 {
        session.append_stored_message(stored_text(index, format!("message-{index}")));
    }
    let agent = live_agent(session);
    let sessions = Arc::new(RwLock::new(HashMap::<String, crate::server::SessionAgentEntry>::from([(
        session_id.to_string(),
        crate::server::SessionAgentEntry::new(
            agent,
            RuntimeFastState::invalid(session_id.to_string()),
        ),
    )])));
    let event = request_preview(7, session_id, u16::MAX, &sessions, &empty_connections()).await;

    let ServerEvent::SessionPreview {
        id,
        messages,
        activity,
        ..
    } = event
    else {
        panic!("expected SessionPreview");
    };
    assert_eq!(id, 7);
    assert_eq!(messages.len(), MAX_SESSION_PREVIEW_MESSAGES);
    assert_eq!(
        messages.first().map(|message| message.content.as_str()),
        Some("message-5")
    );
    assert_eq!(
        messages.last().map(|message| message.content.as_str()),
        Some("message-24")
    );
    assert!(!activity.is_processing);
    let wire = crate::protocol::encode_event(&ServerEvent::SessionPreview {
        id,
        session_id: session_id.to_string(),
        revision: 1,
        messages,
        activity,
    });
    assert!(
        wire.len() < 8_192,
        "bounded preview wire payload was {} bytes",
        wire.len()
    );
}

#[tokio::test]
async fn session_preview_omits_image_data_but_keeps_an_attachment_label() {
    let session_id = "preview-image";
    let mut session = crate::session::Session::create_with_id(
        session_id.to_string(),
        None,
        Some("preview".to_string()),
    );
    session.append_stored_message(crate::session::StoredMessage {
        id: "image".to_string(),
        role: crate::message::Role::User,
        content: vec![
            ContentBlock::Text {
                text: "look here".to_string(),
                cache_control: None,
            },
            ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "data:image/png;base64,secret-image-data".to_string(),
            },
        ],
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
    });
    let sessions = Arc::new(RwLock::new(HashMap::<String, crate::server::SessionAgentEntry>::from([(
        session_id.to_string(),
        {
            let agent = live_agent(session);
            crate::server::SessionAgentEntry::new(
                agent,
                RuntimeFastState::invalid(session_id.to_string()),
            )
        },
    )])));
    let event = request_preview(8, session_id, 20, &sessions, &empty_connections()).await;
    let wire = crate::protocol::encode_event(&event);
    assert!(wire.contains("Image attachment omitted"));
    assert!(!wire.contains("secret-image-data"));
    assert!(!wire.contains("data:image"));
}

#[tokio::test]
#[expect(
    clippy::await_holding_lock,
    reason = "the regression deliberately holds the target Agent mutex"
)]
async fn busy_session_without_persisted_snapshot_returns_temporary_error_without_waiting() {
    let _env_lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("temporary JCODE_HOME");
    let old_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", home.path());

    let session_id = "preview-busy-no-snapshot";
    let session = crate::session::Session::create_with_id(
        session_id.to_string(),
        None,
        Some("preview".to_string()),
    );
    let agent = live_agent(session);
    std::fs::remove_file(crate::session::session_path(session_id).expect("session path"))
        .expect("remove automatic session snapshot");
    let journal = crate::session::session_journal_path(session_id).expect("journal path");
    if journal.exists() {
        std::fs::remove_file(journal).expect("remove automatic session journal");
    }
    let sessions = Arc::new(RwLock::new(HashMap::<String, crate::server::SessionAgentEntry>::from([(
        session_id.to_string(),
        crate::server::SessionAgentEntry::new(
            Arc::clone(&agent),
            RuntimeFastState::invalid(session_id.to_string()),
        ),
    )])));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let held_lock = agent.lock().await;
    let result = tokio::time::timeout(
        Duration::from_millis(100),
        handle_get_session_preview(9, session_id, 20, &sessions, &empty_connections(), &tx),
    )
    .await
    .expect("busy preview must not await the Agent lock");
    result.expect("handler returns a correlated error event");
    drop(held_lock);

    match rx.recv().await.expect("error event") {
        ServerEvent::Error {
            id,
            message,
            retry_after_secs,
        } => {
            assert_eq!(id, 9);
            assert!(message.contains("temporarily unavailable"));
            assert_eq!(retry_after_secs, Some(1));
        }
        other => panic!("expected temporary preview error, got {other:?}"),
    }

    match old_home {
        Some(value) => crate::env::set_var("JCODE_HOME", value),
        None => crate::env::remove_var("JCODE_HOME"),
    }
}
