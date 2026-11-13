# Run a bounded operator fixture

Implementation status: source-only draft awaiting the coordinated test/build.
`launch` is deliberately disabled until owned remote-process/container
finalization and archive integration are complete. The isolated RCON list fast
path is available for the operator's bounded read-only installation check:

```sh
mc-sdk-bench rcon-list 127.0.0.1:25575 /private/operator-only/rcon.password
```

It reads a canonical private host file at runtime, uses only typed `list`, and
prints a bounded JSON reply with password redaction. It needs no fixture,
profile, model or actor. The operator must keep that password outside all bot
mounts. This command does not perform container-mount inspection.

`mc-sdk-bench` prepares one declared case using an **existing** container,
ordinary SDK commands and an optional private loopback RCON connection. It does
not install services, allocate characters, call a model itself, create a coding
harness, heal, respawn, roll back the world or award a Survival PASS.

Build/review the evidence package under the operator's coordinated compiler
lease. Run the bench on the operator host, never inside a model workspace.
`preflight` makes no SDK/gameplay/RCON calls; it inspects the existing container
and hashes its declared artifacts. `stage` and `launch` make real connections and,
when RCON is configured, the narrowly declared staging operations.

```sh
mc-sdk-bench preflight /private/case/fixture.json FIXTURE_SHA256 /private/case/run
mc-sdk-bench stage /private/case/fixture.json FIXTURE_SHA256 /private/case/run
mc-sdk-bench launch /private/case/fixture.json FIXTURE_SHA256 /private/case/run
```

Use an absolute canonical mode-700 case directory outside bot mounts. Its files
are mode600, created without overwriting or following symlinks. Use the actual
SHA-256 of the reviewed fixture, with real allocated UUIDs and pinned artifacts;
placeholders fail validation. Each command is attempted once in that directory.
A failed command is retained, not replaced with a successful-looking attempt.

Stage connects the **already allocated, disconnected** bot profile using the
fixture's original request ID. The dedicated secret directory must be mounted
read-only into the container: a directory mount sees the CLI's atomic session
profile updates, unlike an old single-file bind mount. It may contain only this
bot's SDK profile, optional allocated proxy key and explicitly pinned public CA
bundle. Blocked mode needs no model key: set its model fields to null and the
handler config's `model_key` to an empty string. Task mode requires the allocated
proxy key. No credential is copied to a workspace.
The proxy allocation is operator-attested; it may be shared and is not a claim
of actor-unique or server-enforced model authority. Opaque key text alone cannot
prove its authority. Personal/provider account credential
files, native/server tokens, Docker sockets and unrelated host bind mounts are
not permitted. Common personal/provider key formats are refused as well.

With RCON, stage checks the actual SDK actor/life, full HP20/food20 and empty
inventory/cursor, requests guarded teleport, and checks native pose and unchanged
life afterward. Each staging snapshot uses one advertised
`minecraft:fresh_observation` query, without cached fallback or retry. Without
RCON, it records **pending operator staging** and leaves
the actor connected. The operator must complete staging and proceed promptly or
run `mc-sdk disconnect /private/bot-secrets/profile.json`. Stage does not summon.

Launch uses the existing pinned probe, with exactly:
`CONFIG blocked|task 120 ADMISSION_SPEC`. It validates the handler's actual
`body_probe_ready` event (actor, session life, NPC UUID, HP20/food20, inventory,
cursor and start pose) before one guarded ordinary skeleton summon. If RCON is
absent, it prints a pending spawn handoff and writes its private receipt;
it does not claim a summon. The handler's native admission is separately required.
No new model/harness code is executed by the bench beyond the reviewed handler.

Exposure is120seconds; admission is independently bounded to120seconds by the
pinned spec. Container GNU `timeout` bounds the handler at245seconds including
shutdown, with a255second host deadline. Use a non-root existing read-only-root
container with GNU `timeout`, `sha256sum` and `test` available. Container secret
readability is checked without returning its contents; fix UID mapping/ownership
on the operator host if the non-root user cannot read the dedicated mount.
These deadlines do not
stretch the declared exposure or fabricate provider/compiler overlap. The bench
attempts ordinary SDK disconnect after the handler, including fatal/timeout paths.
When it attempted a summon, cleanup checks a bounded3x3 chunk ring around the
last native NPC position (or declared spawn position if no later snapshot exists).
It queries existing force-load state and adds only chunks confirmed previously
unforced. It requires a positive exact skeleton location, a confirmed typed kill
and subsequent absence before claiming that NPC removed. It releases only loads
confirmed added by this run; preexisting loads remain unchanged. Ambiguous
responses, an NPC outside the loaded ring, uncertain chunk operations or failed
release leave cleanup **INCOMPLETE** and the CLI nonzero, with an ownership ledger.
There is no expanding search. Without RCON, manual cleanup remains a pending
operator handoff and launch cannot claim complete closure. Inspect shutdown and
cleanup receipts before further work.
Never replay an uncertain mutation or reconnect automatically to settle it.

The bench preserves redacted complete stdout/stderr, including a final fatal
non-JSON line and nonzero exit. It records READY/admission separately and checks
artifact hashes again after execution. A writable artifact is not a trusted
compiler-input attestation merely because its before/after hashes match.

## Private loopback console setup

The operator may configure loopback RCON using the separately reviewed
`configure_loopback_rcon` evidence example, following its `--help`/usage and the
operator's normal backup/zero-player restart procedure. The bench never edits
`server.properties`, restarts Java or reads a password from command-line text.

Set `operator_rcon` to `null` for explicit handoffs, or provide a loopback socket
and a canonical mode600 password file **outside every bot mount**, including the
read-only bot secret mount. Only the host bench reads it at runtime. Commands
are typed: guarded staging/summon and exact owned skeleton removal. A response
is not native proof; timeouts/ambiguous responses preserve uncertainty and are
not retried. No raw console command CLI exists.

## Freeze the fixture

All paths below are examples. Hash actual files and allocate real identities.
The config must use the scoped secret paths below, a workspace under `/workspace`,
`body.enabled=false`, `period_ms=150` and `decision_samples=128`. The pinned
admission spec must bind the same NPC/positions, player HP20/food20 and120second
admission budget. The handler enables the protective body after admission.

```json
{
  "version": 1,
  "case_id": "declared-case-01",
  "instance_id": "ACTUAL_64_HEX_INSTANCE",
  "player_uuid": "ALLOCATED_ACTOR_UUID",
  "npc_uuid": "NEW_OWNED_NPC_UUID",
  "connection_request_id": "declared-case-01-connect",
  "duration_secs": 120,
  "source_revision": "ACTUAL_40_HEX_SOURCE_COMMIT",
  "container": "existing-operator-container",
  "workspace": "/private/operator/workspace",
  "container_workspace": "/workspace",
  "secret_dir": "/private/operator/bot-secrets",
  "container_secret_dir": "/run/bench-secrets",
  "profile_name": "profile.json",
  "model_key_name": null,
  "allocated_model_actor_uuid": null,
  "allocated_model_kind": null,
  "ca_file": {"file":{"path":"/private/operator/bot-secrets/ca.pem","sha256":"ACTUAL_CA_SHA256"},"container_path":"/run/bench-secrets/ca.pem"},
  "sdk": {"path":"/private/bin/mc-sdk","sha256":"ACTUAL_SHA256"},
  "docker": {"path":"/absolute/canonical/docker","sha256":"ACTUAL_SHA256"},
  "evidence": {"path":"/private/bin/mc-sdk-evidence","sha256":"ACTUAL_SHA256"},
  "artifacts": [
    {"file":{"path":"/private/pins/body-probe","sha256":"ACTUAL_SHA256"},"container_path":"/workspace/body-probe"},
    {"file":{"path":"/private/pins/config.json","sha256":"ACTUAL_SHA256"},"container_path":"/workspace/config.json"},
    {"file":{"path":"/private/pins/spec.json","sha256":"ACTUAL_SHA256"},"container_path":"/workspace/spec.json"}
  ],
  "handler": "/workspace/body-probe",
  "handler_args": ["/workspace/config.json","blocked","120","/workspace/spec.json"],
  "expected_player_position": [262.5,63.0,-720.5],
  "expected_npc_position": [262.5,64.0,-709.5],
  "position_tolerance": 0.5,
  "expected_health": 20.0,
  "expected_food": 20,
  "trace_action_prefix": null,
  "operator_rcon": {"address":"127.0.0.1:25575","password_file":"/private/operator-only/rcon.password"}
}
```

At least500GB free is required. The bench reuses the existing container/cache;
it never builds images, creates target directories, purges caches or installs
packages. If an executable is a symlink, pin its canonical target path instead.
Preflight requires dropped ALL capabilities, no added capabilities/devices,
no-new-privileges, non-host/non-shared namespaces and positive limits of at most
3GiB memory, two CPU equivalents and2048PIDs. It rejects unreviewed mounts,
including read-only volumes, and checks an allowlist of environment **names**
without returning their values. This checks these declared boundaries; it is not
a general sandbox certification or an audit of every existing workspace file.
For task mode, supply the allocated model key/name and
`allocated_model_kind:"operator-allocated-proxy"`; actor allocation metadata is
optional operator context, not proof that a shared proxy key is actor-unique.
The public CA may be null, included in the scoped directory, or a separately
pinned read-only mount. The bench sets `MC_SDK_CA_CERT` to its host path for SDK
commands and its container path for the handler, retaining normal TLS verification.

## Capture every original action

After disconnection and the operator's ordinary saved-state capture/projection:

```sh
mc-sdk-bench nativecapture /private/case/fixture.json FIXTURE_SHA256 \
  /private/case/capture /private/gateway.sqlite /private/case/run/handler.stdout \
  /private/case/spec-template.json \
  /private/case/player-before.json /private/case/stats-before.json \
  /private/case/player-after.json /private/case/stats-after.json
```

The template is the existing `mc-sdk-evidence` Spec format. Bind the actual trace
SHA, actor, instance, source, program/controller hashes, model counts, failures and
assists; its action list is replaced by all original matching SQLite actions.
No generated programme means an explicitly disclosed empty-content hash, not a
compiled-programme claim. Saved files are operator-supplied JSON projections;
the bench retains and hashes them without inventing missing death statistics.

The bench derives the body epoch from original trace receipt IDs, checks every
intent against every actor/prefix row, and refuses omissions/extras. An optional
exact `trace_action_prefix` can be frozen when already known. Without any receipt
or an explicitly bound prefix it fails incomplete rather than guessing an epoch.
Multiple epochs require explicit separate captures. All outcomes—including
running, rejected, cancelled and uncertain—remain unchanged. No status/reconcile
API or SQLite writer is used. The library underlying `mc-sdk-evidence` validates
native receipts; reports are sharded by size and its128-action limit. The complete
index and file hashes are in `capture-manifest.json`. Do not average shard
percentiles or discard missing timing attempts.

`native-original.log` preserves the supplied trace, including fatal non-JSON
lines; `native-projection.jsonl` contains only parsed JSON and explicitly counts
excluded lines. Neither projection nor a clean exit certifies goal completion,
full health continuity, actual inference/compiler overlap or Survival competence.
