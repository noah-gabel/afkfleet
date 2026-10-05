# Security policy

## Reporting a vulnerability
Please **don't** open a public issue, discussion or pull request for a security problem.

Report it privately through GitHub's private vulnerability reporting instead:
1. Open this repository's **Security** tab.
2. Choose **Report a vulnerability**.
3. Describe the problem, how to reproduce it, and what an attacker could do with it.

You'll get an answer in the advisory thread. Once a fix is released, the advisory is published, with credit to you if you want it.

## Supported versions
afkfleet is pre-release software. Only the latest commit on `main` receives security fixes.

## Scope
In scope: everything in this repository, meaning the server, the agent, the desktop app and the deployment files.

Out of scope:
- vulnerabilities in third-party dependencies that are already publicly known (we track them with `cargo deny` and `pnpm audit`)
- Minecraft servers, Minecraft itself, and Microsoft's or Mojang's services
