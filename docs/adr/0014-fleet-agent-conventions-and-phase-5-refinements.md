# 0014. fleet-agent conventions and Phase 5 refinements

- **Status:** Accepted
- **Date:** 2026-10-09
- **Related:** Plan.md Phase 5 (P5.1–P5.7), §5, §6, §7.6, Appendices A and D; ADR-0010, ADR-0011, ADR-0013

## Context
Phase 5 builds `fleet-agent`, the first runnable product: `afkfleet-agent run --config agent.toml` runs a few offline-mode dev bots against a local server, survives server restarts and shuts down cleanly. It wires fleet-mc's `AzaleaConnector` to fleet-runtime's `Fleet`.

Phases 3 and 4 already handed it several decisions (ADR-0011, ADR-0013):
- fleet-mc's diagnostics become metrics
- the agent exits at the abandoned-thread limit, and when the supervisor ends unasked
- a `getrandom` seed for `Fleet::new`
- the Prometheus recorder is installed before `Fleet::new`
- the compose file's `stop_grace_period`
- `clashes_with` in the config check
- azalea's log levels

Before writing any code, the Phase 5 plan listed every question those left open. The user answered them on 2026-10-09, one at a time, and this ADR records the answers. The figment 0.10.19, garde 0.23.0, tracing-subscriber 0.3.23 and tracing-log 0.2.0 APIs, and std's on the pinned nightly, were checked in their sources. If a later group's review changes a decision, that group's PR amends this ADR.

## Decision
### Grouping
One branch and one PR per group, one commit per task, in this order:

| Group | Branch | Tasks |
|---|---|---|
| A | `p5/p5.1-p5.2-config-and-telemetry` | P5.1, P5.2. fleet-agent is a library only: no binary, no placeholder command |
| B | `p5/p5.3-p5.4-wiring-and-shutdown` | P5.3, P5.4. `afkfleet-agent run`, `just dev-agent`, `deploy/dev/agent.toml` |
| C | `p5/p5.5-p5.6-healthcheck-and-docker` | P5.5, P5.6. The healthcheck exists for the container |
| D | `p5/p5.7-e2e-and-wrap-up` | P5.7, the DoD demo and the phase wrap-up |

Each later group asks its own implementation-level questions in its session: tokio features, chrono's feature for the wall-clock anchor, the rustls provider, getrandom's §5 entry and the diagnostics sampling period (B); Docker base images and compose hardening (C); and where the demo goes in the Minecraft-version bump steps (D).

### Config (P5.1)
- **Loading.**
  - The file given by `--config` must exist at exactly that path. figment's `Toml::file` searches parent directories and treats a missing file as an empty config, so `load` checks `is_file()` first and reads the file with `Toml::file_exact`.
  - `AFKFLEET_AGENT__…` environment variables then override any key, with `__` between the parts (`Env::prefixed("AFKFLEET_AGENT__").split("__")`).
- **Unknown keys are errors,** in the file and in the environment, through `#[serde(deny_unknown_fields)]` on every raw struct.
  - No struct uses `#[serde(flatten)]`, because serde silently ignores `deny_unknown_fields` there. `[standalone]` and `[control_plane]` are two optional top-level fields that validation turns into one `AgentMode`.
  - Tests show that a misspelled key fails with its path, both in the TOML file and as an `AFKFLEET_AGENT__` variable.
- **Optional keys.**
  - Every `[runtime]` and `[retry]` key, and both sections, may be left out and take Appendix A's values. The runtime's come from `RuntimeConfig::default()` and `McConfig::default()`, so they can't drift apart.
  - `name` is required.
  - In each `[[standalone.bots]]` entry, `username`, `server` and `mode` are required; `conflict_texts` defaults to an empty list.
  - `heartbeat_file` defaults to `std::env::temp_dir().join("afkfleet-agent.alive")`, so `just dev-agent` works on Windows too. That's `/tmp` on Linux and in Docker.
- **Ranges** (garde), wide enough for any real setup and narrow enough to catch typos. The upper bounds also keep every duration far from overflowing chrono or tokio.

  | Key | Range |
  |---|---|
  | `max_bots` | 1–1000 |
  | `watchdog_timeout_secs`, `packet_liveness_timeout_secs` | 5–600 |
  | `connect_timeout_secs` | 5–300 |
  | `max_abandoned_threads` | 1–100 |
  | `shutdown_timeout_secs` | 1–300 |
  | `base_delay_secs` | 1–3600 |
  | `max_delay_secs` | 1–86 400, plus fleet-core's rule: at least twice the base |
  | `stable_after_secs` | **60**–86 400 |
  | `circuit_failures` | 1–1000 |
  | `circuit_window_secs`, `circuit_cooldown_secs` | 1–86 400 |

  `stable_after_secs` starts at a minute *(the user's decision)*. A stable session resets the attempt counter and records a breaker success, so a tiny value would let a server that kicks the bot a few seconds after every join cause endless reconnects at the base delay, with the breaker never opening. The reason sits next to the range in the code.
- **`name`** is the new `fleet_core::value::AgentName`, because P10 sends it to the server (Appendix D's `agents.name`).
  - It has 1–64 characters: ASCII letters, digits, `-`, `_` and `.`.
  - It starts with a letter or digit, so `.`, `..` and hidden-file names like `.agent` are refused *(the user's addition)*: the name is meant to be safe in file names.
  - The case is kept. `AgentNameError` is `InvalidLength`, `InvalidChar` or `InvalidStart`, checked in that order.
- **`heartbeat_file`** must be an absolute path, since the healthcheck runs in another process and possibly another directory.
- **`[control_plane]`** has its four Appendix A keys, typed and non-empty. Exactly one of `[standalone]` and `[control_plane]` must be present; neither or both is an error. The URL and the files are checked in P10, when the client exists, and `run` refuses managed mode until then (group B).
- **Standalone bots.**
  - At least 1 and at most `max_bots`, so `Fleet::apply` can't answer `AtCapacity` at startup. The count is checked only when `max_bots` itself is valid.
  - Two entries whose accounts clash are refused with `BotAccount::clashes_with` (decided in ADR-0013). Only valid names are compared, and the later entry gets the error: `standalone.bots[2].username: clashes with standalone.bots[0] (names compare ignoring case)`.
  - **Offline only, by construction:** an entry only has a `username`, which becomes `BotAccount::Offline`.
  - An entry has no `BotId`; group B mints one at every start.
- **Modes by name.** `fleet_core::mode::ModePreset { Afk, Farm }` has `ALL`, `name()`, `definition()` and `FromStr`, which takes exact lowercase names only. `UnknownPresetError`'s message lists the valid names from `ALL` ("unknown mode preset; expected one of: afk, farm") and never the input *(the user's addition)*. P11.1 can hang the presets' fixed IDs on the enum.
- **Errors.**
  - **Parse errors stop at the first:** bad TOML, a wrong type, or an unknown key; nothing can be checked after them.
    - figment's error is converted at once into a `ParseError { key, source, problem }` and never kept or printed: its own text has a profile prefix (`default.runtime…`), and for environment variables a key that isn't the variable's name.
    - The source is the file's path, or the variable's name rebuilt from the key.
    - A wrong type may show the value it found (no secrets live in the config).
    - A TOML syntax error keeps only its first line, the position: the rest quotes the file, which could hold a conflict text.
  - **Validation reports every problem at once,** sorted by key, one per line with its key path (`standalone.bots[1].username: …`), and never echoes a value.
    - To make that possible, texts are read as plain `String`s, and required keys as `Option`s. They're converted with the core constructors (`AgentName`, `McUsername`, `ServerAddress`, `ModePreset`, `ConflictTexts`) in the validation pass, not through serde's `try_from`, which stops at the first bad value *(the user's note)*.
    - A check that depends on a key with a problem of its own is skipped, so each mistake is reported once. `RetryPolicy::try_new` and `CircuitPolicy::try_new` run only when nothing under `retry.` is out of range.
    - `ConflictTexts` reports only its first bad entry per bot, with its index.
- **Known limits of environment variables** (figment):
  - A value that reads as a number or a boolean can't fill a text key. `AFKFLEET_AGENT__NAME=123` fails as a wrong type; quote it: `AFKFLEET_AGENT__NAME='"123"'`.
  - `[[standalone.bots]]` can only be replaced as a whole, with `[{…}]` syntax.

### Telemetry (P5.2)
- **A new `[log]` section** in Appendix A: `format = "json" | "pretty"` (default `json`, the safe production choice) and `filter` (EnvFilter syntax, default `info`). `AFKFLEET_AGENT__LOG__…` overrides them like any key, and `RUST_LOG` has no effect. `deploy/dev/agent.toml` sets `pretty`. Logs go to stdout.
- **JSON** uses `flatten_event(true)`: timestamp (UTC, RFC 3339), level, target and the event's fields at the top level, plus `span` and `spans`, so a bot's lines carry its `bot_id`. `run` enters a root span `agent{agent=…}`.
  - Since flattened fields share the line's keys, CLAUDE.md's logging rules forbid fields named `timestamp`, `level`, `target`, `message` (except tracing's own), `span`, `spans` or `fields`, and span fields named `name` *(the user's addition)*.
- **Pretty** is tracing-subscriber's single-line `Full` format, with ANSI colors only when stdout is a terminal.
- **azalea's levels.** azalea's targets stay at `warn` unless the operator names them, and `azalea_auth` is capped at `info` (ADR-0011). The layer's filter is `(EnvFilter ∧ azalea cap ∧ azalea_auth cap) ∨ panic target`:
  - **EnvFilter** gets a default for azalea in front of the operator's directives unless the operator has a *plain* `azalea` directive (no span, no fields). That's the only directive with the same specificity, where the order would decide (EnvFilter matches targets by prefix and prefers the longer target, then a span).
    - *(found in the build, the user's decision)* The default is `warn`, or the operator's global level if that's lower (`off` when there's none), so it only ever lowers azalea. A plain `azalea=warn` was more specific than the global level, so `filter = "off"` or `"error"` still let azalea's warnings through.
  - **The azalea cap** (a `Targets` filter, *the user's decision*) holds every `azalea…` target at `warn`, unless some operator directive names that target. The cap then takes the most verbose level the directives naming it set, so nothing depends on order. A target-free span directive such as `[mc_session]=debug`, fleet-mc's session span, can't lift azalea by accident.
  - **The `azalea_auth` cap** (a `Targets` filter) keeps `azalea_auth` at `info`, whatever the filter names.
  - **Panic reports** (target `afkfleet::panic`) always get through, whatever the filter says *(the user's decision)*. The default panic hook is replaced, so a filter without a global level, `off` or `panic=off` would otherwise drop them silently.
  - **A startup warning,** once, when an operator directive names an azalea target above `warn` *(the user's addition)*: azalea's kick rendering at info or below can be crashed or slowed by a hostile server (ADR-0011).
- **The panic hook** replaces the default one, so no extra stderr text breaks the JSON lines. It logs one `error` event with the location, the thread's name and the payload.
  - **The payload is untrusted** *(the user's decision)*. Only dependencies panic (our code is linted against it), and their messages can carry server text, which security rule 7 treats as untrusted. azalea's rendering can also grow huge (ADR-0011).
    - So it goes through fleet-core's new `text::sanitize_untrusted(raw, 1024)`, the chat sanitizer with line breaks kept, after which each line break becomes ` | `. The result is one line that can't forge a log line in pretty mode.
    - It's cut at 1024 characters, with ` [truncated]` appended when cut.
  - A `backtrace` field appears only when `RUST_BACKTRACE` asks for one.
- **The `log` bridge.** reqwest, rustls and hickory log through the `log` crate, which `try_init` bridges into tracing. tracing-log 0.2.0 dispatches each record with its real target, so filters and caps apply to it. A test proves it, with `log` 0.4.34 as a new fleet-agent dev-dependency.

- *(as built, group A)*:
  - **Shape.** `telemetry::init(&LogConfig)` installs the global subscriber with the `log` bridge, then logs the startup warning and installs the panic hook; a second call is `TelemetryError::AlreadyInstalled`. `telemetry::layer(config, ansi, writer)` builds the layer, so tests write into a buffer. `config::LogFilter` reads each directive with tracing-subscriber's own `Directive` parser and refuses a bad one by its index, without echoing it.
  - **EnvFilter's span directives** match the span's target as well as the event's, so `azalea[x]=debug` means azalea's own spans named `x`. Inside such a span, EnvFilter enables every target at that level; the caps still hold.
  - **Panic reports** use the target `afkfleet::panic`, which no crate's prefix shares, with the fields `thread`, `location`, `payload` and `backtrace`.
  - **fleet-core's `text` module** becomes public with only `sanitize_untrusted` and `UntrustedText`; the chat sanitizer's own items stay private.
  - **Tests** run the real layer in JSON into a buffer: 11 filters against 5 probe targets, panic reports, the startup warning, the JSON shape and the pretty format. `tests/telemetry_init.rs` and `tests/panic_hook.rs` each hold one test, since `init` sets process-wide state. Everything was red against stubs first.

### Wiring and shutdown (P5.3, P5.4; group B)
- **`run` is generic and tested on fakes.** The library's `run` is generic over the connector, a small agent-side `HostDiagnostics` trait (fleet-mc's five numbers) and a shutdown future.
  - main.rs passes `AzaleaConnector` and the real signals.
  - Tests use fleet-testkit's `FakeConnector`, a fake diagnostics source and a future the test triggers, on paused time, so the exits at the abandoned-thread limit and at the supervisor's end are tested.
- **Bot IDs.** Each standalone bot gets a random `BotId::new_v7(anchor, getrandom bytes)` at every start, which is fine since standalone persists nothing. One `info` line per bot logs its new ID with its username, server and mode, so logs, which carry only the `bot_id`, can be matched to the config after a restart *(the user's addition)*.
- **Metrics.** `PrometheusBuilder::install_recorder()` only, with no listener and no port. P12.4 adds the endpoint and its key.
- **Exit codes** *(the user's decision)*. They're listed in the README and in `--help`:

  | Code | Meaning |
  |---|---|
  | 0 | Clean shutdown after a signal, even if some bots had to be aborted; the report is logged |
  | 1 | Startup error: config, telemetry, `FleetSetupError` |
  | 2 | clap's own usage errors, left as they are |
  | 3 | The abandoned-thread limit was reached, after shutting down |
  | 4 | The supervisor ended without a requested shutdown, or didn't end when asked *(group B, the user's decision)* |

- **Fleet events.** In standalone mode, one subscriber task logs `ChatReceived` and `ModeChatSent` at `debug`, and `Lagged` at `debug` with its count. Nothing else, since the actor already logs every state change.
- **Signals.** A signal calls `Fleet::shutdown(shutdown_timeout)`, which returns the `ShutdownReport` for the log. Cancelling the token gives no report.
  - A second signal only logs that the shutdown is already running; it's bounded at `shutdown_timeout` plus the 5 s reply timeout.
  - The signals are SIGTERM and SIGINT on Linux, and Ctrl+C and Ctrl+Break on Windows.

- *(as built, group B: P5.3; the user answered group B's questions on 2026-10-09)*:
  - **Shape.**
    - The binary is `afkfleet-agent`. `fleet_agent::cli` holds its clap structs; `run --config <PATH>` is required, so no stray `agent.toml` in the working directory is picked up. `run --help` lists the exit codes, and the top-level help points there.
    - `run::run(RunParts { config, connector, diagnostics, anchor, seed }) -> Outcome { exit, report }`. `Exit::code()` gives the process's code.
    - `diagnostics::HostDiagnostics::sample() -> HostSample` reads fleet-mc's five numbers. `AzaleaConnector` implements it; it owns its `McHostPool`, so main.rs passes one connector as both.
  - **main.rs, in order:**
    1. `Cli::try_parse`, handling clap's error itself (its own output and code), so group C can map `healthcheck`'s usage errors to 1.
    2. `config::load`, before any async runtime exists, since it reads a file.
    3. `telemetry::init`. Errors up to here go to stderr as one plain message, written with `writeln!` (no `#[expect(clippy::print_stderr)]`), and exit 1.
    4. The aws-lc-rs rustls provider, installed now although Phase 5 uses no TLS, so the rule holds from the first binary.
    5. The Prometheus recorder (`install_recorder()`, no exporter features). Its handle isn't kept: nothing serves it until P12.4, and without histograms it needs no upkeep.
    6. A multi-threaded tokio runtime, so one slow actor can't stall the others or the signal handling.
    7. `run`, with `Utc::now()` as the anchor (chrono's `now`, which adds no crates; `clock` would add local time zones) and `getrandom::u64()` as the seed.
    8. `Runtime::shutdown_timeout(1 s)`, so nothing stuck keeps the process alive once the code is decided.
    9. The last line, "the agent stopped", with `exit_code`, and `stopped`, `aborted` and `crashed` when a report exists, on every exit path once logging is up.

    Every startup error after `telemetry::init` is one `error` event, "the agent can't start", with the error.
  - **No anyhow:** main.rs maps typed errors to exit codes itself, so anyhow would only wrap and unwrap them. §5 notes that it arrives with the first `main.rs` that uses it.
  - **One deadline per shutdown** *(the user's decision)*: shutdown_timeout + reply_timeout from the shutdown's start, the bound above.
    - On `Ok(report)`, the supervisor's task is joined by the deadline.
    - On `Busy` or `ShuttingDown` (both immediate), a `warn`, then the supervisor's token is cancelled, which shuts the fleet down within shutdown_timeout without a report, and its task is joined by the deadline.
    - `TimedOut` has used up the deadline: the token is cancelled and the run exits 4 at once.
    - A supervisor that doesn't end by the deadline, or panics, is an `error` and exits 4.
    - A bot the fleet refuses at startup (after the config's checks, only `Busy`, `ShuttingDown` or `TimedOut` can happen) is logged at `error` with its ID and username; the fleet shuts down the same way, and the run exits 1, or 4 if the supervisor doesn't end.
    - A fresh wait after an error would double the worst case to 30 s.
  - **Diagnostics** are sampled at once and every 5 s *(the user's decision)*, a constant, with missed ticks skipped. A thread counts as abandoned only after its 5 s shutdown timeout anyway. Each sample sets the metrics, with no labels *(the user's names)*:

    | Metric | Kind |
    |---|---|
    | `afkfleet_mc_abandoned_threads_total` | counter, from fleet-mc's total (`absolute()`) |
    | `afkfleet_mc_dropped_chat_total` | counter: incoming chat dropped because a session's event queue was full |
    | `afkfleet_mc_ignored_action_bar_total` | counter |
    | `afkfleet_mc_host_threads` | gauge: running host threads, abandoned ones that still run included |
    | `afkfleet_mc_worlds` | gauge |

    They're described and registered at 0 right after `Fleet::new`. Counts convert without `as`: counters saturate at `u64::MAX`, gauges at `u32::MAX`, which `f64` holds exactly.
  - **Logs.**
    - `run` enters the root span `agent{agent=…}` itself, and spawns the supervisor in it, so every bot's span nests in it. main.rs logs its own lines in a span of the same name, outside `run`. fleet-mc's `mc_session` spans are created on host threads and stay roots.
    - At `info`: one "starting a standalone bot" per bot (`bot_id`, `username`, `server`, `mode`), "the agent is running" (`bots`) once every bot is applied, and "the agent stopped".
    - A `JoinError` is never logged: its text carries the panic payload, which is untrusted and already logged, sanitized, by the panic hook. The lines carry `panicked` instead.
    - The event task's chat text is a plain string field, never `%`, so both formats escape its line breaks and a chat line can't forge a log line.
  - **Testing `finish`** uses a real `Fleet` whose `Supervisor` a test task runs, holds without running, drops, or panics, instead of a trait for the tests only *(the user's decision)*. Each case checks the exit code and the paused time at which it ends.
  - **The binary's tests** (`tests/cli.rs`) start the real `afkfleet-agent`. Each run has its own deadline and a guard that kills it, since nextest's `ci` profile has no slow-timeout. Threads drain its pipes, and its config and an absolute heartbeat file live in a directory of their own under Cargo's temp directory for tests *(the user's safeguards)*.
  - **`deploy/dev/agent.toml`** runs AfkBot1 and AfkBot2 on `afk` and AfkBot3 on `farm` against `127.0.0.1:25565`: compose binds that address, and `localhost` may resolve to `::1` first on Windows. It's for `just dev-agent` on the host only; the compose agent (group C) reaches the server as `minecraft:25565` with its own config.
- *(as built, group B: P5.4)*:
  - **A signal source instead of "a shutdown future"** *(the user's decision)*. One future can't show a second signal, so `run` is generic over `signals::ShutdownSignals`, whose `recv() -> Signal` is cancel-safe.
    - `OsSignals::install()` registers SIGTERM and SIGINT (Unix) or Ctrl+C and Ctrl+Break (Windows). main.rs calls it inside the runtime, which tokio's signal registration needs, and before the fleet starts, so an early signal isn't lost; an error is a startup error.
    - When a tokio stream ends, `recv` waits forever instead of making a signal up or spinning; the tests' channel fake does the same once its sender is dropped.
    - A platform that's neither Unix nor Windows fails to build with a `compile_error!`.
  - **Exit code 0** (`Exit::Stopped`) follows the first signal, even when bots had to be aborted. P5.3's fallbacks keep it: `Busy` or `ShuttingDown` still end with 0 once the supervisor ends, while `TimedOut` or a hung supervisor end with 4.
  - **The log** has "shutting down" with `signal` for the first signal, and "already shutting down" for every later one, both while the fleet shuts down and while its supervisor is awaited. The deadline never moves.
  - **Tests.** The red step failed on assertions against stubs that compiled, *(the user's correction)*: the stub source installed the real handlers but never yielded, so a test's own SIGTERM was swallowed instead of ending the test process. `tests/signals.rs` is one sequential test (SIGTERM, then SIGINT) in its own binary, since two tests signalling their own process would see each other's signals under plain `cargo test`. It and the binary's SIGTERM test are Unix only, since Windows' console events can't be sent without unsafe code; they ran red and green in a Linux container.

### Healthcheck and Docker (P5.5, P5.6; group C)
- **The heartbeat** touches the file every 10 s, and only when `fleet.snapshot_all()` returns `Ok` *(the user's decision)*. `Busy`, `TimedOut` and `ShuttingDown` never touch it: a hung supervisor leaves each timed-out call in its queue, and after about 64 heartbeats every call answers `Busy` at once, which would hide the hang. A test covers that case.
- **`healthcheck --config <path>`** loads the same config, so the two can't disagree. The file is stale when it's missing or its modification time is 30 s old or more (3 missed beats).
  - It exits only 0 (healthy) or 1 (unhealthy), with one line saying why, because Docker reserves 2 for healthchecks.
  - clap's usage errors for `healthcheck` map to 1 too, through `try_parse`; `--help` and `--version` stay 0.
- **P5.6** as planned: cargo-chef, a pinned nightly builder, `gcr.io/distroless/cc-debian12:nonroot`, a `HEALTHCHECK` through the subcommand, and a `stop_grace_period` above `shutdown_timeout` (ADR-0013).
  - *(group B, the user's decision)* The agent's worst case is shutdown_timeout + reply_timeout, plus main.rs's 1 s runtime-shutdown bound, so the grace period must be above that: **20 s** for the 10 s default. Otherwise Docker's SIGKILL replaces exit code 4 and the last line.

- *(as built, group C: P5.5; the user answered group C's questions on 2026-10-09)*:
  - **The heartbeat is a port.** `heartbeat::Heartbeat::touch()` is a fifth generic of `RunParts`, so the run's tests count beats on paused time; a file's modification time follows the wall clock, which paused time doesn't move. main.rs passes `FileHeartbeat` on `heartbeat_file`.
  - **A touch** opens the file with `create(true).write(true).truncate(false)` (Windows needs write access to set the time) and calls `File::set_modified(now)`, in `spawn_blocking`, since the rules keep blocking file IO off the runtime. The file stays empty and is never truncated or deleted *(the user's decision)*: a wrong `heartbeat_file` can't destroy data, and deleting the file would. After a container restart, an old file can look fresh only until the new agent's first beat.
  - **The beat is a task of its own** *(the user's decision)*, owned by the run with a child token, so a slow `snapshot_all` (up to the 5 s reply timeout) never delays the signals or the diagnostics. It starts once "the agent is running", beats at once and then every 10 s, and awaits each touch before the next beat, so at most one is in flight. The run cancels it when the shutdown starts.
  - **Failures are logged once per streak** *(the user's decisions)*: a `warn` when the fleet stops answering or the file can't be touched (with `file` and the IO error), and an `info` when it works again. The agent keeps running, and the container turns unhealthy. Since the first beat comes right after the start, a wrong path shows up at once.
  - **The check's output** *(the user's decisions)*: one line on stdout, healthy or not. stderr is left for errors that keep the check from running, a config that can't be loaded (the same text and exit 1 as `run`) or a usage error. A usage error counts as `healthcheck`'s when the first argument is `healthcheck`.
  - **A time in the future** *(the user's decision)* counts as age 0 up to 30 s ahead, so small clock corrections never fail the check. Further ahead is unhealthy with a line of its own: a healthy agent's next beat fixes the time within 10 s and Docker's retries absorb that check, while a hung agent can't look healthy for the length of a big backward jump.
- *(as built, group C: P5.6)*:
  - **Debian 13, not 12** *(found in the plan's review, the user's decision)*. Under distroless's SUPPORT_POLICY.md, the Debian 12 `cc` images reached end of life in September 2026 (GoogleContainerTools/distroless#2129) and get no more updates, so the runtime is `gcr.io/distroless/cc-debian13:nonroot`. The builder is `rust:1.99.0-slim-trixie`, the same Debian, so the binary's glibc matches. Plan.md P5.6 keeps the original text, with a deviation note. *(Corrected in group D: group C gave distroless's README as the reason, but the README still has a Debian 12 table.)*
  - **Production runs on linux/arm64** *(the user's requirement)*; development and CI run on x86_64.
    - Both base images are pinned by tag and **multi-arch index digest**, the top-level digest of `docker buildx imagetools inspect`, never one platform's. Nothing in the Dockerfile names an architecture, so the same file builds natively on both.
    - deny.toml's `[graph] targets` gains `aarch64-unknown-linux-gnu`. That checks more of the graph and weakens nothing; `cargo deny check` stays clean, with no `ring` on aarch64.
    - The arm64 builder stage was built under QEMU as a smoke check. P12.1 builds the whole image for arm64.
  - **The builder** *(the user's decisions)* installs cargo-chef with `cargo install --locked` on the image's stable toolchain, then copies `rust-toolchain.toml` alone and runs `rustup toolchain install`, two cached layers before any source. The toolchain file stays the nightly's only pin, so ADR-0003's bump steps don't change. Everything builds with `--locked`.
  - **The runtime** runs as distroless's nonroot user, with static OCI labels only (title, description, source, licenses). The config belongs at `/etc/afkfleet/agent.toml`, the path of both the `CMD` and the `HEALTHCHECK` (10 s interval, 5 s timeout, 30 s start period, 3 retries).
  - **`.dockerignore` is an allowlist** *(the user's decision)*: only the workspace's manifests, `rust-toolchain.toml`, `.cargo/config.toml` and `crates/` enter the context. `.gitignore`'s secret patterns are then excluded again at any depth, since the last matching rule wins and `crates/` is allowed whole. A build listing the context showed only those files.
  - **The compose agent** *(the user's decisions)*:
    - It has its own config, `deploy/dev/agent.compose.toml`: AfkBot4–6, so `just dev-agent`'s AfkBot1–3 can run beside it, at `minecraft:25565`, with JSON logs.
    - It starts once the server is healthy, so its bots don't use up retries, or open the breaker, while the server's first start builds the world.
    - `stop_grace_period: 20s`; `restart: on-failure`, since exit codes 3 and 4 should restart it and a stop by a signal (0) shouldn't.
    - `read_only` with a 1 MiB tmpfs at `/tmp` for the heartbeat file, `cap_drop: [ALL]`, `no-new-privileges`, 512 MiB and 1 CPU (about twice ADR-0008's 50-bot measurement).
    - `image: afkfleet-agent:dev` with `pull_policy: build`, so compose never pulls that name from a registry.
    - `just stack-up` builds it and waits until both services are healthy.
  - **A fast test guards the files** *(the user's decisions)*. `tests/deploy.rs` loads both dev configs and checks that the grace period outlasts the compose config's worst-case shutdown, with `RUNTIME_SHUTDOWN` now public in `run` for it. It also checks that compose mounts the config where the Dockerfile's `CMD` and `HEALTHCHECK` read it. The approved `in_jail` helper moved to `tests/common/jail.rs`, so it's still one `#[expect]`.
  - **No CI job builds the image on every PR** *(the user's decision)*. Group D's PR, and every later PR that changes the Dockerfile, `.dockerignore`, `Cargo.lock` or `rust-toolchain.toml`, runs the "Slow tests" workflow on its branch before it's merged.

### End-to-end test and demo (P5.7, DoD; group D)
- **`crates/fleet-agent/tests/slow_compose.rs`**, a `slow_` test, drives `docker compose` through `std::process::Command`, with its own project name, so it never touches the `afkfleet-dev` stack. It reads the agent's JSON logs for state changes and RCON `list` for who's online, and checks the exit code and the `ShutdownReport`.
  - `just test-slow` builds the agent image first, since a cold build can outlast the slow profile's 10 minutes.
  - The test always cleans up *(the user's addition)*: `down -v` for its own project, once before it starts and in a guard that also runs when the test panics.
- **`just demo-agent`** has its logic in a script under `scripts/`. It starts the compose stack with 5 bots, restarts the MC container once around the middle, stops after 1 h, and prints a summary:
  - warn and error lines by target and count
  - the state-change timeline
  - reconnect times after the restart
  - the shutdown report

  The AI runs it in group D and puts its output in the PR; the user can re-run it, for example after a Minecraft-version bump.

- *(as built, group D: P5.7; the user answered group D's questions on 2026-10-09)*:
  - **Isolation** *(the user's decision)*. `deploy/compose.dev.yaml` publishes the server on the fixed `127.0.0.1:25565`, so a second project would clash with `just mc-up`. The new `deploy/compose.isolated.yaml` removes that port (`ports: !reset []`); neither the test nor the demo needs it, since the agent reaches the server over the compose network and RCON goes through `exec`. `!reset` needs Compose 2.24, so both check `docker compose version` first and stop with a clear message rather than a YAML error.
  - **Five bots** *(the user's decision)*. `agent.compose.toml` gains AfkBot7 and AfkBot8 on `afk`; AfkBot6 stays on `farm`. `just stack-up`, the test and the demo then run the same config, and the demo also runs the farm preset for an hour.
  - **A graceful restart** *(the user's decisions)*:
    - The restart is `docker compose restart --no-deps minecraft`. itzg's runner turns SIGTERM into `stop`.
    - compose.dev.yaml gives the server `stop_grace_period: 60s`, since Docker's default 10 s could kill it mid-stop. The bots would then see reset sockets, which azalea logs as an ERROR. `tests/deploy.rs` asserts at least 30 s, so removing it fails `just check`, not only the weekly slow run.
    - The final teardown uses `down -v --timeout 10`, so the 60 s don't eat the teardown allowance.
    - The agent's `depends_on` has no `restart: true`, and the test asserts the agent wasn't restarted (its StartedAt and restart count, and one "the agent is running"). The reconnects must come from the agent's own retry logic.
  - **"Within the policy"** *(the user's decisions)*:
    - **Every wait** from `Backoff { attempt: n }` to the next `AwaitingSession` lies in `RetryPolicy::bounds(n)`, with 100 ms of slack below (the times are taken when lines are written, not when the timer starts) and 1 s above. A reconnect storm still misses the lower bound by seconds.
    - **Every bot is Online again by a deadline** from the server's healthy moment: bounds(n).max for its current backoff, one more failed attempt with its backoff (connect + bounds(n+1).max), the attempt that works (connect), plus 1 s. Docker probes the health only every 5 s, so an attempt in flight then can still fail. The per-gap check already holds every wait to the policy; the deadline only proves each bot comes back.
    - **The healthy moment** is the end of the first good probe after the server's new StartedAt. It's read from `docker inspect` as soon as it appears, because Docker keeps only five probes. It runs on the Docker VM's clock, like the agent's log, so no host clock enters a judgment.
    - **Step 1's deadline** has the same shape from "the agent is running": connect + bounds(1).max + connect + 1 s.
  - **The budget** *(the user's decisions)*. Before each deadline, the test asserts elapsed + deadline + the agent's stop_grace_period + 60 s of teardown ≤ the slow profile's limit. The limit is read from `.config/nextest.toml` (period × terminate-after), so nextest can't kill it mid-cleanup.
  - **A clean stop** *(the user's decisions)*:
    - exit code 0, and the report as the last line (stopped 5, aborted 0, crashed 0)
    - no WARN or ERROR from "shutting down" on
    - RCON's list empty within 5 s, polled, since the server drops a player on its next tick

    It doesn't check the server's log: its wording can change with a version bump, and a socket closed with unread data gets a TCP reset on Linux.
  - **The whole log** *(the user's decision)*. No ERROR, and every WARN must match an entry of `deploy/dev/stack-checks.json` by target and message prefix. Failures print every offending line in full.
  - **`stack-checks.json`** *(the user's decisions)* is shared by the test (`include_str!`) and the demo script (`readFileSync`), so their lists and numbers can't drift. It holds:
    - the expected warnings, each with a `why`
    - the bounds(n) table up to the first capped window (the last entry applies to every later attempt), so the script never re-implements fleet-core's jitter formula
    - the connect timeout and the agent's grace period

    `tests/deploy.rs` compares every entry with `bounds(n)` of the loaded `agent.compose.toml`, and checks that the table ends at the cap.
  - **The image** *(the user's decisions)*:
    - `just test-slow` first pulls the server image (a cold pull would eat into a timed test), then runs `docker compose build agent`.
    - It then sets `AFKFLEET_E2E_IMAGE_BUILT=1` through a `$`-parameter. The justfile's rule and CLAUDE.md's now allow `export` or `$`-parameters, since just exports both.
    - Without the variable, the test stops at once, so it never tests an image left from another branch. It starts the stack with `up --wait --no-build`.
  - **No vacuous pass** *(the user's addition)*. Every check asserts it saw data: five bots in the log, at least one wait per bot after the restart, a healthy probe, and the bots on RCON's list before the stop.
  - **Red first** *(the user's decision)*. The helpers' 52 unit tests, and the scenario on stub parsers, failed on assertions first. One uncommitted run with `stop_signal: SIGKILL` failed on exit code 137, which shows step 3 catches a real bad stop.
  - **Found in the runs:** the restart took about 3 s, and the bots saw `ConnectionClosed` rather than the "Server closed" kick, so azalea's "Got disconnect packet" didn't show. It stays in the list for a slower stop.
  - **Connection resets** *(found by the demo's first run; the user's decisions)*. The whole-log check allows azalea's reset ERROR only during the restart, and no more often than sessions closed there. The rule and its reasons are under the demo below; the test and the script share it through `stack-checks.json`'s `expected_restart_errors`.

- *(as built, group D: the demo; the user answered group D's questions on 2026-10-09)*:
  - **`scripts/demo-agent.mjs`** uses Node's built-ins only, like the other scripts. Its parsing and judging are exported pure functions tested in `scripts/demo-agent.test.mjs`. Driving Docker isn't unit-tested; a short run (`just demo-agent 8`) exercises it.
  - **The same stack and the same numbers as the e2e test:**
    - the project `afkfleet-demo` with `compose.isolated.yaml`
    - the expected warnings and the retry windows from `stack-checks.json`
    - the reconnect deadline's formula, from the server's healthy moment read from `docker inspect`
    - `restart --no-deps minecraft`
    - `down -v --timeout 10` at the end, also after Ctrl+C or an error, once the logs are saved
  - **A verdict** *(the user's decision)*: PASS (0) or FAIL (1), listing each failed criterion with its offending lines:
    - an ERROR, except the connection resets a server restart may cause (below)
    - a warning nobody expects, or a line that isn't the agent's JSON
    - a bot not Online again by its deadline
    - an agent restart: its StartedAt, its restart count, or a second "the agent is running"
    - a shutdown that isn't exit 0 with aborted 0 and crashed 0

    A re-run after a version bump then gives a clear answer.
  - **The stats** *(the user's decisions)*:
    - One streamed `docker stats --format "{{json .}}"` gives the agent's peak, mean and p95 memory and CPU, with each peak's time relative to the restart. Each JSON object is cut out of its line, since the stream redraws the terminal even into a pipe.
    - The numbers are reported, never judged: they're P12.2's measurement, and an OOM kill would already fail as a restart.
    - The summary also says where they were measured: `docker info`'s architecture, CPUs, OS and memory. The CPU model comes from `lscpu` in the server's container, else from `/proc/cpuinfo`, else "unknown"; ARM has no "model name" in cpuinfo. Production runs on arm64, so memory carries over roughly, but CPU percentages don't.
  - **Optional minutes** *(the user's decisions)*:
    - `just demo-agent 8` checks the script before the hour.
    - A run shorter than its own checks need fails with exit 2 before it starts: half the run must hold attempt 3's reconnect deadline plus the agent's stop, derived from `stack-checks.json` (7 minutes with the defaults).
    - Once the real attempts are known after the restart, a run that can't hold them plus the stop ends as "too short to judge" (exit 2), not as a misleading FAIL.
    - A run under 60 minutes is marked "not the DoD run".
  - **Raw data** *(the user's decisions)* goes under `target/demo-agent/<UTC time>/`, which is gitignored: `agent.log`, `minecraft.log`, `stats.jsonl` and `summary.txt`. A pull request gets only `summary.txt`, never excerpts from the raw logs. `minecraft.log` carries each bot's container IP and port, and CLAUDE.md's rule against IP addresses in git applies to the public pull request too. The summary has no host path or host name.
  - **The version bump** *(the user's decision)*: ADR-0003's verify step now runs `just demo-agent` (1 h), and the bump's pull request includes its `summary.txt`. P12.6's runbook carries the step.
  - **Connection resets during a restart** *(found in the first run, the user's decisions)*:
    - **What happens.** azalea logs a TCP reset at ERROR (`azalea_client::plugins::connection`, "Error reading packet from Client: IoError { … ConnectionReset …"). A server that closes a socket with unread client data sends a TCP reset, and in vanilla's shutdown that can arrive before the "Server closed" kick. In the 8-minute run, 2 bots got the kick, and 3 were closed without it, one of them with a reset. The agent recovered as designed: `ConnectionClosed`, a backoff, and back Online in about 25 s.
    - **The rule.** It's random, so "no ERROR anywhere" would make the e2e test and the demo flaky. `stack-checks.json`'s new `expected_restart_errors` holds that one entry. Both accept it only:
      - between the agent's last line before the restart and the server's healthy moment, by the Docker VM's clock, never the host's
      - no more often than bot sessions ended there with `ConnectionClosed`. azalea's line carries no bot ID, but each reset ends one live session, and the bot logs that with its ID. Together with "every bot Online again by its deadline", a second reset for a bot, or a reset for a bot that doesn't come back, still fails.

      Every other ERROR, and this one outside the restart, still fails.
    - **Alternatives considered:** allowing it anywhere (a real network fault would pass), dropping the target in the agent's log filter (it would hide azalea's packet-decode errors too), and pairing each ERROR with the closest bot by time (it can mis-pair bots that disconnect in the same millisecond).

### Dependencies
Approved by the user, all in Plan.md §5:
- `figment` 0.10.19 with `toml` and `env`, and `test` (`Jail`) in tests. It brings `toml` 0.8 beside the graph's newer one, a duplicate-version warning only.
- `garde` 0.23.0 with `derive` (it has no default features).
- `serde` with `derive`.
- `tracing-subscriber` with `env-filter` and `json`, its default features kept.
- `log` 0.4.34 as a fleet-agent dev-dependency.
- *(group B)*:
  - `clap` 4.6.7 with `derive`.
  - `getrandom` 0.4.3, with no features, for the seed and the bot IDs' bytes. §5's "Used in" gains the agent.
  - `metrics-exporter-prometheus` 0.18.3 with no features until P12.4. Its metrics-util dependency always enables `storage`, which brings rand 0.9 (`thread_rng`) and getrandom 0.3 into normal dependencies; both were already in the lockfile, and nothing uses them for secrets. One `metrics` 0.24.x in the tree keeps the runtime and the exporter on the same recorder.
  - `rustls` 0.23.45 with its default features (aws-lc-rs), already in the lockfile through reqwest.
  - Already declared, newly used by the agent: tokio (`rt`, `rt-multi-thread`, `macros`, `sync`, `time`; `test-util` in tests), tokio-util (no features), chrono (`now`), the `metrics` facade (the agent records the diagnostics itself), and fleet-testkit as a dev-dependency.
- *(group C)*: no new crates. The agent image's base images are `rust:1.99.0-slim-trixie` (builder) and `gcr.io/distroless/cc-debian13:nonroot` (runtime), both pinned by multi-arch index digest, and the builder installs cargo-chef 0.1.78 (§5's tools).
- *(group D)*: no new crates. The compose e2e test uses only fleet-agent's existing dependencies and dev-dependencies (serde, serde_json, figment, chrono, rstest).
- fleet-agent depends on fleet-core, fleet-runtime and fleet-mc (§4).
- No crate-local `clippy.toml`: the root one applies.
- **One lint exception in tests** *(the user's approval)*. figment's `Jail` fixes its closure's error type to `figment::Error`, which is larger than `clippy::result_large_err` allows. So every test goes through one helper, `in_jail`, whose closure carries `#[expect(clippy::result_large_err, reason = …)]`. Production code isn't affected: `load` converts figment's error into a small boxed one.

## Consequences
- **Easier:**
  - A typo in `agent.toml` or in an environment variable fails the start with its key, instead of silently keeping a default.
  - Fixing a config takes one run, since every problem is listed at once.
  - fleet-core's `ModePreset`, `AgentName` and `sanitize_untrusted` are reused by P10's proto and server code.
- **Harder:**
  - fleet-core grows three public items.
  - The config reads texts as `String`s and converts them by hand, instead of through serde, to report all problems at once.
  - Numeric-looking texts from environment variables must be quoted.
- **Later phases inherit:**
  - **P10:** check `[control_plane]`'s URL and files; `AgentName` goes into the `Hello` and `agents.name`; the managed provider uses the same config.
  - **P11.1:** fixed IDs for `ModePreset`.
  - **P12.4:** the metrics endpoint and its key.
  - **P12.1** *(group C)*: build the agent image for linux/arm64, natively on the server or on GitHub's `ubuntu-24.04-arm` runner. The weekly audit scans the built image (e.g. Trivy), since `cargo deny` covers only crates. `created`, `revision` and `version` labels come through build args once CI publishes the image.
  - **P12.2** *(group C)*: production is linux/arm64, and the agent's memory and CPU limits come from group D's measured demo numbers, not from the dev values.

## Alternatives considered
- **More, smaller groups (5), or fewer, larger ones (3).** Five would split P5.3 from P5.4, which share the run loop. Three would put the healthcheck and Docker in one oversized PR.
- **A `check-config` subcommand in group A.** It's a command the plan doesn't list, and it would pull clap and anyhow in early.
- **An optional config file** (config from env only). `[[standalone.bots]]` can't be written well in env, and a missing file would silently give an empty config.
- **Ignoring, or warning about, unknown keys.** A typo would silently keep the default.
- **Every key required.** The file would be longer than needed, with no gain in safety; the defaults come from the runtime's own.
- **Ranges only as wide as the types allow.** A typo (`max_bots = 5000`) would pass.
- **A relative `heartbeat_file`.** The healthcheck process might resolve it elsewhere.
- **Fully validating `[control_plane]` now** (garde's `url` feature), or refusing it until P10. The first adds a dependency for a client that doesn't exist yet; the second makes the mutual-exclusivity rule untestable until P10.
- **Allowing 0 bots, or no upper check.** An idle standalone agent is a mistake, and too many would fail at startup with `AtCapacity`.
- **Bot IDs from the entry's position, or minted in the config.** Positional IDs change when entries move, and minting in the config needs the clock and randomness in group A.
- **`ModeDefinition::preset(name)`, or case-insensitive names.** A lookup function keeps the names in one match only; `AFK` would be a second spelling for the same thing.
- **Stopping at the first validation error, or quoting values.** One run per mistake; values could carry conflict texts into logs.
- **Env-only logging (`RUST_LOG`), a pretty default, or a format derived from the mode.** Logging would differ from every other setting, production would need an extra key, or the format would be implicit.
- **The operator's filter replacing azalea's default, or a hard cap at `warn`.** A broad `debug` would turn on azalea's info kick rendering; a hard cap would make azalea impossible to debug.
- **Relying on directive order** (`azalea=warn` first, the operator's last wins). The same-target case would hinge on the order, and a span directive would still lift azalea.
- **Logging the panic payload as is, or not at all.** Untrusted server text could reach the logs unbounded; without the payload, a panic is hard to diagnose.
- **A `log`-bridge test left out.** A future change to the bridge could silently bypass the caps.
- **Raising `large-error-threshold` for the agent, or dropping `Jail`.** The first weakens the lint for production code; the second would pass the environment into `load` by hand.

The B–D alternatives (metrics endpoint now, 0/1 exit codes, cancelling the token on a signal, a second signal that forces the exit, logging chat at `info`, a heartbeat that only proves the process is alive, a `--file` healthcheck, testcontainers' compose support, a demo the user runs alone) are recorded with their groups' plans.
