# v0.1 release gates

This is a development acceptance record, not a declaration of stable support.
Update each gate only with corresponding observed evidence.

| Gate | Current evidence |
| --- | --- |
| Independent Rust clean build, format, tests, clippy | Local build/test checks underway |
| Minimal NeoForge-only build | Independent module builds with no optional mods |
| Public registration / persistent UUID / observations / look | Real minimal-server pilot passed |
| Mining / building / crafting / transfers / chat / mod menu | Pending full observed interaction trial |
| Disconnect / reconnect same inventory and location | Pending final trial |
| Key scoping, rotation, reservations, one controller, durable duplicate IDs | Store/contract tests pass; HTTP adversarial tests pending |
| Interrupted action reconciliation without replay | Store restart test passes; process interruption trial pending |
| Normal authentication and claim denials | Integration preserves vanilla handlers; live denial tests pending |
| Rootless runner and aggregate resources | Dedicated Linux account/daemon/cgroup v2 verified |
| Container filesystem / cross-workspace / network / resource adversarial probes | Pending real container trial |
| Persistent bounded coding workspaces | Pending deployment completion; initial kit uses bounded tmpfs |
| Installed modpack compatibility and fresh live-world pilot | Pending saved-restart deployment after backup |
| Existing four character migrations / journal preservation | Pending individual imports; no existing character started |
| Source, licenses, secrets, reproducible release artifacts | Pending final release audit |

Do not label an untested mod operation, dispatched packet or reconciled current
snapshot a verified historical gameplay success. Release artifacts must include
only SDK programs and the integration JAR, never runtime worlds or game assets.
