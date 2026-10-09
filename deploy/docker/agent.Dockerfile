# The afkfleet agent's image (Plan.md P5.6; ADR-0014).
#
# Build it from the repository root, which .dockerignore narrows to the Rust
# workspace:
#   docker build --file deploy/docker/agent.Dockerfile --tag afkfleet-agent:dev .
# `just stack-up` builds it through deploy/compose.dev.yaml.
#
# Production runs on linux/arm64 and development on linux/amd64, so this file
# builds natively on both. Every base image is pinned by its multi-arch index
# digest (the top-level Digest of `docker buildx imagetools inspect <image>`),
# never by one platform's digest, and nothing here names an architecture.
# Digests are bumped deliberately, in their own pull request.

# --- chef: the build tools, each in its own cached layer before any source ---
FROM rust:1.99.0-slim-trixie@sha256:24e632c09342c20abf8312cf4f61430a911c01ed3a5e4c02b87292b1c39c5273 AS chef
# cargo-chef with the image's stable toolchain, before the nightly is pinned.
RUN cargo install --locked cargo-chef --version 0.1.78
WORKDIR /build
# The nightly comes from rust-toolchain.toml alone, its only pin (ADR-0003).
COPY rust-toolchain.toml ./
RUN rustup toolchain install

# --- planner: the workspace's dependency recipe ---
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY .cargo ./.cargo
COPY crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

# --- builder: the dependencies (cached while the recipe stays), then the agent ---
FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --locked --package fleet-agent --bin afkfleet-agent --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY .cargo ./.cargo
COPY crates ./crates
RUN cargo build --release --locked --package fleet-agent --bin afkfleet-agent

# --- runtime: glibc and libgcc only, no shell, the nonroot user (65532) ---
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:e792ab3d241a468a4fd7519ddbbebe66b49b5f365771716ea688ad40b6c6f1c2
LABEL org.opencontainers.image.title="afkfleet-agent" \
      org.opencontainers.image.description="The afkfleet agent: runs Minecraft Java AFK bots." \
      org.opencontainers.image.source="https://github.com/noah-gabel/afkfleet" \
      org.opencontainers.image.licenses="GPL-3.0-or-later"
COPY --from=builder /build/target/release/afkfleet-agent /usr/local/bin/afkfleet-agent
ENTRYPOINT ["/usr/local/bin/afkfleet-agent"]
# The config belongs at /etc/afkfleet/agent.toml. A deployment that runs the
# agent with another --config path must override HEALTHCHECK with the same
# path, or the check reads another config's heartbeat_file.
# AFKFLEET_AGENT__… variables apply to both either way.
CMD ["run", "--config", "/etc/afkfleet/agent.toml"]
# The agent beats every 10 s; the check fails once the heartbeat is 30 s old.
HEALTHCHECK --interval=10s --timeout=5s --start-period=30s --retries=3 \
    CMD ["/usr/local/bin/afkfleet-agent", "healthcheck", "--config", "/etc/afkfleet/agent.toml"]
