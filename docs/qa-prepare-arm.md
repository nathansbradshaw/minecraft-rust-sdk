# Restore an owned QA fixture and run its existing bench

`mc-sdk-bench prepare PREPARATION.json SHA256 NEW_PRIVATE_CASE`

Operator-host tooling only. This is not a gameplay tool or public gateway API.
The first real preparation run is still an open acceptance check. Required source
CI and synthetic tests do not establish faster setup, useful play or Survival.
Never apply this command to an active arm or a production installation.

## What it does

1. Verify the running bench hash, frozen fixture and snapshot hashes, a private
   canonical QA root and its pinned ownership marker, and the pinned prior
   zero-player/zero-session closure witness. Every prior service PID must be
   absent; even PID reuse blocks rather than granting permission to signal it.
   All declared loopback listeners must be free. Existing working and frozen
   gateway databases must have the fixture's instance and zero sessions.
2. Create an exclusive `.prepare-arm.lock`. Decode the declared gzip/ustar
   snapshots into a new private staging tree. Only named roots, ordinary files
   and directories are accepted; links, devices, PAX/GNU extensions, duplicate
   entries and unsafe paths fail. Archives needing normalization must be reviewed
   and frozen again **outside** a measured arm; the command never rewrites them.
3. Preserve previous world/registry/auxiliary roots and the working database plus
   WAL/SHM through exact recoverable renames. Durable install intent/completion
   records identify partial installs. No broad deletion or automatic rollback.
4. Start only the pinned Java executable in its declared cwd. Bound read-only
   `GET /v1/discover` readiness **before starting the gateway**. Unauthenticated
   `401` is listener-only evidence. Then start the pinned gateway and optional
   owned relay, and require actual SDK discovery of the frozen ready instance
   and fresh-observation capability. Typed RCON `list` must show zero players;
   the working database must still have zero sessions. No actor is connected.
5. Delegate to the existing `acquire` or `combine` command, preserving its full
   preflight, guarded staging, immediate launch, deadline and finalization logic.
   No duplicate staging logic or connected manual idle interval is introduced.

The native-readiness and SDK-readiness phases each use a configured wait of up
to120 seconds,121 attempts and500ms spacing. A deadline may be exceeded by at
most one bounded in-flight read (and synchronous pinned-file verification).
Native HTTP reads have a two-second total receive deadline; SDK children have
their existing two-second subprocess deadline. Their real wall times
include verification and waiting. Read-only connection refusal can be retried;
no gameplay mutation is retried by this wrapper.

## Frozen private input

The preparation JSON rejects unknown fields. All `Pinned` objects are
`{"path":"/absolute/canonical/file","sha256":"64hex"}`. Files containing
operator state are regular private files owned by the operator. Configure:

- `version:1`, stable `owner`, canonical private `root`, and the actual
  `bench_sha256` of this rebuilt executable.
- `marker`: a file directly at that root containing
  `{"format":"minecraft-qa-root-v1","owner":"REVIEWED_OWNER",` followed by
  `"isolated_qa":true,"production":false}`.
- `prior_closure`: the reviewed saved receipt containing `format` equal to
  `minecraft-qa-preparation-closure-v1`, matching `owner` and `root`,
  `players:0`, `sessions:0`, and one record per service:
  `{role,pid,generation,executable_sha256}`. This is operator-attested historical
  closure evidence, not a fresh native player observation. Fresh empty-server
  evidence is checked again before delegation.
- `fixture`: the existing reviewed acquisition/combined bench fixture. Its
  workspace/secrets and operator gateway database must be inside this QA root;
  evidence, trusted services and restored roots must not overlap bot mounts.
  An older layout with its database elsewhere must be prepared/reviewed separately
  before freezing a new arm; this wrapper never moves unrelated host paths.
- `archives`: one to three `{file,destination,roots}` objects. `destination`
  is a safe relative directory already inside the QA root; `roots` contains only
  explicitly reviewed single-component names. The combined restored paths must
  be unique and nonoverlapping. Frozen inputs cannot live in replaced trees.
- `database:{file,destination}`: a standalone frozen SQLite database and a safe
  relative working path equal to the fixture's operator journal path.
- `services`: one Java, one gateway and optionally one relay record, each
  `{role,tool,cwd,args,listen}`. Roles are `java`, `gateway`, `relay`. `tool` is
  pinned; cwd is canonical inside the isolated root. Arguments are direct argv,
  never shell interpolation. No shell/interpreter executable, environment
  overrides, service adoption, Docker control or process-name/port signalling.
  This private operator declaration is authority; never accept it from bot code.
- `native:{address}`: the Java integration's declared loopback listener.
- The fixture RCON address must occur in Java's declared listeners. Its host
  profile's SDK endpoint must have a canonical HTTP(S) loopback IP and explicit
  port occurring in the declared gateway/relay listeners. Hostnames, userinfo,
  implicit ports and query credentials are rejected. HTTPS verification remains
  the existing SDK's responsibility; a matching instance alone is not ownership.
- `ready_timeout_secs`:1–120, independently bounded for native and SDK readiness.

Keep private config/token paths in reviewed service arguments, never secret
values. Arguments and environment values are not copied into phase receipts.
The wrapper captures no service stdout/stderr; this keeps service output bounded
and prevents native credentials from entering transcripts. Readiness results and
owned child generation records remain available. The gateway must read the newly
rotated private native credential through its existing reviewed configuration.

## Limits, evidence and recovery

Archives have at most32768 entries each,64MiB per file and4GiB total restored
archive bytes; the private standalone database is bounded to64MiB. Require505GB
free before restoration, then check the500GB reserve before each imported file.
Existing bench container, output, resource and evidence caps remain applicable.
No new bot/model allowance, inference call or helper gameplay policy is supplied.

`prepare-summary.json` and phase receipts record actual monotonic wall time,
UTC boundaries, frozen/build hashes, read attempts and failure/recovery links.
SDK/bench subprocess statuses retain their existing child times; do not add them
and call that total setup time. Snapshot hashing and tool verification also take
wall time. Never infer missing setup durations or claim a speedup without matched
measured runs.

Before bench delegation, failure attempts to stop only direct children created
by this invocation, using their owned process groups. Unknown ownership is never
signalled. Partial restore trees, prior state, snapshots and phase evidence remain.
After delegation, services are **explicitly handed back to the operator** even
when the bench fails: automatically stopping Java could interrupt an unresolved
actor. Exact child/PID/hash records identify the handoff; they are not reusable
permission to signal a later process with the same PID. Ordinary bench actor/NPC
closure and separate owned-service shutdown must be verified. Remove the exclusive
root lock only after inspecting recovery state and proving actor/service closure.
A host crash, SIGKILL or detached service is unknown; use retained ownership
records. A successful command is no autonomous-play or competitive verdict.
