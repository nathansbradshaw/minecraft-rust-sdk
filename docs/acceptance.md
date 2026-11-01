# v0.1 alpha acceptance record

Observed on 2026-11-01 with Minecraft 1.21.1 / NeoForge 21.1.233. These are
bounded trials, not autonomous Survival endurance or support for every mod.
Raw worlds, private profiles and native journals are excluded from source.

| Gate | Observed evidence |
| --- | --- |
| Independent builds | Workspace tests, formatting, clippy and independent Java module build; CI repeats source/build checks |
| Minimal installation | Real NeoForge-only server with this integration, normal online authentication and secure profiles |
| Useful gameplay | Native observed look, walking, staged mining, genuine pickup, placement, inventory transfers, sandstone crafting from four gathered sand and chat on the minimal fixture |
| Installed modpack | Fresh non-operator Survival characters in the existing world; observed stairs, short drop/jumping movement, natural dirt mining/pickup, VisualWorkbench opening and four verified slot clicks transferring a stack into its grid and back |
| Public self-service | Registration, private mode-600 profile, connection and observed gameplay from a rootless coding container over public HTTPS; no human account |
| Continuity | Disconnect/reconnect resumes the same UUID and observed location/inventory |
| Adversarial HTTP | Rotation/old-key rejection, cross-bot key substitution, forged actor/privileged fields, raw-packet kinds, wrong session, controller conflict, unknown capability, duplicate/changed IDs, cross-bot status and oversized-body probes |
| Interrupted mutation | Adapter interrupted during real movement; receipt uncertain, gateway quarantined, restart and identical ID produced no movement; reconciliation reported no replay and no historical proof |
| Authentication/capacity | Unticketed Java login receives encrypted authentication challenge on both installations; disposable one-slot server returns `ServerFull`; live capacity remains ten |
| Permissions/claims | Protected minimal-spawn mining left block unchanged and mining inactive; operator command failed to elevate authority. Installed Open Parties and Claims: peer opened unclaimed workbench, owner temporarily claimed its chunk, peer denied while owner retained access, peer access returned after normal unclaim; temporary claim removed |
| Filesystem isolation | Real non-root container, read-only root, capabilities zero and no-new-privileges; no personal/server/socket mounts; bounded workspace/cache writes work |
| Network isolation | Allowed HTTPS positive control succeeds; unrelated proxy, direct host/LAN/metadata/IPv4/IPv6 targets fail. Peer listener has an inside-peer positive control and is unreachable from the other bot network |
| Resources | Kernel memory/swap/CPU/PID limits inspected; real process exhaustion and workspace ENOSPC contained. Aggregate budget is independent of registry counts; no memory-OOM trial claimed |
| Workspace persistence | Bounded private checkpoint, container stop/restore, identical source/binary hashes. Tmpfs requires explicit idle checkpoints; no unattended checkpoint automation claimed |
| Existing history | Four original UUIDs/names imported separately, 867,536 original IDs referenced read-only outside containers; source bytes unchanged in regression; scoped workspaces/checkpoints provisioned, sessions disconnected |
| Original progress/services | Saved installation backup verified before Java-hook restart; all 28 preexisting progress files matched afterward; authentication, map routes and unrelated services retained |
| Publication | Independent MIT history, staged/history exclusion audit, pinned dependencies, thin authored-only integration JAR and dependency license-text collection; checksums accompany installable artifacts |

## Reproducing bounded checks

- `cargo test --workspace --locked` and `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo run --locked -p minecraft-sdk-xtask -- .`
- `cargo run --locked -p minecraft-sdk-xtask -- --licenses target/dependency-licenses`
- `server-neoforge/gradlew -p server-neoforge build`
- SDK examples `minimal_survival`, `security_probe`, `menu_transfer` and
  `interruption_probe` require explicitly connected disposable characters. Read
  their prerequisites. The interruption probe signals only a verified local
  adapter after a prepared intent; it is an operator trial.
- The Java adapter's `normal_auth_probe` never authenticates or enters the world.
- `examples/container-boundary-probe.rs` and `peer-listener.rs` provide container
  probes; positive controls are required before interpreting a network failure.

## Limits

Ordinary menu slots include the tested VisualWorkbench menu. No specialized
machine plugin is installed. Combat, item use, commands and other actions without
matching native evidence remain uncertain until explicit reconciliation;
dispatch alone is not success. Claim command effects were independently checked
through access changes. Navigation has a finite step budget and can require
intermediate waypoints. Food, shelter and defense belong in bot programs.

Imported characters have editable SDK starter programs and private keys. Their
former autonomous strategies remain private and are not silently started or
claimed to have been ported. Model access must be allocated per deployment.
Back up gateway state and original journals in addition to Minecraft's world
and native-identity directory.

Java without this integration, other loaders/versions and Bedrock are unsupported.
An alpha release does not promise every mod interaction or human-device acceptance.
Never treat reconciled current state as historical proof.
