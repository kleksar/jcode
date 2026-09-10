# Session Name Statusline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render the current session icon and friendly name at the far right of the stable TUI footer.

**Architecture:** Extend the footer compositor with a third, right-anchored span group. Build that group from `TuiState::session_display_name()` and `crate::id::session_icon()`, reserve its complete width before applying existing left-side truncation, and omit it when it cannot fit as a complete unit.

**Tech Stack:** Rust, Ratatui `Span`/`Line`/`Paragraph`, `unicode-width`, Ratatui `TestBackend`, Cargo tests.

## Global Constraints

- Render the label as `icon + space + friendly name`, for example `🐍 snake`.
- Keep directory, branch, model, context, and quota left-aligned in their current order.
- Keep the footer at one row and preserve the current tiny-terminal threshold.
- Do not change the header, terminal title, right-side fact stack, or session naming.
- When left content exists, show the complete session label only if its width plus a two-cell gap leaves at least one cell for the left side.
- Never render a partial icon or partial session name.

---

### Task 1: Right-anchored session label

**Files:**
- Modify: `crates/jcode-tui/src/tui/ui_input.rs:1251-1304,2491-2687`
- Test: `crates/jcode-tui/src/tui/ui_input.rs` inline `tests` module
- Test: `crates/jcode-tui/src/tui/app/tests/footer_quota.rs`

**Interfaces:**
- Consumes: `TuiState::session_display_name() -> Option<String>` and `crate::id::session_icon(name: &str) -> &'static str`.
- Produces: `footer_session_spans(app: &dyn TuiState) -> Vec<Span<'static>>` and `footer_compositor_spans(facts, quota, session, width) -> Vec<Span<'static>>`.

- [ ] **Step 1: Write failing compositor tests**

Add direct compositor tests and update existing calls to pass an empty session group:

```rust
#[test]
fn footer_compositor_right_aligns_complete_session_label() {
    let spans = footer_compositor_spans(
        vec![Span::raw("model")],
        vec![Span::raw("quota")],
        vec![Span::raw("🐍 snake")],
        30,
    );
    let text = spans.iter().map(|span| span.content.as_ref()).collect::<String>();
    assert_eq!(unicode_width::UnicodeWidthStr::width(text.as_str()), 30);
    assert!(text.starts_with("model  quota"));
    assert!(text.ends_with("🐍 snake"));
}

#[test]
fn footer_compositor_hides_session_label_when_it_cannot_fit_whole() {
    let spans = footer_compositor_spans(
        vec![Span::raw("model")],
        Vec::new(),
        vec![Span::raw("🐍 snake")],
        9,
    );
    let text = spans.iter().map(|span| span.content.as_ref()).collect::<String>();
    assert!(!text.contains("snake"));
    assert!(!text.contains('🐍'));
}
```

- [ ] **Step 2: Write a failing full-frame footer test**

In `footer_quota.rs`, render the existing local test app with `TestBackend`, extract the footer row, and assert that it ends with the exact current session label:

```rust
let footer = (0..140)
    .map(|x| terminal.backend().buffer()[(x, 29)].symbol())
    .collect::<String>();
let name = crate::tui::TuiState::session_display_name(&app).unwrap();
let expected = format!("{} {}", crate::id::session_icon(&name), name);
assert!(footer.trim_end().ends_with(&expected), "footer: {footer:?}");
```

- [ ] **Step 3: Run the new tests and verify RED**

Run:

```bash
PATH=/Volumes/macOS/Users/kleksar/.cargo/bin:$PATH cargo test -p jcode-tui --lib footer_compositor_ -- --nocapture
PATH=/Volumes/macOS/Users/kleksar/.cargo/bin:$PATH cargo test -p jcode-tui --lib footer_renders_current_session_label -- --nocapture
```

Expected: the compositor test initially fails to compile because the function lacks the session argument, and the frame assertion fails because the footer has no session label.

- [ ] **Step 4: Implement the session spans**

Add:

```rust
fn footer_session_spans(app: &dyn TuiState) -> Vec<Span<'static>> {
    let Some(name) = app.session_display_name().filter(|name| !name.trim().is_empty()) else {
        return Vec::new();
    };
    let icon = crate::id::session_icon(&name);
    vec![
        Span::styled(format!("{icon} "), Style::default().fg(rgb(120, 120, 132))),
        Span::styled(name, Style::default().fg(rgb(175, 175, 188)).bold()),
    ]
}
```

Pass `footer_session_spans(app)` from `draw_session_footer` into the compositor.

- [ ] **Step 5: Implement right-edge composition**

Change the compositor signature to:

```rust
fn footer_compositor_spans(
    facts: Vec<Span<'static>>,
    quota: Vec<Span<'static>>,
    session: Vec<Span<'static>>,
    width: usize,
) -> Vec<Span<'static>>
```

Preserve the existing quota reduction rules. Compose facts and quota as the left group. Compute `session_width` with `Span::width()`. If the left group is non-empty and `session_width + 2 + 1 > width`, omit the session group. Otherwise reserve `session_width + 2`, truncate the left group into the remaining width, insert enough plain-space padding to make the final display width equal `width`, and append the complete session group. If the left group is empty, right-align the complete label using only padding.

- [ ] **Step 6: Run focused tests and verify GREEN**

Run:

```bash
PATH=/Volumes/macOS/Users/kleksar/.cargo/bin:$PATH cargo test -p jcode-tui --lib footer_ -- --nocapture
```

Expected: all footer tests pass, including right alignment, narrow-width omission, existing fact/quota ordering, and the full-frame assertion.

- [ ] **Step 7: Run formatting, broader checks, and application build**

Run:

```bash
PATH=/Volumes/macOS/Users/kleksar/.cargo/bin:$PATH cargo fmt --all -- --check
PATH=/Volumes/macOS/Users/kleksar/.cargo/bin:$PATH cargo test -p jcode-tui --lib footer -- --nocapture
PATH=/Volumes/macOS/Users/kleksar/.cargo/bin:$PATH scripts/dev_cargo.sh build --profile selfdev -p jcode --bin jcode
git diff --check
```

Expected: commands exit 0. Existing unrelated warnings may remain, but no warning or failure may originate from the changed code.

- [ ] **Step 8: Verify a rendered candidate frame**

Use an isolated debug tester or an explicitly approved client reload. Capture a normal-width frame and confirm the footer ends with the current session icon and name while existing left facts retain their order. Do not switch shared client or daemon artifacts without the repository provenance and visual-acceptance gate.

- [ ] **Step 9: Commit the implementation**

```bash
git add crates/jcode-tui/src/tui/ui_input.rs crates/jcode-tui/src/tui/app/tests/footer_quota.rs
git diff --cached --check
git commit -m "feat(tui): show session name in statusline"
```
