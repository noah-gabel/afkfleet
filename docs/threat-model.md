# Threat model

> **Status: skeleton (P0.13).** Based on Plan.md §7.1. It gets expanded as the components are built:
> - the Minecraft servers (B4) in **P3** (done)
> - authentication and authorization in **P7** (done)
> - the desktop app (B2, B7) in **P8** (done)
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
| Brute force / credential stuffing | Argon2id; per-IP and per-account throttling; lockout with exponential delay; mandatory 2FA for every user; invite-only registration | `the_fifth_wrong_password_locks_the_name_and_the_right_one_waits`, `wrong_codes_count_toward_the_lockout`, `the_sixth_login_step_in_a_minute_from_one_address_is_429`, `an_invite_registers_once_and_its_reuse_is_refused_and_recorded`; B1 below |
| Token theft | Short-lived opaque access tokens; refresh-token rotation with reuse detection that revokes the whole family; tokens hashed at rest; refresh token only in the OS keychain, never in JS or localStorage (ADR-0006, ADR-0007) | `reusing_a_replaced_refresh_token_revokes_the_whole_session`, `an_access_token_counts_until_its_last_millisecond`, `a_minted_tokens_hash_is_the_hash_of_its_text`, `no_answer_of_a_whole_session_holds_a_token`, `a_first_login_sets_up_2fa_and_starts_a_session_whose_token_only_the_keychain_holds`; B2 below |
| Privilege escalation by a friend | One pure `authorize()` for every route, deny by default; per-account grants, never to oneself; Admins can't touch the Owner or other Admins; only the Owner changes roles and enrolls agents; authorization-matrix test; route-coverage test; audit log | `authorization_matrix`, `every_route_that_needs_credentials_answers_401_without_them`, `a_member_cant_make_themselves_an_admin`, `an_admin_cant_change_the_owner`; P13.1 (planned) |
| Bot abuse (`/op`, `/pay`, …) | Slash commands need **Manage**, or must be on the allowlist (compared exactly and case-sensitively), also inside modes; per-bot and per-user chat rate limits; every sent message is audited | P2.9, P11.3 |
| Database or backup leak | MS tokens and TOTP secrets encrypted with XChaCha20-Poly1305, bound to their record by AAD; key stored outside the DB and backups; passwords hashed with Argon2id; session tokens stored as SHA-256 | `the_plaintext_never_appears_in_the_envelope`, `another_record_is_rejected`, `a_minted_tokens_hash_is_the_hash_of_its_text`; P9.2, P12.3 (planned) |
| Rogue or compromised agent | mTLS with server-signed certificates; single-use enrollment tokens that only the Owner creates; fingerprint allowlist and revocation; an agent only ever gets session tokens for bots assigned to it | P2.9, P10.3, P10.6, P10.9 |
| Malicious chat / XSS | Chat is sanitized plain text (control characters, format codes, bidi controls and invisible characters stripped, length capped); React escaping; no `dangerouslySetInnerHTML`; strict CSP | P2.3, P8.3, P11.9 |
| DoS | Rate limits, body-size limits, timeouts, bounded queues, an argon2 semaphore, WebSocket connection caps | `a_clients_burst_is_allowed_then_refused_until_its_bucket_refills`, `with_every_slot_and_waiting_place_taken_the_next_caller_is_busy_at_once`; P6.6, P11.5, P13.4 (planned) |
| Supply chain | `cargo deny` (advisories, licenses, sources, bans incl. a single TLS provider), `pnpm audit`, committed lockfiles, minimal crate registry, actions pinned to commit SHAs, weekly audit run (ADR-0009) | P0.5, P0.11 |
| Information leakage | Generic error messages with server-generated request IDs (a client's `x-request-id` never survives), also for errors a layer or a panic produces; no stack traces and no panic text in responses; security headers on every response (`nosniff`, `no-referrer`, `no-store`, a CSP and `X-Frame-Options: DENY` against framing); uniform login responses plus a dummy hash against username enumeration; secrets redacted from logs: credentials marked sensitive, and the request log holds no headers, no bodies, no client paths and no query strings | `an_unknown_name_a_wrong_password_and_a_disabled_user_answer_alike`, `a_refusal_is_logged_with_its_reason_and_no_part_of_the_token`; P6.5, P6.6, P9 |

## Per-boundary analysis
To be written per boundary (STRIDE: spoofing, tampering, repudiation, information disclosure, denial of service, elevation of privilege) in the phase listed under "Expanded in" above.

### Authentication and authorization (B1)
*Expanded in P7 (ADR-0006, ADR-0010, ADR-0016).*

**What crosses the boundary.** Every request to the HTTP API, through Caddy's TLS:
- **From the client:** usernames, passwords, invite codes and 2FA codes in request bodies; access, mfa and setup tokens as `Authorization: Bearer`; refresh tokens in a body; IDs in paths and page cursors in query strings.
- **To the client:** an mfa or setup token after the password step, the session's tokens after the code step or a refresh, an invite's code once, recovery codes once, and profiles, sessions, users and invites as lists.
- **The client's address:** the socket peer, or behind a proxy listed in `[http] trusted_proxies` the right-most untrusted `X-Forwarded-For` address. It keys the per-IP limits and is recorded in the audit log.

**What an attacker controls:** every byte of a request (body, headers, path and query), how many requests come and from how many addresses, the timing it measures, and possibly a stolen password, token or laptop. An insider holds a real session with the role of a Member or an Admin.

| STRIDE | Threat | Mitigation | Verified by |
|---|---|---|---|
| Spoofing | Guessing or stuffing passwords | Argon2id at OWASP's baseline or above; a per-IP limit on every request and one bucket for both login steps; the per-username lockout (5 failures lock for a minute, doubling up to an hour, unknown names alike); 2FA for every user | `the_fifth_wrong_password_locks_the_name_and_the_right_one_waits`, `each_further_failure_doubles_the_lock_up_to_an_hour`, `the_sixth_login_step_in_a_minute_from_one_address_is_429`, `the_two_login_routes_share_one_bucket` |
| Spoofing | Guessing 2FA codes with the password | Wrong codes count toward the same lockout; an mfa token takes 3 wrong codes; a code fits only its own and the neighbouring time steps | `wrong_codes_count_toward_the_lockout`, `the_third_wrong_code_uses_the_mfa_token_up`, `a_code_outside_the_window_or_of_another_secret_is_wrong` |
| Spoofing | Replaying a TOTP code or a recovery code | Only a step after the last used one is accepted, with a conditional write; the confirming code's step counts as used; a recovery code is used once | `a_totp_code_is_accepted_once_and_no_older_code_after_it`, `the_confirming_code_cant_log_in_afterwards`, `a_recovery_code_is_accepted_once`, `a_recovery_code_logs_in_once` |
| Spoofing | A stolen refresh token | Rotation on every refresh; a replaced token presented again revokes its whole session and is recorded; sessions end 7 days idle and 30 days after login | `reusing_a_replaced_refresh_token_revokes_the_whole_session`, `two_refreshes_with_the_same_token_give_one_rotation_and_one_reuse`, `a_session_idle_for_seven_days_cant_refresh`, `a_session_past_thirty_days_cant_refresh_however_often_it_did` |
| Spoofing | A token of one kind used as another | Each kind has its prefix and shape, and each route takes one kind; the route registry checks every route with every other kind | `a_setup_token_is_refused_on_every_route_an_access_token_is_for`, `an_mfa_token_is_refused_on_every_route_that_needs_credentials`, `an_access_token_is_refused_on_every_setup_route`, `a_token_presented_as_the_other_kind_is_refused` |
| Spoofing | Registering without an invite, or with one already used | Invite-only registration; an invite is single-use and expires; its creator must still be allowed to invite; its use is a conditional write in the registration's transaction | `an_invite_registers_once_and_its_reuse_is_refused_and_recorded`, `an_invite_is_refused_from_its_expiry_on`, `two_registrations_with_one_invite_create_one_user`, `an_invite_whose_creator_lost_the_right_is_refused_and_recorded` |
| Spoofing | A client choosing its own address (`X-Forwarded-For`) to dodge per-IP limits or forge the audit log | The header counts only from a trusted proxy, walked from the right; a /0 proxy is refused; `serve` passes the real peer address | `an_untrusted_peers_forwarded_for_is_ignored`, `a_trusted_proxys_forwarded_for_names_the_client`, `requests_over_real_connections_reach_the_per_ip_limit_with_their_address` |
| Tampering | A session outliving its user's role change or disabling | Every request reads its session and user; a role change or disabling revokes every session of the user | `a_role_change_counts_on_the_next_request`, `a_disabled_users_session_is_rejected`, `the_owner_promotes_a_member_whose_sessions_all_end`, `an_admin_disables_a_member_who_is_refused_from_then_on` |
| Tampering | A change landing between a check and its commit | Every commit that acts on a verified password re-reads the session, the user and the password's time; every admin write re-reads the actor and the target and asks `authorize()` again | `a_password_changed_between_the_steps_refuses_the_login_and_keeps_the_hash`, `a_password_changed_before_the_commit_is_403_uncounted_and_unstamped`, `a_password_changed_since_the_step_up_needs_a_new_one`, `an_admin_demoted_meanwhile_cant_disable_a_member`, `a_member_promoted_meanwhile_cant_be_disabled_by_an_admin`, `a_creator_demoted_meanwhile_leaves_the_invite_unusable` |
| Tampering | A stored TOTP secret moved to another user or altered | The vault's envelope is bound by its AAD to the user's ID and the purpose; a golden envelope pins the format | `every_changed_byte_is_refused`, `another_record_is_rejected`, `the_golden_version_1_envelope_still_opens` |
| Repudiation | Denying a login, a role change, a disabling or an invite | Every write commits with its audit entry; a real invite that can't be used is recorded too; merged action names are never renamed | `the_owner_promotes_a_member_whose_sessions_all_end`, `an_invite_registers_a_user_who_then_sets_up_2fa_at_login`, `each_lock_records_security_lockout_in_the_failing_attempts_commit`, `action::tests::every_name_is_kept` |
| Information disclosure | Which usernames exist | One answer for an unknown name, a wrong password and a disabled user, after a dummy hash with today's parameters; unknown names lock alike; the typed name is never stored | `an_unknown_name_a_wrong_password_and_a_disabled_user_answer_alike`, `an_unknown_name_locks_exactly_like_a_real_one`, `checks_for_unknown_users_queue_and_answer_busy_like_real_ones`, `the_typed_name_is_stored_nowhere` |
| Information disclosure | Secrets in logs, errors and `Debug` | Secrets are `SecretString` or `SecretText`; a refusal's log line names its reason, never a token; errors name files, never their content | `a_refusal_is_logged_with_its_reason_and_no_part_of_the_token`, `debug_shows_no_part_of_a_token`, `weak_errors_never_show_the_password`, `errors_name_the_file_and_never_its_content` |
| Information disclosure | A database or backup leak | Tokens and invite codes only as SHA-256, passwords as Argon2id, TOTP secrets in the vault, lockout rows under an HMAC key derived from the vault key | `a_minted_tokens_hash_is_the_hash_of_its_text`, `the_plaintext_never_appears_in_the_envelope`, `the_lockout_key_is_the_hmac_of_its_label_under_the_active_key` |
| Information disclosure | Another user's existence or resources | `authorize()` answers 404 to an actor who can't even see a resource; a user's own routes answer 404 for anyone else's | `members_cannot_see_other_users_or_invites`, `a_member_gets_404_for_any_other_user`, `a_member_gets_404_for_any_invite`, `someone_elses_or_an_ended_session_is_404` |
| Denial of service | Floods of requests | A per-IP limit on every request, health checks and unknown paths included; per-class limits for login, refresh and register; a per-user limit on every authed request | `a_clients_burst_is_allowed_then_refused_until_its_bucket_refills`, `unmatched_paths_and_wrong_methods_spend_the_per_ip_limit`, `refresh_and_register_have_their_own_limits`, `a_users_burst_is_allowed_then_refused_and_users_dont_share` |
| Denial of service | Exhausting argon2 | A bounded queue answers `busy` at once; length caps before any hashing; a bad invite or a taken name is refused before hashing | `with_every_slot_and_waiting_place_taken_the_next_caller_is_busy_at_once`, `a_cancelled_caller_keeps_its_slot_until_the_work_finishes`, `a_password_over_1024_characters_is_422`, `an_unknown_or_malformed_code_is_the_same_422_and_never_recorded` |
| Denial of service | Growing tables | Expired lockout streaks are purged in batches; a creator holds at most 20 pending invites | `the_purge_deletes_every_expired_streak_in_batches_and_keeps_the_others`, `the_twenty_first_pending_invite_is_refused` |
| Elevation of privilege | A Member becoming an Admin, an Admin acting on the Owner or another Admin, an Owner invite | One pure `authorize()` with an exhaustive matrix; only the Owner changes roles, between Member and Admin; Admins disable and enable only Members; nobody invites an Owner | `authorization_matrix`, `a_member_cant_make_themselves_an_admin`, `an_admin_cant_change_the_owner`, `an_admin_cant_disable_another_admin`, `an_admin_cant_change_any_role`, `nobody_makes_another_owner`, `an_admin_cant_invite_an_admin`, `nobody_invites_an_owner` |
| Elevation of privilege | A route without authentication or step-up | Routes exist only through the registry; every route that needs credentials answers 401 without them, every step-up route 403 without a fresh stamp; the public routes are an allowlist | `every_route_that_needs_credentials_answers_401_without_them`, `the_check_catches_an_authed_route_that_doesnt_ask_for_credentials`, `every_step_up_route_answers_403_without_a_fresh_step_up`, `outside_dev_mode_the_public_routes_are_exactly_the_allowlist` |
| Elevation of privilege | A stolen session used for sensitive actions | Changing the password, replacing 2FA and every admin write (a role change, disabling or enabling a user, creating or revoking an invite; since Phase 8, group E) need the password again within 5 minutes; a password change or a 2FA replacement ends every other session | `the_right_password_allows_step_up_actions_for_five_minutes`, `a_change_needs_a_fresh_step_up`, `a_stepped_up_user_changes_their_password_and_every_other_session_ends`, `a_replacement_swaps_the_secret_and_the_codes_and_revokes_the_other_sessions`, `a_user_change_without_a_fresh_step_up_is_refused_and_changes_nothing`, `creating_an_invite_needs_a_fresh_step_up`, `revoking_an_invite_needs_a_fresh_step_up` |

**P7.12's security tests.** Each item of Plan.md P7.12, with the tests that check it, in the group that built its defense (ADR-0016):

| Item | Where | Verified by |
|---|---|---|
| User enumeration | over HTTP (D) | `an_unknown_name_a_wrong_password_and_a_disabled_user_answer_alike`, `the_typed_name_is_stored_nowhere` |
| Lockout | the service (C), over HTTP (D) | `four_failures_dont_lock_and_the_fifth_locks_for_a_minute`, `each_further_failure_doubles_the_lock_up_to_an_hour`, `a_locked_name_is_refused_and_the_refusal_isnt_counted`, `an_unknown_name_locks_exactly_like_a_real_one`, `the_fifth_wrong_password_locks_the_name_and_the_right_one_waits`, `wrong_codes_count_toward_the_lockout`, `wrong_step_up_passwords_lock_the_name_for_login_too` |
| Refresh reuse | the service (B), over HTTP (D) | `reusing_a_replaced_refresh_token_revokes_the_session_and_records_it`, `reusing_a_replaced_refresh_token_revokes_the_whole_session` |
| Expired or disabled sessions | `AuthUser` (B), refresh (D), disabling (E) | `a_disabled_users_session_is_rejected`, `an_expired_access_token_is_refused_like_any_other`, `a_session_idle_for_seven_days_cant_refresh`, `a_session_past_thirty_days_cant_refresh_however_often_it_did`, `an_admin_disables_a_member_who_is_refused_from_then_on` |
| Escalation | over HTTP (E) | `a_member_cant_make_themselves_an_admin`, `an_admin_cant_change_the_owner`, `an_admin_cant_disable_another_admin`, `an_admin_cant_change_any_role`, `nobody_makes_another_owner`, `an_admin_cant_invite_an_admin`, `nobody_invites_an_owner` |
| Invites: reuse and expiry | over HTTP (E) | `an_invite_registers_once_and_its_reuse_is_refused_and_recorded`, `two_registrations_with_one_invite_create_one_user`, `an_invite_is_refused_from_its_expiry_on` |
| TOTP replay | the service and the repository (D) | `a_totp_code_is_accepted_once_and_no_older_code_after_it`, `the_confirming_code_cant_log_in_afterwards` |

**Where the code lives.** The auth code spans five crates on purpose: each part lives where its rules about IO, dependencies and reuse put it.

| Crate, layer | What | Why there |
|---|---|---|
| fleet-core `auth`, `authz`, `audit` | The pure rules: new passwords, session expiry and refresh, the lockout schedule, TOTP's window and replay, invites' states, the step-up window; `authorize()`; the audit action names | No IO and no clock: every rule is tested exhaustively on fixed times, without a database (ADR-0010, ADR-0016) |
| fleet-server `app/` | The use cases: login, 2FA, tokens and sessions, the lockout, users, invites | Each owns its transaction, its audit entry and the re-check before its commit |
| fleet-server `ports/` | The traits: the store, users, sessions, invites, second factors, passwords, tokens, the vault | The services see only traits, never sqlx or a crypto crate |
| fleet-server `infra/` | SQLite repositories; argon2, SHA-256 tokens, HMAC lockout keys, the XChaCha20-Poly1305 vault, TOTP, recovery codes; the QR code | The crypto and the SQL stay at the edge |
| fleet-server `http/` | The route registry, the rate-limit middleware, the extractors (`AuthUser`, `SteppedUpUser`, `SetupToken`, `ValidJson`, `ValidPath`, `ValidQuery`), the handlers, the error mapping | A handler only parses, authorizes, calls a service and maps the result |
| fleet-startup `secret_file` | Reading every `*_file` secret, the vault keys first | One set of rules for both binaries' secret files (ADR-0016) |
| fleet-api-types | The DTOs, `SecretText`, `Timestamp`, the TypeScript export | Shared by the server, `fleet-client` and the app; the OpenAPI document and the TypeScript can't drift |

### Desktop app (B2, B7)
*Expanded in P8 (ADR-0007, ADR-0017).*

**What crosses B2, the webview and the app's Rust side.** The UI calls the 20 commands its one capability grants, and nothing else: no core command and no plugin API, and `withGlobalTauri` is off.
- **Into Rust:**
  - the server's address and a custom CA's certificates
  - usernames, passwords (login, registration, step-up, a new password), 2FA and recovery codes, invite codes
  - IDs and page cursors
- **Back to the UI:**
  - profiles, users, invites and sessions
  - the 2FA setup's secret, its QR code and the recovery codes, once each
  - a new invite's code, once
  - errors: a closed code with a fixed message and the server's details
- **Never a token.** fleet-client keeps the access token and the login's step tokens in memory, and the refresh token lives in the Windows Credential Manager.
- **Secrets in the UI** live only in the state of the screen that needs them, never in TanStack Query's caches, and go when the screen does.

**What sits on B7, the user's PC:**
- the refresh token, a generic credential in the Windows Credential Manager, one per server origin (`<origin>.io.github.noah-gabel.afkfleet`), stored with Local persistence (this PC only, never roaming)
- `server-profile.json` in `%LOCALAPPDATA%\io.github.noah-gabel.afkfleet\`: the origin and a custom CA's certificates, no secret
- the exe and WebView2's profile

**What an attacker controls:**
- a page the webview might load: it loads only the app's own bundle (the CSP names no remote origin, and the navigation guard refuses every other URL)
- a compromised npm package inside that bundle
- a hostile server's answers, which are rendered as text
- on B7, the PC itself: whoever uses the unlocked Windows account, or malware running as that user

| STRIDE | Threat | Mitigation | Verified by |
|---|---|---|---|
| Spoofing | A page or script that isn't the app calling the commands | The CSP (`default-src 'self'`, no remote origin, no `unsafe-eval`); a navigation guard that allows only the app's own origin; no global Tauri API; an ACL that refuses every command no capability grants, and every core command. `just tauri-check` pins the settings | `an_app_command_no_capability_grants_is_refused_by_the_acl`, `a_core_command_is_refused_since_no_core_permission_is_granted`, `navigation::everything_else_is_refused` |
| Spoofing | Another server receiving a session's token | The refresh token is stored per origin and resumes only under the identical profile. A switch logs out of the old server first. Plain http goes only to a loopback address. Certificates are always checked, against the system's roots or only the configured CA, and redirects are never followed | `a_token_stored_for_one_origin_is_never_used_by_a_client_of_another`, `another_origin_never_sees_the_token`, `switching_logs_out_of_the_old_server_and_leaves_no_token_for_it`, `url::anything_else_is_refused_without_repeating_it`, `another_ca_is_refused`, `the_system_roots_refuse_a_private_ca`, `a_profile_with_a_ca_trusts_only_that_ca`, `transport::a_redirect_is_never_followed` |
| Tampering | Commands with arguments the Rust side doesn't expect | Every argument deserializes into the shared types, or the command is refused before it runs. Each request is checked with garde before any call; the UI checks it with zod first, and the server checks again | `a_command_with_an_unknown_argument_shape_is_refused_before_it_runs`, `every_contract_command_passes_the_acl_and_answers_as_the_contract_says` |
| Tampering | Injected styles or evaluated code: a foothold for script injection, or a library that does it | `style-src 'self'`, no `unsafe-eval` and a frozen `Object.prototype`. React escapes text, and Biome refuses `dangerouslySetInnerHTML`. The QR code is shown only as an `<img>` data URI. Every Vitest test ends with a check that the document holds no `<style>` (`packages/ui/src/test/styleGuard.ts`). Every Playwright spec runs under the release CSP in Edge and fails on any violation, console error or injected style (`apps/desktop/e2e/fixtures.ts`) | P8.3, P8.6, P8.9 |
| Information disclosure | A token reaching JavaScript, where injected script or a devtools user could read it | No command takes or returns a token. fleet-client holds the access token and the step tokens, and the keychain the refresh token. Devtools exist only in debug builds | `no_answer_of_a_whole_session_holds_a_token`, `a_first_login_sets_up_2fa_and_starts_a_session_whose_token_only_the_keychain_holds` |
| Information disclosure | Secrets kept in the UI's caches after their screen is gone | Every call whose request or answer carries a secret is a direct call, never a query or mutation. The cache tests run each flow and find no password, TOTP secret, QR code, recovery code or invite code in the QueryClient | P8.7, P8.8: the Vitest cache tests of the sign-in, profile and admin screens |
| Denial of service | A broken profile file or keychain entry locking the user out | A file that can't be used starts the app without a profile, which asks for the server again. A keychain entry that can't be read is removed at logout | `a_saved_profile_whose_store_cant_be_built_is_unreadable_client`, `a_logout_removes_a_keychain_entry_that_cant_be_read` |
| Elevation of privilege | The UI offering an action the user's role doesn't allow | The server decides every write with `authorize()`. The UI's mirror only hides what the server would refuse, and its test reads fleet-core's authorization matrix itself (`packages/ui/src/features/admin/permissions.test.ts`) | `authorization_matrix` |

**A stolen or shared PC with a live session (B7).** The app keeps the user signed in, so whoever opens it on the user's unlocked Windows account is that user, without a password.
- **What it gives:**
  - everything that needs no password: the user's profile and sessions, and for an Admin the user and invite lists
  - ending the user's other sessions, and logging out
  - from Phase 9 on, the bots and accounts the user may use, unless that phase puts them behind a step-up (its Plan note)
- **What it no longer gives:** changing the password, replacing the authenticator, and every admin write need the password within the last 5 minutes. The admin writes are a role change, disabling or enabling a user, and creating or revoking an invite; they need it since Phase 8, group E. Guessing the password at the step-up counts toward the username's lockout, which then locks the login too.
- **Malware running as the user can read the refresh token,** as any process of that Windows account can read its generic credentials, and use it elsewhere. Rotation limits that: the first refresh on either side replaces the token, and the other side's next refresh is reuse, which ends the whole session. The real user is logged out, which they notice.
- **The remedies:**
  - from another PC, the session list: sessions can't be told apart, so "Log out all other sessions" ends them all, every page included
  - changing the password or replacing the authenticator, either of which ends every other session
  - an Admin disabling the user, which ends every session
  - a session ends 7 days after its last use and 30 days after its login at the latest

| STRIDE | Threat | Mitigation | Verified by |
|---|---|---|---|
| Spoofing | Someone at the user's unlocked PC acting as them | A step-up for every sensitive action. The session list with "Log out all other sessions", a password change or a 2FA replacement, and an Admin disabling the user. The session's idle and absolute end | `a_user_change_without_a_fresh_step_up_is_refused_and_changes_nothing`, `creating_an_invite_needs_a_fresh_step_up`, `revoking_an_invite_needs_a_fresh_step_up`, `a_change_needs_a_fresh_step_up`, `wrong_step_up_passwords_lock_the_name_for_login_too`, `a_stepped_up_user_changes_their_password_and_every_other_session_ends`, `an_admin_disables_a_member_who_is_refused_from_then_on`, `a_session_idle_for_seven_days_cant_refresh` |
| Information disclosure | Malware running as the user reading the refresh token | The token sits in the Credential Manager with Local persistence, one per origin. Rotation with reuse detection ends the session at the next refresh of either copy | `reusing_a_replaced_refresh_token_revokes_the_whole_session`, `a_lost_refresh_answer_is_reuse_and_ends_the_whole_session` |

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

**What a hostile server controls:** every byte a bot receives, and when it kicks the bot or goes quiet. fleet-core's sanitizer and classifier and fleet-mc's adapter treat all of it as untrusted. The runtime's watchdog (P4.6, planned) is the last line.

| STRIDE | Threat | Mitigation | Verified by |
|---|---|---|---|
| Spoofing | An impostor server, through DNS or the network path | The token goes only to Mojang over TLS, and Mojang binds the join to the server's key, so an impostor can't pass the login on. The rest is an accepted risk (below) | `fleet-mc` `online::slow_online_mode_scenario`; the user's `real_account::manual_real_account_scenario` |
| Spoofing | Forged chat senders: system text naming a player, or a sender the server made up | Senders come only from player and disguised packets, and system chat never gets one. No permission or trigger decision relies on a sender (accepted risk, below) | `map::system_chat_has_no_sender_even_when_its_text_names_one`, `incoming::a_spoofed_sender_in_system_text_stays_text` |
| Tampering | Chat or kick text with control characters, format codes, bidi controls or newlines, aimed at logs and the UI | The core sanitizer strips them and caps the length. Text is stored and rendered as plain text, and chat is logged only at `debug`, as a structured `?` field | P2.3 (`incoming::received_chat_is_always_clean`), `disconnect::kick_messages_are_sanitized_and_capped`, `map::chat_text_and_sender_are_sanitized`; P8.3 (the CSP; B2 above), P11.9 (planned) |
| Tampering | A dependency's panic message that carries server text into the agent's log | The panic hook sanitizes the payload like chat (control, format, bidi and invisible characters), turns line breaks into ` \| ` and caps it at 1024 characters | `text::tests::untrusted_text_becomes_one_capped_line`, the agent's `panic::tests::payloads_become_one_clean_line` and `tests/panic_hook.rs` |
| Tampering | A kick text that pretends to be a ban or a duplicate login | Kicks are classified by translation key, never by text, and a kick without a known key is transient. Per-server conflict texts are opt-in (P4.1, planned; accepted risk below) | P2.4 (`disconnect::classifies_kicks_by_key`, `kicks_without_a_known_key_are_transient`), `map::nested_args_keep_a_ban_permanent` |
| Repudiation | What a bot said on a server | Every message a bot sends is audited. An online account signs its chat, so what it says is attributable to the player, as with any client | P11.3 (planned); signed chat in the user's real-account check |
| Information disclosure | The Minecraft token or the chat-signing key reaching a log | fleet-mc never logs the token, and both binaries cap `azalea_auth` at `info`, whatever the filter names (the agent since P5.2; fleet-server in P9.3, planned). Redaction tests capture every level of every target. The real-account check also allows `PRIVATE KEY` only under `azalea_auth::certs` | `account::tests::the_token_never_appears_in_the_adapters_logs`, `online::slow_online_mode_scenario`, `real_account::manual_real_account_scenario`; the agent's `telemetry::filter::tests::azaleas_rules_hold_for_every_filter` and `tests/telemetry_init.rs` (records through the log crate) |
| Information disclosure | What a server and the DNS learn: the account, the agent's IP, the server names it resolves | Inherent to playing. IP addresses are never looked up, and the DNS fallback is an accepted risk (below) | — |
| Denial of service | Nested translations that render exponentially | fleet-mc renders server text itself and stops at 4,096 characters or 16,384 steps. azalea's own rendering, in its `info` kick log, stays off: the agent holds azalea's targets at `warn` unless a filter directive names one, and warns at startup when one is named above `warn` (P5.2) | `render::nested_arguments_stop_at_the_char_budget`, `nested_empty_arguments_stop_at_the_step_budget`, `map::nested_system_chat_is_cut_off_and_marked_truncated`; the agent's `telemetry::filter::tests::azaleas_rules_hold_for_every_filter` and `telemetry::filter::tests::a_span_directive_without_a_target_never_lifts_azalea` |
| Denial of service | Chat floods | The bounded bridge drops only chat, and counts it. Lifecycle events are always delivered | `bridge::full_queue_drops_only_chat_and_counts_it`, `lifecycle_events_are_delivered_past_the_capacity` |
| Denial of service | A login that stalls, or a flood of events before the join | The connect timeout runs from `connect()` to `Joined` and is checked before events | `driver::a_quiet_server_times_out_at_the_deadline`, `a_flood_of_events_before_the_join_still_times_out_at_the_deadline`, `tests/connector.rs` |
| Denial of service | A server that goes quiet after the join | Packets and ticks stamp the session's liveness, and the watchdog tears a quiet session down | `bridge::forwarded_ticks_and_keep_alives_only_stamp_liveness`; P4.6 (planned) |
| Denial of service | A packet that makes azalea panic or hang (accepted risks below) | One host thread and App per bot, with the single-threaded executor. A panic ends only that session, as crashed. A hung thread is abandoned and counted, and at the limit the agent restarts (P5.3, planned) | `containment::slow_fault_containment_scenario`, `app::every_schedule_runs_single_threaded`, `tests/host_pool.rs` (`hung_thread_is_abandoned_on_shutdown_and_counted`, `pool_refuses_new_threads_once_the_abandoned_limit_is_reached`) |
| Denial of service | Oversized or compressed packets | azalea caps a packet at 8 MiB uncompressed and checks that before decompressing. Accumulated state isn't capped (accepted risk below) | azalea-protocol 0.16.0 `read.rs`, re-checked at every azalea bump |
| Denial of service | Kick and reconnect loops | Retry backoff and the circuit breaker. A duplicate login pauses the bot, which never fights a human | P2.5, P2.6 |
| Denial of service | Leaks across reconnects | Every way a session ends frees its host thread and World. On Linux, the OS thread count doesn't grow past its baseline after a warm-up | `teardown::slow_teardown_scenario` (P3.8), `tests/connector.rs` |
| Elevation of privilege | A server making a bot act for it | Bots act only on their mode and on users' commands, never on received chat. Slash commands need Manage or the allowlist | P2.9, P11.3 (planned) |
| Elevation of privilege | Code execution through parsing | No `unsafe` in our code (`unsafe_code = "forbid"`). azalea is pinned exactly, its advisories are reviewed (ADR-0012), and a fault stays in its host thread | The workspace lints, `cargo deny`, the containment scenario |

## Accepted risks and known limits
- **Keeping an account locked.** The lockout lets anyone who knows a username keep it locked by failing once per lock: one failure an hour once the lock reached its cap. The user's existing sessions keep working, and an unknown name locks exactly like a real one, so the lock tells nothing about which names exist (ADR-0016).
- **Stale hashes after an `argon2_*` change.** A user who doesn't log in keeps a hash made with the old parameters, so a login for that name takes a different time than the dummy hash, and the name stays distinguishable until they log in once (their hash is re-hashed then).
- **Guessing 2FA codes with the password.** At the default threshold of 5, about 33 wrong codes a day get through (5 free, then locks of 1, 2, 4, 8, 16 and 32 minutes, then one an hour). Each guess matches one of about 3 in 10^6 codes (the window's three steps), so about 1e-4 a day and 3.5 % a year for an attacker who has the password and never stops, while the real user stays locked out nearly all the time. `GET /me` counts the wrong codes since the previous login, without the typos of the login that succeeded (Phase 8, group E), and the app warns about them on Home and in the profile, so the user sees the attempts (ADR-0016).
- **An account without 2FA.** An invitee who registers and never finishes the 2FA setup leaves an account that whoever first logs in with its password enrolls. The app logs in and starts the setup right after registering (P8.7), and the Users screen shows such an account's 2FA as "Not set up", with a hint to disable it (P8.8).
- **No app lock.** The app asks for nothing when it starts: a session in the Credential Manager signs the user in (B7 above). The Windows account's own lock and disk encryption protect the PC, and an Owner's or Admin's PC deserves both. Every sensitive action needs the password again (a step-up).
- **A secret on the clipboard.** "Copy all" (recovery codes) and an invite's copy buttons put a secret on the clipboard, where Windows' clipboard history may keep it; their toasts say so, and the text is also on screen to copy by hand.
- **Displacing a login.** Someone with a user's password can replace the user's live login challenge with a password step of their own, the same class as keeping an account locked.
- **Names an invitee can probe.** Whoever holds a usable invite learns, through registration's "is taken", which usernames exist; the register limit (5 an hour per address) bounds it, and the invite isn't spent by a taken name.
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
  - The agent container's memory limit (P12.2, planned) and Docker's restart (Plan.md §6 row 10) bound it, which briefly disconnects every bot on that agent.
  - If this turns out to happen, a process-per-bot connector is the fallback (ADR-0004), as for hangs.
- **azalea's resolver may fall back to Google's DNS.** azalea looks server names up with hickory and the system's resolver configuration. When the host has none, it queries Google's public DNS instead, which then learns which server names the agent resolves. Server addresses that are IP addresses are never looked up. The connect timeout bounds every lookup, since azalea's resolver has none (ADR-0011).
- **azalea 0.16.0 dependencies with open advisories.** `hickory-proto` 0.25.2 (RUSTSEC-2026-0118/0119) and `rsa` (RUSTSEC-2023-0071). The user accepted them with recorded reasons: the hickory code is either not compiled or not reachable, and azalea never decrypts with RSA. `deny.toml` ignores exactly these IDs, and every azalea bump re-checks them (ADR-0011, ADR-0012).
