# Diff list and generic side-panel navigation

Date: 2026-09-09

## Frozen behavior
- The empty Chat composer enters the Diff pane with a changed-file list focused. Up/Down changes the selected file, Enter focuses that file's diff, and Up/Down then scrolls content. Esc returns to the list. List Left returns to Chat and Right enters Files. This must work for local, connected, and disconnected sessions, including empty and narrow layouts, snapshot refreshes, and nonempty composer caret preservation.
- The worktree arrow chain is only Chat <-> Diff <-> Files. Documents is removed from the worktree tab header, hit rectangles, Tab/BackTab, and arrow transitions.
- Side-panel page types, persistence, generic rendering, page cycling, Alt-M, mouse, Esc, scrolling, and tool-loaded LinkedFile/Managed/Ephemeral behavior remain generic and do not mutate the worktree tab. Only a chat-clicked repository-relative Markdown link may focus Files and the Files inspector Read view. External, SSH, URL, and outside-repository links retain fallback/refusal behavior.

## Implementation order
1. Update worktree state and input routing so Diff has explicit list/content focus and stable selected paths. Keep Files inspector state independent and guard empty/narrow cases.
2. Remove Documents from the worktree surface and arrow chain while retaining Markdown document state and generic side-panel controls. Route repository Markdown links into Files without creating/replacing side-panel pages.
3. Ensure snapshot application never auto-selects the worktree tab and generic focused side-panel pages render through the existing side renderer.
4. Replace stale Documents expectations with routed input/render tests covering the contracts and preserve Files inspector regressions.

## Validation
Use the stable toolchain path. Run scoped rustfmt (edition 2024), `git diff --check`, nonzero-count targeted test filters, and `cargo check -p jcode-tui --tests` plus targeted tests. Do not deploy, reload, alter channels, or touch the open Orangutan client.
