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

The operator bench requires a pinned `runtime_check` artifact and runs it before
its first connection, including standalone stage. Build and test the helper in
the shared compiler lane before live use.

The same pinned Linux helper supports `process-witness ABSOLUTE_ELF_PATH`.
After READY, the bench invokes this once inside the frozen container rather than
making a Docker request for each PID. It hashes the bounded regular ELF, scans
at most64 `/proc` entries under a2-second internal deadline, and matches same-UID
processes by device/inode with stable PID start times. It never reads command
lines or environments. The outer Docker request has a10-second timeout; container
identity verification is separate. This snapshot does not prove later immutability.

The bench requires a complete `minecraft-runtime-process-witness-v1` result,
the frozen executable path/hash, and exactly one matching process. It retains
`ready-process-witness.json` and records binding failures in
`remote-bind-failed.json`; rejected process callbacks also appear in the handler
status. Older helper binaries do not implement this command and must be replaced
and repinned before running the updated bench. No actor or gameplay call is made.
