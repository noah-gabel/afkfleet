# 0030. The fuzz targets

- **Status:** Accepted
- **Date:** 2026-11-11
- **Related:** Plan.md Phase 13 (P13.2), §4, §5; ADR-0010 (the value objects' proptests), ADR-0012 (license exceptions), ADR-0015 (`testkit-check`), ADR-0019 (S25, group G), ADR-0020 (S80, R138), ADR-0022 (S9), ADR-0025, ADR-0027 (group C)

## Context
P13.2 asks for fuzz targets over what the fleet reads from outside: the value-object parsers, the chat sanitizer, mode definitions, the proto to core conversions and the vault's envelope parser. Earlier phases added to the list: `read_hello` and the codec's check of a hello's bots (ADR-0019, S25), `IpNet`'s parser and `PrivateTargets::problems` (group G), `ChatFormats::try_new` and `sender_of` (ADR-0020, S80, R138), and the agent's metrics responder (ADR-0022, S9).

Plan.md §5 named the tools: cargo-fuzz with `libfuzzer-sys` and `arbitrary`. Two things about them shaped the design:
- **cargo-fuzz builds with sanitizer and coverage flags, on nightly, in a crate of its own.** Nothing of that may reach the workspace's build or its lockfile.
- **Its own documentation disagrees about Windows:** the book says it works through MSVC's AddressSanitizer, the README says Unix only. Tried here on the development PC: it builds; a target exits with `0xc0000135` unless MSVC's `clang_rt.asan_dynamic-x86_64.dll` is on its PATH; with that folder on PATH it runs. Without a sanitizer (`--sanitizer none`) it doesn't link on MSVC.

Everything below is decided under ADR-0025.

## Decision

### Two crates
- **`crates/fleet-fuzz`, a workspace member, holds every target's body:** a plain `pub fn(&[u8])` that panics when a property breaks. Its own tests run every target over its seeds, the empty input, proptest's bytes and text, and its seeds with a stretch of bytes replaced. So `just check` and CI's test jobs hold every property on Windows and Linux, with 1000 cases in CI, and the targets can't rot between fuzz runs.
- **`fuzz/`, the cargo-fuzz crate, is a workspace of its own** (`exclude` in the root manifest), with its own `Cargo.lock` and `libfuzzer-sys` as its one dependency besides fleet-fuzz. Each target there is one line that calls its body by name. `just scripts-test` fails when `fuzz/fuzz_targets/`, `fuzz/Cargo.toml`'s binaries and fleet-fuzz's `TARGETS` disagree.
- **fleet-fuzz is test tooling:** nothing but `fuzz/` depends on it. It depends on what its targets read, fleet-server and fleet-agent among them, so it isn't one of `stable_crates`. It implements its own counting `SecureRandom` for the vault target: fleet-testkit may only ever be a dev-dependency (ADR-0015).
- **The targets take bytes and cut them themselves** (text as lossy UTF-8, lists a line each, two parts at the first zero byte), so `arbitrary` isn't used; it stays libfuzzer-sys's own dependency.

### The targets and what each holds
None may panic. Beyond that:

| Target | Reads | Holds |
|---|---|---|
| `value_objects` | Every string parser of fleet-core: the nine value objects, IDs, `IpNet`, `ModeName`, `CertFingerprint`, `AuditAction`, the small enums; `ChatMessage`, `EnrollmentBundle`, `NewPassword`, `TotpCode`, the allowlist and the conflict texts | A value that's accepted and can be written reads back as itself |
| `chat_sanitizer` | `sanitize_untrusted` at any cap, and a received system line | Within its cap; no control character or `§`, and no line break in a log line; sanitizing again changes nothing |
| `chat_formats` | `ChatFormats::try_new` over any list | An error as shown is plain ASCII and no longer than its own wording with its numbers, so it can't hold an entry's text; accepted formats read back as themselves |
| `chat_sender` | `sender_of` over any valid formats and any line | A sender is in the characters a format may read; a player's line never has one |
| `private_targets` | `IpNet`'s parser, `PrivateTargets::problems` | No problem exactly when the list is accepted |
| `mode_definition` | A stored definition's JSON; the API's create and update requests | A definition reads back as itself; a request garde accepts has a name `ModeName` takes and steps `definition()` takes, which the handlers assume |
| `agent_message` | What the server reads: the codec's check, `AgentMessage`, `read_hello`, `EnrollRequest` | A hello the check lets through lists at most 1000 bots; a message the core accepts reads back from what the core sends |
| `server_message` | What an agent reads: `ServerMessage`, `EnrollResponse`, `RenewResponse` | No panic |
| `vault_envelope` | `Vault::open` and `key_id_of` over any bytes; a sealed secret with one byte changed | Nothing the vault didn't seal opens; both call the same envelopes malformed; a secret opens only unchanged, for its record and purpose |
| `agent_metrics` | The agent's metrics endpoint, through its own `serve` over an in-memory pipe, time paused | The answer is nothing, or one whole response with one of the responder's four statuses; the series only for a `GET` of `/metrics` |

**Each property's check has a test that shows it failing** on a value built to break it (a type that writes what its parser refuses, a hello over the bound that was let through, an answer with another status), so a target that silently checks nothing fails the build.

### Seeds
**The seeds are built in code** (`fleet_fuzz::seeds`), from the real types: an encoded hello, a welcome, a valid mode, an envelope's header. A folder of binary files would go stale with the first wire change. fleet-fuzz's `seeds` example writes them into `fuzz/corpus/<target>/seed-<n>`, which `just fuzz` runs first; it only adds those files. `fuzz/corpus/` and `fuzz/artifacts/` are ignored by git.

### Where it runs
- **Every pull request:** fleet-fuzz's tests, in `just check` and CI's test jobs.
- **`just fuzz [target] [seconds]`:** the seeds, then `scripts/fuzz.mjs`, which runs `cargo fuzz run <target> -- -max_total_time=<seconds>` for one target or all. On Windows it asks vswhere for the folder of the AddressSanitizer runtime and puts it first on the target's PATH.
- **The Fuzz workflow:** weekly, on demand, and on pull requests that touch `fuzz/`, `crates/fleet-fuzz/`, the script or the workflow: `just fuzz all`, 300 s a target (60 s on a pull request). It isn't a required check: it builds the server and the agent with a sanitizer. A failing input is printed into the log as Base64 by a step of its own; nothing is uploaded.
- **A failing input is kept by the target itself,** as `fuzz/artifacts/<target>/failing-input`. libFuzzer saves a crash only when the process dies in a way it handles. A Rust panic ends a fuzz target through `abort`, which on Windows is a fast-fail: the target exits with `0xc0000409` and libFuzzer saves nothing, as the first long run here showed. So every target runs through `fleet_fuzz::kept::run`, which remembers its input, and a panic hook installed in the target's `init` writes that input to the path `scripts/fuzz.mjs` hands over in `AFKFLEET_FUZZ_KEEP_AT`, before libfuzzer-sys's own hook aborts. On Linux libFuzzer's `crash-<hash>` is there too.
- **A finding** becomes a regression test in the crate it broke, and a seed if it's a new kind of input.

### cargo-deny
**`just deny` gains a third pass, over `fuzz/Cargo.toml`.** Its lockfile is its own, so the first two passes never saw `libfuzzer-sys` or `arbitrary`. Like the desktop pass it allows an ignore that matches nothing (`-A advisory-not-detected`): its graph has no dev-dependency.

**`deny.toml` gains one license exception: `libfuzzer-sys@0.4.13` may be `NCSA`** (its license is `(MIT OR Apache-2.0) AND NCSA`, the last being libFuzzer's own). NCSA is OSI-approved and GPL-compatible. It's per crate and version, as ADR-0012's two are, and the crate is linked only into fuzz binaries, which are never shipped. It's listed under **Weakened** in the pull request.

### Versions
cargo-fuzz 0.13.2 (2026-06-09) and libfuzzer-sys 0.4.13 (2026-06-04) are still the newest, re-checked on 2026-11-11. `arbitrary` resolves to 1.5.0 (2026-10-06) in `fuzz/Cargo.lock`; Plan.md §5 had 1.4.2 for a direct use that never came.

## What the runs found
**No defect in the code under test.** On the development PC (Windows, with AddressSanitizer), on 2026-11-11:
- **fleet-fuzz's tests with 30,000 proptest cases** a property instead of 256: clean.
- **Every target for 60 s:** clean, 11.8 million inputs.
- **Every target for 300 s** (`just fuzz all 300`): clean, 36.5 million inputs, from 1.3 million (`agent_metrics`, which builds a runtime for each) to 7.1 million (`server_message`).

**Two findings about the fuzzing itself,** both from a first 300 s run that stopped at its third target:
- **A property of the target's own was wrong.** `chat_formats` checked that an error never contains an entry of 12 characters or more. Within a minute the fuzzer wrote an entry that spells a piece of the error's fixed wording ("chat format "), which the error then "contains". The error type holds only numbers, so the claim was true and its check wasn't. It now checks the error's shape: plain ASCII, and no longer than its wording with its numbers.
- **The failing input was gone.** The target had panicked and exited with `0xc0000409`, and `fuzz/artifacts/` was empty: see "A failing input is kept by the target itself" above. The check afterwards, with the sanitizer temporarily keeping `§`: the target fails within seconds and its four-byte input is in `failing-input`.

**Three temporary mutations of the code under test,** each restored by checksum, each caught by fleet-fuzz's own tests, without a fuzzer: the sanitizer keeping `§` (`chat_sanitizer`, on its seeds and on proptest's text), the codec letting 1001 bots through (the bound's edge test), the metrics responder answering the series to a `POST` (`agent_metrics`, on its seeds).

## Consequences
- **A new parser of untrusted input gets a target**, or joins one: a function, a line in `TARGETS`, seeds, a one-line file and a `[[bin]]`.
- **`fuzz/Cargo.lock` is a second lockfile.** It started as a copy of the workspace's, and cargo keeps it resolving the same path crates; a dependency bump updates it when the fuzz crate is next built, and the third deny pass reads it meanwhile.
- **A fuzz build is about 2.5 GB in `fuzz/target/`** on Windows, built in about four minutes after the workspace's own crates are cached.
- **The workflow's failing input is only in its log,** as Base64: decoded into a file, `cargo fuzz run <target> <file>` reproduces it.
- **Windows needs one more Visual Studio component** to fuzz locally; without it `just fuzz` says which.

## Alternatives considered
- **The bodies in `fuzz/` alone,** as cargo-fuzz lays a project out: the properties would then run only where cargo-fuzz does, weekly on Linux, and a target broken by a refactor would show a week later.
- **Committed seed files:** stale at the first change to a message or a format, and binary files in git.
- **`arbitrary`'s derived inputs** (a `Vec<String>` for the formats, a struct for the vault's input): another dependency in the workspace for what a split at a zero byte does, and inputs that no longer are what an attacker sends.
- **Fuzzing in a Linux container on the development PC:** it works everywhere Docker does, but its build would live in Docker's own disk, which doesn't shrink again; the native build is a folder under the repository that can simply be deleted.
- **The fuzz job on every pull request:** some twenty minutes of build for a run fleet-fuzz's tests already approximate.
- **Adding NCSA to the license allowlist:** broader than the one crate needs.
