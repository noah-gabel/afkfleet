# 0020. Bot operations, modes and live events: Phase 11 refinements

- **Status:** Accepted
- **Date:** 2026-10-19
- **Related:** Plan.md Phase 11 (P11.3–P11.11), §6, §7.2, §7.3, §7.4, Appendices A to E; ADR-0003, ADR-0010, ADR-0011, ADR-0013, ADR-0015, ADR-0016, ADR-0017, ADR-0018, ADR-0019

## Context
Phase 11 makes the fleet usable from the app day to day: chat, modes, live events, retention, the Tauri side of the events, the screens, dark mode and a design pass. Phase 10 built P11.1 and P11.2 already (ADR-0019, U1), and left one follow-up for Phase 11's first group: U24, a refused login's next session request stays fresh.

The plan read the code first. Nothing builds `ServerMessage::SendChat` yet, the modes port only finds one mode, there's no `/events`, no `GET /audit` route, no retention job, and `agent release` is CLI-only. fleet-client has bot methods only, and the app has no bot, mode, agent or audit screens.

Every open question went to its own read-only review agent, given the options neutrally (CLAUDE.md's step 10). The user approved their decisions (S1–S6) at the plan review, with one addition of their own (U1). Each later group asks its own questions in its session and adds its decisions here.

## Decision
### Grouping *(proposed in the plan, approved by the user)*
One branch and one PR per group, one commit per task, in this order, each after the previous PR is merged. Backend first; each screen comes after the endpoints it needs.

| Group | Branch | Tasks |
|---|---|---|
| A | `p11/u24-p11.3-fresh-requests-and-agent-chat` | U24, then P11.3's agent side: commands that need a signature (S2), the chat rate in each bot's spec (S3) |
| B | `p11/p11.3-chat-endpoints` | P11.3's API (S4–S6), ticked here |
| C | `p11/p11.4-modes` | P11.4 |
| D | `p11/p11.5-live-events` | P11.5 |
| E | `p11/p11.6-p11.7-retention-and-full-flow` | P11.6, P11.7 |
| F | `p11/p11.9-admin-endpoints` | P11.9's backend: `GET /audit`, `agent release` over the API, the grants review at a role change |
| G | `p11/p11.8-events-in-the-app` | P11.8 |
| H | `p11/p11.9-bot-screens` | P11.9's dashboard and bot detail; the DoD's first check |
| I | `p11/p11.9-mode-editor` | P11.9's mode editor |
| J | `p11/p11.9-admin-screens` | P11.9's admin screens, ticked here |
| K | `p11/p11.10-dark-mode` | P11.10 |
| L | `p11/p11.11-design-pass` | P11.11 |
| M | `p11/p11-wrap-up` | The threat model, the README, the DoD's 24-hour run |

- **P11.3 is spread over A and B.** With the chat rate travelling in each bot's spec (S3) and secure chat in fleet-mc (S2), one group would have been about 10,000 lines; Phase 10's group G (about 17,000) was too much for one review, and F and H (about 5,300 each) read well. **P11.9 is spread over F, H, I and J**, ticked in J.
- **New dependencies** arrive with their group, each with the user's approval of its version, license and release date (checked on crates.io at the planning):
  - D: axum's `ws` feature, which brings tokio-tungstenite 0.29.0 (MIT, 2026-03-17) and tungstenite 0.29.0 (MIT OR Apache-2.0, 2026-03-17), since axum 0.8.9 pins `tokio-tungstenite = "0.29.0"`; tokio-tungstenite as fleet-server's dev-dependency; futures-util 0.3.34 and moka 0.12.16 only if D needs them. 0.30.0 exists but would build a second tungstenite.
  - E: tokio-tungstenite as fleet-integration's dev-dependency (ADR-0019's note for P11.7).
  - G: tokio-tungstenite and futures-util in fleet-client; the TLS features are G's question, since §5's `rustls-tls-native-roots` wouldn't honour the app's custom CA.
  - H, I, J: single Radix packages, one per shadcn component a screen needs. L: bundled fonts.
- **Who decides what:** the review agents decide what step 10 lets them. The user decides P11.6's size bound (E), P11.11's stable screenshots and visual direction (L), the texts friends read (one table per group plan), the looks and UX questions, and whether the chat console names who sent each line (B). A review agent's option that would add a `commit_unaudited` exemption (E), a route kind (D) or a real-time moka test (D) comes to the user.
- **Hands-on checks:** none in A (the user may run `just test-real-account` once for S2's refusal); B's chat through `just dev-bots`; G's live waiting count; each screen group in the app; K's theme switch; L's directions as screenshots.
- **The DoD's two checks** are the user's, with their own accounts: the first in H, once the bot screens exist; the 24-hour run in M, after the design pass. Each gets default-profile tests that run its own setup, config and log check without the account, as Phase 10's managed check got after its first run (ADR-0019, group H).

### The user's choices
- **Approved at the plan review:** the thirteen groups; S1–S6; the chat limits' ranges and defaults (rule 10: `chat_per_bot_interval_secs` 1–60, default 3; `chat_per_bot_burst` 1–10, default 3; `chat_per_user_per_min` 1–60, default 20); the runtime's chat-quota test moving to fleet-core's `ChatRate` with the setting it tests (S3); the `/me` step of `just test-real-account` expecting fleet-mc's refusal (S2); and B's three error texts as drafted:
  - `bot_not_online` (409): "Only an online bot can send chat."
  - `chat_unavailable` (409): "This Minecraft server only accepts signed chat, and the bot can't sign this message. Commands with a message, such as /msg or /me, can't be signed."
  - `command_needs_step_up` (403): "This command needs your password: confirm it's you to send it."
- **U1. The DoD checks with real accounts get a data folder of their own** *(the user's addition at the plan review)*. Linking a real account stores its Microsoft refresh token in the server's database, sealed by a vault key file that sits right next to it.
  - Both checks (H's first, M's 24-hour run) use one folder, `target/dod-run/`: its database, its vault key, its PKI, the agent's identity and the logs. They run through recipes of their own, which H adds, never the everyday `target/dev-server/`, so the account linked in H can stay for M.
  - They log at the default `info`, so no chat text is written to disk (chat is logged only at `debug`). A default-profile test pins the recipes' filter.
  - A cleanup recipe deletes that whole folder when the user is done. A default-profile test proves it removes everything inside and touches nothing outside: the other folders in `target/`, a file beside it, and a link inside it that points outside, which it removes without following. It refuses any other path.
  - The steps say: unlink the accounts in the app first, then run the cleanup, then optionally end the app's consent in the Microsoft account.
  - This mirrors production, where the key file lives on the server's host next to the database it seals; `target/` is never committed.

### Decided by the review agents *(approved by the user at the plan review)*
One read-only agent per question, each given the options neutrally.
- **S1 (security). U24 lives in fleet-core's state** (ADR-0019, U24: "after a refused login, a bot's next session request stays fresh until a fresh grant arrives").
  - `BotState::Backoff { attempt, fresh }`. `session_unavailable` carries `AwaitingSession.fresh` into the backoff, and `retry_due` asks with it. `restore` keeps it for `AwaitingSession` and `Backoff`, and gives false for `Connecting` and `Online`, where a grant had arrived. `session_ready` turns it into `Connecting { auth_retried: fresh }` as before, which ends it.
  - Every backoff entered from `Connecting` or `Online` is non-fresh; an `AuthInvalid` while Online stays transient (ADR-0010), so a Minecraft server can't drive Microsoft refreshes. Start, Stop, Reset, Resume, a restart and a server change begin non-fresh, as before.
  - The proto's `Backoff` gains `bool fresh = 2`; `PROTOCOL_VERSION` stays 1 until the first release (ADR-0019, S9). No database, DTO or `ManagedCredentials` change: the server stores only the state's name, and the agent already wipes a kept grant on a fresh request and never hands one to it (S63).
  - *Consequences:* a fresh grant after a backoff that's refused again ends `Failed(Auth)`, which is §6 row 5's one fresh token; a refused request isn't a token. S58's limit still fits: with the default backoff, about six fresh requests fall in the first five minutes, the burst, then at most one per 10 minutes reaches the token service and the rest are denied retryably with the bot still fresh. Requests without a stream fail at the agent and never spend it.
  - *Why:* lifecycle state goes through `transition()`; the flag survives the supervisor's restore after an actor crash, which reads the published state; it's U24's wording exactly; fresh requests stay visible to the state machine, as S62 required.
  - *Rejected:* also across Stop then Start and the sticky states (overturns ADR-0010's "`Reset` from `Failed(Auth)` asks for a normal session", and puts fields on four more states and the stored sticky state, against at most one more refused join per deliberate action); the flag in `ScheduleRetry` and `RetryDue` (a crash during the backoff loses it); the agent's provider upgrading requests (the state machine would read `auth_retried: false` and a second refusal would loop; S62 rejected hiding a refresh from it); the server dropping its cached session on a reported refusal (the request still wouldn't be fresh, and a compromised agent could force refreshes outside S58's limit).
  - *Residual:* a deliberate restart, a server change or an agent restart after a refusal asks once without `fresh`, which can cost one refused join before the normal fresh retry.
- **S2 (security). Secure chat: refuse what the server would reject, judged by its own command tree** (P11.3's flagged note; ADR-0011's "Commands: a known limit, not decided"; ADR-0013's mode chat "through `ChatUnavailable`").
  - Wherever fleet-mc's `needs_signing` holds (an online account, an authenticated join, a server whose login packet enforces secure chat), a command whose name (`ChatMessage::command_name()`, the name the allowlist uses) can reach a `message` argument in the server's command tree is refused before it's sent, with the existing `SessionError::ChatUnavailable`, even when the session signs chat.
  - The reachability follows children and redirects but never a redirect to the root, as vanilla's own check stops at the root: `/execute … run` isn't caught, while `tell`, `w`, `tm` and `minecraft:` forms are. It's one linear backward pass from the message nodes with a visited set, ignoring indexes out of range, so a tree built to be slow costs one pass. Until a tree arrives, commands go out as before, as the vanilla client does with an empty tree.
  - User chat gets `ChatFailed{ChatUnavailable}` through the existing path; mode chat is skipped at `debug` as before. No wire and no runtime change. fleet-mc warns once per session about a refused command, in its own words, never the command.
  - *Why:* the vanilla client decides what to sign from this same tree, and the server checks against its own copy, so this refuses exactly what the server would reject; a plugin's `/msg` with a plain text argument still goes out. It ends `Ok` for chat the server drops, the bug the user's P3.7 fix ruled out, and an ERROR in the server's log for every repeating `/me` of a mode.
  - *Rejected:* signing commands (azalea 0.16 has none, "TODO: commands that require chat signing"; it needs a brigadier parse of argument types and azalea's signing internals, and a wrong signature can disable the session's chat; it can be built on this tree later); a fixed list (refuses plugin commands that work unsigned, misses datapacks', and needs updating per Minecraft version); refusing at the API (breaks `/msg` where nothing is enforced and overturns ADR-0013's decision); keeping the limit.
- **S3 (security). One chat rate per bot, the server's, sent in its spec** (P4.5's note: "decide where that setting lives (server settings, sent to agents)").
  - fleet-core's `chat::ChatRate`: an interval of 1–60 s and a burst of 1–10, 3 s and 3 by default, refused and never clamped. The burst stops at 10 because the agent enforces the value too, and that keeps every allowed value under vanilla's spam kick.
  - `BotSpec.chat_rate` (proto: a `ChatRate { interval_secs, burst }` message, field 7). The server's `[rate_limits] chat_per_bot_interval_secs` and `chat_per_bot_burst` fill it in the reconciler's specs; standalone agents use the default, and `agent.toml` gets no key.
  - The agent's runtime builds each bot's bucket from its spec and swaps it in place when an update changes the rate (behind a std mutex, never held across an await); an equal rate keeps the bucket. The runtime's fleet-wide `chat_interval` and `chat_burst`, and `FleetSetupError::ChatQuota`, go; the zero-interval test moves to `ChatRate`.
  - The HTTP limits are B's: `chat_per_user_per_min` (keyed by the user), then the per-bot rate (keyed by the bot), spent after `authorize()` whatever the outcome, so a 429 never reveals a bot (as S52). An agent's `RateLimited` reaches the user as the same 429.
  - *Why:* the user's Phase 4 answer named "server settings, sent to agents", and Appendix A plans the keys; one source keeps both ends equal, so §7.4's "all configurable" works both ways; the agent's bucket is the only guard that sees mode and user chat together before the Minecraft server's spam kick.
  - *Rejected:* a fixed, independent agent default (a looser server gives successes followed by agent refusals, so the real rate could only be lowered); an `agent.toml` key (two hosts to keep in sync, against "server settings"); enforcing only on the server (overturns P4.5's shared bucket and leaves mode chat unguarded); the rate in `Welcome` (a fleet-wide setter, applied before the reconcile, and no per-server rates later).
- **S4 (security, built in B). Step-up for chat:** `POST /bots/{id}/chat` (authed) takes plain chat and allowlisted commands (Control) and never sends a command off the allowlist, even after a step-up: a Manage holder gets 403 `command_needs_step_up`, before any chat limit is spent, and the app resends through `useStepUpAction` to `POST /bots/{id}/command` (a step-up route, any command, Manage).
  - *Why:* S47's test. An unlisted command fails all three of its conditions: `/op` widens access on a Minecraft server, `/pay`, `/clear` and `/give` lose things, and nothing undoes them. Plain chat and allowlisted commands pass. The Owner and Admins hold implicit Manage on many accounts, so an unlocked PC or a stolen refresh token could otherwise `/op` someone on every server where those accounts are operators.
  - *For C (P11.4):* writing the text of a command off the allowlist into a mode needs a step-up too; switching to an existing mode stays as S47 has it (its text is fixed), recorded as an accepted risk.
  - *Rejected:* no step-up for chat (the most dangerous bot action left open); a step-up for every command (prompts for `/spawn`, and disagrees with the mode switch); a step-up for all chat (a password to say hello); one route that answers `step_up_required` itself (checks the stamp in an authed handler, against rule 1 and the user's "checked by the extractor only", ADR-0016).
- **S5 (security, built in B). The send's answer.**
  - One write transaction re-reads the actor, the bot and the grant, authorizes again, and judges reach (fleet-core's new `judge_chat`, table-tested like `judge_command`): no agent, desired Stopped, or held without an Online report is 409 `bot_not_online`; an agent not Online or a bot not held is 409 `bot_unreachable`. A refusal writes and audits nothing.
  - Otherwise it mints the `ChatRequestId`, appends the row (S6) and commits the entry, all before anything is sent. Then it subscribes to the bus and sends through a new `LiveAgents::send_chat`, checked and queued in one critical section as `command` is.
  - It waits outside any transaction until the earlier of the send plus 6 s and the handler's start plus `request_timeout` − 2 s. It matches the request ID, the bot and the agent, not the generation, since an answer may come on a later stream.
  - *Answers:* 200 `sent`; 202 `pending` when nothing came or the subscriber lagged; `NotOnline`, `Disconnected` and `NotInWorld` 409 `bot_not_online`; `ChatUnavailable` 409 `chat_unavailable`; `RateLimited` and `QueueFull` 429 `rate_limited` with `Retry-After` the per-bot interval; `TimedOut`, `SessionBusy`, `AgentBusy`, `UnknownBot` and `ShuttingDown` 409 `bot_unreachable`; `Unsendable` 500. The match has no wildcard. The body is `ChatSendResponse { request_id, message_id, outcome }`, `outcome` an `Open<sent | pending>`.
  - *Why:* S3's 429 and S2's error need the agent's answer, and ADR-0019's group G note planned a bounded wait ("one that never comes reads as unknown"). Refusals and `ChatSent` come within milliseconds, and fleet-mc's 5 s job timeout fits, so `TimedOut` comes back as an answer.
  - *Rejected:* no wait (no feedback before P11.8); a wait for refusals only (S2's refusal is a delivery failure); a wait of about 12 s (handlers held longer, and a slow database pushes the answer into a 408).
- **S6 (security, built in B). A user's message is a chat row; its audit entry holds no text.**
  - A `chat_messages` row, direction `out`, kind `user`, appended in the send's audited transaction through the existing guarded `append`, with the agent reach found; `None` rolls back as 409 `bot_unreachable`. No migration: `kind` has no CHECK, and an outgoing row has no sender and no cut. No sender column: the audit entry names the actor and the row.
  - *Field rule:* an outgoing command row shows its arguments only to Manage holders; everyone else sees `/name`. Mode rows follow the same rule. A `/login <password>` in the history mustn't reach View holders.
  - *The audit:* `bot.send_chat` and `bot.send_command`, one per route; the actor, the IP, `AuditTarget::Bot`; metadata `request` (the `ChatRequestId`), `message` (the row's ID), `command` (`none`, `allowlisted` or `unlisted`) and `command_name` only when `allowlisted`. Never the text or its length.
  - *Why:* ADR-0016's metadata rule allows names from closed sets and values the server made, never text a client sent; Admins read the audit log while other Admins' and the Owner's bots are hidden from them; commands can carry passwords; and with no text in the audit, retention really deletes what users said. Accountability stays: who, which bot, when, which route, whether a command.
  - *Rejected:* a row only once `ChatSent` arrives (the text would wait in memory, and the ingest's `EventBatch` would carry a user's decision, which widens U17 in substance, the user's); no row (the console would show replies without what was asked); the text in the audit (above).
  - *The user's, at B's planning:* whether the console names who sent each line, and to whom. A column can be added and backfilled from the audit entries' `message` key later. Unlisted command names or text in the audit would loosen ADR-0016's rule, also the user's.

### Notes for later groups
- **B (P11.3's API):** S4–S6; the HTTP chat limits (S3); the answers' order as R38: 401, the per-user 429, the step-up's 403 on `/command`, a malformed path's 404, the body's 400/422, an unseen bot's 404, 403 `forbidden`, the 403 pointer on `/chat`, the chat 429s, 409; fleet-client's `send_chat`, `send_command` and the history; `just dev-bots`' `chat`, `command` and `history`.
- **C (P11.4):** S4's rule for modes; the serde error mapping and "on respawn" are C's questions.
- **D (P11.5):** the send publishes `Chat { id, entry: UserSent }` after its commit and before the stream send, with its request ID, so live viewers pair `ChatSent` and `ChatFailed` with the line; S6's field rule applies to live events too.
- **E (P11.6):** `user` rows are deleted by age and size like other chat; no chat text in the audit to scrub (S6).
- **H (P11.9):** the send box: a spinner of at most 6 s, a 202 as sent with a pending mark, `chat_unavailable` without a "try again"; U1's folder and recipes, the cleanup and its test.
- **M:** U1's 24-hour run on the same folder, logging JSON at `info` to files.

## Consequences
- **Easier:**
  - A refused login can't be undone by the next backoff: the server's grant after it is always fresh.
  - A user learns at once that a Minecraft server won't take a command, instead of a success the server dropped.
  - One chat rate per bot, set in one place, holds on both ends.
- **Harder:**
  - The protocol gains two fields before its first release (`Backoff.fresh`, `BotSpec.chat_rate`); after the release, removing either needs a version decision (S9).
  - fleet-mc reads another server packet, the command tree, as untrusted input.

## Alternatives considered
- **P11.3 in one group:** about 10,000 lines, more than one review holds.
- The rejected options of S1–S6, each listed with its decision above.
