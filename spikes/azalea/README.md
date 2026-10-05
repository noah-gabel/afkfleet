# azalea spike (Phase 1)

Throwaway experiments that de-risk [azalea](https://github.com/azalea-rs/azalea) 0.16.0 (Minecraft 26.1) before `fleet-mc` is built. **This isn't a workspace member.** CI doesn't build it, and it's allowed to be messy. [`FINDINGS.md`](FINDINGS.md) records what each experiment showed; the decisions are in [ADR-0008](../../docs/adr/0008-azalea-integration.md).

## Running
Every experiment is a subcommand. Start the dev server first:

```sh
just mc-up                      # from the repository root
cd spikes/azalea
cargo run -- <command> [args]   # RUST_LOG=debug for azalea's own logs
```

| Command | Task | What it does |
|---|---|---|
| `join [secs]` | P1.1 | One offline bot (`AfkBot1`) joins with `Client::join` and stays online for `secs` seconds (default: until Ctrl-C) |
| `host [join\|builder\|custom] [bots]` | P1.2 | Bots on one MC host thread, handed to the multi-threaded runtime; calls from a worker thread; `exit()` and clean-up |
| `host-builder-exit [plain\|nested][-remote]` | P1.2 | Does `ClientBuilder::start()` return after `exit()`? (It can deadlock: see FINDINGS) |
| `exit-race [variant] [trials]` | P1.2 | How many `exit()` calls each trial needs until the event channel closes |
