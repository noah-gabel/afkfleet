# afkfleet

A self-hosted fleet of **Minecraft Java Edition AFK bots**, one per Microsoft account, run on your own server and managed from a Windows desktop app.

- **Bots stay online by themselves.** They reconnect, heal themselves, and back off when a human logs into the same account.
- **Modes** such as `afk` (anti-AFK-kick) and `farm` (look in one direction and attack or use an item on a schedule), or your own. A bot sitting in a minecart clicks and attacks from its seat, as a player there would.
- **Chat:** read what each bot hears, live, and have it send a line or a command.
- **Shared with friends:** invite-only accounts, roles and mandatory 2FA; each Microsoft account stays its owner's and can be shared.

> **Use responsibly.** Only run the bots on servers whose rules allow AFK bots. afkfleet isn't affiliated with Mojang or Microsoft.

**Status:** v0.1.2 is released: the server, its agents and the Windows app, with the installer, updates and the operators' runbook. Phase 13 (hardening) is under way. Bots speak Minecraft Java Edition **26.1**; servers on newer versions need ViaVersion/ViaBackwards.

## Quickstart
**Using the app** (your admin runs a server and gave you its address and an invite):
1. Download `afkfleet_X.Y.Z_x64-setup.exe` from the [latest release](https://github.com/noah-gabel/afkfleet/releases/latest) and run it (Windows 10 or 11).
2. Enter the server's address, register with your invite code, and set up 2FA.
3. Link your Microsoft account and start your bot from Home.

Step by step, with what to expect: [Using the afkfleet app](docs/using-the-app.md).

**Hosting it** (a Linux host with Docker, and a domain name pointing at it):
1. Download the release's server files, check them and fill in `.env`.
2. Run the setup script: it creates the keys and starts everything. Then create the Owner.
3. Copy the keys off the host, and let your PC pull the backups.
4. Log in from the app and invite your friends.

Every command, with what success looks like: [the runbook](docs/runbook.md#installing). Updates are two commands.

## Guides
- [Using the app](docs/using-the-app.md), for friends.
- [The runbook](docs/runbook.md), for whoever hosts afkfleet.
- [Releasing](docs/releasing.md), for the maintainer.
- [Developing afkfleet](docs/development.md), for contributors. [docs/](docs/README.md) lists every other document.

## Disclosure

afkfleet is **a hobby project** I'm building for my friends with Claude Code. I use Claude to code faster and follow production standards to keep the risk of vulnerabilities low, but the code hasn't been professionally audited: use it at your own risk. A lot of the structure is deliberately overkill for a hobby project; it's also an experiment in how far strict tooling can keep AI-written code safe.

There's no support, and I don't take feature requests. Security reports are welcome, though: see [SECURITY.md](SECURITY.md).

This project is written by AI (Claude Code); its maintainer reviews and merges every pull request.

## License
Copyright (C) 2026 the afkfleet contributors.

afkfleet is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version. See [`LICENSE`](LICENSE).
