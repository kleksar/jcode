use super::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
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
    pub list_area: Rect,
    pub body_area: Rect,
    pub list_scroll: usize,
    pub paths: Arc<Vec<String>>,
    pub working_dir: Option<String>,
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

fn command_output(repo: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn nul_paths(output: &[u8]) -> Vec<String> {
    output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect()
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
    let title = Line::from(vec![
        Span::styled(" changes ", Style::default().fg(tool_color())),
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
    ]);
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
    let body_y = inner
        .y
        .saturating_add(list_height)
        .saturating_add(1)
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
    WORKTREE_PANE_LAYOUT.with(|layout| {
        *layout.borrow_mut() = Some(WorktreePaneLayout {
            area,
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
}
