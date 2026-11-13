# Build from reviewed source. Resolve and record the resulting image digest before launch.
FROM rust:1.91.1-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY licenses ./licenses
RUN cargo build --release --locked -j 1 -p minecraft-sdk-cli -p minecraft-sdk-runner
RUN cargo run --release --locked -j 1 -p minecraft-sdk-xtask -- --licenses /build/dependency-licenses
COPY LICENSE THIRD_PARTY_NOTICES.md ./

FROM debian:bookworm-slim AS proxy
COPY --from=build /build/target/release/minecraft-sdk-runner /usr/local/bin/minecraft-sdk-runner
COPY --from=build /build/LICENSE /build/THIRD_PARTY_NOTICES.md /usr/share/doc/minecraft-sdk/
COPY --from=build /build/licenses /usr/share/doc/minecraft-sdk/licenses
COPY --from=build /build/dependency-licenses /usr/share/doc/minecraft-sdk/dependencies
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/minecraft-sdk-runner"]

FROM rust:1.91.1-bookworm AS coding
RUN apt-get update && apt-get install -y --no-install-recommends ripgrep && rm -rf /var/lib/apt/lists/*
COPY --from=build /build/target/release/mc-sdk /usr/local/bin/mc-sdk
COPY --from=build /build/Cargo.toml /build/Cargo.lock /opt/minecraft-sdk/
COPY --from=build /build/crates /opt/minecraft-sdk/crates
COPY --from=build /build/LICENSE /build/THIRD_PARTY_NOTICES.md /usr/share/doc/minecraft-sdk/
COPY --from=build /build/licenses /usr/share/doc/minecraft-sdk/licenses
COPY --from=build /build/dependency-licenses /usr/share/doc/minecraft-sdk/dependencies
ENV HOME=/workspace CARGO_HOME=/cache/cargo MC_SDK_SOURCE=/opt/minecraft-sdk
USER 1000:1000
WORKDIR /workspace
CMD ["sleep", "infinity"]
