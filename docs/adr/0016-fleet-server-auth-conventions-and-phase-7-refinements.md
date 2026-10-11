# 0016. fleet-server auth conventions and Phase 7 refinements

- **Status:** Accepted
- **Date:** 2026-10-11
- **Related:** Plan.md Phase 7 (P7.1–P7.13), §4, §5, §7.2, §7.4, §7.5, §8, Appendix A, Appendix B, Appendix D, P8.1, P11.6, P12.2, P12.4, P12.7; ADR-0006, ADR-0010, ADR-0011, ADR-0015

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
  - **Ownership:** a blocking task can't be aborted. The permit and the caller's `JoinHandle` own it. That's how the hash meets CLAUDE.md's "every task has an owner".
  - **At shutdown:** `serve`'s runtime waits at most `RUNTIME_SHUTDOWN` (1 s) for a running hash (about 10 ms); past that the process exits and the OS ends the blocking task.
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

### As built (group A)
- **`[auth]`'s ranges are checked in validation, not by garde.**
  - **Why:** clippy's `struct_field_names` refuses four fields sharing the `argon2_` prefix, so `RawAuth`'s fields drop it and serde names the keys. garde would then report the field's name instead of the key: 0.23 parses its `rename` attribute but never applies it (checked in its source).
  - **What validation does:** it checks each value against `Argon2Params`' bounds and reports every problem under its key, in garde's words ("lower than …", "greater than …"). It then builds `Argon2Params::try_new`, the type's only checked constructor besides `Default`, whose bounds the password service also uses for the stored-hash limits.
- **Characters before length in the key files** *(the user's decision during the build)*.
  - **The problem:** base64 0.23.1's `decode_slice` checks that the whole input fits before it looks at any character (`decode.rs` line 213), so a 45-character text with a stray space would have been "the wrong length".
  - **The order now:**
    1. A byte-order mark (UTF-8 `EF BB BF`, UTF-16 `FF FE` or `FE FF`) gets its own error, "starts with a byte-order mark; save it as plain text without BOM". Windows editors and Windows PowerShell 5.1's `>` add one, and it's invisible.
    2. Any byte outside the standard alphabet and `=` is "not base64".
    3. Then `decode_slice` decides the padding, the trailing bits and the length; a 33-byte key is still `WrongLength` through `OutputSliceTooSmall`.
- **No copy of a key or a password is left unwiped:**
  - **Vault keys** decode straight into their `SecretBox<[u8; 32]>`, with no intermediate buffer. `XChaChaVault::new` consumes the loaded keys: each goes into its cipher by reference (hybrid-array's `From<&[u8; 32]> for &Array`), which chacha20poly1305's `zeroize` feature wipes on drop, and the loaded copy is dropped (wiped) right after.
  - **The secret-file loader and `--password-stdin`** allocate their buffers once at full size (`Zeroizing`), so reading never reallocates and leaves a copy behind.
  - **A typed password** is copied into a `SecretString` of exactly its length, so boxing it never reallocates, and the original `String` is zeroized.
- **Each error's chain names everything once.** The manual check of a missing key file showed "the secret file … can't be read" twice, because the errors printed their source in their own message and also returned it from `source()`, which the server's `chain()` joins. `SecretFileError` now says "the secret file … can't be read" with the OS's error as its source, and `VaultKeyError::File` says only "vault key 1". The log reads "the vault's keys can't be loaded: vault key 1: the secret file … can't be read: <the OS's reason>". Regression tests in both crates check the chain.
- **`VaultError::TooLarge`** joins the port's errors: XChaCha20-Poly1305 refuses plaintexts beyond about 256 GiB, and `seal` returns a `Result`, so nothing may panic there. No stored secret comes near it.
- **base64 stays without `alloc` in tests too:** the tests encode with `encode_slice` into a fixed buffer, and the serve tests' key is a constant, checked with Node.
- **Password reads off the runtime** (from the group A review, fixed in group B's first commit). The prompt and the `--password-stdin` read ran as blocking IO inside async code, against CLAUDE.md. Both now run in `spawn_blocking`: `PasswordInput` owns its source (`Box<dyn PasswordPrompt + Send>`, `Box<dyn Read + Send>`), and each read moves it onto the blocking thread and hands it back. A failed blocking task is refused like a missing terminal (the prompt) or as unreadable (stdin). The Owner check still runs before any read. Two tests on a current-thread runtime prove it: the stand-in source answers only once another task on the same runtime has seen it start reading, bounded at 5 s; read inline, that task never runs.
- **`user create-owner`'s output:**
  - **Success:** "the Owner "alice" is created" on stdout, and `info` "the Owner is created" with the user's ID.
  - **A final refusal:** one `afkfleet-server: …` line on stderr.
  - **A round's refusal at the prompt** (a mismatch, the length rule, a weak password with its hints): a plain line before the next prompt.
  - **A failure of the database or the password service:** one `error` with its chain as well.
- **Coverage gates:**
  - **The entries,** each a file llvm-cov's report lists (`infra/crypto/mod.rs` holds only `mod` lines, and `ports/passwords.rs` only a trait and error types, so neither is listed):
    - auth: `crates/fleet-core/src/auth/`, `crates/fleet-server/src/{app/users.rs, infra/crypto/passwords.rs}`
    - vault: `crates/fleet-server/src/{infra/crypto/vault.rs, ports/vault.rs}`, `crates/fleet-startup/src/secret_file.rs`
  - **The other gates keep their regex.**
  - **Reporting:** a gate's result names its missing prefixes, and the script prints them.
- **Lockfile:** Cargo.lock gained argon2, password-hash, phc, blake2, chacha20poly1305, aead, cipher, inout, poly1305, universal-hash, zxcvbn, fancy-regex, bit-set, bit-vec, rpassword and rtoolbox. base64 0.23.1, subtle, zeroize, chacha20, time, regex, itertools and lazy_static were already there.
- **Tests,** each task red against compiling stubs first:
  - P7.2: fleet-core 19 of 29 and fleet-server 68 of 77 failed on assertions.
  - P7.4: fleet-startup 15 of 16 failed on Windows (the Unix-only cases run in Linux CI), fleet-server 49, and the dev script's 7 of 8. The BOM and alphabet rule added 4 more.
  - P7.5: 26 failed.
  - The coverage gate script: 7 of 15.

  Then all passed. One test was vacuous against a stub: the byte-flip test looped over an empty envelope. It now asserts the envelope's length first.

### Tests that could block
Any test that could block on a read (the FIFO case, a pipe, a child process) runs that part on a thread or under a timeout and fails with a clear message after a few seconds *(the user's rule)*. nextest's default profile has no terminate-after, so a hang would otherwise run until CI's job timeout without naming the test.

### Notes for later groups
- **P7.6 (D)** *(the user's decisions)*:
  - Whether a successful login re-hashes a password made under older `argon2_*` parameters, and which audit action that write records.
  - Login's request DTO gets a generous upper bound on the password field, so an oversized login is refused before any hashing. It's the first request DTO, which brings garde to fleet-api-types and the mapping of axum's JSON rejections (ADR-0015).
  - `POST /auth/step-up` lives in D.
- **P7.10 (C)** *(the user's addition)*: the one real-socket test sends a request over a real connection to a route with the per-IP limit, and fails if the peer address doesn't reach the limiter. `oneshot` sets `ConnectInfo` itself, so it can't catch a missing `into_make_service_with_connect_info`.
- **P7.11 (C)** *(the user's addition)*: the route-coverage test must not pass while the authed list is empty. For example, a test-only authed route proves the 401 check, and D's and E's routes join it automatically.
- **D** *(group B)*: `fleet_core::audit::target`'s test of an unknown target type uses `"session"`, which becomes a known type when `AuditTarget::Session` arrives with `auth.logout`; that test needs another name then. Handlers put `AuthUser` before any body extractor, so a request without credentials is refused before its body is read.
- **P8.1** *(group B, the user's decisions)*: a request with an access token a refresh has since replaced gets 401. fleet-client's single-flight refresh retries once with the current token when a refresh already replaced the one it used, and refreshes only on a 401 for the current token. A lost refresh response means logging in again.
- **P11.6** *(group B, the user's decision)*: a session and its history rows are deleted once `created_at` + the current absolute lifetime has passed, revoked or not.
- **P12.4** *(group B, the user's decision)*: a counter of detected refresh-token reuses among the key metrics.
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

### Group B: sessions, tokens and `AuthUser` (P7.1's `sessions`, P7.3, P7.8)
The user answered group B's questions on 2026-10-11, one at a time.

- **Dependencies** *(the user's approval)*:
  - sha2 0.11.0, no default features (no `alloc`, no `oid`), with `zeroize`, so the hasher's block buffer, which holds the tail of the token it just hashed, is wiped on drop. It was already in Cargo.lock through sqlx and azalea.
  - axum-extra 0.12.6, no default features (no `tracing`), only `typed-header`, for `TypedHeader<Authorization<Bearer>>`. It adds axum-extra, headers 0.4, headers-core 0.3 and sha1 0.10 to the lockfile; base64 0.22, mime, httpdate and digest 0.10 were already there.
  - base64 stays without `alloc`: tokens are encoded with `encode_slice` into a fixed buffer.
- **One row per family** *(the user's choice)*. A `sessions` row is one login's token family; its ID (`fleet_core::id::SessionId`) stays the same across every refresh, so E's `/me/sessions` and `DELETE /me/sessions/{id}` address it. The row holds the hashes of the current access and refresh tokens. Every refresh token it replaced moves into `rotated_refresh_tokens(hash, session_id, rotated_at)`, kept until the session is deleted, so reusing any rotated token is recognized while the family exists.
- **Access tokens live in the session row** *(the user's choice)*. A refresh replaces both hashes, so the previous access token stops working the moment the refresh commits. A request still in flight with it gets 401; P8.1's client handles that (a note there).
- **What is hashed** *(the user's choice)*: SHA-256 of the whole token text, prefix included, stored as a 32-byte BLOB (`CHECK (length(…) = 32)`).
  - A strict shape check comes first: the prefix, then exactly 43 base64url characters. Anything else is refused before any lookup, so a refresh token sent as a Bearer token never reaches the database.
  - After the indexed lookup, the stored hash is compared with the computed one by `TokenHash::ct_eq` (`subtle`). The index lookup itself can't be constant-time; that's harmless, since a hash of 256 random bits can't be inverted.
  - `TokenHash` has no `PartialEq`, and its `Debug` shows no byte.
- **Expiry from anchors and the running config** *(the user's choice)*. Only `created_at` (the absolute anchor) and `refreshed_at` (when the current pair was issued) are stored. `fleet_core::auth` computes:
  - access expiry = `refreshed_at` + access TTL
  - idle expiry = `refreshed_at` + idle lifetime *(the user's choice: idle counts from the last refresh, not the last request)*
  - absolute expiry = `created_at` + absolute lifetime

  Access and idle expiry are capped at the absolute expiry; a time is expired when `now >= expiry`. Lowering a TTL shortens every existing session at the next restart; raising one lengthens them.
- **`[auth]`'s token keys** *(the user's choice)*: `access_token_ttl_secs` 900 (60–3600), `refresh_token_idle_days` 7 (1–90), `refresh_token_absolute_days` 30 (1–365). Validation refuses idle > absolute. All three are on `NOT_SECRETS` with their reasons. Problems are reported under the key in garde's words, like the argon2 keys.
- **No client data** *(the user's choice)*: a session stores no IP address and no user agent. The login's IP is in D's `auth.login` entry. A nullable column can be added by a later migration.
- **`last_used_at`, the touch** *(the user's choices)*:
  - **Display only:** expiry never depends on it.
  - **At most once a minute:** `TOUCH_INTERVAL` = 60 s, decided by the pure rule `touch_due`.
  - **Conditional:** `UPDATE … SET last_used_at = ? WHERE id = ? AND last_used_at <= ?` (now − 60 s), so two concurrent requests that both read a stale value write it once, and a clock that went back never moves it backwards.
  - **Bounded:** only `Store::write()` (the wait for the write connection and `BEGIN IMMEDIATE`) runs under a 250 ms timeout. Once the transaction is open, the UPDATE and the commit take milliseconds and run to the end; a future dropped there would leave the outcome and the connection's cleanup to sqlx's cancellation handling.
  - **Never fails a request:** past the bound, or on any error, the touch is skipped and logged at `debug`, and the request goes on. The next due request writes it.
- **Unaudited writes** *(the user's choices; ADR-0015's rule)*: `WriteTx::commit_unaudited(UnauditedWrite)` commits without an entry, for named exemptions only:
  - `SessionTouch`: the touch above. An entry per request would bury the log, and it's display data.
  - `SessionRotate`: a successful refresh. It happens about every 15 minutes per active client and says nothing new: the session's creation is audited by D's `auth.login`, and its end by logout, reuse, disabling or a role change.
- **The refresh rule** *(the user's choices)*, a pure function in `fleet_core::auth`, checked in this order inside the write transaction, whose lookup joins `users`:
  1. an unknown token: 401
  2. a revoked family: 401
  3. an expired family (idle or absolute): 401
  4. a rotated token of a live family: **reuse**. The family is revoked and the entry recorded in the same commit, even for a disabled user.
  5. the current token of a disabled user: 401, nothing rotates (a session whose user row is gone isn't found at all: step 1)
  6. otherwise the tokens rotate

  There's no grace window: a client whose refresh response got lost retries with the old token, which is reuse, so it must log in again (a note under P8.1). Reuse in a revoked or expired family is a plain 401 with no entry, so replays can't flood the log. The lookup runs inside the write transaction, so two refreshes with the same token take turns (`BEGIN IMMEDIATE`) and the second finds it rotated. New tokens are minted before `Store::write()`, since no crypto runs while a `WriteTx` is open.
- **`security.refresh_token_reuse`** *(the user's choices)*:
  - outcome Failure (ADR-0015)
  - no actor: whoever presented the old token isn't authenticated and may be the thief
  - `actor_ip`: the presenting client's, passed in by D's `/auth/refresh`
  - target `User(the session's user)`: the lasting link is whose account was attacked, and it survives the session row's cleanup
  - metadata `session_id` (the revoked family) and `rotated_at` (when the presented token was replaced), so an investigation sees which session was hit and whether the thief or the real app used the old token first

  `AuditTarget::Session` arrives with D's `auth.logout`, where the actor is the session's own user.
- **Typed metadata values** *(the user's choice)*: `AuditMetadata::id(key, impl Into<Uuid>)` (lowercase hyphenated) and `time(key, DateTime<Utc>)` (UTC milliseconds, the database's form), stored as JSON strings. **The naming rule's metadata clause is clarified:** values are closed-enum names or typed values the server made (IDs, timestamps), never text a client sent.
- **Issuing a session** *(the user's choice)*: `TokenService::prepare(user_id)` mints both tokens, their hashes, the session's ID and its times before any transaction. The caller inserts it with `tx.sessions().insert(…)` and commits with its own entry: D's login with `auth.login`, in the same transaction as its other writes. B's tests commit with a test-only action built through `AuditAction::try_from`, never listed in `audit_actions!`, so it can't enter `AuditAction::ALL` or the names snapshot *(the user's addition)*.
- **Step-up in B** *(the user's choice)*: the `stepped_up_at` column, `STEP_UP_WINDOW` (5 minutes, a constant: §7.2 fixes it and Appendix A has no key) and the pure `is_stepped_up(stamp, now)`: a stamp at most 5 minutes old counts, a stamp in the future (a clock that went back) doesn't. `AuthUser` carries the stamp. D adds `ErrorCode::StepUpRequired` with its first step-up route.
- **`AuthUser`** *(the user's choices)*:
  - It looks up the session and its user on every request, through one join on the read pool, so revocation, disabling and role changes count on the very next request.
  - **Every rejection is `ApiError::Unauthorized`** with a bare `WWW-Authenticate: Bearer`: no header, more than one `Authorization` header (it isn't a list header, and a proxy and the server must not disagree about which one counts), another scheme, a malformed token, an unknown one, an expired access token, a revoked session, a disabled user.
  - **A database failure isn't a rejection:** busy is 503 and anything else 500, through `From<StoreError>`. A 401 would make the app throw its tokens away over a database hiccup.
  - `require(permission, resource)` returns `authorize()`'s `AuthzError`, which a handler turns into `ApiError` with `?`.
- **Logging** *(the user's choice)*: a detected reuse is one `warn` with `user_id` and `session_id`. Each `AuthUser` rejection is one `debug` line naming its reason, with `session_id` and `user_id` where known. No token, hash or header value is ever logged, at any level, and the reason never reaches the client.
- **Repository scope** *(the user's choice)*: only what B uses: insert; the joined lookup by access hash on the read pool; inside the transaction, the lookup by refresh hash (current or rotated), rotate and revoke; the conditional touch. Logout reuses revoke in D; revoking all of a user's sessions and listing them arrive with E.
- **Layout** *(the user's choice)*:
  - `ports::tokens`: `TokenKind`, `TokenHash` and the synchronous `Tokens` trait. It's on the auth gate, since `ct_eq` is security code.
  - `infra::crypto::tokens::Sha256Tokens`.
  - `ports::sessions` and `infra::sqlite::sessions`: the repository, under the workspace gate like every repository.
  - `app::auth::TokenService`, where D's login joins it.
  - `http::extractors`: `AuthUser`.
  - New auth-gate entries: `ports/tokens.rs`, `infra/crypto/tokens.rs`, `app/auth.rs`, `http/extractors/`.
- **Wiring** *(the user's choice)*: `AppState::new(health, tokens)`; `serve` builds `TokenService` from `[auth]`'s lifetimes, `SystemClock`, `OsRandom` and the database.
- **Cleanup comes with P11.6** *(the user's choice)*: a session, with its history rows (ON DELETE CASCADE), is deleted once `created_at` + the current absolute lifetime has passed, whether it was revoked or not. A revoked session is unusable anyway, and keeping its history until then lets an investigation after a reuse see the family's timeline. The migration has the indexes this needs: `sessions(created_at)` for the cleanup, `sessions(user_id)` for revoking and listing a user's sessions, `rotated_refresh_tokens(session_id)` for the cascade.

### As built (group B)
- **P7.1's `sessions`** (`0003_sessions.sql`): the conventions of 0001 and 0002 (STRICT, ID and time shapes); hashes as BLOBs with a length CHECK; no ordering CHECKs. `user_id` references `users` without cascade, like the audit log's actor. Every lookup joins `users`, so a session whose user row is gone isn't found.
- **`SessionId`** joins fleet-core's IDs, and the `serialized_ids` snapshot gains it.
- **Tests,** red against compiling stubs (a migration without CHECKs or cascade, a repository that found and wrote nothing, a `TokenHash` that compared nothing and read nothing back, a `commit_unaudited` that rolled back): 21 of 26 repository tests and 3 of 8 `TokenHash` tests failed on assertions. The 5 repository tests that passed assert that nothing is found or that a row is refused. Then all passed.
- **P7.3, the pieces:**
  - `fleet_core::auth::{SessionPolicy, SessionTimes, RefreshFacts, refresh_decision}`. `SessionPolicy::try_new` refuses a zero lifetime, an access lifetime not shorter than the idle one, and an idle lifetime longer than the absolute one; its `Default` is §7.2's.
  - `ports::tokens::{Tokens, TokenKind, MintedToken, MintTokenError}` and `infra::crypto::tokens::Sha256Tokens`. The token is built in `Zeroizing` buffers of fixed size and copied once into a `SecretString` of exactly its length. A known-answer test pins the hash (computed with Python's hashlib and Node's crypto).
  - `app::auth::TokenService::{prepare, refresh}`. `refresh` mints the new pair before `Store::write()`, confirms the indexed match with `ct_eq`, and matches the rule's decision together with the lookup, so no impossible combination is handled by hand.
  - **The slot for typed metadata.** `AuditMetadata`'s reserved room per free slot grows from the largest integer to the largest ID or time in quotes, so an ID or a time is never cut, whatever came before.
  - **`[auth]`'s ranges** are checked by one helper for the argon2 and the token keys, in garde's words; idle > absolute is the new `ProblemKind::GreaterThanKey`, checked only when both are in range.
- **P7.8, the pieces:**
  - `fleet_core::auth::{access_decision, AccessFacts, AccessRejection, touch_due, touch_stale_since, TOUCH_INTERVAL, is_stepped_up, STEP_UP_WINDOW}`.
  - `TokenService::authenticate` and its touch (`TOUCH_WAIT` = 250 ms around `Store::write()` only), with `RejectedAccess` naming the reason for the log.
  - `http::extractors::AuthUser` (`FromRequestParts<AppState>`, `Rejection = ApiError`) with `actor()`, `session_id()`, `stepped_up_at()` and `require()`. It counts the `Authorization` headers first, then reads `TypedHeader<Authorization<Bearer>>`, whose `Debug` would show the token, so only its text goes on, as a `SecretString`.
  - `AppState::new(health, tokens)`; `serve::app` builds `Sha256Tokens` and `TokenService` over the real clock and randomness.
- **The scheme's spaces** *(the user's decision during the build)*: the plan described headers 0.4 as allowing exactly one space after `Bearer`. Its `Bearer::token()` trims them (`["Bearer ".len()..].trim_start()`), as RFC 6750's `"Bearer" 1*SP b64token` allows, so one or more spaces are accepted; the token's shape check does the rest.
- **P7.8, tests,** through a test-only router behind the real middleware stack, red against compiling stubs (an extractor that didn't count the headers, a service that skipped the decision and the touch): 7 of 15 failed on assertions, then all passed. The held-write-connection test passed against the stub too, which never touched. fleet-core's access rules: 9 of 18 failed against stubs, then all passed.
- **P7.3, tests,** red against compiling stubs (rules that allowed everything, typed metadata that added nothing, an adapter that minted nothing, config that ignored the keys, a service that refused every refresh): fleet-core 20 of 25 rule tests, 5 metadata tests and the names snapshot; fleet-server 11 adapter tests, 8 config tests and 13 of 21 service tests failed on assertions. The ones that passed against the stubs check refusals and `Debug`. Then all passed.

### Group C: `login_attempts`, the route registry, rate limits and the lockout (P7.1's `login_attempts`, P7.11, P7.10)
The user answered group C's questions on 2026-10-11, one at a time.

- **No tower_governor** *(the user's choice)*. tower_governor 0.8.0 (checked in its source on docs.rs):
  - hard-codes governor's quanta clock, so no clock can be injected and its limits can't be tested deterministically
  - depends on governor with its default features, which turns on `quanta`, `dashmap` and `jitter` (rand 0.9, getrandom 0.3) for the whole workspace
  - turns on axum's default features through its `axum` feature
  - reports its wait in whole seconds, rounded down

  fleet-server uses the workspace's governor with `std` only (already fleet-runtime's): keyed `RateLimiter::hashmap_with_clock` limiters behind the server's own axum middleware. Every refusal is `ApiError::RateLimited { retry_after }` with governor's `NotUntil::wait_time_from`, which the error model rounds up. tower_governor leaves §5 and CLAUDE.md's registry. The middleware keeps tower_governor's useful part: `retain_recent()` on a timer.
- **The limiters' clock** *(the user's choice and boundary)*. The limiters run on the server's `Clock` port, as nanoseconds since its first reading (saturating at 0 if it goes back).
  - **In production:** a new `infra::system::MonotonicClock` drives **only** the limiters. It's one `SystemClock` reading as the anchor plus tokio's `Instant` elapsed, in whole milliseconds, so a wall-clock step back can't refuse every client until the clock catches up.
  - **Everything stored or audited** (sessions, `login_attempts` and the lockout, audit entries, the touch) keeps `SystemClock`. A monotonic clock ignores NTP corrections and, on Linux, the time a VM spent suspended, so stored times would drift from real time over a long uptime; the limiters only measure intervals.
  - `serve` wires both clocks explicitly. Tests pass the same `ManualClock` to both, so one `advance` moves sessions, the lockout and the limiters together.
- **Health counts against the global per-IP limit** *(the user's choice)*. The global per-IP limit sits in the middleware stack and covers every request, matched or not, so 404 scans count too. Docker's and Caddy's probes stay far below 20/s, and `/health/ready` pings the database.
- **The classes** *(the user's choices)*. C defines every HTTP limit of §7.4 now, each with its `[rate_limits]` keys, so D and E only register routes with an existing class:
  - `Standard`: only the global per-IP limit
  - `Login`: 5/min per IP, **one bucket shared by `/auth/login` and `/auth/login/mfa`**
  - `Refresh`: 30/min per IP
  - `Register`: 5/h per IP
  - the per-user limit on every authed route

  Class limits are a `route_layer`, so a 404 or 405 spends none of them. The per-user limit runs in `AuthUser`, right after authentication. The chat keys and `ws_connections_per_user` come with P11.
- **The client's address** *(the user's choices and additions)*:
  - **The peer:** `ConnectInfo<SocketAddr>` from `into_make_service_with_connect_info`, canonicalized, so an IPv4 peer that a dual-stack `[::]` listener reports as `::ffff:a.b.c.d` still matches an IPv4 CIDR. Without that, Caddy would count as untrusted and every client would land in Caddy's one bucket.
  - **X-Forwarded-For** counts only when the peer is in `[http] trusted_proxies`:
    - all XFF headers form one list, in order, each entry trimmed and canonicalized
    - the list is walked from the right past trusted proxies, and the first untrusted address is the client, never the left-most one, which the client controls
    - an entry that isn't a plain IP address (garbage, `unknown`, a port) stops the walk at the last trusted hop
    - if every entry is trusted, the left-most is the client
    - `Forwarded` and `X-Real-IP` are never read
  - **A request without `ConnectInfo`** is a wiring bug: 500, logged at `error`.
  - **The limiter key:** an IPv4 client by its address, an IPv6 client by its /64, since one host usually gets a whole /64. Only the key is masked; audit entries record the full address.
- **`IpNet`, our own** *(the user's choice)*: a small value type with `addr/len` parsing, `contains` and the /64 masking, no new crate.
  - **`[http] trusted_proxies` rules:** at most 32 CIDRs, default empty; host bits must be zero; a bare address is refused; duplicates are refused. Each problem names the key, never the value.
  - **The cross-check** *(the user's addition)*: a property test compares `contains` and the /64 masking with `ipnet` 2.12.2, a fleet-server **dev-only** dependency, already in Cargo.lock through reqwest. Production code doesn't use it.
- **The lockout's key** *(the user's choice)*: a `login_attempts` row is keyed by HMAC-SHA256(lockout key, normalized `Username`), a 32-byte BLOB, never the name: people type their password into that field (P7.13's note), and a database or backup leak mustn't let anyone test guesses offline.
  - **The lockout key** is derived at startup from the vault's active key as HMAC(key, `"afkfleet login_attempts v1"`). It's never in the database or a backup, and there's nothing new to configure. Rotating the active vault key resets every streak, which is harmless.
  - **New direct dependency:** `hmac` 0.13.0, RustCrypto like sha2 0.11, already in Cargo.lock.
- **Unknown names lock exactly like real ones** *(the user's rule)*. Rows hold no user ID and are the same for any name, and every lookup takes the same path. Only the audit entry's target differs, and the audit log is Admin+.
- **The schedule** in `fleet_core::auth` *(the user's choices)*:
  - **Locks:** the threshold's failure (5) locks for 1 min, and each further failure doubles the lock, capped at 1 h.
  - **Streak expiry:** a streak expires 24 h after its last failure (D's successful login clears it too). A steady attacker gets about 30 guesses on the first day and about 24 a day after; waiting for an expiry buys 5 a day.
  - **Configuration:** `[auth] lockout_threshold` (default 5, 3–20) is the only key. The first lock, the cap and the 24 h expiry are constants, as §7.4 fixes them.
- **While locked** *(the user's choice)*: 429 `rate_limited` with `Retry-After` = the rest of the lock, before any password check. The refusal isn't counted, so hammering can't extend the lock.
- **One attempt per name in flight** *(the user's choice)*. An in-memory guard (a std `Mutex<HashSet>`, never held across `.await`, released on drop) lets one attempt per key run; a concurrent one gets 429 with `Retry-After: 1`. Without it, parallel attempts from many addresses could all pass the check before the first failure is written.
- **`security.lockout`** *(the user's choice)*:
  - **When:** recorded in the failing attempt's own transaction each time a failure starts a lock (the 5th, 6th, …).
  - **The entry:** outcome Failure; no actor; the client's IP; target `User(id)` when the name is a user's, none otherwise; metadata `failures` (an integer) and `locked_until` (a time).
  - **The log:** one `info` line with `user_id` when known, never the name. `warn` is for recovered faults, and users who mistype are common.
- **Purging expired streaks** *(the user's choice and refinement)*. The server's periodic housekeeping deletes rows whose last failure is at least 24 h old and that have no running lock:
  - in batches of 1000, each batch its own short transaction, repeating until a batch comes back short, so an attacker spraying names can't outgrow it
  - each batch is committed with a new named exemption, **`UnauditedWrite::LoginAttemptsPurge`**: it removes only rows the rule already treats as absent, so an entry per batch would say nothing
  - separate transactions keep the one write connection free for logins and touches between batches
  - the run stops at shutdown between batches
- **The housekeeping task** is the server's first long-lived background task. It runs every 60 s:
  - the limiters' `retain_recent()`
  - the purge
  - a summary of the refusals since the last run (below)

  It belongs to `serve`'s `JoinSet` and stops with a child of the shutdown token at the first signal. The close stage waits for it before closing the database, within the same deadline.
- **Refusals don't flood the log** *(the user's addition)*. The trace layer logs a 429's "request finished" line at `debug`, like the health routes. The limiters count their refusals per class in atomic counters, and the housekeeping logs one `info` line with the counts since its last run, only when any is non-zero, then resets them. Under a flood, the log grows by one line a minute, not thousands a second (Docker's default log driver doesn't rotate).
- **The route registry** *(P7.11)*:
  - **The only way in:** `public_route(class, routes!(…))` and `authed_route(class, routes!(…))` on the `Routes` builder, which wraps utoipa-axum's `OpenApiRouter`, so every route keeps its OpenAPI entry and gets its class and its registry entry at registration.
  - **The health routes** move to `public_route`. The dev-only `GET /api/openapi.json` is recorded as public with the `Standard` class, and stays out of the document.
  - **The route-coverage test** builds the production routes plus a **test-only authed route** whose handler takes `AuthUser`, so the authed list is never empty. It then calls every authed route without credentials and expects 401; D's and E's routes join automatically. It checks the public allowlist in dev and normal mode, and that the registry equals the document's operations, so a route added past the helpers fails.

### As built (group C)
- **P7.1's `login_attempts`** (`0004_login_attempts.sql`):
  - **Columns:** `username_key` (a 32-byte BLOB, the primary key), `failures` (`>= 1`), `last_failure_at` and `locked_until` (NULL until a lock), with the time shapes of 0001–0003.
  - **Index:** on `last_failure_at`, for the purge.
  - **No `user_id`, no name:** a real user's row and an unknown name's look alike.
  - **The repository:** `find` on the read pool and in the transaction, `upsert`, `delete`, and `purge_stale(failed_by, now, limit)`, which chooses its rows through a `rowid IN (SELECT … LIMIT ?)` subquery, since the bundled SQLite has no `DELETE … LIMIT`.
  - **`AttemptKey`** has `ct_eq` and no `PartialEq`, and its `Debug` shows no byte, like `TokenHash`.
  - **Tests,** red against compiling stubs (a repository that stored and found nothing, a key that compared and read back nothing): 14 of 18 failed, the migrations' snapshot among them. The four that passed check the redacted `Debug`, that nothing is found (twice), and the table's CHECKs, which the real migration already enforces. Then all passed. The snapshot gained line 4, read before accepting.
- **P7.11, the registry** (`http::routes`):
  - **Recording:** `Routes::register` reads each route's methods and paths from the `OpenAPI` path items its `routes!` call carries: GET, PUT, POST, DELETE, OPTIONS, HEAD, PATCH and TRACE. utoipa 6's `query` and additional operations aren't methods axum routes; one would differ from the document, which the registry's test compares.
  - **`Debug`:** `Routes` shows its entries only, since utoipa-axum's router has none.
  - **Tests,** red against a registry that recorded nothing: 8 of 9 failed (only the path-parameter helper passed), then all passed. The existing health and `OpenAPI` tests stayed green through the new builder, the document's snapshot unchanged.
- **P7.10, the pieces:**
  - `fleet_core::auth::lockout`: `LockoutPolicy` (3–20, default 5), `FIRST_LOCK`, `MAX_LOCK`, `FAILURE_MEMORY`, `check_lock`, `after_failure`, `is_expired` and `expired_failed_by`.
  - `config::{RateLimitConfig, Rate}`: a `Rate` is a replenish interval and a burst. `per_minute` and `per_hour` allow their whole count at once, as governor's quotas do. `HttpConfig::trusted_proxies` and `AuthConfig::lockout` were added, with `ProblemKind::{IpNet, TooManyEntries, Duplicate}`.
  - `http::rate_limit`: `IpNet`, `TrustedProxies` and `limiter_key`; `client_ip` and the `ClientIp` extension and extractor; `LimiterClock`, `RateLimits` (`check_ip`, `check_class`, `check_user`, `retain_recent`, `tracked`, `take_refusals`); and the two middlewares.
  - **`infra::system::MonotonicClock`.**
  - `ports::login_attempts::AttemptKeys` and `infra::crypto::attempt_keys::HmacAttemptKeys`. They share `hmac_sha256` with `VaultKeys::derive_key(DerivedKey::LoginAttempts)`.
  - `app::lockout::LockoutService` (`begin`, `Attempt::record_failure`, `Attempt::clear`, `purge_stale`, `take_refusals`) and `From<LockoutError> for ApiError`.
  - **`cli::housekeeping`:** `Housekeeping`, `INTERVAL` and the generic `every`. `serve::App { router, housekeeping }`.
- **The peer through axum's extractor.** The per-IP layer reads the peer with `ConnectInfo`'s own `FromRequestParts`, not the raw extension: tests' `MockConnectInfo` stores itself, not a `ConnectInfo`, and axum's extractor falls back to it. The first wiring read the extension directly and failed every `MockConnectInfo` test with the missing-peer 500, which the full suite caught.
- **The HMAC without a fallible constructor.** `hmac_sha256` passes the 32-byte key as HMAC-SHA256's zero-padded 64-byte block, which RFC 2104 defines as the same HMAC; built that way, nothing can fail. Known-answer tests computed with Python's `hmac` and Node's `crypto` pin it, the derivation and two row keys.
- **No unwiped copy.** hybrid-array's `zeroize` isn't on in this build, so the padded key block is a `Zeroizing<[u8; 64]>` passed by reference, and the tag is wiped through its byte slice. The derived lockout key goes straight into its `SecretBox`.
- **governor's `retain_recent`** drops a key only one replenish interval after its bucket is full again (`now − t ≥` its theoretical arrival time). The first test assumed "once full" and was corrected to governor's documented behavior; the doc comment says so.
- **The housekeeping's first run** is one interval after the start (`interval_at`, `MissedTickBehavior::Delay`). A run isn't cut short by the stop: the purge checks the token between its batches.
- **The lockout's `info` line** is logged when the failure is recorded, inside the caller's transaction; if that commit then fails, its own error is logged too.
- **The lockout's refusals** are counted like the limiters' (`locked`, `in_flight`) and are part of the housekeeping's summary, so a flood of locked logins doesn't flood the log either.
- **Lockfile:** only the direct edges of governor, hmac and ipnet; no new package.
- **Coverage gates:** the auth gate gains `http/routes.rs` (the route registry), `http/rate_limit/` (the rate-limit middleware), `app/lockout.rs`, `infra/crypto/attempt_keys.rs` and `ports/login_attempts.rs` (`AttemptKey::ct_eq`, like `ports/tokens.rs`). The vault gate already covers the derivation in `vault.rs`.
- **P7.10, tests,** each written first and red against compiling stubs:
  - the lockout rule: 25 of 40
  - `IpNet`: 33 of 46
  - config: 19 of 21
  - the limiter and the client's address: 19 of 33
  - the router-level limits: 13 of 16
  - the HMAC, the derivation and `MonotonicClock`: 9 of 13
  - the lockout service (P7.12's lockout counting): 16 of 19
  - the housekeeping: 5 of 8
  - the real-socket test, with `serve` stripped of `into_make_service_with_connect_info`: it saw a 500
  - the housekeeping's stop, with a token the shutdown never cancels: it hit its 30 s bound

  Then all passed. The ones that passed against the stubs check allowances, refusals, `Debug` or labels that a do-nothing stub satisfies too. The router-level tests went red against stubs swapped back in after the checks existed, since they were written once the limiter was. Two new snapshots, the 429 response and the audit-action names with `security.lockout` added, were read before accepting. One existing test changed on purpose: the list of known config sections gained `rate_limits`.
- **Tests that could block** bound their waits (this ADR's rule): the housekeeping's paused-time tests wait for its task at most 10 s of paused time. A first version without that bound hung against the stub and was stopped.
- **Amended in group D (from the group C review): no /0 proxy.** `[http] trusted_proxies` accepted `0.0.0.0/0` and `::/0`, and trusting every address lets any client choose its own address through `X-Forwarded-For`: past every per-IP limit, login's included, and into the audit log's IPs. `IpNet` refuses a prefix length of 0 for both families (`IpNetError::Everything`, "a prefix length of 0 would trust every address; list only your proxies' own networks"). The unit tests that had used /0 as a valid network now use /1 networks, and the proptests draw lengths from 1.
- **Open for later groups** *(the user's notes, recorded under their tasks)*:
  - P7.6, refresh checked on the read pool first
  - P7.7, 2FA codes and the lockout
  - P7.12 and the threat model, keeping an account locked as an accepted residual risk
  - P12.7, the smoke test proving `trusted_proxies` works through Caddy

### Group D: login, 2FA, refresh, logout and step-up (P7.1's `user_mfa`, P7.7, P7.6's `login`, `login/mfa`, `refresh`, `logout`, `POST /auth/step-up`)
The user answered group D's questions on 2026-10-11, one at a time, and amended the plan in review.

- **Dependencies** *(the user's choice)*: totp-rs 6.0.0 with no default features plus `otpauth` and `zeroize` (no `std`, so no wall-clock `*_current`; no `gen_secret`, so no rand; no `serde`; no `qr`), and qrcodegen 1.8.0, which has no dependencies. The lock gains totp-rs, base32, constant_time_eq and qrcodegen; url, percent-encoding, sha1 0.11, hmac 0.13 and sha2 0.11 were already there. axum's `json` feature (P7.6's Phase 6 note) brings serde_path_to_error. fleet-api-types gains garde, zeroize and secrecy, all workspace crates.
- **The QR code is SVG, not PNG** *(the user's choice, a deviation from P7.7)*. totp-rs's `qr` feature goes through qrcodegen-image and image 0.25, which pull in moxcms, pxfm, png, fdeflate and a second miniz_oxide, some with SIMD and unsafe code. Our own small writer turns qrcodegen's grid (ECC Medium, a 4-module quiet zone) into one `<path>` of unit squares in a `viewBox`, black on white, and nothing else: no text, links, scripts or external references. The enroll response carries it as `data:image/svg+xml;base64,…` next to the otpauth URI; both contain the secret, so they're never logged and the response is `no-store`. P8 shows it only through `<img src="data:…">`, never as HTML.
- **Where the login challenges live: in memory** *(the user's choice)*. The mfa token (`afk_mt_`, 5 min) and the setup token (`afk_st_`, 10 min) are 32 bytes from `SecureRandom`, base64url behind their prefix, shape-checked and held only as SHA-256 through the Tokens port (`TokenKind::{Mfa, Setup}`).
  - **Two slots per user:** the login slot holds an mfa or setup challenge, and a new password step replaces it; the replace slot holds a 2FA replacement's setup token, and a new replacement replaces it. A login challenge never replaces a live replace token, nor the other way round. So the map is bounded by twice the number of users.
  - **No migration, no write at the password step, no new unaudited exemption.** A restart drops them, and the user logs in again.
  - **Enroll's pending secret** waits in its setup challenge (wiped on drop) until confirm writes it.
- **`user_mfa` and `recovery_codes`** *(the user's choice)*, in `0005_user_mfa.sql`: `user_mfa(user_id PK, secret_envelope, last_used_step, confirmed_at)` holds confirmed enrollments only, so a user has 2FA exactly when their row exists; `recovery_codes(hash PK, user_id, used_at)` keeps a used code's row. No `key_id` column: the envelope's header carries it, and P9's rotate-key reads it there.
- **Recovery codes are plain SHA-256** *(the user's rule)* of their normalized text, looked up by hash and checked against the user's ID. That's safe only because every code carries at least 80 bits (100 in fact) from `SecureRandom`, so a leaked database can't be brute-forced offline. Not a keyed hash: a key derived from the vault's active key would make every recovery code unusable after P9's key rotation, since the hashes can't be recomputed without the codes; unlike the lockout's counters, that loss wouldn't be harmless. The migration's comments say both.
- **The vault's AAD** *(the user's choice)*: `record` = the user's ID, `VaultPurpose::TotpSecret`. An envelope copied into another user's row fails to open.
- **Enroll and confirm take only a setup token** *(the user's choice)*, as `Authorization: Bearer afk_st_…`, under a new registry kind `Access::Setup`, so there's one way in, as §7.2 says.
  - **Enroll is repeatable** *(the user's choice)*: each call makes a new pending secret. Confirm before any enroll is 409.
  - **A wrong or malformed code at confirm** is 422 on `code`, keeps the token, and never counts toward the lockout: whoever holds the token chose the secret, so there's nothing to guess.
  - **Confirm is single-use under concurrency** *(the user's review)*: once the code matched, it takes its token out of the challenge store atomically, before the commit, so a second concurrent confirm with the same token gets 401. A first confirm only inserts (a row already there: 409, nothing written); only a replacement replaces.
  - **The confirming code's step becomes `last_used_step`** *(the user's review)*, so that code can't log in afterwards. P8 asks for the next code after setup.
  - **The enroll response** *(the user's choice)*: `{otpauth_uri, qr_svg, secret}`, the last as base32 for typing in by hand.
- **TOTP** *(the user's choice)*: SHA-1, 6 digits, 30 s steps, ±1 step, a 20-byte secret, issuer "afkfleet", label `afkfleet:<username>`, all constants. A code is accepted only for a step strictly after `last_used_step`, which then becomes the matched step. The window and the replay rule are pure in `fleet_core::auth`; totp-rs only computes one step's code and builds the URI, and codes are compared with `subtle`.
- **Recovery codes** *(the user's choice)*: 20 base32 characters (100 bits), shown as `abcd-efgh-ijkl-mnop-qrst`; normalized by lowercasing ASCII and dropping `-` and spaces.
- **login/mfa takes one `code` field** *(the user's choice)*: 6 digits are a TOTP code, 20 base32 characters a recovery code, anything else is 422 (not counted, no attempt spent).
- **Wrong codes count like a wrong password** *(the user's choice; group C's open question)*: login/mfa calls `LockoutService::begin` on the challenge's user before the check, `record_failure` goes into the failure commit and `clear` only into the commit that issues tokens, so knowing the password never resets the code count. **3 attempts per mfa token** *(the user's choice)*; a used-up, expired or unknown token answers like a wrong code.
- **Login's refusals** *(the user's choices)*: a name that isn't a valid `Username` is 422 on `username` before the lockout or any hashing (the rules are public, so it tells nothing about accounts). The password field is capped at 1024 `chars()` at login and step-up (422 beyond). A disabled user answers like a wrong password, counted, audited with `reason` = `disabled`, at both steps.
- **The error codes** *(the user's choices)*: a new `invalid_credentials` (401) at login and login/mfa; `step_up_required` (403); a wrong step-up password is 403 `forbidden`. `WWW-Authenticate: Bearer` is keyed on the 401 status, not on one code, in both places that set it. A refused refresh stays 401 `unauthorized`. Token and code fields are capped at 64 characters (422 beyond); a wrong shape answers like an unknown token.
- **Re-hashing** *(the user's choice; group A's open question)*: at the password step, when `needs_rehash` (any difference from what `hash()` would produce now: variant, version, m, t or p, up or down, so a stored hash's timing matches the dummy hash), the same password is re-hashed through a private path without the strength check. It's best-effort: a full queue skips it with a `debug` line, a hashing failure with a `warn` line, and the login never fails because of it. The new hash waits in the mfa challenge and is written in the `auth.login` success commit (metadata `rehashed` = true), after the second factor passed. It writes only `password_hash`, never `password_changed_at`, so two concurrent stale logins both succeed. Users without 2FA get it at their first full login.
- **The re-check rule** *(the user's rule)*: every commit that acts on a verified password re-reads, inside its own transaction, `password_changed_at`, the user's `disabled`, and the session where there is one.
  - login/mfa: refused with `invalid_credentials`; a disabled user is counted (`disabled`), a changed password audited as `password_changed` and not counted.
  - A first confirm: answers like an expired setup token (401, the token is consumed, nothing written).
  - Step-up: a dead session or a disabled user is 401; a changed password is 403, audited as `password_changed`, not counted; nothing is stamped, and the stamp checks that exactly one row changed.
  - A replacement's confirm: as a first confirm, and its bound session must still be live.
  - **A replacement between the code check and login's commit** *(the user's review)*: `advance_step` also matches the `confirmed_at` read with the secret, so a code of the old secret can't log in once the replacement committed (`mfa_changed`, not counted). Recovery codes are already safe: the replacement deletes them, so `use_code` changes no row.
- **Refresh checks the read pool first** *(the user's choice; group C's open question)*: `refresh_hash_exists` (current or rotated) answers an unknown token 401 without taking the one write connection; known tokens take B's race-safe path unchanged.
- **Step-up** *(the user's choices)*: a plain authed route with `RateClass::Login` plus the per-user limit (not a step-up route itself, or nobody could step up). `begin` on the session user's name; a wrong password is `record_failure` in an `auth.step_up` Failure commit and 403; the right one stamps and clears in an `auth.step_up` Success commit, 204. No re-hash here.
- **The 2FA is replaced, never reset** *(the user's decision, replacing `DELETE /me/mfa`)*: otherwise someone with the password could poll login and grab a setup token the moment the 2FA is gone. `POST /me/mfa/reset` (authed and step-up) answers 200 `{setup_token, expires_in}`, a replace token bound to this session; nothing else changes yet, and the old TOTP and recovery codes keep working. Confirm with it swaps `user_mfa` and the recovery codes, revokes every other session of the user (`revoke_all_for_user(user, except)`, which E reuses) and records `mfa.reset` with `sessions_revoked`, in one commit.
- **Limits** *(the user's choices)*: enroll, confirm and reset use `Standard` plus the per-user limit (the `SetupToken` extractor spends the token's user's, as `AuthUser` does); login, login/mfa and step-up `Login`; refresh `Refresh`; logout `Standard`.
- **Audit** *(the user's choices)*:
  - `auth.login` Success when tokens are issued: actor and target the user, IP, `session_id`, `method` (`totp` | `recovery_code`), `rehashed` only when true.
  - `auth.login` Failure on every counted refusal, inside `record_failure`'s commit: no actor, IP, target the user (none for an unknown name), `step` (`password` | `code`) and `reason` (`wrong_password` | `wrong_code` | `disabled` | `password_changed` | `mfa_changed`).
  - `auth.logout`: actor the user, target the new `AuditTarget::Session`, IP.
  - `auth.step_up` Success and Failure: actor the user, target the session, IP, and the same `reason` values as login.
  - `mfa.enroll`: a first enrollment's confirm. `mfa.reset`: a replacement's confirm, with `sessions_revoked`.
  - Never audited: 422s, 429s, unknown, expired or used-up tokens, and starting a replacement (nothing is written).
- **The responses** *(the user's choice)*: the password step 200 `{status, token, expires_in}`; login/mfa and refresh 200 `{access_token, refresh_token, access_expires_in, refresh_expires_in}`; confirm 200 `{recovery_codes}`; logout and step-up 204. Lifetimes are integer seconds. P8 uses `expires_in` to restart a timed-out step itself and refreshes only on a 401, never on a timer.
- **Secrets in DTOs** *(the user's choice and rules)*: `fleet_api_types::SecretText`, serde-transparent, `#[ts(type = "string")]`, a string schema with maxLength, a `[redacted]` Debug and wiped on drop. It has no Display, PartialEq, Hash or Deref; it's read only through `expose()`, and the server takes it with `into_secret()`, so every use is greppable. garde's messages never contain the value, and a test formats every DTO holding one.
- **axum's JSON rejections** *(the user's choice)*: a missing or non-JSON content type is 415; malformed JSON and a wrong shape (an unknown, missing or mistyped field) are 400; only garde's rules give 422. serde's message never reaches a response or a log line, since it can quote the value; only the rejection's kind and the line and column.
- **Logging** *(the user's choices)*: `info` for a completed login, a logout, a step-up, a first enrollment and a replaced 2FA; `debug` for refusals and a re-hash skipped by a full queue; `warn` for a re-hash whose hashing failed; `error` for a stored secret the vault won't open, naming its `key_id`, answered 500.
- **OpenAPI and the registry** *(the user's choices)*: two bearer schemes, `access_token` and `setup_token`, attached to each operation by `Routes::register` from its Access kind, never by hand. Routes that need a fresh stamp register as `Access::AuthedStepUp` and carry `x-step-up: true`; the registry test calls each with a live session but no fresh stamp and expects 403 `step_up_required`, so E's `/me/password` can't forget the check. A live mfa token or setup token as Bearer is 401 on every route that doesn't take it.

### As built (group D)
- **P7.1's `user_mfa`** (`0005_user_mfa.sql`): the conventions of 0001–0004. `secret_envelope` is pinned to a version 1 envelope of a 20-byte secret (65 bytes); another envelope version comes with its own migration. `last_used_step >= 0`. Both tables reference `users` without cascade; `recovery_codes(user_id)` is indexed.
  - **The repository** (`ports::mfa`, `infra::sqlite::mfa`): `find` on the read pool and in the transaction, `find_unused_code`, `insert` (a primary-key violation is `InsertMfaError::Exists`), `replace`, the conditional `advance_step` (`… WHERE user_id = ? AND last_used_step < ? AND confirmed_at = ?`), `replace_codes` and the conditional `use_code`. A step beyond SQLite's integers is `Unstorable`.
  - **`RecoveryCodeHash`** (`ports::recovery_codes`) has `ct_eq` and no `PartialEq`, and its `Debug` shows no byte, like `TokenHash`.
  - **For the flows:** `SessionReads::refresh_hash_exists`; `SessionWrites::{revoke_all_for_user, find, stamp_step_up}`; `UserWrites::{get, set_password_hash}`, the latter never touching `password_changed_at`.
  - **SQLite's code for a duplicate primary key** is 1555 (`SQLITE_CONSTRAINT_PRIMARYKEY`), not UNIQUE's 2067, with the same message form; `unique_violation` recognizes both, and every existing caller still maps only its named columns.
  - **Tests,** red against compiling stubs (a repository that found and wrote nothing, a hash type that compared and read back nothing): 27 of 39 new tests failed on assertions, and the migrations' snapshot gained line 5, read before accepting. The ones that passed check the tables' CHECKs and foreign keys, which the real migration already enforces, `Debug`, nothing found, and the lookup by session ID that reuses B's query. Then all passed.

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
- **Group C, easier:**
  - No route can exist without its access and its rate-limit class, and an authed route that forgets `AuthUser` fails a test.
  - Every limit is tested deterministically on the clock everything else uses.
  - A flood of refused requests adds one log line a minute.
  - A database or backup leak reveals no typed username.
- **Group C, harder:**
  - The server owns its rate-limit middleware instead of a library's.
  - An attacker can keep one account locked (P7.12's note).
  - Behind a reverse proxy, a missing `trusted_proxies` puts every client into the proxy's one bucket, which P12.7's smoke test must catch.

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
- **Group C:**
  - **tower_governor anyway:** quanta time in tests, governor's and axum's defaults workspace-wide, a wait rounded down.
  - **governor's `FakeRelativeClock` in tests:** a second clock beside `ManualClock`, and a test seam in production code.
  - **The wall clock for the limiters:** a step back would refuse every client.
  - **Health exempt from the limits, or in its own bucket:** an unlimited database ping, or one more class.
  - **IPv6 by the full address:** trivially evaded. **A configurable prefix:** a key Appendix A doesn't have.
  - **A 400 for an unreadable `X-Forwarded-For`,** which breaks every request behind a misconfigured proxy, **or skipping the bad entry,** which reads further into what the client wrote.
  - **The `ipnet` crate in production,** or a lenient parser that silently masks host bits.
  - **The lockout key as its own secret file:** one more secret to manage. **Random per process:** every restart resets the counts. **Plain SHA-256:** a leaked database lets anyone test guesses offline.
  - **A streak forgotten 1 h after its lock:** about 5 fresh guesses an hour. **Never forgotten:** one mistake a month locks a user eventually.
  - **No in-flight guard:** a few more guesses per lock, bounded only by the argon2 queue. **Counting refusals during a lock:** nobody, the real user included, gets in while an attacker keeps trying.
  - **The lockout entry only on the first lock of a streak.** **Keys for the first lock, the cap and the expiry:** knobs §7.4 fixes.
  - **Leaving the purge to P11.6:** unbounded growth until then. **One purge batch per run:** can fall behind a spraying attacker. **Deleting stale rows on read:** sprayed names never come back.
  - **One bucket per login route,** or only the classes C's own routes use.
