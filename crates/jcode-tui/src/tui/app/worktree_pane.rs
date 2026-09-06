use super::*;

const FILTER_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_TERMINAL_LINES: usize = 2_000;
const MIN_TERMINAL_RUNNING_TIME: Duration = Duration::from_millis(500);

type TerminalCommandResult = Result<std::process::Output, String>;

pub(super) fn terminal_prompt_path(cwd: &str) -> String {
    let path = std::path::Path::new(cwd);
    if let Some(home) = dirs::home_dir() {
        if path == home {
            return "~".to_string();
        }
        if let Ok(relative) = path.strip_prefix(&home) {
            return format!("~/{}", relative.display());
        }
    }
    cwd.to_string()
}

pub(super) fn safe_terminal_output_lines(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\n' => lines.push(std::mem::take(&mut line)),
            // PTY output normally uses CRLF. Ignoring CR also avoids turning
            // every shell output row into two rows in the pane.
            '\r' => {}
            // macOS `script` closes its input with `^D\b\b`; interpreting
            // backspace rather than printing it removes that harmless marker.
            '\u{8}' => {
                line.pop();
            }
            '\u{1b}' => match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    let mut sequence = String::from("\u{1b}[");
                    let mut final_byte = None;
                    for next in chars.by_ref() {
                        sequence.push(next);
                        if ('@'..='~').contains(&next) {
                            final_byte = Some(next);
                            break;
                        }
                    }
                    // Preserve only SGR styling. Cursor movement, screen
                    // clearing, and other terminal control sequences must not
                    // affect Jcode's outer terminal.
                    if final_byte == Some('m') {
                        line.push_str(&sequence);
                    }
                }
                Some(']') => {
                    chars.next();
                    // Discard OSC sequences, including hyperlinks and title/
                    // clipboard controls, through BEL or ST.
                    let mut previous_escape = false;
                    for next in chars.by_ref() {
                        if next == '\u{7}' || (previous_escape && next == '\\') {
                            break;
                        }
                        previous_escape = next == '\u{1b}';
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            '\t' => line.push(ch),
            ch if !ch.is_control() => line.push(ch),
            _ => {}
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn run_interactive_shell_command(command: &str, cwd: &str) -> Result<std::process::Output, String> {
    #[cfg(windows)]
    {
        let shell = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
        return std::process::Command::new(shell)
            .arg("/C")
            .arg(command)
            .current_dir(cwd)
            .output()
            .map_err(|error| error.to_string());
    }

    #[cfg(not(windows))]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let mut process;

        // `script` gives the child a real PTY on macOS. That matters for the
        // user's normal shell setup: zsh loads ~/.zshrc in interactive mode,
        // aliases such as `la` exist, and tools emit the same colours they do
        // in iTerm. Other Unix platforms retain interactive rc loading and
        // colour-forcing even when `script` syntax differs.
        #[cfg(target_os = "macos")]
        {
            process = std::process::Command::new("/usr/bin/script");
            process
                .arg("-q")
                .arg("/dev/null")
                .arg(&shell)
                .arg("-ic")
                .arg(command);
        }
        #[cfg(not(target_os = "macos"))]
        {
            process = std::process::Command::new(&shell);
            process.arg("-ic").arg(command);
        }

        process
            .current_dir(cwd)
            .env(
                "TERM",
                std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into()),
            )
            .env("CLICOLOR_FORCE", "1")
            .env("FORCE_COLOR", "1")
            .output()
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum WorktreePaneTab {
    Diff,
    #[default]
    Files,
    Terminal,
}

impl WorktreePaneTab {
    pub(super) fn next(self) -> Self {
        match self {
            Self::Diff => Self::Files,
            Self::Files => Self::Terminal,
            Self::Terminal => Self::Diff,
        }
    }

    pub(super) fn previous(self) -> Self {
        match self {
            Self::Diff => Self::Terminal,
            Self::Files => Self::Diff,
            Self::Terminal => Self::Files,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Diff => "Diff",
            Self::Files => "Files",
            Self::Terminal => "Terminal",
        }
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
    pub(super) terminal_input: String,
    pub(super) terminal_lines: Vec<String>,
    pub(super) terminal_cwd: Option<String>,
    pub(super) terminal_running: bool,
    terminal_command_started_at: Option<Instant>,
    terminal_command_rx: Option<std::sync::mpsc::Receiver<TerminalCommandResult>>,
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
            terminal_input: String::new(),
            terminal_lines: vec!["Jcode terminal. Type a command and press Enter.".to_string()],
            terminal_cwd: None,
            terminal_running: false,
            terminal_command_started_at: None,
            terminal_command_rx: None,
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

    fn prepare_worktree_pane_state(&mut self) {
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

    pub(super) fn worktree_terminal_tab_active(&self) -> bool {
        self.worktree_pane.tab == WorktreePaneTab::Terminal
    }

    pub(super) fn worktree_pane_tab(&self) -> WorktreePaneTab {
        self.worktree_pane.tab
    }

    pub(super) fn worktree_pane_explicit_open(&self) -> bool {
        self.worktree_pane.explicit_open
    }

    pub(super) fn set_worktree_pane_tab(&mut self, tab: WorktreePaneTab) {
        self.prepare_worktree_pane_state();
        self.worktree_pane.explicit_open = true;
        if self.worktree_pane.tab == tab {
            return;
        }
        self.worktree_pane.tab = tab;
        self.worktree_pane.tree_preview_focused = false;
        self.reset_worktree_diff_scroll();
        self.set_status_notice(match tab {
            WorktreePaneTab::Diff => "Right pane: Diff (Tab switches tabs)",
            WorktreePaneTab::Files => {
                "Right pane: Files (arrows navigate, Enter expands/previews, Tab switches tabs)"
            }
            WorktreePaneTab::Terminal => {
                "Right pane: Terminal (type commands, Enter runs, Tab switches tabs)"
            }
        });
    }

    pub(super) fn handle_project_terminal_focus_key(&mut self, code: KeyCode) -> bool {
        if !self.worktree_terminal_tab_active() {
            return false;
        }
        self.prepare_worktree_pane_state();
        if self.worktree_pane.terminal_running {
            if code == KeyCode::Esc {
                self.set_diff_pane_focus(false);
            } else {
                self.set_status_notice("Terminal command is still running");
            }
            return true;
        }
        match code {
            KeyCode::Char(ch) => self.worktree_pane.terminal_input.push(ch),
            KeyCode::Backspace => {
                self.worktree_pane.terminal_input.pop();
            }
            KeyCode::Enter => self.run_project_terminal_command(),
            KeyCode::Esc => self.set_diff_pane_focus(false),
            KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => {}
            _ => {}
        }
        true
    }

    fn run_project_terminal_command(&mut self) {
        let command = std::mem::take(&mut self.worktree_pane.terminal_input);
        let command = command.trim().to_string();
        if command.is_empty() {
            return;
        }
        // `clear` normally emits screen-control sequences for a real terminal.
        // This pane is rendered by ratatui inside Jcode, so applying those
        // sequences to the outer terminal would be unsafe and printing them is
        // useless. Clear the pane's own scrollback instead.
        if command == "clear" {
            self.worktree_pane.terminal_lines.clear();
            return;
        }
        let cwd = self
            .worktree_pane
            .terminal_cwd
            .clone()
            .or_else(|| self.session.working_dir.clone())
            .unwrap_or_else(|| ".".to_string());
        self.worktree_pane.terminal_lines.push(format!(
            "{} {}",
            terminal_prompt_path(&cwd),
            command
        ));

        let cd_target = (command == "cd")
            .then_some("~")
            .or_else(|| command.strip_prefix("cd ").map(str::trim));
        if let Some(target) = cd_target {
            let target = if target.is_empty() || target == "~" {
                std::env::var("HOME").unwrap_or_else(|_| cwd.clone())
            } else {
                target.to_string()
            };
            let path = std::path::Path::new(&cwd).join(target);
            match path.canonicalize() {
                Ok(path) if path.is_dir() => {
                    self.worktree_pane.terminal_cwd = Some(path.to_string_lossy().into_owned());
                }
                _ => self
                    .worktree_pane
                    .terminal_lines
                    .push("cd: directory not found".to_string()),
            }
        } else {
            let (tx, rx) = std::sync::mpsc::channel();
            self.worktree_pane.terminal_command_rx = Some(rx);
            self.worktree_pane.terminal_running = true;
            self.worktree_pane.terminal_command_started_at = Some(Instant::now());
            std::thread::spawn(move || {
                let _ = tx.send(run_interactive_shell_command(&command, &cwd));
            });
        }
        self.trim_project_terminal_lines();
    }

    fn trim_project_terminal_lines(&mut self) {
        if self.worktree_pane.terminal_lines.len() > MAX_TERMINAL_LINES {
            let remove = self.worktree_pane.terminal_lines.len() - MAX_TERMINAL_LINES;
            self.worktree_pane.terminal_lines.drain(..remove);
        }
    }

    fn poll_project_terminal_command(&mut self) -> bool {
        if self
            .worktree_pane
            .terminal_command_started_at
            .is_some_and(|started| started.elapsed() < MIN_TERMINAL_RUNNING_TIME)
        {
            return false;
        }
        let result = match self
            .worktree_pane
            .terminal_command_rx
            .as_ref()
            .map(std::sync::mpsc::Receiver::try_recv)
        {
            Some(Ok(result)) => result,
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) | None => return false,
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                Err("terminal command worker disconnected".to_string())
            }
        };
        self.worktree_pane.terminal_command_rx = None;
        self.worktree_pane.terminal_running = false;
        self.worktree_pane.terminal_command_started_at = None;
        match result {
            Ok(output) => {
                self.worktree_pane
                    .terminal_lines
                    .extend(safe_terminal_output_lines(&output.stdout));
                self.worktree_pane
                    .terminal_lines
                    .extend(safe_terminal_output_lines(&output.stderr));
                if !output.status.success() {
                    self.worktree_pane.terminal_lines.push(format!(
                        "command exited with {}",
                        output
                            .status
                            .code()
                            .map_or_else(|| "signal".to_string(), |code| code.to_string())
                    ));
                }
            }
            Err(error) => self
                .worktree_pane
                .terminal_lines
                .push(format!("failed to run command: {error}")),
        }
        self.trim_project_terminal_lines();
        true
    }

    pub(super) fn open_project_files_pane(&mut self) {
        self.set_worktree_pane_tab(WorktreePaneTab::Files);
        self.set_diff_pane_focus(true);
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
                    self.worktree_pane.tree_preview_focused = true;
                    self.set_status_notice("Files: preview focus (j/k scroll, Left returns)");
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
        let terminal_updated = self.poll_project_terminal_command();
        let Some(path) = self.worktree_pane.selected_file.as_deref() else {
            return terminal_updated;
        };
        let expired = self
            .worktree_pane
            .last_activity
            .is_some_and(|last| now.saturating_duration_since(last) >= FILTER_IDLE_TIMEOUT);
        let stale = !self.worktree_pane_matches_session()
            || crate::tui::ui::worktree_file_is_present(self.session.working_dir.as_deref(), path)
                == Some(false);
        if !expired && !stale {
            return terminal_updated;
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
                layout.files_tab_area,
            ) {
                self.set_worktree_pane_tab(WorktreePaneTab::Files);
                self.set_diff_pane_focus(true);
                return true;
            }
            if crate::tui::layout_utils::point_in_rect(
                mouse.column,
                mouse.row,
                layout.terminal_tab_area,
            ) {
                self.set_worktree_pane_tab(WorktreePaneTab::Terminal);
                self.set_diff_pane_focus(true);
                return true;
            }
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
