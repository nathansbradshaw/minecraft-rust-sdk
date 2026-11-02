# Operator-side run evidence

`mc-sdk-evidence` captures selected authoritative action rows directly from an
operator-owned gateway SQLite journal, using a read-only connection and one
consistent transaction. It never opens the gateway's writer Store, migrates a
database, connects a player or performs gameplay. Run it outside bot containers,
as the gateway's Linux account. It has no public HTTP/MCP endpoint.

```text
mc-sdk-evidence capture GATEWAY.sqlite PRIVATE_SPEC.json NEW_PRIVATE_REPORT.json
```

The journal and specification must be private regular files owned by the caller.
The output is a new mode-600 file; overwrite and symlink destinations fail.
Keep reports private: native receipts include world/player state and chat may
appear in the separately bound controller trace. Never publish those artifacts
with reusable source or install packages.

The specification binds a run/goal, server instance, player UUID, full source
commit, controller/program/source/trace SHA-256 hashes, model request counts,
labelled resource measurements, environmental assists, interventions, failures
and expected typed action IDs/intents/outcomes. Read the `Spec` Rust structure for
its closed JSON schema. Hash actual artifacts independently; measure resource
usage before/after runs where deltas matter. An end-of-run snapshot is not peak
memory or per-run CPU usage. Preserve unmeasured values explicitly as null.

Capture resolves the actor from the authoritative identity registry and compares
every requested action with that actor's real journal row. Unknown IDs, other
actors/instances, changed intents and different outcomes fail. `observed` also
requires the authoritative native-effect flag and no error. Reconciled state is
recordable but contributes zero observed effects.

This is narrow evidence capture. Native rows come from the operator-selected
gateway journal. Source/build/model/resource metadata and interventions remain
operator attestations, not independently verified claims. The tool does not
certify that a candidate executable ran, that an overall goal succeeded, that
the run occurred at a particular time, or that an agent is competent. Inspect
controller and trusted inference logs, binaries and native observations
separately. A journal controlled by a bot is not an authoritative input. A full
benchmark still needs its frozen conditions, independent outcome checks,
complete failure accounting and comparison procedures.
