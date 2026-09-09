# Repository Guidelines

## Development Workflow

- **Stay on your own branch** - Do not take, cherry-pick, merge, or copy code from other
  people's or other agents' branches unless the source branch belongs to a repository
  maintainer and the user explicitly asks you to integrate it. Only work from your branch
  and its base (e.g. `main`) otherwise. Never integrate branches owned by non-maintainers
  or other agents yourself; tell the user and let them decide how to proceed.

## Install Notes
- `~/.local/bin/jcode` is the launcher symlink used from `PATH`.
- `~/.jcode/builds/current/jcode` is the active local/source-build channel; self-dev builds and `scripts/install_release.sh` point the launcher here.
- `~/.jcode/builds/stable/jcode` is the stable release channel; `scripts/install.sh` installs this and points the launcher here.
- `~/.jcode/builds/versions/<version>/jcode` stores immutable binaries.
- `~/.jcode/builds/canary/jcode` still exists for canary/testing flows, but it is not the primary self-dev install path.
- On Windows, the equivalents are `%LOCALAPPDATA%\\jcode\\bin\\jcode.exe` for the launcher, `%LOCALAPPDATA%\\jcode\\builds\\stable\\jcode.exe` for stable, and `%LOCALAPPDATA%\\jcode\\builds\\versions\\<version>\\jcode.exe` for immutable installs; `scripts/install.ps1` currently installs the stable channel.
- Ensure `~/.local/bin` is **before** `~/.cargo/bin` in `PATH`.

## Local deployment and runtime verification

### Build storage and end-of-work cleanup

- Finish code changes and required tests before one final application build.
  Reuse the existing target for the active compilation lane instead of creating
  duplicate target trees. If isolation is necessary, identify its owner and
  cleanup scope before creating another target.
- Before closing build-related work, review session-created artifacts and
  report storage reclaimed and storage deferred, with reasons. This reminder
  is not authorization for blanket or automatic deletion.
- Delete only explicitly authorized, exact paths after resolving symlinks and
  refreshing loaded-executable, open-file, working-directory, channel, and
  active-build references. Skip any used or ambiguous artifact. Never use broad
  `cargo clean` while a compilation lane is active.
- Preserve the current incremental target and final binary, every loaded or
  channel-pinned binary, rollback and pending candidates, source/Git data, and
  sessions, transcripts, memory, auth, and configuration. Keep concise manifests,
  build/audit logs, source snapshots, and rollback journals when removing an
  explicitly superseded binary. A scratch directory is not inherently disposable.
- Record exact deleted paths and allocated sizes, then measure filesystem free
  space before and after. Deduplicate path aliases and hardlinks. Report observed
  recovery separately from nominal allocation because APFS clones and snapshots
  can retain shared extents. Cleanup never implies build, activation, or restart
  approval. Artifact replacement still requires the provenance gate below.

Before replacing any local artifact, follow the mandatory provenance and
rollback gate in [`docs/LOCAL_DEVELOPMENT.md`](docs/LOCAL_DEVELOPMENT.md).
Identify the loaded client and daemon independently: checkout `HEAD`, channel
symlinks, and daemon version do not establish the client baseline. Do not deploy
when provenance or preservation of local customizations is unresolved.

`cargo build` alone proves neither runtime behavior nor the TUI being displayed.
An executable swap or symlink change does not replace an already-running client;
verify the loaded client binary after an explicitly approved switch. Isolated
socket builds are permitted for testing and must not implicitly promote or reload
the shared client or daemon.

### Recovered custom UI baseline

The user-approved custom baseline was recovered from user-owned GitHub ref
`archive/combined-ui-lifecycle` at exact commit
`e4d87f2151b773141801618e1078fe2bf69102aa`, cloned at
`/Users/diplomat/jcode-custom` on `jcode/session-manager-archive-integration`.
It is an integration baseline only. The migration has not yet received build,
visual, deployment, or runtime acceptance certification. Preserve the factual
2026-09-08 incident record in `docs/LOCAL_DEVELOPMENT.md`; do not infer that
recovery authorizes installation, reload, channel changes, or a client swap.
