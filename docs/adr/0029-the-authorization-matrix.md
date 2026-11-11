# 0029. The authorization matrix of the HTTP API

- **Status:** Accepted
- **Date:** 2026-11-11
- **Related:** Plan.md Phase 13 (P13.1), §7.3; ADR-0010 (`authorize()` and its matrix), ADR-0016 (the route registry and the route-coverage test), ADR-0018 (group E), ADR-0019 (S52, S54), ADR-0022 (S9), ADR-0025, ADR-0027 (group B), ADR-0028

## Context
**Two things were proven apart.** fleet-core's `authorize()` has an exhaustive matrix of its own (ADR-0010): every permission against every role, owner and grant. The route-coverage test proves that every route needing credentials answers 401 without them (ADR-0016). What neither shows is that each route asks `authorize()` the right question about the right thing: that `DELETE /accounts/{id}` asks `DeleteAccount` and not `ViewBot`, or that it asks about the account in the path. Each of the 68 routes has tests of its own that show it for the cases their author thought of, spread over the server's test files.

P13.1 asks for one table: every endpoint against {anonymous, Member without grant, View, Control, Manage, Admin, Owner}, generated and snapshotted. Its notes from earlier phases ask for the account routes in the right states, the bot routes with a fresh limit per row, the mode switch on and off the allowlist, and the metrics router.

Everything below is decided under ADR-0025.

## Decision

### Where it runs
**The matrix is `authz_matrix`, a module of fleet-server's `accounts` test binary.** That binary already holds serve's own app (`app_with`) behind the real router and middleware, over the Microsoft fake and the pacing fake; every service is the real one. A binary of its own would compile those fakes a second time, or share them through `#[path]` with dead-code allowances. The auth tests' harness wires idle account, grant and link services, so a matrix there couldn't ask those routes.

### One world per cell
**Every cell gets a fresh database and a fresh app, and makes one request.** A world holds the same people and things each time: the holder, who owns one active Microsoft account and its bot; a friend, a Member; a steward, an Admin; one enrolled agent; and the cast, a user of their own. A row adds what its route needs (a pending link, a failed bot, a grant to revoke), through the ports or through the routes themselves (a login for a setup token, an invite for its code).

So no cell's write, limit, lockout or session change reaches another: the bot's own limit and the login's per-address bucket start full in every cell, which is the "fresh limit per row" P13.1's note asked for, and a cell that deletes the account doesn't change what the next cast finds. The 691 worlds take about 17 s on the development PC, eight at a time. The password service is built once and shared, since its start hashes; every user's hash is the same one.

**The config is the default but for two settings**, each needed for a row to show its authorization: offline accounts are on (off, the route answers the same to everyone), and `/spawn` is the command allowlist (empty, no mode or chat line is "allowlisted").

### The rows
**Each registered route has at least one row, and a test compares the rows with the registry** (`api_routes` and `metrics_routes`): a new route fails until it has a row, and a row for a removed route fails too. A route has more than one row where the case changes the answer:
- the two step-up routes, stepped up and not (ADR-0028);
- the two setup routes, with an access token (401 for everyone: a setup route takes no session) and with the setup token of the cast's own login;
- the mode switch, the chat routes and the mode writes, on the allowlist and off it;
- a user change by what it changes and whom; an invite by its role; a mode by whose it is and its visibility; a link flow and a session by whose they are;
- `GET /metrics` on its own port and on the API's (404).

**A row's request is valid, and its target is in the state the route wants**, so an answer is the authorization's: a pending account to approve, a failed bot to reset, a paused one to resume, a disabled agent to release. Three rows can't reach a success in-process, because nothing here runs a bot: a restart ends at `409 bot_not_running`, and the three chat rows at `409 bot_not_online`. Each comes after `authorize()`, and the row's case says so. The live events socket is asked as a real upgrade over an in-memory pipe, so its cell is 101.

### The casts, and the second block
**The seven casts are P13.1's.** View, Control and Manage are Members who hold that grant on the holder's account; the Member, the Admin and the Owner hold none.

**The holder is a Member, so one block can't show whose account it is.** An Admin has implicit Manage on a Member's account and may not even see another Admin's. A handler that judged every account as a Member's would pass the first block. So the 25 rows under `/bots/{id}` and `/accounts/{id}` run again in worlds where the holder is an Admin, asked by a Member, another Admin and the Owner.

### What's asserted beyond the snapshot
A snapshot proves only that nothing changed since someone read it. These rules hold whatever it says, and fail with the table in the message:
- **No cell is a request's shape, a limit or a server error** (400, 405, 413, 415, 422, 429, 5xx): such a cell would say nothing about authorization.
- **A public route answers every cast alike.**
- **Every other route answers nobody with 401**, and an authed route never answers a user's session with 401.
- **A Member without a grant gets 404 on every route under a bot or an account**, never 403 (security rule 1).
- **A higher grant may do what a lower one may, and the Owner what an Admin may.**
- **Each step-up route has a row where every user is asked to step up, and one where none is.**
- **An Admin's account is 404 for a Member and for another Admin on every route**, and the Owner is answered there exactly as about a Member's account.

### How it was tested
- **Red first:** the matrix ran against a caller stubbed to answer status 0, and failed on the rules' assertion for all 616 cells of the first block.
- **The snapshot was read cell by cell** against fleet-core's matrix and each handler's documented answers before it was accepted. Nothing disagreed.
- **It pins behaviour that existed, so it was shown red by temporary mutations**, each file restored by checksum:

| Mutation | What failed |
|---|---|
| A row removed | The registry comparison |
| `GET /agents` without its `require` | The snapshot: Members read the agents |
| `GET /accounts/{id}` without its `require` | The rules: a 500, since the response's own field rule refuses a viewer who can't see the account |
| fleet-core: an Admin manages every account | The second block's rule, on all 25 rows |
| fleet-core: an unseen account answers `Forbidden` | The Member's 404 rule, on all 25 rows |
| The lifecycle handler without its `require` | Nothing |

**The last one is by design.** Every bot write is authorized in the handler and again in the service's transaction, on what the transaction reads (ADR-0019, S55), so either check alone gives the same answers. The matrix tests what a caller is answered, not which layer answered. What the handler's check adds is its place before the bot's own limit (S52), which one request per cell can't show; `bots::a_bot_the_caller_may_not_see_is_404_on_every_route_like_an_unknown_one` and the limit's own tests hold that.

## Consequences
- **A new route can't merge without a row**, and a change to who may call a route shows as a changed cell in the pull request's diff.
- **What the table reads:** nothing unexpected. P13.1 found no route that asks the wrong question.
- **The table shows one config.** With offline accounts off or another allowlist, some cells differ; the routes' own tests cover those settings.
- **The relations the casts don't span stay fleet-core's:** an account of the Owner's, a grant on an Admin's account, a caller acting on their own account. The second block covers the one a handler could get wrong on its own, the owner's role.
- **`just check` runs 17 s longer** on the development PC, more on CI's runners.
- **The table is 245 columns wide.** It reads as a table in an editor and poorly in a narrow diff.

## Alternatives considered
- **One world per row, the casts in turn:** a tenth of the worlds, but each cast after the first would meet what the earlier ones changed (a deleted account, a spent limit, a used invite), and every row would need its own undo.
- **`#[sqlx::test]`'s database:** one per test function, so one per cell would need 700 test functions and a table stitched from their outputs.
- **Expected statuses written out in the test instead of a snapshot:** the same table twice, and P13.1 asks for a generated one. The rules above are the part worth writing by hand.
- **The account's owner as an eighth cast:** every per-account cell would read as Manage's does, by `effective_level`; fleet-core's matrix shows it.
- **A real agent for the restart and chat rows** (the loopback suite's): each of those cells would take seconds, for a status that comes after the authorization either way.
