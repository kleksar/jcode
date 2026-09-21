use super::background_tasks::resume_owner_after_completed_comm_report;
use super::swarm::update_member_status_with_report_tldr_and_delivery;
use super::{SessionAgents, SessionInterruptQueues, SwarmEvent, SwarmMember, truncate_detail};
use crate::protocol::ServerEvent;
use jcode_swarm_core::format_structured_completion_report as format_report;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::sync::{RwLock, broadcast, mpsc};

pub(super) struct CommReportContext<'a> {
    pub(super) sessions: &'a SessionAgents,
    pub(super) soft_interrupt_queues: &'a SessionInterruptQueues,
    pub(super) swarm_members: &'a Arc<RwLock<HashMap<String, SwarmMember>>>,
    pub(super) swarms_by_id: &'a Arc<RwLock<HashMap<String, HashSet<String>>>>,
    pub(super) event_history: &'a Arc<RwLock<VecDeque<SwarmEvent>>>,
    pub(super) event_counter: &'a Arc<AtomicU64>,
    pub(super) swarm_event_tx: &'a broadcast::Sender<SwarmEvent>,
}

/// Shared CommReport orchestration for initialized and lightweight clients.
///
/// This function owns the report completion predicate. Generic member-status
/// changes never reach the owner-resumption path because they do not call this
/// handler, and only an exact `completed` CommReport triggers the wake policy.
pub(super) async fn handle_comm_report(
    id: u64,
    req_session_id: String,
    status: Option<String>,
    message: String,
    validation: Option<String>,
    follow_up: Option<String>,
    tldr: Option<String>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
    context: CommReportContext<'_>,
) {
    let CommReportContext {
        sessions,
        soft_interrupt_queues,
        swarm_members,
        swarms_by_id,
        event_history,
        event_counter,
        swarm_event_tx,
    } = context;
    let status = status.unwrap_or_else(|| "ready".to_string());
    let report = format_report(&message, validation.as_deref(), follow_up.as_deref());
    let detail = Some(truncate_detail(&message, 160));
    let delivery = update_member_status_with_report_tldr_and_delivery(
        &req_session_id,
        &status,
        detail,
        Some(report),
        tldr,
        swarm_members,
        swarms_by_id,
        Some(event_history),
        Some(event_counter),
        Some(swarm_event_tx),
    )
    .await;

    if status == "completed" {
        resume_owner_after_completed_comm_report(
            delivery,
            sessions,
            soft_interrupt_queues,
            swarm_members,
            swarms_by_id,
            event_history,
            event_counter,
            swarm_event_tx,
        )
        .await;
    }

    let _ = client_event_tx.send(ServerEvent::CommReportResponse {
        id,
        status,
        message: "Report recorded and delivered to the coordinator when applicable.".to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;
    use crate::message::{Message, StreamEvent, ToolDefinition};
    use crate::provider::{EventStream, Provider};
    use crate::server::{RuntimeFastState, SessionAgentEntry};
    use crate::tool::Registry;
    use anyhow::Result;
    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};
    use tokio::time::timeout;

    #[derive(Clone, Default)]
    struct ReportProvider {
        calls: Arc<AtomicUsize>,
        responses: Arc<StdMutex<Vec<Vec<StreamEvent>>>>,
    }

    impl ReportProvider {
        fn queue_response(&self, response: Vec<StreamEvent>) {
            self.responses
                .lock()
                .expect("report provider response lock")
                .push(response);
        }
    }

    #[async_trait]
    impl Provider for ReportProvider {
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> Result<EventStream> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let response = self
                .responses
                .lock()
                .expect("report provider response lock")
                .pop()
                .unwrap_or_default();
            Ok(Box::pin(tokio_stream::iter(response.into_iter().map(Ok))))
        }

        fn name(&self) -> &str {
            "report-test"
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(self.clone())
        }
    }

    struct ReportFixture {
        worker_id: String,
        provider: Arc<ReportProvider>,
        owner_agent: Arc<tokio::sync::Mutex<Agent>>,
        sessions: SessionAgents,
        soft_interrupt_queues: SessionInterruptQueues,
        owner_queue: jcode_agent_runtime::SoftInterruptQueue,
        swarm_members: Arc<RwLock<HashMap<String, SwarmMember>>>,
        swarms_by_id: Arc<RwLock<HashMap<String, HashSet<String>>>>,
        event_history: Arc<RwLock<VecDeque<SwarmEvent>>>,
        event_counter: Arc<AtomicU64>,
        swarm_event_tx: broadcast::Sender<SwarmEvent>,
        owner_rx: mpsc::UnboundedReceiver<ServerEvent>,
        sibling_rx: mpsc::UnboundedReceiver<ServerEvent>,
    }

    async fn fixture() -> ReportFixture {
        let worker_id = "worker-report".to_string();
        let provider = Arc::new(ReportProvider::default());
        provider.queue_response(vec![
            StreamEvent::TextDelta("owner resumed".to_string()),
            StreamEvent::MessageEnd { stop_reason: None },
        ]);
        let provider_dyn: Arc<dyn Provider> = provider.clone();
        let registry = Registry::new(provider_dyn.clone()).await;
        let owner_agent = Arc::new(tokio::sync::Mutex::new(Agent::new(provider_dyn, registry)));
        let owner_id = owner_agent.lock().await.session_id().to_string();
        let owner_queue = owner_agent.lock().await.soft_interrupt_queue();
        let sessions = Arc::new(RwLock::new(HashMap::from([(
            owner_id.clone(),
            SessionAgentEntry::new(
                owner_agent.clone(),
                RuntimeFastState::invalid(owner_id.clone()),
            ),
        )])));
        let soft_interrupt_queues = Arc::new(RwLock::new(HashMap::from([(
            owner_id.clone(),
            owner_queue.clone(),
        )])));

        let (owner_tx, owner_rx) = mpsc::unbounded_channel();
        let (sibling_tx, sibling_rx) = mpsc::unbounded_channel();
        let (worker_tx, _worker_rx) = mpsc::unbounded_channel();
        let swarm_id = Some("swarm-report".to_string());
        let member = |session_id: &str,
                      role: &str,
                      status: &str,
                      report_back_to_session_id: Option<String>,
                      event_tx: mpsc::UnboundedSender<ServerEvent>,
                      attached: bool| SwarmMember {
            session_id: session_id.to_string(),
            event_tx: event_tx.clone(),
            event_txs: attached
                .then(|| HashMap::from([(String::from("test-client"), event_tx)]))
                .unwrap_or_default(),
            working_dir: None,
            swarm_id: swarm_id.clone(),
            swarm_enabled: true,
            status: status.to_string(),
            detail: None,
            task_label: None,
            friendly_name: Some(session_id.to_string()),
            report_back_to_session_id,
            latest_completion_report: None,
            role: role.to_string(),
            joined_at: Instant::now(),
            last_status_change: Instant::now(),
            is_headless: false,
            output_tail: None,
            todo_progress: None,
            todo_items: Vec::new(),
            runtime: crate::protocol::SwarmMemberRuntime::default(),
        };
        let members = HashMap::from([
            (
                worker_id.clone(),
                member(
                    &worker_id,
                    "agent",
                    "running",
                    Some(owner_id.clone()),
                    worker_tx,
                    false,
                ),
            ),
            (
                owner_id.clone(),
                member(&owner_id, "coordinator", "ready", None, owner_tx, true),
            ),
            (
                "sibling-report".to_string(),
                member("sibling-report", "agent", "ready", None, sibling_tx, true),
            ),
        ]);
        let swarm_members = Arc::new(RwLock::new(members));
        let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
            "swarm-report".to_string(),
            HashSet::from([
                worker_id.clone(),
                owner_id.clone(),
                "sibling-report".to_string(),
            ]),
        )])));
        let event_history = Arc::new(RwLock::new(VecDeque::new()));
        let event_counter = Arc::new(AtomicU64::new(0));
        let (swarm_event_tx, _) = broadcast::channel(16);

        ReportFixture {
            worker_id,
            provider,
            owner_agent,
            sessions,
            soft_interrupt_queues,
            owner_queue,
            swarm_members,
            swarms_by_id,
            event_history,
            event_counter,
            swarm_event_tx,
            owner_rx,
            sibling_rx,
        }
    }

    fn context<'a>(fixture: &'a ReportFixture) -> CommReportContext<'a> {
        CommReportContext {
            sessions: &fixture.sessions,
            soft_interrupt_queues: &fixture.soft_interrupt_queues,
            swarm_members: &fixture.swarm_members,
            swarms_by_id: &fixture.swarms_by_id,
            event_history: &fixture.event_history,
            event_counter: &fixture.event_counter,
            swarm_event_tx: &fixture.swarm_event_tx,
        }
    }

    async fn collect_until_done(
        receiver: &mut mpsc::UnboundedReceiver<ServerEvent>,
    ) -> Vec<ServerEvent> {
        timeout(Duration::from_secs(2), async {
            let mut events = Vec::new();
            loop {
                let event = receiver.recv().await.expect("owner event stream open");
                let done = matches!(event, ServerEvent::Done { id: 0 });
                events.push(event);
                if done {
                    return events;
                }
            }
        })
        .await
        .expect("owner should receive one completed live turn")
    }

    fn notification_count(events: &[ServerEvent]) -> usize {
        events
            .iter()
            .filter(|event| matches!(event, ServerEvent::Notification { .. }))
            .count()
    }

    #[tokio::test]
    async fn completed_comm_report_resumes_attached_idle_owner_once_and_isolates_recipients() {
        let mut fixture = fixture().await;
        let (response_tx, mut response_rx) = mpsc::unbounded_channel();

        handle_comm_report(
            41,
            fixture.worker_id.clone(),
            Some("completed".to_string()),
            "worker report".to_string(),
            Some("tests passed".to_string()),
            None,
            Some("done".to_string()),
            &response_tx,
            context(&fixture),
        )
        .await;

        let owner_events = collect_until_done(&mut fixture.owner_rx).await;
        assert_eq!(notification_count(&owner_events), 1);
        assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 1);
        assert!(fixture.sibling_rx.try_recv().is_err());
        assert!(matches!(
            response_rx.try_recv().expect("CommReport response"),
            ServerEvent::CommReportResponse { id: 41, .. }
        ));
    }

    #[tokio::test]
    async fn completed_comm_report_defers_once_when_owner_is_busy() {
        let mut fixture = fixture().await;
        let _busy_guard = fixture.owner_agent.lock().await;
        let (response_tx, _response_rx) = mpsc::unbounded_channel();

        handle_comm_report(
            42,
            fixture.worker_id.clone(),
            Some("completed".to_string()),
            "busy report".to_string(),
            None,
            None,
            None,
            &response_tx,
            context(&fixture),
        )
        .await;

        let mut notifications = 0;
        while let Ok(event) = fixture.owner_rx.try_recv() {
            if matches!(event, ServerEvent::Notification { .. }) {
                notifications += 1;
            }
        }
        assert_eq!(notifications, 1);
        assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 0);
        let queued = fixture.owner_queue.lock().expect("owner queue lock");
        assert_eq!(queued.len(), 1);
        assert!(queued[0].content.contains("busy report"));
    }

    #[tokio::test]
    async fn noncompleted_comm_report_does_not_resume_owner() {
        let mut fixture = fixture().await;
        let (response_tx, _response_rx) = mpsc::unbounded_channel();

        handle_comm_report(
            43,
            fixture.worker_id.clone(),
            Some("ready".to_string()),
            "progress report".to_string(),
            None,
            None,
            None,
            &response_tx,
            context(&fixture),
        )
        .await;

        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 0);
        assert!(
            fixture
                .owner_queue
                .lock()
                .expect("owner queue lock")
                .is_empty()
        );
        assert!(
            fixture.owner_rx.try_recv().is_ok(),
            "the report notification remains delivered to the owner"
        );
    }

    #[tokio::test]
    async fn generic_member_status_completion_does_not_resume_owner() {
        let fixture = fixture().await;
        let _ = super::super::swarm::update_member_status(
            &fixture.worker_id,
            "completed",
            None,
            &fixture.swarm_members,
            &fixture.swarms_by_id,
            Some(&fixture.event_history),
            Some(&fixture.event_counter),
            Some(&fixture.swarm_event_tx),
        )
        .await;

        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 0);
        assert!(
            fixture
                .owner_queue
                .lock()
                .expect("owner queue lock")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn completed_comm_report_with_absent_owner_is_safe() {
        let fixture = fixture().await;
        fixture
            .swarm_members
            .write()
            .await
            .get_mut(&fixture.worker_id)
            .expect("worker member")
            .report_back_to_session_id = Some("missing-owner".to_string());
        let (response_tx, _response_rx) = mpsc::unbounded_channel();

        handle_comm_report(
            44,
            fixture.worker_id.clone(),
            Some("completed".to_string()),
            "orphaned report".to_string(),
            None,
            None,
            None,
            &response_tx,
            context(&fixture),
        )
        .await;

        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(fixture.provider.calls.load(Ordering::SeqCst), 0);
        assert!(
            !fixture
                .soft_interrupt_queues
                .read()
                .await
                .contains_key("missing-owner")
        );
    }
}
