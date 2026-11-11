# afkfleet's documents

Every document in this folder. Start with the repository's [README](../README.md); the roadmap and the design are in [`Plan.md`](../Plan.md), at the root.

## Guides
- [Using the afkfleet app](using-the-app.md), for friends: installing the app, connecting to a server, registering with an invite, 2FA, updates and removing it.
- [The runbook](runbook.md), for whoever hosts afkfleet on a Linux host with Docker: installing, settings, updates, backups and restores, the keys' copy, metrics, agents on other hosts, incident response and uninstalling.
- [Releasing afkfleet](releasing.md), for the maintainer: GitHub's setup, the updater's key, each release's steps and the Minecraft version bump.
- [Developing afkfleet](development.md), for contributors: the architecture, a local fleet, the binaries' settings and the API, the commands, and the checks only you run with your own account.

## Security, design and checks
- [The threat model](threat-model.md): what afkfleet protects, from whom, and how, each mitigation with the tests behind it.
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
