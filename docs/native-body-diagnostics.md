# Native body diagnostic admission

`body-probe CONFIG blocked|compile|exposed SECONDS STIMULUS` optionally gates a
diagnostic on an actual SDK observation. Use a private stimulus JSON file:

```json
{
  "entity_uuid": "00000000-0000-4000-8000-000000000001",
  "entity_type": "minecraft:skeleton",
  "entity_position": [10.5, 1.0, 0.5],
  "entity_health": 20.0,
  "player_position": [0.5, 1.0, 0.5],
  "position_tolerance": 0.25,
  "player_health": 20.0,
  "player_food": 20,
  "main_hand": "minecraft:bow",
  "day_time_range": [13000, 20500],
  "timeout_secs": 300
}
```

The operator prepares a fresh empty Survival character and a disclosed disposable
arena before starting the probe inside its bounded container. Wait for
`body_probe_ready` before spawning the declared enemy through the operator's
existing server console. For the example UUID, the corresponding Java entity
UUID integer array is `[I;0,16384,-2147483648,1]`. Use a fresh UUID per attempt.
Keep normal enemy AI enabled; do not freeze it or force its target. This tool
does not expose the server console to bot programs.

The probe stays connected with the protective body and model calls disabled
while awaiting the stimulus. It admits only the declared observed UUID, type,
pose, health, visible ranged weapon and optional natural day window, while the
player still has its expected pose, health, food and empty inventory. A present
but displaced/dead/wrong stimulus, an unexpected visible hostile, changed player
state, or timeout ends the attempt with a retained error and session cleanup.
No timed trial starts and no in-run repair is attempted.

`body_probe_stimulus_admitted` retains the actual SDK start observation and native
clock. The body then starts on the same character session, followed by the full
declared test duration. `compile` runs the real Rust compiler; retain its complete
artifact output and prove native movement overlaps that interval. Body death
and ordinary respawn still fail Survival acceptance. A healthy run with a weak
or mismatched stimulus cannot establish matched-pressure performance.

Without a stimulus file, the existing diagnostic modes remain available. Their
after-startup scene can already include enemy or character movement; it is not
an admission witness. These operator diagnostics make no autonomous competence,
open-world Survival, or competitive-performance claim.
