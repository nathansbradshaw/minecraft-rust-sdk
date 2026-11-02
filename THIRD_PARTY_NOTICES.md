The bounded Java framing/compression implementation and transport deadline
handling were extracted from author-owned source and relicensed here under MIT.
No private Git history or runtime captures are included.

`crates/java-1-21-1/packet-ids.json` contains packet identifier mappings derived
from PrismarineJS/minecraft-data, commit
`1df9432c07c81b47326e4fb1e06d3c8d723bf4aa`, `data/pc/1.21.1/protocol.json`.
That revision's README declares MIT; the declaration, attribution and standard
MIT terms are recorded in `licenses/minecraft-data-MIT.txt`.
These are protocol metadata, not Minecraft assets or server binaries.

The Gradle wrapper is an Apache-2.0 build tool from Gradle 9.2.1.
See `licenses/Gradle-Apache-2.0.txt` and the wrapper's embedded notices.

Cargo dependencies keep their own licenses. `Cargo.lock` pins the dependency
closure; release checks must review licenses and advisories for that closure.
`cargo run --locked -p minecraft-sdk-xtask -- --licenses OUTPUT_DIRECTORY`
collects sanitized SPDX declarations and packaged license texts for the pinned
Linux dependency closure. Binary distributions and the supplied Docker images
include this collection. No installed game/mod JAR is embedded in this project.

The coding image also installs the distribution's Rust ripgrep package for source
search. Ripgrep is offered under MIT or Unlicense; its packaged copyright/notice
and completion-file notices remain under `/usr/share/doc/ripgrep/`. Base-image
and operating-system packages retain their own distribution license notices.
