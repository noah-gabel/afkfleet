# The afkfleet server's image (Plan.md P12.1; ADR-0022, S13).
#
# Build it from the repository root, which .dockerignore narrows to the Rust
# workspace:
#   docker build --file deploy/docker/server.Dockerfile --tag afkfleet-server:dev .
# `just images` builds it beside the agent's.
#
# It follows deploy/docker/agent.Dockerfile (ADR-0014): the same base images,
# pinned by their multi-arch index digests, and the same chef and planner
# stages word for word, so a build of both shares those layers; a test keeps
# them equal (crates/fleet-server/tests/deploy.rs). Nothing here names an
# architecture: it builds natively on linux/arm64 and linux/amd64. Digests
# are bumped deliberately, in their own pull request, in both files at once.

# --- chef: the build tools, each in its own cached layer before any source ---
FROM rust:1.99.0-slim-trixie@sha256:24e632c09342c20abf8312cf4f61430a911c01ed3a5e4c02b87292b1c39c5273 AS chef
# cargo-chef with the image's stable toolchain, before the nightly is pinned.
RUN cargo install --locked cargo-chef --version 0.1.78
WORKDIR /build
# The nightly comes from rust-toolchain.toml alone, its only pin (ADR-0003).
COPY rust-toolchain.toml ./
RUN rustup toolchain install

# --- planner: the workspace's dependency recipe ---
# The desktop crate is a workspace member, so cargo can't load the workspace without it;
# nothing here builds it (ADR-0017).
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY .cargo ./.cargo
COPY crates ./crates
COPY apps/desktop/src-tauri ./apps/desktop/src-tauri
RUN cargo chef prepare --recipe-path recipe.json

# --- builder: the dependencies (cached while the recipe stays), then the server ---
FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --locked --package fleet-server --bin afkfleet-server --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY .cargo ./.cargo
COPY crates ./crates
COPY apps/desktop/src-tauri ./apps/desktop/src-tauri
RUN cargo build --release --locked --package fleet-server --bin afkfleet-server
# The folders compose mounts its volumes at, owner-only (ADR-0022, S14): the
# database, the backups, the vault key and CA key, the CA certificate, the
# server's TLS pair. A new named volume takes its folder's owner and mode, so
# the server (65532) owns it. There's no VOLUME: a forgotten mount then lands
# on the read-only root, and the server refuses to start, instead of an
# anonymous volume quietly taking the data (ADR-0015).
RUN mkdir -m 0700 /folders /folders/data /folders/backups /folders/keys /folders/ca /folders/tls

# --- runtime: glibc, libgcc and the CA certificates, no shell, the nonroot user (65532) ---
# distroless/cc carries /etc/ssl/certs/ca-certificates.crt, which reqwest's
# platform verifier reads for the Microsoft accounts' calls (ADR-0018).
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:e792ab3d241a468a4fd7519ddbbebe66b49b5f365771716ea688ad40b6c6f1c2
LABEL org.opencontainers.image.title="afkfleet-server" \
      org.opencontainers.image.description="The afkfleet server: the API, the agents' control plane and the database." \
      org.opencontainers.image.source="https://github.com/noah-gabel/afkfleet" \
      org.opencontainers.image.licenses="GPL-3.0-or-later"
COPY --from=builder --chown=65532:65532 /folders/ /
COPY --from=builder /build/target/release/afkfleet-server /usr/local/bin/afkfleet-server
USER 65532:65532
# The API and the agents' gRPC port. The metrics port (9480) is never exposed:
# it's for the project's own network only (ADR-0022, S10).
EXPOSE 8080 7443
ENTRYPOINT ["/usr/local/bin/afkfleet-server"]
# The config belongs at /etc/afkfleet/server.toml. A deployment that runs the
# server with another --config path must override HEALTHCHECK with the same
# path. AFKFLEET_SERVER__… variables apply to both either way.
CMD ["serve", "--config", "/etc/afkfleet/server.toml"]
# The check asks /health/ready on loopback within its own 3 s timeout (ADR-0015).
HEALTHCHECK --interval=10s --timeout=5s --start-period=30s --retries=3 \
    CMD ["/usr/local/bin/afkfleet-server", "healthcheck", "--config", "/etc/afkfleet/server.toml"]
