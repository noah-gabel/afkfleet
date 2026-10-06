# 0003. azalea and a pinned nightly toolchain

- **Status:** Accepted
- **Date:** 2026-10-05
- **Related:** Plan.md §1 (assumptions), §5, task P0.2; ADR-0008 (azalea integration, P1.10)

## Context
The bots speak the Minecraft Java protocol through [azalea](https://github.com/azalea-rs/azalea). Three facts constrain us:
- azalea needs **nightly Rust**. Upstream pins only an undated `nightly`, so it can break whenever nightly changes.
- Each azalea release supports **exactly one Minecraft version**. Servers on another version need a translation plugin (ViaVersion / ViaBackwards).
- At the time of this decision (2026-10-05):

  | Source | Version | Minecraft |
  |---|---|---|
  | Current Minecraft release (2026-09-15) | | 26.3 |
  | azalea on crates.io (released 2026-03-28) | `0.16.0+mc26.1` | 26.1 |
  | azalea `main` (git, unreleased) | | 26.2 |
  | azalea 26.3 support | a draft pull request with merge conflicts | 26.3 |

The project needs a reproducible toolchain: the same compiler for every developer, CI and the Docker builder.

## Decision
- **azalea:** `azalea` and `azalea-auth` are pinned to `=0.16.0` from crates.io, so **Minecraft 26.1**.
  - The pin is exact (`=`) because semver ignores the `+mc26.1` build metadata: `^0.16.0` could silently move to a release for another Minecraft version.
  - crates.io is the only allowed source (`deny.toml`).
- **Toolchain:** `rust-toolchain.toml` pins **`nightly-2026-08-21`** (rustc 1.100.0-nightly, `8925ea358`, 2026-08-20).
  - Components: `rustfmt`, `clippy`, `llvm-tools-preview`; profile `minimal`.
- **How the date was chosen:**
  1. The first candidate, `nightly-2026-10-01`, fails to compile azalea 0.16.0: `azalea-core`'s const-generic `FixedBitSet<N>` hits `E0284` ("cannot infer the value of the constant").
  2. A bisect between 2026-03-28 (azalea's release) and 2026-10-01 counted only that exact error as a failure. It found:
     - **last good:** `nightly-2026-08-21`
     - **first bad:** `nightly-2026-08-22`
  3. `spikes/toolchain-check/` (only `azalea = "=0.16.0"`) builds with the pinned nightly on Windows (MSVC, NASM enforced) and on Linux (`rust:1-bookworm`).
  4. All three components exist for this date on both targets.
- **Code that doesn't depend on azalea** must also build on stable Rust. The `stable-check` CI job enforces this, using stable 1.99.0 at this decision.

### Bump procedure (Minecraft version upgrade)
One pull request changes all of these together, never one alone:
1. **azalea:** pick the new azalea release on crates.io and note its Minecraft version.
2. **Pins:** update `azalea`/`azalea-auth` in Plan.md §5, `Cargo.toml` (once used) and `spikes/toolchain-check/Cargo.toml`.
3. **Nightly:** start with the newest nightly whose `rustfmt`, `clippy` and `llvm-tools-preview` exist for `x86_64-pc-windows-msvc` and `x86_64-unknown-linux-gnu`. If azalea fails to build, bisect back to the newest nightly that works, counting only the azalea error as "bad".
4. **Test server:** update `VERSION` (and, if needed, the pinned `itzg/minecraft-server` image) in `deploy/compose.dev.yaml` (from P1.1).
5. **Verify:**
   - build `spikes/toolchain-check` on Windows and in a Linux container
   - run `cargo deny` against its graph
   - run `just ci` and `just test-slow`
6. **Record:** update this ADR, or supersede it, and `docs/runbook.md` (from P12).

## Consequences
- **Server versions.** Target servers must run Minecraft 26.1, or translate with ViaVersion/ViaBackwards (needed for 26.2 and 26.3 servers). Moving to a newer Minecraft waits for an azalea release.
- **Frozen toolchain.** The toolchain stays at 2026-08-21 until the next bump. Compiler fixes after that date aren't available. Nothing in the plan depends on them.
- **Problems in azalea 0.16.0's dependency graph that P3 must resolve** before azalea enters the workspace. Found by a dry run of `cargo deny` against the spike:
  - **Licenses** not on the allowlist, both GPL-compatible:
    - `minecraft_folder_path` (Unlicense)
    - `socks5-impl` (GPL-3.0-or-later)
  - **Advisories:**
    - `hickory-proto` 0.25.2: RUSTSEC-2026-0118 and RUSTSEC-2026-0119, fixed only in 0.26.1
    - `rsa`: RUSTSEC-2023-0071 (Marvin), no fix released
  - Each one needs the user's approval (license exception or advisory ignore, with an ADR), or an azalea release that fixes it.
- **Bans and sources already pass.** azalea's HTTP client uses rustls with aws-lc-rs, so it fits ADR-0009's single provider.

## Alternatives considered
- **azalea `main` via a git revision (Minecraft 26.2).** A newer Minecraft, but an unreleased git source that `deny.toml` would have to allow. Rejected by the user in favor of a released crate.
- **azalea's 26.3 branch.** Matches the current Minecraft release, but it's a draft pull request with merge conflicts.
- **The newest nightly.** It doesn't compile azalea 0.16.0.
