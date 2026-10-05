# Threat model

> **Status: skeleton (P0.13).** Based on Plan.md §7.1. It gets expanded as the components are built:
> - authentication and authorization in **P7**
> - the vault and Microsoft accounts in **P9**
> - the control plane in **P10**
>
> It's reviewed as a whole in **P13.7**. Every mitigation listed here must eventually point to the test or control that proves it.

## Scope
The whole afkfleet system:

| Component | Runs on |
|---|---|
| `afkfleet-server` (HTTP API, WebSocket, gRPC control plane, SQLite, vault) | Linux host, Docker Compose |
| Caddy (TLS termination for the HTTP API) | Linux host, Docker Compose |
| `afkfleet-agent` (the bots) | Linux host(s) |
| Desktop app (Tauri) | Users' Windows machines |

Out of scope:
- Minecraft servers themselves
- Microsoft's and Mojang's services
- the security of the host operating systems

Hardening the host is in `docs/runbook.md` (P12).

## Assets
Ranked by impact if compromised:

| # | Asset | Why it matters | Where it lives |
|---|---|---|---|
| 1 | Microsoft refresh tokens | Takeover of the owner's Microsoft/Minecraft account | Server DB, encrypted by the vault |
| 2 | Vault master key, CA private key | Decrypt every stored token; impersonate the server or agents | Docker secrets, outside the DB and backups |
| 3 | User credentials and sessions | Act as any user of the app | DB (Argon2id hashes, SHA-256 token hashes); refresh token in the OS keychain |
| 4 | Control over bots | Chat and run commands as the player, possibly with operator rights on a server | Server (desired state), agents |
| 5 | Chat history and audit log | Privacy of players; accountability | Server DB |

## Trust boundaries
| # | Boundary | What crosses it | Primary protection | Expanded in |
|---|---|---|---|---|
| B1 | Internet → Caddy → server HTTP API | REST, WebSocket | TLS, authentication, `authorize()`, rate limits, input validation | P6, P7 |
| B2 | Webview ↔ Rust core of the desktop app | Tauri IPC commands | Strict CSP, capabilities for our commands only, tokens kept out of JavaScript (ADR-0007) | P8 |
| B3 | Agent ↔ server | gRPC control stream, session grants | mTLS with server-signed certificates, fingerprint allowlist, single-use enrollment tokens | P10 |
| B4 | Agent ↔ Minecraft servers | Minecraft protocol, chat, kick reasons | Untrusted input: sanitized in core, rendered as text, bounded queues | P2, P3 |
| B5 | Server ↔ Microsoft/Mojang | OAuth device flow, token refresh | TLS with certificate checks, timeouts, single-flight refresh | P9 |
| B6 | Server ↔ storage | DB file, backups, Docker secrets | Secrets encrypted at rest with AAD binding, keys outside the DB, file permissions | P6, P9, P12 |
| B7 | User's machine | OS keychain, installed app | Refresh token only in the keychain, signed updates | P8, P12 |

## Attackers
- **Internet attacker** probing the public API: credential stuffing, enumeration, DoS, injection.
- **Malicious or compromised friend account** (insider): privilege escalation, abuse of bots, reading other users' data.
- **Stolen laptop** with the app installed and a session in the keychain.
- **Compromised agent host:** trying to obtain tokens for bots it doesn't run, or to impersonate other agents.
- **Hostile Minecraft server:** malicious chat or kick messages (injection into logs and UI, resource exhaustion).

## Threats and mitigations
| Threat | Mitigation | Verified by |
|---|---|---|
| Brute force / credential stuffing | Argon2id; per-IP and per-account throttling; lockout with exponential delay; mandatory 2FA for every user; invite-only registration | P7.10, P7.12 |
| Token theft | Short-lived opaque access tokens; refresh-token rotation with reuse detection that revokes the whole family; tokens hashed at rest; refresh token only in the OS keychain, never in JS or localStorage (ADR-0006, ADR-0007) | P7.3, P7.12, P8.4 |
| Privilege escalation by a friend | One pure `authorize()` for every route; per-account grants; Admins can't touch the Owner or other Admins; authorization-matrix test; route-coverage test; audit log | P2.9, P7.11, P13.1 |
| Bot abuse (`/op`, `/pay`, …) | Slash commands need **Manage**, or must be on the allowlist; per-bot and per-user chat rate limits; every sent message is audited | P11.3 |
| Database or backup leak | MS tokens and TOTP secrets encrypted with XChaCha20-Poly1305, bound to their record by AAD; key stored outside the DB and backups; passwords hashed with Argon2id; session tokens stored as SHA-256 | P7.4, P9.2, P12.3 |
| Rogue or compromised agent | mTLS with server-signed certificates; single-use enrollment tokens; fingerprint allowlist and revocation; an agent only ever gets session tokens for bots assigned to it | P10.3, P10.6, P10.9 |
| Malicious chat / XSS | Chat is sanitized plain text (control and format codes stripped, length capped); React escaping; no `dangerouslySetInnerHTML`; strict CSP | P2.3, P8.3, P11.9 |
| DoS | Rate limits, body-size limits, timeouts, bounded queues, an argon2 semaphore, WebSocket connection caps | P6.6, P7.10, P11.5, P13.4 |
| Supply chain | `cargo deny` (advisories, licenses, sources, bans incl. a single TLS provider), `pnpm audit`, committed lockfiles, minimal crate registry, actions pinned to commit SHAs, weekly audit run (ADR-0009) | P0.5, P0.11 |
| Information leakage | Generic error messages with request IDs; no stack traces; uniform login responses plus a dummy hash against username enumeration; secrets redacted from logs | P6.5, P7.12, P9 |

## Per-boundary analysis
To be written per boundary (STRIDE: spoofing, tampering, repudiation, information disclosure, denial of service, elevation of privilege) in the phase listed under "Expanded in" above.

### Authentication and authorization (B1)
*To be expanded in P7.*

### Desktop app (B2, B7)
*To be expanded in P8.*

### Vault and Microsoft accounts (B5, B6)
*To be expanded in P9.*

### Control plane (B3)
*To be expanded in P10.*

### Minecraft servers (B4)
*To be expanded in P2/P3.*

## Accepted risks and known limits
- **Session tokens outlive server outages.** A Minecraft session token lives about 24 h. If the server is unreachable for longer, a bot that disconnects can't rejoin until it's back (Plan.md §6).
- **Users with Manage act as the player.** That's the purpose of the permission: an insider with Manage on an account can run any command as that player. The audit log records it.
- **azalea faults share a host thread.** A panic inside azalea can affect every bot on the same MC host thread. P1.6/P1.7 measure the blast radius (ADR-0004).
- **azalea 0.16.0 dependencies with open advisories.** `hickory-proto` 0.25.2 (RUSTSEC-2026-0118/0119) and `rsa` (RUSTSEC-2023-0071). They must be resolved or explicitly accepted before P3 (ADR-0003).
