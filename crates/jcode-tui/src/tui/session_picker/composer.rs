//! Active-session-only clean-session composer. Directory strings are server-authoritative.
//! This module intentionally performs no client filesystem access or canonicalization.
use crossterm::event::{KeyCode, KeyModifiers};
use std::time::{Duration, Instant};

const COMPLETION_DEBOUNCE: Duration = Duration::from_millis(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NewSessionComposerPhase {
    EditingPrompt,
    ChoosingDirectory,
    EditingPath,
    ResolvingPath,
    Creating,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkingDirectorySource {
    Here,
    SelectedSession,
    Recent,
    Home,
    Manual,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkingDirectoryChoice {
    pub(super) label: String,
    pub(super) absolute_path: String,
    source: WorkingDirectorySource,
}
#[derive(Debug, Clone)]
struct CreateSessionDraft {
    prompt: String,
    phase: NewSessionComposerPhase,
    confirmed_working_dir: Option<WorkingDirectoryChoice>,
    manual_path: String,
    validation_error: Option<String>,
    completion_due_at: Option<Instant>,
    completion_candidates: Vec<String>,
    completion_selected: Option<usize>,
    completion_truncated: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ComposerAction {
    Resolve { path: String },
    Create { prompt: String, working_dir: String },
}

#[derive(Debug, Clone)]
pub(super) struct NewSessionComposer {
    enabled: bool,
    draft: Option<CreateSessionDraft>,
    home_dir: Option<String>,
    recent_dirs: Vec<String>,
    source_dir: Option<String>,
    selected_dir: Option<String>,
    selected_index: usize,
    resolution: Option<(u64, String)>,
    creation: Option<u64>,
    unavailable: Option<String>,
}
impl Default for NewSessionComposer {
    fn default() -> Self {
        Self {
            enabled: false,
            draft: None,
            home_dir: None,
            recent_dirs: Vec::new(),
            source_dir: None,
            selected_dir: None,
            selected_index: 0,
            resolution: None,
            creation: None,
            unavailable: None,
        }
    }
}
impl NewSessionComposer {
    pub(super) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if enabled {
            self.unavailable = None;
        } else {
            self.draft = None;
            self.resolution = None;
            self.creation = None;
        }
    }
    pub(super) fn enabled(&self) -> bool {
        self.enabled
    }
    pub(super) fn set_source_dir(&mut self, dir: Option<String>) {
        self.source_dir = dir;
    }
    pub(super) fn set_context(&mut self, home_dir: String, recent_dirs: Vec<String>) {
        self.home_dir = Some(home_dir);
        self.recent_dirs = recent_dirs
            .into_iter()
            .filter(|path| !path.is_empty())
            .fold(Vec::new(), |mut unique, path| {
                if unique.len() < 5 && !unique.iter().any(|existing| existing == &path) {
                    unique.push(path);
                }
                unique
            });
        self.unavailable = None;
    }
    pub(super) fn unavailable(&mut self, message: String) {
        self.unavailable = Some(message);
    }
    pub(super) fn is_active(&self) -> bool {
        self.draft.is_some()
    }
    pub(super) fn prompt(&self) -> Option<&str> {
        self.draft.as_ref().map(|draft| draft.prompt.as_str())
    }

    pub(super) fn append_prompt_paste(&mut self, text: &str) -> bool {
        let Some(draft) = self.draft.as_mut() else {
            return false;
        };
        match draft.phase {
            NewSessionComposerPhase::EditingPrompt => draft.prompt.push_str(text),
            // A bracketed paste is direct manual path input, regardless of the
            // highlighted chooser row. Preserve the complete paste and use the
            // normal edit path so completion remains debounced.
            NewSessionComposerPhase::ChoosingDirectory => {
                draft.phase = NewSessionComposerPhase::EditingPath;
                draft.manual_path.push_str(text);
                Self::path_edited(draft);
            }
            NewSessionComposerPhase::EditingPath => {
                draft.manual_path.push_str(text);
                Self::path_edited(draft);
            }
            _ => return false,
        }
        true
    }
    fn phase(&self) -> Option<NewSessionComposerPhase> {
        self.draft.as_ref().map(|draft| draft.phase)
    }
    pub(super) fn directory_mode(&self) -> bool {
        self.phase()
            .is_some_and(|phase| phase != NewSessionComposerPhase::EditingPrompt)
    }
    pub(super) fn choosing_directory(&self) -> bool {
        self.phase() == Some(NewSessionComposerPhase::ChoosingDirectory)
    }
    pub(super) fn selected_index(&self) -> usize {
        self.selected_index
    }
    pub(super) fn manual_path(&self) -> Option<&str> {
        self.draft
            .as_ref()
            .filter(|draft| draft.phase == NewSessionComposerPhase::EditingPath)
            .map(|draft| draft.manual_path.as_str())
    }
    pub(super) fn completion_candidates(&self) -> Option<(&[String], Option<usize>, bool)> {
        self.draft
            .as_ref()
            .filter(|draft| draft.phase == NewSessionComposerPhase::EditingPath)
            .map(|draft| {
                (
                    draft.completion_candidates.as_slice(),
                    draft.completion_selected,
                    draft.completion_truncated,
                )
            })
    }
    pub(super) fn take_due_completion(&mut self, now: Instant) -> Option<String> {
        let draft = self.draft.as_mut()?;
        if draft.phase == NewSessionComposerPhase::EditingPath
            && draft.completion_due_at.is_some_and(|due| due <= now)
        {
            draft.completion_due_at = None;
            return Some(draft.manual_path.clone());
        }
        None
    }
    pub(super) fn completion_input(&self) -> Option<&str> {
        self.draft
            .as_ref()
            .filter(|draft| draft.phase == NewSessionComposerPhase::EditingPath)
            .map(|draft| draft.manual_path.as_str())
    }
    pub(super) fn begin_completion(&mut self, _id: u64, input: &str) -> bool {
        self.completion_input() == Some(input)
    }
    pub(super) fn apply_completions(
        &mut self,
        input: &str,
        candidates: Vec<String>,
        truncated: bool,
    ) -> bool {
        let Some(draft) = self.draft.as_mut() else {
            return false;
        };
        if draft.phase != NewSessionComposerPhase::EditingPath || draft.manual_path != input {
            return false;
        }
        draft.completion_candidates = candidates.into_iter().take(32).collect();
        draft.completion_selected = None;
        draft.completion_truncated = truncated;
        true
    }
    pub(super) fn fail_completion(&mut self, input: &str, message: String) -> bool {
        let Some(draft) = self.draft.as_mut() else {
            return false;
        };
        if draft.phase != NewSessionComposerPhase::EditingPath || draft.manual_path != input {
            return false;
        }
        draft.validation_error = Some(message);
        true
    }
    fn path_edited(draft: &mut CreateSessionDraft) {
        draft.validation_error = None;
        draft.completion_candidates.clear();
        draft.completion_selected = None;
        draft.completion_truncated = false;
        draft.completion_due_at = Some(Instant::now() + COMPLETION_DEBOUNCE);
    }
    pub(super) fn path_being_resolved(&self) -> Option<&str> {
        self.resolution.as_ref().map(|(_, path)| path.as_str())
    }
    pub(super) fn resolving_path(&self) -> bool {
        self.phase() == Some(NewSessionComposerPhase::ResolvingPath)
    }
    pub(super) fn resolved_path(&self) -> Option<&str> {
        self.draft.as_ref().and_then(|draft| {
            (draft.phase == NewSessionComposerPhase::Creating)
                .then(|| {
                    draft
                        .confirmed_working_dir
                        .as_ref()
                        .map(|choice| choice.absolute_path.as_str())
                })
                .flatten()
        })
    }
    pub(super) fn creating(&self) -> bool {
        self.phase() == Some(NewSessionComposerPhase::Creating)
    }
    pub(super) fn feedback(&self) -> Option<&str> {
        self.draft
            .as_ref()
            .and_then(|draft| draft.validation_error.as_deref())
            .or(self.unavailable.as_deref())
    }
    pub(super) fn choices(&self) -> Vec<WorkingDirectoryChoice> {
        let mut choices = Vec::new();
        let mut push = |label: String, path: String, source| {
            if !path.is_empty()
                && !choices
                    .iter()
                    .any(|choice: &WorkingDirectoryChoice| choice.absolute_path == path)
            {
                choices.push(WorkingDirectoryChoice {
                    label,
                    absolute_path: path,
                    source,
                });
            }
        };
        if let Some(path) = &self.source_dir {
            push("Here".into(), path.clone(), WorkingDirectorySource::Here);
        }
        if let Some(path) = &self.selected_dir {
            push(
                "Selected session".into(),
                path.clone(),
                WorkingDirectorySource::SelectedSession,
            );
        }
        for path in &self.recent_dirs {
            push(
                "Recent".into(),
                path.clone(),
                WorkingDirectorySource::Recent,
            );
        }
        if let Some(path) = &self.home_dir {
            push("Home".into(), path.clone(), WorkingDirectorySource::Home);
        }
        choices.push(WorkingDirectoryChoice {
            label: "Type path…".into(),
            absolute_path: String::new(),
            source: WorkingDirectorySource::Manual,
        });
        choices
    }
    pub(super) fn selected_choice(&self) -> Option<WorkingDirectoryChoice> {
        self.choices().get(self.selected_index).cloned()
    }
    fn clear_draft(&mut self) {
        self.draft = None;
        self.resolution = None;
        self.creation = None;
        self.selected_index = 0;
        self.selected_dir = None;
    }
    pub(super) fn apply_resolved(
        &mut self,
        id: u64,
        input: &str,
        absolute_path: String,
    ) -> Option<ComposerAction> {
        let Some((request, requested)) = self.resolution.as_ref() else {
            return None;
        };
        if *request != id || requested != input {
            return None;
        }
        let Some(draft) = self.draft.as_mut() else {
            return None;
        };
        if draft.phase != NewSessionComposerPhase::ResolvingPath {
            return None;
        }
        self.resolution = None;
        draft.confirmed_working_dir = Some(WorkingDirectoryChoice {
            label: "Resolved path".into(),
            absolute_path,
            source: WorkingDirectorySource::Manual,
        });
        draft.validation_error = None;
        // The Enter that initiated this request is the explicit create intent.
        // Only this matching, server-canonical resolution may consume it. Move
        // to Creating before emitting the action so repeated Enter cannot queue
        // a second create during the hand-off to the app tick.
        draft.phase = NewSessionComposerPhase::Creating;
        Some(ComposerAction::Create {
            prompt: draft.prompt.clone(),
            working_dir: draft
                .confirmed_working_dir
                .as_ref()
                .expect("resolved working directory was set")
                .absolute_path
                .clone(),
        })
    }
    pub(super) fn fail(&mut self, id: u64, message: String) -> bool {
        if self
            .resolution
            .as_ref()
            .is_some_and(|(request, _)| *request == id)
        {
            self.resolution = None;
            if let Some(draft) = self.draft.as_mut() {
                draft.phase = NewSessionComposerPhase::EditingPath;
                draft.validation_error = Some(message);
            }
            return true;
        }
        if self.creation == Some(id) {
            self.creation = None;
            if let Some(draft) = self.draft.as_mut() {
                // Preserve the typed draft after a rejected create. A later
                // Enter is an explicit fresh validation and retry, never an
                // automatic resend of an ambiguous create request.
                // A server-provided chooser selection is already authoritative,
                // so return to the prompt state where Enter creates with the
                // preserved choice. Manual paths must instead return to path
                // editing, which requires fresh server validation on retry.
                draft.phase = match draft.confirmed_working_dir.as_ref() {
                    Some(choice) if choice.source != WorkingDirectorySource::Manual => {
                        NewSessionComposerPhase::EditingPrompt
                    }
                    _ => NewSessionComposerPhase::EditingPath,
                };
                draft.validation_error = Some(message);
            }
            return true;
        }
        false
    }
    pub(super) fn begin_resolution(&mut self, id: u64, input: String) {
        if let Some(draft) = self.draft.as_mut() {
            if draft.phase == NewSessionComposerPhase::EditingPath && draft.manual_path == input {
                draft.phase = NewSessionComposerPhase::ResolvingPath;
                self.resolution = Some((id, input));
            }
        }
    }
    pub(super) fn begin_creation(&mut self, id: u64) {
        if let Some(draft) = self.draft.as_mut() {
            draft.phase = NewSessionComposerPhase::Creating;
            self.creation = Some(id);
        }
    }
    pub(super) fn complete_creation(&mut self, id: u64) -> bool {
        if self.creation == Some(id) {
            self.clear_draft();
            true
        } else {
            false
        }
    }
    pub(super) fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Option<ComposerAction> {
        if !self.enabled {
            return None;
        }
        if self.draft.is_none() {
            if let KeyCode::Char(c) = code {
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
                    self.draft = Some(CreateSessionDraft {
                        prompt: c.to_string(),
                        phase: NewSessionComposerPhase::EditingPrompt,
                        confirmed_working_dir: None,
                        manual_path: String::new(),
                        validation_error: None,
                        completion_due_at: None,
                        completion_candidates: Vec::new(),
                        completion_selected: None,
                        completion_truncated: false,
                    });
                }
            }
            return None;
        }
        let selected_choice = self.selected_choice();
        let draft = self.draft.as_mut().expect("draft checked");
        match draft.phase {
            NewSessionComposerPhase::EditingPrompt => match code {
                KeyCode::Char(c)
                    if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    draft.prompt.push(c)
                }
                KeyCode::Backspace => {
                    draft.prompt.pop();
                }
                KeyCode::Esc => {
                    if draft.prompt.is_empty() {
                        self.clear_draft();
                    } else {
                        self.clear_draft();
                    }
                }
                KeyCode::Enter if draft.prompt.is_empty() => {}
                KeyCode::Enter if draft.confirmed_working_dir.is_some() => {
                    let cwd = draft
                        .confirmed_working_dir
                        .as_ref()
                        .unwrap()
                        .absolute_path
                        .clone();
                    return Some(ComposerAction::Create {
                        prompt: draft.prompt.clone(),
                        working_dir: cwd,
                    });
                }
                KeyCode::Enter => {
                    draft.phase = NewSessionComposerPhase::ChoosingDirectory;
                    self.selected_index = 0;
                }
                _ => {}
            },
            NewSessionComposerPhase::ChoosingDirectory => match code {
                KeyCode::Esc => draft.phase = NewSessionComposerPhase::EditingPrompt,
                KeyCode::Up => self.selected_index = self.selected_index.saturating_sub(1),
                KeyCode::Down => {
                    let max = self.choices().len().saturating_sub(1);
                    self.selected_index = (self.selected_index + 1).min(max);
                }
                KeyCode::Enter => {
                    if let Some(choice) = selected_choice.clone() {
                        if choice.source == WorkingDirectorySource::Manual {
                            draft.phase = NewSessionComposerPhase::EditingPath;
                        } else {
                            // Remember the authoritative choice before asking the
                            // server to create. A rejected create returns to this
                            // draft, where Enter must retry the same cwd rather
                            // than silently losing it.
                            draft.confirmed_working_dir = Some(choice.clone());
                            return Some(ComposerAction::Create {
                                prompt: draft.prompt.clone(),
                                working_dir: choice.absolute_path,
                            });
                        }
                    }
                }
                // Printable input always begins manual path editing. This lets a
                // keyboard user type immediately from Here, Recent, or Home,
                // without first moving selection to the Type path row.
                KeyCode::Char(c)
                    if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    draft.phase = NewSessionComposerPhase::EditingPath;
                    draft.manual_path.push(c);
                    Self::path_edited(draft);
                }
                _ => {}
            },
            NewSessionComposerPhase::EditingPath => match code {
                KeyCode::Esc => {
                    draft.phase = NewSessionComposerPhase::ChoosingDirectory;
                    draft.completion_due_at = None;
                    draft.completion_candidates.clear();
                    draft.completion_selected = None;
                }
                KeyCode::Up => {
                    if !draft.completion_candidates.is_empty() {
                        draft.completion_selected =
                            Some(draft.completion_selected.unwrap_or(0).saturating_sub(1));
                    }
                }
                KeyCode::Down => {
                    if !draft.completion_candidates.is_empty() {
                        let last = draft.completion_candidates.len() - 1;
                        draft.completion_selected = Some(match draft.completion_selected {
                            None => 0,
                            Some(index) => index.saturating_add(1).min(last),
                        });
                    }
                }
                KeyCode::Tab => {
                    let candidate = draft
                        .completion_selected
                        .and_then(|i| draft.completion_candidates.get(i))
                        .cloned()
                        .or_else(|| {
                            (draft.completion_candidates.len() == 1)
                                .then(|| draft.completion_candidates[0].clone())
                        });
                    if let Some(candidate) = candidate {
                        draft.manual_path = candidate;
                        Self::path_edited(draft);
                    }
                }
                KeyCode::Backspace => {
                    draft.manual_path.pop();
                    Self::path_edited(draft);
                }
                KeyCode::Char(c)
                    if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    draft.manual_path.push(c);
                    Self::path_edited(draft);
                }
                KeyCode::Enter if draft.completion_selected.is_some() => {
                    if let Some(candidate) = draft
                        .completion_selected
                        .and_then(|i| draft.completion_candidates.get(i))
                        .cloned()
                    {
                        draft.manual_path = candidate;
                    }
                    return Some(ComposerAction::Resolve {
                        path: draft.manual_path.clone(),
                    });
                }
                KeyCode::Enter if !draft.manual_path.is_empty() => {
                    return Some(ComposerAction::Resolve {
                        path: draft.manual_path.clone(),
                    });
                }
                _ => {}
            },
            NewSessionComposerPhase::ResolvingPath => {
                if code == KeyCode::Esc {
                    self.resolution = None;
                    draft.phase = NewSessionComposerPhase::EditingPath;
                }
            }
            NewSessionComposerPhase::Creating => {}
        }
        None
    }
}
