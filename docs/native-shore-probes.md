# Fast native shoreline probes

Use `body-probe CONFIG shore SECONDS SPEC` to diagnose the body and foreground
navigator without inference. Run one controller for the character. Keep the
configuration, profile, spec and output private; preserve the character's native
save and journals. This is a mechanics fixture, not an autonomous benchmark.

A blocked-reasoning spec:

```json
{"workload":"blocked","stationary_seconds":30,"stationary_radius":1.5,"dry_samples":3}
```

Use `"compile"` for a real offline Cargo build in the configured bot workspace.
The build must finish successfully before a dry-ground result can pass. Its
elapsed time and saved compiler output establish whether the compiler actually
ran or simply reused artifacts.

Use `"navigate"` with a `"goal"` containing an ordinary SDK navigation goal for
an operator-declared dry destination. The driver resumes bounded navigation
slices; both `reached` and actual dry/on-ground observations are required. A
reached underwater waypoint cannot pass. Goal coordinates and dimension IDs
come from the fixture's observations and remain in the private spec.

```sh
umask 077
body-probe private-config.json shore 60 private-spec.json > private-trace.jsonl
```

The monitor consumes the resident body's observations, including the pre-body
start witness. It requires water to have actually been seen, followed by repeated
advancing native dry/on-ground samples. Death, actor/entity/dimension/clock change,
workload failure or the overall deadline fails. A wet character that remains
within the configured displacement radius for the stationary window fails early;
physical displacement resets that window. Repeated copies of the same native tick
do not count as new samples or progress. The window uses 20 native game ticks per
nominal second; server stalls reach the wall-clock deadline instead.

The summary reports health/air minima, wet/dry observations, control identity and
observed input-release flags. These are client diagnostics. Independently verify
trusted action receipts, input release, death/damage statistics and saved state
before closing a gameplay card. Three dry samples do not establish gathering,
crafting, skill reliability or broad Survival competence. The parent probe stops
the body and disconnects on success, failure, deadline or Ctrl-C.
