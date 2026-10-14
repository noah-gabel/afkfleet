# 0018. Microsoft accounts and the vault: conventions, Phase 9 refinements

- **Status:** Accepted
- **Date:** 2026-10-14
- **Related:** Plan.md Phase 9 (P9.1–P9.8), P10.6, §4, §5, §7.2, §7.4, §7.5, Appendix D; ADR-0002, ADR-0003, ADR-0010, ADR-0011, ADR-0012, ADR-0015, ADR-0016, ADR-0017

## Context
Phase 9 links Microsoft accounts. A user starts Microsoft's device-code flow, the server stores the account's refresh token, sealed by the vault, and only short-lived Minecraft session tokens ever leave the server (security rule 6). The phase also extends the vault: a second purpose, and `vault rotate-key`, which §7.5 promised since Phase 7.

Before any code, the user split the phase into groups and answered group A's open questions on 2026-10-14, one at a time, and added two requirements at the plan's approval. This ADR records the answers. Decisions already made elsewhere aren't repeated: ADR-0010 (the account rules of `authorize()`), ADR-0011 (azalea-auth's `trace` logging and the cap), ADR-0016 (the vault's envelope and keys, step-up) and ADR-0017 (step-up in the app). Each later group asks its own implementation questions and adds its decisions here.

## Decision
### Grouping *(the user's choice: six groups, P9.3 alone)*
One branch and one PR per group, one commit per task, in this order, each after the previous PR is merged:

| Group | Branch | Tasks |
|---|---|---|
| A | `p9/p9.1-p9.2-accounts-table-and-vault` | P9.1's `mc_accounts`; P9.2 |
| B | `p9/p9.3-microsoft-auth-provider` | P9.3, alone, so the Microsoft token handling gets a focused review (like P3.3) |
| C | `p9/p9.5-session-token-service` | P9.5 |
| D | `p9/p9.4-p9.7-link-flow-and-offline` | P9.4, P9.7 |
| E | `p9/p9.1-p9.6-grants-and-account-endpoints` | P9.1's `account_grants`; P9.6 |
| F | `p9/p9.8-app-screens-wrap-up` | P9.8; the DoD, the threat model and the README |

- **P9.1 is spread over A and E,** as P7.1 was over Phase 7's groups (ADR-0016): each table arrives with its first user. `mc_accounts` comes in A, since `vault rotate-key` re-encrypts it and P9.2's swap test needs accounts. `account_grants` comes in E, with the grants' endpoints. P9.1 is ticked in E.
- **New dependencies** arrive with their group, each with the user's approval of its version, license and release date: azalea-auth in B, moka in C, tauri-plugin-opener in F. Group A adds none.
- **Hands-on steps:** each group's PR names exactly what the user does with their own Microsoft account and what they should see. Group A has no Microsoft step, since nothing can be linked before D.

### Device-code phishing in the friend group *(the user's decisions)*
**The threat.** A user starts a link flow and gets someone else to enter the code at Microsoft's page. That person's Minecraft account then lands under the attacker's afkfleet account, with Manage. Microsoft's page names the app behind azalea's client ID, never afkfleet or who started the flow. The counters, each built by the group named:
- **New links wait for approval** (D sets the status, E builds the endpoints and decides their step-up, F the screen).
  - A new account is `pending` until someone above the linker approves it: a Member's link needs an Admin or the Owner, an Admin's needs the Owner. The Owner's own links need none.
  - The approver sees who linked which Minecraft name.
  - No bot can use a pending account.
  - For Admins and the Owner, Home shows how many links wait for them, so none is missed.
- **One link per Minecraft account** (A's table, D's flow). A Minecraft account (by its UUID) that's already linked, pending ones included, can't be linked by anyone else. The refusal names nobody. A friend who linked their own account first is safe. A victim takes theirs back by revoking the app's access in their Microsoft account and having an Admin delete the row.
- **Audit and visibility** (D, E). `account.link` records the creator, the Minecraft name and the UUID. Admins and the Owner see every account, with who linked it and when.
- **A warning in the link dialog only** (F), the user's exact text: "Only enter a code you started yourself here. Never enter a code someone sent you, it would hand them your Minecraft account." Home carries no warning.
- **The threat model** (F) records the threat, these counters and the residual risk.

### Group A: the accounts table (P9.1's `mc_accounts`) *(the user's choices)*
- **One table for Microsoft and offline accounts** (`0008_mc_accounts.sql`).
  - An offline account (P9.7, dev only) has no UUID and no envelope; CHECKs tie both to `kind`. The game derives an offline UUID from the exact name, so storing it would split `AfkBot1` from `afkbot1`.
  - A Microsoft account is unique by its profile's UUID, pending ones included (the one-link rule). An offline account is unique by its lowercased name, since Minecraft names compare without case (P9.1's note).
  - An offline account is always `active` (a CHECK): it needs no approval and has no token Microsoft could refuse.
- **No `key_id` column,** as in `user_mfa` (ADR-0016): the envelope's header names its key. This corrects Plan.md's P9.1 and Appendix D.
- **No CHECK on `status`'s values** (`pending`, `active`, `reauth_required`). The set may grow, and SQLite can't change a CHECK without rebuilding the table (ADR-0015's rule). fleet-core's `AccountStatus` reads it, and an unknown value is `StoreError::Corrupt`.
- **No CHECK on `mc_name`'s shape,** like `users.username`: `McUsername` checks it when a row is read back.
- **The envelope's CHECK** is only `length > 45`, a non-empty token. Its upper bound is the adapter's (B).
- **Core types in fleet-core:**
  - `fleet_core::account::{AccountKind, AccountStatus}`, written and read through `as_str` and `FromStr` as ADR-0010 does for SQL columns.
  - `fleet_core::value::McUuid`, a newtype over `Uuid` so a record's ID and a profile's UUID can't be swapped. It refuses only the nil UUID, takes any version (Microsoft profiles are version 4), and is written and parsed only in lowercase hyphenated form. It has no serde until a DTO needs it.
  - `SessionCredentials` keeps its plain `Uuid`, so fleet-mc and fleet-runtime don't change; later groups convert at the edge.
- **The Rust row type** puts the status inside the Microsoft variant: `AccountCredential::Microsoft { uuid, refresh_envelope, status } | Offline`, with `McAccount::status()` answering `Active` for an offline account. An offline account that waits for approval can't be built.
- **Repository scope: only what A uses** (as Phase 7's groups did): `insert`, with `InsertAccountError::Taken` for a linked UUID or offline name, and `find`, both on the read pool and inside a transaction. Later groups add theirs.

### Group A: the vault (P9.2) *(the user's choices)*
- **The second purpose:** `VaultPurpose::MsRefreshToken`, named `ms_refresh`, with the account's ID as the AAD's record (Plan.md's `account_id|ms_refresh`). A second golden envelope pins it.
- **"Lookup by key_id" is P7.4's `open`,** which already finds the key the envelope's header names, plus:
  - a header reader on the port, `key_id_of`, refusing exactly what `open` calls malformed
  - `vault status`

  The envelope's layout constants move from the adapter to the port.
- **`vault status`** reads only the envelopes' headers, never a key file. It reports:
  - every key ID, configured or named by an envelope, with its counts per purpose
  - which one is the active key
  - a configured key that seals nothing (the "can leave" hint only for a key that isn't active)
  - a key that seals secrets but has no file
  - malformed headers, on a line of their own

  Exit 0 when every envelope's key is configured and none is malformed, 3 when not, 1 on a config or database failure, 2 on a usage error.
- **Neither vault command creates or migrates the database.** A missing file or pending migrations stop the command with exit 1 ("run `afkfleet-server migrate` first"). `rotate-key` checks before it reads any key file, so a rotation never changes the schema as a side effect.
- **`vault rotate-key`:**
  - It re-encrypts only the envelopes under other keys than `[vault] active_key_id`. Envelopes already under the active key are left as they are, so a second run does nothing, and a forgotten `active_key_id` change reads as "nothing to re-encrypt".
  - **Its crypto runs before the transaction** (ADR-0015's rule, no exception). It reads every envelope, then opens and re-seals each under the active key, each plaintext dropped and wiped at once. Then one write transaction swaps each row, but only while the row still holds the envelope it read. It also checks that no envelope under another key is left, and commits with the audit entry. A row changed meanwhile (a running server wrote) rolls everything back, and the run says to run it again.
  - **An envelope that doesn't open** stops the whole rotation. Every bad envelope is tried and listed (table, row ID, reason; never a byte), with one `vault.rotate_key` Failure entry each, all in one commit, and exit 1. One that doesn't open may mean someone changed the database.
  - **Nothing to re-encrypt, a failed random source, or a change meanwhile** write nothing and record nothing. The last two log one `error` line and exit 1.
  - **There's no confirmation prompt:** the rotation is one transaction, and the old key file undoes it as long as it's kept.
- **The audit entries,** `vault.rotate_key` (the naming rule of ADR-0016):
  - **Success:** no actor, IP or target. Metadata `sealed_with` (the active key's ID), `totp` and `ms_refresh` (how many of each were re-encrypted).
  - **Failure:** target the user (a TOTP secret) or the account (a refresh token). Metadata `reason` (`unknown_key`, `rejected` or `malformed`) and, when the header is readable, `sealed_with` (the key it names).

  The names were first `key_id`, `totp_secrets` and `refresh_tokens`. The audit log's existing rule refuses a metadata key with a secret word (`key`, `token`, `secret`, …; `SECRET_WORDS`), and the rule stays. *(The user's choice of the new names during the build.)*
- **CLI only:** key rotation gets no API route. A new key is a file the operator adds to the config, which no request can do, and the running server loads keys only at startup. So there's no step-up to decide for it; §7.2's "rotating keys" is the CLI's (Phase 9's step-up note, for group A).
- **The README's procedure** *(the user's choice and additions)*:
  1. Stop the server.
  2. Copy the database.
  3. Add the new key file and make it `active_key_id`.
  4. Run `vault rotate-key`.
  5. Check `vault status`.
  6. Remove the old key from the config and start the server.

  - **Keep the old key file offline** as long as any backup from before the rotation exists, since those backups are sealed under it.
  - **To restore such a backup:** put the old key back in `master_key_files`, run `vault rotate-key` with the server stopped, check `vault status` (it exits 3 until the key is back), then remove the old key and start.
  - **A new active key resets every lockout streak,** since the lockout key derives from it (ADR-0016 calls that harmless).
- **Coverage** *(the user's choice)*: the vault gate (90 %) gains the service (`app/vault.rs`) and the database layer's vault code, kept in files of its own for that: `ports/envelopes.rs` and `infra/sqlite/envelopes.rs`. `cli/vault.rs` stays under the workspace gate, like `cli/user.rs`. This adds to a gate, and changes no threshold.
- **Dev recipes** *(the user's choice: fixed to key 2)*:
  - `just dev-vault-status`: the dev database's status with key 1.
  - `just dev-rotate-key`: copies the dev database to `target/dev-server/rotation/afkfleet.db`, creates `vault-key-2` when it's missing, and rotates the copy with keys 1 and 2.
  - `just dev-server-rotated`: serves the copy with key 2 only.

  The copy is refused, before anything changes, when the folder exists (it never deletes or overwrites anything), when a `-wal` file sits next to the dev database (a running `dev-server`), or when the dev database is missing (`scripts/dev-rotation.mjs`). The paths can be overridden, like `dev-server`'s.
- **The `-wal` check, proven** *(the user's addition at approval)*. Reading a `-wal` file as "dev-server runs" holds only if nothing else leaves one behind. Tests show that neither a status run's reads nor a server's clean stop does:
  - `a_closed_open_migrated_database_leaves_no_wal_or_shm`
  - `a_server_stopped_by_a_signal_leaves_no_wal_or_shm`
  - the CLI tests' `leftovers` checks
  - the existing `closing_checkpoints_and_removes_the_wal`, for a write and a close

  A real run of `dev-vault-status`, then `dev-rotate-key`, in a scratch folder copied without a refusal. Had any left a file behind, the plan was to stop and design a check that detects an open database instead.

### As built (group A)
- **fleet-core:** `account::{AccountKind, AccountStatus, AccountError}`; `value::{McUuid, McUuidError}`; the audit action `vault.rotate_key`.
- **fleet-server:**
  - `ports::accounts` (`McAccount`, `AccountCredential`, `InsertAccountError`, `AccountReads`, `AccountWrites`) and `infra::sqlite::accounts`.
  - `ports::envelopes` (`SealedRecord`, `SealedEnvelope`, `EnvelopeReads`, `EnvelopeWrites`) and `infra::sqlite::envelopes`.
  - `ports::vault::{key_id_of, header_of, VERSION_1, HEADER_BYTES, OVERHEAD}` and `Vault::active_key_id`.
  - `infra::sqlite::open_migrated` with `OpenError::{MissingFile, Pending}`.
  - `app::vault::VaultService`; `cli::vault` (`status`, `rotate_key`, `report`) and `cli::VaultCommand`.
  - `Store::{accounts, envelopes}` and `WriteTx::{accounts, envelopes}`. The auth tests' `HookedStore`, the only other `Store`, forwards them.
- **The offline-name conflict** is reported by SQLite as `index 'mc_accounts_offline_name'`, since an index on an expression has no column name. The repository maps that text, kept as a constant.
- **The migration check reads `_sqlx_migrations` with plain SELECTs on the read pool.** sqlx's own `run` creates the table first, and its validation is private. A pure `schema_state` checks in sqlx's order: a failed migration (`Dirty`), one this server doesn't know, an edited one, then the first pending one. The write pool is lazy, so a status run never opens a write connection.
- **Tests,** each red first against compiling stubs that returned wrong values:
  - **fleet-core:** 10 of 31 for the types (the 21 that passed check refusals a refuse-everything stub gives too); the names snapshot, with the new action.
  - **The accounts repository:** 10 of 12 (the two that passed check "nothing found" and valid raw rows the bare table takes). The port's `Debug`. `status()` was shown red through a temporary mutation (Microsoft accounts always `Active`).
  - **The vault:** all 9 port tests, the 6 new vault tests (the golden refresh envelope against an empty constant).
  - **The envelope repository:** 3 of 5 and all 3 port tests.
  - **`open_migrated`:** all 7 integration tests and 4 of its 6 unit tests.
  - **The service:** 9 of 10 (nothing to do is what the stub answers).
  - **The commands:** 11 of 12.
  - **`scripts/dev-rotation.mjs`:** 10 of 12 (one passes against the do-nothing stub, one is Unix-only).
  - **Shown red through temporary mutations, each reverted:**
    - `close()` skipping the read pool: both `-wal` tests and the server's clean-stop test
    - `status` loading the keys: "status reads no key file"
    - renamed subcommands without their exit-code help: the parser tests (5 of 8) and the binary tests
    - swapped vault arms in `main.rs`: the binary run on a fresh database
    - another environment prefix: `the_active_key_can_come_from_the_environment`, which the dev recipes rely on
  - **New snapshots,** read before accepting: the migrations' line 8, the audit action names (`vault.rotate_key` added) and the report with every kind of key line.
- **Found during the build:**
  - The metadata names above.
  - "nothing to re-encrypt: every secret is under key N" read oddly for an empty vault. It's now "no secret is sealed under another key than key N (active)".

### Group B: the Microsoft port and its adapter (P9.3) *(the user's choices, 2026-10-14)*
The user answered group B's questions one at a time, after reading azalea-auth 0.16.0's source and asking for each fact to be checked in docs.rs's source view.

- **Why not azalea-auth.** In 0.16.0:
  - **No status checks.** Every call parses the body straight into its success type, so a refused refresh (`invalid_grant`), a Microsoft 429, an XSTS refusal and a missing profile all come back as the same decode error.
  - **No single poll.** `get_ms_auth_token` loops until the code expires, drops every refusal (a decline included), gives up on the first network error, and times itself with `std::time::Instant`.
  - **Panics.** `get_minecraft_token`, the only public call from Xbox Live to Minecraft, indexes `display_claims["xui"]`, unwraps, `expect`s an unexpired Xbox token, and adds `expires_in` to `SystemTime` with a `+` that overflows.
  - **Secrets in `Debug`.** `DeviceCodeResponse`, `AccessTokenResponse`, `MinecraftTokenResponse` and `MinecraftAuthResponse` all print theirs.
- **Own requests for all** *(the user's choice over a hybrid and over azalea-auth alone)*.
  - `infra::msauth::ReqwestMicrosoftAuth` sends every request itself, with azalea-auth 0.16.0's protocol: the client ID `00000000441cc96b` (Nintendo Switch's, which works for accounts under 18; private in azalea-auth, so copied), the scope `service::user.auth.xboxlive.com::MBI_SSL`, the same form fields, JSON bodies and headers (`x-xbl-contract-version: 1`, the poll's `?client_id=`), and no User-Agent.
  - fleet-server doesn't depend on azalea-auth, so `rsa` doesn't reach it (ADR-0012's analysis of RUSTSEC-2023-0071 is no longer needed there).
  - Each azalea bump compares the adapter's endpoints, fields and headers with the new azalea-auth's `auth.rs` (ADR-0003).
- **The guard** *(the user's choice, with the full wrapper list)*. `deny.toml` bans `azalea-auth` with `wrappers = ["azalea", "azalea-client", "azalea-inventory", "azalea-protocol"]`, the four crates that depend on it in azalea 0.16.0 (checked in Cargo.lock), so a direct dependency in any member fails `just deny`. With only `azalea`, `main` itself fails. A new azalea crate that depends on azalea-auth is named by cargo-deny at the bump, and the list is updated with a note here (ADR-0003, ADR-0012). fleet-mc's clippy bans on `azalea::auth` stay.
- **`stable_crates` unchanged** *(the user's choice)*. fleet-server, fleet-client and the desktop crate stay. The earlier notes said they leave, but that rested on azalea-auth reaching fleet-server.
- **Dependencies** *(the user's approvals)*:
  - reqwest 0.13.5 (2026-09-08, MIT OR Apache-2.0) as a direct dependency, with the workspace's `rustls` plus `form` only. JSON request bodies are written with serde_json and an explicit `Content-Type`.
  - rustls (0.23.46 since the separate bump below) as a direct dependency, for the provider install and the certificate check.
  - rcgen 0.14.10 as a dev-dependency (`aws_lc_rs`, `pem`), for the certificate test.
  - Only fleet-server's own dependency list changed in Cargo.lock.
- **Clippy** *(the user's decisions)*:
  - **The readers:** `reqwest::Response::{json, text, bytes, text_with_charset}` are method bans, "read Microsoft's answers through the adapter's capped reader"; `json` and `text_with_charset` with `allow-invalid`. Cargo unifies features across the workspace (azalea-auth turns `json` on), so leaving the feature out of fleet-server alone doesn't keep the unbounded readers away.
  - **`reqwest::get`** is a method ban, since it builds a client without naming its type.
  - **`reqwest::Client` and `reqwest::ClientBuilder`** are type bans, "build and hold reqwest clients only in infra/msauth/client.rs, through the adapter's one builder". The first plan had method bans on `Client::new` and `Client::default`, but clippy can't name `Client::default`: it's the `Default` impl's method ("does not refer to a reachable function"). A method ban on `Client::new` also misses that impl's own call to it inside reqwest.
  - **The one exception:** `client.rs` carries the one approved `#![expect(clippy::disallowed_types, reason = …)]`, and the adapter holds its client as a newtype, `HttpClient`, so the type is named nowhere else.
  - **The probe** (temporary, with reqwest's `json` and `charset` switched on for fleet-server alone, as ADR-0015's probe did) flagged all 10 uses: a struct field of type `reqwest::Client`, `Client::default()`, `Client::new()`, `Client::builder()`, `ClientBuilder::new()`, `reqwest::get` and the four readers.
- **The TLS provider** *(the user's choice)*. `serve` installs aws-lc-rs as the process's rustls provider in main.rs's `start()`, before the runtime. A provider installed first is `StartError::CryptoProvider` ("a TLS crypto provider is already installed"), as in fleet-agent. **Never in the library's `serve::run` or `serve::serve`:** in-process tests (fleet-client's, with several servers per process) call those, and a second install fails. The other commands make no TLS connection.
- **The port's shape** *(the user's choice: per-method enums)*:
  - `start_device_flow() -> Result<DeviceFlow, Failure>`.
  - `poll(&DeviceCode) -> Result<PollOutcome, Failure>`: one try. `PollOutcome` is `Pending`, `SlowDown`, `Declined`, `Expired` or `Authorized(MicrosoftTokens)`: a declined or expired flow ended normally. After `SlowDown`, the caller polls at `PollInterval::slowed_down()`, 5 s longer, at most 60 s (RFC 8628).
  - `refresh(&RefreshToken) -> Result<MicrosoftTokens, RefreshError>` with `RefreshError = InvalidGrant | Failed(Failure)`.
  - `minecraft_session(&MsAccessToken) -> Result<MinecraftSession, SessionError>` with `SessionError = Xbox(XboxRefusal) | NotOwned | NoProfile | Failed(Failure)`.
  - `Failure = Unreachable | Certificate | TimedOut | RateLimited | Unavailable { status } | Unexpected { endpoint, rule }`. `RateLimited` carries no Retry-After: the callers back off on their own.
  - The secrets are newtypes over `SecretString` with an opaque `Debug`: `DeviceCode`, `RefreshToken`, `MsAccessToken` and `McToken`. Each has `TryFrom<SecretString>`, which checks the token rules, so P9.5 builds a `RefreshToken` from the vault's plaintext. `UserCode` has an opaque `Debug` too. `MinecraftSession`'s `Debug` shows only its expiry.
  - `VerificationUri` is parsed with reqwest's `Url` inside the port, so the adapter and the tests' fake share one rule; no reqwest type is in the port's API.
- **The answers** *(the user's choices)*:
  - **OAuth codes on the poll (400 or 401):**
    - `authorization_pending` is Pending; `slow_down` is SlowDown.
    - `authorization_declined` and `access_denied` are Declined.
    - `expired_token`, `bad_verification_code` and `invalid_grant` are Expired.
  - **On a refresh,** only `invalid_grant` is `InvalidGrant`, so `invalid_client` or `invalid_scope`, a problem of our own setup, never marks an account `reauth_required`.
  - **An unknown code** is Unexpected and named in the rule ("refresh: OAuth code invalid_request") when it matches `[a-z_]{1,64}`: protocol words, not secrets. Anything else, a missing or non-string `error`, or a body that isn't JSON, is "unreadable OAuth error" *(the user's change at the plan's review)*.
  - **Statuses:** 429 is RateLimited, 5xx is Unavailable, and any other unexpected status (a redirect included) is `Rule::Status`.
  - **A 200 without `refresh_token`:** on a refresh, the token that was sent is kept (RFC 6749 §6: the server MAY issue a new one), so `MicrosoftTokens.refresh` is always the token P9.5 stores. On the poll it's Unexpected, since the account couldn't be kept *(the user's change at the plan's review)*.
  - **A missing field** is Unexpected, except the device answer's `interval`, which defaults to 5 s as RFC 8628 says *(the user's choice)*.
  - **Xbox refusals:** only an XSTS 401 with a readable `XErr` is `SessionError::Xbox`, with `Banned` (2148916227), `NoXboxProfile` (…233), `CountryUnavailable` (…235), `AdultVerification` (…236, …237), `ChildAccount` (…238) and `Other { xerr }`. An XSTS 401 without a readable `XErr` (missing, not a number, or over u32), and any refusal from Xbox Live's user authentication, is Unexpected: the access token just came from Microsoft.
  - **Ownership:** azalea's rule, so an empty `items` list is NotOwned and any entitlement owns the game (Game Pass included). Then the profile: a 404 whose `error` or `errorType` is `NOT_FOUND` is NoProfile. The entitlements' signatures aren't verified (TLS to Mojang, as azalea does).
- **Microsoft's answer is input** *(the user's choices)*:
  - **Number bounds,** refused outside, so nothing waits on a bad value:

    | Value | Range |
    |---|---|
    | the device code's `interval` | 1–60 s |
    | the device code's `expires_in` | 60–3600 s |
    | a Microsoft access token | 60 s–1 day |
    | a Minecraft token | 60 s–2 days |

  - **Sizes and shapes:**
    - every answer body is at most 256 KiB: a `Content-Length` over it is refused before reading, and a body that grows past it is cut off with its connection
    - every token (the device code, refresh, access, Xbox Live, XSTS and Minecraft tokens) is 1 B–16 KiB of visible ASCII (0x21–0x7E), so it's always a valid header value. **That's the stored refresh token's upper bound**, group A's open point.
    - the user code is 4–16 of `[A-Z0-9-]`
    - the Xbox user hash is 1–64 ASCII letters and digits
    - the profile's UUID is exactly 32 hex digits, through `McUuid`, and its name goes through `McUsername`
  - **The verification link** must be https, with the host exactly `www.microsoft.com` or `microsoft.com`, no user, the default port, and no query or fragment. If the user's run shows another real value, the list is widened only with their approval.
  - **Refusals** name the endpoint and the rule ("profile: body over 256 KiB"), never the value.
  - **Expiries** come from fleet-core's `Clock`, read just before the request whose `expires_in` they use (for `minecraft_session`, before `login_with_xbox`), so a computed expiry is never later than the real one.
- **The client** *(the user's choices)*: connect 5 s, each request 15 s, `Policy::none()`, `no_proxy()`, `https_only(true)`; an outer bound of 20 s for a device-code start, a poll or a refresh, 60 s for `minecraft_session`; constants. **No retries:** callers own retries and backoff, and a replayed refresh could spend a rotated token twice.
- **`Failure::Certificate`** *(the user's choice)*: rustls's `InvalidCertificate` anywhere in the error's chain, with fleet-client's chain walk. reqwest's platform verifier reads the system CA store on Linux, so a server image without CA certificates fails every call this way (a P12.6 note).
- **Logs** *(the user's choice)*: one `debug` line, "a Microsoft call failed", per failed call, with the endpoint, the status when there is one, and the error's text, which names only the rule. The outer bound's line names the call instead. Never a body, a header or reqwest's error text, and nothing on success.
- **Tests** *(the user's choices)*:
  - **No Microsoft in CI.** A `#[cfg(test)]` constructor takes base URLs and shorter bounds and allows plain http. An axum fake on `127.0.0.1:0` scripts each endpoint and records every request. Raw TCP servers send a false `Content-Length`, an endless body, a silent socket, or text instead of TLS, and an rcgen-certified rustls server presents an untrusted certificate. They run on real time with short bounds, never a sleep; every test that relies on a bound also bounds itself.
  - **The production constructor's own protections,** which the test constructor's plain http bypasses, have tests through the one shared builder. The production client refuses an `http://` URL of the fake, and the fake sees no request. A 302 comes back as Unexpected, and its target sees no request.
  - **Coverage:** a new `msauth` gate (90 %, required) over `ports/msauth.rs` and `infra/msauth/`.
  - **The fake** lives in a multi-file test binary, `tests/accounts/` (`support/msauth.rs`), so it never reaches a release build and isn't compiled into several binaries, where `dead_code` would fire. It has per-method scripts answered in order, a panic naming the method on an unscripted call, a call log with the tokens it got (invented only), and a scripted `Hang`. P9.4 and P9.5 add their tests to that binary.
- **Redaction** *(the user's requirements)*:
  - **The cap,** `tests/telemetry.rs`: a server config loaded with `fleet_server::config::load` in a figment jail, through `layer_with(&config.log, &NoRules, …)`, the layer `init_with` installs.
    - Four filters in the file (`trace`, `trace,azalea_auth=trace`, `azalea_auth=trace`, `trace,azalea_auth::auth=trace`), and the environment's `AFKFLEET_SERVER__LOG__FILTER` over a file that says `info`.
    - Every case also sets `RUST_LOG=trace,azalea_auth=trace`, in case a later switch to `from_default_env` opens a second, uncapped way in.
    - Nothing from `azalea_auth` or `azalea_auth::auth` below `info` gets through. Its `info` line must, every time, and a `fleet_server` trace line wherever the filter enables everything at `trace`. The plan's text asked for the `fleet_server` line in every case, which `azalea_auth=trace` alone can't give.
  - **The adapter's own logs:** `fleet_testkit::log_capture` at every level from every target, through every success and failure path, with invented values for every token, the device code, the user code, the user hash, the name and the UUID in both forms. The capture's working is checked through the failure lines' count.
- **The hands-on check** *(the user's choices)*. `just test-ms-account [filter]` runs `cargo nextest run -p fleet-server --profile manual --no-capture`. **The user only; the AI never runs it.** It has two tests:
  - **`manual_microsoft_account_check`** prints the link, the code and the deadline, then pass or FAIL for each step: pending seen, authorized, the Minecraft session, a refresh with a second session of the same profile ("refresh token rotated: yes/no", for information), and an invented refresh token answered with `invalid_grant`.
  - **`manual_microsoft_declined_link`** expects Declined.

  Both end with the redaction check over every value seen, reported as targets and counts. Everything stays in memory. A failed step prints its error's text, which names a rule but never a value. **They're the only tests with a real sleep:** Microsoft's interval, waited at most 8 minutes or until the code expires (inside the `manual` profile's 10-minute limit).
- **The rustls advisories** *(found during the build, the user's decision)*. RUSTSEC-2026-0346 to 0349 against rustls 0.23.45 appeared mid-build and failed `just deny` on `main`. A separate PR (#41) raised the workspace's floor to 0.23.46, and group B continued on top of it.
- **Known limits.**
  - The form and JSON bodies reqwest sends hold copies of the device code and the tokens, which can't be wiped, and neither can hyper's receive buffers (as ADR-0011's `access_token()`).
  - The reader reserves each body's announced size, or 256 KiB, up front, so its own buffer never moves and is wiped on drop.

### As built (group B)
- **fleet-server:**
  - `ports::msauth`: `MicrosoftAuthProvider`; `DeviceCode`, `RefreshToken`, `MsAccessToken`, `McToken` and `TokenError`; `UserCode`, `VerificationUri`, `PollInterval` and `OAuthCode` with their errors; `DeviceFlow`, `PollOutcome`, `MicrosoftTokens` and `MinecraftSession`; `Failure`, `Endpoint`, `Rule`, `RefreshError`, `SessionError` and `XboxRefusal`; `MAX_TOKEN_BYTES` and `MAX_ANSWER_BYTES`.
  - `infra::msauth`: `ReqwestMicrosoftAuth` and `BuildError`, with the private modules `client` (the builder, `HttpClient`, the reader, the classification), `wire` (serde shapes without `Debug`) and `answers` (the pure mapping).
  - `cli::serve::StartError::CryptoProvider`, and main.rs's provider install.
- **Tests,** each red first against compiling stubs that returned wrong values:
  - **The port:** 87 of 105. The 18 that passed are acceptance cases an accept-everything stub passes, the two `Other` XErr cases, and the flow's `Debug` test, shown red with the token's `Debug` printing its text.
  - **The adapter:** 167 of 176 (the mapping tables, the client's tests and the fake-server tests). Of the 9 that passed, 5 are cases a stub that answers `Unreachable` passes: the chain without rustls, another TLS error, no connection, garbage instead of TLS, a closed port. The other 4 were shown red through temporary mutations: another connect bound, the adapter's `Debug` with its endpoints, the production client without `https_only`, and the outer bound removed.
  - **Shown red by mutating the protection itself:**
    - the redirect test: a policy that follows 10 redirects
    - the redaction test: a `debug` line with the answer's body
    - the cap tests: the cap lifted to `TRACE` in fleet-startup (all 5)
  - **`StartError::CryptoProvider`'s message:** 1 of 1.
  - **The coverage gate's two new script tests:** 2 of 2.
  - **The fake's 8 tests,** written after it and shown red by mutations: answering from the back (2), no log entry for a poll (1), another panic message (4), a hang that answers after a second (2).
  - Every mutation was reverted and checked by checksum.
- **`cargo tree -p fleet-server -e normal,build,dev --target all -i rsa`** (and `-i azalea-auth`) find nothing.
- **The deny proof:**
  1. `just deny` passes.
  2. A temporary `azalea-auth = "=0.16.0"` in fleet-server fails it: `error[banned]: crate 'azalea-auth = 0.16.0+mc26.1' is explicitly banned`, "advisories ok, bans FAILED, licenses ok, sources ok".
  3. With the probe reverted, it passes again.

  The desktop pass reports the wrapper as unused, like its other not-encountered entries.
- **Found during the build:**
  - `Client::default` can't be named in clippy's path syntax (the type bans above).
  - On Windows a refused connection takes about 2 s, so the fake-server tests use the production 5 s connect bound.
  - A mutation of the fake's queue order made one of its tests hang, so every call in the fake's tests is bounded now.
  - The rustls advisories (the separate PR above).

### Group C: the session-token service (P9.5) *(the user's choices, 2026-10-14)*
The user answered group C's questions one at a time and added three requirements: one with the backoff answer, one with the wait bound's, and one at the plan's approval.

- **No moka** *(the user's choice)*.
  - Its facts: moka 0.12.16 (released 2026-08-09, license "(MIT OR Apache-2.0) AND Apache-2.0").
  - Its source (`future/value_initializer.rs`) shows two problems with `try_get_with`:
    - When the caller evaluating the init is dropped, a waiting caller runs its own init (`WaiterValue::EnclosingFutureAborted`). Here that would be a second refresh with a token Microsoft may already have rotated.
    - It panics after repeated aborts (`panic_if_retry_exhausted_for_aborting`).
  - A per-account lock was needed either way. So each account's slot has a `tokio::sync::Mutex`, and the request that needs Microsoft hands its owned guard to a task, the flight. The flight holds it until the rotated token is stored and the session cached (fleet-client's pattern, ADR-0017).
  - §5's moka row keeps only P11's WebSocket tickets, which ask again.
- **The flight's owner** *(the user's choice)*: a `TaskTracker`, with a child `CancellationToken` checked only right before a refresh is sent.
  - It takes tokio-util's `rt` feature on fleet-server. That pulls in only futures-util, already in its tree, so `Cargo.lock` gains one line in tokio-util's list.
  - Callers await the flight's `JoinHandle`. `shutdown()` cancels, closes the tracker and waits.
  - No new exception to CLAUDE.md's task rule.
- **Refusals after linking** *(the user's choices)*:
  - NotOwned, NoProfile and every Xbox refusal set a new status, `refused`. Its reason goes into a new nullable column, `status_reason` (`0009_mc_accounts_status_reason.sql`, `ALTER TABLE ADD COLUMN`).
  - The reasons: `not_owned`, `no_profile`, `xbox_banned`, `xbox_no_xbox_profile`, `xbox_country_unavailable`, `xbox_adult_verification`, `xbox_child_account`, `xbox_other` and `profile_mismatch`. An `xbox_other`'s XErr is kept in the audit metadata only.
  - Only `refused` has a reason. There's no CHECK, as for `status` (group A).
- **The type** *(the user's choice)*:
  - fleet-core's `AccountStatus` gains `Refused(RefusalReason)` and stays `Copy`. `as_str()` stays the `status` column's name; `reason()` gives the reason.
  - `AccountStatus::from_columns(status, reason)` replaces `FromStr`, so group A's "`as_str` and `FromStr`" reading changes here. It refuses:
    - a refused status without a reason (`MissingReason`)
    - an unknown reason (`UnknownReason`)
    - a reason on any other status (`UnexpectedReason`)
  - The repository reads such a row as `Corrupt`, naming `status_reason`.
- **A profile with another UUID** *(the user's choice)*: `refused` with `profile_mismatch`.
  - It's recorded as `security.account_profile_mismatch` (Failure), not `account.set_status`.
  - One `error` line, and nothing is handed out.
- **A changed name** *(the user's choices)*:
  - Online-mode servers check a join's name against the profile, so the profile's current name is handed out.
  - `mc_name` is updated after the session in a short transaction, a compare-and-set on the exact old name, recorded as `account.rename`. A change of case counts too.
  - If the update fails, the session is still handed out with one `warn`, and the next session tries again.
- **Backoff** *(the user's choice, with an addition)*, on the `Clock`; the next success resets it:
  - Per account, every `Failure` starts a backoff: 5 s, doubling to 5 minutes, no jitter (`ACCOUNT_BACKOFF`).
  - Server-wide, `RateLimited` pauses every account: 30 s, doubling to 10 minutes (`GLOBAL_PAUSE`). Microsoft's 429 applies to the server's IP and client ID, not to one account.
  - A request that needs Microsoft gets `BackingOff { until }` at once (the later of both waits) and never waits.
  - **The addition:** the backoff applies only to requests that would call Microsoft. A cached session still outside the margin is handed out during any backoff, the account's own or the global one. A test pins it: during a global pause, a second account's cached session is still handed out.
  - The schedule is fleet-core's `resilience::Doubling`, a policy like `LockoutPolicy::lock_for`.
- **No Microsoft access token is kept** *(the user's choice)*: every new Minecraft token is a refresh plus a session. With tokens that live about a day, that's about one refresh per account per day, plus fresh requests.
- **The margin** *(the user's choice)*: `REUSE_MARGIN`, 10 minutes, a constant. A freshly minted session is always handed out; the margin only decides whether a cached one is reused.
- **Fresh requests** *(the user's choice)*: `session(account, fresh)`. A fresh request reuses only a session whose minting started after it arrived.
  - Each slot numbers its mintings. A request notes the number before its first await, and the flight takes the next number right before `minecraft_session`, so a minting that started before the request never counts.
  - A burst of fresh requests costs one refresh. The backoff still applies to them.
- **Audit** *(the user's choices)*. None of these has an actor or IP, and the target is the account:
  - `account.set_status` (Success) for `invalid_grant`, with metadata `status`, `previous_status` and `reason`. The reason is `invalid_grant`, though the column stays NULL.
  - `account.set_status` for every refusal too, with `xerr` for `xbox_other` only.
  - `security.account_profile_mismatch` (Failure) for the UUID mismatch, with `status` and `previous_status`, never either UUID.
  - `account.refresh` (Success) for every stored rotation, with `rotated`: whether Microsoft sent another token than the one sent, compared in constant time. That's about one entry per account per day, and it shows when the server used each account's token. No new unaudited exemption.
  - `account.rename` (Success) with `name` and `previous_name`.
- **A stored token that can't be used** *(the user's choice)*: an envelope that doesn't open, or a plaintext that isn't a token.
  - The answer is `Envelope` or `StoredToken`, neither retryable.
  - One `error` line names the account and the vault's or the token rule's error, never a byte.
  - No write, no audit, no call to Microsoft: `vault status` and `vault rotate-key` already report such envelopes.
  - The plaintext is read with `str::from_utf8`, never `String::from_utf8`, whose error would carry the bytes.
- **A failed store** *(the user's choices, with the clarification at the plan's approval)*:
  - Nothing is handed out and the status is left alone.
  - The rotated token stays in the account's slot, and the next request stores it before any refresh, then refreshes with it.
  - It's discarded only when the row's envelope changed (linked again) or the row is gone. A change of the account's status or of its owner never discards it: a request for an inactive account or a disabled owner is refused before the flight and leaves the token waiting.
  - The same holds for a failed seal or an entry that can't be built.
  - A restart loses the token, like any crash.
- **A disabled owner** *(the user's choice)*: `OwnerDisabled`, not retryable. Nothing is stored, so enabling the owner restores the account. A missing owner, which the foreign key forbids, counts as disabled.
- **Only active Microsoft accounts get a session.** Unknown, offline, pending, `reauth_required` and `refused` accounts are refused before any call. The account and its owner are read on every request, so a cached session is withheld once the account stops being usable.
- **The wait bound** *(the user's choice, with an addition)*:
  - `WAIT_BOUND` is 2 minutes, above the flight's worst case: a 20 s refresh, a 60 s session and a few 5 s acquires.
  - One deadline covers the lock (`InFlight`) and the answer (`TimedOut`). Past it the flight goes on, and the next request finds its result.
  - **The addition:** `with_wait_bound` only shortens it. Zero and anything above `WAIT_BOUND` are `WaitBoundError`, so no caller can make the wait longer or unbounded. It's documented as for tests.
- **serve doesn't build it** *(the user's choice)*: P10.6 wires it over D's adapter, with a child of serve's shutdown token, and calls `shutdown()` within serve's deadline.
- **Hands-on** *(the user's choice)*: none. Everything new is local logic the fake drives, and Microsoft's side hasn't changed since B.
- **Readings of these decisions** (in the plan the user approved, or found while building):
  - **Compare-and-set.** Every write reads its row inside its own `BEGIN IMMEDIATE` transaction first, and the audit entry is built and the token sealed before that (ADR-0015).
    - Storing a rotation compares only the envelope. A status changed meanwhile doesn't lose the token; it only answers `NotActive`.
    - A status change happens only while the account is still active and holds the envelope that was used. Otherwise nothing changes, and the current state answers: `NotActive`, `ChangedMeanwhile` or `NotFound`.
  - **The flight reads the account again** once it holds the lock, since the flight before may have rotated the token.
  - **The global reset.** A minted session starts the pause's run over, so the next 429 waits 30 s again. A running pause still runs out: a success that started before the 429 says nothing about the rate limit.
  - **A kept token during a backoff** is stored anyway, since that calls no Microsoft; the backoff still answers the request.
  - **Slots** exist only for accounts that passed the first read, and are never removed, so an account never has two flights. A refusal clears the cached session. A missing row also drops the kept token: a flight finds the row gone itself, and a request takes the slot's lock without waiting (`try_lock`).
  - **`retryable()`** says yes for `BackingOff`, `Microsoft`, `InFlight`, `TimedOut`, `ChangedMeanwhile`, `ShuttingDown`, `Interrupted`, `Seal`, and a busy or failing database. It says no for the refusals, `Envelope`, `StoredToken`, a corrupt or unstorable value, and `Audit`. An entry that can't be built fails the same way every time; the plan had listed it with the store errors.
  - **Logs** carry the account's ID and an error's text, never a token, a name, a UUID or a byte. Levels:
    - `warn`: `reauth_required`, a refusal, a kept token stored at last, the global pause, a failed rename.
    - `error`: a UUID mismatch, an unusable stored token, a failed store, a flight that ended without an answer.

### As built (group C)
- **fleet-core:**
  - `account::RefusalReason`, `AccountStatus::Refused`, `AccountStatus::{reason, from_columns}`, and `AccountError::{UnknownReason, MissingReason, UnexpectedReason}`.
  - `resilience::Doubling`.
  - The audit actions `account.refresh`, `account.set_status`, `account.rename` and `security.account_profile_mismatch`.
- **fleet-server:**
  - `0009_mc_accounts_status_reason.sql`, and `AccountWrites::{set_status, rename}`.
  - `app::session_tokens`: `SessionTokenService`, `SessionTokenPorts`, `SessionTokenError`, `WaitBoundError` and the four constants. Private modules: `slot` (the slot, the backoff and the pure checks) and `flight` (the task).
  - `MinecraftSession` derives `Clone`.
  - The service's module joins the `msauth` coverage gate.
- **Tests,** each red first against compiling stubs that returned wrong values:
  - **fleet-core:**
    - 33 of 34 account tests. The one that passed checks the errors' fixed messages.
    - 10 of 11 `Doubling` tests. The one that passed checks the accessors.
    - The action names snapshot.
  - **The repository:** 4 of 5. `set_status` leaving an offline or missing account alone was shown red by dropping `kind = 'microsoft'`. The migrations snapshot is red too.
  - **The fake's gate:** 2 tests, written after it and shown red by mutations: a gate that never says it was entered, and a dropped gate that panics.
  - **The service's unit tests:**
    - `retryable()`: 12 of 23. The 11 that passed are the cases whose answer is the stub's `false`, and the messages.
    - The slot's checks: 9 of 21. The 12 that passed answer "no" or "none", like the stub.
  - **The service:** all 49 tests in `tests/accounts/session_tokens/`. The stub's `session` answered `Err(InFlight)`, its `shutdown` did nothing, and its `with_wait_bound` accepted everything.
  - **Four of those failed against the stub before reaching their assertion,** so each was shown red by a mutation:
    - a `debug` line with the rotated token: the redaction test
    - a `Debug` that prints the cached tokens
    - an unbounded wait for the flight (red at the test's own 10 s bound)
    - the minting numbered when the session is cached instead of before it starts: the fresh request's test
  - **Ten more tests came from the coverage run** for the races and failure paths it showed untested. Each was shown red by a mutation of the code it guards:
    - a kept token stored during a pause
    - a kept token stored while the status changed just before
    - a kept token whose account was deleted just before its store
    - a status change during the refresh
    - a link again during the refresh
    - an account deleted during the refresh (with a rotated token, and with `invalid_grant`, each by its own mutation)
    - a refusal whose status write fails
    - a stop while a kept token is stored
    - a database failure that keeps the cache
    - the flight's second read
  - **The test store's gates.** `FlakyStore`'s write gate and account-read gate hold one call while the test changes the row, so each race runs the same way every time.
  - **The coverage gate's script test:** 1 of 1.
  - Every mutation was reverted and checked by checksum.
- **Coverage:** on Windows with `--no-rustc-wrapper`, the `msauth` gate's lines were at 99.05 % and `app/session_tokens/` at 99.7 %. The two lines left: a `set_status` that finds no row inside the transaction that just read it, and a rename that loses its compare-and-set.
- **Found during the build:**
  - tracing's macros read a constant as a field: `warn!(account_id = %id, REAUTH)` logged a field named `REAUTH` and no message. The messages are format strings now (`"{REAUTH}"`), and the tests check each line's message and level.
  - In one instrumented, fully parallel coverage run on Windows, five of these tests failed in `Database::connect` under the load. The rerun and every normal run passed.
  - The request's own stop check before spawning the flight is gone: the biased `select!` already answers `ShuttingDown` first, and the flight checks again right before the refresh.

### Notes for later groups
- **B (P9.3):** done (above). Two of these notes changed with the user's decisions. fleet-server, fleet-client and the desktop crate stay in `stable_crates`, and azalea-auth doesn't reach fleet-server at all. The redaction test runs with the configured filter at `trace`, as planned.
  - ~~fleet-server leaves `stable_crates` together with fleet-client and the desktop crate, since both dev-depend on fleet-server and `stable-check` builds `--all-targets`.~~
  - ~~azalea-auth comes from the same azalea release as fleet-mc.~~
- **C (P9.5):** done (above); every note below was applied. A rotated refresh token is stored with the account's ID as the AAD's record (`VaultPurpose::MsRefreshToken`). `reauth_required` is the status's name.
  - *(from B)* **Tokens:**
    - A stored refresh token becomes a `RefreshToken` through `TryFrom<SecretString>` on the vault's plaintext.
    - After every refresh, store `MicrosoftTokens.refresh`: it's the token sent when Microsoft returned none.
    - Only `RefreshError::InvalidGrant` marks an account `reauth_required`. The other `Failure`s are Microsoft's or the network's, and `Unexpected` names the rule broken.
  - *(from B)* **Sessions:** `minecraft_session` checks the entitlements and the profile every time, so compare its `uuid` with the account's `McUuid` before handing out its token.
  - *(from B)* **Retries:** the adapter makes none; the service owns them, and its single-flight keeps a rotated token from being spent twice.
  - *(from B)* **Tests:** the service's tests join the `tests/accounts/` binary with its fake, and its module joins the `msauth` coverage gate.
- **D (P9.4, P9.7):**
  - A Member's or an Admin's link starts `pending`, the Owner's `active`.
  - *(from B)* **The poller:**
    - It polls once per `DeviceFlow.interval` and switches to `PollInterval::slowed_down()` after a `SlowDown`.
    - Declined and Expired are outcomes, not errors.
    - Its own bounds (the flow's `expires_at`, the server's caps) come on top of the adapter's 20 s per poll.
    - The fake's `Script::Hang` tests its bounds and its cancellation.
    - `DeviceFlow.code` is a secret, kept only in memory.
  - *(from B)* **Wiring:** `ReqwestMicrosoftAuth::new(clock)` is built in `serve`, after main.rs has installed the TLS provider.
  - *(from B)* **Unknown codes:** `Rule::OAuthCode` names an OAuth code the port doesn't map, so a surprise is visible in the logs.
  - A linked UUID or offline name is `InsertAccountError::Taken`, whose refusal names nobody.
  - *(from C)* A new link's status has no reason (`AccountStatus` builds the pair), and D's poller uses `minecraft_session` itself: the session-token service is for linked accounts only.
  - `account.link` records the creator, the name and the UUID.
  - `[accounts]` arrives here.
- **E (P9.1's `account_grants`, P9.6):**
  - The approve and refuse endpoints, and whether each needs a step-up.
  - The grants table.
  - P9.6's open point: how a Member picks a grantee without enumerating usernames.
  - *(from C)* **Linking again** must check that the profile's UUID is the account's, and sets the status to `active` (clearing a `refused` reason) together with the new envelope. Its envelope change makes a flight in progress drop its rotated token (`ChangedMeanwhile`).
  - *(from C)* In C's design a `refused` or `reauth_required` account comes back through linking again; E decides whether approving or refusing may touch them too.
  - *(from C)* Deleting an account needs nothing from the service: a request finds the row gone, and the slot's kept token is dropped.
  - P9.1 is ticked here.
- **F (P9.8):**
  - The warning text above, in the link dialog only.
  - *(from B)* The verification link is already checked (https, `www.microsoft.com` or `microsoft.com`); the user code is shown but never logged.
  - *(from B)* Each `XboxRefusal`, `NotOwned` and `NoProfile` has a text for the user. `Failure::Certificate` on a Linux server means a missing CA store (P12.6).
  - *(from B)* The threat model's B5 row gains the adapter: TLS with certificate checks, bounded answers, no redirects or proxies.
  - Home's count of pending links for Admins and the Owner.
  - The threat model's device-code phishing entry and B5/B6, citing A's swap tests.
  - The DoD's "rotate-key works on a copy of the DB" again, with a linked account.
  - *(from C)* A text per `RefusalReason` and per status, for the account list and the re-auth prompt; `xbox_other`'s code is in the audit log only.

## Consequences
- **Easier:**
  - A Minecraft account is linked once, and a victim of a phished link sees it waiting for approval before any bot uses it.
  - Every stored secret can be moved to a new key in one transaction, without holding the write connection for any crypto. A tampered or unopenable secret stops the rotation and is recorded.
  - An operator can check, without any key, which key files are still needed (`vault status`, exit 3), and a rotation can't change the schema by accident.
- **Harder:**
  - Two tables hold envelopes, so every new one joins `ports::envelopes` and the rotation.
  - A running server only knows the keys it loaded at startup, and any write during a rotation makes it ask to run again, so the documented procedure stops the server for it.
  - The account's status set lives only in Rust, so a stored status an older server doesn't know reads as corrupt.
  - Approval adds a step to every Member's and Admin's link.
- **Group B, easier:**
  - The later groups can tell apart what they need: a declined or expired code, `invalid_grant` and only it, a 429, a server error, an Xbox refusal, a game not owned, a missing profile.
  - Nothing Microsoft sends can make the server wait unbounded, allocate unbounded, panic or log a secret, and every rule is tested without Microsoft.
  - azalea-auth, its trace logs and `rsa` stay out of fleet-server, and cargo-deny keeps them out.
- **Group B, harder:**
  - The server owns the Microsoft, Xbox Live and Minecraft login protocol. A change at Microsoft or Mojang isn't fixed by an azalea bump; each bump compares our requests with azalea-auth's (ADR-0003).
  - The client ID is copied from azalea-auth, not read from it.
  - The form and JSON bodies reqwest sends keep unwipeable copies of the tokens.

- **Group C, easier:**
  - A rotated refresh token is never lost to a dropped caller, a timeout or a failed store, and nothing is handed out before it's stored.
  - One refresh per account at a time, and one per burst of fresh requests. A 429 pauses every account, while cached sessions keep working.
  - Every way Minecraft's side refuses an account is a status with its reason, visible to the app, and every change is audited.
- **Group C, harder:**
  - A kept token lives only in memory: a restart while the database fails loses it, and the account may need linking again.
  - A request reads the account and its owner every time, two queries even for a cached session.
  - The status is two columns now, read together through `from_columns`.

## Alternatives considered
- **Five groups, with P9.3 and P9.5 together:** the first Microsoft-facing PR would hold both the adapter and the service that uses it.
- **Both tables in A:** `account_grants`' columns fixed before its endpoints exist.
- **Offline accounts with their derived UUID:** uuid's `v3` feature, a new dependency (`md5`), and still a lowercased-name index.
- **Microsoft accounts only now:** offline accounts would need a migration that rebuilds the table.
- **A CHECK on the status:** a new status would need a table rebuild.
- **The types in fleet-server, or a plain `Uuid`:** DTOs couldn't re-export them, or IDs could be mixed up.
- **A `key_id` column:** a second copy of what the header holds, and `user_mfa` would differ.
- **All crypto inside the transaction:** a new exception to ADR-0015's rule.
- **Re-encrypting every envelope:** a second run rewrites everything, and a forgotten key change isn't noticed.
- **Migrating in the vault commands:** a status check that writes, and a rotation that changes the schema.
- **Only a Success entry:** a tampered envelope would leave no trace in the log.
- **Stopping at the first bad envelope:** one run per problem.
- **An API route for key rotation:** a route that can't add a key, and a step-up decision for it.
- **Key-aware recipes, or none:** more knobs than the one check needs, or environment variables typed by hand.
- **Approval only by the Owner, or a warning on Home:** every Member's link would wait for one person, or the warning would be noise for everyone.
- *(group B)* **azalea-auth for every call, as Plan.md said:** refusals would collapse into one error. P9.5 couldn't tell `invalid_grant` from an outage, a declined code would read as pending for 15 minutes, its panics would need catching, and its poll would ignore paused time.
- *(group B)* **A hybrid** (own requests for the poll, the refresh and the Xbox-to-Minecraft chain, azalea-auth for the device code and the entitlements): azalea-auth, `rsa` and its trace logs would stay for two calls whose 429 still couldn't be told apart.
- *(group B)* **clippy method bans on `Client::new` and `Client::default`:** clippy can't name a trait impl's method, and reqwest's `Default` calls `new` inside reqwest.
- *(group B)* **A `cargo tree` check for azalea-auth, or none:** it would cover only fleet-server, or only review.
- *(group B)* **Retry-After kept, timeouts as config keys, retries in the adapter:** more input to bound, more config to test, and hidden traffic under an outage.
- *(group C)* **moka for the cache and the coalescing:** a dropped caller would let a waiter refresh again with a spent token, and repeated aborts panic; the per-account lock was needed anyway.
- *(group C)* **Plain `tokio::spawn` owned by the caller's wait (fleet-client's exception):** a fifth exception to the task rule, and shutdown couldn't wait for a refresh in flight.
- *(group C)* **`reauth_required` for every refusal, or keeping the account active:** a misleading status (linking again doesn't buy the game), or nothing in the account list.
- *(group C)* **A flat `Refused` with a separate reason field:** a refused status without its reason could be built.
- *(group C)* **A CHECK on the reason, or a reason for `reauth_required` too:** a table rebuild for a new reason, or a column that says what the status already says.
- *(group C)* **Caching the Microsoft access token:** fewer refreshes, but a broader secret in memory.
- *(group C)* **A timestamp instead of mint numbers for fresh requests:** a manual clock that doesn't move, or two requests in one millisecond, would blur before and after.
- *(group C)* **An unaudited exemption for the rotation's store:** the log couldn't show when an account's token was used.
- *(group C)* **Discarding a token whose store failed, or retrying inside the task:** the next refresh would spend a token Microsoft may have invalidated, and the account would need linking again.
- *(group C)* **A configurable margin or wait bound:** more config to validate, and a wait bound that could be made unbounded.
- *(group B)* **The fake in fleet-testkit with the port in fleet-core, or a shared `tests/common`:** services generic over the provider unlike every other server port, or `dead_code` in each binary that doesn't use every helper.
- *(group B)* **Testing only the mapping, or a public endpoint override over TLS:** the reader, the bounds and the client's settings untested, or an override reachable from production code.
