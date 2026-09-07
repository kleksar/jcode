# Collaboration policy

The canonical behavioral rules for this fork are the built-in prompts:

- `crates/jcode-base/src/prompt/system_prompt.md` defines collaboration,
  autonomy, safety, workflow scope, progress reporting, and proportionate
  verification.
- `crates/jcode-base/src/prompt/swarm_prompt.md` defines worker-pool routing,
  delegation boundaries, and worker reporting.

This document records migration and verification only. It does not duplicate
those rules.

## Migration from compatibility workflows

Compatibility and third-party skills remain available as techniques. They do
not automatically impose a lifecycle. Preserve applicable project agreements,
technical constraints, and access or security rules, but do not preserve a
previous tool's generic planning or delegation ritual merely for compatibility.

Remove obsolete local prompt and swarm overrides that conflict with the
built-in policy. This migration removed the former coordinator-only
`~/.jcode/prompt-overlay.md` and the former `~/.jcode/swarm-prompt.md` routing
override. In local configuration, it changed only `[features] auto_poke` from
`true` to `false`. Targeted memory retirement removed the legacy blanket
read-only-first and low-autonomy records, added a short Git-policy pointer, and
kept the useful records. Subject agreements, access and security constraints,
and the selected main and worker-pool model, effort, and mode are preserved.

Keep a local override only for a genuine machine- or project-specific
constraint that cannot be represented in the tracked default. The prompt
overlay loader tries the project path and then the global path, appending every
readable file without deduplicating paths. When the working directory is the
home directory, those paths can name the same canonical file, so the loader
appends it twice. Do not place a second copy of this policy in an overlay. Swarm
prompt selection instead uses the first nonempty project, then global, then
built-in file.

The built-in prompts are embedded at build time. Rebuild and install after
changing them, then start a new session. Existing sessions retain their captured
prompt. New swarm workers capture the current swarm prompt; already-running
workers retain theirs. `skill_manage reload_all` refreshes only the shared
global skill registry. Project-local skill overlays are refreshed from disk
when the effective registry is composed for the current working directory, and
remain session-scoped rather than entering that shared registry.

## Verification

After a build and install, use a fresh session outside the home directory to
exercise only these focused scenarios:

1. A discussion request performs no edit.
2. A simple explicit task proceeds without a planning ritual.
3. An ambiguous implementation request first establishes the agreement.
4. A new material contract, architecture, scope, or tradeoff fact produces a
   concrete choice before execution continues.
5. A swarm request preserves configured worker model, effort, and mode unless a
   task-specific justification changes one; each worker has a distinct claim.

Confirm the installed binary starts and the selected provider configuration is
unchanged. Record unavailable integration checks and remaining limits rather
than substituting redundant checks.

Normal swarm and `swarm-deep` runtime directives can still add their own
mode-specific behavior. Treat that interaction as a remaining verification risk
rather than silently broadening this policy change.
