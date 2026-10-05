#!/bin/sh
# Runs the spike in a Linux container (rust:1-bookworm), on the dev server's
# compose network, so measurements reflect the production platform
# (P1.6, P1.7, P1.9). Start the dev server first (`just mc-up`).
#
#   spikes/azalea/linux.sh <spike command> [args...]
#
# Environment: SPIKE_CPUS (default 4), RUST_LOG, MALLOC_ARENA_MAX.
# The first run downloads the pinned nightly and builds azalea (slow); the
# named volumes afkfleet-p1-* cache both. Remove them with
#   docker volume rm afkfleet-p1-cargo afkfleet-p1-rustup afkfleet-p1-target
set -eu

root=$(git rev-parse --show-toplevel)
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*)
    # Git Bash: give Docker a Windows path and stop MSYS from rewriting paths.
    root=$(cygpath -m "$root")
    export MSYS_NO_PATHCONV=1
    ;;
esac

exec docker run --rm --init \
  --name afkfleet-p1-spike \
  --network afkfleet-dev_default \
  --cpus "${SPIKE_CPUS:-4}" \
  --mount "type=bind,src=$root,dst=/src,readonly" \
  --mount type=volume,src=afkfleet-p1-cargo,dst=/usr/local/cargo/registry \
  --mount type=volume,src=afkfleet-p1-rustup,dst=/usr/local/rustup \
  --mount type=volume,src=afkfleet-p1-target,dst=/target \
  -e CARGO_TARGET_DIR=/target \
  -e RUST_LOG="${RUST_LOG:-info}" \
  -e MALLOC_ARENA_MAX \
  -e SPIKE_SERVER=minecraft:25565 \
  -w /src/spikes/azalea \
  rust:1-bookworm \
  cargo run --release --locked --quiet -- "$@"
