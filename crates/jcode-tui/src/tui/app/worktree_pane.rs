use super::*;
pub(crate) use crate::tui::MarkdownDocumentMode;

const FILTER_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum WorktreePaneTab {
    Diff,
    #[default]
    Files,
    Documents,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct MarkdownDocumentUiState {
    pub(super) mode: MarkdownDocumentMode,
    scrolls: [usize; 3],
}

impl MarkdownDocumentUiState {
    fn scroll(&self) -> usize {
        self.scrolls[self.mode.index()]
    }

    fn save_scroll(&mut self, scroll: usize) {
        self.scrolls[self.mode.index()] = scroll;
    }
}

pub(super) struct WorktreePaneState {
    pub(super) selected_file: Option<String>,
    pub(super) list_scroll: usize,
    pub(super) last_activity: Option<Instant>,
    pub(super) explicit_open: bool,
    pub(super) tab: WorktreePaneTab,
    pub(super) tree_selected_path: Option<String>,
    pub(super) tree_scroll: usize,
    pub(super) tree_expanded_dirs: std::collections::HashSet<String>,
    pub(super) tree_preview_scroll: usize,
    pub(super) tree_preview_focused: bool,
    pub(super) document_ui: std::collections::HashMap<String, MarkdownDocumentUiState>,
    session_id: String,
    working_dir: Option<String>,
}

impl Default for WorktreePaneState {
    fn default() -> Self {
        Self {
            selected_file: None,
            list_scroll: 0,
            last_activity: None,
            explicit_open: true,
            tab: WorktreePaneTab::Files,
            tree_selected_path: None,
            tree_scroll: 0,
            tree_expanded_dirs: std::collections::HashSet::new(),
            tree_preview_scroll: 0,
            tree_preview_focused: false,
            document_ui: std::collections::HashMap::new(),
            session_id: String::new(),
            working_dir: None,
        }
    }
}

impl App {
    pub(super) fn worktree_pane_matches_session(&self) -> bool {
        self.worktree_pane.session_id == self.session.id
            && self.worktree_pane.working_dir == self.session.working_dir
    }

    pub(super) fn current_worktree_selected_file(&self) -> Option<&str> {
        self.worktree_pane_matches_session()
            .then_some(self.worktree_pane.selected_file.as_deref())
            .flatten()
    }

    pub(super) fn prepare_worktree_pane_state(&mut self) {
        if !self.worktree_pane_matches_session() {
            let explicit_open = self.worktree_pane.explicit_open;
            let tab = self.worktree_pane.tab;
            self.worktree_pane = WorktreePaneState {
                explicit_open,
                tab,
                session_id: self.session.id.clone(),
                working_dir: self.session.working_dir.clone(),
                ..Default::default()
            };
        }
    }

    pub(super) fn worktree_files_tab_active(&self) -> bool {
        self.worktree_pane.tab == WorktreePaneTab::Files
    }

    pub(super) fn worktree_documents_tab_active(&self) -> bool {
        self.worktree_pane.tab == WorktreePaneTab::Documents && self.worktree_documents_available()
    }

    pub(super) fn worktree_documents_available(&self) -> bool {
        self.side_panel.focused_page().is_some()
    }

    pub(super) fn markdown_document_mode(&self, page_id: &str) -> MarkdownDocumentMode {
        self.worktree_pane
            .document_ui
            .get(page_id)
            .map(|state| state.mode)
            .unwrap_or_default()
    }

    pub(super) fn focused_markdown_document_mode(&self) -> MarkdownDocumentMode {
        self.side_panel
            .focused_page_id
            .as_deref()
            .map(|id| self.markdown_document_mode(id))
            .unwrap_or_default()
    }

    pub(super) fn save_focused_document_ui(&mut self) {
        if let Some(page_id) = self.side_panel.focused_page_id.clone() {
            self.prepare_worktree_pane_state();
            self.worktree_pane
                .document_ui
                .entry(page_id)
                .or_default()
                .save_scroll(self.diff_pane_scroll);
        }
    }

    pub(super) fn restore_focused_document_ui(&mut self) {
        let Some(page_id) = self.side_panel.focused_page_id.clone() else {
            return;
        };
        self.prepare_worktree_pane_state();
        let state = *self.worktree_pane.document_ui.entry(page_id).or_default();
        self.diff_pane_scroll = state.scroll();
        self.diff_pane_auto_scroll = self.diff_pane_scroll == usize::MAX;
    }

    pub(super) fn reset_focused_document_ui(&mut self) {
        let Some(page_id) = self.side_panel.focused_page_id.clone() else {
            return;
        };
        self.prepare_worktree_pane_state();
        self.worktree_pane
            .document_ui
            .insert(page_id, Default::default());
        self.diff_pane_scroll = 0;
        self.diff_pane_scroll_x = 0;
        self.diff_pane_auto_scroll = false;
    }

    pub(super) fn cycle_markdown_document_mode(&mut self) -> bool {
        let Some(page_id) = self.side_panel.focused_page_id.clone() else {
            return false;
        };
        self.prepare_worktree_pane_state();
        let state = self.worktree_pane.document_ui.entry(page_id).or_default();
        state.save_scroll(self.diff_pane_scroll);
        state.mode = state.mode.next();
        self.diff_pane_scroll = state.scroll();
        self.diff_pane_auto_scroll = self.diff_pane_scroll == usize::MAX;
        true
    }

    fn set_markdown_document_mode(&mut self, mode: MarkdownDocumentMode) -> bool {
        let Some(page_id) = self.side_panel.focused_page_id.clone() else {
            return false;
        };
        self.prepare_worktree_pane_state();
        let state = self.worktree_pane.document_ui.entry(page_id).or_default();
        state.save_scroll(self.diff_pane_scroll);
        state.mode = mode;
        self.diff_pane_scroll = state.scroll();
        self.diff_pane_auto_scroll = self.diff_pane_scroll == usize::MAX;
        true
    }

    pub(super) fn focus_adjacent_document_page(&mut self, delta: isize) -> bool {
        let page_count = self.side_panel.pages.len();
        if page_count < 2 {
            return false;
        }
        let current = self
            .side_panel
            .focused_page_id
            .as_deref()
            .and_then(|id| self.side_panel.pages.iter().position(|page| page.id == id))
            .unwrap_or(0);
        self.save_focused_document_ui();
        let next = (current as isize + delta).rem_euclid(page_count as isize) as usize;
        let id = self.side_panel.pages[next].id.clone();
        self.side_panel.focused_page_id = Some(id.clone());
        self.last_side_panel_focus_id = Some(id);
        self.restore_focused_document_ui();
        crate::tui::clear_side_panel_render_caches();
        true
    }

    pub(super) fn worktree_pane_explicit_open(&self) -> bool {
        self.worktree_pane.explicit_open
    }

    pub(super) fn set_worktree_pane_tab(&mut self, tab: WorktreePaneTab) {
        if self.worktree_documents_tab_active() && tab != WorktreePaneTab::Documents {
            self.save_focused_document_ui();
        }
        self.prepare_worktree_pane_state();
        self.worktree_pane.explicit_open = true;
        if self.worktree_pane.tab == tab {
            return;
        }
        self.worktree_pane.tab = tab;
        self.worktree_pane.tree_preview_focused = false;
        self.reset_worktree_diff_scroll();
        if tab == WorktreePaneTab::Documents {
            self.restore_focused_document_ui();
        }
        self.set_status_notice(match tab {
            WorktreePaneTab::Diff => "Right pane: Diff (Tab switches to Files)",
            WorktreePaneTab::Files => {
                "Right pane: Files (arrows navigate, Enter opens Markdown or previews, Tab switches)"
            }
            WorktreePaneTab::Documents => "Right pane: Documents ([/] pages, m mode, Tab switches)",
        });
    }

    pub(super) fn open_project_files_pane(&mut self) {
        self.set_worktree_pane_tab(WorktreePaneTab::Files);
        self.set_diff_pane_focus(true);
    }

    /// Plain horizontal arrows leave an empty composer for the right-pane tabs.
    /// Keep this ahead of pane-local handlers so arrows never activate a Files
    /// selection or pan a document while performing the linear pane traversal.
    pub(super) fn handle_empty_composer_horizontal_navigation(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> bool {
        if !self.input.is_empty() || !modifiers.is_empty() {
            return false;
        }

        // The render snapshot is authoritative: stale focus must never enter a
        // hidden worktree pane after a resize or while another side panel owns
        // the right surface.
        if crate::tui::ui::worktree_pane_layout().is_none() {
            if self.diff_pane_focus {
                self.set_diff_pane_focus(false);
            }
            return false;
        }

        match (self.diff_pane_focus, code) {
            (false, KeyCode::Right) => {
                self.set_worktree_pane_tab(WorktreePaneTab::Diff);
                self.set_diff_pane_focus(true);
                true
            }
            (true, KeyCode::Right) => {
                match self.worktree_pane.tab {
                    WorktreePaneTab::Diff => self.set_worktree_pane_tab(WorktreePaneTab::Files),
                    WorktreePaneTab::Files if self.worktree_documents_available() => {
                        self.set_worktree_pane_tab(WorktreePaneTab::Documents)
                    }
                    WorktreePaneTab::Files | WorktreePaneTab::Documents => {}
                }
                true
            }
            (true, KeyCode::Left) => {
                match self.worktree_pane.tab {
                    WorktreePaneTab::Documents => {
                        self.set_worktree_pane_tab(WorktreePaneTab::Files)
                    }
                    WorktreePaneTab::Files => self.set_worktree_pane_tab(WorktreePaneTab::Diff),
                    WorktreePaneTab::Diff => self.set_diff_pane_focus(false),
                }
                true
            }
            _ => false,
        }
    }

    pub(super) fn close_project_files_pane(&mut self) {
        self.prepare_worktree_pane_state();
        self.worktree_pane.explicit_open = false;
        self.worktree_pane.tree_preview_focused = false;
        self.set_diff_pane_focus(false);
        self.set_status_notice("Project files pane hidden");
    }

    fn select_project_tree_index(
        &mut self,
        layout: &crate::tui::ui::WorktreePaneLayout,
        index: usize,
    ) {
        let Some(row) = layout.tree_rows.get(index) else {
            return;
        };
        self.prepare_worktree_pane_state();
        if self.worktree_pane.tree_selected_path.as_deref() != Some(&row.path) {
            self.worktree_pane.tree_preview_scroll = 0;
            self.worktree_pane.tree_preview_focused = false;
        }
        self.worktree_pane.tree_selected_path = Some(row.path.clone());
        if !row.is_dir {
            let capabilities = if std::path::Path::new(&row.path)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
            {
                super::files_inspector::FileInspectorCapabilities {
                    read: true,
                    source: true,
                    changes: true,
                }
            } else {
                super::files_inspector::FileInspectorCapabilities::source_and_changes()
            };
            if let Some(root) = self.session.working_dir.clone() {
                self.files_inspector.select_file(root, row.path.clone(), capabilities);
            }
        }
        let height = layout
            .tree_area
            .map(|area| area.height as usize)
            .unwrap_or(1)
            .max(1);
        let max_scroll = layout.tree_rows.len().saturating_sub(height);
        let mut scroll = layout.tree_scroll.min(max_scroll);
        if index < scroll {
            scroll = index;
        } else if index >= scroll.saturating_add(height) {
            scroll = index
                .saturating_add(1)
                .saturating_sub(height)
                .min(max_scroll);
        }
        self.worktree_pane.tree_scroll = scroll;
    }

    fn toggle_project_tree_dir(&mut self, path: &str, expanded: bool) {
        self.prepare_worktree_pane_state();
        if expanded {
            self.worktree_pane.tree_expanded_dirs.remove(path);
        } else {
            self.worktree_pane
                .tree_expanded_dirs
                .insert(path.to_string());
        }
    }

    fn scroll_project_preview(
        &mut self,
        layout: &crate::tui::ui::WorktreePaneLayout,
        delta: isize,
    ) {
        self.prepare_worktree_pane_state();
        let height = layout
            .preview_area
            .map(|area| area.height as usize)
            .unwrap_or(0);
        let max_scroll = layout.preview_total_lines.saturating_sub(height);
        let current = layout.preview_scroll.min(max_scroll);
        self.worktree_pane.tree_preview_scroll = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current.saturating_add(delta as usize).min(max_scroll)
        };
    }

    pub(super) fn handle_project_files_focus_key(&mut self, code: KeyCode) -> bool {
        let Some(layout) = crate::tui::ui::worktree_pane_layout() else {
            return false;
        };
        if !layout.files_tab_active {
            return false;
        }
        self.prepare_worktree_pane_state();
        if self.files_inspector.is_focused() {
            let page = layout
                .preview_area
                .map(|area| area.height.saturating_sub(1) as isize)
                .unwrap_or(1)
                .max(1);
            match code {
                KeyCode::Char('j') | KeyCode::Down => self.scroll_project_preview(&layout, 1),
                KeyCode::Char('k') | KeyCode::Up => self.scroll_project_preview(&layout, -1),
                KeyCode::Char('d') | KeyCode::PageDown => self.scroll_project_preview(&layout, page),
                KeyCode::Char('u') | KeyCode::PageUp => self.scroll_project_preview(&layout, -page),
                KeyCode::Char('g') | KeyCode::Home => self.worktree_pane.tree_preview_scroll = 0,
                KeyCode::Char('G') | KeyCode::End => self.worktree_pane.tree_preview_scroll = usize::MAX,
                KeyCode::Char('h') | KeyCode::Left | KeyCode::Esc => {
                    self.files_inspector.exit_focus();
                    self.set_status_notice("Files: tree focus");
                }
                _ => {}
            }
            return true;
        }
        if self.worktree_pane.tree_preview_focused {
            let page = layout
                .preview_area
                .map(|area| area.height.saturating_sub(1) as isize)
                .unwrap_or(1)
                .max(1);
            match code {
                KeyCode::Char('j') | KeyCode::Down => self.scroll_project_preview(&layout, 1),
                KeyCode::Char('k') | KeyCode::Up => self.scroll_project_preview(&layout, -1),
                KeyCode::Char('d') | KeyCode::PageDown => {
                    self.scroll_project_preview(&layout, page)
                }
                KeyCode::Char('u') | KeyCode::PageUp => self.scroll_project_preview(&layout, -page),
                KeyCode::Char('g') | KeyCode::Home => self.worktree_pane.tree_preview_scroll = 0,
                KeyCode::Char('G') | KeyCode::End => {
                    self.worktree_pane.tree_preview_scroll = usize::MAX
                }
                KeyCode::Char('h') | KeyCode::Left | KeyCode::Esc => {
                    self.worktree_pane.tree_preview_focused = false;
                    self.set_status_notice("Files: tree focus");
                }
                _ => {}
            }
            return true;
        }

        if layout.tree_rows.is_empty() {
            if code == KeyCode::Esc {
                self.set_diff_pane_focus(false);
            }
            return true;
        }
        let current = self
            .worktree_pane
            .tree_selected_path
            .as_deref()
            .and_then(|path| layout.tree_rows.iter().position(|row| row.path == path))
            .unwrap_or(0);
        let page = layout
            .tree_area
            .map(|area| area.height.saturating_sub(1) as usize)
            .unwrap_or(1)
            .max(1);
        let target = match code {
            KeyCode::Char('j') | KeyCode::Down => {
                Some((current + 1).min(layout.tree_rows.len() - 1))
            }
            KeyCode::Char('k') | KeyCode::Up => Some(current.saturating_sub(1)),
            KeyCode::Char('d') | KeyCode::PageDown => {
                Some(current.saturating_add(page).min(layout.tree_rows.len() - 1))
            }
            KeyCode::Char('u') | KeyCode::PageUp => Some(current.saturating_sub(page)),
            KeyCode::Char('g') | KeyCode::Home => Some(0),
            KeyCode::Char('G') | KeyCode::End => Some(layout.tree_rows.len() - 1),
            _ => None,
        };
        if let Some(index) = target {
            self.select_project_tree_index(&layout, index);
            return true;
        }

        let row = layout.tree_rows[current].clone();
        match code {
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('l') | KeyCode::Right => {
                if row.is_dir {
                    self.toggle_project_tree_dir(&row.path, row.expanded);
                } else if layout.preview_area.is_some() {
                    if self.files_inspector.enter_focus() {
                        self.set_status_notice("Files: inspector focus (j/k scroll, Left returns)");
                    }
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                if row.is_dir && row.expanded {
                    self.toggle_project_tree_dir(&row.path, true);
                } else if let Some((parent, _)) = row.path.rsplit_once('/')
                    && let Some(index) =
                        layout.tree_rows.iter().position(|item| item.path == parent)
                {
                    self.select_project_tree_index(&layout, index);
                }
            }
            KeyCode::Esc => self.set_diff_pane_focus(false),
            _ => {}
        }
        true
    }

    pub(super) fn note_worktree_pane_activity(&mut self) {
        if crate::tui::ui::worktree_pane_layout().is_some() {
            self.prepare_worktree_pane_state();
            self.worktree_pane.last_activity = Some(Instant::now());
        }
    }

    fn reset_worktree_diff_scroll(&mut self) {
        self.diff_pane_scroll = 0;
        self.diff_pane_scroll_x = 0;
        self.diff_pane_auto_scroll = false;
        if self.mouse_scroll_target == Some(MouseScrollTarget::SidePane) {
            self.mouse_scroll_queue = 0;
        }
        if self.current_copy_selection_pane() == Some(crate::tui::CopySelectionPane::SidePane)
            || self
                .copy_selection_pending_anchor
                .is_some_and(|point| point.pane == crate::tui::CopySelectionPane::SidePane)
        {
            self.exit_copy_selection_mode();
            self.copy_selection_edge_autoscroll = None;
        }
    }

    /// Called by both local and remote ticks, even when there are no input events.
    pub(super) fn update_worktree_file_filter(&mut self, now: Instant) -> bool {
        let Some(path) = self.worktree_pane.selected_file.as_deref() else {
            return false;
        };
        let expired = self
            .worktree_pane
            .last_activity
            .is_some_and(|last| now.saturating_duration_since(last) >= FILTER_IDLE_TIMEOUT);
        let stale = !self.worktree_pane_matches_session()
            || crate::tui::ui::worktree_file_is_present(self.session.working_dir.as_deref(), path)
                == Some(false);
        if !expired && !stale {
            return false;
        }
        self.worktree_pane.selected_file = None;
        self.worktree_pane.last_activity = None;
        // Do not move an explicit markdown/image pane that replaced the diff.
        if crate::tui::ui::worktree_pane_layout().is_some()
            || !crate::tui::ui::has_explicit_side_pane_content(self)
        {
            self.reset_worktree_diff_scroll();
        }
        true
    }

    /// Index clicks precede text selection. The diff body keeps normal copy/scroll behavior.
    pub(super) fn handle_worktree_pane_mouse(&mut self, mouse: MouseEvent) -> bool {
        let Some(layout) = crate::tui::ui::worktree_pane_layout() else {
            return false;
        };
        if layout.working_dir != self.session.working_dir
            || !crate::tui::layout_utils::point_in_rect(mouse.column, mouse.row, layout.area)
        {
            return false;
        }
        self.note_worktree_pane_activity();
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            if crate::tui::layout_utils::point_in_rect(
                mouse.column,
                mouse.row,
                layout.diff_tab_area,
            ) {
                self.set_worktree_pane_tab(WorktreePaneTab::Diff);
                self.set_diff_pane_focus(true);
                return true;
            }
            if crate::tui::layout_utils::point_in_rect(
                mouse.column,
                mouse.row,
                layout.documents_tab_area,
            ) {
                self.set_worktree_pane_tab(WorktreePaneTab::Documents);
                self.set_diff_pane_focus(true);
                return true;
            }
            if crate::tui::layout_utils::point_in_rect(
                mouse.column,
                mouse.row,
                layout.files_tab_area,
            ) {
                self.set_worktree_pane_tab(WorktreePaneTab::Files);
                self.set_diff_pane_focus(true);
                return true;
            }
            if let Some(mode) = [
                (layout.document_read_area, MarkdownDocumentMode::Read),
                (layout.document_source_area, MarkdownDocumentMode::Source),
                (layout.document_changes_area, MarkdownDocumentMode::Changes),
            ]
            .into_iter()
            .find_map(|(area, mode)| {
                crate::tui::layout_utils::point_in_rect(mouse.column, mouse.row, area)
                    .then_some(mode)
            }) {
                self.set_markdown_document_mode(mode);
                self.set_diff_pane_focus(true);
                return true;
            }
            if crate::tui::layout_utils::point_in_rect(
                mouse.column,
                mouse.row,
                layout.document_previous_page_area,
            ) {
                self.focus_adjacent_document_page(-1);
                self.set_diff_pane_focus(true);
                return true;
            }
            if crate::tui::layout_utils::point_in_rect(
                mouse.column,
                mouse.row,
                layout.document_next_page_area,
            ) {
                self.focus_adjacent_document_page(1);
                self.set_diff_pane_focus(true);
                return true;
            }
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && mouse.row == layout.area.y
        {
            // Header padding and the left rail chrome are not body controls.
            return false;
        }
        // Documents share the standard right-pane renderer and its smooth wheel
        // queue. Do not let the old worktree list fallback swallow body wheels.
        if self.worktree_documents_tab_active()
            && matches!(
                mouse.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            )
            && crate::tui::layout_utils::point_in_rect(mouse.column, mouse.row, layout.body_area)
        {
            return false;
        }
        if layout.files_tab_active {
            if let Some(tree_area) = layout.tree_area
                && crate::tui::layout_utils::point_in_rect(mouse.column, mouse.row, tree_area)
            {
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        let index = layout.tree_scroll + (mouse.row - tree_area.y) as usize;
                        if let Some(row) = layout.tree_rows.get(index).cloned() {
                            self.select_project_tree_index(&layout, index);
                            if row.is_dir {
                                self.toggle_project_tree_dir(&row.path, row.expanded);
                            }
                            self.set_diff_pane_focus(true);
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        let current = self
                            .worktree_pane
                            .tree_selected_path
                            .as_deref()
                            .and_then(|path| {
                                layout.tree_rows.iter().position(|row| row.path == path)
                            })
                            .unwrap_or(layout.tree_scroll);
                        self.select_project_tree_index(&layout, current.saturating_sub(3));
                    }
                    MouseEventKind::ScrollDown => {
                        let current = self
                            .worktree_pane
                            .tree_selected_path
                            .as_deref()
                            .and_then(|path| {
                                layout.tree_rows.iter().position(|row| row.path == path)
                            })
                            .unwrap_or(layout.tree_scroll);
                        let target = current
                            .saturating_add(3)
                            .min(layout.tree_rows.len().saturating_sub(1));
                        self.select_project_tree_index(&layout, target);
                    }
                    _ => {}
                }
                return true;
            }
            if let Some(preview_area) = layout.preview_area
                && crate::tui::layout_utils::point_in_rect(mouse.column, mouse.row, preview_area)
            {
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        self.prepare_worktree_pane_state();
                        self.worktree_pane.tree_preview_focused = true;
                        self.set_diff_pane_focus(true);
                    }
                    MouseEventKind::ScrollUp => self.scroll_project_preview(&layout, -3),
                    MouseEventKind::ScrollDown => self.scroll_project_preview(&layout, 3),
                    _ => {}
                }
                return true;
            }
            return true;
        }
        if !crate::tui::layout_utils::point_in_rect(mouse.column, mouse.row, layout.list_area) {
            return false;
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let index = layout.list_scroll + (mouse.row - layout.list_area.y) as usize;
                if let Some(path) = layout.paths.get(index) {
                    self.worktree_pane.selected_file =
                        if self.worktree_pane.selected_file.as_ref() == Some(path) {
                            None
                        } else {
                            Some(path.clone())
                        };
                    self.reset_worktree_diff_scroll();
                    self.set_diff_pane_focus(true);
                }
                true
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let max = layout
                    .paths
                    .len()
                    .saturating_sub(layout.list_area.height as usize);
                self.worktree_pane.list_scroll = if mouse.kind == MouseEventKind::ScrollUp {
                    layout.list_scroll.saturating_sub(3)
                } else {
                    layout.list_scroll.saturating_add(3).min(max)
                };
                true
            }
            _ => false,
        }
    }
}
