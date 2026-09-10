# Historical branch recovery (2026-09-11)

## Canonical baseline

The recovery branch was created from `custom/ui-stable` at
`717bf8ac7225da56176408c4670c7789d005b7fb`. The canonical branch itself was not
moved.

Local rollback refs created before integration:

- `backup/recovery-custom-717bf8ac7`
- `backup/recovery-local-master-91e87bc97`
- `backup/recovery-fork-master-78287e0e2`

These refs are local only until an explicitly authorized push.

## Integrated history

The five commits published on the old `fork/master` line were merged as one
conflict-free history unit:

- `19fa795ab` show session worktree Git status
- `c52b2090b` optional RTK bash backend
- `4ec5f1090` adopt an agent worktree after file edits
- `51d4874ce` RTK dispatch coverage
- `78287e0e2` write-tool worktree adoption coverage

Three later fixes were carried over by intent:

- `0becf0d2f` remote worktree propagation
- `1c0afad61` unchanged completion-gate retry suppression
- `91e87bc97` session-scoped completion-gate fingerprints

`0becf0d2f` conflicted only in `jcode-protocol::ServerEvent`. The resolution kept
the current session preview and clean-session creation events and added
`WorkingDirChanged` as a separate variant. No current protocol capability was
removed.

## Superseded history

The following patches were intentionally not applied:

- `7d44e6b82` navigate diff pane with arrow keys
- `88922c7b0` align panel navigation and multiline input
- `3bbdf0ba7` keep Left in a non-empty chat editor

Their requirements are represented by the newer worktree rail, Files inspector,
Active Sessions gate, and composer implementation. Applying the old patches
would replace newer focus states and would make Active Sessions open more
aggressively than the current empty-input, cursor-boundary, and configuration
contract.

Representative current acceptance checks include:

- `left_arrow_on_empty_input_opens_active_sessions_by_default`
- `routed_worktree_arrows_retain_final_tab_content_after_focus_exit`
- `test_shift_enter_inserts_newline`
- `test_alt_enter_inserts_newline`
- `test_ctrl_j_inserts_newline_in_non_empty_draft`

## Validation and promotion boundary

Focused recovery checks cover RTK rewriting and its destructive-command gate,
protocol round-tripping, local and remote worktree adoption, completion-gate
fingerprints, and the superseding navigation/newline contracts.

No remote was renamed, no recovery ref was pushed, no public branch was moved,
and no binary or channel was replaced as part of this recovery. Promotion and
runtime or visual acceptance remain separate, explicitly authorized steps under
`docs/LOCAL_DEVELOPMENT.md`.
