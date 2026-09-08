# `/resume` behavior

`/resume`, `/session`, and `/sessions` open the interactive session picker. They are local UI commands and must not be sent as chat prompts.

## Default action

- `Enter` resumes the highlighted session in the current terminal.
- `Ctrl+Enter` opens the highlighted session in a new terminal.
- `Esc`, `q`, or `Ctrl+C` closes the picker without changing sessions.

The default is controlled by:

```toml
[keybindings]
session_picker_enter = "current-terminal"
```

Set `session_picker_enter = "new-terminal"` to swap `Enter` and `Ctrl+Enter`.

## Current-terminal resume

When resuming in the current terminal:

- Jcode sessions switch the current workspace/client to that session.
- Importable external sessions are first converted to a Jcode session, then resumed in place.
- If multiple sessions are selected, only the first selected target is resumed in place and the UI tells the user this.
- The picker closes after queueing the in-place resume.

## New-terminal resume

When opening in a new terminal:

- Each selected target opens in its own terminal when possible.
- If terminal launch is unavailable, the UI prints manual `jcode --resume <id>` commands.
- The picker remains open after launching and clears the current multi-selection so more sessions can be opened.

## Saved sessions

`/save [label]` bookmarks the current session so it appears in the saved section of the picker. `/unsave` removes that bookmark.

## Lifecycle controls

- The picker opens in **Active** view, which shows live ready and working sessions. Press `Ctrl+A` to toggle Active and All recent sessions.
- Press `Ctrl+X` twice within two seconds to close an idle, non-current Jcode session. Working, current, closed, and external sessions cannot be closed from the picker.
- Closing a terminal or losing its connection only detaches that client. It does not close the session or cancel a server-owned turn.
- Closing a session retains its transcript. A closed session can be resumed later.

## Starting a clean session from Active Sessions

When the main chat composer is empty, `Left` opens **Active Sessions**. This
manager is also a clean-session composer: ordinary typing edits a new prompt,
without changing the ordinary `/sessions` or `/resume` picker shortcuts.

- With a non-empty draft, `Enter` opens the server-side working-directory
  chooser. Choose **Here** (the source session directory), the highlighted
  session directory, a recent directory, server Home, or enter a manual path.
  Every path is interpreted and validated by the connected server, not the
  local terminal client.
- Confirming a directory creates a clean session with no inherited transcript
  or parent. `/fork` and `/split` remain the ways to create sessions that do
  inherit their source conversation.
- The created tile appears immediately to the right of the focused tile,
  becomes focused, and attaches in the current client. Its first prompt waits
  for that exact session's History response, so another session's History or a
  duplicate reconnect response cannot send it to the wrong session.
- `Ctrl+F` filters the list, `Ctrl+A` toggles Active and All, and `Ctrl+X`
  retains its double-press close behavior. With an empty draft, `Enter`
  resumes the highlighted session, `Right` returns to chat, and `Esc` closes
  the manager.

Older servers leave browsing and ordinary resume available but show an
update-required notice instead of offering clean creation. If attaching the
created session or writing its first prompt to the socket fails, the created
tile remains visible and the prompt is restored in the focused composer for a
manual retry. A successful socket write is only transport acceptance. The
server's subsequent History and turn events remain the authoritative evidence
that the prompt was persisted and processed.

## Active Sessions preview

Active Jcode sessions show a server-owned transcript tail, rather than relying
on a local session file. The daemon returns at most 20 renderable messages, so
the preview remains bounded while a session is working. The picker first checks
that the connected daemon supports this protocol. An older daemon shows an
update-required state instead of receiving an unknown request.

While a live preview is being fetched it shows `Loading preview…`. A clean
session shows `(empty session)` only after an authoritative successful response.
Transient request failures remain retryable and display a retry status, without
being interpreted as an empty transcript. Historical and imported rows retain
their existing bounded local metadata loading behavior.
