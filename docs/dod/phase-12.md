# Phase 12's definition of done

Phase 12 is done when both checks below pass (Plan.md, Phase 12's DoD; ADR-0022, U6): **(a)** a clean afkfleet install on an existing Linux host that keeps its other services, following the runbook alone; then **(b)** a friend on another Windows PC installs the release's app, registers with an invite and logs in with 2FA over HTTPS against that install, following the friends' guide alone.

- **You alone run it,** on your own host and PCs. The AI never touches the host, its login, its DNS or its keys.
- **Open this file at the release's tag** on GitHub (the release's page links the runbook there), so every link below reaches that release's guides.
- **Each step** links the section it follows, says what success looks like, and leaves a place for its outcome. A step that fails stops the check: note what you saw, in words that hold nothing private, and report it.
- **Report the outcomes** in the pull request or the chat. Never commit a filled-in copy of this file.

**Never write here, or report, the host's name, address, domain or provider, `.env`'s values, a fingerprint, an invite, a username or a password (CLAUDE.md rule 11).**

## The record

- Release: vX.Y.Z
- Date (UTC): ____
- Proxy path: ☐ afkfleet's Caddy ☐ a proxy in a container ☐ a proxy on the host
- Dynamic DNS: ☐ yes ☐ no
- The host's architecture: ☐ arm64 ☐ amd64

## Before the checks

- The release vX.Y.Z is published, with its installer, its host's files (`afkfleet-deploy-X.Y.Z.tar.gz`) and `SHA256SUMS`.
- The host runs its other services as on any day, and you know which, to compare at the end of part (a).
- Nothing of afkfleet is on the host yet: no afkfleet folder, compose project, volume, backups' folder or `afkfleet-backup` account.
- Your PC holds an SSH key for the backups' pull and two offline media for the keys' copy; the friend's PC is a second Windows PC, with an authenticator app on a phone.

## (a) A clean install by the runbook alone

1. **The prerequisites.**
   - **Do:** [Before you start](../runbook.md#before-you-start).
   - **Success:** Docker Engine 28.0 or newer and Compose 2.24 or newer, rootful; `curl`, `sha256sum` and `tar` on the host; the domain's A record points at the host; NTP on.
   - **Outcome:** ☐ pass ☐ fail — notes: ____
2. **The host's firewall, and its domain if the address changes.**
   - **Do:** [The host](../runbook.md#the-host).
   - **Success:** 80 and 443 open in your provider's firewall and the host's, 8080, 9480 and 9481 open nowhere; with dynamic DNS, the update's journal ends `Deactivated successfully`.
   - **Outcome:** ☐ pass ☐ fail — notes: ____
3. **The proxy path,** chosen before the install's step 4.
   - **Do:** [Choosing a proxy path](../runbook.md#choosing-a-proxy-path).
   - **Success:** one `COMPOSE_FILE` line in `.env`; with your own proxy, each of its duties in place; the install's `check` names the path you kept.
   - **Outcome:** ☐ pass ☐ fail — notes: ____
4. **The install.**
   - **Do:** [Installing](../runbook.md#installing), every step in order.
   - **Success:** each step ends as its own Success says: the bundle's sum `OK`, `check` and then `init` ending `ok: afkfleet is running`, the Owner created, `ok: the keys' copy is in …` and the SFTP account's `ok:` line.
   - **Outcome:** ☐ pass ☐ fail — notes: ____
5. **The first login and the friend's invite.**
   - **Do:** [First login and inviting friends](../runbook.md#first-login-and-inviting-friends).
   - **Success:** the Owner logs in with 2FA and Home opens; one Member invite exists for the friend, handed over with the address.
   - **Outcome:** ☐ pass ☐ fail — notes: ____
6. **The backups' pull to your PC,** once the first backup exists (about a minute after the start).
   - **Do:** [Keeping backups off the host](../runbook.md#keeping-backups-off-the-host).
   - **Success:** the `sshd -T` check prints its five lines as the runbook gives them; from your PC, after comparing the host key's fingerprint, `ls` lists a backup and `get` copies it; a `put` or `rm` there is refused.
   - **Outcome:** ☐ pass ☐ fail — notes: ____
7. **The keys' copy, off the host.**
   - **Do:** [The keys' copy](../runbook.md#the-keys-copy), its way off the host (not the way back).
   - **Success:** the SHA-256 on the host and on your PC are the same; the host's transfer folder is gone; the copy is on two offline media.
   - **Outcome:** ☐ pass ☐ fail — notes: ____
8. **The metrics.**
   - **Do:** [Metrics](../runbook.md#metrics), both `probe` commands.
   - **Success:** both print `# HELP afkfleet_…` lines; the server's holds `afkfleet_refresh_token_reuses_total 0`.
   - **Outcome:** ☐ pass ☐ fail — notes: ____
9. **The host's other services.**
   - **Do:** [What afkfleet adds to a host](../runbook.md#what-afkfleet-adds-to-a-host).
   - **Success:** every service the host ran before runs as it did, and nothing is new on the host but what that list names.
   - **Outcome:** ☐ pass ☐ fail — notes: ____

## (b) A friend's login by the friends' guide alone

On the friend's PC, with the friend following the guide and you only handing over the address and the invite.

1. **The app.**
   - **Do:** [Installing the app](../using-the-app.md#installing-the-app).
   - **Success:** the installer's SHA-256 equals its line in the release's `SHA256SUMS`; after Windows' one warning, afkfleet starts and asks for a server.
   - **Outcome:** ☐ pass ☐ fail — notes: ____
2. **The server.**
   - **Do:** [Connecting to a server](../using-the-app.md#connecting-to-a-server).
   - **Success:** the address starts with `https://`; without a custom CA (or with its fingerprints compared first), the login screen opens.
   - **Outcome:** ☐ pass ☐ fail — notes: ____
3. **Registering.**
   - **Do:** [Registering with an invite](../using-the-app.md#registering-with-an-invite).
   - **Success:** the invite is taken and the app goes on to 2FA; the same invite is refused a second time.
   - **Outcome:** ☐ pass ☐ fail — notes: ____
4. **2FA.**
   - **Do:** [Setting up 2FA](../using-the-app.md#setting-up-2fa).
   - **Success:** the authenticator's code is taken, and the recovery codes are shown and kept.
   - **Outcome:** ☐ pass ☐ fail — notes: ____
5. **Logging in.**
   - **Do:** [Logging in](../using-the-app.md#logging-in), after logging out once.
   - **Success:** with the password and a new code, Home opens.
   - **Outcome:** ☐ pass ☐ fail — notes: ____
6. **The version.**
   - **Do:** [Updates](../using-the-app.md#updates): open **Profile › Updates**.
   - **Success:** it reads `Installed: afkfleet X.Y.Z.`, the release's version, and **Check for updates** finds no newer one.
   - **Outcome:** ☐ pass ☐ fail — notes: ____
