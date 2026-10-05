# 0006. Opaque tokens instead of JWT

- **Status:** Accepted
- **Date:** 2026-10-05
- **Related:** Plan.md §7.1, §7.2, Phase 7; ADR-0005

## Context
Users authenticate to the server from the desktop app (and possibly a website later). Requirements from the security model:
- Logout, disabling a user and role changes must take effect **immediately**.
- A stolen token must be detectable and revocable.
- A database leak must not leak usable tokens.

There is one server and a small number of users, so looking up a session per request is cheap.

## Decision
We use **opaque, random bearer tokens** backed by a session row in the database.
- **Generation:** 256 bits from `getrandom`, encoded as base64url, with the prefix `afk_at_` (access) or `afk_rt_` (refresh) so secret scanners can recognize them.
- **Storage:** only the **SHA-256 hash** of each token is stored. It's compared in constant time (`subtle`).
- **Lifetimes:**
  - access tokens: 15 minutes
  - refresh tokens: 7 days idle, 30 days absolute
- **Rotation:** refresh tokens rotate on every use and belong to a **family**. Reusing an already rotated refresh token revokes the whole family and writes the audit event `security.refresh_token_reuse`.
- **Every request looks up its session in the database.** That makes revocation, user disabling and role changes immediate.
- **Login is two steps:** the password, then a TOTP code. Neither the password step nor a setup token ever returns real tokens on its own (Plan.md §7.2).

## Consequences
- **Immediate revocation.** There's no window in which a revoked token still works.
- **Leaked hashes are useless.** A leaked database contains only hashes; 256-bit random tokens can't be brute-forced from them.
- **No signing keys** to manage or rotate, and no JWT algorithm pitfalls (`alg: none`, key confusion).
- **One database read per request.** Acceptable at this scale; a short-lived cache could be added later if needed.
- **Tokens mean nothing outside our server.** Other services can't verify them without asking it. Nothing needs that.

## Alternatives considered
- **JWT access tokens + refresh tokens.** Stateless verification, but revocation needs a denylist (so a database lookup anyway), and the signing keys become a high-value secret.
- **Server-side sessions in cookies.** That's the plan for the future website (Plan.md Phase 14). The desktop app proxies through Rust (ADR-0007) and uses bearer tokens; the token model underneath is the same.
