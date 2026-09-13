# Fast shared build lane

Use a shared incremental lane from any Jcode worktree:

```bash
scripts/fast_build.sh focused test exact_test_name --test integration_suite
```

The lane hashes repository identity, `Cargo.lock`, the requested target, and the
**effective runner fingerprint** emitted by `scripts/dev_cargo.sh`. That
fingerprint is produced after dev_cargo selects toolchain, wrappers, linker, and
Rust flags, and also covers the runner and Cargo configuration file contents.
Only digests appear in keys or status output, so flags and paths are not exposed.
Worktree paths are not in the key.

## Commands

```bash
# Exact Cargo command, serialized and forwarded to dev_cargo.
scripts/fast_build.sh cargo check -p jcode-app-core

# Fixed syntax. The filter must be the second cargo argument.
scripts/fast_build.sh focused test session_picker::tests::opens --test integration_suite

# Check only mapped changed packages, then run exactly the focused command.
scripts/fast_build.sh candidate test session_picker::tests::opens

# One explicit release invocation.
scripts/fast_build.sh release build --release

# Read-only inventory.
scripts/fast_build.sh cache-path
scripts/fast_build.sh status
```

`focused` rejects a missing filter and rejects selector-first forms such as
`focused test --test integration_suite`. The invocation is otherwise forwarded
unchanged. dev_cargo retains its explicit-filter zero-test guard.

`candidate` maps committed, staged/unstaged, and untracked paths to their nearest
package manifest. It fails rather than falling back to `cargo check` for the
whole workspace when no package maps. Set
`JCODE_FAST_CHANGED_PACKAGES=package_a,package_b` for an explicit selection, or
`JCODE_FAST_BASE_REF` for a different committed comparison base.

## Lock protocol

Each compatibility key has an independent `mkdir` directory lock. The winning
process writes a unique regular token and publishes a hard-linked regular
`owner` record atomically. Lock paths and owner records are rejected if they are
symlinks or unexpected types. Cleanup verifies the acquired directory device and
inode plus its token, then removes only its exact regular `owner` and token files
and uses `rmdir`. It never recursively deletes a lock path or follows a link.

A fresh ownerless directory remains active during the grace interval
(`JCODE_FAST_BUILD_LOCK_GRACE`, default 2 seconds). Recovery needs two stable
directory identity checks and a dead or PID-start-mismatched regular owner
record. Every waiting state observes `JCODE_FAST_BUILD_LOCK_TIMEOUT` (default
1800 seconds). A live owner is never killed.

The outer lane exports `JCODE_DEV_CARGO_GATE_HELD=1` only while it owns the
verified per-key lock. This is the single documented nested-lock protocol:
dev_cargo skips its global gate for that invocation. Direct dev_cargo calls keep
their existing global gate behavior. Different fast-build keys remain
independent.

## Retention

The lane never runs `cargo clean`, removes target trees, or prunes artifacts.
`status` and `cache-path` are read-only. Any manual cleanup requires explicit
confirmation of exact paths after checking active owners and measured sizes.
