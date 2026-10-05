# azalea spike (Phase 1)

Throwaway experiments that de-risk [azalea](https://github.com/azalea-rs/azalea) 0.16.0 (Minecraft 26.1) before `fleet-mc` is built. **This isn't a workspace member.** CI doesn't build it, and it's allowed to be messy. [`FINDINGS.md`](FINDINGS.md) records what each experiment showed; the decisions are in [ADR-0008](../../docs/adr/0008-azalea-integration.md).

## Running
Every experiment is a subcommand. Start the dev server first:

```sh
just mc-up                      # from the repository root
cd spikes/azalea
cargo run -- <command> [args]   # RUST_LOG=debug for azalea's own logs
```

Measurements (P1.6, P1.7, P1.9) run in Linux, the production platform. `linux.sh` runs the same commands in `rust:1-bookworm` on the dev server's Docker network:

```sh
spikes/azalea/linux.sh scale an-st 50     # from Git Bash or any POSIX shell
```

The first run downloads the pinned nightly and builds azalea; named volumes (`afkfleet-p1-*`) cache both.

| Command | Task | What it does |
|---|---|---|
| `join [secs]` | P1.1 | One offline bot (`AfkBot1`) joins with `Client::join` and stays online for `secs` seconds (default: until Ctrl-C) |
| `host [join\|builder\|custom] [bots]` | P1.2 | Bots on one MC host thread, handed to the multi-threaded runtime; calls from a worker thread; `exit()` and clean-up |
| `host-builder-exit [plain\|nested][-remote]` | P1.2 | Does `ClientBuilder::start()` return after `exit()`? (It can deadlock: see FINDINGS) |
| `exit-race [variant] [trials]` | P1.2 | How many `exit()` calls each trial needs until the event channel closes |
| `fail <scenario> [custom\|join] [version]` | P1.3 | One failure scenario: `closed`, `blackhole`, `blackhole-exit`, `unresolvable`, `kick`, `kick-noreason`, `ban`, `ban-ip`, `whitelist`, `duplicate`, `full`, `outdated [version]`, `idle`, `stop`, `kill`, `pause`, `death`. Some restart the dev server |
| `chat` | P1.4 | Two bots and RCON exchange chat, whispers, emotes, announcements, `tellraw` and commands; logs how each arrives |
| `actions` | P1.5 | Look, jump, sneak, swing, hotbar, use item, attack (with the raw-packet workaround), respawn, each checked via RCON |
| `idle-actions` | P1.5 | Six bots, one action type each, under `setidletimeout 1`: which actions keep a bot from being kicked |
| `fault <mode> [st]` | P1.6 | Panic or hang in a custom ECS system: `custom-panic`, `swarm-panic`, `custom-hang`, `starve`; `st` = single-threaded executor. Run in Linux (`linux.sh`) |
| `scale <model>[-st] <bots>` | P1.7 | Steady-state RSS, CPU, threads and fds for `a1`, `a4`, `an`, `s10`, `s`. Run in Linux |
| `account-check` | P1.8 | No real credentials: redacted `Debug`, a garbage-token join and an offline join against the local online-mode server (`compose.online.yaml`) |
| `fetch-token [file]`, `account-join [file] [--server addr] [--corrupt-token]` | P1.8 | The optional real-account test, run by the user only (see below) |

## P1.8: optional real-account test (run by the user only)
This checks `ExternalTokenAccount` with a real Minecraft token against a **local** online-mode server, so no public server is involved. The token never leaves your machine: it's written to the gitignored `secrets/` folder and never printed.

```sh
docker compose --file spikes/azalea/compose.online.yaml up --detach --wait   # from the repository root
cd spikes/azalea
cargo run -- fetch-token          # device-code sign-in; writes secrets/p1.8-account.txt
RUST_LOG=info cargo run -- account-join
```

- **Report** only the outcome: did the bot spawn, did the chat line go through, and any error *kind*. Don't paste the full output.
- **Clean up afterwards:**
  - `docker compose --file spikes/azalea/compose.online.yaml down --volumes`. The server's usercache and logs hold your account name.
  - Delete `secrets/p1.8-account.txt`.
- **Note:** `fetch-token` uses azalea-auth's default Microsoft client ID and keeps no cache file.
