# Scripting spike (Phase 15's planning)

> The findings are recorded in [ADR-0036](../../docs/adr/0036-scripted-modes.md), "What was measured". This code isn't maintained and isn't built by CI. It stays here as evidence, and as a quick way to check Rhai's behaviour again at a bump.

Throwaway measurements for scripted modes, with [Rhai](https://rhai.rs/) 1.27.0 and the features the ADR proposes. **This isn't a workspace member,** and it's allowed to be messy.

## Running
```sh
cd spikes/scripting
cargo run --release -- all            # everything the ADR quotes
cargo run --release -- facts          # what works, with Rhai's own words for what doesn't
cargo run --release -- numbers        # operations a second, a hook's cost, the auction scan
cargo run --release -- ticks 200 10   # hooks that run to their end: 200 bots, 10 s
cargo run --release -- threads 200 10 # a thread a bot with wait(): 200 bots, 10 s
```

The same in Linux, the production platform, in `rust:1-bookworm` (needs Docker):

```sh
spikes/scripting/linux.sh all 200 10  # from Git Bash or any POSIX shell
```

## What it holds
| File | What it is |
|---|---|
| `src/main.rs` | A sandboxed engine with a small part of the proposed API, and the four commands |
| `scripts/minecart_farm.rhai` | The minecart farm written with helper functions: the check that game objects and state work inside a script's own functions |
| `scripts/auction_scan.rhai` | A 54-slot auction page scanned for its cheapest listing, the price read by a native helper |
| `scripts/auction_scan_plain.rhai` | The same scan with Rhai's own string functions, to show what the helper saves |

## Reading the output
- `FACT ok` and `FACT FAIL` lines: a `FAIL` isn't a failed test. It shows what Rhai refuses and the text it refuses it with, which the ADR quotes.
- `NUMBER` lines: one thread, a release build.
- `TICKS` and `THREADS` lines: both time models. The memory and CPU readings are the process's own, as the load test reads them (ADR-0032); the CPU counter's step is about 16 ms on Windows.
