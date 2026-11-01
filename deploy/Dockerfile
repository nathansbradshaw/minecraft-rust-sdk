# Build from reviewed source. Resolve and record the resulting image digest before launch.
FROM rust:1.91.1-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked -j 1 -p minecraft-sdk-cli -p minecraft-sdk-runner

FROM debian:bookworm-slim AS proxy
COPY --from=build /build/target/release/minecraft-sdk-runner /usr/local/bin/minecraft-sdk-runner
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/minecraft-sdk-runner"]

FROM rust:1.91.1-bookworm AS coding
COPY --from=build /build/target/release/mc-sdk /usr/local/bin/mc-sdk
ENV HOME=/workspace CARGO_HOME=/cache/cargo
USER 1000:1000
WORKDIR /workspace
CMD ["sleep", "infinity"]
