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
