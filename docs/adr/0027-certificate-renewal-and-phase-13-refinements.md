# 0027. Agents renew their certificates; Phase 13's groups and refinements

- **Status:** Accepted
- **Date:** 2026-11-10
- **Related:** Plan.md Phase 13 (P13.1–P13.8), Appendix C; ADR-0019 (S1, S9, S11, S24, S26, S27, S30, S68, R10, R52), ADR-0017 (a save that outlives its caller), ADR-0022 (the update's checks, "The DoD's run"), ADR-0025

## Context
**An agent's certificate lasts 60 days and nothing renewed it.** `[grpc] agent_cert_lifetime_days` defaults to 60, and `deploy/` doesn't change it. When it ends, the TLS handshake fails, the agent finds its own certificate expired and exits, and every bot it ran goes offline. Enrolling again makes a new agent, so the old one's bots wait until someone disables it and releases them. Nothing warned beforehand except the Agents screen's line in the last 14 days. The first agents installed with v0.1.0, in early November 2026, reach that end in early January 2027, so this is Phase 13's first group and goes out as v0.1.2.

Phase 10 left renewal to P13.6 and kept the way open (ADR-0019, S1): `agent_certs.agent_id` isn't unique, the SAN's agent ID comes from the server's record, the agent's identity lives on its writable data volume, and P13.6's notes say where `RenewCertificate` sits (behind `Connect`'s gate and identity check, the same bucket and answer), that its answer is PEM checked like `enroll`'s, and that the identity must be swapped under a running agent.

**The server's own certificate** lasts 365 days and is renewed by hand (`pki server-cert`, `setup.sh renew-cert`). Serve logged an expired one at `error`; nothing warned before (the user's note at Phase 10's plan review).

**The app's update download** took any URL scheme `latest.json` names (Phase 12's DoD finding). The installer is verified against the committed key either way.

Everything below is decided under ADR-0025. The user set the scope: renewal at two thirds of the lifetime, at once for an agent already past it, an expired certificate still enrolls again; warnings in the logs and the metrics, and in the Agents screen and the runbook if it fits; mixed versions keep working, the server updated first; https for the update download in release builds.

## Decision

### Phase 13's groups
One pull request each, in this order:

| Group | Task | What |
|---|---|---|
| A | P13.6, and the update download's scheme | This ADR's renewal and warnings; released as v0.1.2 |
| B | P13.1 | The authorization matrix |
| C | P13.2 | The fuzz targets |
| D | P13.3 | Mutation testing; after B, whose tests kill their share first |
| E | P13.4 | The load test and its hot spots |
| F | P13.5 | The chaos tests, and whether `serve` refuses a missing database file in production (an unmounted data volume is the same kind of fault as a full one) |
| G | P13.7, P13.8 | The reviews, ASVS, and the phase's DoD |

The user's rough split had the load and the chaos tests in one group; they're two, since each carries a long list of notes.

### The protocol
- **`AgentControl` gains `RenewCertificate(RenewRequest) returns (RenewResponse)`.** `RenewResponse` is `certificate_pem` (1) and `optional int64 not_after_unix_ms` (2), as `EnrollResponse` without the agent's ID.
- **`RenewRequest` is empty: there's no CSR.** The call's own client certificate names the agent and holds the key, and the TLS handshake proved the caller has that key. The server signs that same key again. A renewal can't bind another key, and the server parses nothing a caller wrote. An unknown field a later agent might send is skipped by prost and builds nothing.
- **`PROTOCOL_VERSION` stays 1.** ADR-0019's S9 asks for a decision about the number at every wire change after the first release: a new call changes no message an existing peer reads, an older server answers it `UNIMPLEMENTED`, and an older agent never makes it. The version keeps meaning "the stream's messages".
- Both ends bound the call: the server signs within 15 s (`RENEW_SERVER_DEADLINE`), the agent waits 20 s (`RENEW_CLIENT_DEADLINE`) and reads an answer of at most 16 KiB.

### The server
- **The gate and the admission are `Connect`'s** (ADR-0019, S24, S30), because the call is in the same service: no client certificate is `UNAUTHENTICATED` at once; the certificate's `connect_per_cert_per_min` is spent, so a renewal costs what a connect costs; the fingerprint's row, fleet-core's `admit` (unrevoked, unexpired by the server's clock, enabled) and the SAN are checked by the same `AgentConnections::admit`. Every identity problem is the one `CERTIFICATE_REFUSED`, recorded as an `agent.connect` Failure under the same daily cap. An expired certificate is refused: it enrolls again.
- **The call needs no live stream and takes no slot.** Binding it to the connection that carries the agent's stream would add state to the one path that keeps agents alive, and a copy of the key gains nothing from a renewal it can't use without the slot (below).
- **Signing** (`AgentCertificates::renew`) reads the presented certificate with x509-parser, only after its fingerprint's row was found (ADR-0019, R23), takes its Ed25519 public key (algorithm OID, no parameters, 32 bytes) and signs the agent profile again: the SAN from the row's agent ID, a new serial, valid from an hour before the server's clock for `agent_cert_lifetime_days`. It runs in `spawn_blocking`, outside any transaction, and the deadline bounds only it.
- **One transaction** then reads the presented certificate and its agent again and applies `admit`, so a disable or a revocation since the admission wins; revokes the agent's other unrevoked certificates but the presented one; stores the new fingerprint; and commits `agent.cert_renew` (`fingerprint`, `replaces`, `not_after`, `certificates_revoked`). It runs to its end past the deadline, as an enrollment's does.
- **The presented certificate stays valid.** If the answer is lost, the agent still connects with what it has and asks again; that second renewal revokes the first one's result, so an agent holds at most two unrevoked certificates.
- **The hello ends the old certificate.** A hello's transaction revokes the agent's certificates that were stored before the one it presents (`revoke_older_certs`, by row order), and says so in `agent.connect`'s `certificates_revoked`. A process still holding a replaced certificate is refused as `revoked`. **Never a newer one:** the first version revoked every other certificate at a hello, and a test of "the answer may be lost" showed that a hello with the old certificate then revoked a renewal the agent might already have on disk, which would lock it out at its next start.
- **Metrics:** `afkfleet_agent_certificate_renewals_total`.
- **The gate's three log lines** say "a control call" now, since they're `RenewCertificate`'s too.

### The agent
- **When.** Once two thirds of the certificate's lifetime have passed, by the run's clock (fleet-core's `renewal_due`, from the certificate's own `notBefore` and `notAfter`): 40 days and 40 minutes after a 60-day certificate's start. An agent that starts past that point asks right after its first `Welcome`. After a failure it asks again every hour, and after a renewal no sooner than an hour later, whatever the new certificate's dates say: a server whose clock is far behind can answer a certificate that's due already, which would otherwise end every stream with another renewal (a test found the loop).
- **How.** On the welcomed stream's own connection, so nothing new is dialled. The answer passes `enroll`'s check (one PEM certificate and nothing around it, a client certificate the pinned CA signed, for this agent's key, valid now). Then, in this order: the new endpoint is built; the certificate's file is replaced; the link's endpoint and kept certificate are swapped; the stream ends with the cause `renewed` and the link connects again after its usual backoff. Nothing is stored unless every step before it passed.
- **An attempt is never raced against the stream.** The link's loop races only the wait; the attempt itself is awaited to its end before the loop reads another message. A call dropped between the server's commit and the file's write would leave the file and the process disagreeing about the certificate, the hazard ADR-0017 met with refresh tokens. The server's "never a newer one" rule covers the same case from the other side.
- **The file** is replaced by writing `<cert_file>.renew.tmp` (new, owner-only on Unix, synced) and renaming it over `cert_file`, which replaces it in one step on Unix and Windows. It's the one exception to "an identity's files are written once": only the certificate, only by a renewal, never the key. `enroll`'s own lock (`.tmp`) is another name and is never touched.
- **The key doesn't rotate.** The agent keeps it in memory (an `AgentKey`, wiped when dropped) to check the answer and build the next TLS identity; tonic already holds a copy for the process's life (ADR-0019's residual).
- **x509-parser in fleet-agent**, to read the certificate's two dates after rustls checked it. It was fleet-server's alone; Cargo.lock gains only the edge.
- **A server that doesn't renew** answers `UNIMPLEMENTED`: a `warn` line says to update the server, the agent keeps running, and it asks again each hour (repeats at `debug`).
- **An expired certificate** is as before: the agent stops and enrolls again (ADR-0019, S68, R52).
- **Metrics:** `afkfleet_identity_not_after_timestamp_seconds` and `afkfleet_identity_renewals_total` by `outcome` (`renewed`, `failed`).

### The warnings
- **One rule for an agent's certificate** (fleet-core's `expiry`): it ends soon in its last 14 days, or its last quarter when it lives shorter than 56 days, so a short-lived one isn't warned of all its life. 14 days is the Agents screen's own (ADR-0020, U126). An agent that renews never gets there: the warning means it couldn't.
- **The server** (`app::certificates::CertificateWatch`, run by the housekeeping each minute, only with a control plane): the gauge `afkfleet_agent_certificates` by `state` (`expiring`, `expired`) counts the enabled agents by their latest unrevoked certificate; once a day, the first run included, each such agent gets a line with its ID, its name and the certificate's end: `warn` while it can still renew, `error` once it expired.
- **The server's own certificate:** `afkfleet_server_certificate_not_after_timestamp_seconds` from the start; within 30 days of its end, a `warn` line at the start and once a day, naming `pki server-cert`; expired stays an `error`, now daily too.
- **The agent:** its start line gains `not_after`; while its certificate ends soon and isn't renewed, one `warn` a day with the days left.
- **The Agents screen** keeps its 14-day line and changes its text from "Enroll a new agent before then." to "It renews once connected on 0.1.2 or later.", which is what's wrong when it shows. No DTO changed.
- **The runbook** gains "Agents' certificates", two alert examples and the new series.

### Mixed versions
| Server | Agent | What happens |
|---|---|---|
| 0.1.2 | 0.1.1 | The agent never asks. The server warns in the certificate's last 14 days; update the agent before its certificate ends, and it renews at its first `Welcome` |
| 0.1.1 | 0.1.2 | The agent asks, gets `UNIMPLEMENTED`, keeps running and asks hourly; it renews within the hour after the server's update |
| 0.1.2 | 0.1.2 | Renewal at two thirds |

The hello's revocation changes nothing for an agent with one certificate. An older app shows `agent.cert_renew` by its name: audit actions are an open list.

### The update download
A release build downloads over https alone: the manifest's URL is checked before anything is sent (another scheme is `update_invalid`), and reqwest's `https_only` refuses a redirect to anything else. The rehearsal's debug build still downloads from loopback over plain http. `manage_updates` passes the same `!cfg!(debug_assertions)` that decides whether updates are on.

### How it was tested
- **Red first** for every piece, against stubs with the real signatures. Where a test was written after its code (the port-level renewal tests, the start's warning, the end-to-end case, the hello with an agent's only certificate), a temporary mutation showed it red, and the file was restored by checksum: the handler answering `UNIMPLEMENTED`, `open` without its certificate line, the link without its swap, a hello that revokes every certificate.
- **The end-to-end case** (`loopback::certificate_renewal`) runs the real agent against the real server. TLS reads the real time, so the server's manual clock starts 40 days back, the agent enrolls there, and the clock moves to the real time before the agent starts: the renewal is due 20 s after the enrollment, with the bot online first. It waits those 20 s in real time, as that suite's cases do.
- **The agent's cases** run the real managed run against the fake control server, whose `RenewCertificate` answers a script and `UNIMPLEMENTED` without one.

## Consequences
- An agent that runs at least once in the last third of its certificate's lifetime, against a server that renews, never expires. One switched off for those 20 days still does, and enrolls again.
- **A renewal costs one reconnect** every 40 days: the stream ends and comes back in a second or two, as after any network blip; the bots stay on their servers.
- **A copy of an agent's key dies at the next renewal:** once the real agent renewed and said hello, the copied certificate is revoked. A thief who renews first and takes the slot while the agent is away revokes the agent's certificate instead: the agent stops with a refusal, which is visible, and a disable ends both. Before, both could alternate for 60 days unseen but for the conflict's audit entry.
- **A restored backup forgets renewals made after it:** an agent that renewed since is unknown to the restored server ("a certificate the CA signed but the server doesn't know"), stops, and enrolls again. The runbook says so.
- **Each renewal is an audit entry and a row** in `agent_certs`; rows are never deleted. About nine a year per agent.
- The agent parses its own certificate with a second library; it reads two dates of a certificate rustls already accepted.
- The warnings are log lines and gauges; nothing pages anyone. An operator without a scraper sees them in the logs, the Agents screen, or not at all.

## Alternatives considered
- **A CSR in the request,** as `enroll` has: the server's CSR check would run on a caller's bytes again for a key the handshake already proved. Rejected; a CSR could be added as field 1 later for key rotation.
- **Rotating the key at each renewal:** two files to replace together, and a window where they disagree. The key's exposure is bounded by the hello's revocation instead.
- **The server decides when** (the agent asks daily, the server answers "not yet"): one more answer to get wrong, and the agent needs its certificate's end for its own warning anyway.
- **Revoking the old certificate when the new one is issued:** a lost answer would lock the agent out.
- **Revoking every other certificate at a hello:** the lockout the test found (above).
- **Renewal only on the connection of the agent's live stream:** above.
- **Reading the dates through rustls's verifier** (its errors carry `not_before` and `not_after`): no new dependency edge, but a parser by side effect.
- **A flag in `AgentResponse`** for the Agents screen: the 14-day rule is already there, and the text was the only thing wrong.
- **A warning by fixed days alone:** `agent_cert_lifetime_days` goes down to 1.
- **The version bump in a pull request of its own** (releasing.md's step 1): the texts name 0.1.2, and the release follows this merge directly, so the bump is this group's last commit.
