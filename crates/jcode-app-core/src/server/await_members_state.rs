use crate::protocol::{AwaitedMemberStatus, ServerEvent};
use crate::server::durable_state::{
    hashed_request_key, load_json_state, now_unix_ms, save_json_state, state_dir,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, mpsc};

const AWAIT_MEMBERS_DIR: &str = "jcode-await-members";
const FINAL_STATE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const PENDING_STATE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedAwaitMembersResult {
    pub completed: bool,
    pub members: Vec<AwaitedMemberStatus>,
    pub summary: String,
    pub resolved_at_unix_ms: u64,
    /// Cancellation records created while restoring an orphaned await are kept
    /// for audit, but a later requester retry must evaluate live member state.
    #[serde(default = "default_true")]
    pub replayable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedAwaitMembersState {
    pub key: String,
    pub session_id: String,
    pub swarm_id: String,
    pub target_status: Vec<String>,
    pub requested_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub created_at_unix_ms: u64,
    pub deadline_unix_ms: u64,
    /// Active delivery policy for the semantic wait. It is deliberately not
    /// part of `request_key`: a retry updates this policy in place while the
    /// same watcher continues to own the target and deadline.
    #[serde(default)]
    pub background: bool,
    /// Surface a completion notification card to attached clients.
    #[serde(default = "default_true")]
    pub notify: bool,
    /// Wake an idle requesting agent on completion (or soft-interrupt if busy).
    #[serde(default = "default_true")]
    pub wake: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_response: Option<PersistedAwaitMembersResult>,
}

fn default_true() -> bool {
    true
}

impl PersistedAwaitMembersState {
    pub fn is_pending(&self) -> bool {
        self.final_response.is_none()
    }

    pub fn remaining_timeout(&self) -> Duration {
        let now = now_unix_ms();
        Duration::from_millis(self.deadline_unix_ms.saturating_sub(now))
    }
}

#[derive(Clone)]
struct AwaitMembersWaiter {
    request_id: u64,
    client_event_tx: mpsc::UnboundedSender<ServerEvent>,
}

#[derive(Clone, Default)]
pub(crate) struct AwaitMembersRuntime {
    active_keys: Arc<RwLock<HashSet<String>>>,
    waiters: Arc<RwLock<HashMap<String, Vec<AwaitMembersWaiter>>>>,
}

impl AwaitMembersRuntime {
    pub(super) async fn add_waiter(
        &self,
        key: &str,
        request_id: u64,
        client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
    ) {
        let mut waiters = self.waiters.write().await;
        let entries = waiters.entry(key.to_string()).or_default();
        // A transport retry can reach the server before the original blocking
        // call resolves. It is the same waiter, not another completion that
        // should re-enter the coordinator, so replace it in place.
        if let Some(waiter) = entries
            .iter_mut()
            .find(|waiter| waiter.request_id == request_id)
        {
            waiter.client_event_tx = client_event_tx.clone();
        } else {
            entries.push(AwaitMembersWaiter {
                request_id,
                client_event_tx: client_event_tx.clone(),
            });
        }
    }

    pub(super) async fn mark_active_if_new(&self, key: &str) -> bool {
        let mut active = self.active_keys.write().await;
        active.insert(key.to_string())
    }

    pub(super) async fn clear_active(&self, key: &str) {
        self.active_keys.write().await.remove(key);
    }

    pub(super) async fn retain_open_waiters(&self, key: &str) -> usize {
        let mut waiters = self.waiters.write().await;
        let Some(entries) = waiters.get_mut(key) else {
            return 0;
        };
        entries.retain(|waiter| !waiter.client_event_tx.is_closed());
        let remaining = entries.len();
        if remaining == 0 {
            waiters.remove(key);
        }
        remaining
    }

    /// Resolve as soon as one currently registered blocking requester loses its
    /// receiving half. The watcher always follows this with
    /// `retain_open_waiters`, which is the authoritative cleanup/ownership
    /// operation and handles a concurrent replacement or another live waiter.
    pub(super) async fn waiter_disconnects(&self, key: &str) {
        let waiters = self
            .waiters
            .read()
            .await
            .get(key)
            .cloned()
            .unwrap_or_default();

        if waiters.is_empty() {
            return;
        }

        futures::future::select_all(
            waiters
                .iter()
                .map(|waiter| Box::pin(waiter.client_event_tx.closed()))
                .collect::<Vec<_>>(),
        )
        .await;
    }

    pub(super) async fn take_waiters(
        &self,
        key: &str,
    ) -> Vec<(u64, mpsc::UnboundedSender<ServerEvent>)> {
        self.waiters
            .write()
            .await
            .remove(key)
            .unwrap_or_default()
            .into_iter()
            .map(|waiter| (waiter.request_id, waiter.client_event_tx))
            .collect()
    }
}

fn is_stale(state: &PersistedAwaitMembersState) -> bool {
    let now = now_unix_ms();
    if let Some(final_response) = &state.final_response {
        now.saturating_sub(final_response.resolved_at_unix_ms) > FINAL_STATE_TTL.as_millis() as u64
    } else {
        now.saturating_sub(state.deadline_unix_ms) > PENDING_STATE_TTL.as_millis() as u64
    }
}

pub(super) fn request_key(
    session_id: &str,
    swarm_id: &str,
    requested_ids: &[String],
    target_status: &[String],
    mode: Option<&str>,
) -> String {
    let mut requested = requested_ids.to_vec();
    requested.sort();

    let mut target = target_status.to_vec();
    target.sort();

    hashed_request_key(
        session_id,
        "await_members",
        &[
            swarm_id.to_string(),
            requested.join("\u{1f}"),
            target.join("\u{1f}"),
            mode.unwrap_or("all").to_string(),
        ],
    )
}

pub(super) fn load_state(key: &str) -> Option<PersistedAwaitMembersState> {
    load_json_state(AWAIT_MEMBERS_DIR, key, is_stale)
}

pub(super) fn save_state(state: &PersistedAwaitMembersState) {
    save_json_state(AWAIT_MEMBERS_DIR, &state.key, state, "await_members state")
}

#[expect(
    clippy::too_many_arguments,
    reason = "pending await state mirrors persisted fields and existing call sites"
)]
pub(super) fn ensure_pending_state(
    key: &str,
    session_id: &str,
    swarm_id: &str,
    requested_ids: &[String],
    target_status: &[String],
    mode: Option<&str>,
    deadline_unix_ms: u64,
    background: bool,
    notify: bool,
    wake: bool,
) -> PersistedAwaitMembersState {
    if let Some(mut existing) = load_state(key).filter(PersistedAwaitMembersState::is_pending) {
        // Delivery flags are active mutable policy, not wait identity. The
        // atomic durable replacement performed by save_state makes the latest
        // duplicate request authoritative for the sole existing watcher.
        if existing.background != background || existing.notify != notify || existing.wake != wake {
            existing.background = background;
            existing.notify = notify;
            existing.wake = wake;
            save_state(&existing);
        }
        return existing;
    }

    let state = PersistedAwaitMembersState {
        key: key.to_string(),
        session_id: session_id.to_string(),
        swarm_id: swarm_id.to_string(),
        target_status: target_status.to_vec(),
        requested_ids: requested_ids.to_vec(),
        mode: mode.map(str::to_string),
        created_at_unix_ms: now_unix_ms(),
        deadline_unix_ms,
        background,
        notify,
        wake,
        final_response: None,
    };
    save_state(&state);
    state
}

pub(super) fn persist_final_response(
    state: &PersistedAwaitMembersState,
    completed: bool,
    members: Vec<AwaitedMemberStatus>,
    summary: String,
) -> PersistedAwaitMembersState {
    let mut next = state.clone();
    next.final_response = Some(PersistedAwaitMembersResult {
        completed,
        members,
        summary,
        resolved_at_unix_ms: now_unix_ms(),
        replayable: true,
    });
    save_state(&next);
    next
}

pub(super) fn persist_non_replayable_final_response(
    state: &PersistedAwaitMembersState,
    completed: bool,
    members: Vec<AwaitedMemberStatus>,
    summary: String,
) -> PersistedAwaitMembersState {
    let mut next = persist_final_response(state, completed, members, summary);
    next.final_response
        .as_mut()
        .expect("persisted final response exists")
        .replayable = false;
    save_state(&next);
    next
}

pub fn pending_await_members_for_session(session_id: &str) -> Vec<PersistedAwaitMembersState> {
    let mut pending: Vec<PersistedAwaitMembersState> = all_pending_await_members()
        .into_iter()
        .filter(|state| state.session_id == session_id)
        .collect();
    pending.sort_by_key(|state| state.deadline_unix_ms);
    pending
}

/// Load every still-pending await state across all sessions, pruning stale
/// files as a side effect. Used both for per-session lookups and for resuming
/// backgrounded watchers after a server reload.
pub(super) fn all_pending_await_members() -> Vec<PersistedAwaitMembersState> {
    let now = now_unix_ms();
    all_pending_await_members_including_expired()
        .into_iter()
        .filter(|state| state.deadline_unix_ms > now)
        .collect()
}

/// Like [`all_pending_await_members`], but also returns pending states whose
/// deadline has already passed (still within the pending TTL). Startup resume
/// uses this so background awaits that expired while the server was down can
/// be finalized with a timeout instead of silently dropping the promised
/// notify/wake.
pub(super) fn all_pending_await_members_including_expired() -> Vec<PersistedAwaitMembersState> {
    let dir = state_dir(AWAIT_MEMBERS_DIR);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut pending = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let Ok(state) = crate::storage::read_json::<PersistedAwaitMembersState>(&path) else {
            continue;
        };
        if is_stale(&state) {
            let _ = std::fs::remove_file(path);
            continue;
        }
        if state.is_pending() {
            pending.push(state);
        }
    }

    pending
}
