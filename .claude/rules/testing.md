---
paths:
  - "**/tests/**"
  - "**/*.test.ts"
  - "**/*.test.tsx"
  - "**/*.test.mjs"
  - "apps/desktop/e2e/**"
  - "crates/fleet-testkit/**"
  - "crates/fleet-integration/**"
  - ".config/nextest.toml"
---

# Testing details
Moved from `CLAUDE.md` word for word; it points here. CLAUDE.md keeps the testing conventions most tasks need.

## Real-time exceptions to "never a real sleep"
- **The one exception: the four `manual_` tests that wait on Microsoft.** fleet-server's three wait Microsoft's real poll interval: the adapter's two checks at most 8 minutes each, the link flow's at most 4 minutes for each of its two codes, reading the flow's status every 2 s (ADR-0018). fleet-integration's Phase 10 check reads its link's status every 2 s for at most 4 minutes, then the bot every 2 s for at most 90 s until it's Online and 60 s until it's offline, beside the container's start of at most 300 s (the user's choice, ADR-0019, U22). Its steps stop at 520 s, and the whole check, teardown included, ends within 580 s of the profile's 600 s.
- **fleet-integration's one wait helper** (`Logs::until`, real time: the cases run sqlx and real TLS) wakes at every log line and re-checks in-process state at most every 100 ms, always bounded by `PATIENCE`; never a fixed delay before an assertion. Its proxy pauses 100 ms after an accept error, as serve's own port does, so a repeating error can't spin it (the user's choice, ADR-0019, U23). `Logs::within(bound)` is the same wait with a shorter bound than `PATIENCE`, for a case that tries something again, and says whether it came; the slow on-respawn case re-reads the container through RCON at each of its wakes (the user's choice, ADR-0020, U9).
- **Tests over real sockets run on real time** (the Microsoft adapter against its local fakes), with short bounds and never a sleep; paused time would fire their timeouts early (ADR-0018).
- **The desktop keychain's tests run on real time** for the same reason: its blocking calls run on Tauri's runtime, outside the test's. Short bounds (100 ms for a call that must time out, 5 s for everything else), never a sleep (ADR-0017).

## Doubles and fixtures
- Config tests run inside figment's `Jail` through `fleet_testkit::jail::in_jail`, which carries the workspace's one approved `#[expect(clippy::result_large_err)]`. Don't write a second helper (ADR-0015).
- Tests of what a log layer prints write into a `fleet_testkit::log_buffer::LogBuffer`. Checks that a secret never reaches a log use `fleet_testkit::log_capture` instead.
- HTTP:
  - **Handler tests:** use `tower::ServiceExt::oneshot`.
  - **Full-stack tests:** run `fleet-client` against an in-process server on `127.0.0.1:0`.
