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

The Rust Brain embeds Lua 5.4 through mlua (MIT), built from lua-src's packaged
Lua sources (MIT). The pinned Cargo license collection includes their notices;
Lua behavior execution does not require a separately installed interpreter.
The published `mlua-sys` 0.13.0 crate omits its license text. Its exact upstream
repository revision `a186d461c7772aed4cb0a9fa1d1899121c6450e7` supplies
[`LICENSE`](https://github.com/mlua-rs/mlua/blob/a186d461c7772aed4cb0a9fa1d1899121c6450e7/LICENSE),
retained verbatim in `licenses/mlua-sys-0.13.0-MIT.txt` (SHA256
`63b61a30d3e3abcfcb1bda4b99f4eea9fdb7d6ab05fbc513461fef6cf89baf29`).
The collector uses it only for that exact crates.io package/version and MIT
declaration and records its upstream origin. Other missing texts still fail.
The embedded Lua 5.4.9 engine's complete copyright and MIT notice is also
retained verbatim from `lua-src` 551.0.2's `lua-5.4.9/lua.h` at upstream revision
`3c65a4674fd25a89fb924538ce7fe1c89c64a71b`, in `licenses/Lua-5.4.9-MIT.txt`.
The collector adds it alongside that exact package's existing license texts.

The coding image also installs the distribution's Rust ripgrep package for source
search. Ripgrep is offered under MIT or Unlicense; its packaged copyright/notice
and completion-file notices remain under `/usr/share/doc/ripgrep/`. Base-image
and operating-system packages retain their own distribution license notices.
