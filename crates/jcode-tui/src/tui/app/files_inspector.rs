use std::collections::HashMap;
use std::path::PathBuf;

/// The lower inspector's available presentation modes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FileInspectorMode {
    Read,
    Source,
    Changes,
}

/// Capabilities for the currently selected file, supplied by the cached data owner.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct FileInspectorCapabilities {
    pub(super) read: bool,
    pub(super) source: bool,
    pub(super) changes: bool,
}

impl FileInspectorCapabilities {
    pub(super) const fn read() -> Self {
        Self {
            read: true,
            source: false,
            changes: false,
        }
    }

    pub(crate) const fn source_and_changes() -> Self {
        Self {
            read: false,
            source: true,
            changes: true,
        }
    }

    pub(super) fn supports(self, mode: FileInspectorMode) -> bool {
        match mode {
            FileInspectorMode::Read => self.read,
            FileInspectorMode::Source => self.source,
            FileInspectorMode::Changes => self.changes,
        }
    }

    pub(super) fn modes(self) -> impl Iterator<Item = FileInspectorMode> {
        [
            FileInspectorMode::Read,
            FileInspectorMode::Source,
            FileInspectorMode::Changes,
        ]
        .into_iter()
        .filter(move |mode| self.supports(*mode))
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct FileInspectorIdentity {
    root: PathBuf,
    path: PathBuf,
}

impl FileInspectorIdentity {
    fn new(root: PathBuf, path: PathBuf) -> Self {
        Self { root, path }
    }
}

/// State for the lower Files inspector. It intentionally contains no file content or I/O.
#[derive(Debug, Default)]
pub(super) struct FileInspectorUiState {
    selected: Option<FileInspectorIdentity>,
    capabilities: FileInspectorCapabilities,
    mode: Option<FileInspectorMode>,
    focused: bool,
    offsets: HashMap<(FileInspectorIdentity, FileInspectorMode), u16>,
}

impl FileInspectorUiState {
    pub(crate) fn select_file(
        &mut self,
        root: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
        capabilities: FileInspectorCapabilities,
    ) {
        let identity = FileInspectorIdentity::new(root.into(), path.into());
        let changed = self.selected.as_ref() != Some(&identity);
        self.selected = Some(identity.clone());
        self.capabilities = capabilities;
        if changed {
            self.mode = None;
        } else {
            self.mode = self.mode.filter(|mode| capabilities.supports(*mode));
        }
        if self.mode.is_none() {
            self.mode = capabilities.modes().next();
        }
        if changed {
            self.focused = false;
        }
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selected = None;
        self.capabilities = FileInspectorCapabilities::default();
        self.mode = None;
        self.focused = false;
    }

    pub(super) fn selected_file(&self) -> Option<(&PathBuf, &PathBuf)> {
        self.selected
            .as_ref()
            .map(|identity| (&identity.root, &identity.path))
    }

    pub(super) fn capabilities(&self) -> FileInspectorCapabilities {
        self.capabilities
    }

    pub(super) fn mode(&self) -> Option<FileInspectorMode> {
        self.mode
    }

    pub(super) fn select_mode(&mut self, mode: FileInspectorMode) -> bool {
        if !self.capabilities.supports(mode) {
            return false;
        }
        self.store_current_offset();
        self.mode = Some(mode);
        true
    }

    pub(super) fn available_modes(&self) -> impl Iterator<Item = FileInspectorMode> {
        self.capabilities.modes()
    }

    pub(crate) fn enter_focus(&mut self) -> bool {
        if self.selected.is_some() && self.mode.is_some() {
            self.focused = true;
            true
        } else {
            false
        }
    }

    pub(crate) fn exit_focus(&mut self) {
        self.store_current_offset();
        self.focused = false;
    }

    pub(super) fn is_focused(&self) -> bool {
        self.focused
    }

    pub(super) fn scroll_offset(&self) -> u16 {
        self.selected
            .as_ref()
            .zip(self.mode)
            .and_then(|(identity, mode)| self.offsets.get(&(identity.clone(), mode)).copied())
            .unwrap_or(0)
    }

    pub(super) fn set_scroll_offset(&mut self, offset: u16) {
        if let (Some(identity), Some(mode)) = (self.selected.clone(), self.mode) {
            self.offsets.insert((identity, mode), offset);
        }
    }

    fn store_current_offset(&mut self) {
        let offset = self.scroll_offset();
        self.set_scroll_offset(offset);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn filters_modes_and_uses_expected_defaults() {
        let mut state = FileInspectorUiState::default();
        state.select_file(
            "/repo",
            "README.md",
            FileInspectorCapabilities {
                read: true,
                source: true,
                changes: true,
            },
        );
        assert_eq!(state.mode(), Some(FileInspectorMode::Read));
        assert_eq!(
            state.available_modes().collect::<Vec<_>>(),
            vec![
                FileInspectorMode::Read,
                FileInspectorMode::Source,
                FileInspectorMode::Changes
            ]
        );
        assert!(state.select_mode(FileInspectorMode::Changes));
        state.select_file(
            "/repo",
            "main.py",
            FileInspectorCapabilities::source_and_changes(),
        );
        assert!(!state.select_mode(FileInspectorMode::Read));
        assert_eq!(state.mode(), Some(FileInspectorMode::Source));
    }

    #[test]
    fn restores_offsets_per_file_and_mode() {
        let mut state = FileInspectorUiState::default();
        state.select_file(
            "/repo",
            "a.py",
            FileInspectorCapabilities::source_and_changes(),
        );
        state.set_scroll_offset(4);
        state.select_mode(FileInspectorMode::Changes);
        assert_eq!(state.scroll_offset(), 0);
        state.set_scroll_offset(9);
        state.select_mode(FileInspectorMode::Source);
        assert_eq!(state.scroll_offset(), 4);
        state.select_file(
            "/repo",
            "b.py",
            FileInspectorCapabilities::source_and_changes(),
        );
        assert_eq!(state.scroll_offset(), 0);
        state.select_file(
            "/repo",
            "a.py",
            FileInspectorCapabilities::source_and_changes(),
        );
        assert_eq!(state.scroll_offset(), 4);
        state.select_mode(FileInspectorMode::Changes);
        assert_eq!(state.scroll_offset(), 9);
    }

    #[test]
    fn identity_includes_root_and_focus_transitions_are_safe() {
        let mut state = FileInspectorUiState::default();
        state.select_file(
            "/one",
            Path::new("src/main.py"),
            FileInspectorCapabilities::source_and_changes(),
        );
        assert!(state.enter_focus());
        assert!(state.is_focused());
        state.set_scroll_offset(7);
        state.exit_focus();
        assert!(!state.is_focused());
        state.select_file(
            "/two",
            Path::new("src/main.py"),
            FileInspectorCapabilities::source_and_changes(),
        );
        assert!(!state.is_focused());
        assert_eq!(state.scroll_offset(), 0);
    }
}
