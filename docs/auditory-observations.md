# Auditory observations

Hearing is separate from sight. NeoForge module `0.1.0-alpha.14` advertises the
optional `minecraft:auditory_observation` capability. `world.auditory` contains
versioned, listener-scoped sound cues from packets actually sent to that admitted
character. Visual entity visibility and radius stay unchanged.

The frame format is `minecraft-sdk-auditory-v1`. It includes listener UUID,
dimension, connection/dimension epoch, sample game time, latest sequence, dropped
count, retention ticks and events. Events contain runtime sound ID, category,
emission origin, native game time, volume, optional pitch, delivery provenance,
native sound-event range and distance at delivery. They contain no emitter entity
ID, health, inventory or AI target. An emission origin does not establish an
unseen entity's current pose. Treat auditory facts and threat hypotheses separately.

The initial native carriers are direct positioned sound packets, entity sound packets
and explosion packets. Explosion pitch is chosen client-side and is unavailable.
Capture excludes non-finite, zero-volume and beyond-native-range cues, retains at
most 128 events/64 KiB for 100 game ticks, and bounds deferred main-thread capture.
Dimension changes, clock rollback and reconnect start a new epoch. Expiration is
normal; the dropped counter reports capture/capacity loss. Off-thread backpressure
can occur before a packet's final eligibility is determined.

This is a server-delivered audio-cue interface, not an audio renderer. Client
mixers, resource-pack attenuation overrides, local-only playback, other level or
entity event carriers and voice-chat mods need separate coverage. Native sound
IDs are semantic assistance and must be disclosed in agent comparisons. No sound
assets are distributed. NeoForge documents [server versus local playback and
sound-definition attenuation](https://docs.neoforged.net/docs/1.21.1/resources/client/sounds/).

Rust clients use `minecraft_sdk::auditory::frame(&observation)` to validate scope
and bounds, then a persisted `auditory::Cursor` to consume fresh events once.
Missing capability/data is unavailable, not silence. Cursors reject sequence
rollback within an unchanged epoch. Their state belongs in the caller's own
workspace; they perform no gameplay or inference.

`auditory-probe PRIVATE_CONNECTED_PROFILE.json SECONDS [PRIVATE_CURSOR.json]`
provides a bounded read-only diagnostic for 1–30 seconds. Connect and disconnect
with `mc-sdk`; retain private profile/output files. It reports only observations
and hashes and cannot prove a body response or Survival. Validate native hearing
and negative controls before implementing sound-triggered tactical behavior.

The default reference body requires this capability and a valid initial frame;
older modules fail startup clearly. Its independent thread keeps up to 32 recent
emissions for 600 native ticks and gives reasoning the most recent eight. Facts
and cursor are persisted in the character's private body journal. A separate,
four-entry alarm memory prevents unrelated sound bursts from erasing an active
priming cue. Listener, life, dimension, server clock and sound epoch changes
clear old hypotheses; repeated observations never extend their lifetime.

A creeper priming sound permits a precautionary blast hypothesis for 40 ticks
using 12-block clearance because charge state is unknown. TNT priming uses 100
ticks and eight blocks. These are client policy leases, not observed countdowns
or proof of a stationary explosive. The body uses existing finite movement over
known support and cancels travel toward the emission origin. Water, terrain,
visible threats and projectile handling remain active. No entity row, hidden
target or present-day pose is manufactured from a sound.

Footsteps, bow shots and other cues reach reasoning as auditory facts. Automatic
bow-source tracking, voice transcription and general sound investigation are not
implemented. Source checks establish neither reaction timing nor survival: native
heard-but-unseen response trials, benign/stale controls and sustained coding-load
trials are separate acceptance requirements.
