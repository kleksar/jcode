use super::*;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

// Tool and shell completions invalidate this cache immediately. The longer
// fallback interval catches edits made outside Jcode without polling git on
// every idle frame.
#[cfg(not(test))]
const REFRESH_INTERVAL: Duration = Duration::from_secs(10);
const MAX_FILES: usize = 50;
const MAX_LINES_PER_FILE: usize = 500;
const MAX_UNTRACKED_BYTES: u64 = 256 * 1024;

/// Geometry from the last frame, not inferred from the scrolling diff rows.
#[derive(Clone)]
pub(crate) struct WorktreePaneLayout {
    pub area: Rect,
    pub diff_tab_area: Rect,
    pub files_tab_area: Rect,
    pub terminal_tab_area: Rect,
    pub files_tab_active: bool,
    pub terminal_tab_active: bool,
    pub list_area: Rect,
    pub body_area: Rect,
    pub list_scroll: usize,
    pub paths: Arc<Vec<String>>,
    pub tree_area: Option<Rect>,
    pub preview_area: Option<Rect>,
    pub tree_scroll: usize,
    pub tree_rows: Arc<Vec<ProjectTreeRow>>,
    pub preview_total_lines: usize,
    pub preview_scroll: usize,
    pub working_dir: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectTreeRow {
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
}

thread_local! {
    static WORKTREE_PANE_LAYOUT: std::cell::RefCell<Option<WorktreePaneLayout>> = const {
        std::cell::RefCell::new(None)
    };
}

pub(crate) fn worktree_pane_layout() -> Option<WorktreePaneLayout> {
    WORKTREE_PANE_LAYOUT.with(|layout| layout.borrow().clone())
}

pub(super) fn clear_worktree_pane_layout() {
    WORKTREE_PANE_LAYOUT.with(|layout| *layout.borrow_mut() = None);
}

/// A configured File/Pinned mode can still be showing the worktree fallback.
/// Use the same content predicates as draw_inner before preserving its scroll.
pub(crate) fn has_explicit_side_pane_content(app: &dyn TuiState) -> bool {
    app.side_panel().focused_page().is_some()
        || (app.diff_mode().is_file() && app.has_display_edit_tool_messages())
        || (app.diff_mode().is_pinned()
            && super::collect_pinned_diffs_cached(
                app.display_messages(),
                app.display_messages_version(),
            ))
}

/// Read only the cached snapshot. Input and ticks must never run git synchronously.
pub(crate) fn worktree_file_is_present(working_dir: Option<&str>, path: &str) -> Option<bool> {
    let cache = worktree_cache().lock().ok()?;
    let entry = cache.get(Path::new(working_dir?))?;
    let snapshot = entry.snapshot.as_ref()?;
    Some(snapshot.files.iter().any(|file| file.path == path))
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum WorktreeLineKind {
    Context,
    Add,
    Del,
    Hunk,
    Meta,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct WorktreeDiffLine {
    kind: WorktreeLineKind,
    old_line: Option<usize>,
    new_line: Option<usize>,
    content: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct WorktreeFileChange {
    path: String,
    additions: usize,
    deletions: usize,
    lines: Vec<WorktreeDiffLine>,
    truncated: bool,
}

#[derive(Clone, Debug, Default, Hash, PartialEq, Eq)]
pub(super) struct WorktreeChangesSnapshot {
    revision: u64,
    files: Vec<WorktreeFileChange>,
    additions: usize,
    deletions: usize,
    truncated: bool,
}

impl WorktreeChangesSnapshot {
    fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

const MAX_PROJECT_TREE_FILES: usize = 20_000;
const MAX_PROJECT_TREE_DEPTH: usize = 32;
#[cfg(not(test))]
const MAX_PROJECT_TREE_ROOTS: usize = 8;
const MAX_PROJECT_PREVIEW_BYTES: u64 = 512 * 1024;
const MAX_PROJECT_PREVIEW_LINES: usize = 5_000;

#[derive(Clone, Debug, Default)]
struct ProjectTreeNode {
    name: String,
    path: String,
    is_dir: bool,
    children: Vec<ProjectTreeNode>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct ProjectTreeSnapshot {
    root: PathBuf,
    root_label: String,
    nodes: Vec<ProjectTreeNode>,
    file_count: usize,
    truncated: bool,
    revision: u64,
}

#[derive(Default)]
struct ProjectTreeCacheEntry {
    fetched_at: Option<Instant>,
    snapshot: Option<Arc<ProjectTreeSnapshot>>,
    generation: u64,
    #[cfg(not(test))]
    refreshing: bool,
}

fn project_tree_cache() -> &'static Mutex<HashMap<PathBuf, ProjectTreeCacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, ProjectTreeCacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(not(test))]
fn trim_project_tree_cache(cache: &mut HashMap<PathBuf, ProjectTreeCacheEntry>, keep: &Path) {
    while cache.len() > MAX_PROJECT_TREE_ROOTS {
        let Some(oldest) = cache
            .iter()
            .filter(|(path, _)| path.as_path() != keep)
            .min_by_key(|(_, entry)| entry.fetched_at)
            .map(|(path, _)| path.clone())
        else {
            break;
        };
        cache.remove(&oldest);
    }
}

#[derive(Default)]
struct WorktreeCacheEntry {
    fetched_at: Option<Instant>,
    snapshot: Option<Arc<WorktreeChangesSnapshot>>,
    #[cfg(not(test))]
    refreshing: bool,
}

fn worktree_cache() -> &'static Mutex<HashMap<PathBuf, WorktreeCacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, WorktreeCacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn worktree_redraw_pending() -> &'static AtomicBool {
    static PENDING: AtomicBool = AtomicBool::new(false);
    &PENDING
}

fn take_worktree_changes_redraw() -> bool {
    worktree_redraw_pending().swap(false, Ordering::AcqRel)
}

#[cfg(not(test))]
pub(crate) fn poll_worktree_changes(working_dir: Option<&str>) -> bool {
    // Rendering starts the initial fetch, while idle polling is what notices
    // TTL expiry when an external editor changes a quiet worktree. The call is
    // cheap while the cache is fresh and collection remains off the UI thread.
    let _ = snapshot_for_worktree(working_dir);
    if let Some(path) = working_dir.map(PathBuf::from) {
        let already_requested = project_tree_cache()
            .lock()
            .is_ok_and(|cache| cache.contains_key(&path));
        if already_requested {
            let _ = snapshot_for_project_tree(working_dir);
        }
    }
    take_worktree_changes_redraw()
}

#[cfg(test)]
pub(crate) fn poll_worktree_changes(_working_dir: Option<&str>) -> bool {
    take_worktree_changes_redraw()
}

#[cfg(not(test))]
pub(super) fn snapshot_for_worktree(
    working_dir: Option<&str>,
) -> Option<Arc<WorktreeChangesSnapshot>> {
    let working_dir = working_dir?.trim();
    if working_dir.is_empty() {
        return None;
    }
    let path = PathBuf::from(working_dir);
    let mut cache = worktree_cache().lock().ok()?;
    let entry = cache.entry(path.clone()).or_default();
    let fresh = entry
        .fetched_at
        .is_some_and(|fetched| fetched.elapsed() < REFRESH_INTERVAL);
    if !fresh && !entry.refreshing {
        entry.refreshing = true;
        std::thread::spawn(move || {
            let next = collect_worktree_changes(&path).map(Arc::new);
            if let Ok(mut cache) = worktree_cache().lock() {
                let entry = cache.entry(path).or_default();
                entry.snapshot = next;
                entry.fetched_at = Some(Instant::now());
                entry.refreshing = false;
            }
            worktree_redraw_pending().store(true, Ordering::Release);
        });
    }
    entry
        .snapshot
        .clone()
        .filter(|snapshot| !snapshot.is_empty())
}

#[cfg(not(test))]
pub(super) fn snapshot_for_project_tree(
    working_dir: Option<&str>,
) -> Option<Arc<ProjectTreeSnapshot>> {
    let working_dir = working_dir?.trim();
    if working_dir.is_empty() {
        return None;
    }
    let path = PathBuf::from(working_dir);
    let mut cache = project_tree_cache().lock().ok()?;
    if !cache.contains_key(&path) && cache.len() >= MAX_PROJECT_TREE_ROOTS {
        let oldest = cache
            .iter()
            .min_by_key(|(_, entry)| entry.fetched_at)
            .map(|(path, _)| path.clone());
        if let Some(oldest) = oldest {
            cache.remove(&oldest);
        }
    }
    let entry = cache.entry(path.clone()).or_default();
    let fresh = entry
        .fetched_at
        .is_some_and(|fetched| fetched.elapsed() < REFRESH_INTERVAL);
    if !fresh && !entry.refreshing {
        entry.refreshing = true;
        let generation = entry.generation;
        std::thread::spawn(move || {
            let next = collect_project_tree(&path).map(Arc::new);
            if let Ok(mut cache) = project_tree_cache().lock() {
                let entry = cache.entry(path.clone()).or_default();
                if entry.generation == generation {
                    entry.snapshot = next;
                    entry.fetched_at = Some(Instant::now());
                } else {
                    entry.fetched_at = None;
                }
                entry.refreshing = false;
                trim_project_tree_cache(&mut cache, &path);
            }
            worktree_redraw_pending().store(true, Ordering::Release);
        });
    }
    entry.snapshot.clone()
}

#[cfg(test)]
pub(super) fn snapshot_for_project_tree(
    working_dir: Option<&str>,
) -> Option<Arc<ProjectTreeSnapshot>> {
    let path = PathBuf::from(working_dir?);
    project_tree_cache()
        .lock()
        .ok()?
        .get(&path)
        .and_then(|entry| entry.snapshot.clone())
}

#[cfg(test)]
pub(super) fn snapshot_for_worktree(
    working_dir: Option<&str>,
) -> Option<Arc<WorktreeChangesSnapshot>> {
    let path = PathBuf::from(working_dir?);
    worktree_cache()
        .lock()
        .ok()?
        .get(&path)
        .and_then(|entry| entry.snapshot.clone())
        .filter(|snapshot| !snapshot.is_empty())
}

pub(crate) fn invalidate_worktree_changes_cache() {
    if let Ok(mut cache) = worktree_cache().lock() {
        for entry in cache.values_mut() {
            entry.fetched_at = None;
        }
    }
    if let Ok(mut cache) = project_tree_cache().lock() {
        for entry in cache.values_mut() {
            entry.fetched_at = None;
            entry.generation = entry.generation.wrapping_add(1);
        }
    }
}

#[cfg(test)]
pub(crate) fn prime_worktree_changes_for_tests(working_dir: &Path) {
    let snapshot = collect_worktree_changes(working_dir);
    if let Ok(mut cache) = worktree_cache().lock() {
        cache.insert(
            working_dir.to_path_buf(),
            WorktreeCacheEntry {
                fetched_at: Some(Instant::now()),
                snapshot: snapshot.map(Arc::new),
            },
        );
    }
}

#[cfg(test)]
pub(crate) fn prime_project_tree_for_tests(working_dir: &Path) {
    let snapshot = collect_project_tree(working_dir);
    if let Ok(mut cache) = project_tree_cache().lock() {
        cache.insert(
            working_dir.to_path_buf(),
            ProjectTreeCacheEntry {
                fetched_at: Some(Instant::now()),
                snapshot: snapshot.map(Arc::new),
                generation: 0,
            },
        );
    }
}

fn command_output(repo: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn read_nul_paths_limited(
    reader: &mut impl BufRead,
    limit: usize,
) -> std::io::Result<(Vec<String>, bool)> {
    let mut paths = Vec::with_capacity(limit.min(4096));
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let read = reader.read_until(0, &mut buffer)?;
        if read == 0 {
            return Ok((paths, false));
        }
        if buffer.last() == Some(&0) {
            buffer.pop();
        }
        if buffer.is_empty() {
            continue;
        }
        if paths.len() == limit {
            return Ok((paths, true));
        }
        paths.push(String::from_utf8_lossy(&buffer).into_owned());
    }
}

fn git_project_paths(root: &Path) -> Option<(Vec<String>, bool)> {
    let mut child = Command::new("git")
        .current_dir(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            ".",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let mut reader = BufReader::new(stdout);
    let result = read_nul_paths_limited(&mut reader, MAX_PROJECT_TREE_FILES);
    drop(reader);
    let (paths, truncated) = match result {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    if truncated {
        let _ = child.kill();
    }
    let status = child.wait().ok()?;
    if !truncated && !status.success() {
        return None;
    }
    Some((paths, truncated))
}

fn nul_paths(output: &[u8]) -> Vec<String> {
    output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect()
}

fn normalized_relative_components(path: &str) -> Option<Vec<String>> {
    let mut components = Vec::new();
    for component in Path::new(path).components() {
        match component {
            std::path::Component::Normal(value) => {
                components.push(value.to_string_lossy().into_owned())
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => return None,
        }
    }
    (!components.is_empty() && components.len() <= MAX_PROJECT_TREE_DEPTH).then_some(components)
}

fn project_path_label(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '\n' => '␤',
            '\r' => '␍',
            '\t' => '⇥',
            character if character.is_control() => '�',
            character => character,
        })
        .collect()
}

#[derive(Default)]
struct ProjectTreeArenaNode {
    name: String,
    path: String,
    is_dir: bool,
    children: Vec<usize>,
}

fn build_project_nodes(paths: Vec<String>) -> (Vec<ProjectTreeNode>, usize) {
    let mut arena = vec![ProjectTreeArenaNode::default()];
    let mut indices = HashMap::<String, usize>::new();
    let mut file_count = 0;
    for path in paths {
        let Some(components) = normalized_relative_components(&path) else {
            continue;
        };
        file_count += 1;
        let mut parent = 0;
        let mut prefix = String::new();
        for (component_index, name) in components.iter().enumerate() {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(name);
            let is_dir = component_index + 1 < components.len();
            let index = if let Some(index) = indices.get(&prefix).copied() {
                if is_dir {
                    arena[index].is_dir = true;
                }
                index
            } else {
                let index = arena.len();
                arena.push(ProjectTreeArenaNode {
                    name: project_path_label(name),
                    path: prefix.clone(),
                    is_dir,
                    children: Vec::new(),
                });
                indices.insert(prefix.clone(), index);
                arena[parent].children.push(index);
                index
            };
            parent = index;
        }
    }

    fn materialize(index: usize, arena: &[ProjectTreeArenaNode]) -> ProjectTreeNode {
        let node = &arena[index];
        ProjectTreeNode {
            name: node.name.clone(),
            path: node.path.clone(),
            is_dir: node.is_dir,
            children: node
                .children
                .iter()
                .map(|child| materialize(*child, arena))
                .collect(),
        }
    }
    let mut nodes = arena[0]
        .children
        .iter()
        .map(|index| materialize(*index, &arena))
        .collect::<Vec<_>>();
    sort_project_nodes(&mut nodes);
    (nodes, file_count)
}

fn sort_project_nodes(nodes: &mut [ProjectTreeNode]) {
    nodes.sort_by(|left, right| {
        right
            .is_dir
            .cmp(&left.is_dir)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });
    for node in nodes {
        sort_project_nodes(&mut node.children);
    }
}

fn ignored_fallback_dir(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".hg"
            | ".svn"
            | "target"
            | "node_modules"
            | "__pycache__"
            | ".venv"
            | "venv"
            | "dist"
            | "build"
    )
}

fn collect_filesystem_paths(root: &Path, relative: &Path, paths: &mut Vec<String>, depth: usize) {
    if paths.len() >= MAX_PROJECT_TREE_FILES || depth >= MAX_PROJECT_TREE_DEPTH {
        return;
    }
    let directory = root.join(relative);
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name().to_string_lossy().to_lowercase());
    for entry in entries {
        if paths.len() >= MAX_PROJECT_TREE_FILES {
            break;
        }
        let name = entry.file_name();
        let name_text = name.to_string_lossy();
        let child_relative = relative.join(&name);
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if ignored_fallback_dir(&name_text) {
                continue;
            }
            collect_filesystem_paths(root, &child_relative, paths, depth + 1);
        } else {
            paths.push(child_relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

fn collect_project_tree(working_dir: &Path) -> Option<ProjectTreeSnapshot> {
    let session_root = working_dir.canonicalize().ok()?;
    if !session_root.is_dir() {
        return None;
    }
    let root = command_output(&session_root, &["rev-parse", "--show-toplevel"])
        .and_then(|output| {
            let path = PathBuf::from(String::from_utf8_lossy(&output).trim());
            (!path.as_os_str().is_empty())
                .then(|| path.canonicalize().ok())
                .flatten()
        })
        .filter(|path| path.is_dir())
        .unwrap_or(session_root);
    let (mut paths, git_truncated) = git_project_paths(&root).unwrap_or_else(|| {
        let mut paths = Vec::new();
        collect_filesystem_paths(&root, Path::new(""), &mut paths, 0);
        (paths, false)
    });
    paths.sort();
    paths.dedup();
    let truncated = git_truncated || paths.len() > MAX_PROJECT_TREE_FILES;
    paths.truncate(MAX_PROJECT_TREE_FILES);
    let mut revision = std::collections::hash_map::DefaultHasher::new();
    paths.hash(&mut revision);
    let revision = revision.finish();

    let (nodes, file_count) = build_project_nodes(paths);
    Some(ProjectTreeSnapshot {
        root_label: root
            .file_name()
            .map(|name| project_path_label(&name.to_string_lossy()))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| root.display().to_string()),
        root,
        nodes,
        file_count,
        truncated,
        revision,
    })
}

fn collect_worktree_changes(working_dir: &Path) -> Option<WorktreeChangesSnapshot> {
    let root_output = command_output(working_dir, &["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(String::from_utf8_lossy(&root_output).trim());
    if root.as_os_str().is_empty() {
        return None;
    }

    let has_head = command_output(&root, &["rev-parse", "--verify", "HEAD"]).is_some();
    let tracked_args: &[&str] = if has_head {
        &["diff", "--name-only", "-z", "HEAD", "--"]
    } else {
        &["diff", "--name-only", "-z", "--"]
    };
    let mut paths = command_output(&root, tracked_args)
        .map(|output| nul_paths(&output))
        .unwrap_or_default();
    if !has_head
        && let Some(output) =
            command_output(&root, &["diff", "--cached", "--name-only", "-z", "--"])
    {
        paths.extend(nul_paths(&output));
    }

    let untracked: HashSet<String> =
        command_output(&root, &["ls-files", "--others", "--exclude-standard", "-z"])
            .map(|output| nul_paths(&output).into_iter().collect())
            .unwrap_or_default();
    paths.extend(untracked.iter().cloned());
    paths.sort();
    paths.dedup();

    let truncated = paths.len() > MAX_FILES;
    paths.truncate(MAX_FILES);
    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        let change = if untracked.contains(&path) {
            collect_untracked_file(&root, &path)
        } else {
            collect_tracked_file(&root, &path, has_head)
        };
        files.push(change);
    }

    let additions = files.iter().map(|file| file.additions).sum();
    let deletions = files.iter().map(|file| file.deletions).sum();
    let mut snapshot = WorktreeChangesSnapshot {
        revision: 0,
        files,
        additions,
        deletions,
        truncated,
    };
    let mut hasher = DefaultHasher::new();
    snapshot.hash(&mut hasher);
    snapshot.revision = hasher.finish();
    Some(snapshot)
}

fn collect_tracked_file(root: &Path, path: &str, has_head: bool) -> WorktreeFileChange {
    let mut args = vec!["diff", "--no-ext-diff", "--no-color", "--unified=3"];
    if has_head {
        args.push("HEAD");
    }
    args.extend(["--", path]);
    let output = command_output(root, &args).unwrap_or_default();
    let mut lines = parse_unified_diff(&String::from_utf8_lossy(&output));
    if lines.is_empty() {
        lines.push(WorktreeDiffLine {
            kind: WorktreeLineKind::Meta,
            old_line: None,
            new_line: None,
            content: "file metadata changed".to_string(),
        });
    }
    let additions = lines
        .iter()
        .filter(|line| line.kind == WorktreeLineKind::Add)
        .count();
    let deletions = lines
        .iter()
        .filter(|line| line.kind == WorktreeLineKind::Del)
        .count();
    let truncated = lines.len() > MAX_LINES_PER_FILE;
    lines.truncate(MAX_LINES_PER_FILE);
    WorktreeFileChange {
        path: path.to_string(),
        additions,
        deletions,
        lines,
        truncated,
    }
}

fn collect_untracked_file(root: &Path, path: &str) -> WorktreeFileChange {
    let full_path = root.join(path);
    let mut lines = Vec::new();
    let mut truncated = false;
    match std::fs::metadata(&full_path) {
        Ok(metadata) if metadata.len() > MAX_UNTRACKED_BYTES => {
            lines.push(WorktreeDiffLine {
                kind: WorktreeLineKind::Meta,
                old_line: None,
                new_line: None,
                content: format!("new file, {} bytes", metadata.len()),
            });
        }
        Ok(_) => match std::fs::read(&full_path) {
            Ok(bytes) if bytes.contains(&0) => lines.push(WorktreeDiffLine {
                kind: WorktreeLineKind::Meta,
                old_line: None,
                new_line: None,
                content: "new binary file".to_string(),
            }),
            Ok(bytes) => {
                let content = String::from_utf8_lossy(&bytes);
                for (index, content) in content.lines().enumerate() {
                    if lines.len() >= MAX_LINES_PER_FILE {
                        truncated = true;
                        break;
                    }
                    lines.push(WorktreeDiffLine {
                        kind: WorktreeLineKind::Add,
                        old_line: None,
                        new_line: Some(index + 1),
                        content: content.to_string(),
                    });
                }
            }
            Err(_) => lines.push(WorktreeDiffLine {
                kind: WorktreeLineKind::Meta,
                old_line: None,
                new_line: None,
                content: "new unreadable file".to_string(),
            }),
        },
        Err(_) => lines.push(WorktreeDiffLine {
            kind: WorktreeLineKind::Meta,
            old_line: None,
            new_line: None,
            content: "new file".to_string(),
        }),
    }
    let additions = lines
        .iter()
        .filter(|line| line.kind == WorktreeLineKind::Add)
        .count();
    WorktreeFileChange {
        path: path.to_string(),
        additions,
        deletions: 0,
        lines,
        truncated,
    }
}

fn parse_hunk_start(line: &str) -> Option<(usize, usize)> {
    let header = line.strip_prefix("@@ ")?;
    let mut parts = header.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let old = old.split(',').next()?.parse().ok()?;
    let new = new.split(',').next()?.parse().ok()?;
    Some((old, new))
}

fn parse_unified_diff(diff: &str) -> Vec<WorktreeDiffLine> {
    let mut lines = Vec::new();
    let mut old_line = 0usize;
    let mut new_line = 0usize;
    let mut in_hunk = false;

    for raw in diff.lines() {
        if let Some((old, new)) = parse_hunk_start(raw) {
            old_line = old;
            new_line = new;
            in_hunk = true;
            lines.push(WorktreeDiffLine {
                kind: WorktreeLineKind::Hunk,
                old_line: None,
                new_line: None,
                content: raw.to_string(),
            });
            continue;
        }
        if !in_hunk {
            if raw.starts_with("Binary files ") || raw.starts_with("GIT binary patch") {
                lines.push(WorktreeDiffLine {
                    kind: WorktreeLineKind::Meta,
                    old_line: None,
                    new_line: None,
                    content: raw.to_string(),
                });
            }
            continue;
        }
        if let Some(content) = raw.strip_prefix('+') {
            lines.push(WorktreeDiffLine {
                kind: WorktreeLineKind::Add,
                old_line: None,
                new_line: Some(new_line),
                content: content.to_string(),
            });
            new_line += 1;
        } else if let Some(content) = raw.strip_prefix('-') {
            lines.push(WorktreeDiffLine {
                kind: WorktreeLineKind::Del,
                old_line: Some(old_line),
                new_line: None,
                content: content.to_string(),
            });
            old_line += 1;
        } else if let Some(content) = raw.strip_prefix(' ') {
            lines.push(WorktreeDiffLine {
                kind: WorktreeLineKind::Context,
                old_line: Some(old_line),
                new_line: Some(new_line),
                content: content.to_string(),
            });
            old_line += 1;
            new_line += 1;
        } else if raw.starts_with("\\ No newline") {
            lines.push(WorktreeDiffLine {
                kind: WorktreeLineKind::Meta,
                old_line: None,
                new_line: None,
                content: raw.to_string(),
            });
        }
    }
    lines
}

fn line_number(value: Option<usize>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn build_file_index(
    snapshot: &WorktreeChangesSnapshot,
    selected: Option<&str>,
) -> Vec<Line<'static>> {
    let mut rendered = Vec::new();
    for file in &snapshot.files {
        rendered.push(Line::from(vec![
            Span::styled(
                file.path.clone(),
                if selected == Some(file.path.as_str()) {
                    Style::default().fg(tool_color()).add_modifier(
                        ratatui::style::Modifier::BOLD | ratatui::style::Modifier::UNDERLINED,
                    )
                } else {
                    Style::default().fg(dim_color())
                },
            ),
            Span::raw(" "),
            Span::styled(
                format!("+{}", file.additions),
                Style::default().fg(diff_add_color()),
            ),
            Span::raw(" "),
            Span::styled(
                format!("-{}", file.deletions),
                Style::default().fg(diff_del_color()),
            ),
        ]));
    }
    rendered
}

fn build_render_lines(
    snapshot: &WorktreeChangesSnapshot,
    selected: Option<&str>,
) -> Vec<Line<'static>> {
    let mut rendered = Vec::new();
    for file in snapshot
        .files
        .iter()
        .filter(|file| selected.is_none_or(|path| path == file.path))
    {
        rendered.push(Line::from(Span::styled(
            file.path.clone(),
            Style::default()
                .fg(rgb(220, 225, 235))
                .add_modifier(ratatui::style::Modifier::BOLD),
        )));
        let extension = Path::new(&file.path)
            .extension()
            .and_then(|ext| ext.to_str());
        for line in &file.lines {
            let gutter = format!(
                "{:>4} {:>4} ",
                line_number(line.old_line),
                line_number(line.new_line)
            );
            let (prefix, foreground, background) = match line.kind {
                WorktreeLineKind::Add => {
                    ("+ ", diff_add_color(), Some(diff_add_background_color()))
                }
                WorktreeLineKind::Del => {
                    ("- ", diff_del_color(), Some(diff_del_background_color()))
                }
                WorktreeLineKind::Context => ("  ", rgb(205, 210, 220), None),
                WorktreeLineKind::Hunk => ("  ", rgb(130, 170, 220), None),
                WorktreeLineKind::Meta => ("  ", dim_color(), None),
            };
            let mut spans = vec![
                Span::styled(gutter, Style::default().fg(dim_color())),
                Span::styled(prefix, Style::default().fg(foreground)),
            ];
            if matches!(
                line.kind,
                WorktreeLineKind::Add | WorktreeLineKind::Del | WorktreeLineKind::Context
            ) {
                for span in markdown::highlight_line(&line.content, extension) {
                    spans.push(if let Some(background) = background {
                        with_diff_background(span, background)
                    } else {
                        span
                    });
                }
            } else {
                spans.push(Span::styled(
                    line.content.clone(),
                    Style::default().fg(foreground),
                ));
            }
            let mut rendered_line = Line::from(spans);
            if let Some(background) = background {
                rendered_line = rendered_line.style(Style::default().bg(background));
            }
            rendered.push(rendered_line);
        }
        if file.truncated {
            rendered.push(Line::from(Span::styled(
                format!("… diff truncated after {MAX_LINES_PER_FILE} lines"),
                Style::default().fg(dim_color()),
            )));
        }
        rendered.push(Line::from(""));
    }
    rendered
}

type RenderLinesCache = HashMap<(u64, Option<String>), Arc<Vec<Line<'static>>>>;

fn render_lines_cache() -> &'static Mutex<RenderLinesCache> {
    static CACHE: OnceLock<Mutex<RenderLinesCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_render_lines(
    snapshot: &WorktreeChangesSnapshot,
    selected: Option<&str>,
) -> Arc<Vec<Line<'static>>> {
    let key = (snapshot.revision, selected.map(str::to_owned));
    if let Ok(cache) = render_lines_cache().lock()
        && let Some(lines) = cache.get(&key)
    {
        return lines.clone();
    }

    let lines = Arc::new(build_render_lines(snapshot, selected));
    if let Ok(mut cache) = render_lines_cache().lock() {
        // A session only needs the current and a few recent snapshots. Keep the
        // renderer cache bounded even when external tools edit files rapidly.
        if cache.len() >= 8 {
            cache.clear();
        }
        cache.insert(key, lines.clone());
    }
    lines
}

const DIFF_TAB_LABEL: &str = " Diff ";
const FILES_TAB_LABEL: &str = " Files ";
const TERMINAL_TAB_LABEL: &str = " Terminal ";

fn project_pane_tab_style(active: bool) -> Style {
    if active {
        Style::default()
            .fg(rgb(235, 235, 245))
            .bg(rgb(55, 55, 68))
            .add_modifier(ratatui::style::Modifier::BOLD)
    } else {
        Style::default().fg(dim_color())
    }
}

fn project_pane_title(
    files_active: bool,
    terminal_active: bool,
    mut suffix: Vec<Span<'static>>,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            DIFF_TAB_LABEL,
            project_pane_tab_style(!files_active && !terminal_active),
        ),
        Span::raw(" "),
        Span::styled(FILES_TAB_LABEL, project_pane_tab_style(files_active)),
        Span::raw(" "),
        Span::styled(TERMINAL_TAB_LABEL, project_pane_tab_style(terminal_active)),
        Span::raw("  "),
    ];
    spans.append(&mut suffix);
    Line::from(spans)
}

fn project_pane_tab_areas(area: Rect) -> (Rect, Rect, Rect) {
    let header_x = area.x.saturating_add(1);
    let diff = Rect::new(header_x, area.y, DIFF_TAB_LABEL.len() as u16, 1);
    let files = Rect::new(
        diff.right().saturating_add(1),
        area.y,
        FILES_TAB_LABEL.len() as u16,
        1,
    );
    let terminal = Rect::new(
        files.right().saturating_add(1),
        area.y,
        TERMINAL_TAB_LABEL.len() as u16,
        1,
    );
    (diff, files, terminal)
}

fn flatten_project_nodes(
    nodes: &[ProjectTreeNode],
    depth: usize,
    app: &dyn TuiState,
    rows: &mut Vec<ProjectTreeRow>,
) {
    for node in nodes {
        let expanded = node.is_dir && app.project_tree_dir_expanded(&node.path);
        rows.push(ProjectTreeRow {
            path: node.path.clone(),
            name: node.name.clone(),
            depth,
            is_dir: node.is_dir,
            expanded,
        });
        if expanded {
            flatten_project_nodes(&node.children, depth + 1, app, rows);
        }
    }
}

type ProjectFlattenCache = HashMap<(PathBuf, u64, u64), Arc<Vec<ProjectTreeRow>>>;

fn project_flatten_cache() -> &'static Mutex<ProjectFlattenCache> {
    static CACHE: OnceLock<Mutex<ProjectFlattenCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_project_tree_rows(
    snapshot: &ProjectTreeSnapshot,
    app: &dyn TuiState,
) -> Arc<Vec<ProjectTreeRow>> {
    let key = (
        snapshot.root.clone(),
        snapshot.revision,
        app.project_tree_expansion_hash(),
    );
    if let Ok(cache) = project_flatten_cache().lock()
        && let Some(rows) = cache.get(&key)
    {
        return rows.clone();
    }
    let mut rows = Vec::new();
    flatten_project_nodes(&snapshot.nodes, 0, app, &mut rows);
    let rows = Arc::new(rows);
    if let Ok(mut cache) = project_flatten_cache().lock() {
        if cache.len() >= 16 {
            cache.clear();
        }
        cache.insert(key, rows.clone());
    }
    rows
}

#[derive(Clone)]
struct ProjectPreviewCacheEntry {
    modified: Option<std::time::SystemTime>,
    len: u64,
    lines: Option<Arc<Vec<Line<'static>>>>,
    #[cfg(not(test))]
    refreshing: bool,
}

type ProjectPreviewCache = HashMap<(PathBuf, String), ProjectPreviewCacheEntry>;

fn project_preview_cache() -> &'static Mutex<ProjectPreviewCache> {
    static CACHE: OnceLock<Mutex<ProjectPreviewCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn preview_message(text: impl Into<String>) -> Arc<Vec<Line<'static>>> {
    Arc::new(vec![Line::from(Span::styled(
        text.into(),
        Style::default().fg(dim_color()),
    ))])
}

#[cfg(unix)]
fn open_project_preview(root: &Path, relative: &str) -> Option<(PathBuf, File)> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    let components = normalized_relative_components(relative)?;
    let root_path = CString::new(root.as_os_str().as_bytes()).ok()?;
    let root_fd = unsafe {
        libc::open(
            root_path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if root_fd < 0 {
        return None;
    }
    let mut directory = unsafe { OwnedFd::from_raw_fd(root_fd) };
    for (index, component) in components.iter().enumerate() {
        let component = CString::new(component.as_bytes()).ok()?;
        let final_component = index + 1 == components.len();
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if final_component {
                0
            } else {
                libc::O_DIRECTORY
            };
        let fd = unsafe { libc::openat(directory.as_raw_fd(), component.as_ptr(), flags) };
        if fd < 0 {
            return None;
        }
        let opened = unsafe { OwnedFd::from_raw_fd(fd) };
        if final_component {
            return Some((root.join(relative), File::from(opened)));
        }
        directory = opened;
    }
    None
}

#[cfg(not(unix))]
fn open_project_preview(_root: &Path, _relative: &str) -> Option<(PathBuf, File)> {
    // Keep previews disabled until this platform has a handle-relative,
    // reparse-point-safe equivalent of the Unix openat traversal above.
    None
}

fn read_project_preview(root: &Path, relative: &str) -> Arc<Vec<Line<'static>>> {
    let Some((path, mut file)) = open_project_preview(root, relative) else {
        return preview_message("Preview blocked outside the project root");
    };
    let Ok(metadata) = file.metadata() else {
        return preview_message("Unable to read file metadata");
    };
    if metadata.len() > MAX_PROJECT_PREVIEW_BYTES {
        return preview_message(format!(
            "Preview limited to files under {} KiB",
            MAX_PROJECT_PREVIEW_BYTES / 1024
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    if file.read_to_end(&mut bytes).is_err() {
        return preview_message("Unable to read file");
    }
    if bytes.iter().take(8192).any(|byte| *byte == 0) {
        return preview_message("Binary file preview is not available");
    }
    let text = String::from_utf8_lossy(&bytes);
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let line_count = text.lines().count().clamp(1, MAX_PROJECT_PREVIEW_LINES);
    let gutter_width = line_count.to_string().len();
    let mut lines = Vec::with_capacity(line_count.saturating_add(1));
    for (index, content) in text.lines().take(MAX_PROJECT_PREVIEW_LINES).enumerate() {
        let mut spans = vec![Span::styled(
            format!("{:>width$}  ", index + 1, width = gutter_width),
            Style::default().fg(dim_color()),
        )];
        spans.extend(markdown::highlight_line(
            content,
            (!extension.is_empty()).then_some(extension),
        ));
        lines.push(Line::from(spans));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "(empty file)",
            Style::default().fg(dim_color()),
        )));
    }
    if text.lines().count() > MAX_PROJECT_PREVIEW_LINES {
        lines.push(Line::from(Span::styled(
            format!("… preview truncated after {MAX_PROJECT_PREVIEW_LINES} lines"),
            Style::default().fg(dim_color()),
        )));
    }
    Arc::new(lines)
}

fn build_project_preview(root: &Path, relative: &str) -> Arc<Vec<Line<'static>>> {
    let joined = root.join(relative);
    let Ok(path) = joined.canonicalize() else {
        return preview_message("File is no longer available");
    };
    if !path.starts_with(root) || !path.is_file() {
        return preview_message("Preview blocked outside the project root");
    }
    let Ok(metadata) = path.metadata() else {
        return preview_message("Unable to read file metadata");
    };
    if metadata.len() > MAX_PROJECT_PREVIEW_BYTES {
        return preview_message(format!(
            "Preview limited to files under {} KiB",
            MAX_PROJECT_PREVIEW_BYTES / 1024
        ));
    }
    let signature = (metadata.modified().ok(), metadata.len());
    let key = (root.to_path_buf(), relative.to_string());
    if let Ok(cache) = project_preview_cache().lock()
        && let Some(entry) = cache.get(&key)
        && (entry.modified, entry.len) == signature
        && let Some(lines) = entry.lines.as_ref()
    {
        return lines.clone();
    }

    #[cfg(test)]
    {
        let lines = read_project_preview(root, relative);
        if let Ok(mut cache) = project_preview_cache().lock() {
            if cache.len() >= 32 {
                cache.clear();
            }
            cache.insert(
                key,
                ProjectPreviewCacheEntry {
                    modified: signature.0,
                    len: signature.1,
                    lines: Some(lines.clone()),
                },
            );
        }
        lines
    }

    #[cfg(not(test))]
    {
        if let Ok(mut cache) = project_preview_cache().lock() {
            let already_refreshing = cache
                .get(&key)
                .is_some_and(|entry| (entry.modified, entry.len) == signature && entry.refreshing);
            if !already_refreshing {
                if cache.len() >= 32 {
                    cache.clear();
                }
                cache.insert(
                    key.clone(),
                    ProjectPreviewCacheEntry {
                        modified: signature.0,
                        len: signature.1,
                        lines: None,
                        refreshing: true,
                    },
                );
                let root = root.to_path_buf();
                let relative = relative.to_string();
                std::thread::spawn(move || {
                    let lines = read_project_preview(&root, &relative);
                    if let Ok(mut cache) = project_preview_cache().lock()
                        && let Some(entry) = cache.get_mut(&key)
                        && (entry.modified, entry.len) == signature
                    {
                        entry.lines = Some(lines);
                        entry.refreshing = false;
                    }
                    worktree_redraw_pending().store(true, Ordering::Release);
                });
            }
        }
        preview_message("Loading preview…")
    }
}

fn project_tree_line(row: &ProjectTreeRow, selected: bool, focused: bool) -> Line<'static> {
    let marker = if selected { "› " } else { "  " };
    let disclosure = if row.is_dir {
        if row.expanded { "▾ " } else { "▸ " }
    } else {
        "  "
    };
    let mut style = if row.is_dir {
        Style::default().fg(file_link_color())
    } else {
        Style::default().fg(rgb(190, 190, 205))
    };
    if selected {
        style = style
            .bg(if focused {
                rgb(52, 52, 68)
            } else {
                rgb(42, 42, 50)
            })
            .add_modifier(ratatui::style::Modifier::BOLD);
    }
    Line::from(vec![
        Span::styled(marker, style),
        Span::styled("  ".repeat(row.depth), style),
        Span::styled(disclosure, style),
        Span::styled(row.name.clone(), style),
    ])
}

pub(super) fn draw_project_files(
    frame: &mut Frame,
    area: Rect,
    app: &dyn TuiState,
    snapshot: Option<&ProjectTreeSnapshot>,
    focused: bool,
) {
    if area.width < 30 || area.height < 3 {
        return;
    }
    let suffix = if let Some(snapshot) = snapshot {
        vec![
            Span::styled(
                snapshot.root_label.clone(),
                Style::default().fg(tool_color()),
            ),
            Span::styled(
                format!(
                    "  {} files{}",
                    snapshot.file_count,
                    if snapshot.truncated { "+" } else { "" }
                ),
                Style::default().fg(dim_color()),
            ),
        ]
    } else {
        vec![Span::styled("loading…", Style::default().fg(dim_color()))]
    };
    let title = project_pane_title(true, false, suffix);
    let border = Style::default().fg(if focused { tool_color() } else { dim_color() });
    let Some(inner) = super::draw_right_rail_chrome(frame, area, title, border) else {
        return;
    };
    super::clear_area(frame, inner);
    let (diff_tab_area, files_tab_area, terminal_tab_area) = project_pane_tab_areas(area);

    let Some(snapshot) = snapshot else {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Indexing project files…",
                Style::default().fg(dim_color()),
            ))),
            inner,
        );
        super::set_pinned_pane_total_lines(0);
        super::set_last_diff_pane_max_scroll(0);
        super::set_last_diff_pane_effective_scroll(0);
        WORKTREE_PANE_LAYOUT.with(|layout| {
            *layout.borrow_mut() = Some(WorktreePaneLayout {
                area,
                diff_tab_area,
                files_tab_area,
                terminal_tab_area,
                files_tab_active: true,
                terminal_tab_active: false,
                list_area: inner,
                body_area: inner,
                list_scroll: 0,
                paths: Arc::new(Vec::new()),
                tree_area: Some(inner),
                preview_area: None,
                tree_scroll: 0,
                tree_rows: Arc::new(Vec::new()),
                preview_total_lines: 0,
                preview_scroll: 0,
                working_dir: app.working_dir(),
            })
        });
        return;
    };

    let rows = cached_project_tree_rows(snapshot, app);
    let selected_index = app
        .project_tree_selected_path()
        .and_then(|path| rows.iter().position(|row| row.path == path))
        .unwrap_or(0)
        .min(rows.len().saturating_sub(1));
    let selected_file = rows
        .get(selected_index)
        .filter(|row| !row.is_dir)
        .map(|row| row.path.clone());
    let preview_lines = selected_file
        .as_deref()
        .map(|path| build_project_preview(&snapshot.root, path));
    let show_preview = preview_lines.is_some() && inner.height >= 10;
    let tree_height = if show_preview {
        (inner.height / 2).clamp(4, inner.height.saturating_sub(5))
    } else {
        inner.height
    };
    let tree_area = Rect::new(inner.x, inner.y, inner.width, tree_height);
    let tree_max_scroll = rows.len().saturating_sub(tree_area.height as usize);
    let mut tree_scroll = app.project_tree_scroll().min(tree_max_scroll);
    if selected_index < tree_scroll {
        tree_scroll = selected_index;
    } else if selected_index >= tree_scroll.saturating_add(tree_area.height as usize) {
        tree_scroll = selected_index
            .saturating_add(1)
            .saturating_sub(tree_area.height as usize)
            .min(tree_max_scroll);
    }
    let visible_tree_lines = rows
        .iter()
        .enumerate()
        .skip(tree_scroll)
        .take(tree_area.height as usize)
        .map(|(index, row)| {
            project_tree_line(
                row,
                index == selected_index,
                focused && !app.project_tree_preview_focused(),
            )
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(visible_tree_lines.clone()), tree_area);

    let mut preview_area = None;
    let mut preview_total_lines = 0;
    let mut preview_scroll = 0;
    if let (Some(path), Some(lines)) = (selected_file.as_deref(), preview_lines.as_ref())
        && show_preview
    {
        let separator_y = tree_area.bottom();
        frame.render_widget(
            Paragraph::new(Span::styled(
                "━".repeat(inner.width as usize),
                Style::default().fg(border_color()),
            )),
            Rect::new(inner.x, separator_y, inner.width, 1),
        );
        let header_area = Rect::new(inner.x, separator_y.saturating_add(1), inner.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" {}", project_path_label(path)),
                    Style::default()
                        .fg(file_link_color())
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
                Span::styled(
                    if app.project_tree_preview_focused() {
                        "  preview focus"
                    } else {
                        "  Enter to scroll"
                    },
                    Style::default().fg(dim_color()),
                ),
            ])),
            header_area,
        );
        let body = Rect::new(
            inner.x,
            header_area.bottom(),
            inner.width,
            inner.bottom().saturating_sub(header_area.bottom()),
        );
        preview_total_lines = lines.len();
        let max_scroll = preview_total_lines.saturating_sub(body.height as usize);
        preview_scroll = app.project_tree_preview_scroll().min(max_scroll);
        let visible = lines
            .iter()
            .skip(preview_scroll)
            .take(body.height as usize)
            .cloned()
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(visible.clone()), body);
        if app.project_tree_preview_focused() {
            super::set_pinned_pane_total_lines(preview_total_lines);
            super::set_last_diff_pane_max_scroll(max_scroll);
            super::set_last_diff_pane_effective_scroll(preview_scroll);
            super::record_side_pane_snapshot(
                lines.as_slice(),
                preview_scroll,
                (preview_scroll + body.height as usize).min(preview_total_lines),
                body,
            );
        }
        preview_area = Some(body);
    }
    if !app.project_tree_preview_focused() {
        super::set_pinned_pane_total_lines(rows.len());
        super::set_last_diff_pane_max_scroll(tree_max_scroll);
        super::set_last_diff_pane_effective_scroll(tree_scroll);
        super::record_side_pane_snapshot(
            &visible_tree_lines,
            0,
            visible_tree_lines.len(),
            tree_area,
        );
    }
    WORKTREE_PANE_LAYOUT.with(|layout| {
        *layout.borrow_mut() = Some(WorktreePaneLayout {
            area,
            diff_tab_area,
            files_tab_area,
            terminal_tab_area,
            files_tab_active: true,
            terminal_tab_active: false,
            list_area: tree_area,
            body_area: preview_area.unwrap_or(tree_area),
            list_scroll: tree_scroll,
            paths: Arc::new(Vec::new()),
            tree_area: Some(tree_area),
            preview_area,
            tree_scroll,
            tree_rows: rows,
            preview_total_lines,
            preview_scroll,
            working_dir: app.working_dir(),
        })
    });
}

pub(super) fn draw_empty_worktree_changes(
    frame: &mut Frame,
    area: Rect,
    app: &dyn TuiState,
    focused: bool,
) {
    if area.width < 30 || area.height < 3 {
        return;
    }
    let title = project_pane_title(
        false,
        false,
        vec![Span::styled(
            "working tree clean",
            Style::default().fg(dim_color()),
        )],
    );
    let border = Style::default().fg(if focused { tool_color() } else { dim_color() });
    let Some(inner) = super::draw_right_rail_chrome(frame, area, title, border) else {
        return;
    };
    super::clear_area(frame, inner);
    let line = Line::from(Span::styled(
        "No uncommitted changes. Switch to Files to browse the project.",
        Style::default().fg(dim_color()),
    ));
    frame.render_widget(Paragraph::new(line.clone()), inner);
    super::set_pinned_pane_total_lines(1);
    super::set_last_diff_pane_max_scroll(0);
    super::set_last_diff_pane_effective_scroll(0);
    super::record_side_pane_snapshot(std::slice::from_ref(&line), 0, 1, inner);
    let (diff_tab_area, files_tab_area, terminal_tab_area) = project_pane_tab_areas(area);
    WORKTREE_PANE_LAYOUT.with(|layout| {
        *layout.borrow_mut() = Some(WorktreePaneLayout {
            area,
            diff_tab_area,
            files_tab_area,
            terminal_tab_area,
            files_tab_active: false,
            terminal_tab_active: false,
            list_area: inner,
            body_area: inner,
            list_scroll: 0,
            paths: Arc::new(Vec::new()),
            tree_area: None,
            preview_area: None,
            tree_scroll: 0,
            tree_rows: Arc::new(Vec::new()),
            preview_total_lines: 0,
            preview_scroll: 0,
            working_dir: app.working_dir(),
        })
    });
}

fn apply_terminal_sgr(style: &mut Style, parameters: &str) {
    use ratatui::style::{Color, Modifier};

    let values = if parameters.is_empty() {
        vec![0]
    } else {
        parameters
            .split(';')
            .map(|value| value.parse::<u16>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    let mut index = 0;
    while index < values.len() {
        let value = values[index];
        match value {
            0 => *style = Style::default(),
            1 => *style = style.add_modifier(Modifier::BOLD),
            2 => *style = style.add_modifier(Modifier::DIM),
            3 => *style = style.add_modifier(Modifier::ITALIC),
            4 => *style = style.add_modifier(Modifier::UNDERLINED),
            7 => *style = style.add_modifier(Modifier::REVERSED),
            22 => *style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => *style = style.remove_modifier(Modifier::ITALIC),
            24 => *style = style.remove_modifier(Modifier::UNDERLINED),
            27 => *style = style.remove_modifier(Modifier::REVERSED),
            30..=37 => *style = style.fg(Color::Indexed((value - 30) as u8)),
            39 => *style = style.fg(Color::Reset),
            40..=47 => *style = style.bg(Color::Indexed((value - 40) as u8)),
            49 => *style = style.bg(Color::Reset),
            90..=97 => *style = style.fg(Color::Indexed((value - 90 + 8) as u8)),
            100..=107 => *style = style.bg(Color::Indexed((value - 100 + 8) as u8)),
            38 | 48 if values.get(index + 1) == Some(&5) => {
                if let Some(color) = values
                    .get(index + 2)
                    .and_then(|value| u8::try_from(*value).ok())
                {
                    if value == 38 {
                        *style = style.fg(Color::Indexed(color));
                    } else {
                        *style = style.bg(Color::Indexed(color));
                    }
                    index += 2;
                }
            }
            38 | 48 if values.get(index + 1) == Some(&2) => {
                let rgb = values.get(index + 2..index + 5).and_then(|rgb| {
                    Some((
                        u8::try_from(*rgb.first()?).ok()?,
                        u8::try_from(*rgb.get(1)?).ok()?,
                        u8::try_from(*rgb.get(2)?).ok()?,
                    ))
                });
                if let Some((red, green, blue)) = rgb {
                    if value == 38 {
                        *style = style.fg(Color::Rgb(red, green, blue));
                    } else {
                        *style = style.bg(Color::Rgb(red, green, blue));
                    }
                    index += 4;
                }
            }
            _ => {}
        }
        index += 1;
    }
}

pub(super) fn terminal_ansi_line(line: &str) -> Line<'static> {
    let mut spans = Vec::new();
    let mut style = Style::default();
    let mut text = String::new();
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' || chars.peek() != Some(&'[') {
            text.push(ch);
            continue;
        }
        chars.next();
        let mut parameters = String::new();
        let mut is_sgr = false;
        for next in chars.by_ref() {
            if next == 'm' {
                is_sgr = true;
                break;
            }
            if ('@'..='~').contains(&next) {
                break;
            }
            parameters.push(next);
        }
        if !text.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut text), style));
        }
        if is_sgr {
            apply_terminal_sgr(&mut style, &parameters);
        }
    }
    if !text.is_empty() || spans.is_empty() {
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

#[cfg(test)]
mod terminal_ansi_tests {
    use super::*;
    use ratatui::style::{Color, Modifier};

    #[test]
    fn terminal_ansi_line_maps_standard_indexed_and_rgb_colours() {
        let line = terminal_ansi_line(
            "plain \x1b[1;31mred\x1b[0m \x1b[38;5;42mindexed\x1b[38;2;1;2;3mrgb",
        );
        assert_eq!(line.spans.len(), 5);
        assert_eq!(line.spans[0].content, "plain ");
        assert_eq!(line.spans[1].content, "red");
        assert_eq!(line.spans[1].style.fg, Some(Color::Indexed(1)));
        assert!(line.spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[3].style.fg, Some(Color::Indexed(42)));
        assert_eq!(line.spans[4].style.fg, Some(Color::Rgb(1, 2, 3)));
    }
}

pub(super) fn draw_project_terminal(
    frame: &mut Frame,
    area: Rect,
    app: &dyn TuiState,
    focused: bool,
) {
    if area.width < 30 || area.height < 3 {
        return;
    }
    let cwd = app.project_terminal_cwd().unwrap_or(".");
    let title = project_pane_title(
        false,
        true,
        vec![Span::styled(
            cwd.to_string(),
            Style::default().fg(dim_color()),
        )],
    );
    let border = Style::default().fg(if focused { tool_color() } else { dim_color() });
    let Some(inner) = super::draw_right_rail_chrome(frame, area, title, border) else {
        return;
    };
    super::clear_area(frame, inner);

    let output_height = inner.height.saturating_sub(1) as usize;
    let lines = app.project_terminal_lines();
    let start = lines.len().saturating_sub(output_height);
    let mut rendered = lines[start..]
        .iter()
        .map(|line| terminal_ansi_line(line))
        .collect::<Vec<_>>();
    if app.project_terminal_running() {
        rendered.push(Line::from(Span::styled(
            "⠋ command running…",
            Style::default().fg(tool_color()),
        )));
    } else {
        rendered.push(Line::from(vec![
            Span::styled(
                format!("{} ", app.project_terminal_prompt()),
                Style::default().fg(tool_color()),
            ),
            Span::raw(app.project_terminal_input().to_string()),
            Span::styled(
                if focused { "█" } else { "" },
                Style::default().fg(file_link_color()),
            ),
        ]));
    }
    frame.render_widget(Paragraph::new(rendered.clone()), inner);
    super::set_pinned_pane_total_lines(lines.len().saturating_add(1));
    super::set_last_diff_pane_max_scroll(0);
    super::set_last_diff_pane_effective_scroll(start);
    super::record_side_pane_snapshot(&rendered, start, lines.len().saturating_add(1), inner);

    let (diff_tab_area, files_tab_area, terminal_tab_area) = project_pane_tab_areas(area);
    WORKTREE_PANE_LAYOUT.with(|layout| {
        *layout.borrow_mut() = Some(WorktreePaneLayout {
            area,
            diff_tab_area,
            files_tab_area,
            terminal_tab_area,
            files_tab_active: false,
            terminal_tab_active: true,
            list_area: inner,
            body_area: inner,
            list_scroll: 0,
            paths: Arc::new(Vec::new()),
            tree_area: None,
            preview_area: None,
            tree_scroll: 0,
            tree_rows: Arc::new(Vec::new()),
            preview_total_lines: lines.len().saturating_add(1),
            preview_scroll: start,
            working_dir: app.working_dir(),
        })
    });
}

pub(super) fn draw_worktree_changes(
    frame: &mut Frame,
    area: Rect,
    app: &dyn TuiState,
    snapshot: &WorktreeChangesSnapshot,
    scroll: usize,
    focused: bool,
) {
    if area.width < 30 || area.height < 3 || snapshot.is_empty() {
        return;
    }
    let title = project_pane_title(
        false,
        false,
        vec![
            Span::styled("changes ", Style::default().fg(tool_color())),
            Span::styled(
                format!("{} files", snapshot.files.len()),
                Style::default()
                    .fg(rgb(220, 225, 235))
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                format!("+{}", snapshot.additions),
                Style::default().fg(diff_add_color()),
            ),
            Span::raw(" "),
            Span::styled(
                format!("-{}", snapshot.deletions),
                Style::default().fg(diff_del_color()),
            ),
        ],
    );
    let border = Style::default().fg(if focused { tool_color() } else { dim_color() });
    let Some(inner) = super::draw_right_rail_chrome(frame, area, title, border) else {
        return;
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // The index gets at most half the rail, leaving a usable body even for a
    // worktree with many files. Its own wheel target exposes overflow files.
    let selected = app
        .worktree_selected_file()
        .filter(|path| snapshot.files.iter().any(|file| file.path == *path));
    let list_height =
        (snapshot.files.len() as u16).min((inner.height.saturating_sub(2) / 2).max(1));
    let list_area = Rect::new(inner.x, inner.y, inner.width, list_height);
    let list_scroll = app
        .worktree_file_list_scroll()
        .min(snapshot.files.len().saturating_sub(list_height as usize));
    let header_bottom = list_area.bottom().saturating_add(1).min(inner.bottom());
    // Isolate the fixed file index + filter hint from the scrolling diff body.
    // Preserve a small usable diff viewport first, then spend remaining rows on
    // a heavy rule and one padding row. Tiny panes degrade without underflow.
    const MIN_DIFF_BODY_HEIGHT: u16 = 3;
    let boundary_height = PADDED_SECTION_BOUNDARY_HEIGHT.min(
        inner
            .bottom()
            .saturating_sub(header_bottom)
            .saturating_sub(MIN_DIFF_BODY_HEIGHT),
    );
    let boundary_area = Rect::new(inner.x, header_bottom, inner.width, boundary_height);
    let body_y = header_bottom
        .saturating_add(boundary_height)
        .min(inner.bottom());
    let body = Rect::new(
        inner.x,
        body_y,
        inner.width,
        inner.bottom().saturating_sub(body_y),
    );
    super::clear_area(frame, inner);
    frame.render_widget(
        Paragraph::new(
            build_file_index(snapshot, selected)
                .into_iter()
                .skip(list_scroll)
                .take(list_height as usize)
                .collect::<Vec<_>>(),
        ),
        list_area,
    );
    let mut hint = if selected.is_some() {
        "1 file · click again: all · 60s idle".to_string()
    } else {
        "all files · click to filter".to_string()
    };
    if snapshot.files.len() > list_height as usize {
        hint = format!(
            "{}–{}/{} ↕ · {}",
            list_scroll + 1,
            list_scroll + list_height as usize,
            snapshot.files.len(),
            hint
        );
    }
    if snapshot.truncated {
        hint = format!("first {MAX_FILES} files · {hint}");
    }
    if body_y > list_area.bottom() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                hint,
                Style::default().fg(dim_color()),
            ))),
            Rect::new(inner.x, list_area.bottom(), inner.width, 1),
        );
    }
    draw_padded_section_boundary(frame, boundary_area, true);
    let (diff_tab_area, files_tab_area, terminal_tab_area) = project_pane_tab_areas(area);
    WORKTREE_PANE_LAYOUT.with(|layout| {
        *layout.borrow_mut() = Some(WorktreePaneLayout {
            area,
            diff_tab_area,
            files_tab_area,
            terminal_tab_area,
            files_tab_active: false,
            terminal_tab_active: false,
            list_area,
            body_area: body,
            list_scroll,
            paths: Arc::new(
                snapshot
                    .files
                    .iter()
                    .map(|file| file.path.clone())
                    .collect(),
            ),
            tree_area: None,
            preview_area: None,
            tree_scroll: 0,
            tree_rows: Arc::new(Vec::new()),
            preview_total_lines: 0,
            preview_scroll: 0,
            working_dir: app.working_dir(),
        })
    });

    let lines = cached_render_lines(snapshot, selected);
    let total_lines = lines.len();
    super::set_pinned_pane_total_lines(total_lines);
    let max_scroll = total_lines.saturating_sub(body.height as usize);
    super::set_last_diff_pane_max_scroll(max_scroll);
    let scroll = scroll.min(max_scroll);
    super::set_last_diff_pane_effective_scroll(scroll);
    let visible_end = (scroll + body.height as usize).min(total_lines);
    super::record_side_pane_snapshot(lines.as_slice(), scroll, visible_end, body);

    let visible: Vec<Line<'static>> = lines
        .iter()
        .skip(scroll)
        .take(body.height as usize)
        .cloned()
        .collect();
    frame.render_widget(
        Paragraph::new(visible).scroll((0, app.diff_pane_scroll_x().max(0) as u16)),
        body,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_refresh_requests_one_redraw() {
        worktree_redraw_pending().store(true, Ordering::Release);
        assert!(poll_worktree_changes(None));
        assert!(!poll_worktree_changes(None));
    }

    #[test]
    fn nul_path_reader_stops_at_limit_and_marks_truncation() {
        let input = b"one.rs\0two.rs\0three.rs\0four.rs\0";
        let mut reader = BufReader::new(&input[..]);
        let (paths, truncated) = read_nul_paths_limited(&mut reader, 3).unwrap();

        assert_eq!(paths, ["one.rs", "two.rs", "three.rs"]);
        assert!(truncated);
    }

    #[test]
    fn parses_context_and_line_numbers_from_unified_diff() {
        let lines = parse_unified_diff(
            "diff --git a/demo.rs b/demo.rs\n--- a/demo.rs\n+++ b/demo.rs\n@@ -2,3 +2,3 @@\n same\n-old\n+new\n tail\n",
        );
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[1].old_line, Some(2));
        assert_eq!(lines[1].new_line, Some(2));
        assert_eq!(lines[2].kind, WorktreeLineKind::Del);
        assert_eq!(lines[2].old_line, Some(3));
        assert_eq!(lines[3].kind, WorktreeLineKind::Add);
        assert_eq!(lines[3].new_line, Some(3));
    }

    #[test]
    fn clean_repo_snapshot_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .current_dir(dir.path())
            .args(["init", "-q"])
            .status()
            .unwrap();
        let snapshot = collect_worktree_changes(dir.path()).unwrap();
        assert!(snapshot.is_empty());
    }

    #[test]
    fn untracked_text_file_is_rendered_as_additions() {
        let dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .current_dir(dir.path())
            .args(["init", "-q"])
            .status()
            .unwrap();
        std::fs::write(dir.path().join("new.txt"), "one\ntwo\n").unwrap();
        let snapshot = collect_worktree_changes(dir.path()).unwrap();
        assert_eq!(snapshot.files.len(), 1);
        assert_eq!(snapshot.additions, 2);
        assert_eq!(snapshot.files[0].path, "new.txt");
    }

    #[test]
    fn project_tree_honors_gitignore_and_builds_nested_directories() {
        let dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .current_dir(dir.path())
            .args(["init", "-q"])
            .status()
            .unwrap();
        std::fs::create_dir_all(dir.path().join("src/nested")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn lib() {}\n").unwrap();
        std::fs::write(dir.path().join("src/nested/mod.rs"), "pub mod leaf;\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.log\n").unwrap();
        std::fs::write(dir.path().join("ignored.log"), "hidden\n").unwrap();

        let snapshot = collect_project_tree(dir.path()).expect("project tree");
        fn collect(nodes: &[ProjectTreeNode], paths: &mut Vec<String>) {
            for node in nodes {
                paths.push(node.path.clone());
                collect(&node.children, paths);
            }
        }
        let mut paths = Vec::new();
        collect(&snapshot.nodes, &mut paths);
        assert!(paths.iter().any(|path| path == "src"));
        assert!(paths.iter().any(|path| path == "src/nested"));
        assert!(paths.iter().any(|path| path == "src/nested/mod.rs"));
        assert!(!paths.iter().any(|path| path == "ignored.log"));
    }

    #[test]
    fn project_tree_uses_git_root_when_session_starts_in_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .current_dir(dir.path())
            .args(["init", "-q"])
            .status()
            .unwrap();
        std::fs::create_dir_all(dir.path().join("project/src")).unwrap();
        std::fs::write(dir.path().join("outside.rs"), "fn outside() {}\n").unwrap();
        std::fs::write(dir.path().join("project/src/lib.rs"), "fn inside() {}\n").unwrap();
        Command::new("git")
            .current_dir(dir.path())
            .args(["add", "."])
            .status()
            .unwrap();

        let snapshot = collect_project_tree(&dir.path().join("project")).expect("project tree");
        assert_eq!(snapshot.root, dir.path().canonicalize().unwrap());
        assert!(snapshot.nodes.iter().any(|node| node.path == "project"));
        assert!(snapshot.nodes.iter().any(|node| node.path == "outside.rs"));
    }

    #[test]
    fn project_tree_uses_session_pwd_outside_git() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("project/src")).unwrap();
        std::fs::write(dir.path().join("outside.rs"), "fn outside() {}\n").unwrap();
        std::fs::write(dir.path().join("project/src/lib.rs"), "fn inside() {}\n").unwrap();

        let pwd = dir.path().join("project");
        let snapshot = collect_project_tree(&pwd).expect("project tree");
        assert_eq!(snapshot.root, pwd.canonicalize().unwrap());
        assert_eq!(snapshot.root_label, "project");
        assert!(snapshot.nodes.iter().any(|node| node.path == "src"));
        assert!(!snapshot.nodes.iter().any(|node| node.path == "outside.rs"));
    }

    #[test]
    fn project_tree_uses_linked_worktree_root() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        let linked = dir.path().join("linked");
        std::fs::create_dir(&main).unwrap();
        let git = |cwd: &Path, args: &[&str]| {
            assert!(
                Command::new("git")
                    .current_dir(cwd)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&main, &["init", "-q"]);
        std::fs::write(main.join("tracked.rs"), "fn tracked() {}\n").unwrap();
        git(&main, &["add", "."]);
        git(
            &main,
            &[
                "-c",
                "user.name=Jcode Test",
                "-c",
                "user.email=jcode@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ],
        );
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                linked.to_str().unwrap(),
            ],
        );
        std::fs::create_dir(linked.join("nested")).unwrap();

        let snapshot = collect_project_tree(&linked.join("nested")).expect("worktree tree");
        assert_eq!(snapshot.root, linked.canonicalize().unwrap());
        assert_eq!(snapshot.root_label, "linked");
        assert!(snapshot.nodes.iter().any(|node| node.path == "tracked.rs"));
    }

    #[test]
    fn project_tree_does_not_fallback_when_git_ignores_every_file() {
        let dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .current_dir(dir.path())
            .args(["init", "-q"])
            .status()
            .unwrap();
        std::fs::write(dir.path().join(".git/info/exclude"), "*.secret\n").unwrap();
        std::fs::write(dir.path().join("hidden.secret"), "ignored\n").unwrap();

        let snapshot = collect_project_tree(dir.path()).expect("project tree");
        assert_eq!(snapshot.file_count, 0);
        assert!(snapshot.nodes.is_empty());
    }

    #[test]
    fn project_tree_labels_escape_terminal_control_characters() {
        assert_eq!(project_path_label("bad\nname\tesc\u{1b}"), "bad␤name⇥esc�");
    }

    #[test]
    fn project_preview_blocks_paths_outside_root() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().parent().unwrap();
        let outside = tempfile::tempdir_in(parent).unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret\n").unwrap();
        let relative = format!(
            "../{}/secret.txt",
            outside.path().file_name().unwrap().to_string_lossy()
        );
        assert!(normalized_relative_components(&relative).is_none());
        let lines = build_project_preview(&root.path().canonicalize().unwrap(), &relative);
        let text = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(!text.contains("secret"));
    }

    #[cfg(unix)]
    #[test]
    fn project_preview_blocks_symlinks_that_escape_root() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), "outside secret\n").unwrap();
        symlink(outside.path(), root.path().join("escape.txt")).unwrap();

        let lines = build_project_preview(&root.path().canonicalize().unwrap(), "escape.txt");
        let text = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("blocked"));
        assert!(!text.contains("outside secret"));
    }

    #[cfg(unix)]
    #[test]
    fn project_preview_blocks_symlinked_parent_directories() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(
            outside.path().join("secret.rs"),
            "const SECRET: &str = \"hidden\";\n",
        )
        .unwrap();
        symlink(outside.path(), root.path().join("linked")).unwrap();

        let lines = build_project_preview(&root.path().canonicalize().unwrap(), "linked/secret.rs");
        let text = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("blocked"));
        assert!(!text.contains("hidden"));
    }
}
