# 0012. azalea's advisory and license exceptions

- **Status:** Accepted
- **Date:** 2026-10-07
- **Related:** Plan.md Phase 3 (group B), §5; ADR-0003, ADR-0009, ADR-0011; `deny.toml`

## Context
azalea 0.16.0 (ADR-0003) entered the workspace in Phase 3, group B, as fleet-mc's dependency. Its dependency graph fails `cargo deny` in five places:
- **Three advisories:**
  - RUSTSEC-2023-0071 in `rsa` 0.10.0-rc.18 (the Marvin timing side channel). No fixed version exists.
  - RUSTSEC-2026-0118 in `hickory-proto` 0.25.2 (an NSEC3 validation loop).
  - RUSTSEC-2026-0119 in `hickory-proto` 0.25.2 (quadratic name compression).
- **Two licenses** that aren't on the allowlist: `minecraft_folder_path` 0.1.2 (Unlicense) and `socks5-impl` 0.8.7 (GPL-3.0-or-later).

Weakening a `deny.toml` rule needs the user's approval and an ADR (CLAUDE.md, security rule 10).
- **The approval.** The user approved all five on 2026-10-06. ADR-0011 ("Supply chain") records that decision and why each item is acceptable.
- **This ADR** records the `deny.toml` entries themselves. They land together with azalea, because cargo-deny warns about an ignore that matches nothing.

**The check.** When azalea was added (2026-10-07), `cargo deny check` reported exactly these five items, and nothing else besides duplicate-version warnings, which are allowed (`multiple-versions = "warn"`).

## Decision
### Advisories: `[advisories] ignore`, each with a reason
| Advisory | Why it's acceptable | Removal trigger |
|---|---|---|
| RUSTSEC-2023-0071 (`rsa`) | The attack recovers a key from RSA *decryption* timings, and azalea never decrypts with RSA. It parses Mojang's chat-signing key and signs our own chat with the blinded `sign_with_rng`. The login handshake only *encrypts*, with the server's public key. Only online accounts sign, only our own rate-limited chat, and the key is Mojang's short-lived chat-signing key, not an account credential | No fix exists. Re-checked at every azalea bump |
| RUSTSEC-2026-0118 (`hickory-proto`) | The affected NSEC3 code only compiles with hickory's DNSSEC features. azalea enables `system-config` and `tokio`, so hickory-proto gets only `std`, `tokio` and `futures-io`: the code isn't compiled | Removed at the next azalea bump; azalea `main` already uses hickory 0.26.1 |
| RUSTSEC-2026-0119 (`hickory-proto`) | Only *encoding* is quadratic. We encode only our own one-question queries for a validated `ServerAddress`; responses are decoded. So the quadratic path is unreachable. SRV records keep working | Removed at the next azalea bump |

### Licenses: per-crate `[licenses] exceptions`, pinned to the version
- `minecraft_folder_path@0.1.2`: `Unlicense`
- `socks5-impl@0.8.7`: `GPL-3.0-or-later`, our own license

Both are unconditional azalea dependencies, and both licenses are GPL-compatible. The exceptions name the exact version, so a version change during an azalea bump fails `cargo deny` until it's reviewed again.

### Re-checking
Every azalea bump (ADR-0003's procedure, step 5):
- re-checks the `rsa` advisory
- removes the two hickory ignores once azalea uses hickory-proto ≥ 0.26.1
- updates the license exceptions to the new versions after a review

## Consequences
- **Easier:** azalea builds in the workspace, and `cargo deny` stays strict everywhere else. Each exception carries its reason and its removal trigger in `deny.toml` itself.
- **Harder:** every azalea bump has to revisit five entries. A new advisory against `rsa` or `hickory-proto` still fails the build, because the ignores name single advisory IDs.
- **Accepted risk:** the `rsa` advisory has no fix in sight. The analysis above holds only as long as azalea never decrypts with RSA, so each bump checks that again.

## Alternatives considered
- **Blocking Phase 3 until upstream fixes the advisories.** No `rsa` fix is in sight, so it would stall the project.
- **Resolving addresses with the OS resolver, so hickory never runs.** Servers that rely on SRV records would then need an explicit host and port.
- **Adding Unlicense and GPL-3.0-or-later to the global allowlist.** It's broader than these two crates need, and it would let future dependencies in without review.
- **Exceptions by crate name only, without the version.** A bump could then bring in a relicensed version unnoticed.
