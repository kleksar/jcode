# Arrow-Key Workspace Navigation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the existing arrow-navigation implementation so the always-visible right pane defaults to Diff for a dirty worktree and Files for a clean worktree without overriding a user's active tab choice.

**Architecture:** Keep `WorktreePaneState` as the single source of right-pane tab state, but distinguish an explicit user selection from the computed session default. Resolve the effective default from the existing cached `snapshot_for_worktree` result, reset that default policy on session transitions, and route Chat-to-right navigation through the same policy for local and remote input.

**Tech Stack:** Rust, Crossterm key events, Ratatui TUI, existing Jcode worktree snapshot cache, Cargo unit tests.

## Global Constraints

- Navigation order remains `Sessions ↔ Chat ↔ Diff ↔ Files` with no wrapping at either boundary.
- Plain Left and Right navigate workspace regions only when the composer buffer is exactly empty.
- Non-empty and whitespace-only composer input retains normal cursor navigation.
- Modified arrows and non-session overlays retain their existing ownership.
- The right pane remains visible by default where the existing width and surface-precedence rules allow it.
- Dirty sessions default to Diff and clean sessions default to Files.
- New worktree changes must not replace a tab the user explicitly selected or steal pane focus.
- Session transitions recompute the default from the destination session instead of preserving an implicit prior-session default.
- Local and remote input paths must share the same Chat-to-right transition helper.
- Do not introduce new dependencies or configurable keybindings.

---

### Task 1: Model computed versus explicit right-pane tab selection

**Files:**
- Modify: `crates/jcode-tui/src/tui/app/worktree_pane.rs:5-100`
- Test: `crates/jcode-tui/src/tui/app/tests/worktree_pane.rs:102-121,244-282`

**Interfaces:**
- Consumes: `crate::tui::ui::snapshot_for_worktree(working_dir: Option<&str>) -> Option<Arc<WorktreeChangesSnapshot>>`.
- Produces: `App::default_worktree_pane_tab(&self) -> WorktreePaneTab`, `App::effective_worktree_pane_tab(&self) -> WorktreePaneTab`, and `App::focus_default_worktree_pane(&mut self)`.
- State contract: `WorktreePaneState::tab_explicitly_selected: bool` is false for a new session and true after `set_worktree_pane_tab` is called by a user action.

- [ ] **Step 1: Add failing clean/dirty default tests**

Extend `crates/jcode-tui/src/tui/app/tests/worktree_pane.rs` with tests that prime both caches and assert the effective tab:

```rust
#[test]
fn clean_session_defaults_to_files_and_dirty_session_defaults_to_diff() {
    let _lock = scroll_render_test_lock();
    let clean = tempfile::tempdir().expect("clean project");
    std::process::Command::new("git")
        .current_dir(clean.path())
        .args(["init", "-q"])
        .status()
        .expect("git init");
    crate::tui::ui::prime_worktree_changes_for_tests(clean.path());
    crate::tui::ui::prime_project_tree_for_tests(clean.path());

    let dirty = init_worktree_pane_test_repo();
    crate::tui::ui::prime_worktree_changes_for_tests(dirty.path());
    crate::tui::ui::prime_project_tree_for_tests(dirty.path());

    let mut app = create_test_app();
    app.session.working_dir = Some(clean.path().to_string_lossy().into_owned());
    assert_eq!(
        app.effective_worktree_pane_tab(),
        super::worktree_pane::WorktreePaneTab::Files
    );

    app.session.id = "dirty-session".to_string();
    app.session.working_dir = Some(dirty.path().to_string_lossy().into_owned());
    assert_eq!(
        app.effective_worktree_pane_tab(),
        super::worktree_pane::WorktreePaneTab::Diff
    );
}

#[test]
fn explicit_files_choice_is_not_replaced_when_changes_exist() {
    let _lock = scroll_render_test_lock();
    let dirty = init_worktree_pane_test_repo();
    crate::tui::ui::prime_worktree_changes_for_tests(dirty.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(dirty.path().to_string_lossy().into_owned());

    app.set_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Files);

    assert_eq!(
        app.effective_worktree_pane_tab(),
        super::worktree_pane::WorktreePaneTab::Files
    );
}
```

Replace `test_files_visibility_and_tab_choice_survive_session_transition` with a test that preserves `explicit_open` but verifies that a new session recomputes an implicit tab default.

- [ ] **Step 2: Run the focused tests and verify failure**

Run:

```bash
cargo test -p jcode-tui clean_session_defaults_to_files_and_dirty_session_defaults_to_diff
cargo test -p jcode-tui explicit_files_choice_is_not_replaced_when_changes_exist
```

Expected: FAIL because `effective_worktree_pane_tab` does not exist and current session preparation preserves the previous tab.

- [ ] **Step 3: Add the minimal tab-policy state and helpers**

In `worktree_pane.rs`, add this field immediately after `tab` in `WorktreePaneState`:

```rust
pub(super) tab: WorktreePaneTab,
pub(super) tab_explicitly_selected: bool,
```

Add these helpers inside the existing `impl App` block:

```rust
impl App {
    pub(super) fn default_worktree_pane_tab(&self) -> WorktreePaneTab {
        if crate::tui::ui::snapshot_for_worktree(self.session.working_dir.as_deref()).is_some() {
            WorktreePaneTab::Diff
        } else {
            WorktreePaneTab::Files
        }
    }

    pub(super) fn effective_worktree_pane_tab(&self) -> WorktreePaneTab {
        if self.worktree_pane_matches_session() && self.worktree_pane.tab_explicitly_selected {
            self.worktree_pane.tab
        } else {
            self.default_worktree_pane_tab()
        }
    }

    pub(super) fn focus_default_worktree_pane(&mut self) {
        let tab = self.default_worktree_pane_tab();
        self.prepare_worktree_pane_state();
        self.worktree_pane.tab = tab;
        self.worktree_pane.tab_explicitly_selected = false;
        self.worktree_pane.explicit_open = true;
        self.set_diff_pane_focus(true);
    }
}
```

Update `Default` to initialize `tab_explicitly_selected: false`. Update `prepare_worktree_pane_state` so a session mismatch carries only `explicit_open`; it must initialize the destination session with its computed default and `tab_explicitly_selected: false` rather than preserving `tab`. Update `set_worktree_pane_tab` to set `tab_explicitly_selected = true` even when the selected enum value already matches, because the user has asserted a preference. Update `worktree_files_tab_active` to compare `effective_worktree_pane_tab()` with `WorktreePaneTab::Files`.

- [ ] **Step 4: Run focused worktree-pane tests**

Run:

```bash
cargo test -p jcode-tui clean_session_defaults_to_files_and_dirty_session_defaults_to_diff
cargo test -p jcode-tui explicit_files_choice_is_not_replaced_when_changes_exist
cargo test -p jcode-tui worktree_pane -- --nocapture
```

Expected: PASS. Existing tests that intentionally preserve an explicit tab within one session remain green; the session-transition expectation is updated to the approved recomputation policy.

- [ ] **Step 5: Commit the tab policy**

```bash
git add crates/jcode-tui/src/tui/app/worktree_pane.rs \
  crates/jcode-tui/src/tui/app/tests/worktree_pane.rs
git commit -m "feat(tui): choose useful default worktree tab"
```

---

### Task 2: Route Chat-to-right navigation through the dynamic default

**Files:**
- Modify: `crates/jcode-tui/src/tui/app/input.rs:2700-2810,3112-3119`
- Modify: `crates/jcode-tui/src/tui/app/remote/key_handling.rs:897-918`
- Test: `crates/jcode-tui/src/tui/app/tests/arrow_navigation.rs:1-153`

**Interfaces:**
- Consumes: `App::focus_default_worktree_pane(&mut self)` from Task 1.
- Preserves: `handle_empty_composer_horizontal_navigation(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool`, shared by local and remote input.
- Updates: `handle_basic_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool` so the local basic-key path can enforce the same plain-arrow guard.
- Produces: Chat Right focuses Diff for a dirty cached worktree and Files for a clean cached worktree.

- [ ] **Step 1: Add failing local and remote navigation assertions**

Split the current assumption that Chat Right always opens Diff. Add one clean-repository case and one dirty-repository case:

```rust
#[test]
fn chat_right_focuses_files_for_clean_repo_and_diff_for_dirty_repo() {
    let _lock = scroll_render_test_lock();
    let clean = tempfile::tempdir().expect("clean project");
    std::process::Command::new("git")
        .current_dir(clean.path())
        .args(["init", "-q"])
        .status()
        .expect("git init");
    crate::tui::ui::prime_worktree_changes_for_tests(clean.path());

    let dirty = init_worktree_pane_test_repo();
    crate::tui::ui::prime_worktree_changes_for_tests(dirty.path());

    let mut app = create_test_app();
    app.session.working_dir = Some(clean.path().to_string_lossy().into_owned());
    app.handle_key(KeyCode::Right, KeyModifiers::NONE)
        .expect("clean chat to files");
    assert!(app.diff_pane_focus);
    assert!(app.worktree_files_tab_active());

    app.set_diff_pane_focus(false);
    app.session.id = "dirty-session".to_string();
    app.session.working_dir = Some(dirty.path().to_string_lossy().into_owned());
    app.handle_key(KeyCode::Right, KeyModifiers::NONE)
        .expect("dirty chat to diff");
    assert!(app.diff_pane_focus);
    assert!(!app.worktree_files_tab_active());
}
```

Add the same clean/dirty assertions to `remote_plain_arrows_traverse_views_and_keep_composer_and_overlay_guards` after priming the worktree cache, while retaining its overlay and non-empty-composer checks.

Add this regression test for whitespace and modified arrows:

```rust
#[test]
fn whitespace_and_modified_arrows_remain_composer_or_shortcut_owned() {
    let mut app = create_test_app();
    app.input = " ".to_string();
    app.cursor_pos = 1;

    app.handle_key(KeyCode::Left, KeyModifiers::NONE)
        .expect("move within whitespace input");
    assert_eq!(app.cursor_pos, 0);
    assert!(app.session_picker_overlay.is_none());

    app.input.clear();
    app.cursor_pos = 0;
    app.handle_key(KeyCode::Right, KeyModifiers::SHIFT)
        .expect("modified arrow remains outside workspace navigation");
    assert!(!app.diff_pane_focus);
}
```

- [ ] **Step 2: Run navigation tests and verify failure**

Run:

```bash
cargo test -p jcode-tui chat_right_focuses_files_for_clean_repo_and_diff_for_dirty_repo
cargo test -p jcode-tui remote_plain_arrows_traverse_views_and_keep_composer_and_overlay_guards
```

Expected: FAIL because `handle_empty_composer_horizontal_navigation` currently forces `WorktreePaneTab::Diff` and has no modifier guard.

- [ ] **Step 3: Use the shared default-focus helper**

Change the helper signature and guard it before selecting a destination:

```rust
pub(super) fn handle_empty_composer_horizontal_navigation(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> bool {
    if !modifiers.is_empty() || !app.input.is_empty() {
        return false;
    }

    match code {
        KeyCode::Left => {
            app.maybe_open_active_sessions_on_left();
            true
        }
        KeyCode::Right => {
            app.focus_default_worktree_pane();
            true
        }
        _ => false,
    }
}
```

Change `handle_basic_key` to accept `modifiers: KeyModifiers`, pass it to the helper in both arrow branches, and call it as `handle_basic_key(self, code, modifiers)` from `handle_key_core`. Update both remote arrow branches to call `input::handle_empty_composer_horizontal_navigation(app, code, modifiers)`.

Do not add a second remote-specific tab policy. The existing remote key handler must continue using the shared helper.

- [ ] **Step 4: Run all arrow-navigation and worktree-pane tests**

Run:

```bash
cargo test -p jcode-tui arrow_navigation -- --nocapture
cargo test -p jcode-tui worktree_pane -- --nocapture
```

Expected: PASS, including Sessions boundaries, Diff/Files transitions, overlay precedence, non-empty composer behavior, and local/remote parity.

- [ ] **Step 5: Commit the navigation integration**

```bash
git add crates/jcode-tui/src/tui/app/input.rs \
  crates/jcode-tui/src/tui/app/remote/key_handling.rs \
  crates/jcode-tui/src/tui/app/tests/arrow_navigation.rs
git commit -m "fix(tui): enter the useful right-pane tab"
```

---

### Task 3: Verify rendering, regressions, and the self-dev binary

**Files:**
- Modify only if a failing assertion reveals a defect in the files changed by Tasks 1-2.
- Verify: `crates/jcode-tui/src/tui/app/tests/state_model_poke_01/part_02.rs`
- Verify: `crates/jcode-tui/src/tui/app/tests/state_model_poke_02/part_01.rs`
- Verify: `crates/jcode-tui/src/tui/app/tests/worktree_pane.rs`
- Verify: `crates/jcode-tui/src/tui/app/tests/arrow_navigation.rs`

**Interfaces:**
- Consumes: completed tab policy and shared arrow-navigation helper.
- Produces: evidence that default visibility, rendering, keyboard focus, and local/remote dispatch satisfy the approved design.

- [ ] **Step 1: Run formatting and targeted test suites**

```bash
cargo fmt --all -- --check
cargo test -p jcode-tui arrow_navigation -- --nocapture
cargo test -p jcode-tui worktree_pane -- --nocapture
cargo test -p jcode-tui state_model_poke_01 -- --nocapture
cargo test -p jcode-tui state_model_poke_02 -- --nocapture
```

Expected: every command exits 0.

- [ ] **Step 2: Run the complete package test suite**

```bash
cargo test -p jcode-tui
```

Expected: PASS with no failed tests.

- [ ] **Step 3: Build the isolated self-dev binary**

```bash
cargo build --profile selfdev
./target/selfdev/jcode --version
```

Expected: build exits 0 and the version reports the branch's current commit rather than `8113215b9`.

- [ ] **Step 4: Exercise the binary without disturbing the shared daemon**

Use a dedicated macOS-safe socket path:

```bash
SOCKET="$TMPDIR/jcode-arrow-navigation-$$.sock"
./target/selfdev/jcode run --no-update --socket "$SOCKET" "Reply with exactly READY"
```

Expected: the isolated session starts successfully and returns `READY`. This validates that the built client/server pair launches; deterministic key transitions remain covered by the Ratatui tests because the command runner is non-interactive.

- [ ] **Step 5: Check final scope and commit any test-only adjustment**

```bash
git diff --check
git status --short
git log -3 --oneline
```

Expected: no unstaged implementation changes. If Task 3 required a narrowly scoped assertion correction, commit only that correction:

```bash
git add crates/jcode-tui/src/tui/app/tests/arrow_navigation.rs \
  crates/jcode-tui/src/tui/app/tests/worktree_pane.rs \
  crates/jcode-tui/src/tui/app/tests/state_model_poke_01/part_02.rs \
  crates/jcode-tui/src/tui/app/tests/state_model_poke_02/part_01.rs
git commit -m "test(tui): cover dynamic worktree pane defaults"
```
