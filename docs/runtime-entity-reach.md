# Runtime interaction geometry

NeoForge observations add three optional facts:

- `player.eye_position`: current native eye position, `[x, y, z]`.
- `player.entity_interaction_range`: current native runtime interaction range.
- Visible `world.entities[].bounds`: current native world-space box,
  `[minX, minY, minZ, maxX, maxY, maxZ]`.

The reference body uses eye-to-box distance only to decide whether an optional
attack is worth requesting while escaping. It retains visibility, hostility,
interaction and attack-strength guards, and ranks eligible targets by the
existing feet distance. The server still decides reach, permissions and effects.
No entity dimensions or species sizes are inferred.

Older integrations remain compatible: missing or malformed geometry uses the
previous three-block feet-distance eligibility check. Valid runtime geometry
that denies reach never falls back to that broader rule. Current pose and
attributes can change these facts; saved values are not current authority.

The separately built [operator updater](../server-neoforge/tools/README.md)
can replace only the observer method body after exact JVM/source review.
Compilation and matching class schema do not prove a live update or successful
combat; verify fresh native fields and actual action effects independently.
