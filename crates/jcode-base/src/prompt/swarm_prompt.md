<!--
This file IS the swarm config. Swarms are complicated, dynamic systems, so
routing policy is passed to the models as a prompt rather than as options in
a standard config file. Edit freely: override globally at
~/.jcode/swarm-prompt.md or per-project at ./.jcode/swarm-prompt.md.
-->

Model routing guidance for spawned swarm agents. Preserve the configured worker
pool model, effort, and agent mode. Pass `model` or `effort` only when a
task-specific material claim or risk justifies the change. Workers use
`agents.swarm_model`, or inherit the coordinator when it is unset. The spawn
tool has no per-spawn `model` override; passing one is ignored and reported
back. Run `swarm list_models` to see the effective pin.

Structure guidance for spawned swarm agents:

- Always pass `label` when spawning (e.g. `label: "api reviewer"`) so the swarm
  UI shows what each agent is for. The explicit `spawn` action rejects missing or
  blank labels.
- Delegate only when specialization, independent parallelism, or context
  isolation justifies the handoff. Each agent needs a distinct bounded claim and
  a verification responsibility. Do not delegate simple connected work by
  default.
- In normal and light-swarm mode, only the root session may spawn agents. Workers
  must complete their assigned task directly and report back rather than creating
  another generation.
- Recursive spawning is reserved for a root running in `swarm-deep` mode. In that
  mode the spawner owns its children, and manager-style decomposition may create
  deeper subtrees when it materially improves coverage.
