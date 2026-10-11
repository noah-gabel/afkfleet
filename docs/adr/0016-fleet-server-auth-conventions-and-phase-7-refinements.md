# 0016. fleet-server auth conventions and Phase 7 refinements

- **Status:** Accepted
- **Date:** 2026-10-11
- **Related:** Plan.md Phase 7 (P7.1–P7.13), §4, §5, §7.2, §7.4, §7.5, §8, Appendix A, Appendix B, P12.2, P12.7; ADR-0006, ADR-0010, ADR-0011, ADR-0015

## Context
Phase 7 builds the server's authentication and authorization:
- invite-only registration and mandatory 2FA for every user
- opaque rotating tokens (ADR-0006)
- the central `authorize()` (ADR-0010)
- layered rate limits and a lockout
- the vault for TOTP secrets
- adversarial tests

It's the most security-critical phase so far.

Before writing any code, the Phase 7 plan listed every question the plan left open, at the phase level and then for group A. The user answered them on 2026-10-11, one at a time, and this ADR records the answers. Decisions already made elsewhere aren't repeated: §7, ADR-0006, ADR-0010 and ADR-0015's notes for P7. Each later group asks its own implementation questions and adds its decisions here; if a later group's review changes a decision, that group's PR amends this ADR.

## Decision
### Grouping
**Five groups, each table with its first user** *(the user's choice)*. One branch and one PR per group, one commit per task, in this order:

| Group | Branch | Tasks |
|---|---|---|
| A | `p7/p7.2-p7.5-passwords-vault-owner` | P7.2, P7.4 (with the `*_file` loader), P7.5; `fleet_core::auth` with the password length rule |
| B | `p7/p7.3-p7.8-sessions-and-auth-user` | P7.1's `sessions` (with `stepped_up_at`), P7.3, P7.8 |
| C | `p7/p7.10-p7.11-routes-and-rate-limits` | P7.1's `login_attempts`, P7.11, P7.10 |
| D | `p7/p7.6-p7.7-login-and-2fa` | P7.1's `user_mfa` (and wherever the login challenges live), P7.7; P7.6's `login`, `login/mfa`, `refresh`, `logout`; `/me/mfa/*`; `POST /auth/step-up` |
| E | `p7/p7.6-p7.9-accounts-admin-wrap-up` | P7.1's `invites`; P7.6's `register`, `/me`, `/me/password`, `/me/sessions`; P7.9; the DoD, the threat model and the wrap-up |

- **The guards come before any endpoint.** CLAUDE.md's security rule 3 gives every new endpoint a rate-limit class and a place in the route-coverage test from day one. So P7.8, P7.10 and P7.11 (B, C) come before the first endpoint (D).
- **P7.1 is spread over the groups.** A merged migration is never edited (ADR-0015), so each table is designed together with the feature that first uses it, in its own migration, like Phase 6's "modules arrive with their code". P7.1 is ticked in E, when the last table lands. P7.6 is split between D and E and ticked in E.

### Security tests and audit events ship with the feature *(the user's decision)*
- **Audit actions arrive with their writes.** `WriteTx::commit` needs an entry anyway, so every new write adds its action to Phase 7's one `audit_actions!` block in the group that adds it:
  - A: create-owner
  - B: token reuse, with the family revocation
  - C: the lockout
  - D: login success and failure, logout, 2FA changes
  - E: password changes, role changes, invites created and used
- **Security tests go where the behavior appears.** The service-level test goes in the group that builds the service:
  - B: refresh reuse revokes the family, and `AuthUser` rejects a disabled user's session
  - C: lockout counting
  - D: TOTP replay

  The end-to-end test over HTTP goes in the group that adds the endpoint:
  - D: an unknown user and a wrong password give the same status and body; lockout through login; expired sessions and refresh reuse through `/auth/refresh`
  - E: escalation, disabling through `PATCH /users/{id}`, invite reuse and expiry

  Each is written test-first, so it's red before its feature exists.
- **E's wrap-up only checks.** It checks the DoD (auth and vault coverage ≥ 90 %, every P7.12 item green and named with its test), updates the threat model's auth section, and ticks P7.12 and P7.13.

### Step-up *(the user's choice)*
- `POST /auth/step-up {password}` sets the current session's `stepped_up_at`.
- A step-up action (§7.2: password change, 2FA reset, role change; later P9's account delete and key rotation through the API) checks that it's at most 5 minutes old. Otherwise it answers 403 with a new code, `step_up_required`.
- A wrong password counts toward the username lockout.
- This adds one column to `sessions` (B) and one route (D), with a note under Appendix B. A request body never carries the current password, and P9's `DELETE /accounts/{id}` needs no body.

### Pure rules in `fleet_core::auth` *(the user's choice)*
- **What goes there:** every decision rule of Phase 7, as pure functions and value types with `DateTime<Utc>` passed in, tested exhaustively without a database:
  - the lockout schedule
  - session expiry (access, idle, absolute)
  - the refresh outcome (current, already rotated = reuse, expired)
  - TOTP step acceptance and replay
  - invite validity
  - the step-up window
  - the password length rule
- **Who adds what:** each group adds the rules its feature needs; A adds only the password length rule. Server services load the facts, call the rule and do the IO, as handlers call `authorize()`. The crypto (argon2, SHA-256, totp-rs, zxcvbn) stays in fleet-server.
- **Coverage:** the auth coverage gate covers `crates/fleet-core/src/auth/` too *(the user's addition)*, so the rules must reach 90 % as their own module, not only inside fleet-core's crate-wide average.

### How the tests drive the server *(the user's choice)*
- **`oneshot` against the real `router(...)`:** with the full middleware stack and the limiter, a fresh SQLite per test (`#[sqlx::test]`), and `ManualClock` and `SeededRandom` injected.
- **The peer address:** each request carries it as axum's `ConnectInfo` extension, so per-IP limits and the trusted-proxy rule are tested.
- **One real-socket test** proves that `serve` wires `into_make_service_with_connect_info`.
- **fleet-client's full-stack tests stay P8.1's:** the client doesn't exist yet, and `healthcheck::get` only sends `GET`.

### Coverage gates *(the user's choices)*
- **Auth gate:**
  - fleet-core's `auth` module
  - `app/auth`, `app/users` and `app/invites`
  - all of `infra/crypto/` except the vault
  - `http/extractors/`, the rate-limit middleware and the route registry
- **Vault gate:** the vault and the `*_file` loader.
- **Outside both:** repositories and handlers stay under the 80 % workspace gate; the endpoint tests exercise them anyway.
- **Lists of path prefixes, not one regex.** An entry ending in `/` is a folder; any other entry is exactly one file. A listed prefix that matches no file in the report fails the gate, so a typo or a later rename fails CI instead of a module silently dropping to the workspace gate. Each group adds a prefix only when it creates that module.
- **Required from group A on.** Both gates are `required: true` as soon as their first module exists, so they can no longer report "skip".
- `scripts/coverage-gates.test.mjs` covers:
  - a dead prefix fails
  - a required gate without files fails
  - folder and file entries
  - the existing pass, fail and skip cases

### Group A: the password service (P7.2)
- **Layout: ports and adapters** *(the user's choice)*.
  - **Ports:** `ports::passwords::Passwords` (`#[async_trait]`) and `ports::vault::Vault`, so app services see only traits, as they see `Store`.
  - **Adapters:** `infra::crypto::passwords::Argon2Passwords` and `infra::crypto::vault::XChaChaVault`.
  - **The service:** `app::users::UserService::create_owner`.
  - **The command:** `cli::user`.
- **The argon2 queue** *(the user's choice and rule)*:
  - **Admission:** a waiting semaphore admits at most 4 × `argon2_max_concurrent` callers (a constant) with `try_acquire`. Past it, the answer is `busy` at once (503, `Retry-After: 1`; ADR-0015).
  - **Hashing permit:** an admitted caller takes it with `acquire_owned()` and moves the `OwnedSemaphorePermit` into the `spawn_blocking` closure. So the permit is released only when the hash really finishes, and a request cancelled by the timeout layer or a client disconnect never frees a slot while its argon2 work still runs. More than `argon2_max_concurrent` hashes can never run at once, which is what bounds the memory.
  - **Ownership:** a blocking task can't be aborted. The permit and the caller's `JoinHandle` own it, and the runtime's shutdown waits for blocking tasks within `RUNTIME_SHUTDOWN`. That's how the hash meets CLAUDE.md's "every task has an owner".
  - **Wait bound:** the waiting itself is bounded by the request timeout layer.
- **The dummy hash** *(the user's choice and additions)*:
  - **When it's made:** once at startup, when `Argon2Passwords` is built, from 32 random bytes and a random salt, with the configured parameters. Its parameters always equal real hashes', so an unknown username costs the same argon2 work as a real one.
  - **How it's made:** through a private path that runs argon2 without the strength check, so the public `hash` stays "every hash is checked".
  - **Checking against it:** `verify_unknown_user` takes exactly the path a real check takes (the same queue, permit and verify call) and always answers "wrong password". A test checks that it waits and answers `busy` like a real verify.
- **Re-hashing is D's question** *(the user's decision)*. A stored hash made with older `argon2_*` parameters verifies with different timing than the dummy. Whether a successful login re-hashes it, and which audit action that write records, is decided in group D (a note under P7.6).
- **`NewPassword` in `fleet_core::auth`** *(the user's choice)*:
  - **Shape:** `NewPassword(SecretString)`, built with `TryFrom<SecretString>`, with a redacted `Debug`.
  - **Length:** 12–128 Unicode scalar values (`chars()`). Spaces and any Unicode are allowed, with no trimming and no normalization.
  - **Control characters:** `char::is_control()` (C0, DEL, C1) is refused. Nobody types them, and a password pasted from a file with a trailing newline would otherwise silently include it.
  - **Errors:** `NewPasswordError::{TooShort, TooLong, ControlCharacter}` carries only the limits, never the length or the text.
  - **Scope:** it applies only to new passwords (create-owner, register, password change), never to a login attempt.
- **zxcvbn** *(the user's choices)*:
  - **Where it runs:** inside `Passwords::hash(&NewPassword, &Username)`, in the same queued blocking task and under the same permit, before argon2, with score ≥ 3 (§7.2).
  - **The length rule always comes first:** it takes a `&NewPassword`, never a raw string, so zxcvbn never sees more than 128 characters. It gets slow on long inputs.
  - **User inputs:** the username plus the fixed words "afkfleet" and "minecraft", as zxcvbn recommends for site-specific terms.
  - **Hints as our own types** *(the user's decision)*. The port names no infra crate type, as `StoreError` names no sqlx type. `fleet_core::auth` has `WeakWarning` (14 variants) and `WeakSuggestion` (13), one per zxcvbn 3.1.1 variant, each with its own fixed English `Display` text, the same words as zxcvbn's own. The error is `HashError::Weak { warning: Option<WeakWarning>, suggestions: Vec<WeakSuggestion> }`.
  - **The mapping:** an exhaustive `match` without a wildcard maps zxcvbn's enums (checked in its source: not `#[non_exhaustive]`), so an upgrade that adds a variant fails to compile instead of dropping a hint.
  - **How the 422 exposes the hints is decided in D and E.**
  - **zxcvbn reads the wall clock itself** (`time::OffsetDateTime::now_utc()`, for its "recent years" rule). That's outside fleet-server's clock port and doesn't affect any of our time rules.
- **`[auth]`'s argon2 keys** *(the user's choice)*:

  | Key | Default | Range |
  |---|---|---|
  | `argon2_memory_kib` | 19456 | 19456–1048576 |
  | `argon2_iterations` | 2 | 2–10 |
  | `argon2_parallelism` | 1 | 1–16 |
  | `argon2_max_concurrent` | 4 | 1–64 |

  - **No hashing below the floor:** nobody can configure hashing below §7.2's OWASP baseline.
  - **One way to build it:** `Argon2Passwords` is built only from `Argon2Params`, the type config validation makes, whose `Default` is Appendix A's baseline. **No constructor below the floor exists, not even for tests** *(the user's decision, replacing a first idea of a test-only constructor)*.
  - **Every test hashes at the baseline.** A `#[cfg(test)]` constructor would be invisible to the integration tests in `tests/`.
  - **The build profile:** argon2, blake2 and zxcvbn are compiled with `opt-level = 3` in the dev profile, which the test profile inherits.
  - **Measured on 2026-10-11** (Windows, x86_64; argon2 0.6.0 and zxcvbn 3.1.1 in a throwaway crate, median of 7):

    | Build | argon2 hash at 19456 KiB, t = 2, p = 1 | zxcvbn, 107-character password |
    |---|---|---|
    | Debug, unoptimized | 228 ms | 35 ms |
    | Debug, those three at `opt-level = 3` | 10 ms | 4.5 ms |
    | Release | 10 ms | 3.1 ms |
- **The hash format** *(the user's choice and addition)*:
  - **Format:** `$argon2id$v=19$m=…,t=…,p=…$<salt>$<hash>`, with a 16-byte salt from `SecureRandom` and a 32-byte output. argon2 is built without `getrandom`, so `hash_password_with_salt` gets the salt from the port.
  - **Verifying:** `verify` reads m, t and p from the stored string, so hashes made under older parameters still verify.
  - **What counts as `VerifyError::Corrupt`** (500, logged; never "wrong password", since it means something else wrote the row):
    - any algorithm other than argon2id
    - any version other than 19
    - a string that doesn't parse
    - parameters above config validation's upper bounds (m > 1048576 KiB, t > 10, p > 16), checked before argon2 runs, so a tampered or hand-written row can't make one login allocate gigabytes or run for minutes
  - **No lower bound,** so hashes made under an older, lower floor still verify.

### Group A: the vault and the secret-file loader (P7.4)
- **The envelope, version 1** *(the user's choice)*: `0x01 | key_id (u32, big-endian) | nonce (24 bytes from SecureRandom) | ciphertext with its 16-byte tag`, stored as a BLOB.
  - **AAD:** `0x01 ‖ key_id (4) ‖ record UUID (16 bytes) ‖ purpose name`. Fixed widths come first and the variable-length purpose last, so no two inputs share an AAD. Changing any header byte fails authentication, not only a key mismatch, and a version 2 can never be confused with a v1 envelope.
  - **Purposes:** a closed enum `VaultPurpose`, with fixed ASCII names: A has `TotpSecret` (`totp_secret`), P9 adds the Microsoft refresh token.
  - **A known-answer test** *(the user's addition)*: one golden v1 envelope (hex), made once with the key `[0x42; 32]`, a fixed key ID, a fixed record UUID, `TotpSecret` and a short plaintext. `open` must return the plaintext, so any change to the AAD's order or widths, the key ID's byte order, the header or a purpose name fails. It's never regenerated to make a failing test pass: a real format change means version 2, with its own golden envelope next to v1's.
  - **Layout and name tests** *(the user's addition)*:
    - On `seal`'s output: byte 0 is 0x01, bytes 1–4 are the key ID big-endian, the length is 45 + the plaintext's, and two seals of the same input differ.
    - A test pins every purpose's name, since renaming one would make every stored secret of that purpose undecryptable.
- **Key files** *(the user's choice and additions)*:
  - **Format:** standard base64 with padding, exactly 44 characters decoding to 32 bytes. It's what `openssl rand -base64 32` prints.
  - **Decoding:** with base64's `general_purpose::STANDARD` engine (canonical padding, no trailing bits) through `decode_slice` into a fixed buffer.
  - **Wrong lengths:** 0.23.1 reports a 33-byte key as `DecodeSliceError::OutputSliceTooSmall`, which counts as a wrong length, not "not base64".
  - **Zeroizing:** the file's text and the decoded buffer are `Zeroizing` and wiped once the key is in its `SecretBox<[u8; 32]>`.
  - **Errors:** they name the file and the problem, never any part of the content.
- **`fleet_startup::secret_file`, the loader for every `*_file` key** *(the user's decision: in fleet-startup, not the server's infra)*:
  - **Why there:** both binaries read secret files at startup with the same rules: the server its vault keys now and its PKI keys in P10, the agent its `[control_plane] key_file` in P10. So P10 reuses the loader instead of moving or copying it. fleet-startup gains secrecy and zeroize, and its scope (§4, CLAUDE.md) now names the loader.
  - **No gap between check and read** *(the user's refinement)*:
    1. The path is `stat`ed first (following symlinks), only to refuse anything that isn't a regular file, since opening a FIFO would block startup forever.
    2. Then the file is opened, and every check runs again on the handle's own metadata: a regular file; on Unix, no group or other bits (`mode & 0o077`); not empty; at most 64 KiB.
    3. It's read from that same handle with a cap of 64 KiB + 1. A file swapped between the checks and the read is never used.
  - **Trailing newline:** one trailing `\n` or `\r\n` is removed, nothing else.
  - **Errors:** they name the file, its mode and the fix (`chmod 600`, owned by the server's user), never the content.
  - **Windows:** permissions are ACLs there, so there's no mode check, as for the database (ADR-0015).
  - **Strict permissions mean work for P12** *(the user's choice)*. Compose bind-mounts file secrets with the host file's owner and mode, and its uid/gid/mode settings don't apply to file-backed secrets. So P12's compose setup and runbook `chown 65532:65532` and `chmod 600` every secret file. As for the database, secrets never come from a Windows folder through Docker Desktop (a note under P12.2).
- **`[vault]` is required by `serve` from group A on** *(the user's choice)*:
  - **Validation:** `master_key_files` maps key IDs (1–4294967295, the envelope's u32) to absolute paths, with 1–16 entries; `active_key_id` must be one of them.
  - **Reading at startup:** `serve` reads every key file right after loading the config, before the runtime starts (plain `std::fs`, like the config), and stops with exit 1 naming the file. `migrate` and `healthcheck` validate the paths but never read the files.
  - **Duplicate keys:** two key IDs whose bytes are identical are refused, naming both IDs, compared with `subtle`'s constant-time equality *(the user's addition)*. A "rotation" to a key equal to an old one would leave a possibly leaked key in use while the operator believes it's retired.
  - **Not held yet** *(the user's choice)*. Until D, `serve` builds `XChaChaVault` from the keys and drops it. D adds the `AppState` field together with the MFA service that reads it, so no lint exception or test-only accessor is needed.
  - **The secret-name test:** `vault.active_key_id` names a key but holds an integer, so it's on `NOT_SECRETS` with that reason.
- **The dev key** *(the user's decision)*:
  - **Created by the recipe:** `just dev-server` creates `target/dev-server/vault-key-1` itself when it's missing: 32 random bytes as base64 plus `\n`, mode 0600, never overwritten. It passes the key's absolute path through `AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1`, like the database path.
  - **Next to the dev database:** the key lives beside the dev database whose TOTP secrets it encrypts from D on, so `cargo clean` removes both together. Nothing goes in `secrets/`.
  - **CLAUDE.md's rule 12** lists `target/dev-server/vault-key-*` among the files never to read *(the user's addition)*.
- **`just dev-owner username`** *(the user's addition)*: runs `user create-owner` against the dev database with the same two variables `dev-server` passes. The dev config's validation needs both, even though create-owner never reads the key. D, E and P8 reuse it for their login checks.

### Group A: `user create-owner` (P7.5)
- **The command** *(the user's choice and addition)*: `afkfleet-server user create-owner --config <PATH> --username <NAME> [--password-stdin]`.
  - **Order of checks:** the username is checked by `Username` first. The database opens like `migrate`'s. The Owner check runs before any prompt or stdin read, and the unique index still decides at insert.
  - **The prompt:** the password is typed twice on the terminal with rpassword. On a mismatch, a refused length or a weak password (with zxcvbn's hints) it asks again, at most 3 rounds, then exits 1.
  - **`--password-stdin`** (like `docker login --password-stdin`):
    - It reads one password from stdin, capped at 1 KiB, into a `Zeroizing` buffer, and removes one trailing `\n` or `\r\n`.
    - It runs the same checks once, with no repeat and no retry.
    - It's refused when stdin is a terminal, where the typed password would be visible.
    - P12.7's nightly smoke test needs it, and so do the binary's own tests.
  - **No other way in:** there's no password flag and no environment variable, since those leak through `ps`, shell history and `docker inspect`. Without `--password-stdin` and without a terminal, it exits 1: "needs a terminal, or pass the password with --password-stdin".
  - **Exit codes:** 0 the Owner is created; 1 anything refused or failed; 2 usage error.
  - **No vault:** the command never loads the vault keys.
  - **Its clock and randomness:** it builds `SystemClock` and `OsRandom` for itself, like `serve::app`, so those two are no longer built only there.
- **Its audit entry** *(the user's choice)*: `user.create`, outcome Success, target `User(id)`, metadata `role` = "owner" and `via` = "cli", with no actor and no IP.
  - **`user.create` is shared:** E's register records the same action with `via` = "invite" (plus its own `invite.use`).
- **The naming rule for every audit action** *(the user's decision)*, in `audit_actions!`'s doc comment too:
  - Actions someone performs are `area.verb` in the imperative: `user.create`, `auth.login`, `user.set_role`, `invite.create`, `invite.use`.
  - Things the server detects are `security.<what>` as a noun: `security.refresh_token_reuse`, fixed by §7.2 and ADR-0006.
  - Success or failure is always the outcome column, never part of the name.
  - Details like `via` and `role` go into metadata, with values from closed enums, never free text.
  - Names are stored in rows forever, so a merged name is never renamed or dropped. An insta snapshot of every name in `AuditAction::ALL` makes any rename or removal show up in review.

### Tests that could block
Any test that could block on a read (the FIFO case, a pipe, a child process) runs that part on a thread or under a timeout and fails with a clear message after a few seconds *(the user's rule)*. nextest's default profile has no terminate-after, so a hang would otherwise run until CI's job timeout without naming the test.

### Notes for later groups
- **P7.6 (D)** *(the user's decisions)*:
  - Whether a successful login re-hashes a password made under older `argon2_*` parameters, and which audit action that write records.
  - Login's request DTO gets a generous upper bound on the password field, so an oversized login is refused before any hashing. It's the first request DTO, which brings garde to fleet-api-types and the mapping of axum's JSON rejections (ADR-0015).
  - `POST /auth/step-up` lives in D.
- **P7.10 (C)** *(the user's addition)*: the one real-socket test sends a request over a real connection to a route with the per-IP limit, and fails if the peer address doesn't reach the limiter. `oneshot` sets `ConnectInfo` itself, so it can't catch a missing `into_make_service_with_connect_info`.
- **P7.11 (C)** *(the user's addition)*: the route-coverage test must not pass while the authed list is empty. For example, a test-only authed route proves the 401 check, and D's and E's routes join it automatically.
- **P12.2** *(the user's decision)*: every secret file is `chown 65532:65532` and `chmod 600` in the compose setup and the runbook (P12.6), and P12.7's smoke test runs with them that way.
- **P12.7** *(the user's decision)*: the smoke test creates the Owner with `docker compose run -T … user create-owner --username … --password-stdin`.

### Dependencies (group A, the user's approval)
- **New in `[workspace.dependencies]`,** at §5's versions:
  - **argon2 0.6.0:** no default features (that drops its `getrandom`); `alloc`, `password-hash`, `zeroize`.
  - **chacha20poly1305 0.11.0:** no default features (no `getrandom`; nonces come from `SecureRandom`); `alloc`, `zeroize`.
  - **zxcvbn 3.1.1:** no default features (that drops `derive_builder`).
  - **rpassword 7.5.4.**
  - **base64 0.23.1:** no default features. That drops `simd-unsafe`, the crate's only unsafe code, so it's `forbid(unsafe_code)`.
  - **subtle 2.6.1.**
  - **zeroize 1.9.0:** direct, as §5 lists it, rather than through secrecy's re-export.
- **Who uses them:**
  - **fleet-server:** argon2, chacha20poly1305, zxcvbn, rpassword, base64, subtle, zeroize, and tokio's `sync` (the semaphores).
  - **fleet-startup:** secrecy and zeroize; tempfile in dev.
- **The profile:** `[profile.dev.package.argon2]`, `blake2` and `zxcvbn` with `opt-level = 3`.
- **zxcvbn's tree** (`cargo tree`, no default features): fancy-regex, regex, itertools, lazy_static and time.

## Consequences
- **Easier:**
  - Every rule of the phase is a pure function in fleet-core, tested exhaustively on fixed times without a database.
  - Each table, audit action and security test is designed together with its feature. An attack test fails before its defense exists, and no table is fixed before its feature is understood.
  - A coverage gate can't silently lose a module, and the auth and vault gates can't report "skip" any more.
  - Hashing can't be configured below the baseline, can't exceed its memory bound under cancellation, and can't run without the strength check. Unknown usernames cost the same work as real ones.
  - One loader with one set of rules reads every secret file of both binaries.
  - The vault's format is pinned by a golden envelope, so a stored secret can't become undecryptable by accident.
- **Harder:**
  - Every test that hashes takes about 10 ms, and dev builds compile argon2, blake2 and zxcvbn optimized.
  - `serve`, the dev setup and every server test that starts the binary need a vault key file from group A on.
  - The secret files need the right owner and mode on Unix, which P12 has to arrange.
  - P7.1 and P7.6 are ticked only in E.
  - The step-up endpoint and column are additions to Appendices B and D.

## Alternatives considered
- **P7.1 whole in group A,** or four larger groups. All four tables would be fixed before the token, 2FA and invite designs exist, and a merged migration can't be edited.
- **A final security group.** The attack tests would pass at once instead of failing first.
- **The current password in every step-up request.** No 5-minute window, bodies on `DELETE` routes, and an argon2 check on every step-up action.
- **Step-up with the password and a TOTP code.** Beyond §7.2's "re-entering the password".
- **The rules inside fleet-server's services.** Every rule test would need SQLite and real time.
- **A test JSON client over sockets, or fleet-client brought forward.** Slower tests, one more client to maintain, or a P8 task pulled into Phase 7.
- **Keeping today's gate paths, or adding the repositories.** The first leaves the extractors, the limiter and the loader at 80 %. The second puts the repositories' rarely reachable error paths under 90 %.
- **Concrete services without ports, or the loader under config or the server's infra.** The first breaks the layering. The second puts file IO into config, or makes P10 move or copy the loader.
- **No queue, or a deadline instead of a bound.** A fifth simultaneous login would get a 503, or a burst could hold unbounded tasks.
- **A lazy or constant dummy hash.** The first login of an unknown user would cost two hashes. A constant can't follow the configured parameters.
- **Byte lengths, or no control-character rule.** Users count characters, not bytes, and a pasted trailing newline would silently join the password.
- **A separate strength check.** A caller could hash without it.
- **zxcvbn's own types in the port,** or no hints at all.
- **OWASP's equivalent pairs, or only argon2's own limits.** A combined check instead of plain ranges, or hashing below the baseline.
- **A test-only cheap constructor or cargo feature.** It's invisible to the tests in `tests/`, or it's one more feature combination; it would also mean testing other parameters than production's.
- **§7.5's AAD without the header.** The header would be unauthenticated.
- **Raw or hex key files.** Raw bytes can't be pasted; hex is longer and needs our own decoder.
- **Permissions that allow group or other reads, or only warn about them.** A key readable by other host users would be accepted.
- **The vault keys loaded only in D, or a separate `just dev-vault-key` into `secrets/`.** A key setup error would surface only at the first enrollment. A dev key in `secrets/` could outlive or be separated from the dev database it encrypts.
- **The vault held in `AppState` from A on.** That needs a lint exception or a test-only accessor until D.
- **A single prompt attempt, or exit 3 for "an Owner exists".**
- **`user.create_owner`, or past-tense action names.** A Failure outcome on a past-tense name reads oddly.
- **Unlimited or exactly one key file.**
