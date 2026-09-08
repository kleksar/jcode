//! Server-authoritative working-directory context for clean session creation.
//!
//! This module deliberately reads only the durable compact session metadata
//! index. It never reads session transcripts, locks an Agent, changes the
//! process cwd, or trusts a client-provided local path as server metadata.

use crate::agent::Agent;
use crate::protocol::{ServerEvent, SessionRuntimeSelection};
use crate::provider::Provider;
use crate::recent_session_index::{self, RecentSessionMetadata};
use crate::tool::Registry;
use anyhow::Result;
use jcode_agent_runtime::InterruptSignal;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::sync::{Mutex, RwLock, Semaphore};

use super::{
    SessionInterruptQueues, register_background_tool_signal, register_session_interrupt_queue,
};

type SessionAgents = Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>;

pub(super) const MAX_RECENT_WORKING_DIRS: usize = 5;
const RECENT_METADATA_SCAN_LIMIT: usize = MAX_RECENT_WORKING_DIRS * 4;
const MAX_DIRECTORY_COMPLETION_PATH_BYTES: usize = 4096;
const MAX_DIRECTORY_COMPLETION_RESULTS: usize = 100;
const MAX_DIRECTORY_COMPLETION_ENTRIES: usize = 4096;
const DIRECTORY_COMPLETION_SCAN_BUDGET: Duration = Duration::from_millis(150);
const DIRECTORY_COMPLETION_CACHE_TTL: Duration = Duration::from_secs(2);
const MAX_DIRECTORY_COMPLETION_CACHE_ENTRIES: usize = 32;
const MAX_DIRECTORY_COMPLETION_TASKS: usize = 2;

#[derive(Clone)]
struct DirectoryCompletionCacheEntry {
    parent: PathBuf,
    names: Vec<String>,
    truncated: bool,
    created_at: Instant,
}

#[derive(Default)]
struct DirectoryCompletionCache {
    entries: VecDeque<DirectoryCompletionCacheEntry>,
}

impl DirectoryCompletionCache {
    fn names_for(&mut self, parent: &Path) -> Option<(Vec<String>, bool)> {
        let position = self
            .entries
            .iter()
            .position(|entry| entry.parent == parent)?;
        let entry = self.entries.remove(position)?;
        if entry.created_at.elapsed() > DIRECTORY_COMPLETION_CACHE_TTL {
            return None;
        }
        let result = (entry.names.clone(), entry.truncated);
        self.entries.push_front(entry);
        Some(result)
    }

    fn insert(&mut self, entry: DirectoryCompletionCacheEntry) {
        self.entries
            .retain(|existing| existing.parent != entry.parent);
        self.entries.push_front(entry);
        self.entries
            .truncate(MAX_DIRECTORY_COMPLETION_CACHE_ENTRIES);
    }
}

fn directory_completion_cache() -> &'static StdMutex<DirectoryCompletionCache> {
    static CACHE: OnceLock<StdMutex<DirectoryCompletionCache>> = OnceLock::new();
    CACHE.get_or_init(|| StdMutex::new(DirectoryCompletionCache::default()))
}

fn directory_completion_semaphore() -> Arc<Semaphore> {
    static SEMAPHORE: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Arc::clone(SEMAPHORE.get_or_init(|| Arc::new(Semaphore::new(MAX_DIRECTORY_COMPLETION_TASKS))))
}

/// Resolve a user-entered working directory against this server's Home.
///
/// Only absolute paths, `~`, and `~/...` are accepted. In particular, this
/// refuses relative paths instead of silently falling back to daemon cwd.
pub(super) fn canonicalize_new_session_working_dir(raw: &str) -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or_else(|| "Server Home is unavailable".to_string())?;
    canonicalize_new_session_working_dir_from_home(raw, &home)
}

fn canonicalize_new_session_working_dir_from_home(
    raw: &str,
    home: &Path,
) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("Working directory does not exist".to_string());
    }

    let path = if raw == "~" {
        home.to_path_buf()
    } else if let Some(child) = raw.strip_prefix("~/") {
        // `Path::join` would discard Home for an absolute child such as `~//x`.
        // That spelling is not a valid leading-home form.
        if child.starts_with('/') {
            return Err("Working directory must be an absolute path or start with ~/".to_string());
        }
        home.join(child)
    } else if raw.starts_with('~') || !Path::new(raw).is_absolute() {
        return Err("Working directory must be an absolute path or start with ~/".to_string());
    } else {
        PathBuf::from(raw)
    };

    let canonical = std::fs::canonicalize(&path).map_err(path_error)?;
    let metadata = std::fs::metadata(&canonical).map_err(path_error)?;
    if !metadata.is_dir() {
        return Err("Working directory is not a directory".to_string());
    }
    // Opening a directory is a portable, cwd-independent access check. This
    // also avoids treating an arbitrary local client row as usable server data.
    std::fs::read_dir(&canonical).map_err(|_| "Working directory is not accessible".to_string())?;
    Ok(canonical)
}

fn path_error(error: std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "Working directory does not exist".to_string(),
        std::io::ErrorKind::PermissionDenied => "Working directory is not accessible".to_string(),
        _ => "Working directory is not accessible".to_string(),
    }
}

/// Returns canonical, accessible unique directories in compact-index recency
/// order. Invalid or unavailable historic paths are deliberately omitted.
fn recent_working_dirs(entries: &[RecentSessionMetadata], home: &Path) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut dirs = Vec::new();
    for entry in entries {
        let Some(raw) = entry.working_dir.as_deref() else {
            continue;
        };
        let Ok(path) = canonicalize_new_session_working_dir_from_home(raw, home) else {
            continue;
        };
        let path = path.to_string_lossy().into_owned();
        if seen.insert(path.clone()) {
            dirs.push(path);
            if dirs.len() == MAX_RECENT_WORKING_DIRS {
                break;
            }
        }
    }
    dirs
}

fn session_creation_context_event(
    id: u64,
    home: &Path,
    entries: &[RecentSessionMetadata],
) -> ServerEvent {
    ServerEvent::SessionCreationContext {
        id,
        home_dir: home.to_string_lossy().into_owned(),
        recent_working_dirs: recent_working_dirs(entries, home),
    }
}

pub(super) async fn handle_get_session_creation_context(
    id: u64,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) -> Result<()> {
    let Some(home) = dirs::home_dir() else {
        send_error(id, "Server Home is unavailable", client_event_tx);
        return Ok(());
    };
    let home = match canonicalize_new_session_working_dir_from_home("~", &home) {
        Ok(home) => home,
        Err(message) => {
            send_error(id, &message, client_event_tx);
            return Ok(());
        }
    };

    // This is an indexed SQLite metadata query, never a session/transcript scan.
    let entries = match tokio::task::spawn_blocking(|| {
        recent_session_index::recent(RECENT_METADATA_SCAN_LIMIT)
    })
    .await
    {
        Ok(Ok(entries)) => entries,
        Ok(Err(error)) => {
            crate::logging::warn(&format!(
                "session creation metadata index unavailable: {error}"
            ));
            send_error(
                id,
                "Session creation context is temporarily unavailable",
                client_event_tx,
            );
            return Ok(());
        }
        Err(error) => {
            crate::logging::warn(&format!("session creation metadata task failed: {error}"));
            send_error(
                id,
                "Session creation context is temporarily unavailable",
                client_event_tx,
            );
            return Ok(());
        }
    };

    let _ = client_event_tx.send(session_creation_context_event(id, &home, &entries));
    Ok(())
}

pub(super) async fn handle_resolve_working_directory(
    id: u64,
    raw_path: &str,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) -> Result<()> {
    match canonicalize_new_session_working_dir(raw_path) {
        Ok(path) => {
            let _ = client_event_tx.send(ServerEvent::WorkingDirectoryResolved {
                id,
                input: raw_path.to_string(),
                absolute_path: path.to_string_lossy().into_owned(),
            });
        }
        Err(message) => send_error(id, &message, client_event_tx),
    }
    Ok(())
}

struct DirectoryCompletionQuery {
    parent: PathBuf,
    rendered_parent: String,
    prefix: String,
}

fn directory_completion_query(raw: &str, home: &Path) -> Result<DirectoryCompletionQuery, String> {
    if raw.len() > MAX_DIRECTORY_COMPLETION_PATH_BYTES {
        return Err("Working directory path is too long".to_string());
    }
    if raw.is_empty() {
        return Err("Working directory must be an absolute path or start with ~/".to_string());
    }

    let (filesystem_path, rendered_path) = if raw == "~" {
        (home.to_path_buf(), "~/".to_string())
    } else if let Some(child) = raw.strip_prefix("~/") {
        if child.starts_with('/') {
            return Err("Working directory must be an absolute path or start with ~/".to_string());
        }
        (home.join(child), raw.to_string())
    } else if raw.starts_with('~') || !Path::new(raw).is_absolute() {
        return Err("Working directory must be an absolute path or start with ~/".to_string());
    } else {
        (PathBuf::from(raw), raw.to_string())
    };

    let has_trailing_slash = rendered_path.ends_with('/');
    let (parent, prefix, rendered_parent) = if has_trailing_slash {
        (filesystem_path, String::new(), rendered_path)
    } else {
        let name = filesystem_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "Working directory is not accessible".to_string())?;
        let parent = filesystem_path
            .parent()
            .ok_or_else(|| {
                "Working directory must be an absolute path or start with ~/".to_string()
            })?
            .to_path_buf();
        let rendered_parent = rendered_path
            .rsplit_once('/')
            .map(|(parent, _)| format!("{parent}/"))
            .unwrap_or_else(|| "/".to_string());
        (parent, name.to_string(), rendered_parent)
    };

    Ok(DirectoryCompletionQuery {
        parent,
        rendered_parent,
        prefix,
    })
}

fn scan_directory_completion_parent(parent: &Path) -> Result<(Vec<String>, bool), String> {
    let entries = std::fs::read_dir(parent).map_err(path_error)?;
    let started = Instant::now();
    let mut names = Vec::new();
    let mut truncated = false;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_DIRECTORY_COMPLETION_ENTRIES
            || started.elapsed() >= DIRECTORY_COMPLETION_SCAN_BUDGET
        {
            truncated = true;
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        // Querying the entry path follows a symlink, so a symlink to a directory
        // is a valid completion while dangling links and links to files are
        // excluded.
        if std::fs::metadata(entry.path())
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false)
        {
            names.push(name.to_string());
        }
    }
    names.sort_unstable();
    Ok((names, truncated))
}

fn complete_working_directory_sync(
    raw: &str,
    requested_limit: usize,
) -> Result<(Vec<String>, bool), String> {
    let home = dirs::home_dir().ok_or_else(|| "Server Home is unavailable".to_string())?;
    let query = directory_completion_query(raw, &home)?;
    let cached = {
        directory_completion_cache()
            .lock()
            .expect("directory completion cache lock")
            .names_for(&query.parent)
    };
    let (names, scan_truncated) = match cached {
        Some(cached) => cached,
        None => {
            let scanned = scan_directory_completion_parent(&query.parent)?;
            directory_completion_cache()
                .lock()
                .expect("directory completion cache lock")
                .insert(DirectoryCompletionCacheEntry {
                    parent: query.parent.clone(),
                    names: scanned.0.clone(),
                    truncated: scanned.1,
                    created_at: Instant::now(),
                });
            scanned
        }
    };
    let limit = requested_limit.min(MAX_DIRECTORY_COMPLETION_RESULTS);
    let mut matches = names
        .into_iter()
        .filter(|name| name.starts_with(&query.prefix));
    let candidates: Vec<String> = matches
        .by_ref()
        .take(limit)
        .map(|name| format!("{}{name}/", query.rendered_parent))
        .collect();
    Ok((candidates, scan_truncated || matches.next().is_some()))
}

/// Queue bounded completion work without awaiting it on the client lifecycle.
/// This reads no Agent state and therefore remains available while a source
/// session is streaming or locked by a turn.
pub(super) fn handle_complete_working_directory(
    id: u64,
    raw_path: String,
    limit: usize,
    client_event_tx: mpsc::UnboundedSender<ServerEvent>,
) {
    let Ok(permit) = directory_completion_semaphore().try_acquire_owned() else {
        send_error(
            id,
            "Working directory completion is busy; try again",
            &client_event_tx,
        );
        return;
    };
    let raw_path_for_work = raw_path.clone();
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            complete_working_directory_sync(&raw_path_for_work, limit)
        })
        .await;
        match result {
            Ok(Ok((candidates, truncated))) => {
                let _ = client_event_tx.send(ServerEvent::WorkingDirectoryCompletions {
                    id,
                    input: raw_path,
                    candidates,
                    truncated,
                });
            }
            Ok(Err(message)) => send_error(id, &message, &client_event_tx),
            Err(_) => send_error(
                id,
                "Working directory completion is temporarily unavailable",
                &client_event_tx,
            ),
        }
    });
}

/// Create an unattached, clean live session.  This deliberately constructs a
/// new provider, registry, and agent rather than reading or locking the client
/// that asked for it: the source session may be in the middle of a turn.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_create_session(
    id: u64,
    raw_working_dir: String,
    runtime: SessionRuntimeSelection,
    provider_template: &Arc<dyn Provider>,
    sessions: &SessionAgents,
    shutdown_signals: &Arc<RwLock<HashMap<String, InterruptSignal>>>,
    soft_interrupt_queues: &SessionInterruptQueues,
    mcp_pool: &Arc<crate::mcp::SharedMcpPool>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) -> Result<()> {
    let working_dir = match canonicalize_new_session_working_dir(&raw_working_dir) {
        Ok(path) => path,
        Err(message) => {
            send_error(id, &message, client_event_tx);
            return Ok(());
        }
    };

    let result = build_clean_live_agent(provider_template, &working_dir, runtime, mcp_pool).await;
    let agent = match result {
        Ok(agent) => agent,
        Err(message) => {
            send_error(id, &message, client_event_tx);
            return Ok(());
        }
    };
    let session_id = agent.session_id().to_string();
    let session_name = agent
        .session_short_name()
        .map(str::to_string)
        .unwrap_or_else(|| session_id.clone());

    // Build every lock-free control before publishing the agent.  This makes
    // an empty session immediately attachable and controllable, while all
    // fallible setup above still owns an unpublished agent that will drop.
    let stop_signal = agent.graceful_shutdown_signal();
    let soft_interrupt_queue = agent.soft_interrupt_queue();
    let background_tool_signal = agent.background_tool_signal();
    agent.prewarm_provider_idle().await;
    let agent = Arc::new(Mutex::new(agent));
    {
        let mut signals = shutdown_signals.write().await;
        signals.insert(session_id.clone(), stop_signal);
    }
    register_session_interrupt_queue(soft_interrupt_queues, &session_id, soft_interrupt_queue)
        .await;
    register_background_tool_signal(&session_id, background_tool_signal);
    {
        let mut live = sessions.write().await;
        live.insert(session_id.clone(), agent);
    }

    let _ = client_event_tx.send(ServerEvent::SessionCreated {
        id,
        session_id,
        session_name,
        working_dir: working_dir.to_string_lossy().into_owned(),
    });
    Ok(())
}

/// The portion of ordinary session startup needed for a session that has no
/// client yet.  It is intentionally independent from the requester Agent.
async fn build_clean_live_agent(
    provider_template: &Arc<dyn Provider>,
    working_dir: &Path,
    runtime: SessionRuntimeSelection,
    mcp_pool: &Arc<crate::mcp::SharedMcpPool>,
) -> std::result::Result<Agent, String> {
    if runtime.model.as_deref().map(str::trim) == Some("")
        || runtime.provider_key.as_deref().map(str::trim) == Some("")
        || runtime.route_api_method.as_deref().map(str::trim) == Some("")
        || runtime.reasoning_effort.as_deref().map(str::trim) == Some("")
    {
        return Err("Runtime selection values must not be empty".to_string());
    }
    if runtime.model.is_none()
        && (runtime.provider_key.is_some() || runtime.route_api_method.is_some())
    {
        return Err(
            "A model is required when selecting a provider or route for a clean session"
                .to_string(),
        );
    }

    let provider = provider_template.fork_for_new_session();
    let registry = Registry::new(Arc::clone(&provider)).await;
    // A created session must receive the same project-local tool registration
    // and policy boundary as a normal session, before any client can attach.
    registry
        .register_mcp_tools_for_dir(
            None,
            Some(Arc::clone(mcp_pool)),
            None,
            Some(working_dir.to_path_buf()),
        )
        .await;

    // Apply all fallible provider state before constructing an Agent. Agent
    // setters persist best-effort session state, which would leave a durable
    // orphan if a later requested runtime setting failed.
    if let Some(model) = runtime.model.as_deref() {
        let model_request = crate::provider::MultiProvider::model_switch_request_for_session_route(
            model,
            runtime.provider_key.as_deref(),
            runtime.route_api_method.as_deref(),
        );
        let switch_error =
            crate::provider::set_model_with_auth_refresh(provider.as_ref(), &model_request).err();
        let resolved = provider.model();
        if !models_are_equivalent(&resolved, model) {
            let detail = switch_error
                .map(|error| format!(": {error}"))
                .unwrap_or_else(|| " (the switch reported success)".to_string());
            return Err(format!(
                "Cannot create session on model '{model}' (request '{model_request}'){detail}. It would run '{resolved}' instead"
            ));
        }
    }
    if let Some(effort) = runtime.reasoning_effort.as_deref() {
        provider
            .set_reasoning_effort(effort)
            .map_err(|error| format!("Cannot set reasoning effort '{effort}': {error}"))?;
    }
    let working_dir = working_dir.to_string_lossy().into_owned();
    let mut agent = Agent::new_provisional_with_initial_working_dir(
        Arc::clone(&provider),
        registry,
        Some(&working_dir),
    );
    agent.set_memory_enabled(crate::config::config().features.memory);
    Ok(agent)
}

fn models_are_equivalent(resolved: &str, requested: &str) -> bool {
    fn normalize(model: &str) -> String {
        model
            .trim()
            .rsplit_once('/')
            .map(|(_, model)| model)
            .unwrap_or(model.trim())
            .split_once(':')
            .map(|(model, _)| model)
            .unwrap_or_else(|| model.trim())
            .to_ascii_lowercase()
    }
    let resolved = normalize(resolved);
    let requested = normalize(requested);
    resolved.is_empty() || resolved == requested
}

fn send_error(id: u64, message: &str, client_event_tx: &mpsc::UnboundedSender<ServerEvent>) {
    let _ = client_event_tx.send(ServerEvent::Error {
        id,
        message: message.to_string(),
        retry_after_secs: None,
    });
}

#[cfg(test)]
#[path = "client_session_create_tests.rs"]
mod tests;
