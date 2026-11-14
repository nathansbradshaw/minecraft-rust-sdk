# Run one frozen acquisition arm

Source preparation only: the acquisition path is not yet built or verified.
Keep the existing120-second skeleton fixture unchanged. This separate operator
mode runs the pinned, unmodified Brain entry point without a forced NPC or
supplied solution code.

Use the common fixture fields in [operator-bench.md](operator-bench.md), with:

```json
{
  "mode": "acquisition",
  "duration_secs": 1200,
  "npc_uuid": null,
  "expected_npc_position": null,
  "handler": "/workspace/minecraft-sdk-brain",
  "handler_args": ["run", "/workspace/config.json"],
  "runtime_check": "/workspace/mc-sdk-runtime-check",
  "model_key_name": "allocated-proxy.key",
  "allocated_model_kind": "operator-allocated-proxy"
}
```

The config is a pinned private artifact. It must bind this allocated actor's
internal profile and proxy key, scoped workspace and state path, and enable the
responsive body at startup (`enabled:true`, `period_ms:150`,
`decision_samples:128`). No admission spec or NPC identity is accepted. The host
operator profile stays outside bot mounts; its credential/instance must match
the internal profile, whose endpoint stays suitable for the container.

```sh
mc-sdk-bench preflight FROZEN_FIXTURE.json FIXTURE_SHA NEW_PREFLIGHT_CASE
mc-sdk-bench acquire FROZEN_FIXTURE.json FIXTURE_SHA NEW_ARM_CASE
```

Preflight checks the declared container generation, mount/resource boundaries,
artifact hashes, private config ownership, actual Unix socket permissions and
SDK fresh-observation capability before connecting. `acquire` then connects the
already allocated disconnected actor, verifies HP20/food20/empty inventory and
cursor, performs guarded staging, checks the same life and native pose, and
immediately launches the exact pinned Brain argv. Standalone stage/launch/run
commands reject acquisition fixtures, avoiding a connected manual gap.

The remote Brain child has a1200-second GNU timeout and a1210-second host
stream-capture deadline. These deadlines apply only to that child. **Do not wrap
the whole acquisition helper in a1210-second capture**: setup precedes the child
clock, and disconnect/export/stop finalization must continue afterward. Cleanup
and archive calls retain their separate bounded budgets. Any whole-helper
developer watchdog or host loss is a separate unknown-closure condition, with
the frozen container identity retained for recovery; it is not an acceptance
window or evidence of a clean stop.

The host binds the real container PID/start/executable at the first observed
handler JSON and records one full native GO query, using the same staged SDK
session. That sample may follow startup activity. It is not an exact timestamp
of the Brain's first instruction, and requested settings do not certify matched
native time/weather. The same live actor/life is required; post-start pose,
health/food or inventory drift is recorded for external matching rather than
presented as an unchanged initial state. Original `brain_started` and `controller_exit` events are
retained when present, without adding markers to either frozen source arm.

No NPC, chunk-loading or time/weather mutations run in acquisition mode. After
normal exit, fatal output, timeout or interruption, the shared finalizer attempts
one scoped disconnect, verifies native actor absence, retains source/runtime
data and closes only the frozen owned container generation. Host child exit
alone never proves remote closure. Unknown or failed cleanup remains incomplete;
no uncertain mutation is replayed.

Each arm's comparison manifest must separately freeze source/binary provenance,
models and allocations, config/goal/scaffold, warmed cache state, disposable world
template, starting pose and explicit operator assists. Keep original process,
model, native receipt and saved-state evidence. Report actual GO sampling offsets
and condition mismatches. [The environment gate proposal](qa-environment-gate-proposal.md)
is not an implemented time/weather override.

The helper awards no crafting, survival or comparative performance score. An
external scorer must verify retained crafting-table and wooden-pickaxe inventory,
deaths/life continuity, final saved health, all original action outcomes and
actual model/compiler intervals. A placed table, cursor stack, programme claim,
zero process exit or healthy respawn cannot replace that evidence. Preserve
failed/censored arms and their original acceptance criteria.
