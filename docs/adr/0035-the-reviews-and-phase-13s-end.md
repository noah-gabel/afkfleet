# 0035. The reviews: the threat model, the dependencies, the history and ASVS Level 2

- **Status:** Accepted
- **Date:** 2026-11-13
- **Related:** Plan.md Phase 13 (P13.7, P13.8 and its definition of done); [the threat model](../threat-model.md); [the ASVS review](../asvs-review.md); ADR-0012, ADR-0021, ADR-0024 (the ignored advisories); ADR-0016 (recovery codes, the TOTP window, the session's lifetimes); ADR-0020 (U97, U125, S131: the screenshot baselines); ADR-0022 (R23, S51); ADR-0025; ADR-0028 (a session is enough)

## Context
Phase 13's last group, G, is its two reviews and the phase's end: **every finding is either fixed or explicitly accepted in an ADR.** This is that ADR.

- **P13.7:** the threat model as a whole; the dependencies (`cargo deny`, `cargo tree -d`, `pnpm audit`); a secret scan of the repository's history.
- **P13.8:** a self-review against the parts of OWASP ASVS Level 2 that apply.
- **Brought along:** ADR-0028's edge (a stolen Owner's or Admin's session can invite an account of the attacker's own), ADR-0022's R23 (a vault key that stays configured after a rotation) and S51's four accepted risks.
- **Added by the user:** the five screenshot rows that failed with the fake's mode list held back (PR #119's "Not changed, for you to decide").

Decided under ADR-0025. It's a self-review: the same hands wrote the code and read it again. Nothing here replaces a test by someone else.

## What was reviewed, and how
| Review | How | Result |
|---|---|---|
| The threat model | Every row's last cell read by a script for a cited test; every accepted risk read in full; the rows read against what changed since they were written (ADR-0028 above all) | Stale in ten places, complete otherwise; ten accepted risks and one row added |
| `cargo deny` | `just deny`: the workspace, the desktop crate, `fuzz/` | Clean. Seventeen advisories ignored, each with its reason; no ignore's removal trigger is met (azalea is still 0.16.0, sqlx 0.9.0, testcontainers 0.28.0) |
| `cargo tree -d` | Read for a second TLS or crypto stack | None: no `ring`, no OpenSSL. The duplicates are older majors that one dependency still asks for (`sha2` 0.10 through sqlx, `rand` 0.8 and 0.9, `thiserror` 1); `deny.toml` reports them as warnings |
| `pnpm audit` | As CI's `ui-audit` runs it | No known vulnerabilities |
| Secrets in the history | gitleaks 8.30.1 (`ghcr.io/gitleaks/gitleaks@sha256:c00b6bd0aeb3071cbcb79009cb16a60dd9e0a7c60e2be9ab65d25e6bc8abbb7f`), on an objects-only copy of every local and remote ref (899 commits), in a container with no network, findings redacted | Eight hits, none a secret: six are RFC 6455's sample `Sec-WebSocket-Key` in tests, two a four-character placeholder between PEM markers in a test |
| Personal data in the history | A script over every added line and every commit message of the same copy: home paths, mail addresses, IPv4 addresses, version-4 UUIDs, hosting and DNS providers' names | Nothing in any file or message: placeholders, documentation ranges and ranges' edges, public documentation's UUIDs, and three cloud names in the metadata-address list. One thing in a commit's own header, reported to the user and not written here |
| ASVS 5.0.0, Levels 1 and 2 | Each of the 253 items against the code and the records, with a check in the code wherever the answer wasn't certain | 153 met, 7 met by this group's changes, 15 accepted, 78 not applicable ([the review](../asvs-review.md)) |

**Why ASVS 5.0.0.** It's the current release (May 2025). ADR-0028 quoted 4.0.3's 2.1.6 for a password change that takes the current password; in 5.0.0 that's 6.2.3, and it's met.

## What was fixed
Each with its test first, where it's code.

1. **A refused request's log line didn't say who was refused** (ASVS 16.3.2). The request's line had its route, status, code and request ID. `AuthUser` now records the user's ID in the request's span the moment the access token is accepted, before the user's rate limit and the handler's own check, so a 429, a 403 and a 404 each name the user. A request without a live session has no such field. Test: `auth_user::a_signed_in_users_request_is_logged_with_their_id_whatever_its_answer`.
2. **Neither CSP named `base-uri`, and the app's allowed objects** (ASVS 3.4.3, 3.4.6). `default-src` doesn't cover `base-uri` or `frame-ancestors`.
   - The API's: `default-src 'none'; base-uri 'none'; frame-ancestors 'none'`.
   - The app's: P8.3's, with `object-src 'none'; base-uri 'none'; frame-ancestors 'none'` added. `just tauri-check` pins the whole text, and every e2e spec runs under it.
   - Both are stricter than before; nothing is relaxed. The built app wasn't started with the new policy: the e2e suite runs the same pages under it in Edge, WebView2's own engine.
3. **`fuzz/Cargo.lock` still named 0.1.2 after the bump to 0.1.3, and nothing noticed.** The fuzz crate has its own lockfile; `just deny`'s third pass rewrote it in place and checked the rewritten one. Every pass is `--locked` now: a lockfile that isn't up to date fails the `deny` job, for either workspace. The lockfile is brought up to date, the release's bump step names it, and `scripts/justfile.test.mjs` pins the three passes.
4. **The threat model was stale in ten places:** its status still said "skeleton"; two cells still said "(planned)" for work done since (the backups, the load test); two sentences still described a step-up ADR-0028 removed (disabling an agent, sharing an account); three rows cited phases and no test; one sentence promised sections that exist; one pointed at a screen "to come".
5. **Four things ASVS asks to be written down weren't:** a cryptographic inventory (11.1.2), a log inventory (16.1.1), every way to authenticate in one place (6.1.3), and what happens to a dependency with an advisory (15.1.1). The first three are in the review's page; the fourth is a rule in `.claude/rules/dependencies.md`: it's handled before anything else merges, in a pull request of its own, by a fix or by an ignore with its reason, its removal trigger and an ADR.
6. **The screenshot rows raced their own reads** (the user's addition). With the fake's mode list held back 1.5 s, five rows were shot before the list landed; once those waited, three more showed, and PR #119's `calls` helper turned out to count the end of a read sent before it started. The cure is general: the fixtures keep the commands a page sent that have no answer yet, but those the fake holds back on purpose and the live events watch, and none of a document that's gone; every shot and the sweep's sign-in wait until there is none; `idle(page)` replaces `calls`. With every command held back 300 ms, and with the mode list held back 1.5 s, all 28 screenshot tests pass. No baseline changed.

## What is accepted
The threat model's accepted risks hold each in full. In short:

| What | Why it stays |
|---|---|
| **A stolen Owner's or Admin's session can invite and register an account of the attacker's own** (ADR-0028's edge) | The user's decision: once logged in, nothing asks for the password again but the two changes that replace a login. The invite and the registration are in the audit log and on the Users and Invites screens, and disabling the account ends it. ASVS 7.5.2 is accepted with it: ending a session asks for nothing |
| **No way back from a forgotten password** (ASVS 6.4.3) | Nothing resets one, so nothing can be attacked; but an Owner who forgets theirs can't be replaced. See "Proposed" below |
| **A recovery code holds 100 bits under a plain hash** (6.5.2, 11.5.1) | The user's rule (ADR-0016). ASVS draws its line at 112 |
| **A TOTP code counts for 90 s** (6.5.5) | Its step and both neighbours, for clocks that differ (ADR-0016) |
| **The password again, not a second code, before a new authenticator** (7.5.1) | The session was opened with two factors (ADR-0016, ADR-0028) |
| **No breach list** (6.2.12) | zxcvbn's lists, a score of 3 and 12 characters; a breach list needs a download or a call out with every new password |
| **Port 80 redirects** (4.1.2) | Caddy's default, beside the certificate's challenge. The app sends plain HTTP only to a loopback address. Turning the redirect off would touch the one path that can't be tested here, a real certificate's issue |
| **HSTS without `includeSubDomains`** (3.4.1) | It would bind every name below the app's for a year in any browser that once opened it; an operator's other names (a server's web map over plain HTTP) would break, and no browser is a client |
| **No `Origin` check on the live events socket** (4.4.2) | A socket needs a ticket and the bearer header, which no browser page can send |
| **Plain HTTP inside the compose project** (12.3.1, 12.3.3) | Caddy to the server, a scraper to the metrics ports: one host's own networks |
| **The Minecraft protocol's own ciphers** (11.3.1) | Mojang's, in azalea |
| **Key files, no key management service** (13.3.1) | One host, no paid services; the keys have a volume of their own and never reach a backup |
| **Logs stay on the host, and root can change them** (16.4.2, 16.4.3) | Shipping them is the operator's choice; the audit log travels with the backups |
| **A vault key stays configured after a rotation** (ADR-0022, R23) | It opens nothing the new key doesn't, and a backup from before the rotation needs it anyway |
| **S51's four** (the dynamic-DNS token, a bundle checked for integrity and not authenticity, the `files` service beside the database, `keys-restore` on Docker's `cp -a`) | Read again: each still says what's true, and nothing since has changed its weight |
| **Seventeen ignored advisories** | Each reason holds; no removal trigger is met |

## Proposed, not built
For the user to decide; none is part of this group.

- **A way back for the Owner.** A command on the server's host, `afkfleet-server user reset-password <username>`: the new password typed at the terminal as `create-owner` takes it, every session of the user ended, the authenticator untouched, audited with `via` = `cli` and no actor. Whoever can run it holds the keys anyway, as with the enrollment tokens. A second one for a lost authenticator and lost recovery codes would put the user back to "2FA not set up". It replaces a login credential, so it needs the user's word, like a new step-up route.
- **GitHub's own secret scanning and push protection** on the repository. They're settings, so they're the user's; the scan here was one run.
- **Starting the built app once under the new CSP** before the next release: `just app-build-check`, then `target/app-check/afkfleet-check.exe`.

## Consequences
- **Phase 13 is done:** every task is ticked, and every finding of its seven groups is fixed or accepted in ADR-0027 to this one.
- **A request's log line names its user.** Whoever reads the server's log can tie a user's ID to the routes they called. The log held no user before; the audit log always did.
- **A stale lockfile fails `just deny`.** A change to a `Cargo.toml` without its lockfile fails there, and the fix is to commit the lockfile.
- **The app's CSP is longer by three directives,** and `scripts/tauri-hardening.mjs` pins the new text; relaxing it still needs an ADR.
- **The threat model has a status worth keeping:** a change to a boundary changes its row in the same pull request.
- **The ASVS review is a snapshot** of v0.1.3. It isn't checked by a test; the next review reads it again.
- **Every shot waits for the page's commands.** A row that keeps a command open on purpose uses `hold`, which the wait knows; a new command that stays open by design, as the live events watch does, joins `FakeBackend.keepsOpen`.

## Alternatives considered
- **ASVS 4.0.3,** the version ADR-0028 quoted. It was superseded in 2025; reviewing against it would be reviewing against yesterday's list.
- **Building the password reset here.** A new way to replace a credential is a design of its own, with the user's word first; a review that ends by adding one would be reviewing its own addition.
- **`includeSubDomains`, and Caddy's redirects off,** to tick two more items. Both change what an operator's host does for no client afkfleet has, and the second can't be tested without a real certificate.
- **A secret scan in CI.** GitHub's own scanning covers pushes at no cost and needs no pinned image; a second scanner in the workflow would be one more thing to keep current.
- **Waiting in each of the five rows,** as PR #119 did for one. Three more rows showed the moment those five waited; the wait belongs where every shot is taken.
