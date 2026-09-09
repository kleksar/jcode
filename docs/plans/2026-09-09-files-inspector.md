# Files lower inspector: scoped implementation

## Approved behavior

The Files tab always renders a project tree above a lower inspector. Selecting any supported project file previews it in that lower region, including Markdown. Enter or Right transfers focus to the inspector only, never to the top-level Documents tab or a `SidePanelPage`. Left and Escape return inspector focus to the tree; Escape from the tree returns focus to Chat while Files remains displayed.

The inspector presents only meaningful modes. Code and Python present `Source | Changes` with Source selected initially. Markdown presents `Read | Source | Changes` with Read selected initially. Source preserves the exact cached preview text and Changes is the file's actual cached worktree diff. Per-file and per-mode vertical offsets are retained across selection and mode changes. Existing background/cached preview and diff collection remains the sole data source: input dispatch performs no filesystem reads.

## State and rendering boundary

Introduce a Files-inspector state keyed by selected path with capability-derived mode availability, current mode, focused state, and per-mode offsets. Expose the selected inspector state through `TuiState`; keep tree geometry and input routing in the existing worktree pane module. Extend `WorktreePaneLayout` with lower-inspector tab rectangles and mode-aware scroll metadata so mouse input targets only rendered controls and body. The renderer owns the stacked layout and title style receives whether the right pane is keyboard-focused. Documents rendering stays independent, preserving top-level Documents and repository/external Markdown link behavior outside Files activation.

## Regression scope

Regression tests cover Markdown/Python modes and defaults; Enter does not switch tabs or create a side page; source and actual diff content; switching selection and mode restores offsets; Left/Escape traversal; focused versus unfocused TestBackend tab styling; external Documents behavior; narrow layouts without invisible focus; inspector mouse wheel isolation; and connected/disconnected input routes. Validation is scoped formatting, focused package tests, and an isolated non-promoting package check only.
