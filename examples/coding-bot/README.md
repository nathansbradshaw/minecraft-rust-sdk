# Editable coding workspace

This example runs in the reviewed coding container, where `/opt/minecraft-sdk`
contains the matching read-only SDK source and `mc-sdk` is already installed.
Your source, target files and own private `bot.key` live in `/workspace`.

The program starts no character by default. To explicitly control it:

```sh
mc-sdk connect bot.key YOUR_UNIQUE_CONTROLLER_REQUEST_ID
cargo run --locked -- observe
mc-sdk mcp bot.key
mc-sdk disconnect bot.key
```

Edit the Rust program to implement your strategy. Discover installed capabilities
and use typed `ActionRequest` values with stable IDs. Stop and reconcile uncertain
receipts before another mutation; do not retry under another ID. Use the agent's
container terminal for coding, compiling and running. Allocate model access per
bot; personal credentials and host mounts are not needed.

Take a bounded workspace checkpoint while the program is idle before stopping
the container. The operator retains authoritative journals outside this workspace.
