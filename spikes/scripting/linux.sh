#!/bin/sh
# Runs the spike in a Linux container (rust:1-bookworm), so its numbers
# reflect the production platform. Only the spike's folder is mounted, read
# only, so the image's own stable toolchain builds it.
#
#   spikes/scripting/linux.sh <spike command> [args...]
#
# The named volumes afkfleet-p15-* cache the downloads and the build. Remove
# them with
#   docker volume rm afkfleet-p15-cargo afkfleet-p15-target
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
  --name afkfleet-p15-spike \
  --cpus "${SPIKE_CPUS:-4}" \
  --mount "type=bind,src=$root/spikes/scripting,dst=/src,readonly" \
  --mount type=volume,src=afkfleet-p15-cargo,dst=/usr/local/cargo/registry \
  --mount type=volume,src=afkfleet-p15-target,dst=/target \
  -e CARGO_TARGET_DIR=/target \
  -w /src \
  rust:1-bookworm \
  cargo run --release --locked --quiet -- "$@"
