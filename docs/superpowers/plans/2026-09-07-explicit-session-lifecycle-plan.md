# Explicit Session Lifecycle Controls Implementation Plan

> **For agentic workers:** Execute task-by-task with TDD. Use Jcode-native swarm orchestration only when independent review materially helps. Do not use `subagent-driven-development`.

**Goal:** Make terminal disconnects detach-only, add explicit server-authoritative session close from `/sessions`, add `Ctrl+A` Active/All recent filtering, and keep current-terminal resume responsive while the source session is working.

**Architecture:** The server remains the lifecycle authority. Transport disconnect cleanup removes only client-owned attachment state, while a new `CloseSession` protocol request performs the only user-initiated transition to `Closed`. The picker emits close requests through `WorkspaceClientState`, preserves row identity by `session_id`, and treats `Ctrl+X` and `Ctrl+A` as picker-local controls.

**Tech Stack:** Rust, Tokio, Crossterm, Ratatui, Serde protocol enums, existing Jcode session persistence and server connection registry.

## Global Constraints

- Closing a terminal, receiving SIGHUP, socket EOF, or ordinary client exit must only detach the client.
- Disconnect must not cancel or abort an in-progress turn.
- Only explicit `CloseSession` may persist `SessionStatus::Closed`.
- A working session, the current `here` session, a closed session, and an external session cannot be closed from the picker.
- `Ctrl+X` confirmation is scoped to one `session_id` and expires after two seconds.
- Global chat-input `Ctrl+X` keeps its existing cut behavior.
- `Ctrl+A` toggles Active (`working + ready`) and All recent.
- No visible `detached` state is introduced.
- Existing historical SIGHUP crashes are not rewritten.
- Do not restart the user's active server automatically.
- Do not create implementation commits unless the user separately requests commits.
- Keep the missing `Diff/Files` indicator outside this plan.

---

## File Map

- `crates/jcode-protocol/src/wire.rs`: wire request and response variants.
- `crates/jcode-protocol/src/lib.rs`: request ID matching and protocol exports.
- `crates/jcode-protocol/src/protocol_tests/misc_events.rs`: JSON round-trip tests.
- `crates/jcode-app-core/src/server/client_api.rs`: typed client method for close requests.
- `crates/jcode-app-core/src/server/client_lifecycle.rs`: request dispatch and nonblocking resume-source metadata lookup.
- `crates/jcode-app-core/src/server/client_disconnect_cleanup.rs`: detach-only connection cleanup.
- `crates/jcode-app-core/src/server/client_disconnect_grace_tests.rs`: disconnect behavior regression tests.
- `crates/jcode-app-core/src/server/client_lifecycle_tests.rs`: nonblocking resume and close dispatch tests.
- `crates/jcode-tui-session-picker/src/lib.rs`: picker filter semantics shared by TUI code.
- `crates/jcode-tui/src/tui/session_picker.rs`: picker-local confirmation, filtering, key handling, and result types.
- `crates/jcode-tui/src/tui/session_picker/render.rs`: Active/All label and close feedback rendering.
- `crates/jcode-tui/src/tui/session_picker_tests.rs`: picker state and key regression tests.
- `crates/jcode-tui/src/tui/workspace_client.rs`: pending close request queue alongside pending resume.
- `crates/jcode-tui/src/tui/app/inline_interactive.rs`: translate picker results into queued server operations.
- `crates/jcode-tui/src/tui/app/remote.rs`: send queued close requests and refresh picker state.
- `crates/jcode-tui/src/tui/app/remote/server_events.rs`: handle close success and refusal feedback.
- `crates/jcode-tui/src/tui/app/tests/commands_accounts_01/part_01.rs`: app integration tests.

## Task 1: Restore the Correct Baseline and Keep Nonblocking Resume

**Files:**
- Modify: `crates/jcode-tui/src/tui/app/input.rs`
- Modify: `crates/jcode-tui/src/tui/app/remote/key_handling.rs`
- Modify: `crates/jcode-tui/src/tui/app/tests.rs`
- Delete: `crates/jcode-tui/src/tui/app/tests/session_close_ctrl_x.rs`
- Modify: `crates/jcode-app-core/src/server/client_lifecycle.rs`
- Modify: `crates/jcode-app-core/src/server/client_lifecycle_tests.rs`

**Interfaces:**
- Produces: `fn resume_source_working_dir(agent: &Arc<Mutex<Agent>>, fallback: Option<String>) -> Option<String>`.
- Preserves: global `Ctrl+X` calls `cut_input_line_to_clipboard` exactly as before this session.

- [ ] **Step 1: Remove the incorrect global close prototype**

Restore both local and remote `KeyCode::Char('x')` branches to unconditional input cutting. Remove the `session_close_ctrl_x.rs` include and file.

- [ ] **Step 2: Run existing Ctrl+X tests**

Run:

```bash
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p jcode-tui 'test_ctrl_x_' --no-fail-fast
```

Expected: existing cut and clipboard-failure tests pass.

- [ ] **Step 3: Keep the failing-first nonblocking resume regression**

The test must hold the source `Agent` mutex, call `resume_source_working_dir`, and complete under `Duration::from_millis(100)` using member/session working-directory metadata as fallback.

- [ ] **Step 4: Verify the nonblocking resume test and related resume suite**

Run:

```bash
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p jcode-app-core resume_source_working_dir_does_not_wait_for_busy_agent_lock
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p jcode-app-core server::client_session::tests::resume --no-fail-fast
```

Expected: all selected tests pass without waiting for the held source lock.

## Task 2: Add the Explicit Close Protocol

**Files:**
- Modify: `crates/jcode-protocol/src/wire.rs`
- Modify: `crates/jcode-protocol/src/lib.rs`
- Modify: `crates/jcode-protocol/src/protocol_tests/misc_events.rs`
- Modify: `crates/jcode-app-core/src/server/client_api.rs`
- Modify: `crates/jcode-app-core/src/protocol_tests/misc_events.rs`.

**Interfaces:**
- Produces request:

```rust
Request::CloseSession {
    id: u64,
    session_id: String,
}
```

- Produces response:

```rust
ServerEvent::SessionClosed {
    id: u64,
    session_id: String,
}
```

- Produces client method:

```rust
pub async fn close_session(&mut self, session_id: &str) -> Result<u64>
```

- [ ] **Step 1: Write protocol round-trip tests**

Add one request and one response serialization test that assert `id` and `session_id` survive encode/decode. Extend `Request::id()` coverage for `CloseSession`.

- [ ] **Step 2: Run the tests and observe the expected compile failure**

```bash
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p jcode-protocol close_session --no-fail-fast
```

Expected before implementation: missing `CloseSession` and `SessionClosed` variants.

- [ ] **Step 3: Add the wire variants and ID routing**

Add the exact variants above with the same Serde tagging conventions as `ResumeSession`. Add `Request::CloseSession { id, .. } => *id` to `Request::id()`.

- [ ] **Step 4: Add `Client::close_session`**

Allocate the next request ID, write `Request::CloseSession`, and return the ID using the same writer/error pattern as `resume_session` and `clear`.

- [ ] **Step 5: Verify protocol and client API compilation**

```bash
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p jcode-protocol close_session --no-fail-fast
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo check -p jcode-app-core
```

Expected: protocol tests pass and app-core compiles.

## Task 3: Implement Server-Authoritative Manual Close

**Files:**
- Create: `crates/jcode-app-core/src/server/client_session_close.rs`
- Modify: `crates/jcode-app-core/src/server.rs`
- Modify: `crates/jcode-app-core/src/server/client_lifecycle.rs`
- Create: `crates/jcode-app-core/src/server/client_session_close_tests.rs`

**Interfaces:**
- Produces:

```rust
pub(super) async fn handle_close_session(
    id: u64,
    target_session_id: &str,
    requester_session_id: &str,
    sessions: &SessionAgents,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) -> Result<()>;
```

- Success sends `ServerEvent::SessionClosed`.
- Refusals send `ServerEvent::Error` with stable messages: `Cannot close the current session`, `Session is working`, or `Unknown session`.

- [ ] **Step 1: Write failing server tests**

Cover:

```text
close idle persisted session -> Closed + transcript retained + SessionClosed event
close busy session -> Error("Session is working") + no cancellation + remains Active
close requester current session -> Error("Cannot close the current session")
close already closed session -> idempotent SessionClosed
close unknown session -> Error("Unknown session")
```

Use a held agent mutex and/or authoritative `ClientConnectionInfo::is_processing` to model working state. Assert persisted status by loading the session file after the handler returns.

- [ ] **Step 2: Run tests and observe failure**

```bash
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p jcode-app-core client_session_close --no-fail-fast
```

Expected: handler/module does not exist.

- [ ] **Step 3: Implement validation without waiting on a busy agent**

Check in this order:

1. reject `target_session_id == requester_session_id`;
2. find authoritative target in the live map or persisted session store;
3. reject if any target connection reports `is_processing`, or if the target agent mutex cannot be acquired immediately;
4. for an idle live target, acquire the mutex, call `session.mark_closed()`, save, and release it;
5. remove live runtime/member bookkeeping only after persistence succeeds;
6. return `SessionClosed`.

Do not set shutdown/cancel signals in this handler.

- [ ] **Step 4: Dispatch `Request::CloseSession`**

Add a `client_lifecycle.rs` match arm that invokes `handle_close_session` without changing the requester's attached session.

- [ ] **Step 5: Verify close behavior**

Run the focused tests, then:

```bash
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p jcode-app-core client_session --no-fail-fast
```

Expected: focused close tests and existing session tests pass.

## Task 4: Make All Client Disconnects Detach-Only

**Files:**
- Modify: `crates/jcode-app-core/src/server/client_disconnect_cleanup.rs`
- Modify: `crates/jcode-app-core/src/server/client_disconnect_grace_tests.rs`
- Modify: `crates/jcode-app-core/src/server/client_lifecycle.rs` only if the cleanup signature changes.

**Interfaces:**
- Preserves `detach_client_attachment(...)` as the connection-owned cleanup primitive.
- Changes `cleanup_client_connection(...)` so ordinary transport loss never persists `Closed`/`Crashed`, aborts a turn, or removes the session runtime.

- [ ] **Step 1: Replace old disposition tests with failing detach-contract tests**

Add tests asserting:

```text
idle disconnect -> session remains live/ready and persisted status is not Closed/Crashed
processing disconnect -> JoinHandle is not aborted; turn completion persists; session remains resumable
SIGHUP-equivalent disconnect -> same detach-only result
replacement attachment -> old cleanup cannot remove successor ownership
```

Remove or rewrite tests named `idle_disconnect_is_closed` and `running_disconnect_without_reload_is_crash`, because those expectations contradict the approved contract.

- [ ] **Step 2: Run disconnect tests and confirm current failures**

```bash
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p jcode-app-core disconnect --no-fail-fast
```

Expected before implementation: idle becomes Closed and processing disconnect aborts/crashes.

- [ ] **Step 3: Reduce cleanup to attachment cleanup**

Implement these rules:

- call `detach_client_attachment`;
- abort only the per-client event-forwarding handle;
- do not call `processing_task.take().abort()`;
- dropping a Tokio `JoinHandle` must detach it so the server-owned turn continues;
- do not persist a disconnect disposition;
- do not remove the session agent, swarm member, shutdown signal, soft-interrupt queue, file-touch state, or channels solely because the client vanished;
- preserve successor-attachment guards.

If resource cleanup is genuinely session-scoped, move it to explicit close handling from Task 3 rather than leaving it in disconnect cleanup.

- [ ] **Step 4: Verify detach behavior and turn continuation**

Run:

```bash
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p jcode-app-core disconnect --no-fail-fast
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p jcode-app-core client_disconnect_grace --no-fail-fast
```

Expected: detach tests pass, including a real awaited completion after client loss.

## Task 5: Add Picker `Ctrl+A` Active/All Recent Toggle

**Files:**
- Modify: `crates/jcode-tui-session-picker/src/lib.rs`
- Modify: `crates/jcode-tui/src/tui/session_picker.rs`
- Modify: `crates/jcode-tui/src/tui/session_picker/render.rs`
- Modify: `crates/jcode-tui/src/tui/session_picker_tests.rs`

**Interfaces:**
- Produces picker method:

```rust
fn toggle_active_all_filter(&mut self)
```

- Active membership is exactly live `Working` or `Ready` presence.
- All recent uses the already loaded bounded dataset and includes closed, crashed, and external rows.

- [ ] **Step 1: Write failing filter and selection tests**

Create a mixed picker fixture with working, ready, closed, crashed, current, and external sessions. Assert:

1. initial Active shows only working and ready;
2. `Ctrl+A` shows every fixture row allowed by existing test/debug visibility;
3. second `Ctrl+A` returns to Active;
4. selected `session_id` is preserved when still visible;
5. hiding the selection chooses a valid nearest row;
6. toggling back restores the remembered selection when available.

- [ ] **Step 2: Run picker tests and observe failure**

```bash
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p jcode-tui session_picker_ctrl_a --no-fail-fast
```

Expected: `Ctrl+A` is not handled as the required toggle.

- [ ] **Step 3: Implement the dedicated toggle**

In `handle_overlay_key`, match `KeyCode::Char('a')` with `KeyModifiers::CONTROL` before generic filter/search handling. Toggle only Active and All, store the selected `session_id` before rebuilding, rebuild items, then restore selection by ID or nearest row.

- [ ] **Step 4: Update rendering/help copy**

Render `active` or `all` in the picker header and include a concise `Ctrl+A active/all` hint. Do not remove the existing `s/S` broader filter-cycle behavior unless tests prove it conflicts.

- [ ] **Step 5: Verify picker filters**

Run focused tests plus:

```bash
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p jcode-tui session_picker --no-fail-fast
```

Expected: all picker tests pass.

## Task 6: Add Picker-Scoped `Ctrl+X` Close Flow

**Files:**
- Modify: `crates/jcode-tui/src/tui/session_picker.rs`
- Modify: `crates/jcode-tui/src/tui/session_picker/render.rs`
- Modify: `crates/jcode-tui/src/tui/session_picker_tests.rs`
- Modify: `crates/jcode-tui/src/tui/workspace_client.rs`
- Modify: `crates/jcode-tui/src/tui/app/inline_interactive.rs`
- Modify: `crates/jcode-tui/src/tui/app/remote.rs`
- Modify: `crates/jcode-tui/src/tui/app/remote/server_events.rs`
- Modify: `crates/jcode-tui/src/tui/app/tests/commands_accounts_01/part_01.rs`

**Interfaces:**
- Add picker result:

```rust
PickerResult::CloseSession { session_id: String }
```

- Add picker state:

```rust
pending_close: Option<(String, Instant)>
```

- Add workspace client queue:

```rust
pending_close_session: Option<String>
pub(crate) fn queue_close_session(&mut self, session_id: String)
pub(crate) fn take_pending_close_session(&mut self) -> Option<String>
```

- [ ] **Step 1: Write failing picker confirmation tests**

Assert:

```text
ready row first Ctrl+X -> confirmation armed, no PickerResult
same ready row second Ctrl+X within 2s -> CloseSession result
selection change -> confirmation cleared
filter toggle -> confirmation cleared
expired confirmation -> next Ctrl+X arms again
working row -> refusal, no confirmation
current/here row -> refusal
closed row -> already-closed feedback
external row -> unsupported feedback
```

- [ ] **Step 2: Implement picker-local confirmation**

Use the highlighted native Jcode session ID. Store the exact ID and `Instant`. Check current row, source, status, and live presence before arming. Clear pending close on navigation/filter/search/picker close. Add non-destructive feedback state rendered in the picker footer or status line.

- [ ] **Step 3: Queue close from App**

Handle `OverlayAction::Selected(PickerResult::CloseSession { session_id })` in `handle_session_picker_key` by calling `workspace_client.queue_close_session(session_id)`. Keep the picker open while the request is pending.

- [ ] **Step 4: Send close requests in the remote tick**

Adjacent to `take_pending_resume_session`, consume `take_pending_close_session`, call `remote.close_session`, and remember the request ID/target so the matching server response updates the correct row.

- [ ] **Step 5: Handle server success and refusal**

On `SessionClosed`, refresh/reseed the picker while retaining Active/All mode and valid selection. On matching `Error`, keep the picker open, clear pending confirmation/request state, and show the server's authoritative reason.

- [ ] **Step 6: Verify picker and app integration**

Run:

```bash
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p jcode-tui session_picker_ctrl_x --no-fail-fast
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p jcode-tui session_picker_enter_queues_current_terminal_resume_and_closes_overlay
```

Expected: close flow tests pass and Enter behavior remains intact.

## Task 7: Compatibility, Help, and End-to-End Verification

**Files:**
- Modify: `crates/jcode-tui/src/tui/app/input_help.rs`
- Modify: `crates/jcode-tui/src/tui/app/remote.rs`.
- Modify: `crates/jcode-tui/src/tui/app/remote/server_events.rs`.
- Modify: `docs/RESUME_BEHAVIOR.md`.

**Interfaces:**
- Old server behavior: picker close is disabled with an actionable upgrade/reload notice.
- Updated server behavior: `Enter`, `Ctrl+A`, and `Ctrl+X` match the specification.

- [ ] **Step 1: Add compatibility test**

Simulate a server/runtime identity that predates `CloseSession`. Assert `Ctrl+X` does not disconnect or mutate the session and shows `Update or reload the Jcode server to close sessions`.

- [ ] **Step 2: Update user-facing help and docs**

Document:

```text
Enter       resume highlighted session here
Ctrl+Enter  resume in a new terminal
Ctrl+A      toggle Active / All recent
Ctrl+X      press twice to close an idle, non-current Jcode session
```

State explicitly that terminal close only detaches and does not close/cancel the session.

- [ ] **Step 3: Run formatting**

Run `cargo fmt --all -- --check` through the repository toolchain. If the installed toolchain lacks `cargo-fmt`, do not modify the user's toolchain automatically; run `git diff --check`, preserve rustfmt-compatible layout manually, and report the missing formatter as a verification limitation.

- [ ] **Step 4: Run full relevant verification**

```bash
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p jcode-protocol --no-fail-fast
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p jcode-app-core --no-fail-fast
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo test -p jcode-tui --no-fail-fast
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" \
  cargo check -p jcode-app-core -p jcode-tui
git diff --check
```

Expected: zero failing tests attributable to the change, successful checks, and no whitespace errors. Existing unrelated flaky/platform-specific failures must be rerun individually and reported with evidence rather than hidden.

- [ ] **Step 5: Manual acceptance without touching the user's active server**

Use an isolated `JCODE_HOME` and socket:

1. start a test server and client;
2. create an idle session, close the client window/process, and verify the session remains Active and resumable;
3. start a delayed turn, disconnect the client, reconnect, and verify the completed result exists;
4. open `/sessions`, verify `Ctrl+A` toggles views;
5. verify `Ctrl+X` refuses working/current/external rows;
6. switch away from an idle session, press `Ctrl+X` twice, and verify it becomes Closed in All recent;
7. press Enter on the Closed row and verify it resumes as ready.

## Plan Self-Review

- Spec coverage: detach-only disconnect, turn continuation, explicit close, working/current/external refusal, resumable Closed sessions, nonblocking Enter, `Ctrl+A`, row-scoped `Ctrl+X`, compatibility, documentation, and isolated acceptance are each mapped to a task.
- Scope: `Diff/Files` remains excluded.
- Type consistency: `CloseSession`, `SessionClosed`, `queue_close_session`, and `take_pending_close_session` use `String` session IDs throughout.
- Safety: no step restarts or kills the user's active server; acceptance uses an isolated home/socket.
- Prototype cleanup: Task 1 removes the incorrectly scoped global `Ctrl+X` work before feature implementation.
