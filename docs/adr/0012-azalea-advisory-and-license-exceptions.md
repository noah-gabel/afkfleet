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
- removes the eleven hickory ignores once azalea uses hickory 0.26.2 or newer (two until 2026-11-11, at hickory-proto ≥ 0.26.1; six until 2026-11-12; the amendments below)
- updates the license exceptions to the new versions after a review

### Phase 9, group B: azalea-auth only through azalea *(the user's decision, [ADR-0018](0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md))*
- **A wrapper ban.** `[bans] deny` gains `{ crate = "azalea-auth", wrappers = ["azalea", "azalea-client", "azalea-inventory", "azalea-protocol"] }`: the four crates that depend on azalea-auth in azalea 0.16.0, checked in Cargo.lock. A direct dependency in any member fails `just deny`, shown with a temporary one in fleet-server ("bans FAILED", `error[banned]`). The desktop pass reports the wrapper as unused, like its other not-encountered entries.
- **RUSTSEC-2023-0071 stays where it was.** fleet-server's Microsoft adapter sends its own requests, so neither azalea-auth nor `rsa` reaches it (`cargo tree -p fleet-server -e normal,build,dev --target all -i rsa` finds nothing). The analysis above still covers fleet-mc and the agent, the only crates that build `rsa`; the server never touches certificates or RSA.
- **At a bump,** a new azalea crate that depends on azalea-auth is named by cargo-deny, and the wrapper list is updated together with a note in ADR-0018 (ADR-0003's procedure).

### 2026-11-11: four more `hickory-proto` advisories *(decided under [ADR-0025](0025-a-simpler-workflow.md))*
**What happened.** RUSTSEC-2026-0487, 0489, 0490 and 0491 were published against `hickory-proto` before 0.26.2, and `just deny` failed on `main` and on every open pull request. Nothing in the workspace had changed.

**No fix can be taken.** 0.25.2 is the last 0.25 release; the fixes are in 0.26.2. azalea-protocol 0.16.0 asks for `hickory-resolver` 0.25, and 0.16.0 is still azalea's newest release, so only an azalea bump (a Minecraft-version bump, `docs/releasing.md`) brings 0.26.

| Advisory | Why it's acceptable | Removal trigger |
|---|---|---|
| RUSTSEC-2026-0487 (a malformed `alpn` value in an SVCB or HTTPS record's text panics) | In 0.25.2 the code is `serialize/txt/rdata_parsers/svcb.rs`. `serialize::txt` is declared under `#[cfg(feature = "text-parsing")]`, and `cargo tree -i hickory-proto -e features --target all` shows only `std`, `tokio` and `futures-io`: the code isn't compiled. Nothing here parses zone files or record text either: azalea's resolver only looks names up | Removed once azalea uses hickory-proto ≥ 0.26.2 |
| RUSTSEC-2026-0489 (a parameter value of one quote character panics) | The same file and the same feature: not compiled | The same |
| RUSTSEC-2026-0490 (the zone file lexer panics on a token of 4096 characters) | `serialize/txt/zone_lex.rs`, under the same feature: not compiled | The same |
| RUSTSEC-2026-0491 (decoding a DNS message reserves memory for as many records as its header claims, 65,535 at most, about 18 MB by the upstream advisory, before it reads one) | **This one is compiled and runs:** the agent decodes the answers to its own lookups. Who can send one: the resolver the agent's host is configured with (or Google's, azalea's fallback), or someone who can spoof its answers on that path. A Minecraft server's own name server can't: a recursive resolver writes its own header. What they get: one reservation per answer, freed when the decode fails; the lookup then fails, the bot reads `unresolvable` and backs off. A bot looks a name up only when it connects, the connect timeout bounds each lookup (ADR-0011), and whoever answers the agent's DNS can already keep every bot offline by answering nothing | The same |

**Not done:** resolving with the OS resolver instead (the alternative below, rejected for SRV records); a `[patch]` to a fork of hickory 0.25 with the fix, which would put unreviewed code where a reviewed release was.

### 2026-11-12: five advisories about DNSSEC validation *(decided under [ADR-0025](0025-a-simpler-workflow.md))*
**What happened.** RUSTSEC-2026-0504, 0505, 0511 and 0512 were published against `hickory-proto`, and RUSTSEC-2026-0514 against `hickory-resolver`, a day after the four above. `just deny` failed again on `main` and on the open pull request, with nothing changed in the workspace.

**No fix can be taken,** for the same reason: `hickory-proto` has no fixed 0.25 release (from 0.26 the validation code lives in `hickory-net`, fixed in 0.26.2), `hickory-resolver`'s fix is 0.26.2, and azalea-protocol 0.16.0 asks for `hickory-resolver` 0.25.

**All five are about DNSSEC validation, which isn't compiled here.** Checked in the 0.25.2 sources on docs.rs and with `cargo tree -e features --target all`:
- `hickory-proto` declares its `dnssec` module under `#[cfg(any(feature = "dnssec-aws-lc-rs", feature = "dnssec-ring"))]`. The features in this build are `std`, `tokio` and `futures-io`.
- `hickory-resolver` wraps its client in the validating handle only under `#[cfg(feature = "__dnssec")]`, which only its `dnssec-ring` and `dnssec-aws-lc-rs` features turn on; without it, setting `validate` logs a warning and the plain client answers. The features in this build are `default`, `system-config` and `tokio`.

| Advisory | What it is | Why it's acceptable | Removal trigger |
|---|---|---|---|
| RUSTSEC-2026-0504 | The last NSEC3 record of a zone's chain compared the wrong way round, so it "covers" nearly every name | `hickory-proto`'s `dnssec` module: not compiled | Removed once azalea uses hickory 0.26.2 or newer |
| RUSTSEC-2026-0505 | NSEC3 owner names matched by their first label only, not checked against the zone | The same module: not compiled | The same |
| RUSTSEC-2026-0511 | An NXDOMAIN taken as proven for a name that is an empty non-terminal | The same module: not compiled | The same |
| RUSTSEC-2026-0512 | An unrelated signed record set taken as the validated answer | The same module: not compiled | The same |
| RUSTSEC-2026-0514 | `Resolver::lookup` and `lookup_ip` answered `Ok` with records whose validation had failed | Only with validation on, which needs the features above: here no lookup is validated, and nothing takes one as validated. azalea looks a server's name up as any client without DNSSEC does | The same |

**What it doesn't change:** the agent never relied on DNSSEC. Whoever can forge a DNS answer on the agent's path could always point a bot at another address; these five make that no easier.

**Not done:** the same two as above.

## Consequences
- **Easier:** azalea builds in the workspace, and `cargo deny` stays strict everywhere else. Each exception carries its reason and its removal trigger in `deny.toml` itself.
- **Harder:** every azalea bump has to revisit five entries (nine since 2026-11-11, fourteen since 2026-11-12). A new advisory against `rsa` or `hickory-proto` still fails the build, because the ignores name single advisory IDs.
- **Accepted risk:** the `rsa` advisory has no fix in sight. The analysis above holds only as long as azalea never decrypts with RSA, so each bump checks that again.

## Alternatives considered
- **Blocking Phase 3 until upstream fixes the advisories.** No `rsa` fix is in sight, so it would stall the project.
- **Resolving addresses with the OS resolver, so hickory never runs.** Servers that rely on SRV records would then need an explicit host and port.
- **Adding Unlicense and GPL-3.0-or-later to the global allowlist.** It's broader than these two crates need, and it would let future dependencies in without review.
- **Exceptions by crate name only, without the version.** A bump could then bring in a relicensed version unnoticed.
