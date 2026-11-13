# ASVS Level 2 self-review

> **Reviewed on 2026-11-13** (Plan.md P13.8, [ADR-0035](adr/0035-the-reviews-and-phase-13s-end.md)), at v0.1.3, against the [OWASP Application Security Verification Standard](https://github.com/OWASP/ASVS) **5.0.0**: every Level 1 and Level 2 item, 253 in all.
>
> It's a self-review: the same hands wrote the code and this page. Nobody outside has tested the system.

## How to read it
- **The items** are named by their ASVS number, with a few words of this page's own for what each asks. The standard's own text is in ASVS 5.0.0.
- **Met:** the system does what the item asks. The last column says where: a rule, a test or a row of the [threat model](threat-model.md), whose "Verified by" cells name the tests.
- **Met, in this review:** it didn't before, and group G's pull request changed that.
- **Accepted:** it doesn't, on purpose. The reason is here, and in the threat model's accepted risks or the ADR named.
- **N/A:** the system has nothing the item is about.

| | Items | Met | Met, in this review | Accepted | N/A |
|---|---|---|---|---|---|
| V1 Encoding and sanitization | 27 | 16 | | | 11 |
| V2 Validation and business logic | 11 | 11 | | | |
| V3 Web frontend | 19 | 11 | 2 | 1 | 5 |
| V4 API and web service | 10 | 6 | | 2 | 2 |
| V5 File handling | 9 | 2 | | | 7 |
| V6 Authentication | 35 | 24 | | 4 | 7 |
| V7 Session management | 18 | 13 | | 2 | 3 |
| V8 Authorization | 7 | 6 | | | 1 |
| V9 Self-contained tokens | 7 | | | | 7 |
| V10 OAuth and OIDC | 29 | 2 | | | 27 |
| V11 Cryptography | 14 | 11 | 1 | 1 | 1 |
| V12 Secure communication | 9 | 7 | | 2 | |
| V13 Configuration | 13 | 12 | | 1 | |
| V14 Data protection | 9 | 9 | | | |
| V15 Secure coding and architecture | 13 | 12 | 1 | | |
| V16 Security logging and error handling | 16 | 11 | 3 | 2 | |
| V17 WebRTC | 7 | | | | 7 |
| **All** | **253** | **153** | **7** | **15** | **78** |

**The fifteen accepted items,** in one list: 3.4.1 (HSTS without subdomains), 4.1.2 (port 80 redirects), 4.4.2 (no `Origin` check on the socket), 6.2.12 (no breach list), 6.4.3 (no password reset), 6.5.2 (recovery codes of 100 bits under a plain hash), 6.5.5 (a TOTP code's neighbouring steps), 7.5.1 and 7.5.2 (a password, or nothing, where ASVS asks to authenticate again), 11.3.1 (the Minecraft protocol's own ciphers), 12.3.1 and 12.3.3 (plain HTTP inside the compose project), 13.3.1 (key files, no key management service), 16.4.2 and 16.4.3 (logs stay on the host).

## V1 Encoding and sanitization
| Item | Level | Verdict | Where, or why |
|---|---|---|---|
| 1.1.1 Decode once, before validating | 2 | Met | axum's extractors and serde decode a request once into typed values; validation (garde, fleet-core's constructors) runs on the decoded value. Text from Minecraft servers is decoded by azalea, then cleaned once in fleet-core (`text.rs`) |
| 1.1.2 Encode at the last step | 2 | Met | SQL through bind parameters, JSON through serde, the screen through React's text nodes, log lines through the JSON layer. Nothing is stored encoded |
| 1.2.1 Output encoding fits its context | 1 | Met | The API answers JSON alone (the metrics ports plain text); the app builds no HTML from data (threat model, B2) |
| 1.2.2 URLs built with encoding, safe schemes only | 1 | Met | fleet-client builds every URL with typed parts; the one link the app opens outside itself is checked on both sides: https to Microsoft's two hosts (threat model, B5, "A hostile server making the app open another page") |
| 1.2.3 Script and JSON built with encoding | 1 | Met | serde_json writes every JSON value; no script is ever built |
| 1.2.4 Parameterized queries | 1 | Met | Only `sqlx::query!` and `query_as!` with bind parameters (CLAUDE.md's security rule 8; ADR-0015) |
| 1.2.5 No OS command injection | 1 | Met | The server and the agent start no process. The app hands Windows one URL to open, 1.2.2's checked link, and nothing else |
| 1.2.6 to 1.2.8 LDAP, XPath, LaTeX | 2 | N/A | None is used |
| 1.2.9 Regular expressions' metacharacters | 2 | Met | No member uses a regular expression engine at runtime. A chat format is read by fleet-core's own matcher, whose text is literal but for `{sender}` and `{any}` (threat model, B1, "A chat format that makes reading a hostile server's line expensive") |
| 1.3.1 HTML from rich-text editors | 1 | N/A | No HTML is taken in |
| 1.3.2 No `eval` | 1 | Met | The release CSP has no `unsafe-eval`, zod runs jitless, and Biome refuses `eval` and `new Function` (`.claude/rules/frontend.md`) |
| 1.3.3 Sanitize before a dangerous context | 2 | Met | Chat, kick texts and names from a Minecraft server lose control characters, format codes, bidi controls and invisible characters and are capped, in fleet-core (threat model, B4) |
| 1.3.4 User-supplied SVG | 2 | N/A | The one SVG is the server's own QR code, shown as an `<img>` |
| 1.3.5 Markdown, CSS and other template content | 2 | N/A | None is rendered |
| 1.3.6 SSRF | 2 | Met | The server's Microsoft adapter calls fixed hosts and follows no redirect; a managed agent refuses loopback, private, link-local and metadata addresses right before it dials, but for the networks its operator listed (threat model, B4, "A bot pointed at an internal address"; its limits are accepted risks there) |
| 1.3.7 Template injection | 2 | N/A | No template engine |
| 1.3.8, 1.3.9 JNDI, memcache | 2 | N/A | Neither is used |
| 1.3.10 Format strings | 2 | Met | Rust's format strings are literals checked at compile time |
| 1.3.11 Mail injection | 2 | N/A | afkfleet sends no mail |
| 1.4.1 Memory-safe strings and copies | 2 | Met | `unsafe_code = "forbid"` in every member; indexing and slicing are lint errors outside tests |
| 1.4.2 Integer overflow | 2 | Met | Truncating `as` casts are lint errors (`TryFrom` instead), and every count that comes from outside is checked against a limit before it's used |
| 1.4.3 Freed memory | 2 | Met | Rust's ownership; no `unsafe` |
| 1.5.1 XML parsers | 1 | N/A | No XML is parsed |
| 1.5.2 Safe deserialization | 2 | Met | serde into closed types with `deny_unknown_fields`, never into a type the input names; prost messages with size limits, read again into validated types (`fleet_proto::control`). Ten fuzz targets cover the parsers (ADR-0030) |

## V2 Validation and business logic
| Item | Level | Verdict | Where, or why |
|---|---|---|---|
| 2.1.1 Validation rules are documented | 1 | Met | Every DTO's rules are in `fleet-api-types` (garde) and in the OpenAPI document made from it; `docs/development.md` describes each route's input |
| 2.1.2 Rules for combined values are documented | 2 | Met | The same reference: a mode's steps and their intervals by action, the session lifetimes (idle at most absolute), the heartbeat's two values |
| 2.1.3 Business limits are documented | 2 | Met | `docs/development.md`: the rate-limit classes, and the caps (50 modes an owner, 20 pending invites, 100 servers' settings, 3 sockets a user) |
| 2.2.1 Input is validated against what's expected | 1 | Met | CLAUDE.md's security rule 2: garde at the boundary, fleet-core's constructors for domain values |
| 2.2.2 Validated on the server | 1 | Met | The server never trusts the app; the app's zod schemas only mirror its limits |
| 2.2.3 Combined values are checked | 2 | Met | `ModeDefinition`'s checks, the config's cross-field checks |
| 2.3.1 Steps run in order | 1 | Met | Each step of a login, a 2FA setup and a link has its own single-use token kind, and each route takes one kind (threat model, B1, "A token of one kind used as another") |
| 2.3.2 Business limits are enforced | 2 | Met | Counted inside the write's transaction (threat model, B1, "Growing tables") |
| 2.3.3 A business operation is one transaction | 2 | Met | Every write is one `WriteTx` that commits with its audit entry (`.claude/rules/server.md`) |
| 2.3.4 Limited resources can't be taken twice | 2 | Met | One write connection, and conditional writes for an invite, an enrollment token, a TOTP step, a recovery code and an agent's one slot |
| 2.4.1 Anti-automation | 2 | Met | A per-IP limit on every request, per-class limits, a per-user limit, the lockout (threat model, B1, the denial-of-service rows) |

## V3 Web frontend
The API is called by the desktop app's Rust side, never by a browser, and the app's own pages come from its installer. Both carry browser headers all the same.

| Item | Level | Verdict | Where, or why |
|---|---|---|---|
| 3.2.1 Content can't be rendered in the wrong context | 1 | Met | Every API response is JSON with `nosniff` and a CSP of `default-src 'none'`; the API serves nothing a user supplied as a document |
| 3.2.2 Text is rendered as text | 1 | Met | React text nodes; `dangerouslySetInnerHTML` is refused by Biome; chat is plain text |
| 3.3.1 to 3.3.4 Cookies | 1, 2 | N/A | afkfleet sets and reads no cookie |
| 3.4.1 HSTS | 1 | Accepted | afkfleet's Caddy sends `max-age=31536000`, which is all Level 1 asks. Level 2 also asks for `includeSubDomains`: left out on purpose (threat model, accepted risks, "HSTS doesn't name subdomains") |
| 3.4.2 CORS | 1 | Met | There is no CORS layer and no `Access-Control-Allow-Origin` |
| 3.4.3 A CSP with `object-src 'none'` and `base-uri 'none'` | 2 | Met, in this review | The API's is `default-src 'none'; base-uri 'none'; frame-ancestors 'none'`; the app's is an allowlist of `'self'` with `object-src 'none'`, `base-uri 'none'` and `frame-ancestors 'none'`. Before, neither named `base-uri`, and the app's allowed objects from `'self'` |
| 3.4.4 `nosniff` | 2 | Met | On every response, errors and panics included |
| 3.4.5 Referrer policy | 2 | Met | `Referrer-Policy: no-referrer` on every response; the app never navigates away |
| 3.4.6 `frame-ancestors` | 2 | Met, in this review | The API's CSP had it; the app's has it now |
| 3.5.1, 3.5.2 Cross-site request forgery | 1 | Met | A request counts only with an `Authorization: Bearer` header, which no cross-site page can add without a preflight, and no preflight is answered |
| 3.5.3 Safe methods change nothing | 1 | Met | Every write is a `POST`, `PUT`, `PATCH` or `DELETE`; the route registry names each route's method |
| 3.5.4 Separate applications on separate hostnames | 2 | Met | The API has its name to itself: Caddy answers 404 for everything but the app's routes |
| 3.5.5 `postMessage` | 2 | N/A | The app uses none; its one channel is Tauri's IPC, limited to its own commands by capabilities |
| 3.7.1 Supported client technologies only | 2 | Met | WebView2, React, no plugin content (the CSP refuses it) |
| 3.7.2 No redirect to another host unless allowlisted | 2 | Met | The app follows no link by itself; see 1.2.2 |

## V4 API and web service
| Item | Level | Verdict | Where, or why |
|---|---|---|---|
| 4.1.1 Every body has its `Content-Type` | 1 | Met | `application/json` for the API, `text/plain; version=0.0.4; charset=utf-8` for the metrics |
| 4.1.2 An API doesn't redirect HTTP to HTTPS | 2 | Accepted | Caddy redirects port 80, its default (threat model, accepted risks, "Port 80 redirects to HTTPS"). The app can't send plain HTTP to it: fleet-client takes `http` only for a loopback address |
| 4.1.3 Proxy headers can't be set by a client | 2 | Met | `X-Forwarded-For` counts only from a trusted proxy's address, read from the right (threat model, B1, "A client choosing its own address") |
| 4.2.1 Message boundaries, against request smuggling | 2 | Met | hyper and Caddy, HTTP/1.1 and HTTP/2 only. The one reader of our own, the agent's metrics responder, reads a request head of at most 2 KiB and no body, and ends every connection within 5 s; a fuzz target covers it |
| 4.3.1, 4.3.2 GraphQL | 2 | N/A | None |
| 4.4.1 WebSocket over TLS | 1 | Met | The live events socket is an upgrade on the API's own TLS connection |
| 4.4.2 The handshake's `Origin` is checked | 2 | Accepted | It isn't (threat model, accepted risks, "The live events socket checks no `Origin`"): the socket needs a ticket and the bearer header, which no browser page can send |
| 4.4.3 Dedicated tokens where the session's can't be used | 2 | Met | A ticket: 256 random bits, used once, within 30 s, only with the bearer token of the session it was given to (ADR-0020, S28, S29) |
| 4.4.4 Such a token comes from the authenticated session | 2 | Met | `POST /api/v1/events/ticket` is an authed route |

## V5 File handling
afkfleet takes no file from a user.

| Item | Level | Verdict | Where, or why |
|---|---|---|---|
| 5.1.1 Upload rules are documented | 2 | N/A | No upload |
| 5.2.1 Only sizes it can process | 1 | Met | Every request body is cut at `[http] max_body_bytes` (64 KiB by default); the app's update download at 64 MiB; the host's bundle is bounded by `setup.sh` |
| 5.2.2, 5.2.3 File types, archives | 1, 2 | N/A | No upload. The one archive, a release's bundle, is the operator's own download, checked entry by entry (threat model, "An update that installs other files or images on the host") |
| 5.3.1 Uploaded files aren't executed | 1 | N/A | No upload |
| 5.3.2 File paths come from trusted data | 1 | Met | A backup's name is the server's own; every other path is the operator's config. No path is built from a request |
| 5.4.1 to 5.4.3 Downloads | 2 | N/A | The API serves no file |

## V6 Authentication
| Item | Level | Verdict | Where, or why |
|---|---|---|---|
| 6.1.1 The defenses against guessing are documented | 1 | Met | `docs/development.md` (the lockout, the login limits), the threat model's B1, and its accepted risk "Keeping an account locked" |
| 6.1.2 A list of context words is documented | 2 | Met | The strength check counts the username, `afkfleet` and `minecraft` as guessable (ADR-0016) |
| 6.1.3 Every way to authenticate is documented | 2 | Met | [The ways in](#the-ways-in) below |
| 6.2.1 At least 8 characters | 1 | Met | 12 |
| 6.2.2 Users can change their password | 1 | Met | `POST /api/v1/me/password` |
| 6.2.3 A change takes the current password | 1 | Met | It's one of the two step-up routes: the current password within the last 5 minutes (ADR-0028) |
| 6.2.4 Checked against common passwords | 1 | Met | zxcvbn's lists, about 30,000, and a score of at least 3 |
| 6.2.5 Any composition | 1 | Met | No composition rule; spaces and any Unicode count, only control characters (a line break, a tab) are refused |
| 6.2.6 Masked fields | 1 | Met | `type="password"` |
| 6.2.7 Paste and password managers work | 1 | Met | Nothing blocks them; the code boxes take a pasted code too |
| 6.2.8 Verified as typed | 1 | Met | Nothing is trimmed or normalized |
| 6.2.9 At least 64 characters allowed | 2 | Met | 128 |
| 6.2.10 No forced rotation | 2 | Met | A password never expires |
| 6.2.11 The context words are used | 2 | Met | See 6.1.2 |
| 6.2.12 Checked against breached passwords | 2 | Accepted | Only zxcvbn's lists (threat model, accepted risks, "New passwords aren't checked against a breach list") |
| 6.3.1 The documented defenses are in place | 1 | Met | Threat model, B1, the spoofing rows |
| 6.3.2 No default accounts | 1 | Met | None exists; the Owner is made on the host with a password typed there |
| 6.3.3 More than one factor | 2 | Met | TOTP is mandatory for every user; a login without it can only enroll it |
| 6.3.4 No undocumented way in, and one strength | 2 | Met | The route registry: no route exists outside it, and every route names its access (threat model, B1, "A route without authentication or step-up") |
| 6.4.1 Initial secrets are random, short-lived and never the password | 1 | Met | An invite's code: 256 random bits, used once, 72 hours by default; the invitee chooses the password |
| 6.4.2 No hints or secret questions | 1 | Met | None |
| 6.4.3 A secure reset of a forgotten password | 2 | Accepted | There is none, safe or otherwise (threat model, accepted risks, "No way back from a forgotten password"; ADR-0035 proposes a command on the host) |
| 6.4.4 A lost factor needs proof as strong as enrollment | 2 | Met | Ten recovery codes, each used once in place of a code, then a new authenticator behind the password. Losing those too is 6.4.3's case |
| 6.5.1 Codes work once | 2 | Met | A TOTP step and every step before it, and a recovery code (threat model, B1, "Replaying a TOTP code or a recovery code") |
| 6.5.2 Lookup secrets under 112 bits are hashed like passwords | 2 | Accepted | A recovery code has 100 bits and is stored as its SHA-256, the user's rule (ADR-0016; threat model, accepted risks) |
| 6.5.3 Made by a secure random source | 2 | Met | `SecureRandom`, which is `getrandom` |
| 6.5.4 At least 20 bits | 2 | Met | 100 |
| 6.5.5 A TOTP code lives 30 s | 2 | Accepted | Its step and both neighbours count, so up to 90 s (ADR-0016; threat model, accepted risks) |
| 6.6.1 to 6.6.3 Out-of-band codes, SMS | 2 | N/A | None |
| 6.8.1 to 6.8.4 An identity provider | 2 | N/A | afkfleet's users log in to afkfleet alone. Microsoft's sign-in links a Minecraft account; it never logs anyone in (V10) |

### The ways in
| Who | How | Strength |
|---|---|---|
| A user, through the app | Password, then a TOTP code or a recovery code | Two factors, always |
| A user's open app | The session's refresh token, in the Windows Credential Manager | What the Windows account protects (threat model, B7) |
| An agent | Its client certificate (mTLS), enrolled once with a single-use token | A key that never left its host |
| The host's operator | `afkfleet-server`'s commands, run against the database | Whoever can run them holds the keys anyway (threat model, accepted risks, "The server's host operator creates enrollment tokens without 2FA") |
| A scraper | The metrics ports, no login | Internal networks only (threat model, B8) |

## V7 Session management
| Item | Level | Verdict | Where, or why |
|---|---|---|---|
| 7.1.1 Timeouts are documented, with reasons for what differs from NIST | 2 | Met | `docs/development.md`, `[auth]`: an access token 15 minutes, a session 7 days idle and 30 days at most. Both are longer than NIST SP 800-63B's for this strength; the reasons and what an open app adds are in ADR-0016, ADR-0028 and the threat model's accepted risks ("An open app keeps its session alive") |
| 7.1.2 Parallel sessions are documented | 2 | Met | A user may hold any number of sessions, one per login, each listed in the profile and ended there; at most 3 live events sockets a user |
| 7.1.3 Federated sessions | 2 | N/A | None |
| 7.2.1 Tokens are checked on the server | 1 | Met | Every request reads its session and its user (threat model, B1, "A session outliving its user's role change or disabling") |
| 7.2.2 Tokens are made per session | 1 | Met | Opaque tokens, stored as SHA-256 (ADR-0006) |
| 7.2.3 128 bits from a secure random source | 1 | Met | 256 |
| 7.2.4 A new token at every authentication | 1 | Met | Every login starts a new session with new tokens, and the access token is replaced at every refresh. Entering the password again for a step-up stamps the session it's in: there's no earlier, unauthenticated token it could fix |
| 7.3.1 An inactivity timeout | 2 | Met | 7 days |
| 7.3.2 An absolute lifetime | 2 | Met | 30 days |
| 7.4.1 A terminated session is refused | 1 | Met | A logout or a revocation ends the session in the database, and the very next request is refused; its live events socket closes within 250 ms of the commit |
| 7.4.2 Disabling a user ends their sessions | 1 | Met | And a role change does too |
| 7.4.3 The other sessions can be ended after a factor changes | 2 | Met | They always are: a new password or a new authenticator ends every other session |
| 7.4.4 Logout is easy to find | 2 | Met | In the frame's Account box, on every screen |
| 7.4.5 Administrators can end a user's sessions | 2 | Met | By disabling the user (Admins for Members, the Owner for anyone), which ends all of them; enabling again restores the login, not the sessions |
| 7.5.1 Full authentication again before a factor changes | 2 | Accepted | The password again, within 5 minutes, inside a session that two factors opened; not a second code (ADR-0016, ADR-0028) |
| 7.5.2 Users see their sessions, and end them after authenticating again | 2 | Accepted | They see and end them; ending one asks for nothing since v0.1.2 (the user's decision, ADR-0028) |
| 7.6.1, 7.6.2 Federated re-authentication | 2 | N/A | None |

## V8 Authorization
| Item | Level | Verdict | Where, or why |
|---|---|---|---|
| 8.1.1 The rules are documented | 1 | Met | Plan.md §7.3, and the authorization matrix's snapshot: every route for every kind of caller (ADR-0029) |
| 8.1.2 Field-level rules are documented | 2 | Met | An account's field rule, a bot's server address and agent, a command's arguments, a mode's owner (ADR-0018, S11; ADR-0019, S54; ADR-0020, S9) |
| 8.2.1 Function-level access | 1 | Met | One pure `authorize()`, deny by default, behind every authed route |
| 8.2.2 Data-level access | 1 | Met | Per account, by grants; what a caller can't see answers 404 like something unknown |
| 8.2.3 Field-level access | 2 | Met | Each answer is built for its viewer, and each live frame through the same helpers (threat model, B1) |
| 8.3.1 Enforced on the server | 1 | Met | The app's own hiding of buttons is a convenience (threat model, B2, "The UI offering an action the user's role doesn't allow") |
| 8.4.1 Tenants are kept apart | 2 | N/A | One fleet per server |

## V9 Self-contained tokens
All seven items are N/A: every token is an opaque reference the server looks up (ADR-0006). No JWT is made or read.

## V10 OAuth and OIDC
afkfleet is an OAuth client of one authorization server, Microsoft's, for one purpose: the device-code flow that links a Minecraft account. It is no authorization server, no resource server, and no OpenID relying party.

| Item | Level | Verdict | Where, or why |
|---|---|---|---|
| 10.1.1 Tokens reach only what needs them | 2 | Met | Microsoft's tokens never leave the server, sealed by the vault; an agent gets a short-lived Minecraft session token, and only for a bot assigned to it (CLAUDE.md's security rule 6; threat model, B3) |
| 10.1.2 An answer is taken only for the flow that asked | 2 | Met | The device code stays on the server and belongs to the flow of the user who started it; the profile it ends with is compared at a refresh and a link again (threat model, B5) |
| 10.2.1, 10.2.2 The code flow, more than one authorization server | 2 | N/A | Device-code flow, one server |
| 10.3.1 to 10.3.4 A resource server | 2 | N/A | |
| 10.4.1 to 10.4.11 An authorization server | 1, 2 | N/A | afkfleet's own sessions are V7's |
| 10.5.1 to 10.5.5 An OIDC client | 2 | N/A | No ID token is used |
| 10.6.1, 10.6.2, 10.7.1 to 10.7.3 An OpenID provider, consent | 2 | N/A | |

## V11 Cryptography
| Item | Level | Verdict | Where, or why |
|---|---|---|---|
| 11.1.1 A key management policy | 2 | Met | The runbook: the keys' copy, restoring them, rotating the vault key, renewing the server certificate, agents' certificates; `docs/releasing.md` for the updater's key |
| 11.1.2 A cryptographic inventory | 2 | Met, in this review | [The inventory](#the-cryptographic-inventory) below |
| 11.2.1 Validated implementations | 2 | Met | rustls with aws-lc-rs, the RustCrypto crates (`argon2`, `chacha20poly1305`, `sha2`, `hmac`), `rcgen`, minisign in Tauri's updater. None is our own |
| 11.2.2 Algorithms and keys can be replaced | 2 | Met | The vault's envelope carries a version and a key ID, and `vault rotate-key` seals everything again; a password is hashed again at login after an `argon2_*` change; a token's prefix names its kind; certificates renew |
| 11.2.3 At least 128 bits of security | 2 | Met | Ed25519, 256-bit keys and tokens, SHA-256. The one secret below it is 6.5.2's recovery code |
| 11.3.1 No insecure modes or paddings | 1 | Accepted | None in afkfleet's own cryptography. The Minecraft protocol's login uses RSA with PKCS#1 v1.5 and AES in CFB8, in azalea, as every Minecraft client must (threat model, B4 and its accepted risks) |
| 11.3.2 Approved ciphers | 1 | Met | XChaCha20-Poly1305 for the vault; TLS's suites |
| 11.3.3 Encrypted data is authenticated | 2 | Met | An AEAD, bound to its record by the associated data |
| 11.4.1 Approved hash functions | 1 | Met | SHA-256 and HMAC-SHA-256. SHA-1 appears only where a standard fixes it: inside TOTP's HMAC, which authenticator apps expect, and in Minecraft's login |
| 11.4.2 Passwords under a password hashing function | 2 | Met | Argon2id, 19 MiB, 2 passes, 1 lane at least; the config can't go below |
| 11.4.3 Collision-resistant hashes for integrity | 2 | Met | SHA-256 |
| 11.4.4 Keys derived from passwords are stretched | 2 | N/A | No key comes from a password. The lockout's key is an HMAC of a label under the vault key |
| 11.5.1 Secrets come from a secure random source, 128 bits | 2 | Met | Every token has 256 bits from `getrandom`, a TOTP secret 160. A recovery code has 100 (6.5.2) |
| 11.6.1 Approved key generation and signatures | 2 | Met | Ed25519 keys from the server's secure random source |

### The cryptographic inventory
| What | Algorithm | Key or secret | Where it lives | Replaced by |
|---|---|---|---|---|
| Users' passwords | Argon2id (19456 KiB, 2, 1 by default), a random salt each | None | The database, as PHC strings | The next login after `[auth] argon2_*` changes |
| Access, refresh, mfa and setup tokens, invite codes, enrollment tokens, live events tickets | 256 random bits; stored as SHA-256 | None | The database (tickets in memory); a session's refresh token in the Windows Credential Manager | Their own expiry; a refresh rotates both session tokens |
| Recovery codes | 100 random bits; stored as SHA-256 | None | The database | A new authenticator makes ten new ones |
| TOTP | HMAC-SHA-1, 6 digits, 30 s (RFC 6238) | A 160-bit secret per user | The database, sealed by the vault | A new authenticator (`POST /me/mfa/reset`) |
| The vault: Microsoft refresh tokens, TOTP secrets | XChaCha20-Poly1305, a random nonce each, bound to the record's ID and purpose | 32-byte master keys, by ID | Key files on the `keys` volume, never in the database or a backup | `vault create-key`, `vault rotate-key` (the runbook) |
| The lockout's rows | HMAC-SHA-256 of the username | Derived from the active vault key | The rows in the database, the key in memory | With the vault key |
| The control plane's CA | Ed25519, X.509 | The CA's key, ten years | The `keys` volume; its certificate on `ca` | A new CA enrolls every agent again; never done by a command |
| The server's gRPC certificate | Ed25519, signed by the CA, one year | Its key | The `tls` volume | `pki server-cert` (the runbook, yearly) |
| An agent's certificate | Ed25519, signed by the CA, 60 days | Its key, made on its host | The agent's folder, owner-only | By itself at two thirds of its life (P13.6) |
| The API's TLS | TLS 1.2 and 1.3, Caddy's defaults | Let's Encrypt's certificate | Caddy's volume | Caddy, by itself |
| Every TLS client (the app, the agent, the server's calls to Microsoft) | rustls with aws-lc-rs, certificates always checked | The system's roots; the pinned CA for agents | | |
| The app's updates | minisign (Ed25519) over the installer, bound to its version | The maintainer's key, offline | The public key in the repository | A new key strands installed apps once (threat model, accepted risks) |
| A release's files | SHA-256 (`SHA256SUMS`, the bundle's `RELEASE`) | None | The release page | Integrity, not authenticity (threat model, accepted risks) |
| Minecraft's login and chat signing | Mojang's protocol, in azalea | A session's keys | The agent's memory | Each session |

## V12 Secure communication
| Item | Level | Verdict | Where, or why |
|---|---|---|---|
| 12.1.1 Current TLS versions only | 1 | Met | rustls and Caddy speak TLS 1.2 and 1.3 and nothing older |
| 12.1.2 Recommended cipher suites | 2 | Met | Their defaults, all with forward secrecy; one rustls provider (aws-lc-rs) |
| 12.1.3 A client certificate is trusted before it's used | 2 | Met | The CA's signature, then the fingerprint allowlist, revocation, expiry and the agent's state (threat model, "Rogue or compromised agent") |
| 12.2.1 TLS for every outside connection, no fallback | 1 | Met | The app takes `https` only, but for a loopback address |
| 12.2.2 Publicly trusted certificates | 1 | Met | Let's Encrypt; Caddy's own CA only in tests |
| 12.3.1 Every connection in and out is encrypted | 2 | Accepted | All but two: plain HTTP inside the compose project (Caddy to the server, a scraper to the metrics ports), and the Minecraft protocol itself (threat model, accepted risks) |
| 12.3.2 TLS clients check certificates | 2 | Met | No `danger_*` API anywhere (CLAUDE.md's security rule 9) |
| 12.3.3 TLS between internal services | 2 | Accepted | 12.3.1's first case |
| 12.3.4 Internal certificates are pinned | 2 | Met | An agent trusts its server's CA alone, from its enrollment bundle |

## V13 Configuration
| Item | Level | Verdict | Where, or why |
|---|---|---|---|
| 13.1.1 What the system talks to is documented | 2 | Met | The threat model's boundaries, and the runbook ("What afkfleet adds to a host", "The firewall"): Microsoft, Xbox and Mojang; Let's Encrypt; GitHub for a release; the Minecraft servers users name; DNS |
| 13.2.1 Backend connections are authenticated | 2 | Met | Agents by mTLS. The database is in the server's process. Caddy reaches the server on the project's network, trusted by its address alone (12.3.3) |
| 13.2.2 Least privilege between components | 2 | Met | Every container runs as the nonroot user, read-only, with every capability dropped; the backups' account reads one folder (threat model, "A deployment that exposes or weakens what the binaries protect") |
| 13.2.3 No default credentials | 2 | Met | None exists; `env.example` holds placeholders, which a test checks |
| 13.2.4, 13.2.5 An allowlist of what it may reach | 2 | Met | The server calls fixed hosts. An agent dials what users name, public addresses only unless its operator lists a network (1.3.6). No firewall rule of afkfleet's own limits outbound traffic; the runbook leaves that to the host |
| 13.3.1 A secrets management solution | 2 | Accepted | Key files on a volume of their own, read-only where the server runs, never in a backup, with a root-only copy; and the server's own vault for what it stores. No key management service or hardware module: one host, no paid services |
| 13.3.2 Least privilege for secrets | 2 | Met | Threat model, "The keys copied, restored or read by the volume tools" |
| 13.4.1 No source control metadata is deployed | 1 | Met | The images are built from an allowlisted context; the host's bundle holds `deploy/`'s files alone |
| 13.4.2 Debug modes are off | 2 | Met | `dev_mode` is off unless set, the production config says `false`, and nothing under `deploy/` but the dev folder's own config turns it on; no file there allows offline accounts |
| 13.4.3 No directory listing | 2 | Met | Nothing serves files |
| 13.4.4 No `TRACE` | 2 | Met | 405, like every method a route doesn't take |
| 13.4.5 Documentation and monitoring endpoints aren't exposed | 2 | Met | The OpenAPI document exists only in dev mode; the metrics ports are never published or proxied |

## V14 Data protection
| Item | Level | Verdict | Where, or why |
|---|---|---|---|
| 14.1.1 Sensitive data is classified | 2 | Met | The threat model's assets, ranked |
| 14.1.2 Each class has its requirements | 2 | Met | [What each kind of data gets](#what-each-kind-of-data-gets) below, from the threat model's rows |
| 14.2.1 Nothing sensitive in a URL | 1 | Met | With CLAUDE.md's one exception, the live events ticket (used once, within 30 s, worthless without the session's bearer), which Caddy deletes from every URI it logs, as it does "Share with"'s `q` |
| 14.2.2 Not cached on the way | 2 | Met | `Cache-Control: no-store`; Caddy caches nothing |
| 14.2.3 Not sent to third parties | 2 | Met | No telemetry, no tracker. The app talks to its server, and to GitHub for a release's manifest, which gets nothing but the request |
| 14.2.4 The documented controls are in place | 2 | Met | The table below |
| 14.3.1 Cleared from the client at a session's end | 1 | Met | The app empties its caches and its live record at a logout, at `no_session` and at a new sign-in (threat model, B2) |
| 14.3.2 `no-store` | 2 | Met | On every response |
| 14.3.3 Nothing sensitive in browser storage | 2 | Met | `localStorage` holds the theme's name and nothing else |

### What each kind of data gets
| Data | At rest | In logs | Who reads it | Kept |
|---|---|---|---|---|
| Microsoft refresh tokens, TOTP secrets | Sealed by the vault | Never | The server alone | Until the account or the authenticator goes |
| Passwords | Argon2id | Never | Nobody | Until changed |
| Session tokens, invite and enrollment codes | SHA-256 | Never (a refusal logs its reason) | Their holder, once | Sessions 30 days at most; unused codes with the audit log |
| Minecraft session tokens | Never stored; the agent's memory | Never (azalea's own targets are capped) | The bot's agent | About 24 hours, Mojang's |
| Chat | Plain text | Only at `debug` | A bot's viewers; a command's arguments only Manage holders | 14 days and 50,000 lines a bot by default, then overwritten |
| The audit log, with actors' IP addresses | Plain text | Its own table | Admins and the Owner | 365 days by default |
| Usernames, Minecraft names and UUIDs | Plain text | IDs; a name typed at a login never | By role and grant | With their row |
| Backups | A copy of the database: the sealed stay sealed, the rest is plain | A line per backup | The host; the read-only SFTP account | `backup_keep` × `backup_interval_hours`, 720 hours at most |

## V15 Secure coding and architecture
| Item | Level | Verdict | Where, or why |
|---|---|---|---|
| 15.1.1 Time frames for vulnerable components are documented | 1 | Met, in this review | `.claude/rules/dependencies.md`, "A dependency with an advisory": it's handled before anything else merges. The practice was there (four such pull requests in Phase 13); the sentence wasn't |
| 15.1.2 An inventory of third-party components | 2 | Met | `Cargo.lock`, `fuzz/Cargo.lock` and `pnpm-lock.yaml`, committed and enforced; Plan.md §5's registry says what each is for, and `deny.toml` allows crates.io alone as a source. No SBOM file is published |
| 15.1.3 Costly functions are documented | 2 | Met | Password hashing behind a bounded queue, the retention's batches, backups, a Microsoft refresh (`docs/development.md`; the load test's numbers in ADR-0032) |
| 15.2.1 No component past its time frame | 1 | Met | `just deny` and `pnpm audit` were clean on 2026-11-13. Seventeen advisories are ignored, each with its reason and the release that removes it, none of which exists yet (ADR-0012, ADR-0021, ADR-0024) |
| 15.2.2 Defenses for the costly functions | 2 | Met | `busy` at once when the hashing queue is full, limits, timeouts, bounded queues (threat model, the denial-of-service rows; the load and chaos tests) |
| 15.2.3 No test or development code in production | 2 | Met | The e2e build never ships (a bundle guard checks both shipped bundles); fleet-mc's fault injection is a test-only feature; dev mode is off |
| 15.3.1 Only the fields a caller needs | 1 | Met | Answers are DTOs built per viewer, never a stored row |
| 15.3.2 Backend calls follow no redirect | 2 | Met | The Microsoft adapter follows none. The app's update download follows GitHub's, to https alone |
| 15.3.3 No mass assignment | 2 | Met | `deny_unknown_fields` on every DTO; a write names each column |
| 15.3.4 The client's real address | 2 | Met | 4.1.3 |
| 15.3.5 Strict types and comparisons | 2 | Met | Rust; TypeScript strict, with zod at the app's boundary |
| 15.3.6 No prototype pollution | 2 | Met | `Object.prototype` is frozen in the app (`freezePrototype`) and in every e2e spec |
| 15.3.7 No parameter pollution | 2 | Met | A handler reads each value from one place, through a typed extractor that refuses unknown fields |

## V16 Security logging and error handling
| Item | Level | Verdict | Where, or why |
|---|---|---|---|
| 16.1.1 A log inventory | 2 | Met, in this review | [The logs](#the-logs) below |
| 16.2.1 When, where, who, what | 2 | Met, in this review | A line has its time, level and target, and inside a request the request's ID, method, route and, since this review, the user (16.3.2). An audit entry has its actor, their address, the action, the target and the outcome |
| 16.2.2 Synchronized clocks, UTC | 2 | Met | RFC 3339 in UTC; the runbook has both hosts keep NTP |
| 16.2.3 Logs go only where documented | 2 | Met | stdout, nowhere else |
| 16.2.4 A common format | 2 | Met | JSON lines, no key twice in a line (ADR-0014) |
| 16.2.5 Sensitive data follows its class | 2 | Met | Secrets never; chat only at `debug`; a capture of every level of every target checks it (threat model, "Information leakage") |
| 16.3.1 Every authentication is logged | 2 | Met | The audit log: `auth.login` (success, and failure with its step and reason), `auth.logout`, `auth.step_up`, `mfa.enroll`, `mfa.reset`, `security.lockout`, `security.refresh_token_reuse`; agents' `agent.connect` refusals |
| 16.3.2 Failed authorization is logged | 2 | Met, in this review | A refused request's line now names the user whose session made it; before, it had the route, the status and the code alone (threat model, B1, "Who made a request that was refused") |
| 16.3.3 Security events and attempts to get around controls | 2 | Met | The `security.*` entries; every limiter counts its refusals for the minute's summary; a refused input is its request's line with its code |
| 16.3.4 Unexpected errors and failed controls | 2 | Met | An internal error is an `error` line with the request's ID, a panic included; a certificate the gRPC port refuses is logged and, for a known one, audited |
| 16.4.1 No log injection | 2 | Met | JSON encodes every value, and text from Minecraft servers is cleaned before anything logs it |
| 16.4.2 Logs can't be read or changed by the wrong hands | 2 | Accepted | On the host they're root's (threat model, accepted risks, "Logs stay on the host, and can be changed there") |
| 16.4.3 Logs go to a separate system | 2 | Accepted | The same: afkfleet ships them nowhere |
| 16.5.1 Generic error messages | 2 | Met | An internal error answers a fixed text and a request ID (threat model, "Information leakage") |
| 16.5.2 It keeps working when something outside fails | 2 | Met | Bots reconnect with backoff behind a circuit breaker; Microsoft's 429 pauses refreshes; a busy database is a 503; the chaos tests kill and cut every part (ADR-0033) |
| 16.5.3 It fails closed | 2 | Met | `authorize()` denies by default; an error in a check is a refusal, never a pass |

### The logs
| Log | What's in it | Where | Who reads it | Kept |
|---|---|---|---|---|
| The server's lines | Lifecycle at `info`, each request (route template, status, code, latency, request ID, the user's ID), faults at `warn` and `error`; never a body, a header, a path as sent, a query string or a secret | The container's stdout, in Docker's `json-file` log | Whoever is root on the host | Five files of 10 MiB a container |
| The agent's lines | Each bot's lifecycle, the control stream; chat only at `debug` | The same, on the agent's host | The same | The same |
| Caddy's lines | Its own errors, with `ticket` and `q` deleted from every URI; no access log | The same | The same | The same |
| The audit log | Every write that changes state, with its actor, their address, the target and the outcome; logins and security events | The database | Admins and the Owner, through the API | `[audit] retention_days`, 365 by default |
| The metrics | Counts under fixed labels | Memory, read at the metrics ports | Whoever reaches those ports | Until a restart |

## V17 WebRTC
All seven items are N/A: afkfleet uses no WebRTC.
