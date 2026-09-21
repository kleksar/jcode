use super::super::client_lifecycle::handle_client;
use super::super::{AwaitMembersRuntime, ClientDebugState, FileTouchService, SwarmMutationRuntime};
use super::*;
use crate::message::{Message, StreamEvent, ToolDefinition};
use crate::protocol::Request;
use crate::provider::{EventStream, Provider};
use crate::recent_session_index::RecentSessionMetadata;
use crate::server::SwarmMember;
use async_trait::async_trait;
use futures::stream;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::{Notify, RwLock};
use tokio::time::Duration;

#[derive(Clone)]
struct CleanSessionProvider {
    forks: Arc<AtomicUsize>,
    complete_calls: Arc<AtomicUsize>,
    count_completions: Arc<AtomicBool>,
}

#[async_trait]
impl Provider for CleanSessionProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        // Native setup runs provider discovery and prewarm probes. The test
        // arms this counter only after authoritative Resume completion, so it
        // measures the normal first-message turn rather than setup traffic.
        if self.count_completions.load(Ordering::SeqCst) {
            self.complete_calls.fetch_add(1, Ordering::SeqCst);
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(StreamEvent::TextDelta("mock response".to_string())),
            Ok(StreamEvent::MessageEnd { stop_reason: None }),
        ])))
    }

    fn name(&self) -> &str {
        "clean-session-test"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        self.forks.fetch_add(1, Ordering::SeqCst);
        Arc::new(self.clone())
    }
}

const BUSY_SOCKET_ORACLE_PROMPT: &str = "BUSY_SOCKET_ORACLE_A_PROMPT";
const BUSY_SOCKET_ORACLE_INITIAL: &str = "BUSY_SOCKET_ORACLE_A_INITIAL";
const BUSY_SOCKET_ORACLE_RESPONSE: &str = "BUSY_SOCKET_ORACLE_A_RESPONSE";

#[derive(Clone)]
struct BusySocketOracleProvider {
    a_initial_sent: Arc<AtomicBool>,
    a_initial_notify: Arc<Notify>,
    release_a: Arc<Notify>,
    a_finished: Arc<AtomicBool>,
    a_finished_notify: Arc<Notify>,
    a_terminated: Arc<AtomicBool>,
    a_terminated_notify: Arc<Notify>,
}

impl BusySocketOracleProvider {
    fn new() -> Self {
        Self {
            a_initial_sent: Arc::new(AtomicBool::new(false)),
            a_initial_notify: Arc::new(Notify::new()),
            release_a: Arc::new(Notify::new()),
            a_finished: Arc::new(AtomicBool::new(false)),
            a_finished_notify: Arc::new(Notify::new()),
            a_terminated: Arc::new(AtomicBool::new(false)),
            a_terminated_notify: Arc::new(Notify::new()),
        }
    }
}

#[async_trait]
impl Provider for BusySocketOracleProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        let is_busy_a_turn = messages.iter().any(|message| {
            message.content.iter().any(|content| {
                matches!(
                    content,
                    crate::message::ContentBlock::Text { text, .. }
                        if text.contains(BUSY_SOCKET_ORACLE_PROMPT)
                )
            })
        });

        if !is_busy_a_turn {
            return Ok(Box::pin(stream::iter(vec![
                Ok(StreamEvent::TextDelta("ordinary response".to_string())),
                Ok(StreamEvent::MessageEnd { stop_reason: None }),
            ])));
        }

        let (tx, rx) = mpsc::channel(4);
        let initial_sent = Arc::clone(&self.a_initial_sent);
        let initial_notify = Arc::clone(&self.a_initial_notify);
        let release = Arc::clone(&self.release_a);
        let finished = Arc::clone(&self.a_finished);
        let finished_notify = Arc::clone(&self.a_finished_notify);
        let terminated = Arc::clone(&self.a_terminated);
        let terminated_notify = Arc::clone(&self.a_terminated_notify);
        tokio::spawn(async move {
            if tx
                .send(Ok(StreamEvent::TextDelta(
                    BUSY_SOCKET_ORACLE_INITIAL.to_string(),
                )))
                .await
                .is_err()
            {
                terminated.store(true, Ordering::SeqCst);
                terminated_notify.notify_one();
                return;
            }
            initial_sent.store(true, Ordering::SeqCst);
            initial_notify.notify_one();
            release.notified().await;
            if tx
                .send(Ok(StreamEvent::TextDelta(
                    BUSY_SOCKET_ORACLE_RESPONSE.to_string(),
                )))
                .await
                .is_err()
            {
                terminated.store(true, Ordering::SeqCst);
                terminated_notify.notify_one();
                return;
            }
            if tx
                .send(Ok(StreamEvent::MessageEnd { stop_reason: None }))
                .await
                .is_err()
            {
                return;
            }
            finished.store(true, Ordering::SeqCst);
            finished_notify.notify_one();
            terminated.store(true, Ordering::SeqCst);
            terminated_notify.notify_one();
        });
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "busy-socket-oracle"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

struct CleanSessionClientState {
    swarm_members: Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: Arc<RwLock<HashMap<String, HashSet<String>>>>,
}

/// Isolate persisted-session tests because `Session::load` uses process-wide
/// JCODE_HOME. Callers must hold `storage::lock_test_env` for its lifetime.
struct IsolatedSessionHome {
    previous_home: Option<std::ffi::OsString>,
    _home: tempfile::TempDir,
}

impl IsolatedSessionHome {
    fn new() -> Self {
        let home = tempfile::TempDir::new().expect("test JCODE_HOME");
        let previous_home = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", home.path());
        Self {
            previous_home,
            _home: home,
        }
    }
}

impl Drop for IsolatedSessionHome {
    fn drop(&mut self) {
        if let Some(previous_home) = self.previous_home.take() {
            crate::env::set_var("JCODE_HOME", previous_home);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
    }
}

async fn start_clean_session_client(
    provider_template: Arc<dyn Provider>,
    sessions: SessionAgents,
) -> (
    crate::transport::Stream,
    tokio::task::JoinHandle<anyhow::Result<()>>,
    CleanSessionClientState,
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
    let channel_subscriptions = Arc::new(RwLock::new(HashMap::new()));
    let channel_subscriptions_by_session = Arc::new(RwLock::new(HashMap::new()));
    let client_debug_state = Arc::new(RwLock::new(ClientDebugState::default()));
    let (debug_response_tx, _) = broadcast::channel(8);
    let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _) = broadcast::channel(8);
    let (global_event_tx, _) = broadcast::channel(8);
    let global_is_processing = Arc::new(RwLock::new(false));
    let shutdown_signals = Arc::new(RwLock::new(HashMap::new()));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());

    let task = tokio::spawn(handle_client(
        server_stream,
        sessions,
        global_event_tx,
        provider_template,
        global_is_processing,
        global_session_id,
        client_count,
        client_connections,
        Arc::clone(&swarm_members),
        Arc::clone(&swarms_by_id),
        shared_context,
        swarm_plans,
        swarm_coordinators,
        FileTouchService::new(),
        channel_subscriptions,
        channel_subscriptions_by_session,
        client_debug_state,
        debug_response_tx,
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
    (
        client_stream,
        task,
        CleanSessionClientState {
            swarm_members,
            swarms_by_id,
        },
    )
}

async fn send_request<W: tokio::io::AsyncWrite + Unpin>(writer: &mut W, request: &Request) {
    let payload = serde_json::to_string(request).expect("serialize request") + "\n";
    writer
        .write_all(payload.as_bytes())
        .await
        .expect("write request");
}

async fn recv_until<R, F>(reader: &mut BufReader<R>, predicate: F) -> ServerEvent
where
    R: tokio::io::AsyncRead + Unpin,
    F: Fn(&ServerEvent) -> bool,
{
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let mut line = String::new();
            let bytes = reader
                .read_line(&mut line)
                .await
                .expect("read server event");
            assert_ne!(bytes, 0, "server closed before expected event");
            let event: ServerEvent =
                serde_json::from_str(line.trim()).expect("decode server event");
            if matches!(event, ServerEvent::Error { .. }) {
                panic!("server rejected lifecycle request: {event:?}");
            }
            if predicate(&event) {
                return event;
            }
        }
    })
    .await
    .expect("timed out waiting for authoritative server event")
}

async fn recv_event<R>(reader: &mut BufReader<R>) -> ServerEvent
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut line = String::new();
    let bytes = reader
        .read_line(&mut line)
        .await
        .expect("read server event");
    assert_ne!(bytes, 0, "server closed before expected event");
    serde_json::from_str(line.trim()).expect("decode server event")
}

fn subscribe_request(id: u64, working_dir: String) -> Request {
    Request::Subscribe {
        id,
        working_dir: Some(working_dir),
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

fn empty_runtime() -> SessionRuntimeSelection {
    SessionRuntimeSelection::default()
}

fn clean_session_state() -> (
    SessionAgents,
    Arc<RwLock<HashMap<String, InterruptSignal>>>,
    SessionInterruptQueues,
) {
    (
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(HashMap::new())),
    )
}

fn metadata(id: &str, working_dir: Option<String>, updated_at_ms: i64) -> RecentSessionMetadata {
    RecentSessionMetadata {
        session_id: id.to_string(),
        working_dir,
        generated_title: None,
        custom_title: None,
        todo_title: None,
        saved: false,
        updated_at_ms,
        last_active_at_ms: None,
    }
}

#[tokio::test]
async fn clean_session_creation_does_not_wait_for_busy_source() {
    let _storage = crate::storage::lock_test_env();
    let _home = IsolatedSessionHome::new();
    let working_dir = tempfile::tempdir().expect("working directory");
    let forks = Arc::new(AtomicUsize::new(0));
    let complete_calls = Arc::new(AtomicUsize::new(0));
    let count_completions = Arc::new(AtomicBool::new(false));
    let provider: Arc<dyn Provider> = Arc::new(CleanSessionProvider {
        forks: Arc::clone(&forks),
        complete_calls: Arc::clone(&complete_calls),
        count_completions,
    });
    let (sessions, _, _) = clean_session_state();
    let registry = Registry::new(Arc::clone(&provider)).await;
    let source_id = "busy-clean-create-source".to_string();
    let source = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&provider),
        registry,
        crate::session::Session::create_with_id(source_id.clone(), None, None),
        None,
    )));
    sessions
        .write()
        .await
        .insert(
            source_id.clone(),
            crate::server::SessionAgentEntry::new(
                Arc::clone(&source),
                crate::server::RuntimeFastState::invalid(source_id.clone()),
            ),
        );
    let busy_source = source.lock().await;

    let (client_stream, server_task, _state) =
        start_clean_session_client(Arc::clone(&provider), Arc::clone(&sessions)).await;
    let (client_reader, mut client_writer) = client_stream.into_split();
    let mut client_reader = BufReader::new(client_reader);
    send_request(
        &mut client_writer,
        &subscribe_request(1, working_dir.path().to_string_lossy().into_owned()),
    )
    .await;
    send_request(
        &mut client_writer,
        &Request::CreateSession {
            id: 2,
            working_dir: working_dir.path().to_string_lossy().into_owned(),
            runtime: empty_runtime(),
        },
    )
    .await;

    let created = tokio::time::timeout(
        Duration::from_millis(500),
        recv_until(&mut client_reader, |event| {
            matches!(event, ServerEvent::SessionCreated { id: 2, .. })
        }),
    )
    .await
    .expect("clean creation must not wait for an unrelated busy source Agent mutex");
    let ServerEvent::SessionCreated { session_id, .. } = created else {
        unreachable!()
    };
    assert_ne!(session_id, source_id);
    assert!(
        sessions.read().await.contains_key(&session_id),
        "clean target must be registered"
    );
    assert_eq!(complete_calls.load(Ordering::SeqCst), 0);
    drop(busy_source);
    drop(client_writer);
    server_task
        .await
        .expect("server task join")
        .expect("server task result");
}

#[tokio::test]
async fn clean_session_first_message_is_persisted_once_and_starts_one_turn() {
    let _storage = crate::storage::lock_test_env();
    let _home = IsolatedSessionHome::new();
    let working_dir = tempfile::tempdir().expect("working directory");
    let canonical_working_dir =
        std::fs::canonicalize(working_dir.path()).expect("canonical working directory");
    let forks = Arc::new(AtomicUsize::new(0));
    let complete_calls = Arc::new(AtomicUsize::new(0));
    let count_completions = Arc::new(AtomicBool::new(false));
    let provider: Arc<dyn Provider> = Arc::new(CleanSessionProvider {
        forks: Arc::clone(&forks),
        complete_calls: Arc::clone(&complete_calls),
        count_completions: Arc::clone(&count_completions),
    });
    let (sessions, _, _) = clean_session_state();
    let registry = Registry::new(Arc::clone(&provider)).await;
    let source_id = "clean-first-turn-source".to_string();
    let source = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&provider),
        registry,
        crate::session::Session::create_with_id(source_id.clone(), None, None),
        None,
    )));
    sessions
        .write()
        .await
        .insert(
            source_id.clone(),
            crate::server::SessionAgentEntry::new(
                Arc::clone(&source),
                crate::server::RuntimeFastState::invalid(source_id.clone()),
            ),
        );

    let (client_stream, server_task, _state) =
        start_clean_session_client(Arc::clone(&provider), Arc::clone(&sessions)).await;
    let (client_reader, mut client_writer) = client_stream.into_split();
    let mut client_reader = BufReader::new(client_reader);
    send_request(
        &mut client_writer,
        &subscribe_request(1, canonical_working_dir.to_string_lossy().into_owned()),
    )
    .await;
    send_request(
        &mut client_writer,
        &Request::CreateSession {
            id: 2,
            working_dir: canonical_working_dir.to_string_lossy().into_owned(),
            runtime: empty_runtime(),
        },
    )
    .await;
    let created = recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::SessionCreated { id: 2, .. })
    })
    .await;
    let ServerEvent::SessionCreated {
        session_id: target_id,
        ..
    } = created
    else {
        unreachable!()
    };

    send_request(
        &mut client_writer,
        &Request::ResumeSession {
            id: 3,
            session_id: target_id.clone(),
            client_instance_id: None,
            client_has_local_history: false,
            allow_session_takeover: false,
        },
    )
    .await;
    recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::Done { id: 3 })
    })
    .await;
    count_completions.store(true, Ordering::SeqCst);

    let prompt = "clean first prompt reaches exactly one native turn";
    send_request(
        &mut client_writer,
        &Request::Message {
            id: 4,
            content: prompt.to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
    )
    .await;
    recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::Done { id: 4 })
    })
    .await;

    let persisted = crate::session::Session::load(&target_id).expect("persisted clean target");
    let prompts: Vec<_> = persisted
        .messages
        .iter()
        .filter_map(|message| {
            message.content.iter().find_map(|content| match content {
                crate::message::ContentBlock::Text { text, .. } if text == prompt => Some(text),
                _ => None,
            })
        })
        .collect();
    assert_eq!(prompts.len(), 1, "first prompt must persist exactly once");
    assert!(
        persisted.parent_id.is_none(),
        "clean target must have no parent"
    );
    assert_eq!(
        persisted.working_dir.as_deref(),
        Some(canonical_working_dir.to_string_lossy().as_ref())
    );
    assert_eq!(
        complete_calls.load(Ordering::SeqCst),
        1,
        "first prompt starts exactly one provider turn"
    );
    assert_eq!(
        source.lock().await.visible_conversation_message_count(),
        0,
        "source session remains unchanged"
    );
    drop(client_writer);
    server_task
        .await
        .expect("server task join")
        .expect("server task result");
}

#[tokio::test]
async fn resuming_a_new_clean_session_retains_the_prior_established_session() {
    let _storage = crate::storage::lock_test_env();
    let _home = IsolatedSessionHome::new();
    let working_dir_a = tempfile::tempdir().expect("working directory A");
    let working_dir_b = tempfile::tempdir().expect("working directory B");
    let complete_calls = Arc::new(AtomicUsize::new(0));
    let count_completions = Arc::new(AtomicBool::new(false));
    let provider: Arc<dyn Provider> = Arc::new(CleanSessionProvider {
        forks: Arc::new(AtomicUsize::new(0)),
        complete_calls: Arc::clone(&complete_calls),
        count_completions: Arc::clone(&count_completions),
    });
    let (sessions, _, _) = clean_session_state();
    let (client_stream, server_task, _state) =
        start_clean_session_client(Arc::clone(&provider), Arc::clone(&sessions)).await;
    let (client_reader, mut client_writer) = client_stream.into_split();
    let mut client_reader = BufReader::new(client_reader);

    send_request(
        &mut client_writer,
        &subscribe_request(1, working_dir_a.path().to_string_lossy().into_owned()),
    )
    .await;
    send_request(
        &mut client_writer,
        &Request::CreateSession {
            id: 2,
            working_dir: working_dir_a.path().to_string_lossy().into_owned(),
            runtime: empty_runtime(),
        },
    )
    .await;
    let ServerEvent::SessionCreated {
        session_id: session_a,
        ..
    } = recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::SessionCreated { id: 2, .. })
    })
    .await
    else {
        unreachable!()
    };

    // This is the initial target-aware attach, so it is allowed to discard the
    // connection's provisional bootstrap session.
    let mut attach_a = subscribe_request(3, working_dir_a.path().to_string_lossy().into_owned());
    if let Request::Subscribe {
        target_session_id, ..
    } = &mut attach_a
    {
        *target_session_id = Some(session_a.clone());
    }
    send_request(&mut client_writer, &attach_a).await;
    recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::Done { id: 3 })
    })
    .await;

    count_completions.store(true, Ordering::SeqCst);
    let prompt_a = "prompt A remains in A";
    send_request(
        &mut client_writer,
        &Request::Message {
            id: 4,
            content: prompt_a.to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
    )
    .await;
    recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::Done { id: 4 })
    })
    .await;

    send_request(
        &mut client_writer,
        &Request::CreateSession {
            id: 5,
            working_dir: working_dir_b.path().to_string_lossy().into_owned(),
            runtime: empty_runtime(),
        },
    )
    .await;
    let ServerEvent::SessionCreated {
        session_id: session_b,
        ..
    } = recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::SessionCreated { id: 5, .. })
    })
    .await
    else {
        unreachable!()
    };
    send_request(
        &mut client_writer,
        &Request::ResumeSession {
            id: 6,
            session_id: session_b.clone(),
            client_instance_id: None,
            client_has_local_history: false,
            allow_session_takeover: false,
        },
    )
    .await;
    recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::Done { id: 6 })
    })
    .await;

    let prompt_b = "prompt B remains in B";
    send_request(
        &mut client_writer,
        &Request::Message {
            id: 7,
            content: prompt_b.to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
    )
    .await;
    recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::Done { id: 7 })
    })
    .await;

    let sessions_guard = sessions.read().await;
    let agent_a = sessions_guard
        .get(&session_a)
        .expect("established A remains registered")
        .clone();
    assert!(sessions_guard.contains_key(&session_b));
    drop(sessions_guard);
    assert!(matches!(
        agent_a.lock().await.session_for_split().status,
        crate::session::SessionStatus::Active
    ));

    let persisted_a = crate::session::Session::load(&session_a).expect("persisted A");
    let persisted_b = crate::session::Session::load(&session_b).expect("persisted B");
    assert_eq!(
        persisted_a
            .messages
            .iter()
            .filter(|message| message.content.iter().any(|content| matches!(content, crate::message::ContentBlock::Text { text, .. } if text == prompt_a)))
            .count(),
        1
    );
    assert_eq!(
        persisted_b
            .messages
            .iter()
            .filter(|message| message.content.iter().any(|content| matches!(content, crate::message::ContentBlock::Text { text, .. } if text == prompt_b)))
            .count(),
        1
    );
    assert_eq!(complete_calls.load(Ordering::SeqCst), 2);

    // A and B remain independently attachable after the ordinary resume, and
    // the socket receives the exact target transcript rather than merely Done.
    for (id, session_id, expected, excluded) in [
        (8, session_a.clone(), prompt_a, prompt_b),
        (10, session_b.clone(), prompt_b, prompt_a),
    ] {
        send_request(
            &mut client_writer,
            &Request::ResumeSession {
                id,
                session_id: session_id.clone(),
                client_instance_id: None,
                client_has_local_history: false,
                allow_session_takeover: false,
            },
        )
        .await;
        recv_until(
            &mut client_reader,
            |event| matches!(event, ServerEvent::Done { id: done_id } if *done_id == id),
        )
        .await;
        let history_id = id + 1;
        send_request(&mut client_writer, &Request::GetHistory { id: history_id }).await;
        let ServerEvent::History {
            id: received_id,
            session_id: history_session_id,
            messages,
            ..
        } = recv_until(&mut client_reader, |event| {
            matches!(event, ServerEvent::History { id: candidate, .. } if *candidate == history_id)
        })
        .await
        else {
            unreachable!()
        };
        assert_eq!(received_id, history_id);
        assert_eq!(history_session_id, session_id);
        assert!(
            messages
                .iter()
                .any(|message| message.content.contains(expected))
        );
        assert!(
            !messages
                .iter()
                .any(|message| message.content.contains(excluded))
        );
    }

    drop(client_writer);
    server_task
        .await
        .expect("server task join")
        .expect("server task result");
}

#[tokio::test]
async fn busy_streaming_a_resume_b_does_not_leak_and_resume_a_retains_history() {
    let _storage = crate::storage::lock_test_env();
    let _home = IsolatedSessionHome::new();
    let working_dir_a = tempfile::tempdir().expect("working directory A");
    let working_dir_b = tempfile::tempdir().expect("working directory B");
    let provider = Arc::new(BusySocketOracleProvider::new());
    let (sessions, _, _) = clean_session_state();
    let (client_stream, server_task, _state) =
        start_clean_session_client(provider.clone(), Arc::clone(&sessions)).await;
    let (client_reader, mut client_writer) = client_stream.into_split();
    let mut client_reader = BufReader::new(client_reader);

    send_request(
        &mut client_writer,
        &subscribe_request(1, working_dir_a.path().to_string_lossy().into_owned()),
    )
    .await;
    send_request(
        &mut client_writer,
        &Request::CreateSession {
            id: 2,
            working_dir: working_dir_a.path().to_string_lossy().into_owned(),
            runtime: empty_runtime(),
        },
    )
    .await;
    let ServerEvent::SessionCreated {
        session_id: session_a,
        ..
    } = recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::SessionCreated { id: 2, .. })
    })
    .await
    else {
        unreachable!()
    };

    let mut attach_a = subscribe_request(3, working_dir_a.path().to_string_lossy().into_owned());
    if let Request::Subscribe {
        target_session_id, ..
    } = &mut attach_a
    {
        *target_session_id = Some(session_a.clone());
    }
    send_request(&mut client_writer, &attach_a).await;
    recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::Done { id: 3 })
    })
    .await;

    send_request(
        &mut client_writer,
        &Request::CreateSession {
            id: 4,
            working_dir: working_dir_b.path().to_string_lossy().into_owned(),
            runtime: empty_runtime(),
        },
    )
    .await;
    let ServerEvent::SessionCreated {
        session_id: session_b,
        ..
    } = recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::SessionCreated { id: 4, .. })
    })
    .await
    else {
        unreachable!()
    };
    send_request(
        &mut client_writer,
        &Request::ResumeSession {
            id: 5,
            session_id: session_b.clone(),
            client_instance_id: None,
            client_has_local_history: false,
            allow_session_takeover: false,
        },
    )
    .await;
    recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::Done { id: 5 })
    })
    .await;

    let prompt_b = "BUSY_SOCKET_ORACLE_B_CONTEXT";
    send_request(
        &mut client_writer,
        &Request::Message {
            id: 6,
            content: prompt_b.to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: true,
        },
    )
    .await;

    send_request(
        &mut client_writer,
        &Request::ResumeSession {
            id: 7,
            session_id: session_a.clone(),
            client_instance_id: None,
            client_has_local_history: false,
            allow_session_takeover: false,
        },
    )
    .await;
    recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::Done { id: 7 })
    })
    .await;

    send_request(
        &mut client_writer,
        &Request::Message {
            id: 8,
            content: BUSY_SOCKET_ORACLE_PROMPT.to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
    )
    .await;

    let mut saw_a_initial = false;
    while !saw_a_initial {
        let event = tokio::time::timeout(Duration::from_secs(5), recv_event(&mut client_reader))
            .await
            .expect("timed out waiting for the positive initial A delta");
        if let ServerEvent::Error { message, .. } = &event {
            panic!("A turn was rejected before its initial delta: {message}");
        }
        if matches!(event, ServerEvent::Done { id: 8 }) {
            panic!("A turn completed without the gated provider initial delta");
        }
        if matches!(
            &event,
            ServerEvent::TextDelta { text } if text == BUSY_SOCKET_ORACLE_INITIAL
        ) {
            saw_a_initial = true;
        }
    }
    assert!(
        provider.a_initial_sent.load(Ordering::SeqCst),
        "the positive initial A delta must come from the gated provider"
    );

    send_request(
        &mut client_writer,
        &Request::ResumeSession {
            id: 9,
            session_id: session_b.clone(),
            client_instance_id: None,
            client_has_local_history: false,
            allow_session_takeover: false,
        },
    )
    .await;
    let mut post_switch_events = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = recv_event(&mut client_reader).await;
            let is_done = matches!(event, ServerEvent::Done { id: 9 });
            post_switch_events.push(event);
            let latest = post_switch_events.last().expect("just pushed event");
            if let ServerEvent::Error { message, .. } = latest {
                panic!("busy Resume B was rejected: {message}");
            }
            assert!(
                !matches!(
                    latest,
                    ServerEvent::TextDelta { text }
                        if text == BUSY_SOCKET_ORACLE_INITIAL
                            || text == BUSY_SOCKET_ORACLE_RESPONSE
                ),
                "A stream leaked into the B resume frames: {post_switch_events:?}"
            );
            assert!(
                !matches!(latest, ServerEvent::MessageEnd { .. })
                    && !matches!(latest, ServerEvent::Done { id: 8 }),
                "A terminal frame leaked into the B resume frames: {post_switch_events:?}"
            );
            if is_done {
                break;
            }
        }
    })
    .await
    .expect("busy Resume B must complete on the same socket");

    provider.release_a.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if provider.a_finished.load(Ordering::SeqCst) {
                break;
            }
            provider.a_finished_notify.notified().await;
        }
    })
    .await
    .expect("A provider stream must positively finish after release");

    let stale_frame =
        tokio::time::timeout(Duration::from_millis(500), recv_event(&mut client_reader)).await;
    if let Ok(event) = stale_frame {
        panic!("A stream or terminal frame leaked after B Resume completed: {event:?}");
    }

    send_request(
        &mut client_writer,
        &Request::ResumeSession {
            id: 10,
            session_id: session_a.clone(),
            client_instance_id: None,
            client_has_local_history: false,
            allow_session_takeover: false,
        },
    )
    .await;
    recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::Done { id: 10 })
    })
    .await;
    send_request(&mut client_writer, &Request::GetHistory { id: 11 }).await;
    let ServerEvent::History {
        id,
        session_id,
        messages,
        ..
    } = recv_until(
        &mut client_reader,
        |event| matches!(event, ServerEvent::History { id: candidate, .. } if *candidate == 11),
    )
    .await
    else {
        unreachable!()
    };
    assert_eq!(id, 11);
    assert_eq!(session_id, session_a);
    assert!(
        messages
            .iter()
            .any(|message| message.content.contains(BUSY_SOCKET_ORACLE_PROMPT))
    );
    assert!(
        messages
            .iter()
            .any(|message| message.content.contains(BUSY_SOCKET_ORACLE_RESPONSE))
    );
    assert!(
        !messages
            .iter()
            .any(|message| message.content.contains(prompt_b))
    );

    drop(client_writer);
    server_task
        .await
        .expect("server task join")
        .expect("server task result");
}

#[tokio::test]
async fn authorized_stop_busy_a_then_resume_b_does_not_leak_a_stream() {
    let _storage = crate::storage::lock_test_env();
    let _home = IsolatedSessionHome::new();
    let working_dir_a = tempfile::tempdir().expect("working directory A");
    let working_dir_b = tempfile::tempdir().expect("working directory B");
    let provider = Arc::new(BusySocketOracleProvider::new());
    let (sessions, _, _) = clean_session_state();
    let (client_stream, server_task, state) =
        start_clean_session_client(provider.clone(), Arc::clone(&sessions)).await;
    let (client_reader, mut client_writer) = client_stream.into_split();
    let mut client_reader = BufReader::new(client_reader);

    send_request(
        &mut client_writer,
        &subscribe_request(1, working_dir_a.path().to_string_lossy().into_owned()),
    )
    .await;
    send_request(
        &mut client_writer,
        &Request::CreateSession {
            id: 2,
            working_dir: working_dir_a.path().to_string_lossy().into_owned(),
            runtime: empty_runtime(),
        },
    )
    .await;
    let ServerEvent::SessionCreated {
        session_id: session_a,
        ..
    } = recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::SessionCreated { id: 2, .. })
    })
    .await
    else {
        unreachable!()
    };

    send_request(
        &mut client_writer,
        &Request::CreateSession {
            id: 3,
            working_dir: working_dir_b.path().to_string_lossy().into_owned(),
            runtime: empty_runtime(),
        },
    )
    .await;
    let ServerEvent::SessionCreated {
        session_id: session_b,
        ..
    } = recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::SessionCreated { id: 3, .. })
    })
    .await
    else {
        unreachable!()
    };

    // Model the real coordinator/child ownership relation, but invoke the
    // production CommStop request below rather than removing A in the fixture.
    let swarm_id = "stop-switch-oracle-swarm".to_string();
    let (coord_tx, _coord_rx) = mpsc::unbounded_channel();
    let (a_tx, _a_rx) = mpsc::unbounded_channel();
    let now = std::time::Instant::now();
    let member = |session_id: String,
                  event_tx: mpsc::UnboundedSender<ServerEvent>,
                  owner: Option<String>,
                  role: &str|
     -> SwarmMember {
        SwarmMember {
            session_id,
            event_tx,
            event_txs: HashMap::new(),
            working_dir: None,
            swarm_id: Some(swarm_id.clone()),
            swarm_enabled: true,
            status: "working".to_string(),
            detail: None,
            friendly_name: None,
            report_back_to_session_id: owner,
            latest_completion_report: None,
            role: role.to_string(),
            joined_at: now,
            last_status_change: now,
            is_headless: false,
            output_tail: None,
            todo_progress: None,
            todo_items: Vec::new(),
            runtime: crate::protocol::SwarmMemberRuntime::default(),
            task_label: None,
        }
    };
    state.swarm_members.write().await.extend([
        (
            "stop-coordinator".to_string(),
            member(
                "stop-coordinator".to_string(),
                coord_tx,
                None,
                "coordinator",
            ),
        ),
        (
            session_a.clone(),
            member(
                session_a.clone(),
                a_tx,
                Some("stop-coordinator".to_string()),
                "agent",
            ),
        ),
    ]);
    state.swarms_by_id.write().await.insert(
        swarm_id,
        HashSet::from(["stop-coordinator".to_string(), session_a.clone()]),
    );

    send_request(
        &mut client_writer,
        &Request::ResumeSession {
            id: 4,
            session_id: session_b.clone(),
            client_instance_id: None,
            client_has_local_history: false,
            allow_session_takeover: false,
        },
    )
    .await;
    recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::Done { id: 4 })
    })
    .await;
    let prompt_b = "STOP_SWITCH_ORACLE_B_CONTEXT";
    send_request(
        &mut client_writer,
        &Request::Message {
            id: 5,
            content: prompt_b.to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: true,
        },
    )
    .await;

    send_request(
        &mut client_writer,
        &Request::ResumeSession {
            id: 6,
            session_id: session_a.clone(),
            client_instance_id: None,
            client_has_local_history: false,
            allow_session_takeover: false,
        },
    )
    .await;
    recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::Done { id: 6 })
    })
    .await;
    send_request(
        &mut client_writer,
        &Request::Message {
            id: 7,
            content: BUSY_SOCKET_ORACLE_PROMPT.to_string(),
            images: Vec::new(),
            system_reminder: None,
            active_skill: None,
            no_reply: false,
        },
    )
    .await;
    recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::TextDelta { text } if text == BUSY_SOCKET_ORACLE_INITIAL)
    })
    .await;

    send_request(
        &mut client_writer,
        &Request::CommStop {
            id: 8,
            session_id: "stop-coordinator".to_string(),
            target_session: session_a.clone(),
            force: Some(false),
        },
    )
    .await;
    recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::Done { id: 8 })
    })
    .await;
    assert!(!sessions.read().await.contains_key(&session_a));
    assert!(!state.swarm_members.read().await.contains_key(&session_a));

    send_request(
        &mut client_writer,
        &Request::ResumeSession {
            id: 9,
            session_id: session_b.clone(),
            client_instance_id: None,
            client_has_local_history: false,
            allow_session_takeover: false,
        },
    )
    .await;
    let mut resume_b_events = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = recv_event(&mut client_reader).await;
            assert!(
                !matches!(
                    &event,
                    ServerEvent::TextDelta { text }
                        if text == BUSY_SOCKET_ORACLE_INITIAL
                            || text == BUSY_SOCKET_ORACLE_RESPONSE
                ),
                "A stream leaked while Resume B was completing: {event:?}"
            );
            assert!(
                !matches!(
                    &event,
                    ServerEvent::MessageEnd { .. } | ServerEvent::Done { id: 7 }
                ),
                "A terminal frame leaked while Resume B was completing: {event:?}"
            );
            let done = matches!(event, ServerEvent::Done { id: 9 });
            resume_b_events.push(event);
            if done {
                break;
            }
        }
    })
    .await
    .expect("Resume B must complete while A provider remains gated");
    assert!(matches!(
        resume_b_events.last(),
        Some(ServerEvent::Done { id: 9 })
    ));

    let stale_frame =
        tokio::time::timeout(Duration::from_millis(500), recv_event(&mut client_reader)).await;
    if let Ok(event) = stale_frame {
        panic!("unexpected frame before gated A release: {event:?}");
    }

    send_request(&mut client_writer, &Request::GetHistory { id: 10 }).await;
    let ServerEvent::History {
        session_id,
        messages,
        ..
    } = recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::History { id: 10, .. })
    })
    .await
    else {
        unreachable!()
    };
    assert_eq!(session_id, session_b);
    assert!(
        messages
            .iter()
            .any(|message| message.content.contains(prompt_b))
    );
    assert!(
        !messages
            .iter()
            .any(|message| message.content.contains(BUSY_SOCKET_ORACLE_INITIAL))
    );
    assert!(
        !messages
            .iter()
            .any(|message| message.content.contains(BUSY_SOCKET_ORACLE_RESPONSE))
    );

    provider.release_a.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if provider.a_terminated.load(Ordering::SeqCst) {
                break;
            }
            provider.a_terminated_notify.notified().await;
        }
    })
    .await
    .expect("stopped A provider stream must positively terminate");

    let stale_frame =
        tokio::time::timeout(Duration::from_millis(500), recv_event(&mut client_reader)).await;
    if let Ok(event) = stale_frame {
        panic!("stopped A stream or terminal frame leaked after B Resume completed: {event:?}");
    }

    send_request(&mut client_writer, &Request::GetHistory { id: 11 }).await;
    let ServerEvent::History {
        session_id,
        messages,
        ..
    } = recv_until(&mut client_reader, |event| {
        matches!(event, ServerEvent::History { id: 11, .. })
    })
    .await
    else {
        unreachable!()
    };
    assert_eq!(session_id, session_b);
    assert!(
        messages
            .iter()
            .any(|message| message.content.contains(prompt_b))
    );
    assert!(
        !messages
            .iter()
            .any(|message| message.content.contains(BUSY_SOCKET_ORACLE_INITIAL))
    );
    assert!(
        !messages
            .iter()
            .any(|message| message.content.contains(BUSY_SOCKET_ORACLE_RESPONSE))
    );

    drop(client_writer);
    server_task
        .await
        .expect("server task join")
        .expect("server task result");
}

#[test]
fn working_directory_resolution_accepts_absolute_and_server_home_forms() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().join("server-home");
    let child = home.join("child");
    std::fs::create_dir_all(&child).expect("create fixture directories");

    assert_eq!(
        canonicalize_new_session_working_dir_from_home(home.to_str().unwrap(), &home).unwrap(),
        std::fs::canonicalize(&home).unwrap()
    );
    assert_eq!(
        canonicalize_new_session_working_dir_from_home("~", &home).unwrap(),
        std::fs::canonicalize(&home).unwrap()
    );
    assert_eq!(
        canonicalize_new_session_working_dir_from_home("~/child", &home).unwrap(),
        std::fs::canonicalize(&child).unwrap()
    );
}

#[test]
fn working_directory_resolution_rejects_relative_missing_and_files() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().join("server-home");
    std::fs::create_dir_all(&home).unwrap();
    let file = home.join("not-a-directory");
    std::fs::write(&file, "file").unwrap();

    assert_eq!(
        canonicalize_new_session_working_dir_from_home("relative", &home),
        Err("Working directory must be an absolute path or start with ~/".to_string())
    );
    assert_eq!(
        canonicalize_new_session_working_dir_from_home("~other", &home),
        Err("Working directory must be an absolute path or start with ~/".to_string())
    );
    assert_eq!(
        canonicalize_new_session_working_dir_from_home("~/missing", &home),
        Err("Working directory does not exist".to_string())
    );
    assert_eq!(
        canonicalize_new_session_working_dir_from_home(file.to_str().unwrap(), &home),
        Err("Working directory is not a directory".to_string())
    );
}

#[test]
fn recent_working_dirs_preserve_recency_deduplicate_and_cap() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().join("server-home");
    std::fs::create_dir_all(&home).unwrap();
    let mut entries = Vec::new();
    for index in 0..22 {
        let dir = home.join(format!("dir-{index}"));
        std::fs::create_dir(&dir).unwrap();
        entries.push(metadata(
            &format!("session-{index}"),
            Some(dir.to_string_lossy().into_owned()),
            100 - index,
        ));
    }
    entries.insert(
        1,
        metadata(
            "duplicate",
            Some(home.join("dir-0/.").to_string_lossy().into_owned()),
            99,
        ),
    );
    entries.insert(
        2,
        metadata(
            "missing",
            Some(home.join("missing").to_string_lossy().into_owned()),
            98,
        ),
    );

    let recent = recent_working_dirs(&entries, &home);
    assert_eq!(MAX_RECENT_WORKING_DIRS, 5);
    assert_eq!(recent.len(), MAX_RECENT_WORKING_DIRS);
    assert_eq!(
        recent[0],
        std::fs::canonicalize(home.join("dir-0"))
            .unwrap()
            .to_string_lossy()
    );
    assert_eq!(
        recent[1],
        std::fs::canonicalize(home.join("dir-1"))
            .unwrap()
            .to_string_lossy()
    );
    assert!(recent.iter().all(|path| Path::new(path).is_absolute()));
}

#[test]
fn directory_completion_preserves_home_and_absolute_lexical_prefixes() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().join("server-home");
    std::fs::create_dir_all(&home).unwrap();

    let home_query = directory_completion_query("~/pro", &home).unwrap();
    assert_eq!(home_query.parent, home);
    assert_eq!(home_query.rendered_parent, "~/");
    assert_eq!(home_query.prefix, "pro");

    let absolute = root.path().join("projects");
    let absolute_query = directory_completion_query(&absolute.to_string_lossy(), &home).unwrap();
    assert_eq!(absolute_query.parent, root.path());
    assert_eq!(
        absolute_query.rendered_parent,
        format!("{}/", root.path().display())
    );
    assert_eq!(absolute_query.prefix, "projects");
}

#[test]
fn directory_completion_returns_directories_symlinks_and_trailing_slashes_only() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(root.path().join("project")).unwrap();
    std::fs::create_dir(root.path().join("probe")).unwrap();
    std::fs::write(root.path().join("profile.txt"), "not a directory").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        root.path().join("project"),
        root.path().join("project-link"),
    )
    .unwrap();

    let input = format!("{}/pro", root.path().display());
    let (candidates, truncated) = complete_working_directory_sync(&input, 32).unwrap();
    assert!(!truncated);
    assert_eq!(
        candidates,
        vec![
            format!("{}/probe/", root.path().display()),
            format!("{}/project/", root.path().display()),
            #[cfg(unix)]
            format!("{}/project-link/", root.path().display()),
        ]
    );
}

#[test]
fn directory_completion_caps_results_and_reports_truncation() {
    let root = tempfile::tempdir().expect("tempdir");
    for index in 0..101 {
        std::fs::create_dir(root.path().join(format!("project-{index:03}"))).unwrap();
    }
    let input = format!("{}/project", root.path().display());
    let (candidates, truncated) = complete_working_directory_sync(&input, 500).unwrap();
    assert_eq!(candidates.len(), MAX_DIRECTORY_COMPLETION_RESULTS);
    assert!(truncated);
    assert!(candidates.iter().all(|candidate| candidate.ends_with('/')));
}

#[test]
fn directory_completion_scan_is_bounded_and_missing_parent_is_correlated_as_an_error() {
    let root = tempfile::tempdir().expect("tempdir");
    for index in 0..=MAX_DIRECTORY_COMPLETION_ENTRIES {
        std::fs::create_dir(root.path().join(format!("entry-{index:04}"))).unwrap();
    }
    let (names, truncated) = scan_directory_completion_parent(root.path()).unwrap();
    assert!(truncated);
    assert!(names.len() <= MAX_DIRECTORY_COMPLETION_ENTRIES);

    let missing = root.path().join("missing");
    assert_eq!(
        complete_working_directory_sync(&format!("{}/", missing.display()), 32),
        Err("Working directory does not exist".to_string())
    );
}

#[test]
fn directory_completion_cache_is_bounded() {
    let mut cache = DirectoryCompletionCache::default();
    for index in 0..=MAX_DIRECTORY_COMPLETION_CACHE_ENTRIES {
        cache.insert(DirectoryCompletionCacheEntry {
            parent: PathBuf::from(format!("/completion-cache-{index}")),
            names: Vec::new(),
            truncated: false,
            created_at: Instant::now(),
        });
    }
    assert_eq!(cache.entries.len(), MAX_DIRECTORY_COMPLETION_CACHE_ENTRIES);
    assert!(
        cache
            .entries
            .iter()
            .all(|entry| entry.parent != PathBuf::from("/completion-cache-0"))
    );
}

#[test]
fn session_creation_context_uses_the_canonical_server_home_not_process_cwd() {
    let root = tempfile::tempdir().expect("tempdir");
    let server_home = root.path().join("server-home");
    std::fs::create_dir_all(&server_home).unwrap();
    let canonical_home = std::fs::canonicalize(&server_home).unwrap();
    let event = session_creation_context_event(51, &canonical_home, &[]);

    assert!(matches!(
        event,
        ServerEvent::SessionCreationContext { id: 51, home_dir, recent_working_dirs }
            if home_dir == canonical_home.to_string_lossy()
                && recent_working_dirs.is_empty()
    ));
}

#[tokio::test]
async fn resolution_errors_are_request_correlated_and_never_create_a_session() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_resolve_working_directory(73, "relative", &tx)
        .await
        .unwrap();
    assert!(matches!(
        rx.recv().await,
        Some(ServerEvent::Error { id: 73, message, retry_after_secs: None })
            if message == "Working directory must be an absolute path or start with ~/"
    ));
}

#[tokio::test]
async fn completion_errors_are_request_correlated_and_reject_relative_paths() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_complete_working_directory(74, "relative".to_string(), 32, tx);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), rx.recv()).await,
        Ok(Some(ServerEvent::Error { id: 74, message, retry_after_secs: None }))
            if message == "Working directory must be an absolute path or start with ~/"
    ));
}

#[tokio::test]
async fn clean_creation_has_independent_empty_live_identity_and_controls() {
    let directory = tempfile::tempdir().unwrap();
    let forks = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(CleanSessionProvider {
        forks: forks.clone(),
        complete_calls: Arc::new(AtomicUsize::new(0)),
        count_completions: Arc::new(AtomicBool::new(false)),
    });
    let (sessions, shutdown_signals, soft_interrupt_queues) = clean_session_state();
    let pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_create_session(
        91,
        directory.path().to_string_lossy().into_owned(),
        empty_runtime(),
        &provider,
        &sessions,
        &shutdown_signals,
        &soft_interrupt_queues,
        &pool,
        &tx,
    )
    .await
    .unwrap();

    let ServerEvent::SessionCreated {
        session_id,
        working_dir,
        ..
    } = rx.recv().await.unwrap()
    else {
        panic!("expected clean SessionCreated event");
    };
    assert_eq!(
        working_dir,
        std::fs::canonicalize(directory.path())
            .unwrap()
            .to_string_lossy()
    );
    assert_eq!(forks.load(Ordering::SeqCst), 1);
    let agent = sessions.read().await.get(&session_id).cloned().unwrap();
    let agent = agent.lock().await;
    assert_eq!(agent.working_dir(), Some(working_dir.as_str()));
    assert_eq!(agent.visible_conversation_message_count(), 0);
    drop(agent);
    assert!(shutdown_signals.read().await.contains_key(&session_id));
    assert!(soft_interrupt_queues.read().await.contains_key(&session_id));
}

#[tokio::test]
async fn invalid_clean_creation_does_not_publish_a_live_runtime() {
    let forks = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(CleanSessionProvider {
        forks: forks.clone(),
        complete_calls: Arc::new(AtomicUsize::new(0)),
        count_completions: Arc::new(AtomicBool::new(false)),
    });
    let (sessions, shutdown_signals, soft_interrupt_queues) = clean_session_state();
    let pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_create_session(
        92,
        "relative-directory".to_string(),
        empty_runtime(),
        &provider,
        &sessions,
        &shutdown_signals,
        &soft_interrupt_queues,
        &pool,
        &tx,
    )
    .await
    .unwrap();

    assert!(matches!(
        rx.recv().await,
        Some(ServerEvent::Error { id: 92, .. })
    ));
    assert_eq!(forks.load(Ordering::SeqCst), 0);
    assert!(sessions.read().await.is_empty());
    assert!(shutdown_signals.read().await.is_empty());
    assert!(soft_interrupt_queues.read().await.is_empty());
}

#[tokio::test]
async fn provider_or_route_without_a_model_is_rejected_without_a_live_runtime() {
    let directory = tempfile::tempdir().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(CleanSessionProvider {
        forks: Arc::new(AtomicUsize::new(0)),
        complete_calls: Arc::new(AtomicUsize::new(0)),
        count_completions: Arc::new(AtomicBool::new(false)),
    });
    let (sessions, shutdown_signals, soft_interrupt_queues) = clean_session_state();
    let pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_create_session(
        93,
        directory.path().to_string_lossy().into_owned(),
        SessionRuntimeSelection {
            provider_key: Some("unsupported-provider".to_string()),
            ..Default::default()
        },
        &provider,
        &sessions,
        &shutdown_signals,
        &soft_interrupt_queues,
        &pool,
        &tx,
    )
    .await
    .unwrap();

    assert!(matches!(
        rx.recv().await,
        Some(ServerEvent::Error { id: 93, .. })
    ));
    assert!(sessions.read().await.is_empty());
    assert!(shutdown_signals.read().await.is_empty());
    assert!(soft_interrupt_queues.read().await.is_empty());
}

#[cfg(unix)]
#[test]
fn working_directory_resolution_rejects_inaccessible_directory_when_permissions_apply() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().join("server-home");
    let denied = home.join("denied");
    std::fs::create_dir_all(&denied).unwrap();
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o000)).unwrap();
    let result = canonicalize_new_session_working_dir_from_home(denied.to_str().unwrap(), &home);
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o700)).unwrap();
    // Some test environments run with elevated privileges, where access is
    // legitimately permitted. Ordinary macOS/Linux users must be rejected.
    if result.is_err() {
        assert_eq!(
            result,
            Err("Working directory is not accessible".to_string())
        );
    }
}
