# Local Development and Deployment Safeguards

This guide protects machine-local Jcode customizations when feature work is
built or deployed. It is a deployment gate, not a release procedure. Follow it
before replacing a client, server, stable, current, or immutable local artifact.

## Incident and current posture

On 2026-09-08, source checkout `ef80c7842` (`v0.84.11`, dirty) was installed
over a custom client built from `e4d87f21` (`v0.84.19`). Header and statusline
separators and layout regressed. The previous client source commit is not
available in this checkout or `~/.jcode/source/jcode`. New session manager work
is uncommitted and must be retained. The `ef80` daemon features remain desired;
the recovery concern is client-only.

Archive provenance was recovered on 2026-09-08 from the user-approved,
user-owned GitHub ref `archive/combined-ui-lifecycle` at exact commit
`e4d87f2151b773141801618e1078fe2bf69102aa`. The recovered source is cloned at
`/Users/diplomat/jcode-custom` on the separate integration branch
`jcode/session-manager-archive-integration`. The session-manager transfer is
not acceptance-certified yet. This recovery establishes an approved integration
baseline, not visual proof or deployment approval. Until preserved
customizations are explicitly accounted for, deployment fails closed.

## Non-negotiable rules

1. Identify the running client and daemon independently. Never infer either
   from checkout `HEAD`, a daemon version, or a channel symlink.
2. Maintain one user-approved, cumulative source integration baseline. Create
   feature branches from that baseline. Do not automatically import, merge,
   cherry-pick, or copy another person's or agent's branch. Preserve the branch
   ownership restrictions in `AGENTS.md`.
3. Pin client, server, and stable artifacts separately. A change approved for
   one target must not implicitly promote, reload, or replace another target.
4. If source provenance is unavailable or local customizations have not been
   accounted for, preserve the old artifact and stop deployment. Recover the
   source or obtain an explicit integration decision. Isolated builds are
   allowed because they do not replace shared artifacts.
5. Do not treat functional tests as visual acceptance. Client presentation
   changes require direct visual proof.

## Pre-deployment identity and preservation gate

Before any replacement, record the following in a machine-local manifest and
rollback journal. Keep both private. Do not place secrets in either record.

### Identify what is actually running

For **both** client and daemon, record:

- The loaded executable path, including the resolved target rather than only a
  symlink.
- Its reported version and source hash, when available.
- Its binary checksum.
- Relevant channel links and their resolved targets: launcher, `current`,
  `stable`, `canary` when applicable, and `shared-server` for the daemon.

Also record the source checkout path, branch, `HEAD`, and dirty state. These
are provenance inputs, not substitutes for identifying a loaded executable.
For a running TUI, confirm its process's executable after the switch. Repointing
a link does not change an already-running TUI process.

### Preserve the candidate and the artifact being replaced

For the proposed replacement, record:

- Source path, commit, branch, and dirty patch.
- A private snapshot of relevant untracked source files, after confirming it
  contains no secrets.
- The intended immutable artifact path and checksum.
- Exact build and test commands.
- The baseline client and server versions, source hashes, and binary checksums.
- The approved change target: client, daemon, stable channel, current channel,
  or a named isolated artifact.

Never overwrite the prior immutable artifact. Create a new immutable artifact,
then preserve its manifest entry and journal before switching an approved link
or starting a new process from it.

## Controlled deployment

1. Confirm that the candidate descends from, or has an explicit integration
   decision against, the single approved cumulative baseline.
2. Build a new immutable artifact. Do not reuse or overwrite the old one.
3. Run the recorded build and functional checks against the approved target.
   For experimentation, use an isolated socket or isolated artifact and leave
   global channels and the shared daemon unchanged.
4. Perform and record the preserved-customization acceptance checks below.
5. Switch only the explicitly approved target. Do not use a client deployment
   as permission to reload or promote the daemon, stable channel, or other
   global links.
6. After the switch, compare the loaded client executable, version/hash, and
   checksum to the intended artifact. Separately verify the daemon if, and only
   if, the approved target included it.
7. Add the actual post-switch identities and result to the rollback journal.

If any identity, checksum, source provenance, or acceptance check differs from
the recorded expectation, stop and roll back the changed target. Do not
continue by guessing which process or channel is active.

## Preserved-customization acceptance checks

Before approving a client replacement, capture before-and-after visual evidence
for each applicable behavior:

- Header separator line.
- Statusline separator line.
- Statusline layout.
- Side panel presentation and navigation.
- Arrow-key navigation.
- `Ctrl+X` behavior.
- New-session creation.
- Session preview behavior.

Record the scenario, expected result, observed result, and evidence location in
the private manifest or journal. A passing automated or functional test suite
does not prove these presentation details. If comparison evidence is absent,
the customization is not accounted for and deployment must fail closed.

## Rollback journal

Create the journal entry before switching and finish it immediately after the
comparison. Each entry must include:

- Timestamp, operator, approved scope, and reason for the change.
- Previous and candidate immutable paths, checksums, versions, and source
  provenance.
- Previous and intended channel-link targets, recorded separately for client,
  daemon, and stable pins.
- Required commands to restore the prior target and any process action needed
  for that target to take effect.
- Post-switch loaded executable identities and the visual acceptance result.

Rollback means restoring only the changed approved target to its recorded
previous immutable artifact, then verifying the loaded process independently.
It must not silently reset unrelated daemon, client, or stable pins.

## Proportional workflow

Use this gate for artifact replacement, not for every edit. It does not require
a DAG, a repeated full review cycle, an install, a daemon reload, or a global
promotion. It requires enough provenance, preservation, and visual evidence to
make a replacement reversible and attributable.
