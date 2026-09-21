use super::*;
use crate::server::{apply_runtime_fast_policy, provider_control, remove_session_agent_entry};
use crate::server::comm_session;
use crate::server::headless::{self, HeadlessMemoryScope};
use crate::agent::{Agent, AstraFirstState, AstraFirstStateHandle};
use crate::message::{ContentBlock, Message, StreamEvent, ToolDefinition};
use crate::protocol::ServerEvent;
use crate::provider::{EventStream, Provider};
use async_trait::async_trait;
use futures::stream;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::Condvar;
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use tokio::sync::{Mutex, Notify, RwLock, mpsc};

#[derive(Clone)]
struct OfflineAnalystProvider {
    responses: Arc<StdMutex<VecDeque<Vec<StreamEvent>>>>,
}

#[async_trait]
impl Provider for OfflineAnalystProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        let events = self
            .responses
            .lock()
            .expect("offline analyst response queue")
            .pop_front()
            .unwrap_or_default();
        Ok(Box::pin(stream::iter(
            events.into_iter().map(Ok::<_, anyhow::Error>),
        )))
    }

    fn name(&self) -> &str {
        "offline-analyst"
    }

    fn model(&self) -> String {
        "offline-analyst".to_string()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[derive(Clone)]
struct NeverEndingRootProvider {
    started: Arc<Notify>,
}

#[async_trait]
impl Provider for NeverEndingRootProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        self.started.notify_one();
        Ok(Box::pin(stream::unfold(0_u64, |step| async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            Some((Ok(StreamEvent::TextDelta("root".to_string())), step + 1))
        })))
    }

    fn name(&self) -> &str {
        "never-ending-root"
    }

    fn model(&self) -> String {
        "never-ending-root".to_string()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[derive(Clone)]
struct ScopedFastPolicyProvider {
    model: String,
    override_tier: Arc<StdMutex<Option<Option<String>>>>,
    service_tier: Arc<StdMutex<Option<String>>>,
}

#[derive(Clone)]
struct AdmissionProvenanceProvider {
    model: Arc<StdMutex<String>>,
    override_tier: Arc<StdMutex<Option<Option<String>>>>,
    service_tier: Arc<StdMutex<Option<String>>>,
    observations: Arc<StdMutex<Vec<(String, Option<Option<String>>)>>>,
    complete_entries: Option<Arc<StdMutex<Vec<(String, Option<Option<String>>)>>>>,
    complete_responses: Option<Arc<StdMutex<VecDeque<Vec<StreamEvent>>>>>,
}

impl AdmissionProvenanceProvider {
    fn new(model: &str) -> Self {
        Self {
            model: Arc::new(StdMutex::new(model.to_string())),
            override_tier: Arc::new(StdMutex::new(None)),
            service_tier: Arc::new(StdMutex::new(None)),
            observations: Arc::new(StdMutex::new(Vec::new())),
            complete_entries: None,
            complete_responses: None,
        }
    }

    fn with_complete_recording(
        mut self,
    ) -> (
        Self,
        Arc<StdMutex<Vec<(String, Option<Option<String>>)>>>,
    ) {
        let complete_entries = Arc::new(StdMutex::new(Vec::new()));
        let complete_responses = Arc::new(StdMutex::new(VecDeque::from([
            vec![
                StreamEvent::TextDelta("managed admission".to_string()),
                StreamEvent::MessageEnd { stop_reason: None },
            ],
            vec![
                StreamEvent::TextDelta("managed final".to_string()),
                StreamEvent::MessageEnd { stop_reason: None },
            ],
        ])));
        self.complete_entries = Some(Arc::clone(&complete_entries));
        self.complete_responses = Some(complete_responses);
        (self, complete_entries)
    }
}

#[async_trait]
impl Provider for AdmissionProvenanceProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        let prompt = messages
            .iter()
            .rev()
            .flat_map(|message| message.content.iter().rev())
            .find_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        if let Some(entries) = &self.complete_entries {
            entries
                .lock()
                .expect("admission complete entries lock")
                .push((
                    prompt,
                    self.override_tier
                        .lock()
                        .expect("admission override lock")
                        .clone(),
                ));
        }
        let events = self
            .complete_responses
            .as_ref()
            .and_then(|responses| {
                responses
                    .lock()
                    .expect("admission complete responses lock")
                    .pop_front()
            })
            .unwrap_or_default();
        Ok(Box::pin(stream::iter(
            events.into_iter().map(Ok::<_, anyhow::Error>),
        )))
    }

    fn name(&self) -> &str {
        "admission-provenance-test"
    }

    fn model(&self) -> String {
        self.model.lock().expect("admission model lock").clone()
    }

    fn set_service_tier(&self, service_tier: &str) -> anyhow::Result<()> {
        *self
            .service_tier
            .lock()
            .expect("admission service tier lock") = Some(service_tier.to_string());
        Ok(())
    }

    fn set_model(&self, model: &str) -> anyhow::Result<()> {
        *self.model.lock().expect("admission model lock") = model.trim().to_string();
        Ok(())
    }

    fn service_tier(&self) -> Option<String> {
        self.service_tier
            .lock()
            .expect("admission service tier lock")
            .clone()
    }

    fn supports_scoped_service_tier_override(&self) -> bool {
        true
    }

    fn set_scoped_service_tier_override(
        &self,
        override_tier: Option<Option<String>>,
    ) -> anyhow::Result<()> {
        *self
            .override_tier
            .lock()
            .expect("admission override lock") = override_tier.clone();
        self.observations
            .lock()
            .expect("admission observations lock")
            .push((self.model(), override_tier));
        Ok(())
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            model: Arc::new(StdMutex::new(self.model())),
            override_tier: Arc::new(StdMutex::new(None)),
            service_tier: Arc::new(StdMutex::new(self.service_tier())),
            observations: Arc::clone(&self.observations),
            complete_entries: self.complete_entries.clone(),
            complete_responses: self.complete_responses.clone(),
        })
    }
}

struct AdmissionTestHome {
    previous: Option<std::ffi::OsString>,
    _home: tempfile::TempDir,
}

impl AdmissionTestHome {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("create admission test home");
        let previous = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", home.path());
        crate::config::invalidate_config_cache();
        Self {
            previous,
            _home: home,
        }
    }
}

impl Drop for AdmissionTestHome {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => crate::env::set_var("JCODE_HOME", value),
            None => crate::env::remove_var("JCODE_HOME"),
        }
        crate::config::invalidate_config_cache();
    }
}

impl ScopedFastPolicyProvider {
    fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            override_tier: Arc::new(StdMutex::new(None)),
            service_tier: Arc::new(StdMutex::new(None)),
        }
    }
}

#[async_trait]
impl Provider for ScopedFastPolicyProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        Ok(Box::pin(stream::empty()))
    }

    fn name(&self) -> &str {
        "scoped-fast-policy-test"
    }

    fn model(&self) -> String {
        self.model.clone()
    }

    fn service_tier(&self) -> Option<String> {
        self.service_tier
            .lock()
            .expect("service tier lock")
            .clone()
    }

    fn set_service_tier(&self, service_tier: &str) -> anyhow::Result<()> {
        *self.service_tier.lock().expect("service tier lock") =
            Some(service_tier.to_string());
        Ok(())
    }

    fn supports_scoped_service_tier_override(&self) -> bool {
        true
    }

    fn set_scoped_service_tier_override(
        &self,
        override_tier: Option<Option<String>>,
    ) -> anyhow::Result<()> {
        *self.override_tier.lock().expect("scoped override lock") = override_tier;
        Ok(())
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[test]
fn scoped_worker_policy_checks_route_scope_before_ownership() {
    let web_provider = ScopedFastPolicyProvider::new("gpt-6-astra[web]");
    let web_session = crate::session::Session::create_with_origin(
        None,
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    assert!(!apply_runtime_fast_policy(&web_provider, &web_session, None)
        .expect("Web worker route should be a harmless no-op without ownership"));
    assert_eq!(
        *web_provider
            .override_tier
            .lock()
            .expect("scoped override lock"),
        None
    );

    let native_provider = ScopedFastPolicyProvider::new("gpt-5.6-luna");
    let native_session = crate::session::Session::create_with_origin(
        None,
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    let error = apply_runtime_fast_policy(&native_provider, &native_session, None)
        .expect_err("native worker route must fail closed without provenance");
    assert!(error.to_string().contains("missing authorized parent provenance"));
}

#[tokio::test]
async fn busy_root_nested_provenance() {
    let _storage_guard = crate::storage::lock_test_env();
    let _home = AdmissionTestHome::new();
    let root_provider = Arc::new(AdmissionProvenanceProvider::new("gpt-5.6-luna"));
    let root_provider_dyn: Arc<dyn Provider> = root_provider.clone();
    let root_id = "busy-root-nested-provenance";
    let root_registry = crate::tool::Registry::new(Arc::clone(&root_provider_dyn)).await;
    let root_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&root_provider_dyn),
        root_registry,
        crate::session::Session::create_with_id(root_id.to_string(), None, None),
        None,
    )));
    let root_state = crate::server::RuntimeFastState::from_provider(
        root_id,
        root_provider.as_ref(),
    );
    let sessions: crate::server::SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        root_id.to_string(),
        crate::server::SessionAgentEntry::new(Arc::clone(&root_agent), Arc::clone(&root_state)),
    )])));
    let global_session_id = Arc::new(RwLock::new(String::new()));
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::new()));
    let swarm_plans = Arc::new(RwLock::new(HashMap::<String, crate::server::VersionedPlan>::new()));
    let soft_interrupt_queues = Arc::new(RwLock::new(HashMap::new()));
    let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());

    // This is the real caller-lock condition. Both headless admissions must
    // make progress without awaiting the owning root Agent mutex.
    let _root_agent_guard = root_agent.lock().await;
    let direct_response = tokio::time::timeout(
        Duration::from_secs(2),
        headless::create_headless_session(
            &sessions,
            &global_session_id,
            &root_provider_dyn,
            "create_session:/matrix-b-direct",
            &swarm_members,
            &swarms_by_id,
            &swarm_coordinators,
            &swarm_plans,
            &soft_interrupt_queues,
            false,
            Some("gpt-5.6-luna".to_string()),
            None,
            None,
            None,
            Some(Arc::clone(&mcp_pool)),
            Some(root_id.to_string()),
            HeadlessMemoryScope::RealProject,
            crate::session::SessionOrigin::SwarmWorker,
            Some(Arc::clone(&root_state)),
        ),
    )
    .await
    .expect("direct worker admission must not await the busy root")
    .expect("direct worker admission should succeed");
    let direct_id = serde_json::from_str::<serde_json::Value>(&direct_response)
        .expect("direct response JSON")
        ["session_id"]
        .as_str()
        .expect("direct session id")
        .to_string();
    let direct_state = sessions
        .read()
        .await
        .get(&direct_id)
        .expect("direct worker should be resident")
        .fast_state();
    assert!(Arc::ptr_eq(&direct_state, &root_state));

    let nested_response = tokio::time::timeout(
        Duration::from_secs(2),
        headless::create_headless_session(
            &sessions,
            &global_session_id,
            &root_provider_dyn,
            "create_session:/matrix-b-nested",
            &swarm_members,
            &swarms_by_id,
            &swarm_coordinators,
            &swarm_plans,
            &soft_interrupt_queues,
            false,
            Some("gpt-6-astra".to_string()),
            None,
            None,
            None,
            Some(Arc::clone(&mcp_pool)),
            Some(direct_id.clone()),
            HeadlessMemoryScope::RealProject,
            crate::session::SessionOrigin::SwarmWorker,
            Some(Arc::clone(&direct_state)),
        ),
    )
    .await
    .expect("nested worker admission must not await the busy root")
    .expect("nested worker admission should succeed");
    let nested_id = serde_json::from_str::<serde_json::Value>(&nested_response)
        .expect("nested response JSON")
        ["session_id"]
        .as_str()
        .expect("nested session id")
        .to_string();
    let nested_entry = sessions
        .read()
        .await
        .get(&nested_id)
        .cloned()
        .expect("nested worker should be resident");
    assert!(Arc::ptr_eq(&nested_entry.fast_state(), &root_state));
    assert_eq!(nested_entry.fast_state().owner_root_session_id(), root_id);
    {
        let direct_guard = sessions
            .read()
            .await
            .get(&direct_id)
            .expect("direct worker entry")
            .agent();
        let direct_guard = direct_guard.lock().await;
        assert_eq!(direct_guard.session_for_split().parent_id.as_deref(), Some(root_id));
        assert_eq!(direct_guard.provider_model(), "gpt-5.6-luna");
    }
    {
        let nested_guard = nested_entry.agent();
        let nested_guard = nested_guard.lock().await;
        assert_eq!(nested_guard.session_for_split().parent_id.as_deref(), Some(direct_id.as_str()));
        assert_eq!(nested_guard.provider_model(), "gpt-6-astra");
    }

    assert_eq!(
        root_provider
            .observations
            .lock()
            .expect("admission observations lock")
            .as_slice(),
        &[
            (
                "gpt-5.6-luna".to_string(),
                Some(Some("priority".to_string()))
            ),
            ("gpt-6-astra".to_string(), Some(None)),
        ]
    );

    let missing_owner = crate::session::Session::create_with_origin(
        Some(root_id.to_string()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    let missing_owner_provider = AdmissionProvenanceProvider::new("gpt-5.6-luna");
    let missing_owner_error = apply_runtime_fast_policy(
        &missing_owner_provider,
        &missing_owner,
        None,
    )
    .expect_err("native worker admission must reject a missing owner state");
    assert!(missing_owner_error.to_string().contains("not resident"));

    let mut mismatched = crate::session::Session::create_with_origin(
        Some(root_id.to_string()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    mismatched.model = Some("gpt-5.6-luna".to_string());
    let mismatched_provider = AdmissionProvenanceProvider::new("gpt-5.6-luna");
    let mismatched_state = crate::server::RuntimeFastState::from_provider(
        mismatched.id.clone(),
        &mismatched_provider,
    );
    let mismatched_error = apply_runtime_fast_policy(
        &mismatched_provider,
        &mismatched,
        Some(&mismatched_state),
    )
    .expect_err("native worker admission must reject self-owned mismatched state");
    assert!(mismatched_error.to_string().contains("is invalid"));

    let mut retired = crate::session::Session::create_with_origin(
        Some(root_id.to_string()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    retired.model = Some("gpt-6-astra".to_string());
    let retired_provider = AdmissionProvenanceProvider::new("gpt-6-astra");
    let retired_state = crate::server::RuntimeFastState::from_provider(root_id, &retired_provider);
    retired_state.invalidate();
    let retired_error = apply_runtime_fast_policy(
        &retired_provider,
        &retired,
        Some(&retired_state),
    )
    .expect_err("native worker admission must reject retired owner state");
    assert!(retired_error.to_string().contains("is invalid"));

    let mut missing_parent = crate::session::Session::create_with_origin(
        None,
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    missing_parent.model = Some("gpt-6-astra".to_string());
    let missing_parent_provider = AdmissionProvenanceProvider::new("gpt-6-astra");
    let missing_parent_error = apply_runtime_fast_policy(
        &missing_parent_provider,
        &missing_parent,
        Some(&root_state),
    )
    .expect_err("native worker admission must reject missing parent provenance");
    assert!(missing_parent_error
        .to_string()
        .contains("missing authorized parent provenance"));

}

#[tokio::test]
async fn busy_root_real_spawn_ingress_preserves_parent_and_fast_state() {
    let _storage_guard = crate::storage::lock_test_env();
    let _home = AdmissionTestHome::new();
    let root_provider = Arc::new(AdmissionProvenanceProvider::new("gpt-5.6-luna"));
    let root_provider_dyn: Arc<dyn Provider> = root_provider.clone();
    let root_id = "busy-root-real-spawn-ingress";
    let root_registry = crate::tool::Registry::new(Arc::clone(&root_provider_dyn)).await;
    let root_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&root_provider_dyn),
        root_registry,
        crate::session::Session::create_with_id(root_id.to_string(), None, None),
        None,
    )));
    let root_state = crate::server::RuntimeFastState::from_provider(
        root_id,
        root_provider.as_ref(),
    );
    let sessions: crate::server::SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        root_id.to_string(),
        crate::server::SessionAgentEntry::new(Arc::clone(&root_agent), Arc::clone(&root_state)),
    )])));
    let global_session_id = Arc::new(RwLock::new(String::new()));
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::new()));
    let swarm_plans = Arc::new(RwLock::new(HashMap::<String, crate::server::VersionedPlan>::new()));
    let event_history = Arc::new(RwLock::new(std::collections::VecDeque::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _) = tokio::sync::broadcast::channel(16);
    let soft_interrupt_queues = Arc::new(RwLock::new(HashMap::new()));
    let client_connections = Arc::new(RwLock::new(HashMap::new()));
    let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());
    let working_dir = std::env::current_dir()
        .expect("working directory")
        .to_string_lossy()
        .into_owned();

    let _root_agent_guard = root_agent.lock().await;
    let direct_id = comm_session::spawn_swarm_agent(
        root_id,
        "matrix-b-real-ingress",
        Some(working_dir.clone()),
        None,
        Some(crate::config::SwarmSpawnMode::Headless),
        Some("gpt-5.6-luna".to_string()),
        Some("medium".to_string()),
        Some("matrix-b-direct".to_string()),
        &sessions,
        &global_session_id,
        &root_provider_dyn,
        &swarm_members,
        &swarms_by_id,
        &swarm_coordinators,
        &swarm_plans,
        &event_history,
        &event_counter,
        &swarm_event_tx,
        &mcp_pool,
        &soft_interrupt_queues,
        &client_connections,
    )
    .await
    .expect("direct spawn ingress should succeed while root is busy");
    let direct_entry = sessions
        .read()
        .await
        .get(&direct_id)
        .cloned()
        .expect("direct spawned worker entry");
    assert!(Arc::ptr_eq(&direct_entry.fast_state(), &root_state));
    {
        let direct_agent = direct_entry.agent();
        let direct = direct_agent.lock().await;
        assert_eq!(
            direct.session_for_split().parent_id.as_deref(),
            Some(root_id)
        );
        assert_eq!(direct.provider_model(), "openai:gpt-5.6-luna");
    }

    let nested_id = comm_session::spawn_swarm_agent(
        &direct_id,
        "matrix-b-real-ingress",
        Some(working_dir),
        None,
        Some(crate::config::SwarmSpawnMode::Headless),
        Some("gpt-6-astra".to_string()),
        Some("high".to_string()),
        Some("matrix-b-nested".to_string()),
        &sessions,
        &global_session_id,
        &root_provider_dyn,
        &swarm_members,
        &swarms_by_id,
        &swarm_coordinators,
        &swarm_plans,
        &event_history,
        &event_counter,
        &swarm_event_tx,
        &mcp_pool,
        &soft_interrupt_queues,
        &client_connections,
    )
    .await
    .expect("nested spawn ingress should succeed from direct worker");
    let nested_entry = sessions
        .read()
        .await
        .get(&nested_id)
        .cloned()
        .expect("nested spawned worker entry");
    assert!(Arc::ptr_eq(&nested_entry.fast_state(), &root_state));
    assert_eq!(nested_entry.fast_state().owner_root_session_id(), root_id);
    {
        let nested_agent = nested_entry.agent();
        let nested = nested_agent.lock().await;
        assert_eq!(
            nested.session_for_split().parent_id.as_deref(),
            Some(direct_id.as_str())
        );
        assert_eq!(nested.provider_model(), "openai:gpt-6-astra");
    }
}

#[tokio::test]
async fn lifecycle_policy_refresh_reaches_fresh_headless_admission() {
    let _storage_guard = crate::storage::lock_test_env();
    let _home = AdmissionTestHome::new();
    let root_provider = Arc::new(AdmissionProvenanceProvider::new("gpt-5.6-luna"));
    let root_provider_dyn: Arc<dyn Provider> = root_provider.clone();
    let root_id = "matrix-c-lifecycle-root";
    let root_registry = crate::tool::Registry::new(Arc::clone(&root_provider_dyn)).await;
    let root_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&root_provider_dyn),
        root_registry,
        crate::session::Session::create_with_id(root_id.to_string(), None, None),
        None,
    )));
    let root_state = crate::server::RuntimeFastState::from_provider(
        root_id,
        root_provider.as_ref(),
    );
    let sessions: crate::server::SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        root_id.to_string(),
        crate::server::SessionAgentEntry::new(Arc::clone(&root_agent), Arc::clone(&root_state)),
    )])));
    let global_session_id = Arc::new(RwLock::new(String::new()));
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::new()));
    let swarm_plans = Arc::new(RwLock::new(HashMap::<String, crate::server::VersionedPlan>::new()));
    let soft_interrupt_queues = Arc::new(RwLock::new(HashMap::new()));
    let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    provider_control::handle_set_service_tier(
        1,
        "priority".to_string(),
        &root_agent,
        Some(Arc::clone(&root_state)),
        &event_tx,
    )
    .await;
    assert!(matches!(
        event_rx.try_recv(),
        Ok(ServerEvent::ServiceTierChanged {
            id: 1,
            error: None,
            ..
        })
    ));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Priority);

    let worker_response = headless::create_headless_session(
        &sessions,
        &global_session_id,
        &root_provider_dyn,
        "create_session:/matrix-c-native",
        &swarm_members,
        &swarms_by_id,
        &swarm_coordinators,
        &swarm_plans,
        &soft_interrupt_queues,
        false,
        Some("gpt-6-astra".to_string()),
        None,
        None,
        None,
        Some(Arc::clone(&mcp_pool)),
        Some(root_id.to_string()),
        HeadlessMemoryScope::RealProject,
        crate::session::SessionOrigin::SwarmWorker,
        Some(Arc::clone(&root_state)),
    )
    .await
    .expect("fresh native headless admission should succeed");
    let native_id = serde_json::from_str::<serde_json::Value>(&worker_response)
        .expect("native worker response JSON")["session_id"]
        .as_str()
        .expect("native worker session id")
        .to_string();
    let native_entry = sessions
        .read()
        .await
        .get(&native_id)
        .cloned()
        .expect("native worker should be resident");
    assert!(Arc::ptr_eq(&native_entry.fast_state(), &root_state));
    assert_eq!(
        root_provider
            .observations
            .lock()
            .expect("admission observations lock")
            .last(),
        Some(&("gpt-6-astra".to_string(), Some(Some("priority".to_string()))))
    );

    let off_event_tx = event_tx.clone();
    provider_control::handle_set_service_tier(
        2,
        "off".to_string(),
        &root_agent,
        Some(Arc::clone(&root_state)),
        &off_event_tx,
    )
    .await;
    assert!(matches!(
        event_rx.try_recv(),
        Ok(ServerEvent::ServiceTierChanged {
            id: 2,
            error: None,
            ..
        })
    ));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);

    let unrelated_response = headless::create_headless_session(
        &sessions,
        &global_session_id,
        &root_provider_dyn,
        "create_session:/matrix-c-unrelated",
        &swarm_members,
        &swarms_by_id,
        &swarm_coordinators,
        &swarm_plans,
        &soft_interrupt_queues,
        false,
        Some("offline-analyst".to_string()),
        None,
        None,
        None,
        Some(Arc::clone(&mcp_pool)),
        Some(root_id.to_string()),
        HeadlessMemoryScope::RealProject,
        crate::session::SessionOrigin::SwarmWorker,
        Some(Arc::clone(&root_state)),
    )
    .await
    .expect("unrelated headless admission should succeed");
    let _unrelated_id = serde_json::from_str::<serde_json::Value>(&unrelated_response)
        .expect("unrelated worker response JSON")["session_id"]
        .as_str()
        .expect("unrelated worker session id");
    assert_eq!(
        root_provider
            .observations
            .lock()
            .expect("admission observations lock")
            .last(),
        Some(&("offline-analyst".to_string(), None))
    );

    let restored_response = headless::create_headless_session(
        &sessions,
        &global_session_id,
        &root_provider_dyn,
        "create_session:/matrix-c-restored",
        &swarm_members,
        &swarms_by_id,
        &swarm_coordinators,
        &swarm_plans,
        &soft_interrupt_queues,
        false,
        Some("gpt-6-astra".to_string()),
        None,
        None,
        None,
        Some(Arc::clone(&mcp_pool)),
        Some(root_id.to_string()),
        HeadlessMemoryScope::RealProject,
        crate::session::SessionOrigin::SwarmWorker,
        Some(Arc::clone(&root_state)),
    )
    .await
    .expect("restored native headless admission should succeed");
    let _restored_id = serde_json::from_str::<serde_json::Value>(&restored_response)
        .expect("restored worker response JSON")["session_id"]
        .as_str()
        .expect("restored worker session id");
    assert_eq!(
        root_provider
            .observations
            .lock()
            .expect("admission observations lock")
            .last(),
        Some(&("gpt-6-astra".to_string(), Some(None)))
    );
}

#[tokio::test]
async fn managed_analyst_reuse_refreshes_runtime_fast_tier() {
    let _storage_guard = crate::storage::lock_test_env();
    let _home = AdmissionTestHome::new();
    let root_id = "managed-fast-root";
    let root_provider = Arc::new(AdmissionProvenanceProvider::new("gpt-5.6-luna"));
    let root_provider_dyn: Arc<dyn Provider> = root_provider.clone();
    let root_registry = crate::tool::Registry::new(Arc::clone(&root_provider_dyn)).await;
    let root_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&root_provider_dyn),
        root_registry,
        crate::session::Session::create_with_id(root_id.to_string(), None, None),
        None,
    )));
    let root_state = crate::server::RuntimeFastState::from_provider(
        root_id,
        root_provider.as_ref(),
    );

    let (analyst_provider, complete_entries) =
        AdmissionProvenanceProvider::new("gpt-6-astra").with_complete_recording();
    let analyst_provider = Arc::new(analyst_provider);
    let analyst_provider_dyn: Arc<dyn Provider> = analyst_provider.clone();
    let mut analyst_session = crate::session::Session::create_with_origin(
        Some(root_id.to_string()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    analyst_session.id = "managed-fast-analyst".to_string();
    analyst_session.provider_key = Some("openai-oauth".to_string());
    analyst_session.route_api_method = Some("openai-oauth".to_string());
    let analyst_id = analyst_session.id.clone();
    let analyst_registry = crate::tool::Registry::new(Arc::clone(&analyst_provider_dyn)).await;
    let analyst = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&analyst_provider_dyn),
        analyst_registry,
        analyst_session,
        None,
    )));
    {
        let analyst_guard = analyst.lock().await;
        let provider_handle = analyst_guard.provider_handle();
        let provider_model = analyst_guard.provider_model();
        let provider_key = analyst_guard.session_provider_key();
        let route = analyst_guard.session_route_api_method();
        let session_model = analyst_guard.session_for_split().model.clone();
        assert!(
            Arc::ptr_eq(&provider_handle, &analyst_provider_dyn),
            "managed provider identity mismatch: model={provider_model:?} session_model={session_model:?} provider_key={provider_key:?} route={route:?}"
        );
        assert_eq!(
            provider_model, "gpt-6-astra",
            "managed provider model mismatch: session_model={session_model:?} provider_key={provider_key:?} route={route:?}"
        );
        assert_eq!(
            session_model.as_deref(),
            Some("gpt-6-astra"),
            "managed session model mismatch: provider_model={provider_model:?} provider_key={provider_key:?} route={route:?}"
        );
        assert_eq!(
            provider_key.as_deref(),
            Some("openai-oauth"),
            "managed provider key mismatch: provider_model={provider_model:?} session_model={session_model:?} route={route:?}"
        );
        assert_eq!(
            route.as_deref(),
            Some("openai-oauth"),
            "managed route mismatch: provider_model={provider_model:?} session_model={session_model:?} provider_key={provider_key:?}"
        );
    }
    let sessions: crate::server::SessionAgents = Arc::new(RwLock::new(HashMap::from([
        (
            root_id.to_string(),
            crate::server::SessionAgentEntry::new(
                Arc::clone(&root_agent),
                Arc::clone(&root_state),
            ),
        ),
        (
            analyst_id.clone(),
            crate::server::SessionAgentEntry::new(
                Arc::clone(&analyst),
                Arc::clone(&root_state),
            ),
        ),
    ])));
    let state = test_state(true);
    let (analyst_identity, analyst_child) = {
        let analyst_guard = analyst.lock().await;
        (
            AnalystIdentity {
                model: analyst_guard.provider_model(),
                route: analyst_guard.session_route_api_method(),
                effort: analyst_guard.provider_reasoning_effort(),
            },
            SessionControlHandle::new(
                analyst_guard.session_id(),
                analyst_guard.soft_interrupt_queue(),
                analyst_guard.background_tool_signal(),
                analyst_guard.graceful_shutdown_signal(),
            ),
        )
    };
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();

    provider_control::handle_set_service_tier(
        1,
        "priority".to_string(),
        &root_agent,
        Some(Arc::clone(&root_state)),
        &event_tx,
    )
    .await;
    assert!(matches!(
        event_rx.try_recv(),
        Ok(ServerEvent::ServiceTierChanged {
            id: 1,
            error: None,
            ..
        })
    ));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Priority);

    let generation = try_begin_generation(&state)
        .await
        .expect("managed analyst priority generation should be admitted");
    let first = run_managed_analyst_turn(
        Arc::clone(&analyst),
        analyst_child.clone(),
        "MANAGED-FAST-1 priority analyst turn",
        Vec::new(),
        &state,
        &sessions,
        generation,
        &analyst_identity,
    )
    .await
    .expect("managed analyst priority turn should complete");
    assert_eq!(first, "managed admission");
    assert!(state.lock().await.active_child.is_none());
    finish_generation(&state, generation).await;

    provider_control::handle_set_service_tier(
        2,
        "off".to_string(),
        &root_agent,
        Some(Arc::clone(&root_state)),
        &event_tx,
    )
    .await;
    assert!(matches!(
        event_rx.try_recv(),
        Ok(ServerEvent::ServiceTierChanged {
            id: 2,
            error: None,
            ..
        })
    ));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);

    let next_generation = try_begin_generation(&state)
        .await
        .expect("managed analyst ordinary generation should be admitted");
    let second = run_managed_analyst_turn(
        Arc::clone(&analyst),
        analyst_child,
        "MANAGED-FAST-1 ordinary analyst turn",
        Vec::new(),
        &state,
        &sessions,
        next_generation,
        &analyst_identity,
    )
    .await
    .expect("managed analyst ordinary turn should complete");
    assert_eq!(second, "managed final");
    assert!(state.lock().await.active_child.is_none());
    finish_generation(&state, next_generation).await;

    let entries = complete_entries
        .lock()
        .expect("managed analyst complete entries lock")
        .clone();
    assert_eq!(entries.len(), 2);
    assert!(entries[0].0.contains("priority analyst turn"));
    assert_eq!(entries[0].1, Some(Some("priority".to_string())));
    assert!(entries[1].0.contains("ordinary analyst turn"));
    assert_eq!(entries[1].1, Some(None));

    let resident = sessions
        .read()
        .await
        .get(&analyst_id)
        .cloned()
        .expect("managed analyst remains resident");
    assert!(Arc::ptr_eq(&resident.agent(), &analyst));
    assert!(Arc::ptr_eq(&resident.fast_state(), &root_state));
    assert!(state.lock().await.active_child.is_none());
}

#[test]
fn runtime_fast_state_retirement_fences_late_publishers_and_children() {
    let provider = ScopedFastPolicyProvider::new("gpt-5.6-luna");
    let state = crate::server::RuntimeFastState::from_provider("root", &provider);
    assert!(state.snapshot().valid);
    assert!(!state.snapshot().retired);

    // Model both a retained deferred setter and a later provider refresh using
    // the same Arc that was captured before the root was removed.
    state.invalidate();
    state.publish(crate::server::RuntimeFastTier::Priority);
    state.publish_from_provider(&provider);
    let retired = state.snapshot();
    assert!(!retired.valid, "late publication must not revive a retired root");
    assert!(retired.retired, "retirement must be terminal for this Arc");

    let mut worker = crate::session::Session::create_with_origin(
        Some("root".to_string()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    worker.model = Some("gpt-6-astra".to_string());
    let error = apply_runtime_fast_policy(&provider, &worker, Some(&state))
        .expect_err("children of a retired root must fail closed");
    assert!(error.to_string().contains("owning root root is invalid"));

    // Capability-invalid is recoverable while the state is live, unlike
    // terminal retirement. A replacement gets a fresh valid state and cannot
    // revive the old Arc.
    let unsupported = OfflineAnalystProvider {
        responses: Arc::new(StdMutex::new(VecDeque::new())),
    };
    let live = crate::server::RuntimeFastState::from_provider("live", &unsupported);
    assert!(!live.snapshot().valid);
    assert!(!live.snapshot().retired);
    live.publish_from_provider(&provider);
    assert!(live.snapshot().valid);
    assert!(!live.snapshot().retired);
    let replacement = crate::server::RuntimeFastState::from_provider("root", &provider);
    assert!(replacement.snapshot().valid);
    assert!(!state.snapshot().valid);
    assert!(state.snapshot().retired);
}

#[tokio::test]
async fn actual_deferred_service_tier_cannot_revive_removed_owner() {
    let provider = Arc::new(ScopedFastPolicyProvider::new("gpt-5.6-luna"));
    let provider_dyn: Arc<dyn Provider> = provider.clone();
    let registry = crate::tool::Registry::new(Arc::clone(&provider_dyn)).await;
    let root_id = "deferred-retired-root";
    let session = crate::session::Session::create_with_id(root_id.to_string(), None, None);
    let agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&provider_dyn),
        registry,
        session,
        None,
    )));
    let state = crate::server::RuntimeFastState::from_provider(root_id, provider.as_ref());
    let sessions: crate::server::SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        root_id.to_string(),
        crate::server::SessionAgentEntry::new(Arc::clone(&agent), Arc::clone(&state)),
    )])));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    // Holding the real Agent mutex forces handle_set_service_tier onto its real
    // deferred path. Owner retirement/removal occurs before releasing the mutex.
    let busy_agent = agent.lock().await;
    provider_control::handle_set_service_tier(
        901,
        "priority".to_string(),
        &agent,
        Some(Arc::clone(&state)),
        &client_event_tx,
    )
    .await;
    assert!(client_event_rx.try_recv().is_err());

    let removed = remove_session_agent_entry(&sessions, root_id)
        .await
        .expect("owner entry should be removed");
    assert!(Arc::ptr_eq(&removed.fast_state(), &state));
    assert!(state.snapshot().retired);

    drop(busy_agent);

    let event = tokio::time::timeout(Duration::from_secs(1), client_event_rx.recv())
        .await
        .expect("deferred setter should complete after Agent release")
        .expect("deferred setter event channel should remain open");
    assert!(matches!(
        event,
        ServerEvent::ServiceTierChanged {
            id: 901,
            error: None,
            ..
        }
    ));
    assert!(state.snapshot().retired);
    assert!(!state.snapshot().valid);

    let mut child = crate::session::Session::create_with_origin(
        Some(root_id.to_string()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    child.model = Some("gpt-6-astra".to_string());
    let error = apply_runtime_fast_policy(provider.as_ref(), &child, Some(&state))
        .expect_err("native child of removed owner must fail closed");
    assert!(error.to_string().contains("owning root deferred-retired-root is invalid"));
}

#[tokio::test]
async fn actual_root_and_worker_service_tier_publication_isolated_for_astra() {
    let root_provider = Arc::new(ScopedFastPolicyProvider::new("gpt-5.6-luna"));
    let root_provider_dyn: Arc<dyn Provider> = root_provider.clone();
    let root_registry = crate::tool::Registry::new(Arc::clone(&root_provider_dyn)).await;
    let root_id = "actual-fast-root";
    let root_session = crate::session::Session::create_with_id(root_id.to_string(), None, None);
    let root_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&root_provider_dyn),
        root_registry,
        root_session,
        None,
    )));

    let worker_provider = Arc::new(ScopedFastPolicyProvider::new("gpt-5.6-luna"));
    let worker_provider_dyn: Arc<dyn Provider> = worker_provider.clone();
    let worker_registry = crate::tool::Registry::new(Arc::clone(&worker_provider_dyn)).await;
    let mut worker_session = crate::session::Session::create_with_origin(
        Some(root_id.to_string()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    worker_session.model = Some("gpt-5.6-luna".to_string());
    let worker_id = worker_session.id.clone();
    let worker_agent = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&worker_provider_dyn),
        worker_registry,
        worker_session,
        None,
    )));

    let root_state = crate::server::RuntimeFastState::from_provider(root_id, root_provider.as_ref());
    let sessions: crate::server::SessionAgents = Arc::new(RwLock::new(HashMap::from([
        (
            root_id.to_string(),
            crate::server::SessionAgentEntry::new(Arc::clone(&root_agent), Arc::clone(&root_state)),
        ),
        (
            worker_id.clone(),
            crate::server::SessionAgentEntry::new(Arc::clone(&worker_agent), Arc::clone(&root_state)),
        ),
    ])));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    let root_fast_state = Arc::clone(&root_state);
    provider_control::handle_set_service_tier(
        902,
        "off".to_string(),
        &root_agent,
        Some(root_fast_state),
        &client_event_tx,
    )
    .await;
    let worker_fast_state = {
        let sessions_guard = sessions.read().await;
        let worker_entry = sessions_guard
            .get(&worker_id)
            .expect("worker entry should be resident");
        assert_eq!(
            worker_entry.fast_state.owner_root_session_id(),
            root_id,
            "worker must retain the owning root handle"
        );
        None
    };
    assert!(worker_fast_state.is_none(), "worker must not publish root state");
    provider_control::handle_set_service_tier(
        903,
        "priority".to_string(),
        &worker_agent,
        worker_fast_state,
        &client_event_tx,
    )
    .await;

    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::ServiceTierChanged { id: 902, error: None, .. })
    ));
    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::ServiceTierChanged { id: 903, error: None, .. })
    ));
    assert_eq!(root_provider.service_tier().as_deref(), Some("off"));
    assert_eq!(worker_provider.service_tier().as_deref(), Some("priority"));
    assert_eq!(root_state.snapshot().tier, crate::server::RuntimeFastTier::Ordinary);

    let astra_provider = ScopedFastPolicyProvider::new("gpt-6-astra");
    let mut astra_child = crate::session::Session::create_with_origin(
        Some(root_id.to_string()),
        None,
        crate::session::SessionOrigin::SwarmWorker,
    );
    astra_child.model = Some("gpt-6-astra".to_string());
    assert!(apply_runtime_fast_policy(&astra_provider, &astra_child, Some(&root_state))
        .expect("Astra child should apply the owning root snapshot"));
    assert_eq!(
        *astra_provider.override_tier.lock().expect("scoped override lock"),
        Some(None)
    );
}

fn test_state(enabled: bool) -> AstraFirstStateHandle {
    Arc::new(Mutex::new(AstraFirstState {
        session_id: "root-test".to_string(),
        generation: 0,
        analyst_id: None,
        analyst_model: None,
        analyst_route: None,
        analyst_effort: None,
        busy: false,
        cancelled: false,
        active_child: None,
        cancel_notify: Arc::new(tokio::sync::Notify::new()),
        enabled,
    }))
}

type DirectReleaseLatch = Arc<(StdMutex<bool>, Condvar)>;

fn direct_release_latch() -> DirectReleaseLatch {
    Arc::new((StdMutex::new(false), Condvar::new()))
}

fn wait_for_direct_release(latch: &DirectReleaseLatch) {
    let (released, condvar) = &**latch;
    let mut released = released.lock().expect("direct-release latch mutex");
    while !*released {
        released = condvar
            .wait(released)
            .expect("direct-release latch mutex after wait");
    }
}

struct DirectReleaseTestReleaseGuard {
    latch: DirectReleaseLatch,
}

impl DirectReleaseTestReleaseGuard {
    fn new(latch: DirectReleaseLatch) -> Self {
        Self { latch }
    }

    fn release(&self) {
        let (released, condvar) = &*self.latch;
        let mut released = released.lock().expect("direct-release latch mutex");
        if !*released {
            *released = true;
            condvar.notify_all();
        }
    }
}

impl Drop for DirectReleaseTestReleaseGuard {
    fn drop(&mut self) {
        self.release();
    }
}

#[test]
fn proxy_withholds_root_draft_and_reasoning_but_forwards_progress() {
    let mut draft = String::new();
    assert!(proxy_root_event(
        &ServerEvent::TextDelta {
            text: "draft".to_string(),
        },
        &mut draft,
    )
    .is_none());
    assert_eq!(draft, "draft");
    assert!(proxy_root_event(
        &ServerEvent::TextReplace {
            text: "replaced".to_string(),
        },
        &mut draft,
    )
    .is_none());
    assert_eq!(draft, "replaced");
    assert!(proxy_root_event(
        &ServerEvent::ReasoningDelta {
            text: "hidden reasoning".to_string(),
        },
        &mut draft,
    )
    .is_none());
    assert!(proxy_root_event(
        &ServerEvent::ReasoningDone {
            duration_secs: None
        },
        &mut draft
    )
    .is_none());
    assert!(matches!(
        proxy_root_event(
            &ServerEvent::ToolStart {
                id: "tool-1".to_string(),
                name: "status".to_string(),
            },
            &mut draft,
        ),
        Some(ServerEvent::ToolStart { id, name }) if id == "tool-1" && name == "status"
    ));
    assert!(
        proxy_root_event(&ServerEvent::MessageEnd { stop_reason: None }, &mut draft,).is_none()
    );
}

#[test]
fn retry_rollback_clears_the_withheld_draft() {
    let mut draft = "partial answer".to_string();
    let event = proxy_root_event(
        &ServerEvent::RetryRollback { attempt: 2, max: 3 },
        &mut draft,
    );
    assert_eq!(draft, "");
    assert!(matches!(
        event,
        Some(ServerEvent::RetryRollback { attempt: 2, max: 3 })
    ));
}

#[tokio::test]
async fn generation_busy_cancel_and_late_completion_fail_closed() {
    let state = test_state(true);
    let generation = try_begin_generation(&state)
        .await
        .expect("first generation should be admitted");
    assert_eq!(generation, 1);
    assert!(try_begin_generation(&state).await.is_none());
    assert!(is_current(&state, generation).await);

    assert!(request_cancel(&state).await);
    assert!(!is_current(&state, generation).await);
    assert!(!request_cancel(&state).await);
    finish_generation(&state, generation).await;

    let next = try_begin_generation(&state)
        .await
        .expect("a later generation may start after cancellation");
    assert_eq!(next, 2);
    assert!(!is_current(&state, generation).await);
    assert!(is_current(&state, next).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn direct_release_orders_events_before_late_cancel_and_rejects_cancelled_or_stale() {
    let root_provider: Arc<dyn Provider> = Arc::new(OfflineAnalystProvider {
        responses: Arc::new(StdMutex::new(VecDeque::new())),
    });
    let root_registry = crate::tool::Registry::new(Arc::clone(&root_provider)).await;
    let root_agent = Arc::new(Mutex::new(Agent::new(root_provider, root_registry)));
    let root_session_id = root_agent.lock().await.session_id().to_string();

    let state = test_state(true);
    let generation = try_begin_generation(&state)
        .await
        .expect("direct-release generation should be admitted");
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let hook_entered = Arc::new(Notify::new());
    let hook_release = direct_release_latch();
    let hook_invocations = Arc::new(AtomicUsize::new(0));
    let unrelated_hook_observed = Arc::new(AtomicBool::new(false));
    let hook_entered_for_task = Arc::clone(&hook_entered);
    let hook_release_for_task = Arc::clone(&hook_release);
    let hook_invocations_for_task = Arc::clone(&hook_invocations);
    let unrelated_hook_observed_for_task = Arc::clone(&unrelated_hook_observed);
    let hook_guard = set_direct_release_test_hook(
        &state,
        &root_session_id,
        generation,
        Arc::new(move || {
            if hook_invocations_for_task.fetch_add(1, Ordering::SeqCst) == 0 {
                hook_entered_for_task.notify_one();
                wait_for_direct_release(&hook_release_for_task);
            } else {
                unrelated_hook_observed_for_task.store(true, Ordering::SeqCst);
            }
        }),
    );
    let hook_release_guard = DirectReleaseTestReleaseGuard::new(Arc::clone(&hook_release));

    let release_state = Arc::clone(&state);
    let release_root = Arc::clone(&root_agent);
    let release_session = root_session_id.clone();
    let mut release_task = tokio::spawn(async move {
        release_direct_root(
            generation,
            &release_state,
            &release_root,
            &release_session,
            RootDraft {
                text: "released".to_string(),
                persisted: true,
            },
            &event_tx,
        )
        .await
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        tokio::select! {
            _ = hook_entered.notified() => {}
            result = &mut release_task => {
                panic!("direct-release task finished before hook entry: {result:?}");
            }
        }
    })
    .await
    .expect("direct-release hook entry should not hang");
    let unrelated_provider: Arc<dyn Provider> = Arc::new(OfflineAnalystProvider {
        responses: Arc::new(StdMutex::new(VecDeque::new())),
    });
    let unrelated_registry = crate::tool::Registry::new(Arc::clone(&unrelated_provider)).await;
    let unrelated_root = Arc::new(Mutex::new(Agent::new(
        unrelated_provider,
        unrelated_registry,
    )));
    let unrelated_session = unrelated_root.lock().await.session_id().to_string();
    let unrelated_state = test_state(true);
    let unrelated_generation = try_begin_generation(&unrelated_state)
        .await
        .expect("unrelated direct-release generation should be admitted");
    let (unrelated_tx, mut unrelated_rx) = mpsc::unbounded_channel();
    let unrelated_release_state = Arc::clone(&unrelated_state);
    let unrelated_release_root = Arc::clone(&unrelated_root);
    let unrelated_release_session = unrelated_session.clone();
    let unrelated_release_task = tokio::spawn(async move {
        release_direct_root(
            unrelated_generation,
            &unrelated_release_state,
            &unrelated_release_root,
            &unrelated_release_session,
            RootDraft {
                text: "unrelated".to_string(),
                persisted: true,
            },
            &unrelated_tx,
        )
        .await
    });
    assert_eq!(
        unrelated_release_task
            .await
            .expect("unrelated direct-release task should not panic")
            .expect("unrelated direct release should succeed"),
        "unrelated"
    );
    assert!(
        !unrelated_hook_observed.load(Ordering::SeqCst),
        "an unrelated release must not observe the targeted direct-release hook"
    );
    assert!(matches!(
        unrelated_rx.try_recv(),
        Ok(ServerEvent::TextDelta { text }) if text == "unrelated"
    ));
    assert!(matches!(
        unrelated_rx.try_recv(),
        Ok(ServerEvent::MessageEnd { stop_reason: None })
    ));
    assert!(unrelated_rx.try_recv().is_err());

    let cancel_ready = Arc::new(Barrier::new(2));
    let cancel_ready_for_task = Arc::clone(&cancel_ready);
    let cancel_observed_state_lock = Arc::new(AtomicBool::new(false));
    let cancel_observed_state_lock_for_task = Arc::clone(&cancel_observed_state_lock);
    let cancel_state = Arc::clone(&state);
    let cancel_task = tokio::spawn(async move {
        cancel_observed_state_lock_for_task
            .store(cancel_state.try_lock().is_err(), Ordering::SeqCst);
        cancel_ready_for_task.wait();
        request_cancel(&cancel_state).await
    });
    cancel_ready.wait();
    let state_locked_at_pause = state.try_lock().is_err();
    let mut events_before_pause = Vec::new();
    while let Ok(event) = event_rx.try_recv() {
        events_before_pause.push(event);
    }
    hook_release_guard.release();
    assert_eq!(
        release_task
            .await
            .expect("direct-release task should not panic")
            .expect("direct release should succeed"),
        "released"
    );
    assert!(
        state_locked_at_pause,
        "the state lock must remain held until the pre-send pause is released"
    );
    assert!(
        cancel_observed_state_lock.load(Ordering::SeqCst),
        "request_cancel must observe the state lock held at the pre-send pause"
    );
    assert!(
        events_before_pause.is_empty(),
        "direct-release events must not be enqueued before the pre-send pause is released"
    );
    assert!(cancel_task
        .await
        .expect("late cancellation task should not panic"));
    assert!(matches!(
        event_rx.try_recv(),
        Ok(ServerEvent::TextDelta { text }) if text == "released"
    ));
    assert!(matches!(
        event_rx.try_recv(),
        Ok(ServerEvent::MessageEnd { stop_reason: None })
    ));
    assert!(event_rx.try_recv().is_err());
    drop(hook_release_guard);
    drop(hook_guard);

    let cancelled_state = test_state(true);
    let cancelled_generation = try_begin_generation(&cancelled_state)
        .await
        .expect("cancelled generation should be admitted");
    let (cancelled_tx, mut cancelled_rx) = mpsc::unbounded_channel();
    let cancel_ready = Arc::new(Barrier::new(2));
    let cancel_ready_for_task = Arc::clone(&cancel_ready);
    let cancel_state = Arc::clone(&cancelled_state);
    let cancel_task = tokio::spawn(async move {
        cancel_ready_for_task.wait();
        request_cancel(&cancel_state).await
    });
    cancel_ready.wait();
    assert!(cancel_task
        .await
        .expect("cancellation task should not panic"));
    assert!(release_direct_root(
        cancelled_generation,
        &cancelled_state,
        &root_agent,
        &root_session_id,
        RootDraft {
            text: "cancelled".to_string(),
            persisted: true,
        },
        &cancelled_tx,
    )
    .await
    .is_err());
    assert!(cancelled_rx.try_recv().is_err());

    let stale_state = test_state(true);
    let stale_generation = try_begin_generation(&stale_state)
        .await
        .expect("stale generation should be admitted");
    assert!(request_cancel(&stale_state).await);
    let _current_generation = try_begin_generation(&stale_state)
        .await
        .expect("next generation should be admitted");
    let (stale_tx, mut stale_rx) = mpsc::unbounded_channel();
    assert!(release_direct_root(
        stale_generation,
        &stale_state,
        &root_agent,
        &root_session_id,
        RootDraft {
            text: "stale".to_string(),
            persisted: true,
        },
        &stale_tx,
    )
    .await
    .is_err());
    assert!(stale_rx.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_release_hook_cleanup_unwinds_waiter_before_registry_removal() {
    let state = test_state(true);
    let generation = try_begin_generation(&state)
        .await
        .expect("cleanup generation should be admitted");
    let root_session_id = "root-test".to_string();
    let hook_entered = Arc::new(Notify::new());
    let hook_release = direct_release_latch();
    let hook_invocations = Arc::new(AtomicUsize::new(0));
    let callback_finished = Arc::new(Notify::new());
    let cleanup_task = {
        let cleanup_state = Arc::clone(&state);
        let cleanup_session = root_session_id.clone();
        let cleanup_hook_entered = Arc::clone(&hook_entered);
        let cleanup_hook_release = Arc::clone(&hook_release);
        let cleanup_hook_invocations = Arc::clone(&hook_invocations);
        let cleanup_callback_finished = Arc::clone(&callback_finished);
        tokio::spawn(async move {
            let hook_entered_for_task = Arc::clone(&cleanup_hook_entered);
            let hook_release_for_task = Arc::clone(&cleanup_hook_release);
            let _hook_guard = set_direct_release_test_hook(
                &cleanup_state,
                &cleanup_session,
                generation,
                Arc::new(move || {
                    cleanup_hook_invocations.fetch_add(1, Ordering::SeqCst);
                    hook_entered_for_task.notify_one();
                    wait_for_direct_release(&hook_release_for_task);
                    cleanup_callback_finished.notify_one();
                }),
            );
            let _hook_release_guard =
                DirectReleaseTestReleaseGuard::new(Arc::clone(&cleanup_hook_release));
            assert!(direct_release_test_hook_is_registered(
                &cleanup_state,
                &cleanup_session,
                generation
            ));
            let _callback_task = tokio::task::spawn_blocking(move || {
                run_direct_release_test_hook(&cleanup_state, &cleanup_session, generation);
            });

            tokio::time::timeout(Duration::from_secs(1), cleanup_hook_entered.notified())
                .await
                .expect("cleanup hook entry should not hang");
            panic!("simulated early failure before direct-release release");
        })
    };

    assert!(cleanup_task.await.is_err(), "cleanup task must unwind");
    tokio::time::timeout(Duration::from_secs(1), callback_finished.notified())
        .await
        .expect("cleanup hook waiter should exit after unwind");
    assert_eq!(hook_invocations.load(Ordering::SeqCst), 1);
    assert!(!direct_release_test_hook_is_registered(
        &state,
        &root_session_id,
        generation
    ));
    run_direct_release_test_hook(&state, &root_session_id, generation);
    assert_eq!(hook_invocations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancel_during_luna_does_not_arm_the_next_analyst_phase() {
    let analyst_provider = OfflineAnalystProvider {
        responses: Arc::new(StdMutex::new(VecDeque::from([
            vec![
                StreamEvent::TextDelta("admission".to_string()),
                StreamEvent::MessageEnd { stop_reason: None },
            ],
            vec![
                StreamEvent::TextDelta("final".to_string()),
                StreamEvent::MessageEnd { stop_reason: None },
            ],
        ]))),
    };
    let analyst_provider_for_registry: Arc<dyn Provider> = Arc::new(analyst_provider.clone());
    let analyst_registry = crate::tool::Registry::new(analyst_provider_for_registry.clone()).await;
    let analyst = Arc::new(Mutex::new(Agent::new(
        analyst_provider_for_registry,
        analyst_registry,
    )));
    let analyst_identity = {
        let analyst_guard = analyst.lock().await;
        AnalystIdentity {
            model: analyst_guard.provider_model(),
            route: analyst_guard.session_route_api_method(),
            effort: analyst_guard.provider_reasoning_effort(),
        }
    };
    let analyst_child = {
        let analyst_guard = analyst.lock().await;
        SessionControlHandle::new(
            analyst_guard.session_id(),
            analyst_guard.soft_interrupt_queue(),
            analyst_guard.background_tool_signal(),
            analyst_guard.graceful_shutdown_signal(),
        )
    };
    let root_started = Arc::new(Notify::new());
    let root_provider: Arc<dyn Provider> = Arc::new(NeverEndingRootProvider {
        started: Arc::clone(&root_started),
    });
    let root_registry = crate::tool::Registry::new(Arc::clone(&root_provider)).await;
    let root_agent = Arc::new(Mutex::new(Agent::new(root_provider, root_registry)));
    let root_control = {
        let root_guard = root_agent.lock().await;
        SessionControlHandle::new(
            root_guard.session_id(),
            root_guard.soft_interrupt_queue(),
            root_guard.background_tool_signal(),
            root_guard.graceful_shutdown_signal(),
        )
    };
    let state = test_state(true);
    let generation = try_begin_generation(&state)
        .await
        .expect("initial Astra generation should be admitted");
    let sessions: crate::server::SessionAgents = Arc::new(RwLock::new(HashMap::new()));
    let (event_tx, _event_rx) = mpsc::unbounded_channel();

    let admission = run_managed_analyst_turn(
        Arc::clone(&analyst),
        analyst_child.clone(),
        "admission",
        Vec::new(),
        &state,
        &sessions,
        generation,
        &analyst_identity,
    )
    .await
    .expect("offline admission should complete");
    assert_eq!(admission, "admission");
    assert!(state.lock().await.active_child.is_none());

    let root_task = tokio::spawn(run_root_turn_proxy(
        Arc::clone(&root_agent),
        "root turn",
        Vec::new(),
        None,
        event_tx,
        Arc::new(jcode_tool_core::ScopedInlineAwaitState::new()),
        Arc::new(AtomicBool::new(false)),
    ));
    root_started.notified().await;
    assert!(request_cancel(&state).await);
    root_control.request_cancel();
    let _ = tokio::time::timeout(Duration::from_secs(1), root_task)
        .await
        .expect("cancelled Luna proxy should stop promptly")
        .expect("Luna proxy task should not panic");

    let next_generation = try_begin_generation(&state)
        .await
        .expect("a resend should be admitted after Luna cancellation");
    let final_text = run_managed_analyst_turn(
        analyst,
        analyst_child,
        "final",
        Vec::new(),
        &state,
        &sessions,
        next_generation,
        &analyst_identity,
    )
    .await
    .expect("the reused analyst should receive the resend");
    assert_eq!(final_text, "final");
}

#[test]
fn analyst_identity_mismatch_is_rejected_for_model_route_and_effort() {
    let expected = AnalystIdentity {
        model: "gpt-6-astra[web]".to_string(),
        route: Some("chatgpt-web".to_string()),
        effort: Some("high".to_string()),
    };
    let matching = AnalystIdentity {
        model: "gpt-6-astra[web]".to_string(),
        route: Some("chatgpt-web".to_string()),
        effort: Some("high".to_string()),
    };
    assert!(validate_analyst_identity(&expected, &matching).is_ok());

    for actual in [
        AnalystIdentity {
            model: "other-model".to_string(),
            ..matching.clone()
        },
        AnalystIdentity {
            route: Some("other-route".to_string()),
            ..matching.clone()
        },
        AnalystIdentity {
            effort: Some("low".to_string()),
            ..matching.clone()
        },
    ] {
        assert!(validate_analyst_identity(&expected, &actual).is_err());
    }

    let route_qualified = AnalystIdentity {
        model: "openai:gpt-6-astra[web]".to_string(),
        ..matching.clone()
    };
    assert!(validate_analyst_identity(&expected, &route_qualified).is_ok());
    let permissive_prefix_match = AnalystIdentity {
        model: "gpt-6-astra[web]-preview".to_string(),
        ..matching.clone()
    };
    assert!(validate_analyst_identity(&expected, &permissive_prefix_match).is_err());
}

#[test]
fn admission_prompt_keeps_only_genuine_input_and_final_prompt_preserves_error_chain() {
    let admission = admission_prompt(
        "inspect the supplied project",
        &[("image/png".to_string(), "encoded".to_string())],
        Some("remember the acceptance criteria"),
    );
    assert!(admission.contains("inspect the supplied project"));
    assert!(admission.contains("remember the acceptance criteria"));
    assert!(!admission.contains("prepared source scope"));
    assert!(!admission.contains("crates/jcode-app-core/src/agent.rs"));

    let root_error =
        anyhow::anyhow!("provider returned a terminal failure").context("Luna root turn failed");
    let final_prompt = final_analyst_prompt(
        "answer the user",
        "admission handoff",
        &Err(root_error),
        Some("partial draft"),
    );
    assert!(final_prompt.contains("Luna root turn failed"));
    assert!(final_prompt.contains("provider returned a terminal failure"));
}

#[test]
fn admission_envelope_requires_exact_fields_and_only_valid_waivers_skip_review() {
    let required = admission_decision(
        r#"{"instruction":"inspect the project","final_review":"required","review_reason":""}"#
            .to_string(),
    )
    .expect("required envelope should parse");
    assert_eq!(required.final_review, FinalReviewChoice::Required);
    assert_eq!(required.instruction, "inspect the project");

    let waived = admission_decision(
        r#"{"instruction":"answer directly","final_review":"not_required","review_reason":"No open questions remain."}"#
            .to_string(),
    )
    .expect("valid waiver should parse");
    assert_eq!(waived.final_review, FinalReviewChoice::NotRequired);
    assert_eq!(waived.review_reason, "No open questions remain.");

    let malformed = [
        r#"{"instruction":"answer","final_review":"not_required","review_reason":""}"#,
        r#"{"instruction":"answer","final_review":"not_required"}"#,
        r#"{"instruction":"answer","final_review":"not_required","review_reason":"reason","extra":"field"}"#,
        r#"{"instruction":"answer","final_review":"not_required","review_reason":"reason"} trailing"#,
        r#"{"instruction":"answer","final_review":"required","final_review":"not_required","review_reason":"reason"}"#,
        r#"{"instruction":"answer","final_review":[],"review_reason":"reason"}"#,
    ];
    for original in malformed {
        let decision = admission_decision(original.to_string())
            .expect("non-empty malformed admission remains compatible free text");
        assert_eq!(decision.final_review, FinalReviewChoice::Required);
        assert_eq!(decision.instruction, original);
    }

    let instruction_cannot_waive = admission_decision(
        r#"{"instruction":"not_required","final_review":"required","review_reason":"keep review"}"#
            .to_string(),
    )
    .expect("required envelope should parse regardless of instruction text");
    assert_eq!(
        instruction_cannot_waive.final_review,
        FinalReviewChoice::Required
    );
    assert!(admission_decision("  \n".to_string()).is_err());
}

#[test]
fn admission_and_handoff_prompts_preserve_contract_and_boundaries() {
    let prompt = admission_prompt(
        "requested outcome",
        &[("image/png".to_string(), "image-data".to_string())],
        Some("client reminder"),
    );
    for required in [
        "You have no local tools",
        "Distinguish supplied evidence from unknowns",
        "bounded evidence collection followed by analysis",
        "desired outcome",
        "accepted approach or unresolved",
        "bounded scope",
        "acceptance checks",
        "condition for returning to the analyst",
        "selected Luna worker executable route",
        "model=\"gpt-5.6-luna\"",
        "effort=\"low\"",
        "Role/label text is descriptive",
        "must never be passed as model selector \"luna-low\"",
        "Luna Low is for exact extraction only",
        "Luna High is",
        "Luna Max is for complex ready implementation",
        "native Astra Medium analyst",
        "explicit user approval",
        "prepared substantial read-only research",
        "exactly one JSON object with exactly these string fields",
        "`instruction`",
        "`final_review`",
        "`review_reason`",
    ] {
        assert!(
            prompt.contains(required),
            "admission prompt lost guidance: {required}"
        );
    }
    assert!(prompt.contains("requested outcome"));
    assert!(prompt.contains("Image attachments: 1"));
    assert!(prompt.contains("client reminder"));
    assert!(prompt.contains("must not inspect files, invoke commands, or spawn workers"));

    let reminder = root_handoff_reminder("bounded handoff", Some("existing reminder"));
    for required in [
        "existing reminder",
        "bounded handoff",
        "Do not trigger another admission for internal coordinator steps",
        "Return unknown blockers to the analyst through the existing workflow",
        "do not silently expand permissions",
        "Preserve the existing final-review requirement",
        "do not waive open questions",
    ] {
        assert!(
            reminder.contains(required),
            "handoff reminder lost guidance: {required}"
        );
    }
}

#[tokio::test]
async fn disabled_state_and_stale_generation_do_not_admit_work() {
    let disabled = test_state(false);
    assert!(try_begin_generation(&disabled).await.is_none());
    assert!(!is_current(&disabled, 1).await);

    let state = test_state(true);
    let generation = try_begin_generation(&state).await.expect("generation");
    {
        let mut guard = state.lock().await;
        guard.generation += 1;
    }
    assert!(!is_current(&state, generation).await);
}

#[tokio::test]
async fn completed_owned_child_with_active_turn_blocks_finalization() {
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let member = crate::server::SwarmMember::from_record(
        jcode_swarm_core::SwarmMemberRecord {
            session_id: "owned-child".to_string(),
            working_dir: None,
            swarm_id: Some("root-swarm".to_string()),
            swarm_enabled: true,
            status: jcode_swarm_core::SwarmLifecycleStatus::Completed,
            detail: None,
            task_label: Some("worker".to_string()),
            friendly_name: Some("worker".to_string()),
            report_back_to_session_id: Some("root-test".to_string()),
            latest_completion_report: Some("ready report".to_string()),
            role: jcode_swarm_core::SwarmRole::Agent,
            is_headless: true,
        },
        event_tx,
    );
    let _active_turn = crate::turn_cancel_registry::register_active_turn(
        "owned-child",
        jcode_agent_runtime::InterruptSignal::new(),
    );
    assert!(owned_child_blocks_finalization(
        "root-test",
        Some("persistent-analyst"),
        &member,
    ));
    assert!(!owned_child_finalization_blockers(
        "root-test",
        Some("persistent-analyst"),
        &HashMap::from([(member.session_id.clone(), member.clone())]),
    )
    .is_empty());
}

#[test]
fn ready_owned_child_blocks_only_while_active_and_excludes_persistent_analyst() {
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let member = crate::server::SwarmMember::from_record(
        jcode_swarm_core::SwarmMemberRecord {
            session_id: "ready-child".to_string(),
            working_dir: None,
            swarm_id: Some("root-swarm".to_string()),
            swarm_enabled: true,
            status: jcode_swarm_core::SwarmLifecycleStatus::Ready,
            detail: None,
            task_label: Some("worker".to_string()),
            friendly_name: Some("worker".to_string()),
            report_back_to_session_id: Some("root-test".to_string()),
            latest_completion_report: Some("ready report".to_string()),
            role: jcode_swarm_core::SwarmRole::Agent,
            is_headless: true,
        },
        event_tx,
    );
    assert!(!owned_child_blocks_finalization(
        "root-test",
        Some("persistent-analyst"),
        &member,
    ));

    let _active_turn = crate::turn_cancel_registry::register_active_turn(
        "ready-child",
        jcode_agent_runtime::InterruptSignal::new(),
    );
    assert!(owned_child_blocks_finalization(
        "root-test",
        Some("persistent-analyst"),
        &member,
    ));
    assert!(!owned_child_blocks_finalization(
        "root-test",
        Some("ready-child"),
        &member,
    ));
}

#[test]
fn scoped_abort_blocks_both_final_paths_before_claim_and_after_retention() {
    let state = jcode_tool_core::ScopedInlineAwaitState::new();
    let aborted = AtomicBool::new(true);
    assert!(scoped_inline_await_blocks_finalization(&state, &aborted));

    aborted.store(false, Ordering::Release);
    let state = Arc::new(state);
    let key = jcode_tool_core::ScopedInlineAwaitKey {
        root_session_id: "root".to_string(),
        session_ids: vec!["worker".to_string()],
        target_status: vec!["completed".to_string()],
        mode: jcode_tool_core::ScopedInlineAwaitMode::All,
    };
    state.begin(key).unwrap().retain();
    assert!(scoped_inline_await_blocks_finalization(&state, &aborted));

    aborted.store(true, Ordering::Release);
    assert!(scoped_inline_await_blocks_finalization(&state, &aborted));
}
