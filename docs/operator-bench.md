# Run a bounded operator fixture

For the separate no-NPC1200-second Brain acquisition path, see
[qa-acquisition-lifecycle.md](qa-acquisition-lifecycle.md). Its source preparation
and validation status are tracked separately from this120-second skeleton mode.

The operator commands prepare, run and close one frozen fixture. Their receipts
record failures and incomplete cleanup; they never award gameplay success.
The isolated RCON list fast path checks the installation without an actor:

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
`preflight` uses read-only SDK `discover ENDPOINT` to require the actual frozen
ready instance's `minecraft:fresh_observation` capability, without an actor
connection or RCON/gameplay call. It inspects the existing container and hashes
declared artifacts, and checks config/spec regular-file mode0600 and effective
UID ownership inside the container. Its pinned Linux runtime checker then creates
and verifies a private Unix socket in the declared state filesystem before any
actor connection. `stage` and `launch` make real connections and,
when RCON is configured, the narrowly declared staging operations.

```sh
mc-sdk-bench preflight /private/case/fixture.json FIXTURE_SHA256 /private/case/run
bot-dev capture /private/case/outer 600 -- mc-sdk-bench run /private/case/fixture.json FIXTURE_SHA256 /private/case/run
```

Use an absolute canonical mode-700 case directory outside bot mounts. Its files
are mode600, created without overwriting or following symlinks. Use the actual
SHA-256 of the reviewed fixture, with real allocated UUIDs and pinned artifacts;
placeholders fail validation. Each command is attempted once in that directory.
A failed command is retained, not replaced with a successful-looking attempt.

The automatic `run` path requires RCON and immediately chains stage and launch.
It performs expensive SDK/artifact/filesystem preflight before connecting, then
rechecks only the frozen container generation and local fixture at launch.
Prepare/check everything before connecting. Do not insert a manual idle window
between staging and launch: natural hostiles can kill the unprotected actor.
Standalone stage is an explicit manual handoff only, requiring prompt disconnect
if launch cannot proceed. Post-stage launch initialization failures attempt the
ordinary scoped disconnect without replaying mutations.

Stage connects the **already allocated, disconnected** operator profile using the
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
The separate mode600 `operator_profile` stays outside all bot mounts and can use
the host loopback endpoint. Its instance and complete bot credential must match
the internal profile. Only the session field is atomically synchronized into the
internal profile, preserving its container HTTPS endpoint and credential; both
sessions are cleared after disconnection/verified native absence. Profiles are
never logged.

With RCON, stage checks the actual SDK actor/life, full HP20/food20 and empty
inventory/cursor, requests guarded teleport, and checks native pose and unchanged
life afterward. Each staging snapshot uses one advertised
`minecraft:fresh_observation` query, without cached fallback or retry. Without
RCON, it records **pending operator staging** and leaves
the actor connected. The operator must complete staging and proceed promptly or
run `mc-sdk disconnect /private/operator-only/host-profile.json`. Stage does not summon.

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
confirmed added by this run; preexisting loads remain unchanged.
After one confirmed kill, cleanup permits a two-second read-only settlement
polling window for the normal death animation, with100ms polling plus at most
one in-flight read under the existing one-second command deadline. This is not
a hard two-second end-to-end deadline. It never repeats the kill. Absence must still
be confirmed; expiry remains incomplete and owned chunk releases are attempted.
Ambiguous responses, an NPC outside the loaded ring, uncertain chunk operations or failed
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
  "container_id": "ACTUAL_64_HEX_DOCKER_ID",
  "container_started_at": "ACTUAL_DOCKER_STARTED_AT_UTC",
  "owned_container": true,
  "bot_dev": {"path":"/private/bin/bot-dev","sha256":"ACTUAL_SHA256"},
  "workspace": "/private/operator/workspace",
  "container_workspace": "/workspace",
  "secret_dir": "/private/operator/bot-secrets",
  "container_secret_dir": "/run/bench-secrets",
  "profile_name": "profile.json",
  "operator_profile": "/private/operator-only/host-profile.json",
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
    {"file":{"path":"/private/pins/spec.json","sha256":"ACTUAL_SHA256"},"container_path":"/workspace/spec.json"},
    {"file":{"path":"/private/pins/mc-sdk-runtime-check","sha256":"ACTUAL_SHA256"},"container_path":"/workspace/mc-sdk-runtime-check"}
  ],
  "handler": "/workspace/body-probe",
  "runtime_check": "/workspace/mc-sdk-runtime-check",
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
The reviewed compiler-image names are explicitly allowed: `CARGO_HOME`,
`CARGO_TARGET_DIR`, `CARGO_INCREMENTAL`, `CARGO_BUILD_JOBS`,
`CARGO_PROFILE_DEV_DEBUG`, `MC_SDK_CA_CERT`, `MC_SDK_SOURCE`, `PATH`,
`RUSTUP_HOME`, `RUST_VERSION`, `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER`,
`CC_x86_64_unknown_linux_gnu`, `CXX_x86_64_unknown_linux_gnu`,
`PKG_CONFIG_ALLOW_CROSS` and `PKG_CONFIG_LIBDIR`. This is an exact list, with no
`CARGO_*` wildcard. Credential/account names such as `GITHUB_TOKEN`, `CODEX_HOME`
and `CARGO_REGISTRIES_CRATES_IO_TOKEN` remain rejected. Mount/resource checks
still apply to the same container.
For task mode, supply the allocated model key/name and
`allocated_model_kind:"operator-allocated-proxy"`; actor allocation metadata is
optional operator context, not proof that a shared proxy key is actor-unique.
The public CA may be null, included in the scoped directory, or a separately
pinned read-only mount. The bench sets `MC_SDK_CA_CERT` to its host path for SDK
commands and its container path for the handler, retaining normal TLS verification.

Remote finalization binds the frozen container ID/start and actual handler
PID/start/executable at READY. Host process exit alone is not remote closure.
On error it checks that identity, sends bounded SIGINT, and stops only the same
owned container if closure remains unknown. Before emergency stop it attempts a
three-second nonquiescent archive, explicitly incomplete. Normal closure uses
existing `bot-dev capture` for an untouched compressed tar of workspace and the
distinct pinned config `state` path, preserving path hierarchy and runtime journals.
Exact pinned ELF inputs are retained separately as mode600 byte copies with
verified hashes (64MiB total budget). Only current container paths independently
verified as matching regular files are then excluded by anchored literal paths.
Changed files, symlinks and replacement directories remain in the source capture.
Source, config, admission and runtime journals stay in the compressed
snapshot. The existing8MiB log limit is unchanged; overflowing snapshots remain
incomplete and preserve the idle container for recovery. Copying inputs must
retain500GB free on the operator host.
This is a bounded file snapshot before final container stop; the old handler's
exit and actor absence do not establish that remaining descendants have stopped
writing. The archive explicitly records that limit, and the final container stop
closes those remaining processes.
The state path must be a scoped workspace or `/tmp` child outside secrets.
It excludes credential-file patterns/cache/target; arbitrary embedded content is
not certified keyfree. Failed normal archives preserve the container/tmpfs and
return incomplete rather than discarding source. Recovery argv and ownership
metadata are saved before handler launch. SIGKILL/host loss remains unknown;
verify those ownership fields before recovery, never stop a container by a reused name.

Private directory readability does not prove Unix-socket support. Use the
separately pinned Linux `mc-sdk-runtime-check` described in
[runtime-filesystem-check.md](runtime-filesystem-check.md); preflight requires and
invokes it before connecting. The strict0600 policy is
unchanged. A completion-kit claim additionally needs external retained player
inventory proof of both table and wooden pickaxe; cursor/grid/placed blocks or a
model subgoal cannot substitute. A reconnect's changed menu is a new starting
state, not an exact replay or fresh-world claim.

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

## Keep closure and goal results distinct

Cleanup loads one bounded fallback ring and, if the exact owned skeleton's native
position lies outside it, at most one additional current chunk. Every new ticket
has an absent-before receipt and is released afterward; preexisting tickets stay
in place. A confirmed kill is never replayed while its death animation settles.

An optional `operator_gateway_journal` names the reviewed authoritative host
gateway SQLite file. Keep it private, canonical and outside every bot mount.
The bench uses a read-only transaction to bind instance ID and the operator
profile's exact bot ID/UUID, then counts all session rows, including connecting
states. This is an operator-attested journal snapshot, not automatic proof that
an arbitrary copy belongs to a running gateway. No key or session token is copied.

If the controller already disconnected, the SDK cleanup call can report
`SessionRequired`. Its acknowledgement remains false and its original error is
retained. Only a zero-session journal snapshot plus independent native actor
absence and remote-handler closure can establish `already_closed_verified`.
Missing or mismatched proof stays incomplete; the disconnect is not replayed.
These separate snapshots provide no continuous admission fence or survival credit.

Freeze the completion condition together with the task goal. A pure Lua helper
pressure case must explicitly use and check `completion_inventory: []`; a kit
acquisition case must declare its required items. An inherited kit goal can keep
a successfully run helper working beyond its intended completion. Never rewrite
an already-run fixture or turn that observation into survival credit.
