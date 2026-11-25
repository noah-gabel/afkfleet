# afkfleet's documents

Every document in this folder. Start with the repository's [README](../README.md); the roadmap and the design are in [`Plan.md`](../Plan.md), at the root.

## Guides
- [Using the afkfleet app](using-the-app.md), for friends: installing the app, connecting to a server, registering with an invite, 2FA, updates and removing it.
- [The runbook](runbook.md), for whoever hosts afkfleet on a Linux host with Docker: installing, settings, updates, backups and restores, the keys' copy, metrics, agents on other hosts, incident response and uninstalling.
- [Releasing afkfleet](releasing.md), for the maintainer: GitHub's setup, the updater's key, each release's steps and the Minecraft version bump.
- [Scripted modes: the script reference](scripting.md), for whoever writes a mode as a script: hooks, state, the game objects, limits, converting a step mode, and examples. Part 2, inventory and menus, is planned for Phase 16.
- [Developing afkfleet](development.md), for contributors: the architecture, a local fleet, the binaries' settings and the API, the commands, and the checks only you run with your own account.

## Security, design and checks
- [The threat model](threat-model.md): what afkfleet protects, from whom, and how, each mitigation with the tests behind it.
- [The ASVS Level 2 self-review](asvs-review.md): every Level 1 and 2 item of OWASP ASVS 5.0.0 with its verdict at v0.1.3, and the inventories it asks for: the cryptography, the logs, the ways in.
- [The design brief](design-brief.md): the desktop app's users, screens and constraints, the starting point of its design.
- [Phase 12's definition of done](dod/phase-12.md): the kit for checking a release on your own host, with a friend on another PC.

## Decisions
Each architecture decision record (ADR) holds one decision with its context, its consequences and the alternatives it rejected; [ADR-0001](adr/0001-use-adrs.md) says why. A new one starts from [the template](adr/0000-template.md), and an accepted one is never rewritten: a later ADR supersedes it.
- [0001. Record architecture decisions](adr/0001-use-adrs.md)
- [0002. Hexagonal architecture and crate layout](adr/0002-hexagonal-architecture-and-crate-layout.md)
- [0003. azalea and a pinned nightly toolchain](adr/0003-azalea-and-pinned-nightly.md)
- [0004. One tokio task per bot, azalea on MC host threads](adr/0004-bot-actors-and-mc-host-threads.md)
- [0005. SQLite via sqlx](adr/0005-sqlite-via-sqlx.md)
- [0006. Opaque tokens instead of JWT](adr/0006-opaque-tokens-instead-of-jwt.md)
- [0007. The desktop app talks to the server only from Rust](adr/0007-tauri-rust-side-api-proxy.md)
- [0008. azalea integration](adr/0008-azalea-integration.md)
- [0009. Phase 0 dependency and tooling conventions](adr/0009-phase-0-dependency-and-tooling-conventions.md)
- [0010. fleet-core conventions and Phase 2 refinements](adr/0010-fleet-core-conventions-and-phase-2-refinements.md)
- [0011. fleet-mc and fleet-testkit conventions and Phase 3 refinements](adr/0011-fleet-mc-and-fleet-testkit-conventions-and-phase-3-refinements.md)
- [0012. azalea's advisory and license exceptions](adr/0012-azalea-advisory-and-license-exceptions.md)
- [0013. fleet-runtime conventions and Phase 4 refinements](adr/0013-fleet-runtime-conventions-and-phase-4-refinements.md)
- [0014. fleet-agent conventions and Phase 5 refinements](adr/0014-fleet-agent-conventions-and-phase-5-refinements.md)
- [0015. fleet-server conventions and Phase 6 refinements](adr/0015-fleet-server-conventions-and-phase-6-refinements.md)
- [0016. fleet-server auth conventions and Phase 7 refinements](adr/0016-fleet-server-auth-conventions-and-phase-7-refinements.md)
- [0017. fleet-client and desktop app conventions, Phase 8 refinements](adr/0017-fleet-client-and-desktop-conventions-and-phase-8-refinements.md)
- [0018. Microsoft accounts and the vault: conventions, Phase 9 refinements](adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)
- [0019. The control plane: conventions, Phase 10 refinements](adr/0019-control-plane-conventions-and-phase-10-refinements.md)
- [0020. Bot operations, modes and live events: Phase 11 refinements](adr/0020-bot-operations-and-phase-11-refinements.md)
- [0021. testcontainers' unmaintained tar crate](adr/0021-testcontainers-unmaintained-tar-exception.md)
- [0022. Deployment and operations: Phase 12 refinements](adr/0022-deployment-and-phase-12-refinements.md)
- [0023. Local autonomous pipeline](adr/0023-local-autonomous-pipeline.md) (superseded by 0025)
- [0024. SQLite's advisory via sqlx, zlib in the runtime image, and Caddy 2.11.9](adr/0024-sqlite-zlib-and-caddy-advisories.md)
- [0025. A simpler workflow](adr/0025-a-simpler-workflow.md)
- [0026. A bot in a minecart: fleet-mc tracks the vehicle, keeps the seat and picks from it](adr/0026-a-bot-in-a-minecart.md)
- [0027. Agents renew their certificates; Phase 13's groups and refinements](adr/0027-certificate-renewal-and-phase-13-refinements.md)
- [0028. A session is enough: the password step-up stays only for the two credential changes](adr/0028-a-session-is-enough.md)
- [0029. The authorization matrix of the HTTP API](adr/0029-the-authorization-matrix.md)
- [0030. The fuzz targets](adr/0030-fuzz-targets.md)
- [0031. Mutation testing](adr/0031-mutation-testing.md)
- [0032. The load test](adr/0032-the-load-test.md)
- [0033. Chaos tests, and a server that refuses a missing database](adr/0033-chaos-tests-and-the-missing-database.md)
- [0034. A stopgap mode action into the nearest minecart, and a right-click every 200 ms](adr/0034-a-stopgap-action-into-the-nearest-minecart.md)
- [0035. The reviews: the threat model, the dependencies, the history and ASVS Level 2](adr/0035-the-reviews-and-phase-13s-end.md)
- [0036. Scripted modes: a virtual vanilla client in Rhai, and menus after it](adr/0036-scripted-modes.md)
- [0037. Scripts read chat: system lines, by declared patterns with typed values](adr/0037-scripts-read-system-chat.md)
- [0038. `chrono` is unmaintained: ignored for now, and what would end that](adr/0038-chrono-is-unmaintained.md)
- [0039. `just tidy`: what a build leaves behind, and what is safe to remove](adr/0039-just-tidy.md)
- [0040. `fleet-script` as built: an engine an instance, a sandbox tighter than planned, and what was measured](adr/0040-fleet-script-as-built.md)
- [0041. `picomatch`'s advisory in the build tools: ignored by its one ID, and how `pnpm audit` ignores](adr/0041-picomatch-advisory-in-the-build-tools.md)
- [0042. A scripted mode's inputs in fleet-mc, as built: one place for what's held, guards before azalea's own systems, and what the live scenarios found](adr/0042-scripted-inputs-in-fleet-mc.md)
- [0043. The script runner in fleet-runtime, and a script's hooks on the async workers](adr/0043-the-script-runner-and-hooks-on-the-async-workers.md)
- [0044. Scripted modes on the wire, the server and the agent: who writes one, what's checked at save, never below an agent's level, and what an agent reports](adr/0044-scripted-modes-on-the-wire-the-server-and-the-agent.md)
- [0045. The script editor and a bot's script in the app: CodeMirror in a shadow root, one description, the app's own check, and the chat's ↑ and ↓](adr/0045-the-script-editor-and-a-bots-script-in-the-app.md)
- [0046. The stopgap's way out and Phase 15's check: a step mode converted to a script, the reference generated, and the examples and the spoofs on a real server](adr/0046-the-stopgaps-way-out-and-phase-15s-check.md)
- [0047. The release's indexes without annotations, and its push and merge rehearsed in CI](adr/0047-the-releases-indexes-without-annotations.md)
- [0048. Two tests that failed only in the coverage build: the database's own last close, and a paused clock held over real sockets](adr/0048-two-tests-that-failed-only-in-the-coverage-build.md)
- [0049. The Paper harness: Paper 26.1.2 and a test plugin, every input pinned, the plugin built offline](adr/0049-the-paper-harness.md)
- [0050. Menus in fleet-core: a server's text, the screen's snapshot, the click guard, one click at a time, and the sign desk](adr/0050-menus-in-fleet-core.md)
