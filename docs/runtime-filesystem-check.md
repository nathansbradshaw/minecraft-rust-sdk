# Check the runtime filesystem before connecting

Run `mc-sdk-runtime-check PRIVATE_STATE_DIR` inside the actual idle fixture
container, as its configured non-root user, before connecting an SDK actor.
Pin the Linux executable's hash with the other fixture artifacts. A host-side
check does not establish what the container filesystem supports.

```sh
bot-dev capture /private/case/socket-preflight 15 -- /canonical/docker \
  exec FROZEN_CONTAINER_ID /workspace/mc-sdk-runtime-check /tmp/brain-state
```

The Rust helper creates a missing state leaf with mode700 under an existing
canonical parent. Existing state must be a private directory owned by the
effective user. It creates a unique child directory, binds a Unix socket, applies
mode600, verifies its type/owner/permissions, then removes its own probe entries.
Existing journals remain in place. It makes no SDK, model or gameplay calls.

Require exit0 and `minecraft-runtime-filesystem-check-v1` with both
`unix_socket_bind` and `socket_mode_0600` true. This is an offline filesystem
capability result, not controller startup or survival proof. Retain a failed
capture; a readable config or successful socket bind alone is insufficient.
The private probe subdirectory lengthens the socket pathname. Use a short runtime
path such as `/tmp/brain-state`; a pathname-length error does not establish a
filesystem permission failure.

This check addresses an observed setup failure where a host bind mount allowed
socket creation but rejected `chmod(0600)` with `EINVAL`. Container-local runtime
state such as `/tmp/brain-state` can avoid that mount limitation. Keep the private
socket policy, and archive the declared runtime state together with the workspace
before stopping a container whose state is on tmpfs.

The operator bench must run this check before its first connection when the
helper is integrated and pinned; otherwise socket preflight remains explicitly
pending. Build and test the helper in the shared compiler lane before live use.
