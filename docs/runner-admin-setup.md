# Dedicated rootless runner

Review and build `minecraft-sdk-runner` from the pinned source before executing
an administrator command. Run once on the Linux host:

```sh
sudo /absolute/path/to/minecraft-sdk-runner init-host YOUR_EXISTING_ADMIN_USERNAME
```

This creates `mc-bots` with home `/srv/mc-bots` and no additional groups,
requires the distribution's `useradd` allocation of at least 65,536 subordinate
UIDs/GIDs, makes its home private, enables its user manager at boot, and runs the
installed Docker rootless setup tool as that user. Existing rootful Docker is
preserved. It adds a reviewed `sudoers` rule allowing the named administrator
to manage **only as mc-bots**, and an aggregate user-slice budget of 3 GiB RAM,
no swap, two CPU equivalents and 2,048 tasks. These are deployment defaults;
operators may change the slice budgets for their machine.

Requires preinstalled Docker rootless extras, uidmap, rootlesskit, slirp4netns,
cgroup v2 and delegated resource controllers. It fails rather than adding the
runner to `docker` or `sudo`, replacing another account, or disabling another
Docker daemon. A partially completed setup is safe to inspect and rerun.

Container launch requires the `mc-bots` identity and a rootless daemon with
cgroup v2. Each bot gets its own internal network and a reviewed egress proxy.
The coding image receives no host bind mounts or Docker socket. It runs UID
1000 with capabilities dropped, no new privileges and a read-only root.
Workspace/cache writes use separately sized tmpfs mounts under the memory
budget; logs rotate under an explicit budget. Workspaces survive while the
container exists but **must be exported before stopping/recreating it**.
Persistent quota-backed workspaces remain a deployment acceptance gate.

The egress proxy permits exact operator-configured HTTPS `host:port` pairs.
It validates every DNS result, rejects private/LAN/link-local/metadata and
IPv6 transition addresses, and connects the checked numeric address. An
optional exact SDK socket exception is for a deployment's private gateway.
The proxy image/configuration remain outside the editable coding workspace.

Do not mount personal credentials. Provision a separate bot-scoped SDK key
and explicitly allocated model access. Source compilation/execution happens
inside the coding container; the stdio MCP server provides gameplay tools.
