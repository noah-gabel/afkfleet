# Threat model

> **Status: skeleton (P0.13).** Based on Plan.md §7.1. It gets expanded as the components are built:
> - the Minecraft servers (B4) in **P3** (done)
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
| Privilege escalation by a friend | One pure `authorize()` for every route, deny by default; per-account grants, never to oneself; Admins can't touch the Owner or other Admins; only the Owner changes roles and enrolls agents; authorization-matrix test; route-coverage test; audit log | P2.9, P7.11, P13.1 |
| Bot abuse (`/op`, `/pay`, …) | Slash commands need **Manage**, or must be on the allowlist (compared exactly and case-sensitively), also inside modes; per-bot and per-user chat rate limits; every sent message is audited | P2.9, P11.3 |
| Database or backup leak | MS tokens and TOTP secrets encrypted with XChaCha20-Poly1305, bound to their record by AAD; key stored outside the DB and backups; passwords hashed with Argon2id; session tokens stored as SHA-256 | P7.4, P9.2, P12.3 |
| Rogue or compromised agent | mTLS with server-signed certificates; single-use enrollment tokens that only the Owner creates; fingerprint allowlist and revocation; an agent only ever gets session tokens for bots assigned to it | P2.9, P10.3, P10.6, P10.9 |
| Malicious chat / XSS | Chat is sanitized plain text (control characters, format codes, bidi controls and invisible characters stripped, length capped); React escaping; no `dangerouslySetInnerHTML`; strict CSP | P2.3, P8.3, P11.9 |
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
*Expanded in P3 (ADR-0008, ADR-0010, ADR-0011).*

**What crosses the boundary.** The agent speaks the Minecraft protocol with each server its bots join. It's encrypted only in online mode, with the server's own key.
- **From the server:** chat, kick reasons, game state (chunks, entities) and the timing of every packet.
- **From the bot:** chat and commands, actions, and the account's name and UUID.
- **During an online login,** the agent also calls Mojang over TLS: the session server's join, with the Minecraft token, and the chat-signing certificates.
- **Server names** are looked up through DNS.

A Minecraft server never receives the Minecraft token, and an agent never holds a Microsoft token (security rule 6).

**What a hostile server controls:** every byte a bot receives, and when it kicks the bot or goes quiet. fleet-core's sanitizer and classifier and fleet-mc's adapter treat all of it as untrusted. The runtime's watchdog (P4.6) is the last line.

| STRIDE | Threat | Mitigation | Verified by |
|---|---|---|---|
| Spoofing | An impostor server, through DNS or the network path | The token goes only to Mojang over TLS, and Mojang binds the join to the server's key, so an impostor can't pass the login on. The rest is an accepted risk (below) | `fleet-mc` `online::slow_online_mode_scenario`; the user's `real_account::manual_real_account_scenario` |
| Spoofing | Forged chat senders: system text naming a player, or a sender the server made up | Senders come only from player and disguised packets, and system chat never gets one. No permission or trigger decision relies on a sender (accepted risk, below) | `map::system_chat_has_no_sender_even_when_its_text_names_one`, `incoming::a_spoofed_sender_in_system_text_stays_text` |
| Tampering | Chat or kick text with control characters, format codes, bidi controls or newlines, aimed at logs and the UI | The core sanitizer strips them and caps the length. Text is stored and rendered as plain text, and chat is logged only at `debug`, as a structured `?` field | P2.3 (`incoming::received_chat_is_always_clean`), `disconnect::kick_messages_are_sanitized_and_capped`, `map::chat_text_and_sender_are_sanitized`; P8.3, P11.9 |
| Tampering | A kick text that pretends to be a ban or a duplicate login | Kicks are classified by translation key, never by text, and a kick without a known key is transient. Per-server conflict texts are opt-in (P4.1, accepted risk below) | P2.4 (`disconnect::classifies_kicks_by_key`, `kicks_without_a_known_key_are_transient`), `map::nested_args_keep_a_ban_permanent` |
| Repudiation | What a bot said on a server | Every message a bot sends is audited. An online account signs its chat, so what it says is attributable to the player, as with any client | P11.3; signed chat in the user's real-account check |
| Information disclosure | The Minecraft token or the chat-signing key reaching a log | fleet-mc never logs the token, and both binaries cap `azalea_auth` at `info` (P5.2, P9.3). Redaction tests capture every level of every target. The real-account check also allows `PRIVATE KEY` only under `azalea_auth::certs` | `account::tests::the_token_never_appears_in_the_adapters_logs`, `online::slow_online_mode_scenario`, `real_account::manual_real_account_scenario`; P5.2's cap test |
| Information disclosure | What a server and the DNS learn: the account, the agent's IP, the server names it resolves | Inherent to playing. IP addresses are never looked up, and the DNS fallback is an accepted risk (below) | — |
| Denial of service | Nested translations that render exponentially | fleet-mc renders server text itself and stops at 4,096 characters or 16,384 steps. azalea's own rendering, in its `info` kick log, stays off because azalea's targets default to `warn` (P5.2) | `render::nested_arguments_stop_at_the_char_budget`, `nested_empty_arguments_stop_at_the_step_budget`, `map::nested_system_chat_is_cut_off_and_marked_truncated` |
| Denial of service | Chat floods | The bounded bridge drops only chat, and counts it. Lifecycle events are always delivered | `bridge::full_queue_drops_only_chat_and_counts_it`, `lifecycle_events_are_delivered_past_the_capacity` |
| Denial of service | A login that stalls, or a flood of events before the join | The connect timeout runs from `connect()` to `Joined` and is checked before events | `driver::a_quiet_server_times_out_at_the_deadline`, `a_flood_of_events_before_the_join_still_times_out_at_the_deadline`, `tests/connector.rs` |
| Denial of service | A server that goes quiet after the join | Packets and ticks stamp the session's liveness, and the watchdog tears a quiet session down | `bridge::forwarded_ticks_and_keep_alives_only_stamp_liveness`; P4.6 |
| Denial of service | A packet that makes azalea panic or hang (accepted risks below) | One host thread and App per bot, with the single-threaded executor. A panic ends only that session, as crashed. A hung thread is abandoned and counted, and at the limit the agent restarts (P5.3) | `containment::slow_fault_containment_scenario`, `app::every_schedule_runs_single_threaded`, `tests/host_pool.rs` (`hung_thread_is_abandoned_on_shutdown_and_counted`, `pool_refuses_new_threads_once_the_abandoned_limit_is_reached`) |
| Denial of service | Oversized or compressed packets | azalea caps a packet at 8 MiB uncompressed and checks that before decompressing. Accumulated state isn't capped (accepted risk below) | azalea-protocol 0.16.0 `read.rs`, re-checked at every azalea bump |
| Denial of service | Kick and reconnect loops | Retry backoff and the circuit breaker. A duplicate login pauses the bot, which never fights a human | P2.5, P2.6 |
| Denial of service | Leaks across reconnects | Every way a session ends frees its host thread and World. On Linux, the OS thread count doesn't grow past its baseline after a warm-up | `teardown::slow_teardown_scenario` (P3.8), `tests/connector.rs` |
| Elevation of privilege | A server making a bot act for it | Bots act only on their mode and on users' commands, never on received chat. Slash commands need Manage or the allowlist | P2.9, P11.3 |
| Elevation of privilege | Code execution through parsing | No `unsafe` in our code (`unsafe_code = "forbid"`). azalea is pinned exactly, its advisories are reviewed (ADR-0012), and a fault stays in its host thread | The workspace lints, `cargo deny`, the containment scenario |

## Accepted risks and known limits
- **Session tokens outlive server outages.** A Minecraft session token lives about 24 h. If the server is unreachable for longer, a bot that disconnects can't rejoin until it's back (Plan.md §6).
- **Users with Manage act as the player.** That's the purpose of the permission: an insider with Manage on an account can run any command as that player. The audit log records it.
- **Allowlisted commands pass with any arguments.** The allowlist holds command names only, so allowing `/home` allows `/home <any name>`. Only allowlist commands whose arguments are harmless for the player (ADR-0010).
- **Grants outlive a promotion.** An Admin has implicit Manage on Members' accounts and can grant access to others, for example to an alt account created with a Member invite. When that Member is promoted to Admin, the Admin's implicit access ends, but the grants made before stay. Nobody can grant to themselves, which closes the direct path. The app shows the grants on a promoted Member's accounts so the Owner can review them (P11.9, ADR-0010).
- **Chat senders are server-attributed, not verified.** azalea 0.16 never verifies chat signatures, and fleet-mc shows a player message's `unsigned_content` when the server sends one. So a sender's name, its UUID and the text are only what the server claims. No permission or trigger decision may rely on a chat sender; a feature that needs one must verify the signature and use the signed body, in its own ADR (ADR-0011).
- **Plain-text duplicate-login kicks aren't recognized by default.** The classifier only knows the vanilla key `multiplayer.disconnect.duplicate_login`. A proxy or plugin that kicks the bot with plain text when a human logs in makes the bot reconnect and kick the human; the circuit breaker only limits how often. P4.1 adds per-server conflict texts for such servers. They're empty by default, so the user has to configure them (ADR-0010).
- **A hung azalea thread can't be killed.** Each bot has its own host thread and Bevy App with a single-threaded executor, so a panic or a hang affects exactly one bot (ADR-0008 §2–3). A hung thread is abandoned and leaks until the process restarts. Above the abandoned-thread limit the agent exits and Docker restarts it, which briefly disconnects every bot on that agent (ADR-0008 §5, Plan.md §6 row 8). If hangs turn out to be frequent, a process-per-bot connector is the fallback (ADR-0004).
- **azalea 0.16's NBT chat decoder is exponential.** It parses the nested `with` arguments of a translation twice per level, so a hostile server can send a small chat component that hangs the bot's host thread while decoding it, before fleet-mc ever sees it. The hang stays in that bot's thread: the thread is abandoned and counted, and after 3 abandoned threads the agent exits and Docker restarts it (ADR-0011, Plan.md §6 row 8). Removal trigger: an azalea release that fixes the decoder.
- **azalea's trace logs contain secrets.** Both binaries cap `azalea_auth` at `info`, whatever the operator's filter says, and a redaction test checks each cap (ADR-0011).
  - **Agent:** in every online session, azalea fetches the account's chat-signing certificates with the Minecraft token, and `azalea_auth::certs` logs the whole response at `trace`, the chat-signing private key included. The agent's filter keeps azalea's targets at `warn` by default, and the cap holds even when the operator asks for more (P5.2). fleet-mc's own code never logs the token; a redaction test captures every level from every target to check that.
  - **fleet-server:** its Microsoft flows run through azalea-auth, which at `trace` logs the Microsoft access token, the whole token response with the refresh token, the Xbox Live, XSTS and Minecraft auth responses, and the account cache. The same cap keeps all of it out (P9.3), and P9's redaction test checks it.
- **Minecraft servers aren't authenticated.** The Minecraft protocol doesn't prove a server's identity: it encrypts with the server's own key, without a certificate. So whoever controls DNS or the network path between an agent and a server can pose as that server.
  - The Minecraft token never reaches a Minecraft server, only Mojang's session server over TLS. In online mode Mojang binds the join to the server's key, so an impostor can't pass the bot's login on to the real server.
  - The impostor still gets everything the bot says and does there. That includes anything users send through the bot's chat, such as `/login <password>` for a server's login plugin.
  - **Offline accounts have no protection at all:** the connection isn't encrypted, so anyone on the path can read and change it, and anyone can join under the bot's name (ADR-0008, ADR-0011).
- **A server can grow a bot's memory.**
  - azalea caps a single packet at 8 MiB uncompressed and checks the declared size before decompressing (azalea-protocol 0.16.0). Nothing caps the World a server fills with chunks and entities, and fleet-mc doesn't limit a bot's memory.
  - The agent container's memory limit (P12.2) and Docker's restart (Plan.md §6 row 10) bound it, which briefly disconnects every bot on that agent.
  - If this turns out to happen, a process-per-bot connector is the fallback (ADR-0004), as for hangs.
- **azalea's resolver may fall back to Google's DNS.** azalea looks server names up with hickory and the system's resolver configuration. When the host has none, it queries Google's public DNS instead, which then learns which server names the agent resolves. Server addresses that are IP addresses are never looked up. The connect timeout bounds every lookup, since azalea's resolver has none (ADR-0011).
- **azalea 0.16.0 dependencies with open advisories.** `hickory-proto` 0.25.2 (RUSTSEC-2026-0118/0119) and `rsa` (RUSTSEC-2023-0071). The user accepted them with recorded reasons: the hickory code is either not compiled or not reachable, and azalea never decrypts with RSA. `deny.toml` ignores exactly these IDs, and every azalea bump re-checks them (ADR-0011, ADR-0012).
