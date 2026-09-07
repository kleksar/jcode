# Explicit Session Lifecycle and Picker Controls

**Date:** 2026-09-07
**Status:** Approved design, pending user review of this document

## Goal

Make Jcode session ownership explicit and predictable:

- closing a terminal window only detaches that UI client;
- a session is never closed or cancelled merely because its window disappears;
- users close sessions manually from `/sessions`;
- picker navigation remains responsive while another session is working;
- active and historical sessions are easy to switch between.

The separate regression where the top-level `Diff/Files` indicator disappears is intentionally out of scope. It will receive its own diagnosis and specification after this session-lifecycle work.

## Terminology

- **Session:** Server-owned conversation and runtime state.
- **Client:** A terminal/TUI process attached to a session.
- **Detach:** Remove a client attachment without closing the session or cancelling its work.
- **Close:** An explicit user action that persists the session as `Closed` and releases its live runtime resources.
- **Active picker view:** Sessions whose effective state is `working` or `ready`.
- **All recent picker view:** Recent `working`, `ready`, `closed`, and `crashed` Jcode sessions plus importable external sessions within the picker's existing bounded recent-session window.

No user-visible `detached` status is introduced in this scope. An idle session with no attached terminal remains `ready`; a generating session remains `working`.

## Lifecycle Contract

### Terminal or client disconnect

A terminal close, SIGHUP, socket EOF, or ordinary client process exit is a detach event, not a session close.

The server must:

1. unregister the disconnected client attachment;
2. retain the server-owned session;
3. preserve `working` or `ready` as appropriate;
4. keep the session visible in the `/sessions` Active view;
5. allow a later client to resume it;
6. continue an in-progress turn without aborting it solely because the client disconnected.

A disconnect may still be reported as a transport/client event in logs, but it must not persist the session as `Closed` or `Crashed` unless an independent, genuine server/runtime failure occurred.

### Explicit close

Only an explicit picker action may close a session. The client sends a new server request conceptually equivalent to:

```text
CloseSession { request_id, session_id }
```

The server is authoritative. It rechecks the target state at request time rather than trusting a possibly stale picker row.

A close succeeds only when the target is `ready` or otherwise idle. On success, the server:

1. persists `SessionStatus::Closed`;
2. releases live runtime resources and attachment bookkeeping for that session;
3. retains the transcript and metadata;
4. returns a success event that lets the picker refresh the row in place.

A `working` session cannot be manually closed. The request returns a specific refusal and does not cancel, interrupt, queue a close, or change session state.

### Reopening a closed session

Pressing `Enter` on a `Closed` session resumes it through the existing resume path. It becomes a live `ready` session after successful attachment. Historical data remains intact.

## `/sessions` Interaction Design

### Enter

`Enter` resumes the highlighted session in the current terminal, as already documented.

The resume path must not wait for the source session's `Agent` mutex merely to obtain its working directory. If the source agent is busy, the server uses already available session/member metadata as a nonblocking fallback. Switching away from a working session must therefore remain responsive and must not cancel that session.

### Ctrl+X

`Ctrl+X` is scoped to the `/sessions` picker. It does not become a global chat-input shortcut.

For a highlighted `ready` session:

1. the first `Ctrl+X` arms confirmation for that exact `session_id` and shows a close warning;
2. a second `Ctrl+X` within two seconds sends `CloseSession`;
3. changing the highlighted row, changing the filter, closing the picker, or exceeding the timeout clears confirmation;
4. after success, the picker refreshes while preserving stable selection where possible.

For a highlighted `working` session, `Ctrl+X` immediately displays a refusal such as `Session is working` and does not arm confirmation.

For the current session marked `here`, `Ctrl+X` is refused. The user must first switch the client to another session, then close the previous session from `/sessions`. This avoids terminating the picker client or creating an unwanted replacement session.

For a highlighted importable external session, `Ctrl+X` is a no-op with explanatory feedback because Jcode does not own that external runtime or transcript lifecycle.

For a highlighted `Closed` session, `Ctrl+X` is a no-op with explanatory feedback because the session is already closed.

Existing global `Ctrl+X` behavior outside the picker remains unchanged. In particular, cutting a non-empty input line must continue to work as before.

### Ctrl+A

Within `/sessions`, `Ctrl+A` toggles between:

- **Active:** only `working` and `ready` sessions;
- **All recent:** recent `working`, `ready`, `closed`, and `crashed` Jcode sessions plus importable external sessions from the existing bounded recent-session dataset.

A second press returns to Active. The initial `/sessions` view is Active.

The picker tracks selection by `session_id`, not only by row index. When toggling filters:

1. if the selected session remains visible, it stays selected;
2. otherwise the picker selects the nearest sensible visible row;
3. toggling back restores the prior session selection when it is still available.

The filter applies consistently across Jcode and importable external-session groups, subject to the state information available for each source. It does not remove the existing search feature or the current bounded loading policy.

## Server and Client Data Flow

### Close flow

1. Picker receives the second confirmed `Ctrl+X`.
2. Client sends `CloseSession` for the confirmed `session_id`.
3. Server resolves the live target and checks authoritative processing state.
4. If working, server returns a typed error/refusal.
5. If idle, server persists `Closed`, removes live runtime ownership, and returns success.
6. Picker updates or reloads the affected row without losing unrelated filter/search state.

### Disconnect flow

1. Client transport disappears.
2. Server removes only connection-owned state and event routing for that client.
3. A running turn retains server-side ownership and continues.
4. On turn completion, session becomes `ready` and remains resumable.
5. An idle session remains `ready` and resumable immediately.

Connection cleanup must not unconditionally abort a processing task. Any task lifetime currently owned by the connection handler must be transferred or retained by the session runtime before transport cleanup completes.

## Error Handling

- **Close races with new work:** Server rejects the close as working. Picker retains the row and displays the refusal.
- **Session already closed:** Server returns an idempotent already-closed result or a clear nonfatal response. No history is deleted.
- **Unknown session:** Picker shows an error and refreshes its session data.
- **Client disconnects while close is in flight:** Server completes the explicit request if it received it; otherwise ordinary disconnect semantics apply and the session remains open.
- **Resume target is working:** Attachment may proceed according to existing multi-attachment rules, but leaving the source session must never block on its agent mutex.
- **Stale client/server version:** Protocol capability handling must produce a clear unsupported-request error rather than silently interpreting disconnect as close.

## Testing Strategy

Regression coverage must include:

1. `Enter` switches away from a source agent whose mutex is held, within a short timeout.
2. Disconnecting an idle client leaves the session `ready`, persisted/resumable, and visible in Active.
3. Disconnecting during a turn does not abort the turn; completion persists and the session becomes `ready`.
4. Explicit `CloseSession` closes an idle session and retains its transcript.
5. Explicit close of a working session is rejected without cancellation or state mutation.
6. Reopening a closed session returns it to a live `ready` state.
7. First `Ctrl+X` arms row-scoped confirmation; second within two seconds closes.
8. Moving selection or timing out clears `Ctrl+X` confirmation.
9. `Ctrl+X` on working, current (`here`), closed, and external rows gives correct feedback without sending a destructive request.
10. Global chat-input `Ctrl+X` keeps its pre-existing cut behavior.
11. `Ctrl+A` toggles Active and All recent in both directions, with crashed and external sessions present only in All recent unless they expose an active state.
12. Filter toggling preserves selection by `session_id` when possible.
13. Session picker tests cover mixed `working`, `ready`, `closed`, `crashed`, external, grouped, and current (`here`) rows.

Focused tests should be followed by the relevant `jcode-app-core`, protocol, and `jcode-tui` suites plus formatting and diff checks.

## Migration and Compatibility

- Add the explicit close request and response to the protocol in a backward-compatible manner where possible.
- A client connected to a server without close capability must disable picker close and show an upgrade/reload message.
- Existing persisted `Closed` and `Crashed` sessions remain readable.
- Existing sessions incorrectly marked `Crashed` due solely to historical SIGHUP behavior are not automatically rewritten in this change. They remain resumable through the current restore/resume paths.
- Activating the fix requires compatible updated client and server processes. The implementation must not automatically restart the user's currently active server or sessions.

## Implementation Boundary

This specification includes:

- nonblocking current-terminal resume;
- detach-only terminal/client disconnect semantics;
- preservation of in-progress work after UI disconnect;
- explicit server-authoritative session close;
- picker-scoped double-confirmation `Ctrl+X`;
- picker `Ctrl+A` Active/All recent toggle;
- regression tests and compatibility feedback.

It excludes:

- deletion or garbage collection of closed transcripts;
- a new visible `detached` state;
- queued close-after-turn behavior;
- forced cancellation and close of working sessions;
- closing the current session marked `here` from its own picker client;
- closing or deleting external-provider sessions owned outside Jcode;
- multi-session workspace redesign;
- the missing `Diff/Files` indicator bug.

## Pre-existing Prototype Note

Before the requirements were clarified, an uncommitted prototype incorrectly made `Ctrl+X` close the current chat session globally. Implementation must remove that prototype and retain only picker-scoped close behavior. The nonblocking resume prototype may be retained only after it is reconciled with this specification and its regression tests pass.
