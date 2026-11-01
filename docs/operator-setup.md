# Install a gateway instance

Use a dedicated Minecraft service account. Keep the gateway, version adapter,
identity registry and action journals under that trusted account, outside bot
containers. External authors run their own code; registration does not schedule
anything on your machine.

1. Back up the installation and world using your normal saved backup procedure.
   Install NeoForge 21.1.233 for Minecraft 1.21.1 and this release's thin
   `minecraft-sdk-neoforge` JAR in `mods/`. It works without an installed modpack.
   Keep `online-mode=true` and your existing player capacity and permissions.
2. Save and restart Minecraft to install the Java hooks. They cannot be installed
   through the Rust adapter's process interface. The module creates a private
   `minecraft-sdk/` directory and a mode-600 `native.token`. The token changes
   when Minecraft starts. Never copy it into a bot workspace or public proxy.
3. Install the matching Linux binaries under the service account. Copy and edit
   `deploy/adapter.example.json` and `deploy/gateway.example.json`. Select the
   loopback Minecraft backend, private token file and absolute adapter executable.
   The Java native endpoint defaults to loopback port 19135; operators running
   multiple instances can select another with `-Dminecraftsdk.port=PORT`.
4. Create the journal's parent directory with mode 700, owned by the service
   account. Reserve existing bot labels before accepting public registrations.
   Set `registrations_per_minute` and `journal_max_mib` for this deployment.
   The latter defaults to 1024 MiB and limits SQLite database pages; bounded WAL
   checkpoints add temporary overhead. Use a filesystem quota for a strict total
   storage budget including backups. A full journal refuses new intents rather
   than dispatching unjournaled actions. Never prune IDs to recover disk space.
5. Adapt `deploy/minecraft-sdk-gateway.service` to your service account's paths
   and Minecraft unit. It is a user service with memory/CPU/process limits.
   Install, reload and enable it using that account's user systemd manager.
6. Publish only the gateway's loopback HTTP listener through an HTTPS reverse
   proxy. Forward `/v1/` without adding native or operator routes. Configure your
   own public hostname; the client requires HTTPS except on literal loopback.
   Poll `/v1/server` for compatibility, `ready` and deployment limits.

The integration admits exact registered local player connections using one-use
tickets. An ordinary unticketed Java login still requires normal authentication.
Players are non-operator Survival characters; normal server handlers enforce
claim rules. Unsupported specialized mod operations require an operator-installed
adapter, not a client-supplied plugin or raw packet.

# Operator recovery and migration

`minecraft-sdk-admin SOCKET REQUEST.json NEW_PRIVATE_OUTPUT.json` contacts the
mode-600 same-user Unix socket beside the journal. No operator HTTP/MCP endpoint
exists. Keep the journal path short enough for the platform's Unix socket limit;
a relative path is useful on macOS development machines.

Requests include `{"op":"inspect"}`, `{"op":"reserve_name","name":"LABEL"}`,
`{"op":"revoke","bot_id":"ID"}` and
`{"op":"recover_key","bot_id":"ID"}`. Recovery requires a disconnected
character. Revocation disables the old key before attempting disconnect.
The response file may contain a new key; never paste it into chat, logs or source.
Use `mc-sdk import-profile HTTPS_ORIGIN SERVER_INFO.json RESPONSE.json NEW_PROFILE`
to create a private client profile with no controller session.

For an existing author-owned packet journal, `import_legacy` takes `label`,
`player_uuid`, `player_name` and an absolute `journal` path. The gateway verifies
the private file's recorded identity and retains a read-only reference. It
preserves original action IDs and pending uncertainty without copying large
receipts or rewriting the original journal. Keep that source under the trusted
account and in backups. New reconciliation receipts are separate overrides;
old IDs cannot become new mutations. Imported completed records are marked
`imported`, not retroactively verified by the new adapter. Import never connects
or starts a bot program.

# Coding containers

Follow [runner-admin-setup.md](runner-admin-setup.md). Build the proxy and coding
targets from `deploy/Dockerfile`, resolve immutable image digests, and configure
each bot using `deploy/runner.example.json` and a reviewed exact endpoint allowlist.
Provision one bot-scoped key per workspace. The coding image includes `mc-sdk`,
Rust and read-only SDK source at `/opt/minecraft-sdk`. Copy
`examples/coding-bot/` into the writable workspace; its default program leaves
the character stopped. Connect only when a controller is ready, and disconnect
when its work is done.

Allocate model access separately per bot through local inference or an explicit
bot credential/proxy and allow its exact endpoint. No model account, personal
coding-agent authentication or automatic strategy is bundled. Checkpoint idle
workspaces before stopping containers. Container claims never grant an external
client additional gateway authority.
