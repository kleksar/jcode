# Session Name in Statusline

## Goal

Show the current session's friendly name at the far right of the stable TUI footer so users can identify the active session without opening the session picker.

## Presentation

- Render the session icon and friendly name as `🐍 snake`.
- Anchor the label to the right edge of the one-row session footer.
- Use a neutral color for the icon and a slightly brighter neutral color for the name.
- Do not add another separator after the label.

## Layout behavior

The existing directory, branch, model, context, and quota content remains left-aligned and keeps its current order. The compositor reserves space for the right-aligned session label before truncating the left side.

The session label is treated as a single unit. When left-side content exists, it is shown only if the full label plus a two-cell gap leaves at least one cell for the left side. The compositor truncates the left side into its remaining width. If that condition cannot be met, the complete session label is hidden. When no left-side content exists, the label may use the full footer width and remains right-aligned. The existing footer threshold for tiny terminals remains unchanged.

## Data flow

`draw_session_footer` obtains the name through `TuiState::session_display_name`, derives the icon through `crate::id::session_icon`, and passes the resulting spans to the footer compositor. This uses the same local and remote session identity path already used by the header and frame metrics.

## Scope

- Change only the stable session footer and its focused tests.
- Do not change the header, terminal title, right-side fact stack, session naming, or footer height.
- Do not alter the order or styling of existing footer facts and quota information except where width reservation requires existing truncation behavior.

## Verification

Add tests proving:

1. A normal-width footer renders `icon + name` at the right edge.
2. Existing facts and quota retain their current order.
3. The left side is truncated before the right label is removed when both cannot fit.
4. The complete session label is hidden on widths where it cannot fit safely.
5. A full TestBackend frame exposes the current local session name in the footer.
