use super::*;

const FILTER_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(super) struct WorktreePaneState {
    pub(super) selected_file: Option<String>,
    pub(super) list_scroll: usize,
    pub(super) last_activity: Option<Instant>,
    session_id: String,
    working_dir: Option<String>,
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

    fn prepare_worktree_pane_state(&mut self) {
        if !self.worktree_pane_matches_session() {
            self.worktree_pane = WorktreePaneState {
                session_id: self.session.id.clone(),
                working_dir: self.session.working_dir.clone(),
                ..Default::default()
            };
        }
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
