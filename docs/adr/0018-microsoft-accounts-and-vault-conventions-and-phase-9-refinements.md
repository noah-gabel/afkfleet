# 0018. Microsoft accounts and the vault: conventions, Phase 9 refinements

- **Status:** Accepted
- **Date:** 2026-10-14
- **Related:** Plan.md Phase 9 (P9.1–P9.8), §7.2, §7.4, §7.5, Appendix D; ADR-0010, ADR-0011, ADR-0015, ADR-0016, ADR-0017

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

### Notes for later groups
- **B (P9.3):**
  - fleet-server leaves `stable_crates` together with fleet-client and the desktop crate, since both dev-depend on fleet-server and `stable-check` builds `--all-targets`.
  - azalea-auth comes from the same azalea release as fleet-mc.
  - The server's redaction test runs with the configured filter at `trace`.
- **C (P9.5):** a rotated refresh token is stored with the account's ID as the AAD's record (`VaultPurpose::MsRefreshToken`). `reauth_required` is the status's name.
- **D (P9.4, P9.7):**
  - A Member's or an Admin's link starts `pending`, the Owner's `active`.
  - A linked UUID or offline name is `InsertAccountError::Taken`, whose refusal names nobody.
  - `account.link` records the creator, the name and the UUID.
  - `[accounts]` arrives here.
- **E (P9.1's `account_grants`, P9.6):**
  - The approve and refuse endpoints, and whether each needs a step-up.
  - The grants table.
  - P9.6's open point: how a Member picks a grantee without enumerating usernames.
  - P9.1 is ticked here.
- **F (P9.8):**
  - The warning text above, in the link dialog only.
  - Home's count of pending links for Admins and the Owner.
  - The threat model's device-code phishing entry and B5/B6, citing A's swap tests.
  - The DoD's "rotate-key works on a copy of the DB" again, with a linked account.

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
