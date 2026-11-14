# Operator observer update

This standalone Java instrumentation agent targets only
`dev.minecraftsdk.NativeObservation`. It is not a gateway, SDK gameplay or MCP
interface. Do not use it for arbitrary classes, other loaders or unreviewed
transform history. It does not restart the server or reconnect players.

Compile/package with an installed Java 21 JDK into an existing operator staging
directory, separately from the server mod:

```text
javac --release 21 -d CLASSES dev/minecraftsdk/tools/NativeObservationLiveUpdate.java
jar --create --file OPERATOR.jar --manifest observer-update.mf -C CLASSES .
java -jar OPERATOR.jar PID ABSOLUTE_AGENT_JAR ABSOLUTE_PLAN
```

Set a private umask and use private, bounded regular plan/replacement files.
The closed properties plan requires `pid`, exact process `started` instant,
`resource_sha256`, `replacement`, `replacement_sha256`, fresh `receipt` and
fresh `operation_id`; `mode` is `inspect` or `apply`. Inspection loads the agent
but performs no class redefinition. Review its receipt before applying.

Apply additionally requires `expected_loader_id`, `expected_code_source`,
`expected_schema_sha256`, `expected_previous_submission` and
`audited_baseline=true`. Obtain these from the exact owned JVM and separately
review its artifact/update history. A matching class resource is not proof of
loaded method bytes or absence of third-party transformations. The agent
explicitly records those unknowns; an operator assertion cannot prove them.

Apply claims and forces a fresh receipt before the only redefinition call.
Missing completion or an uncertain result requires investigation of that
original operation, never blind replay. JVM schema acceptance does not prove
new gameplay observations or Survival. Existing stack frames can finish with
old code. Verify fresh observations, actor/life continuity and actual fields
separately after any authorized update.
