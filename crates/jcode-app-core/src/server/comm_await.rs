use super::await_members_state::{
    PersistedAwaitMembersState, all_pending_await_members_including_expired, ensure_pending_state,
    load_state, persist_final_response, persist_non_replayable_final_response, request_key,
};
use super::{AwaitMembersRuntime, SwarmEvent, SwarmMember};
use crate::bus::{Bus, BusEvent, SwarmAwaitCompleted, UiActivity};
use crate::protocol::{AwaitedMemberStatus, ServerEvent, format_comm_awaited_members_with_reports};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{RwLock, broadcast, mpsc};

pub(super) async fn awaited_member_statuses(
    req_session_id: &str,
    swarm_id: &str,
    requested_ids: &[String],
    target_status: &[String],
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
) -> Vec<AwaitedMemberStatus> {
    let watch_ids: Vec<String> = if requested_ids.is_empty() {
        let mut watch_ids: Vec<String> = {
            let swarms = swarms_by_id.read().await;
            swarms
                .get(swarm_id)
                .map(|sessions| {
                    sessions
                        .iter()
                        .filter(|session_id| session_id.as_str() != req_session_id)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        };
        watch_ids.sort();
        watch_ids
    } else {
        requested_ids.to_vec()
    };

    let members = swarm_members.read().await;
    watch_ids
        .iter()
        .map(|session_id| {
            let (name, status, completion_report) = members
                .get(session_id)
                .map(|member| {
                    (
                        member.friendly_name.clone(),
                        member.status.clone(),
                        member.latest_completion_report.clone(),
                    )
                })
                // Terminal members are eventually garbage-collected. An
                // explicitly requested member disappearing therefore means it
                // was stopped, rather than an unhelpful `unknown` that can
                // make a coordinator time out after its worker has gone away.
                .unwrap_or((None, "stopped".to_string(), None));
            let done = target_status.contains(&status)
                || (status == "stopped"
                    && (target_status.contains(&"completed".to_string())
                        || target_status.contains(&"failed".to_string())));
            AwaitedMemberStatus {
                session_id: session_id.clone(),
                friendly_name: name,
                status,
                done,
                completion_report,
            }
        })
        .collect()
}

fn short_member_name(member: &AwaitedMemberStatus) -> String {
    member
        .friendly_name
        .clone()
        .unwrap_or_else(|| member.session_id[..8.min(member.session_id.len())].to_string())
}

pub(super) fn timeout_summary(member_statuses: &[AwaitedMemberStatus]) -> String {
    let pending: Vec<String> = member_statuses
        .iter()
        .filter(|member| !member.done)
        .map(|member| format!("{} ({})", short_member_name(member), member.status))
        .collect();
    format!("Timed out. Still waiting on: {}", pending.join(", "))
}

fn completion_summary(member_statuses: &[AwaitedMemberStatus]) -> String {
    let done_names: Vec<String> = member_statuses.iter().map(short_member_name).collect();
    format!(
        "All {} members are done: {}",
        done_names.len(),
        done_names.join(", ")
    )
}

pub(super) fn completion_mode(mode: Option<&str>) -> &str {
    match mode {
        Some("any") => "any",
        _ => "all",
    }
}

pub(super) fn mode_satisfied(member_statuses: &[AwaitedMemberStatus], mode: Option<&str>) -> bool {
    match completion_mode(mode) {
        "any" => member_statuses.iter().any(|status| status.done),
        _ => member_statuses.iter().all(|status| status.done),
    }
}

pub(super) fn mode_summary(member_statuses: &[AwaitedMemberStatus], mode: Option<&str>) -> String {
    match completion_mode(mode) {
        "any" => {
            let matching: Vec<String> = member_statuses
                .iter()
                .filter(|member| member.done)
                .map(short_member_name)
                .collect();
            format!(
                "Matched {} member{}: {}",
                matching.len(),
                if matching.len() == 1 { "" } else { "s" },
                matching.join(", ")
            )
        }
        _ => completion_summary(member_statuses),
    }
}

pub(super) fn deadline_to_instant(deadline_unix_ms: u64) -> tokio::time::Instant {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    tokio::time::Instant::now() + Duration::from_millis(deadline_unix_ms.saturating_sub(now_ms))
}

pub(super) async fn respond_to_waiters(
    runtime: &AwaitMembersRuntime,
    key: &str,
    completed: bool,
    members: Vec<AwaitedMemberStatus>,
    summary: String,
) {
    for (request_id, client_event_tx) in runtime.take_waiters(key).await {
        let _ = client_event_tx.send(ServerEvent::CommAwaitMembersResponse {
            id: request_id,
            completed,
            members: members.clone(),
            summary: summary.clone(),
            background_started: false,
        });
    }
    runtime.clear_active(key).await;
}

/// Build the swarm-flavored completion notification body delivered to the
/// requesting agent when a backgrounded await finishes. Reuses the same
/// member-status + completion-report rendering as the blocking tool result so
/// the agent sees consistent output whether it waited inline or in the
/// background.
fn background_completion_notification(
    completed: bool,
    summary: &str,
    members: &[AwaitedMemberStatus],
) -> String {
    let reports = HashMap::new();
    let body = format_comm_awaited_members_with_reports(completed, summary, members, &reports);
    format!("🐝 **Swarm await finished**\n\n{}", body)
}

/// Claim terminal ownership while holding the transaction shared with request
/// preference updates, waiter replacement, and disconnect cancellation. The
/// returned state is the exact durable final record, not a snapshot reloaded
/// before the claim, and is the only state a terminal publisher may use.
async fn claim_terminal_response(
    runtime: &AwaitMembersRuntime,
    state: &PersistedAwaitMembersState,
    completed: bool,
    members: Vec<AwaitedMemberStatus>,
    summary: String,
) -> Option<PersistedAwaitMembersState> {
    let _transaction = runtime.transaction_for_finalize(&state.key).await;
    let pending = load_state(&state.key).filter(PersistedAwaitMembersState::is_pending)?;
    let committed = persist_final_response(&pending, completed, members, summary);
    runtime.forget_transaction(&state.key).await;
    Some(committed)
}

/// Atomically cancel a blocking watcher only if it still has no live waiter and
/// has not been promoted to background delivery. A terminal claim that won
/// first is left untouched for its sole publisher to deliver.
async fn cancel_disconnected_blocking_await(
    runtime: &AwaitMembersRuntime,
    state: &PersistedAwaitMembersState,
    members: Vec<AwaitedMemberStatus>,
) -> bool {
    let _transaction = runtime.transaction_for_cancel(&state.key).await;
    let Some(current) = load_state(&state.key) else {
        runtime.clear_active(&state.key).await;
        runtime.forget_transaction(&state.key).await;
        return true;
    };
    if !current.is_pending() {
        return true;
    }
    if current.background || runtime.retain_open_waiters(&state.key).await != 0 {
        return false;
    }

    let _ = persist_non_replayable_final_response(
        &current,
        false,
        members,
        "Await cancelled because requesting client disconnected.".to_string(),
    );
    let _ = runtime.take_waiters(&state.key).await;
    runtime.clear_active(&state.key).await;
    runtime.forget_transaction(&state.key).await;
    true
}

/// Persist the terminal result, reply to any blocking socket waiters, and, when
/// the await was started in background mode, publish a `SwarmAwaitCompleted`
/// bus event so the server's bus monitor can wake/notify the requesting agent
/// the same way background tasks do.
pub(super) async fn finalize_await(
    runtime: &AwaitMembersRuntime,
    state: &PersistedAwaitMembersState,
    completed: bool,
    members: Vec<AwaitedMemberStatus>,
    summary: String,
) {
    let Some(state) =
        claim_terminal_response(runtime, state, completed, members.clone(), summary.clone()).await
    else {
        // Another watcher/cancellation already claimed this semantic wait. It
        // owns all final delivery, so never publish a duplicate bus event.
        return;
    };

    if state.background && (state.notify || state.wake) {
        let notification = background_completion_notification(completed, &summary, &members);
        Bus::global().publish(BusEvent::SwarmAwaitCompleted(SwarmAwaitCompleted {
            session_id: state.session_id.clone(),
            completed,
            summary: summary.clone(),
            notification,
            notify: state.notify,
            wake: state.wake,
        }));
    }

    respond_to_waiters(runtime, &state.key, completed, members, summary).await;
}

pub(super) async fn spawn_or_resume_await_members(
    state: PersistedAwaitMembersState,
    req_session_id: String,
    swarm_members: Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    swarm_event_tx: broadcast::Sender<SwarmEvent>,
    await_members_runtime: AwaitMembersRuntime,
) {
    let key = state.key.clone();
    let swarm_id = state.swarm_id.clone();
    let requested_ids = state.requested_ids.clone();
    let target_status = state.target_status.clone();
    let mode = state.mode.clone();

    tokio::spawn(async move {
        let mut event_rx = swarm_event_tx.subscribe();
        let deadline = deadline_to_instant(state.deadline_unix_ms);

        loop {
            let member_statuses = awaited_member_statuses(
                &req_session_id,
                &swarm_id,
                &requested_ids,
                &target_status,
                &swarm_members,
                &swarms_by_id,
            )
            .await;

            if member_statuses.is_empty() {
                let summary = "No other members in swarm to wait for.".to_string();
                finalize_await(&await_members_runtime, &state, true, vec![], summary).await;
                return;
            }

            if mode_satisfied(&member_statuses, mode.as_deref()) {
                let summary = mode_summary(&member_statuses, mode.as_deref());
                finalize_await(
                    &await_members_runtime,
                    &state,
                    true,
                    member_statuses,
                    summary,
                )
                .await;
                return;
            }

            // Blocking waits stop watching once every socket waiter has
            // disconnected. Background watchers have no socket waiter, so they
            // keep running until they resolve or hit the deadline, delivering
            // the result via notify/wake. This read is advisory only: the
            // cancel path below rechecks the same policy under the per-key
            // transaction before it can persist a cancellation.
            let is_background = load_state(&state.key)
                .filter(PersistedAwaitMembersState::is_pending)
                .map(|latest| latest.background)
                .unwrap_or(state.background);
            if !is_background
                && cancel_disconnected_blocking_await(
                    &await_members_runtime,
                    &state,
                    member_statuses.clone(),
                )
                .await
            {
                return;
            }

            tokio::select! {
                // If a terminal swarm event and a requester disconnect become
                // ready together, cancellation owns this iteration. `biased`
                // makes that ownership deterministic and ensures there is no
                // completion delivery after the requester has gone away.
                biased;
                _ = await_members_runtime.waiter_disconnects(&key), if !is_background => {
                    if cancel_disconnected_blocking_await(
                        &await_members_runtime,
                        &state,
                        member_statuses,
                    )
                    .await
                    {
                        return;
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    let summary = timeout_summary(&member_statuses);
                    finalize_await(&await_members_runtime, &state, false, member_statuses, summary).await;
                    return;
                }
                event = event_rx.recv() => {
                    match event {
                        Ok(event) => {
                            if event.swarm_id.as_deref() != Some(swarm_id.as_str()) {
                                continue;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            // Dropped events are recoverable: the loop re-reads
                            // member statuses from shared state at the top, so
                            // just keep watching instead of orphaning the wait.
                            crate::logging::info(&format!(
                                "await_members watcher lagged by {} swarm events; re-checking statuses",
                                n
                            ));
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            let _transaction = await_members_runtime.transaction_for_cancel(&key).await;
                            await_members_runtime.clear_active(&key).await;
                            await_members_runtime.forget_transaction(&key).await;
                            return;
                        }
                    }
                }
            }
        }
    });
}

pub(super) struct CommAwaitMembersContext<'a> {
    pub client_event_tx: &'a mpsc::UnboundedSender<ServerEvent>,
    pub swarm_members: &'a Arc<RwLock<HashMap<String, SwarmMember>>>,
    pub swarms_by_id: &'a Arc<RwLock<HashMap<String, HashSet<String>>>>,
    pub swarm_event_tx: &'a broadcast::Sender<SwarmEvent>,
    pub await_members_runtime: &'a AwaitMembersRuntime,
}

#[expect(
    clippy::too_many_arguments,
    reason = "await request carries protocol fields plus delivery flags; grouping would churn many call sites"
)]
pub(super) async fn handle_comm_await_members(
    id: u64,
    req_session_id: String,
    target_status: Vec<String>,
    requested_ids: Vec<String>,
    mode: Option<String>,
    timeout_secs: Option<u64>,
    background: bool,
    notify: bool,
    wake: bool,
    ctx: CommAwaitMembersContext<'_>,
) {
    let swarm_id = {
        let members = ctx.swarm_members.read().await;
        members
            .get(&req_session_id)
            .and_then(|member| member.swarm_id.clone())
    };

    if let Some(swarm_id) = swarm_id {
        let key = request_key(
            &req_session_id,
            &swarm_id,
            &requested_ids,
            &target_status,
            mode.as_deref(),
        );
        // This covers final-state replay, preference upsert, waiter
        // replacement, active ownership, and any immediate expiration claim.
        // A retry queued behind a terminal claimant observes that committed
        // result instead of writing its preferences over the final record.
        let transaction = ctx
            .await_members_runtime
            .transaction_for_request(&key)
            .await;
        let mut persisted = load_state(&key);

        let initial_statuses = awaited_member_statuses(
            &req_session_id,
            &swarm_id,
            &requested_ids,
            &target_status,
            ctx.swarm_members,
            ctx.swarms_by_id,
        )
        .await;

        if let Some(final_response) = persisted
            .as_ref()
            .and_then(|state| state.final_response.clone())
        {
            let current_still_satisfies =
                initial_statuses.is_empty() || mode_satisfied(&initial_statuses, mode.as_deref());
            if final_response.replayable && current_still_satisfies {
                let _ = ctx
                    .client_event_tx
                    .send(ServerEvent::CommAwaitMembersResponse {
                        id,
                        completed: final_response.completed,
                        members: final_response.members,
                        summary: final_response.summary,
                        background_started: false,
                    });
                ctx.await_members_runtime.forget_transaction(&key).await;
                return;
            }

            // Orphan cancellation records are durable for audit only. A retry
            // must create a pending state from current member status instead of
            // inheriting the cancellation until the final-response TTL expires.
            persisted = None;
        }

        if initial_statuses.is_empty() {
            let _ = ctx
                .client_event_tx
                .send(ServerEvent::CommAwaitMembersResponse {
                    id,
                    completed: true,
                    members: vec![],
                    summary: "No other members in swarm to wait for.".to_string(),
                    background_started: false,
                });
            return;
        }

        // Already satisfied right now: answer inline regardless of background
        // mode. There is nothing to wait for, so the agent should get the
        // result immediately instead of a "watching in background" stub.
        if mode_satisfied(&initial_statuses, mode.as_deref()) {
            let summary = mode_summary(&initial_statuses, mode.as_deref());
            let _ = ctx
                .client_event_tx
                .send(ServerEvent::CommAwaitMembersResponse {
                    id,
                    completed: true,
                    members: initial_statuses,
                    summary,
                    background_started: false,
                });
            return;
        }

        let requested_deadline = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
            + Duration::from_secs(timeout_secs.unwrap_or(3600)).as_millis() as u64;
        let was_blocking = persisted.as_ref().is_some_and(|state| !state.background);
        // Re-enter the state upsert even when the semantic key already exists:
        // its active delivery policy belongs to the latest retry, while the
        // original deadline and sole watcher remain unchanged.
        let state = ensure_pending_state(
            &key,
            &req_session_id,
            &swarm_id,
            &requested_ids,
            &target_status,
            mode.as_deref(),
            requested_deadline,
            background,
            notify,
            wake,
        );

        let already_expired = state.deadline_unix_ms
            <= SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

        // Background mode: hand off to a detached watcher and answer the tool
        // immediately so the requesting turn stays responsive. Completion is
        // delivered later via notify/wake.
        if background {
            // A duplicate can promote an inline request to detached delivery.
            // Replace old socket waiters now, so exactly one watcher owns this
            // semantic key and no blocking caller waits for a second terminal
            // response. The caller making this request receives the normal
            // direct response below, including when it reused the same id.
            if was_blocking {
                let summary = background_started_summary(&initial_statuses, mode.as_deref(), wake);
                for (waiter_id, waiter_tx) in ctx.await_members_runtime.take_waiters(&key).await {
                    if waiter_id != id {
                        let _ = waiter_tx.send(ServerEvent::CommAwaitMembersResponse {
                            id: waiter_id,
                            completed: false,
                            members: initial_statuses.clone(),
                            summary: summary.clone(),
                            background_started: true,
                        });
                    }
                }
            }
            if already_expired {
                let summary = timeout_summary(&initial_statuses);
                drop(transaction);
                finalize_await(
                    ctx.await_members_runtime,
                    &state,
                    false,
                    initial_statuses.clone(),
                    summary.clone(),
                )
                .await;
                // Answer the requesting tool call directly: no waiter was
                // registered for this request (waiters are only added in the
                // blocking branch), so without this the socket call would hang
                // until its client-side timeout.
                let _ = ctx
                    .client_event_tx
                    .send(ServerEvent::CommAwaitMembersResponse {
                        id,
                        completed: false,
                        members: initial_statuses,
                        summary,
                        background_started: false,
                    });
                return;
            }

            if ctx.await_members_runtime.mark_active_if_new(&key).await {
                publish_await_started_card(&state, &initial_statuses);
                spawn_or_resume_await_members(
                    state,
                    req_session_id,
                    ctx.swarm_members.clone(),
                    ctx.swarms_by_id.clone(),
                    ctx.swarm_event_tx.clone(),
                    ctx.await_members_runtime.clone(),
                )
                .await;
            }

            let summary = background_started_summary(&initial_statuses, mode.as_deref(), wake);
            let _ = ctx
                .client_event_tx
                .send(ServerEvent::CommAwaitMembersResponse {
                    id,
                    completed: false,
                    members: initial_statuses,
                    summary,
                    background_started: true,
                });
            return;
        }

        // Blocking mode: register a socket waiter that the watcher resolves.
        ctx.await_members_runtime
            .add_waiter(&key, id, ctx.client_event_tx)
            .await;

        if already_expired {
            let summary = timeout_summary(&initial_statuses);
            drop(transaction);
            finalize_await(
                ctx.await_members_runtime,
                &state,
                false,
                initial_statuses,
                summary,
            )
            .await;
            return;
        }

        if ctx.await_members_runtime.mark_active_if_new(&key).await {
            spawn_or_resume_await_members(
                state,
                req_session_id,
                ctx.swarm_members.clone(),
                ctx.swarms_by_id.clone(),
                ctx.swarm_event_tx.clone(),
                ctx.await_members_runtime.clone(),
            )
            .await;
        }
    } else {
        let _ = ctx.client_event_tx.send(ServerEvent::Error {
            id,
            message: "Not in a swarm. Use a git repository to enable swarm features.".to_string(),
            retry_after_secs: None,
        });
    }
}

/// One-line summary returned to the tool when a wait is handed off to a
/// background watcher.
fn background_started_summary(
    member_statuses: &[AwaitedMemberStatus],
    mode: Option<&str>,
    wake: bool,
) -> String {
    let pending: Vec<String> = member_statuses
        .iter()
        .filter(|member| !member.done)
        .map(short_member_name)
        .collect();
    let scope = match completion_mode(mode) {
        "any" => "any of",
        _ => "all of",
    };
    let delivery = if wake {
        "You'll be woken with the result when it resolves."
    } else {
        "A notification will appear when it resolves."
    };
    if pending.is_empty() {
        format!("Watching swarm members in the background. {}", delivery)
    } else {
        format!(
            "Watching {} {} in the background. {}",
            scope,
            pending.join(", "),
            delivery
        )
    }
}

/// Emit a swarm-flavored "await started" activity card so attached clients show
/// that a background watcher is now running for this session.
fn publish_await_started_card(
    state: &PersistedAwaitMembersState,
    member_statuses: &[AwaitedMemberStatus],
) {
    if !state.notify {
        return;
    }
    let pending: Vec<String> = member_statuses
        .iter()
        .filter(|member| !member.done)
        .map(short_member_name)
        .collect();
    let watching = if pending.is_empty() {
        "swarm members".to_string()
    } else {
        pending.join(", ")
    };
    Bus::global().publish(BusEvent::UiActivity(UiActivity::background(
        Some(state.session_id.clone()),
        format!(
            "🐝 **Swarm await started** · watching `{}`\n\nJcode is waiting for these members in the background and will report back when they finish.",
            watching
        ),
        Some(format!("Swarm await started · {}", watching)),
    )));
}

/// Cancel pending `await_members` watches after a server (re)start. Neither a
/// blocking socket nor an in-process detached task survives a reload, so both
/// delivery modes are orphaned. The non-replayable audit record ensures a retry
/// evaluates current member state with a fresh deadline and never emits a stale
/// completion wake/notification from the pre-reload request.
pub(super) async fn resume_background_awaits(
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
    swarm_event_tx: &broadcast::Sender<SwarmEvent>,
    await_members_runtime: &AwaitMembersRuntime,
) {
    let pending = all_pending_await_members_including_expired();
    let _ = (swarm_members, swarms_by_id, swarm_event_tx);
    let mut cancelled_orphans = 0usize;
    for state in pending {
        let _transaction = await_members_runtime
            .transaction_for_cancel(&state.key)
            .await;
        if let Some(pending) = load_state(&state.key).filter(PersistedAwaitMembersState::is_pending)
        {
            let _ = persist_non_replayable_final_response(
                &pending,
                false,
                Vec::new(),
                "Await cancelled because reload orphaned its watcher. Rerun the await if it is still needed; the retry uses current member state and a fresh timeout."
                    .to_string(),
            );
            await_members_runtime.forget_transaction(&state.key).await;
            cancelled_orphans += 1;
        }
    }

    if cancelled_orphans > 0 {
        crate::logging::info(&format!(
            "Cancelled {} reload-orphaned swarm await watcher(s)",
            cancelled_orphans
        ));
    }
}
