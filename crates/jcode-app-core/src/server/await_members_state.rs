use crate::protocol::{AwaitedMemberStatus, ServerEvent};
use crate::server::durable_state::{
    hashed_request_key, load_json_state, now_unix_ms, save_json_state, state_dir,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock, mpsc};

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
    /// Monotonic incarnation of this semantic wait. The semantic key remains
    /// stable across preference retries, while this persisted generation fences
    /// a reload orphan snapshot from cancelling a newer requester.
    #[serde(default)]
    pub request_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_response: Option<PersistedAwaitMembersResult>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum AwaitRequestGenerationError {
    Exhausted,
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
    /// Every in-process operation that can change the durable state or waiter
    /// set for one semantic wait uses this lock. The state file is then written
    /// while that operation still owns the lock, so a preference retry cannot
    /// race a terminal claim with independent read-modify-write snapshots.
    /// Weak registry entries cannot keep dead semantic keys alive. Each
    /// transaction lease owns the strong lock reference and removes its exact
    /// registry entry after releasing the final lock holder. The std mutex is
    /// only held for a map lookup/update, never across an await.
    transactions: Arc<StdMutex<HashMap<String, Weak<Mutex<()>>>>>,
    #[cfg(test)]
    transaction_queue_pause: Arc<Mutex<Option<AwaitTransactionQueuePause>>>,
    #[cfg(test)]
    transaction_pause: Arc<Mutex<Option<AwaitTransactionPause>>>,
    #[cfg(test)]
    reload_orphan_pause: Arc<Mutex<Option<AwaitReloadOrphanPause>>>,
}

/// Owns the strong lock reference from registration until a transaction either
/// completes or is cancelled. This is deliberately created before polling
/// `lock_owned`: cancellation while queued must still clean the matching weak
/// registry entry once no other operation retains this lock.
struct AwaitTransactionRegistration {
    registry: Arc<StdMutex<HashMap<String, Weak<Mutex<()>>>>>,
    key: String,
    lock: Arc<Mutex<()>>,
}

impl AwaitTransactionRegistration {
    fn install(registry: Arc<StdMutex<HashMap<String, Weak<Mutex<()>>>>>, key: &str) -> Self {
        let lock = {
            let mut transactions = registry
                .lock()
                .expect("await transaction registry mutex poisoned");
            match transactions.get(key).and_then(Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(Mutex::new(()));
                    transactions.insert(key.to_string(), Arc::downgrade(&lock));
                    lock
                }
            }
        };
        Self {
            registry,
            key: key.to_string(),
            lock,
        }
    }
}

impl Drop for AwaitTransactionRegistration {
    fn drop(&mut self) {
        let mut registry = self
            .registry
            .lock()
            .expect("await transaction registry mutex poisoned");
        let matches_this_lock = registry
            .get(&self.key)
            .is_some_and(|registered| registered.ptr_eq(&Arc::downgrade(&self.lock)));
        // The registration itself contributes the final strong reference only
        // when no queued acquirer, acquired lease, or owned mutex guard remains.
        // Pointer identity prevents an old lease from removing a newer lock.
        if matches_this_lock && Arc::strong_count(&self.lock) == 1 {
            registry.remove(&self.key);
        }
    }
}

/// Couples the pre-acquisition registration with the lock future. Field order
/// is intentional: cancellation drops `acquire` before `registration`, so the
/// temporary Arc owned by `lock_owned` cannot leave a dead weak entry behind.
struct AwaitTransactionAcquisition {
    acquire: Pin<Box<dyn Future<Output = OwnedMutexGuard<()>> + Send>>,
    registration: Option<AwaitTransactionRegistration>,
    #[cfg(test)]
    queue_pause: Option<AwaitTransactionQueuePause>,
}

impl AwaitTransactionAcquisition {
    fn new(
        registry: Arc<StdMutex<HashMap<String, Weak<Mutex<()>>>>>,
        key: &str,
        #[cfg(test)] queue_pause: Option<AwaitTransactionQueuePause>,
    ) -> Self {
        let registration = AwaitTransactionRegistration::install(registry, key);
        let acquire = Box::pin(registration.lock.clone().lock_owned());
        Self {
            acquire,
            registration: Some(registration),
            #[cfg(test)]
            queue_pause,
        }
    }
}

impl Future for AwaitTransactionAcquisition {
    type Output = (AwaitTransactionRegistration, OwnedMutexGuard<()>);

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.as_mut().get_mut();
        match this.acquire.as_mut().poll(cx) {
            Poll::Ready(guard) => Poll::Ready((
                this.registration
                    .take()
                    .expect("await transaction registration present until acquisition"),
                guard,
            )),
            Poll::Pending => {
                #[cfg(test)]
                if let Some(pause) = &this.queue_pause {
                    pause.entered.notify_one();
                }
                Poll::Pending
            }
        }
    }
}

/// A per-key transaction remains registered for every queued/acquired lease.
/// Dropping the final lease removes the matching weak entry only after its
/// mutex guard is gone, so a concurrent acquirer cannot receive a second lock
/// for the same key.
pub(super) struct AwaitTransactionLease {
    registration: Option<AwaitTransactionRegistration>,
    guard: Option<OwnedMutexGuard<()>>,
}

impl Drop for AwaitTransactionLease {
    fn drop(&mut self) {
        // `OwnedMutexGuard` retains a strong Arc. Release it before testing
        // whether this lease is the last operation still associated with key.
        self.guard.take();
        // Drop the composed registration exactly once after the guard. Its
        // Drop implementation conditionally removes the matching weak entry.
        self.registration.take();
    }
}

#[cfg(test)]
impl AwaitTransactionLease {
    pub(super) fn lock_identity(&self) -> usize {
        Arc::as_ptr(
            &self
                .registration
                .as_ref()
                .expect("live transaction lease retains its registration")
                .lock,
        ) as usize
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AwaitTransactionOperation {
    Request,
    Finalize,
    Cancel,
}

#[cfg(test)]
#[derive(Clone)]
struct AwaitTransactionPause {
    operation: AwaitTransactionOperation,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[cfg(test)]
#[derive(Clone)]
struct AwaitTransactionQueuePause {
    entered: Arc<tokio::sync::Notify>,
}

#[cfg(test)]
#[derive(Clone)]
struct AwaitReloadOrphanPause {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl AwaitMembersRuntime {
    /// Acquire the sole in-process transaction for `key`. The durable request
    /// generation is authoritative across reload/process boundaries, while
    /// this lease linearizes the current process's state and waiter mutations.
    async fn transaction(
        &self,
        key: &str,
        #[cfg(test)] operation: AwaitTransactionOperation,
    ) -> AwaitTransactionLease {
        #[cfg(test)]
        let queue_pause = self.transaction_queue_pause.lock().await.clone();
        let (registration, guard) = AwaitTransactionAcquisition::new(
            self.transactions.clone(),
            key,
            #[cfg(test)]
            queue_pause,
        )
        .await;
        // Compose the pre-acquisition registration and owned guard before any
        // later await. A cancellation after lock acquisition therefore uses
        // the normal lease cleanup ordering rather than split local drops.
        let lease = AwaitTransactionLease {
            registration: Some(registration),
            guard: Some(guard),
        };
        #[cfg(test)]
        if let Some(pause) = self.transaction_pause.lock().await.clone()
            && pause.operation == operation
        {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
        lease
    }

    pub(super) async fn transaction_for_request(&self, key: &str) -> AwaitTransactionLease {
        self.transaction(
            key,
            #[cfg(test)]
            AwaitTransactionOperation::Request,
        )
        .await
    }

    pub(super) async fn transaction_for_finalize(&self, key: &str) -> AwaitTransactionLease {
        self.transaction(
            key,
            #[cfg(test)]
            AwaitTransactionOperation::Finalize,
        )
        .await
    }

    pub(super) async fn transaction_for_cancel(&self, key: &str) -> AwaitTransactionLease {
        self.transaction(
            key,
            #[cfg(test)]
            AwaitTransactionOperation::Cancel,
        )
        .await
    }

    #[cfg(test)]
    pub(super) async fn pause_transaction_after_acquire(
        &self,
        operation: AwaitTransactionOperation,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    ) {
        *self.transaction_pause.lock().await = Some(AwaitTransactionPause {
            operation,
            entered,
            release,
        });
    }

    #[cfg(test)]
    pub(super) async fn pause_transaction_while_queued(&self, entered: Arc<tokio::sync::Notify>) {
        *self.transaction_queue_pause.lock().await = Some(AwaitTransactionQueuePause { entered });
    }

    #[cfg(test)]
    pub(super) async fn clear_transaction_queue_pause(&self) {
        *self.transaction_queue_pause.lock().await = None;
    }

    #[cfg(test)]
    pub(super) async fn clear_transaction_pause(&self) {
        *self.transaction_pause.lock().await = None;
    }

    #[cfg(test)]
    pub(super) async fn pause_reload_orphan_after_snapshot(
        &self,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    ) {
        *self.reload_orphan_pause.lock().await = Some(AwaitReloadOrphanPause { entered, release });
    }

    #[cfg(test)]
    pub(super) async fn clear_reload_orphan_pause(&self) {
        *self.reload_orphan_pause.lock().await = None;
    }

    #[cfg(test)]
    pub(super) async fn wait_after_reload_orphan_snapshot(&self) {
        if let Some(pause) = self.reload_orphan_pause.lock().await.clone() {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
    }

    #[cfg(not(test))]
    pub(super) async fn wait_after_reload_orphan_snapshot(&self) {}

    #[cfg(test)]
    /// Count all stored entries without retaining or upgrading weak pointers.
    /// This exposes tombstones directly to lifecycle tests instead of pruning
    /// them as an observation side effect.
    pub(super) fn transaction_registry_raw_len(&self) -> usize {
        self.transactions
            .lock()
            .expect("await transaction registry mutex poisoned")
            .len()
    }

    #[cfg(test)]
    pub(super) fn transaction_registry_len(&self) -> usize {
        self.transaction_registry_raw_len()
    }

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
) -> Result<PersistedAwaitMembersState, AwaitRequestGenerationError> {
    let previous = load_state(key);
    if let Some(mut existing) = previous
        .clone()
        .filter(PersistedAwaitMembersState::is_pending)
    {
        // Every retry gets a new durable incarnation even when its delivery
        // flags happen to match. This fences a reload cleanup snapshot taken
        // before the retry. `checked_add` deliberately refuses wraparound: a
        // wrapped value could make a stale snapshot look current again.
        existing.request_generation = existing
            .request_generation
            .checked_add(1)
            .ok_or(AwaitRequestGenerationError::Exhausted)?;
        // Delivery flags are active mutable policy, not wait identity. The
        // caller holds AwaitMembersRuntime's semantic-key transaction while
        // replacing this durable record, so the latest duplicate request is
        // authoritative for the sole existing watcher.
        if existing.background != background || existing.notify != notify || existing.wake != wake {
            existing.background = background;
            existing.notify = notify;
            existing.wake = wake;
        }
        save_state(&existing);
        return Ok(existing);
    }

    // A non-replayable reload audit is deliberately replaced by a fresh
    // pending watcher on retry, but it still carries the last incarnation.
    // Advance it rather than resetting to one so an older reload snapshot
    // cannot mistake the replacement for its orphan.
    let request_generation = match previous {
        Some(state) => state
            .request_generation
            .checked_add(1)
            .ok_or(AwaitRequestGenerationError::Exhausted)?,
        None => 1,
    };
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
        request_generation,
        final_response: None,
    };
    save_state(&state);
    Ok(state)
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
