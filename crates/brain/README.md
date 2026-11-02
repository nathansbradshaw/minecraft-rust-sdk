# Experimental container-local coding brain

This Rust harness observes through the public SDK and gives Luna/Sol a real tool
loop: inspect files, generate Rust, compile/run, act, inspect outcomes and repair.
It runs only in a non-root container. The trusted inference service returns data
and source; it never receives host tool execution permission. SDK gateway/server
credentials and authoritative journals remain outside the container.

Build `cargo build --locked -p minecraft-sdk-brain` and run inside the isolated
coding container:

```text
minecraft-sdk-brain run /workspace/brain/config.json
```

The private mode-600 JSON config supplies `profile`, `model_key`, `model_endpoint`,
`workspace`, `state`, `goal`, optional `cargo_target`, `max_turns` (default 64),
`tool_timeout_secs` (240), `model_interval_secs` (31), and `exit_on_complete`
(false). Paths and endpoint are deployment configuration, never compiled server
addresses. Use the existing per-bot Cargo cache/target where compatible; do not
create duplicate caches for each command. Model credentials must be a separately
allocated per-bot key accepted by a compatible HTTPS inference proxy. SDK public
registration does not grant model access.

The workspace should contain an ordinary Rust Cargo project with the SDK and
Tokio dependencies and an appropriate dependency cache/vendor configuration.
The harness sets `MC_SDK_PROFILE` for programs, with the already-connected private
profile. Programs can use `Client::with_profile`, `observe`, and typed actions;
they must preserve action IDs and inspect receipts instead of replaying uncertain
mutations. No private companion format is required by the harness.

The terminal's output, execution time and memory retained for results are bounded.
It drains ordinary compiler output instead of breaking the compiler's pipes after
the displayed excerpt. Larger output floods fail. Linux container/kernel budgets
remain the security boundary; workspace path checks are an accidental-write guard,
not a sandbox against arbitrary terminal code. Keep host networking, Docker
sockets, private directories and personal credentials out of the container.

`brain.sqlite` retains source-labelled facts/reports/hypotheses/episodes/skills,
tool results and action receipts. One local file lock fences duplicate harnesses
using the same state directory; operators must still stop any legacy controller
before assigning the same character. The gateway enforces controller identity.
The harness records its own pending action before dispatch and offers explicit
status/reconciliation tools. Current state does not establish a historical effect.
Search includes older journal events and action receipts, so a short context
window does not prevent the agent retrieving its actual action IDs. Source
editing uses separate path and UTF-8 content fields without nested JSON source
encoding. Completed tool activity persists across reasoning turns.
The latest tool result has a separate context budget so ordinary source output
is not silently replaced by its abbreviated history. `minecraft-sdk-brain
context STATE_DIRECTORY` prints the last exact model context for local debugging;
keep that output private because it includes observations and player conversation.

Finite terminal jobs clean up their process group, including background children.
Normal completion, run-budget exit and handled termination disconnect the SDK
character rather than leaving it unattended. Abrupt process/container loss still
needs deployment supervision; this is not a proven background Survival controller.

Model `complete` is recorded as a claim with an observation. It cannot certify an
evaluation or evidence gate. This initial harness has not established autonomous
Survival, comprehensive chat delivery, transfer, endurance, fair player-like
perception, or competitive performance. Those need their own integration and
native evaluations. The current observation projection inherits the installed
SDK provider's perception; it must not be presented as a player-visibility proof.

All model/tool source is MIT. Do not include generated worlds, keys, personal bot
memory or deployment journals in a release or source publication.

Build with a toolchain and libc compatible with the runtime container. A Linux
host build can acquire newer glibc symbol requirements even when source checks
pass. `Cargo.container.toml` defines a minimal three-crate workspace for compiling
inside the existing coding image. Reuse its per-bot Cargo target and reviewed
vendor configuration; do not download another toolchain or dependency bundle.
