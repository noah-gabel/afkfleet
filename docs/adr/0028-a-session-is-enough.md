# 0028. A session is enough: the password step-up stays only for the two credential changes

- **Status:** Accepted
- **Date:** 2026-11-11
- **Related:** Plan.md §7.2, Appendix B, Phase 13 (P13.9); supersedes the step-up decisions of ADR-0016 ("Step-up", and Phase 8 group E's amendment), ADR-0018 (group E's account writes), ADR-0019 (S47's bot address) and ADR-0020 (the step-up in S4, S8, S16, S17, S49, S51 and U54); ADR-0017 (the app's prompt), ADR-0025

## Context
**The step-up as it was.** Since Phase 7 a "step-up action" needed the password entered again within the last 5 minutes: `POST /auth/step-up` stamps the session, and a route registered with `step_up_route` answers 403 `step_up_required` without a fresh stamp (ADR-0016). It began with three actions: a new password, a replaced authenticator and a role change. Every phase since added more, each for a good reason of its own, until 22 routes asked: every admin write, every account write but a link, a bot's server address, a command off the allowlist, a mode holding one, deleting a mode, the servers' settings, disabling an agent, releasing its bots, and leaving a shared account. The app showed "Confirm it's you" inline wherever the server asked.

**The user's decision (2026-11-11).** Once logged in, with password and 2FA, the app never asks for the password again: every `step_up_route` becomes an `authed_route`, and the app's password dialogs go. Roles (Owner, Admin, Member) and per-bot grants (View, Control, Manage) stay exactly as they are. The user left one question to this ADR: whether "change password" and "reset 2FA" keep asking. Their reviewer recommends keeping those two, since they can lock a person out for good and ASVS Level 2 expects it. It ships in v0.1.2, which was on `main` but not yet tagged.

This is a weakening (CLAUDE.md, security rule 10): it's listed under **Weakened** in its pull request, and "Consequences" below says what it gives up.

## Decision

### Which routes still step up
**Two keep the step-up: `POST /me/password` and `POST /me/mfa/reset`.** Everything else needs a valid session and what `authorize()` asks, nothing more.

Why these two, and only these:
- **They replace a login credential.** Every other action is bounded by the user's role and grants and ends with the session. These two change who can log in as the user at all.
- **They can lock the real user out for good.** A new password ends every other session, and a new authenticator replaces the secret and the recovery codes. Whoever holds a live session, at an unlocked PC or with a stolen token, could otherwise take the account and leave its owner with nothing that works: no password, no code, no session. For the Owner there is no one above them in the app to undo it.
- **They'd make a stolen session permanent.** A session ends 7 days after its last use and 30 days after its login. With the password in hand that end means nothing.
- **It costs the user almost nothing.** Both are rare, deliberate actions on the profile screen, where typing the current password is what people expect. It's the one place the dialog was never the complaint.
- **The standard asks for it.** ASVS 4.0.3's 2.1.6 wants a password change to take the current password, and the reviewer's reading is that Level 2 expects re-authentication for both. P13.8's self-review maps the exact items; whatever it finds for the other twenty routes is an accepted deviation recorded here.

The twenty that become authed routes:

| Area | Routes |
|---|---|
| Users and invites | `PATCH /users/{id}`, `POST /invites`, `DELETE /invites/{id}` |
| Accounts | `DELETE /accounts/{id}`, `POST /accounts/{id}/approve`, `POST /accounts/{id}/refuse` |
| Grants | `POST /accounts/{id}/grants`, `PUT` and `DELETE /accounts/{id}/grants/{user}`, `DELETE /accounts/{id}/grants/me` |
| Agents | `POST /agents/{id}/disable`, `POST /agents/{id}/release` |
| Bots | `PATCH /bots/{id}`, `POST /bots/{id}/command` |
| Modes | `POST /modes/with-commands`, `PUT /modes/{id}/with-commands`, `DELETE /modes/{id}` |
| Servers' settings | `POST /server-settings`, `PUT` and `DELETE /server-settings/{id}` |

### The mechanism stays as it is, for the two
`POST /auth/step-up`, the session's stamp and its 5 minutes, `SteppedUpUser`, `step_up_route`, `x-step-up` in the OpenAPI document, the audit entry `auth.step_up`, and a wrong password counting toward the lockout are all unchanged. So is the app's profile flow. A new password still ends this session's step-up and every other session.

Nothing new is built for two routes, and nothing well tested is rebuilt for a release whose point is to remove.

### The twin routes stay, without their step-up
`POST /bots/{id}/command` and the modes' `/with-commands` twins existed so that a command off the allowlist would pass a step-up route (ADR-0020, S4, S16). They stay as plain authed routes, and the plain routes still point there with 403 `command_needs_step_up` and `mode_needs_step_up`. The rights are as before: Manage for a command off the allowlist, whichever route.

- **The two codes keep their names.** A code is part of the wire, and an app of v0.1.1 reads them. Their messages no longer speak of a password: "This command isn't on the server's list: send it as a command." and "This mode holds a command that isn't on the server's list: save it with its commands."
- **The app follows the pointer by itself,** within the same send or save: the box or the form stays locked meanwhile, so what's sent is what's shown, as before (ADR-0020, R118, R162).
- **Merging each twin into its plain route** is the cleaner end, and a change of the API's shape: three routes, two codes, three Tauri commands and the allowlist's pointer tests. It's proposed in the pull request, not done here.

### The app
- **No inline "Confirm it's you" anywhere but the profile.** `useStepUpAction` becomes `useWriteAction`: one write at a time per screen, its failure at its row or form. `StepUpPrompt` is gone; `StepUpForm` stays for the profile's two flows.
- **What asked a question first still does.** Each of these had its own question before the password, and keeps it: a role change, disabling a user, revoking an invite, disabling an agent, releasing its bots, approving and refusing a link, deleting an account, leaving one, revoking a grant, deleting a mode, deleting a server's settings.
- **What ran at once after the password now runs at once:** enabling a user, creating an invite, sharing an account, a grant's level (it changes as soon as it's picked), a bot's server address, a command off the allowlist, and a save of a mode or of a server's settings. The password was their only pause. None of them deletes anything, and each is undone by the next write; no question was added, since the decision is that these no longer interrupt.
- **A server older than the app** still answers `step_up_required` to those writes. The app has no prompt for it any more, so it says: "This server is older than the app and still asks for your password here. Ask its owner to update it." Only the profile's flows read that code themselves.
- **The editor's column loses its prompt slot** (`EditorColumns`): its grid has four rows instead of five, and nothing moves, since the slot was empty and hidden whenever no prompt showed.
- **The dev console** (`just dev-bots`) never asks for the password again after its login.

### Mixed versions
| App | Server | What happens |
|---|---|---|
| v0.1.1 | v0.1.2 | Works, and already without the prompts. The older app sends each write first and asks for the password only when the server answers `step_up_required`, which it now does for the profile's two flows alone. It follows `command_needs_step_up` and `mode_needs_step_up` to the twin as before, and the twin takes the request without asking. |
| v0.1.2 | v0.1.1 | The twenty writes are refused, with the text above. Everything that never needed a step-up works, and so does the profile. |

So the server is updated first, as ADR-0027 already asks for v0.1.2.

### How it was tested
- **Red first.** Each "needs a fresh step-up" test of the server was turned into "needs no step-up" before the routes changed, and every test helper stopped stamping sessions: 157 server tests failed with 403 `step_up_required`, and passed once the twenty routes were authed routes.
- **The registry test names the two.** `the_setup_and_step_up_routes_are_registered_with_their_access_and_class` lists exactly `POST /me/mfa/reset` and `POST /me/password` as step-up routes, and `the_authed_routes_are_registered_with_their_class` lists the twenty among the authed ones. A route that gains or loses a step-up fails one of them. `every_step_up_route_answers_403_without_a_fresh_step_up` still runs over whatever is registered so.
- **No helper steps up.** In fleet-server's, fleet-client's, the desktop bridge's and the integration suite's tests, and in the app's fakes, only the tests of the password, the authenticator and the step-up itself stamp a session. Every other write in every suite runs on a session that never stepped up, so each of them proves it. The app's fakes have no `stepUp` handler outside the profile: a call to it fails the test.
- **Tests of what's gone are gone.** The app's tests of the inline prompt (a wrong password at a row, its countdown, Cancel, the focus in a stacked editor) tested `StepUpForm` through other screens; the profile's own tests cover the form. The dev console's `a_refused_step_up_changes_nothing` has nothing left to test. The pull request lists each.
- **The sweep.** The four rows that showed the inline prompt go with their twelve baselines, and the profile's "Confirm it's you" gets a row, so the form stays under the contrast and focus checks and has a baseline.

## Consequences
**What's easier.** The app asks for the password once, at login. Twenty routes lose a branch, their clients a retry, and the app about a dozen prompt states. P13.1's matrix needs a stepped-up cast for two routes only.

**What a live session can do now, without the password** (it could not from Phase 8 to v0.1.1). Whoever holds one, at the user's unlocked PC or with a stolen token, does whatever the user's role and grants allow until the session ends or is revoked:
- **As the Owner:** make and unmake Admins, disable and enable users, create and revoke invites, change the servers' settings, disable agents and release their bots.
- **As an Admin:** disable and enable Members, invite Members, review Members' links, the servers' settings, agents.
- **With Manage on an account:** delete it with its stored sign-in, share it, change and revoke grants, point its bot at another server, have it send any command, and write commands off the allowlist into modes.
- **An Owner's or Admin's session can mint a login of the attacker's own:** create an invite and register with it (an Owner's: an Admin account). That account outlives the stolen session until someone disables it. It's in the audit log (`invite.create`, `invite.use`, `user.create`) and on the Users and Invites screens, but nothing stops it. This is the sharpest edge of the change.
- **Not undoable:** deleting an account or a mode, refusing a link, disabling an agent. An account is linked again with its Microsoft sign-in; a disabled agent is enrolled again from the host.

**What still holds.**
- The session can't replace its user's password or authenticator, so the user keeps their login, and changing the password ends every other session.
- Every write is authorized by `authorize()`, in the handler and again in its transaction, rate-limited, and audited with its actor and address.
- A session ends 7 days after its last use and 30 days after its login; the session list ends the others; an Admin or the Owner disables a user, which ends every session of theirs.
- The PC's own lock is the defense for an unattended app, as it already was for everything a session could do (threat model, B7).

**What to revisit.**
- P13.7's threat-model review and P13.8's ASVS self-review read this ADR's list as an accepted deviation, or come back with a narrower step-up for the invite if the user wants that edge closed.
- The twin routes and their two codes, whose names now only record history.
- The threat model's B7 section and "Accepted risks" say all of the above.

## Alternatives considered
- **No step-up at all,** the two included. It's the plain reading of "never asks again", and it would let the whole mechanism go: the stamp, the route, the extractor, the code. It lost to the lockout: one unattended minute at an Owner's PC would be enough to own the fleet for good.
- **The current password in the body of the two requests,** instead of the 5-minute stamp. It would also let the mechanism go, and it's what ASVS's wording describes. It changes two DTOs, the client, the bridge and the profile's flows and their tests, in a release that's meant to remove; ADR-0016 weighed the same shape and chose the stamp. It can follow if the user wants the mechanism gone.
- **Keeping the step-up for the writes that can't be undone** (deleting an account, disabling an agent, a role change, an invite). It's the middle the reviewer might prefer, and the invite is the strongest case. The user's decision names every route, so it isn't taken here; it's named above as the thing to revisit.
- **A setting that turns the step-up off.** A switch that weakens a control per installation, silently, and doubles what has to be tested. No.
- **A longer window,** say a day. It keeps the dialog, only rarer, and a stolen session inside the window is as free as without one.
- **Merging the twin routes now.** See "The twin routes stay".
