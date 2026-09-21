use super::client_lifecycle::{
    process_locked_message_streaming_mpsc, process_message_streaming_mpsc,
};
use super::comm_session::{spawn_astra_first_analyst, AstraFirstSpawnContext};
use super::SessionControlHandle;
use super::SessionAgents;
use crate::agent::{Agent, AstraFirstStateHandle};
use crate::protocol::ServerEvent;
use anyhow::{anyhow, Result};
use futures::FutureExt;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};
#[cfg(test)]
use std::sync::{Mutex as StdMutex, OnceLock};
use tokio::sync::{mpsc, Mutex};

#[cfg(test)]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct DirectReleaseTestHookKey {
    state_id: usize,
    root_session_id: String,
    generation: u64,
}

#[cfg(test)]
type DirectReleaseTestHook = Arc<dyn Fn() + Send + Sync>;

#[cfg(test)]
static DIRECT_RELEASE_TEST_HOOKS: OnceLock<
    StdMutex<HashMap<DirectReleaseTestHookKey, DirectReleaseTestHook>>,
> = OnceLock::new();

#[cfg(test)]
struct DirectReleaseTestHookGuard {
    key: DirectReleaseTestHookKey,
}

#[cfg(test)]
impl Drop for DirectReleaseTestHookGuard {
    fn drop(&mut self) {
        DIRECT_RELEASE_TEST_HOOKS
            .get_or_init(|| StdMutex::new(HashMap::new()))
            .lock()
            .expect("direct-release test hook mutex")
            .remove(&self.key);
    }
}

#[cfg(test)]
fn direct_release_test_hook_key(
    state: &AstraFirstStateHandle,
    root_session_id: &str,
    generation: u64,
) -> DirectReleaseTestHookKey {
    DirectReleaseTestHookKey {
        state_id: Arc::as_ptr(state) as usize,
        root_session_id: root_session_id.to_string(),
        generation,
    }
}

#[cfg(test)]
fn set_direct_release_test_hook(
    state: &AstraFirstStateHandle,
    root_session_id: &str,
    generation: u64,
    hook: DirectReleaseTestHook,
) -> DirectReleaseTestHookGuard {
    let key = direct_release_test_hook_key(state, root_session_id, generation);
    DIRECT_RELEASE_TEST_HOOKS
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .expect("direct-release test hook mutex")
        .insert(key.clone(), hook);
    DirectReleaseTestHookGuard { key }
}

#[cfg(test)]
fn direct_release_test_hook_is_registered(
    state: &AstraFirstStateHandle,
    root_session_id: &str,
    generation: u64,
) -> bool {
    let key = direct_release_test_hook_key(state, root_session_id, generation);
    DIRECT_RELEASE_TEST_HOOKS
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .expect("direct-release test hook mutex")
        .contains_key(&key)
}

#[cfg(test)]
fn run_direct_release_test_hook(
    state: &AstraFirstStateHandle,
    root_session_id: &str,
    generation: u64,
) {
    let key = direct_release_test_hook_key(state, root_session_id, generation);
    let hook = DIRECT_RELEASE_TEST_HOOKS
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .expect("direct-release test hook mutex")
        .get(&key)
        .cloned();
    if let Some(hook) = hook {
        hook();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AnalystIdentity {
    pub(super) model: String,
    pub(super) route: Option<String>,
    pub(super) effort: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FinalReviewChoice {
    Required,
    NotRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdmissionEnvelope {
    instruction: String,
    final_review: FinalReviewChoice,
    review_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RootDraft {
    text: String,
    persisted: bool,
}

pub(super) async fn is_enabled(state: &AstraFirstStateHandle) -> bool {
    state.lock().await.enabled
}

pub(super) async fn is_busy(state: &AstraFirstStateHandle) -> bool {
    state.lock().await.busy
}

/// Claim one new root generation before its task is spawned. The caller uses
/// `None` to reject a busy generation without touching the root Agent mutex.
pub(super) async fn try_begin_generation(state: &AstraFirstStateHandle) -> Option<u64> {
    let mut state = state.lock().await;
    if !state.enabled || state.busy {
        return None;
    }
    state.generation = state.generation.saturating_add(1);
    state.cancelled = false;
    state.busy = true;
    state.active_child = None;
    state.cancel_notify = Arc::new(tokio::sync::Notify::new());
    Some(state.generation)
}

pub(super) async fn analyst_id(state: &AstraFirstStateHandle) -> Option<String> {
    state.lock().await.analyst_id.clone()
}

pub(super) async fn state_identity(
    state: &AstraFirstStateHandle,
) -> (Option<AnalystIdentity>, bool) {
    let state = state.lock().await;
    (
        state.analyst_model.as_ref().map(|model| AnalystIdentity {
            model: model.clone(),
            route: state.analyst_route.clone(),
            effort: state.analyst_effort.clone(),
        }),
        state.busy,
    )
}

async fn is_current(state: &AstraFirstStateHandle, generation: u64) -> bool {
    let state = state.lock().await;
    state.enabled && state.busy && state.generation == generation && !state.cancelled
}

fn owned_child_blocks_finalization(
    root_session_id: &str,
    analyst_id: Option<&str>,
    member: &super::SwarmMember,
) -> bool {
    member.report_back_to_session_id.as_deref() == Some(root_session_id)
        && analyst_id != Some(member.session_id.as_str())
        && (crate::turn_cancel_registry::has_active_turn(&member.session_id)
            || !member_status_is_quiescent(&member.status))
}

fn member_status_is_quiescent(status: &str) -> bool {
    status == "ready" || super::swarm::member_status_is_terminal(status)
}

fn scoped_inline_await_blocks_finalization(
    inline_swarm_await: &AtomicUsize,
    inline_swarm_await_aborted: &AtomicBool,
) -> bool {
    inline_swarm_await_aborted.load(Ordering::Acquire)
        || inline_swarm_await.load(Ordering::Acquire) != 0
}

fn owned_child_finalization_blockers(
    root_session_id: &str,
    analyst_id: Option<&str>,
    members: &HashMap<String, super::SwarmMember>,
) -> Vec<String> {
    let mut blockers: Vec<String> = members
        .values()
        .filter(|member| owned_child_blocks_finalization(root_session_id, analyst_id, member))
        .map(|member| {
            format!(
                "{} (status={}, active_turn={})",
                member.session_id,
                member.status,
                crate::turn_cancel_registry::has_active_turn(&member.session_id),
            )
        })
        .collect();
    blockers.sort();
    blockers
}

async fn ensure_root_finalize_safe(
    state: &AstraFirstStateHandle,
    generation: u64,
    root_session_id: &str,
    inline_swarm_await: &Arc<AtomicUsize>,
    inline_swarm_await_aborted: &Arc<AtomicBool>,
    context: &AstraFirstSpawnContext,
) -> Result<()> {
    if !is_current(state, generation).await {
        anyhow::bail!("Astra-first generation became stale before finalization");
    }
    if scoped_inline_await_blocks_finalization(inline_swarm_await, inline_swarm_await_aborted) {
        if inline_swarm_await_aborted.load(Ordering::Acquire) {
            anyhow::bail!(
                "Astra-first orchestration unsafe: scoped inline swarm await was aborted before completion"
            );
        }
        anyhow::bail!(
            "Astra-first orchestration unsafe: {} scoped inline swarm await(s) remain unresolved",
            inline_swarm_await.load(Ordering::Acquire)
        );
    }

    let persistent_analyst_id = analyst_id(state).await;
    let members = context.swarm_members.read().await;
    let blockers = owned_child_finalization_blockers(
        root_session_id,
        persistent_analyst_id.as_deref(),
        &members,
    );
    if !blockers.is_empty() {
        anyhow::bail!(
            "Astra-first orchestration unsafe: directly owned child remains nonterminal or active: {}",
            blockers.join(", ")
        );
    }
    Ok(())
}

async fn set_analyst(
    state: &AstraFirstStateHandle,
    generation: u64,
    analyst_id: String,
    identity: AnalystIdentity,
) -> Result<()> {
    let mut state = state.lock().await;
    if !state.enabled || !state.busy || state.generation != generation || state.cancelled {
        anyhow::bail!("Astra-first analyst spawn became stale before assignment");
    }
    state.analyst_id = Some(analyst_id);
    state.analyst_model = Some(identity.model);
    state.analyst_route = identity.route;
    state.analyst_effort = identity.effort;
    Ok(())
}

async fn set_active_child(
    state: &AstraFirstStateHandle,
    generation: u64,
    child: SessionControlHandle,
) -> Result<()> {
    let mut state = state.lock().await;
    if !state.enabled || !state.busy || state.generation != generation || state.cancelled {
        anyhow::bail!("Astra-first child became stale before execution");
    }
    state.active_child = Some(child);
    Ok(())
}

async fn clear_active_child(state: &AstraFirstStateHandle, generation: u64) {
    let mut state = state.lock().await;
    if state.generation == generation {
        state.active_child = None;
    }
}

pub(super) async fn request_cancel(state: &AstraFirstStateHandle) -> bool {
    let mut state = state.lock().await;
    if !state.enabled || !state.busy {
        return false;
    }
    state.cancelled = true;
    state.busy = false;
    if let Some(child) = state.active_child.take() {
        child.request_cancel();
    }
    state.cancel_notify.notify_waiters();
    true
}

pub(super) async fn finish_generation(state: &AstraFirstStateHandle, generation: u64) {
    let mut state = state.lock().await;
    if state.generation == generation {
        state.busy = false;
        state.active_child = None;
    }
}

pub(super) fn validate_analyst_identity(
    expected: &AnalystIdentity,
    actual: &AnalystIdentity,
) -> Result<()> {
    if canonical_model_identity(&expected.model) != canonical_model_identity(&actual.model) {
        anyhow::bail!(
            "Astra-first analyst model mismatch: expected '{}', actual '{}'",
            expected.model,
            actual.model
        );
    }
    if expected.route != actual.route {
        anyhow::bail!(
            "Astra-first analyst route mismatch: expected {:?}, actual {:?}",
            expected.route,
            actual.route
        );
    }
    if expected.effort != actual.effort {
        anyhow::bail!(
            "Astra-first analyst effort mismatch: expected {:?}, actual {:?}",
            expected.effort,
            actual.effort
        );
    }
    Ok(())
}

fn canonical_model_identity(model: &str) -> String {
    let model = model.trim();
    let model = jcode_provider_core::explicit_model_provider_prefix(model)
        .map(|(_, _, bare)| bare)
        .unwrap_or(model);
    model.to_ascii_lowercase()
}

fn parse_admission_envelope(admission: &str) -> Result<AdmissionEnvelope> {
    let envelope: AdmissionEnvelope = serde_json::from_str(admission)
        .map_err(|error| anyhow!("invalid Astra-first admission envelope: {error}"))?;
    if envelope.instruction.trim().is_empty() {
        anyhow::bail!("Astra-first admission envelope instruction is empty");
    }
    if envelope.final_review == FinalReviewChoice::NotRequired
        && envelope.review_reason.trim().is_empty()
    {
        anyhow::bail!("Astra-first admission envelope waiver requires a non-empty review_reason");
    }
    Ok(envelope)
}

fn admission_decision(admission: String) -> Result<AdmissionEnvelope> {
    if admission.trim().is_empty() {
        anyhow::bail!("Astra-first admission returned empty text");
    }

    // Preserve the pre-envelope free-text contract. A non-empty response that
    // is not a fully valid current envelope is usable as the old handoff, but
    // it can never waive the final review.
    Ok(
        parse_admission_envelope(&admission).unwrap_or(AdmissionEnvelope {
            instruction: admission,
            final_review: FinalReviewChoice::Required,
            review_reason: String::new(),
        }),
    )
}

/// Return only root progress/tool events. Draft text, reasoning, and the root
/// MessageEnd are deliberately withheld until the same analyst produces the
/// final answer.
pub(super) fn proxy_root_event(event: &ServerEvent, draft: &mut String) -> Option<ServerEvent> {
    match event {
        ServerEvent::TextDelta { text } => {
            draft.push_str(text);
            None
        }
        ServerEvent::TextReplace { text } => {
            *draft = text.clone();
            None
        }
        ServerEvent::ReasoningDelta { .. } | ServerEvent::ReasoningDone { .. } => None,
        ServerEvent::MessageEnd { .. } => None,
        ServerEvent::RetryRollback { .. } => {
            draft.clear();
            Some(event.clone())
        }
        _ => Some(event.clone()),
    }
}

fn admission_prompt(
    content: &str,
    images: &[(String, String)],
    system_reminder: Option<&str>,
) -> String {
    format!(
        "You are the Astra admission analyst for a root Luna turn.\n\
         You have no local tools and must not inspect files, invoke commands, or spawn workers.\n\
         Prepare a concise handoff for the ordinary Luna coordinator. Do not present this as the\n\
         final user answer. Distinguish supplied evidence from unknowns. If evidence is missing,\n\
         prescribe bounded evidence collection followed by analysis; never claim to have inspected\n\
         code or tools. The handoff must state the desired outcome, accepted approach or unresolved\n\
         question, bounded scope, acceptance checks, the condition for returning to the analyst,\n\
         and the selected Luna worker executable route: exact extraction uses model=\"gpt-5.6-luna\",\n\
         effort=\"low\". Role/label text is descriptive and must never be passed as model selector \"luna-low\".\n\
         Luna Low is for exact extraction only, Luna High is\n\
         for ready implementation, and Luna Max is for complex ready implementation. Do not use\n\
         Luna to guess unresolved decisions: keep those with the persistent native Astra Medium analyst.\n\
         Propose Astra High or Max only with a concrete reason and\n\
         explicit user approval, never by automatic escalation. Web is only separately authorized\n\
         prepared substantial read-only research. Return exactly one JSON object with exactly these string fields:\n\
         `instruction` (non-empty), `final_review` (`required` or `not_required`), and\n\
         `review_reason` (a string, non-empty when using `not_required`). Do not add fields,\n\
         duplicate fields, markdown fences, or trailing commentary. Use `required` for open\n\
         questions, material risk, or when no justified waiver is available. The runtime only\n\
         checks this envelope syntax and current-turn state; it does not enforce semantic scope\n\
         or approach compliance.\n\n\
         User message:\n{content}\n\n\
         Image attachments: {}\n\n\
         Client reminder:\n{}",
        images.len(),
        system_reminder.unwrap_or("<none>")
    )
}

fn root_handoff_reminder(admission: &str, existing: Option<&str>) -> String {
    format!(
        "{}\n\n<system-reminder>\nAstra admission handoff for the current generation:\n{}\n\nUse the ordinary Luna coordinator and existing bounded swarm workflow. Executable extraction routing is exactly model=\"gpt-5.6-luna\", effort=\"low\"; role/label text is descriptive and must never be passed as model selector \"luna-low\". Do not trigger another admission for internal coordinator steps. Return unknown blockers to the analyst through the existing workflow, and do not silently expand permissions. Preserve the existing final-review requirement; do not waive open questions. Do not treat this reminder as a user message.\n</system-reminder>",
        existing.unwrap_or_default(),
        admission
    )
}

fn final_analyst_prompt(
    content: &str,
    admission: &str,
    root_result: &Result<()>,
    root_draft: Option<&str>,
) -> String {
    let outcome = match root_result {
        Ok(()) => "Luna completed its ordinary coordinator turn.".to_string(),
        Err(error) => format!(
            "Luna's ordinary coordinator turn returned an error: {}. Diagnose it without blindly retrying writes.",
            crate::util::format_error_chain(error)
        ),
    };
    format!(
        "You are the same Astra analyst completing the current generation.\n\
         You still have no local tools. Return the exact final answer for the user, not a plan for\n\
         another agent and not a description of this wrapper.\n\n\
         Original user message:\n{content}\n\n\
         Admission handoff:\n{admission}\n\n\
         Luna outcome: {outcome}\n\
         Luna draft (may be absent or partial):\n{}",
        root_draft.unwrap_or("<none>")
    )
}

async fn prepare_analyst(
    context: &AstraFirstSpawnContext,
    analyst_id: &str,
    expected: &AnalystIdentity,
) -> Result<(Arc<Mutex<Agent>>, AnalystIdentity, SessionControlHandle)> {
    let analyst = context
        .sessions
        .read()
        .await
        .get(analyst_id)
        .map(|entry| entry.agent())
        .ok_or_else(|| anyhow!("stored Astra-first analyst session is unavailable"))?;
    let (actual, child) = {
        let mut analyst_guard = analyst.lock().await;
        analyst_guard.set_inline_output_tap(false);
        analyst_guard.restrict_tools_for_astra_first();
        let actual = AnalystIdentity {
            model: analyst_guard.provider_model(),
            route: analyst_guard.session_route_api_method(),
            effort: analyst_guard.provider_reasoning_effort(),
        };
        validate_analyst_identity(expected, &actual)?;
        if analyst_guard.session_for_split().origin() != crate::session::SessionOrigin::SwarmWorker
        {
            anyhow::bail!("stored Astra-first analyst is not a swarm worker");
        }
        let child = SessionControlHandle::new(
            analyst_id,
            analyst_guard.soft_interrupt_queue(),
            analyst_guard.background_tool_signal(),
            analyst_guard.graceful_shutdown_signal(),
        );
        (actual, child)
    };
    Ok((analyst, actual, child))
}

async fn run_managed_analyst_turn(
    analyst: Arc<Mutex<Agent>>,
    child: SessionControlHandle,
    prompt: &str,
    images: Vec<(String, String)>,
    state: &AstraFirstStateHandle,
    sessions: &SessionAgents,
    generation: u64,
    expected: &AnalystIdentity,
) -> Result<String> {
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let (before_error, result, actual, after_error, text) = {
        let mut analyst_guard = analyst.lock().await;
        let analyst_session_id = analyst_guard.session_id().to_string();
        let phase_cancel_guard = crate::turn_cancel_registry::register_active_turn(
            &analyst_session_id,
            analyst_guard.graceful_shutdown_signal(),
        );
        set_active_child(state, generation, child).await?;
        let before = AnalystIdentity {
            model: analyst_guard.provider_model(),
            route: analyst_guard.session_route_api_method(),
            effort: analyst_guard.provider_reasoning_effort(),
        };
        let before_error = validate_analyst_identity(expected, &before).err();
        let start_message_index = analyst_guard.message_count();
        let (result, panic_error) = if before_error.is_none() {
            match std::panic::AssertUnwindSafe(async {
                let runtime_fast_state = {
                    let sessions_guard = sessions.read().await;
                    sessions_guard.get(&analyst_session_id).and_then(|entry| {
                        let entry_agent = entry.agent();
                        Arc::ptr_eq(&entry_agent, &analyst).then(|| entry.fast_state())
                    })
                };
                let provider = analyst_guard.provider_handle();
                let session = analyst_guard.session_for_split();
                super::apply_runtime_fast_policy(
                    provider.as_ref(),
                    session,
                    runtime_fast_state.as_deref(),
                )?;
                process_locked_message_streaming_mpsc(
                    &mut analyst_guard,
                    prompt,
                    images,
                    None,
                    event_tx.clone(),
                )
                .await
            })
            .catch_unwind()
            .await
            {
                Ok(result) => (Some(result), None),
                Err(_) => (
                    None,
                    Some(anyhow!(
                        "Astra-first analyst provider panicked during execution"
                    )),
                ),
            }
        } else {
            (None, None)
        };
        let after = AnalystIdentity {
            model: analyst_guard.provider_model(),
            route: analyst_guard.session_route_api_method(),
            effort: analyst_guard.provider_reasoning_effort(),
        };
        let after_error = validate_analyst_identity(expected, &after).err();
        let text = result
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .and_then(|_| analyst_guard.latest_assistant_text_after(start_message_index));
        clear_active_child(state, generation).await;
        drop(phase_cancel_guard);
        (
            before_error,
            result.or_else(|| panic_error.map(Err)),
            after,
            after_error,
            text,
        )
    };
    drop(event_tx);
    while event_rx.recv().await.is_some() {}
    if !is_current(state, generation).await {
        anyhow::bail!("Astra-first analyst result became stale or cancelled");
    }
    if let Some(error) = before_error {
        return Err(error);
    }
    result.ok_or_else(|| anyhow!("Astra-first analyst run was not started"))??;
    if let Some(error) = after_error {
        return Err(error);
    }
    validate_analyst_identity(expected, &actual)?;
    let text = text.ok_or_else(|| anyhow!("Astra-first analyst returned no text"))?;
    if text.trim().is_empty() {
        anyhow::bail!("Astra-first analyst returned empty text");
    }
    Ok(text)
}

async fn run_root_turn_proxy(
    root_agent: Arc<Mutex<Agent>>,
    content: &str,
    images: Vec<(String, String)>,
    system_reminder: Option<String>,
    event_tx: mpsc::UnboundedSender<ServerEvent>,
    inline_swarm_await: Arc<AtomicUsize>,
    inline_swarm_await_aborted: Arc<AtomicBool>,
) -> (Result<()>, Option<RootDraft>) {
    let start_message_index = root_agent.lock().await.message_count();
    let (proxy_tx, mut proxy_rx) = mpsc::unbounded_channel();
    let mut root_future = Box::pin(crate::agent::scope_astra_inline_swarm_await_with_abort(
        inline_swarm_await,
        inline_swarm_await_aborted,
        process_message_streaming_mpsc(
            Arc::clone(&root_agent),
            content,
            images,
            system_reminder,
            proxy_tx.clone(),
        ),
    ));
    let mut draft = String::new();
    let result = loop {
        tokio::select! {
            result = &mut root_future => break result,
            event = proxy_rx.recv() => match event {
                Some(event) => {
                    if let Some(event) = proxy_root_event(&event, &mut draft) {
                        let _ = event_tx.send(event);
                    }
                }
                None => break root_future.await,
            }
        }
    };
    drop(proxy_tx);
    while let Some(event) = proxy_rx.recv().await {
        if let Some(event) = proxy_root_event(&event, &mut draft) {
            let _ = event_tx.send(event);
        }
    }
    let persisted = root_agent
        .lock()
        .await
        .latest_assistant_text_after(start_message_index);
    let text = persisted
        .clone()
        .or_else(|| (!draft.trim().is_empty()).then_some(draft));
    (
        result,
        text.map(|text| RootDraft {
            text,
            persisted: persisted.is_some(),
        }),
    )
}

async fn release_direct_root(
    generation: u64,
    state: &AstraFirstStateHandle,
    root_agent: &Arc<Mutex<Agent>>,
    root_session_id: &str,
    root_draft: RootDraft,
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
) -> Result<String> {
    release_direct_root_with_safety(
        generation,
        state,
        root_agent,
        root_session_id,
        root_draft,
        event_tx,
        None,
        None,
        None,
    )
    .await
}

async fn release_direct_root_with_safety(
    generation: u64,
    state: &AstraFirstStateHandle,
    root_agent: &Arc<Mutex<Agent>>,
    root_session_id: &str,
    root_draft: RootDraft,
    event_tx: &mpsc::UnboundedSender<ServerEvent>,
    inline_swarm_await: Option<&Arc<AtomicUsize>>,
    inline_swarm_await_aborted: Option<&Arc<AtomicBool>>,
    context: Option<&AstraFirstSpawnContext>,
) -> Result<String> {
    let final_text = root_draft.text.clone();
    {
        let mut root = root_agent.lock().await;
        if root.session_id() != root_session_id {
            anyhow::bail!("Astra-first root session changed before direct release");
        }

        if let (Some(inline_swarm_await), Some(inline_swarm_await_aborted), Some(context)) =
            (inline_swarm_await, inline_swarm_await_aborted, context)
        {
            {
                let state_guard = state
                    .try_lock()
                    .map_err(|_| anyhow!("Astra-first state changed during direct release"))?;
                if !state_guard.enabled
                    || !state_guard.busy
                    || state_guard.generation != generation
                    || state_guard.cancelled
                {
                    anyhow::bail!("Astra-first generation became stale before direct release");
                }
            }
            ensure_root_finalize_safe(
                state,
                generation,
                root_session_id,
                inline_swarm_await,
                inline_swarm_await_aborted,
                context,
            )
            .await?;
        }

        let state_guard = state
            .try_lock()
            .map_err(|_| anyhow!("Astra-first state changed during direct release"))?;
        if !state_guard.enabled
            || !state_guard.busy
            || state_guard.generation != generation
            || state_guard.cancelled
        {
            anyhow::bail!("Astra-first generation became stale before direct release");
        }
        if !root_draft.persisted {
            root.append_relayed_assistant(&final_text)?;
        }
        #[cfg(test)]
        run_direct_release_test_hook(state, root_session_id, generation);
        let _ = event_tx.send(ServerEvent::TextDelta {
            text: final_text.clone(),
        });
        let _ = event_tx.send(ServerEvent::MessageEnd { stop_reason: None });
    }
    Ok(final_text)
}

async fn run_inner(
    generation: u64,
    state: AstraFirstStateHandle,
    root_agent: Arc<Mutex<Agent>>,
    request_id: u64,
    root_session_id: String,
    content: String,
    images: Vec<(String, String)>,
    system_reminder: Option<String>,
    event_tx: mpsc::UnboundedSender<ServerEvent>,
    context: AstraFirstSpawnContext,
) -> Result<String> {
    let config = crate::config::config()
        .agents
        .astra_first
        .clone()
        .ok_or_else(|| anyhow!("Astra-first is disabled"))?;
    let model = config.model.trim().to_string();
    let effort = config.effort.trim().to_string();
    if model.is_empty() || effort.is_empty() {
        anyhow::bail!("Astra-first requires explicit non-empty analyst model and effort");
    }

    let state_session_id = state.lock().await.session_id.clone();
    if state_session_id != root_session_id {
        anyhow::bail!("Astra-first state is bound to a different root session");
    }

    let (working_dir, actual_root_session_id) = {
        let root = root_agent.lock().await;
        (
            root.working_dir().map(str::to_string),
            root.session_id().to_string(),
        )
    };
    if actual_root_session_id != root_session_id {
        anyhow::bail!("Astra-first root session changed before admission");
    }
    if !is_current(&state, generation).await {
        anyhow::bail!("Astra-first generation is stale before admission");
    }

    let expected = super::comm_session::resolve_astra_first_identity(
        &root_session_id,
        &model,
        &effort,
        &context.sessions,
    )
    .await?;
    let analyst_id = if let Some(analyst_id) = analyst_id(&state).await {
        let (stored, _) = state_identity(&state).await;
        let stored = stored.ok_or_else(|| anyhow!("stored analyst identity is missing"))?;
        validate_analyst_identity(&expected, &stored)?;
        analyst_id
    } else {
        if !is_current(&state, generation).await {
            anyhow::bail!("Astra-first analyst spawn became stale");
        }
        let analyst_id = spawn_astra_first_analyst(
            request_id,
            &root_session_id,
            working_dir,
            model.clone(),
            effort.clone(),
            &event_tx,
            &context,
        )
        .await?;
        if !is_current(&state, generation).await {
            anyhow::bail!("Astra-first analyst spawn completed stale");
        }
        let analyst = context
            .sessions
            .read()
            .await
            .get(&analyst_id)
            .cloned()
            .ok_or_else(|| anyhow!("Astra-first analyst is missing after spawn"))?;
        let actual = {
            let guard = analyst.lock().await;
            AnalystIdentity {
                model: guard.provider_model(),
                route: guard.session_route_api_method(),
                effort: guard.provider_reasoning_effort(),
            }
        };
        validate_analyst_identity(&expected, &actual)?;
        set_analyst(&state, generation, analyst_id.clone(), actual).await?;
        analyst_id
    };

    let (analyst, _, child) = {
        let (stored, _) = state_identity(&state).await;
        let stored =
            stored.ok_or_else(|| anyhow!("Astra-first analyst identity is unavailable"))?;
        prepare_analyst(&context, &analyst_id, &stored).await?
    };

    let admission = run_managed_analyst_turn(
        Arc::clone(&analyst),
        child.clone(),
        &admission_prompt(&content, &images, system_reminder.as_deref()),
        images.clone(),
        &state,
        &context.sessions,
        generation,
        &expected,
    )
    .await?;
    let admission = admission_decision(admission)?;
    if !is_current(&state, generation).await {
        anyhow::bail!("Astra-first admission became stale");
    }

    let root_reminder = root_handoff_reminder(&admission.instruction, system_reminder.as_deref());
    {
        let root = root_agent.lock().await;
        if root.session_id() != root_session_id {
            anyhow::bail!("Astra-first root session changed before Luna run");
        }
    }
    if !is_current(&state, generation).await {
        anyhow::bail!("Astra-first generation became stale before Luna run");
    }
    let inline_swarm_await = Arc::new(AtomicUsize::new(0));
    let inline_swarm_await_aborted = Arc::new(AtomicBool::new(false));
    let (root_result, root_draft) = run_root_turn_proxy(
        Arc::clone(&root_agent),
        &content,
        images,
        Some(root_reminder),
        event_tx.clone(),
        Arc::clone(&inline_swarm_await),
        Arc::clone(&inline_swarm_await_aborted),
    )
    .await;
    if !is_current(&state, generation).await {
        anyhow::bail!("Astra-first root turn became stale or cancelled");
    }
    ensure_root_finalize_safe(
        &state,
        generation,
        &root_session_id,
        &inline_swarm_await,
        &inline_swarm_await_aborted,
        &context,
    )
    .await?;

    if root_result.is_ok() && admission.final_review == FinalReviewChoice::NotRequired {
        let root_draft = root_draft
            .ok_or_else(|| anyhow!("Astra-first root completed without releasable text"))?;
        let final_text = release_direct_root_with_safety(
            generation,
            &state,
            &root_agent,
            &root_session_id,
            root_draft,
            &event_tx,
            Some(&inline_swarm_await),
            Some(&inline_swarm_await_aborted),
            Some(&context),
        )
        .await?;
        return Ok(final_text);
    }

    let final_prompt = final_analyst_prompt(
        &content,
        &admission.instruction,
        &root_result,
        root_draft.as_ref().map(|draft| draft.text.as_str()),
    );
    let final_text = run_managed_analyst_turn(
        analyst,
        child,
        &final_prompt,
        Vec::new(),
        &state,
        &context.sessions,
        generation,
        &expected,
    )
    .await?;
    if !is_current(&state, generation).await {
        anyhow::bail!("Astra-first final became stale or cancelled");
    }

    {
        let mut root = root_agent.lock().await;
        if root.session_id() != root_session_id {
            anyhow::bail!("Astra-first root session changed before final relay");
        }
        ensure_root_finalize_safe(
            &state,
            generation,
            &root_session_id,
            &inline_swarm_await,
            &inline_swarm_await_aborted,
            &context,
        )
        .await?;
        let state_guard = state
            .try_lock()
            .map_err(|_| anyhow!("Astra-first state changed during final relay"))?;
        if !state_guard.enabled
            || !state_guard.busy
            || state_guard.generation != generation
            || state_guard.cancelled
        {
            anyhow::bail!("Astra-first generation became stale before final relay");
        }
        root.append_relayed_assistant(&final_text)?;
        drop(state_guard);
    }
    let _ = event_tx.send(ServerEvent::TextDelta {
        text: final_text.clone(),
    });
    let _ = event_tx.send(ServerEvent::MessageEnd { stop_reason: None });
    Ok(final_text)
}

pub(super) async fn run_astra_first_task(
    generation: u64,
    state: AstraFirstStateHandle,
    root_agent: Arc<Mutex<Agent>>,
    request_id: u64,
    root_session_id: String,
    content: String,
    images: Vec<(String, String)>,
    system_reminder: Option<String>,
    event_tx: mpsc::UnboundedSender<ServerEvent>,
    context: AstraFirstSpawnContext,
) -> Result<String> {
    let result = match std::panic::AssertUnwindSafe(run_inner(
        generation,
        Arc::clone(&state),
        root_agent,
        request_id,
        root_session_id,
        content,
        images,
        system_reminder,
        event_tx,
        context,
    ))
    .catch_unwind()
    .await
    {
        Ok(result) => result,
        Err(payload) => {
            let message = if let Some(message) = payload.downcast_ref::<&str>() {
                (*message).to_string()
            } else if let Some(message) = payload.downcast_ref::<String>() {
                message.clone()
            } else {
                "unknown panic".to_string()
            };
            Err(anyhow!("Astra-first task panicked: {message}"))
        }
    };
    finish_generation(&state, generation).await;
    result
}

#[cfg(test)]
#[path = "astra_first_tests.rs"]
mod tests;
