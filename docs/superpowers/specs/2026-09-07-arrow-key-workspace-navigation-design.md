# Arrow-Key Workspace Navigation Design

## Goal

Make the main Jcode workspace navigable with unmodified Left and Right arrow keys when the chat composer is empty. The navigation order is:

`Sessions ↔ Chat ↔ Diff ↔ Files`

The right-hand workspace remains visible by default and shows useful content instead of an empty diff.

## User-visible behavior

### Default right-hand view

- The right-hand workspace is visible by default.
- When the current session has code changes, its default tab is `Diff`.
- When the current session has no code changes, its default tab is `Files`.
- The default is selected when a session is opened or changed and when the user enters the right-hand workspace from Chat.
- A newly arriving code change does not steal keyboard focus or switch a tab that the user is actively using. The next transition from Chat into the right-hand workspace selects `Diff`.

### Horizontal navigation

Unmodified arrow keys navigate workspace regions only while the chat composer is empty:

| Current region | Left | Right |
| --- | --- | --- |
| Sessions | No-op | Close Sessions and return to the same Chat |
| Chat | Open Sessions | Focus the default right-hand tab |
| Diff | Focus Chat | Focus Files |
| Files | Focus Diff | No-op |

Opening Sessions through this gesture must not change the active session. Pressing Right from Sessions closes the picker and restores Chat for the same session.

### Composer compatibility

- If the composer contains text, Left and Right retain their existing cursor movement behavior.
- At the start or end of non-empty text, arrows do not escape the composer.
- Modified arrow shortcuts keep their existing meanings.
- Other overlays and modal interactions retain precedence over workspace navigation.

## Architecture

Add one focused workspace-navigation helper that maps a direction and the current region to the next action. It reuses the existing session-picker opening/closing APIs, side-pane focus APIs, and right-hand tab-selection state rather than introducing a second focus system.

The helper is called from the normal unmodified Left/Right input path after modal and overlay handlers have had a chance to consume the event. The composer delegates to it only when the input is empty. Diff and Files handlers delegate to the same helper for horizontal region transitions.

A small function determines the right-hand default tab from whether the current session has code changes. This keeps the default-selection policy independent from key dispatch and makes it directly testable.

## State transitions

- `Chat + Left`: open the active Sessions view and remember that it was entered from Chat.
- `Sessions + Right`: dismiss the Sessions view without selecting a different session.
- `Chat + Right`: ensure the right-hand workspace is visible, select `Diff` when changes exist or `Files` otherwise, and focus it.
- `Diff + Left`: clear right-hand focus and restore Chat focus.
- `Diff + Right`: select and focus Files.
- `Files + Left`: select and focus Diff.
- Boundary transitions are consumed as no-ops so they do not trigger unrelated scrolling or cursor behavior.

## Error and edge handling

- If the Sessions view cannot be opened in the current runtime mode, the event is consumed without corrupting focus state.
- If Diff or Files is temporarily unavailable, navigation chooses the nearest available right-hand destination. Chat remains the fallback.
- Session changes recompute the default right-hand tab from that session's own change state.
- Empty means the underlying composer buffer has zero characters. Whitespace remains editable content and therefore keeps normal cursor semantics.

## Testing

Add focused unit tests for:

1. Empty Chat transitions left to Sessions and right to the appropriate right-hand tab.
2. Sessions Right closes the picker and preserves the active session.
3. Diff and Files transition in both directions according to the table.
4. Boundary arrows are safe no-ops.
5. Non-empty and whitespace-only composer input retains cursor behavior.
6. Modified arrows bypass workspace navigation.
7. The default right-hand tab is Diff with code changes and Files without them.
8. A new diff does not steal an actively focused tab, but is selected on the next Chat-to-right transition.
9. Local and remote key dispatch share the same transition behavior where those modes expose the same workspace regions.

## Non-goals

- Changing vertical scrolling keys.
- Reordering or redesigning the Sessions, Diff, or Files interfaces.
- Adding configurable navigation order or new keybindings.
- Automatically switching an actively used Files tab when new code changes arrive.
