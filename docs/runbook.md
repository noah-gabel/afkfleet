# The afkfleet runbook

## About this guide

An operator's guide to running afkfleet on a Linux host with Docker, beside whatever else the host runs: installing it, keeping it running, updating it, backing it up, restoring it and removing it. It names no hosting provider: where a step depends on yours, it says so in general terms.

- **Read it at your release's tag.** Each release's page links this guide as it was for that release (`…/blob/vX.Y.Z/docs/runbook.md`), so its commands match the files you unpacked.
- **What your friends do** in the app (installing it, connecting, registering with an invite, 2FA) is in [Using the afkfleet app](using-the-app.md): hand them that link.
- **Every setting and its default** is in `env.example` (in the release's files) and, for the server's and the agents' own keys, in the repository's README ([Running the server](../README.md#running-the-server), [Running the agent](../README.md#running-the-agent)).
- **Every step says what success looks like.** A command whose block isn't marked otherwise is run as written by afkfleet's own tests; a few steps touch what no test can reach (your firewall, your DNS, your PC), and their blocks say so.

The design behind it is [ADR-0022](adr/0022-deployment-and-phase-12-refinements.md).

## What afkfleet adds to a host

Everything below is afkfleet's own, and [Uninstalling](#uninstalling) removes it again; nothing else on the host changes.

- **A folder** with the release's files and your `.env`: `~/afkfleet` in this guide.
- **A compose project,** `afkfleet`: its containers (the server, the agent, Caddy if you use it), its networks `afkfleet_fleet` (and `afkfleet_edge`), and its volumes `afkfleet_data` (the database), `afkfleet_keys` (the vault key and the CA's key), `afkfleet_ca`, `afkfleet_tls` and `afkfleet_agent-data`. No host networking: nothing joins the host's other networks or services.
- **The backups' folder,** `/srv/afkfleet-backups/backups` by default, owned by the server's user (65532), inside a folder only root changes.
- **The SFTP account** `afkfleet-backup`, if you let your PC pull the backups: the account, its key file and one sshd drop-in.
- **The keys' copies** under `/root`, until you move them off the host.
- **If you use them:** a dynamic-DNS update (a root-only file and a systemd timer) and the firewall openings you made.

Success, once afkfleet runs: the host's other services run as they did before, and nothing on the host is new but what this list names.

Every service runs as the nonroot user (65532) with a read-only root, no capabilities (Caddy keeps the one that lets its binary start), `no-new-privileges`, memory and pids limits without swap, rotated logs and `restart: unless-stopped`. Nothing is published but the proxy path's own ports (and 7443 if you publish it), and the keys' volumes are mounted read-only wherever the server runs.

## Before you start

- **Docker Engine 28.0 or newer with Compose v2.24 or newer,** rootful: `setup.sh` refuses rootless Docker and user-namespace remapping, since the backups' folder must belong to the server's user (65532). It checks all of this and names what it found.
- **Your own account may run Docker:** this guide runs `docker` and `setup.sh` as your user, which must be in the `docker` group. That group is root-equivalent (whoever is in it controls the host), so add only your own account, then log in again; or run each of those commands with `sudo` instead. Success: `groups` lists `docker`.
  > **Not run by afkfleet's tests:** it changes the host's users and groups.
  ```sh
  sudo usermod -aG docker <you>
  ```
- **`curl`, `sha256sum` and `tar`** on the host, for the install and updates.
- **A domain name** pointing at the host's IPv4 address (an A record) if afkfleet's own Caddy gets the HTTPS certificate: Let's Encrypt checks it over port 80. afkfleet listens on IPv4 alone. A free dynamic-DNS subdomain works ([The host](#the-host)).
- **The host's clock in sync** (NTP; [Time](#time)).
- **An SSH key on your PC** if it's to pull the backups ([Keeping backups off the host](#keeping-backups-off-the-host)).
- **Disk:**
  - the database: chat takes about bots × `max_lines_per_bot` lines, plus up to 120,000 lines that can come in between two retention runs; a line takes about 0.3 KB, at worst about 4.7 KB, so 20 bots at the default need about 300 MB, at worst about 5.3 GB, plus SQLite's write-ahead log. The file never shrinks by itself: SQLite reuses freed pages;
  - the backups: (`backup_keep` + 1) copies of the database, plus the write-ahead log's peak while a backup runs;
  - a lowered `max_lines_per_bot` or chat `retention_days` applies at the first retention run after the restart, but the file only shrinks with a `VACUUM`;
  - a full disk makes every retention run and every backup fail, since a delete needs room in the write-ahead log: watch the free space ([Metrics](#metrics)).

Success: [Installing](#installing)'s `check` says `ok: Docker <version> and Compose <version>`, an Engine of 28.0 or newer; on an older one, or rootless Docker, it refuses and names what it found.

## The host

### The firewall

Open TCP 80 and 443 in your provider's firewall and the host's, for afkfleet's Caddy (Let's Encrypt needs 80): they're meant to be open to everyone. With an existing proxy, it's your proxy's ports instead. 8080, 9480 and 9481 are the API behind the proxy and the metrics: afkfleet never publishes them, so no firewall needs to open them.

**Docker's published ports go around the host's own firewall.** Docker writes its own packet-filter rules for each port it publishes, ahead of ufw's and firewalld's, so a ufw or firewalld rule doesn't limit them. That matters for 7443, which agents on other hosts dial ([Agents on other hosts](#agents-on-other-hosts)) and nobody else should reach. Limit it to the agents' addresses:
- in your provider's firewall, in front of the host: 7443 allowed from the agents' addresses alone;
- or in Docker's `DOCKER-USER` chain, which Docker checks first and leaves to you: for example a rule that drops 7443 from every address but one agent's (the same with `ip6tables` if the host has IPv6; keep it across reboots your distribution's way). Success: the chain lists it (`iptables -L DOCKER-USER -n`, as root), and 7443 doesn't answer from another address;
- and, on a host with several addresses, publish 7443 on one alone: `AFKFLEET_GRPC_PUBLISH=192.0.2.10:7443` in `.env`.

> **Not run by afkfleet's tests:** it changes the host's packet filter.
```sh
sudo iptables -I DOCKER-USER -p tcp -m conntrack --ctorigdstport 7443 --ctdir ORIGINAL ! --ctorigsrc 192.0.2.20 -j DROP
```

`setup.sh`'s `check` says whether 80 and 443 are free on the host, but free on the host isn't reachable from outside: only your provider's firewall decides that.

### A domain that follows the host's address

If the host's public address changes and your domain comes from a dynamic-DNS service, keep its record in step from the host. The service's update URL holds a token: whoever has it can point your domain elsewhere and get a certificate for it, and read every login, passwords and 2FA codes included. So the token lives in one root-only file and never on a command line, in a shell's history or in this folder.

> **Not run by afkfleet's tests:** they change the host's systemd units and your DNS record.
```sh
sudo install -d -o root -g root -m 700 /etc/afkfleet-dns
sudoedit /etc/afkfleet-dns/update.curl
```

In the editor, write curl's settings, your service's update URL as its documentation gives it:

```ini
url = "https://dyndns.example.com/update?hostname=fleet.example.com&token=<your token>"
```

Save it; `sudoedit` leaves it root's. Then the unit, `/etc/systemd/system/afkfleet-dns.service`, which reads the file as a credential of its own, as a user that exists only while it runs:

```ini
[Unit]
Description=afkfleet's dynamic-DNS record
Wants=network-online.target
After=network-online.target

[Service]
Type=oneshot
DynamicUser=yes
LoadCredential=update:/etc/afkfleet-dns/update.curl
ExecStart=/usr/bin/curl --config %d/update --ipv4 -sS --fail --max-time 30
```

and its timer, `/etc/systemd/system/afkfleet-dns.timer`, every 5 minutes:

```ini
[Unit]
Description=afkfleet's dynamic-DNS record, every 5 minutes

[Timer]
OnBootSec=1min
OnUnitActiveSec=5min

[Install]
WantedBy=timers.target
```

> **Not run by afkfleet's tests:** they change the host's systemd units and your DNS record.
```sh
sudo systemctl daemon-reload
sudo systemctl enable --now afkfleet-dns.timer
sudo systemctl start afkfleet-dns.service
sudo journalctl -u afkfleet-dns.service -n 5
```

Success: the journal shows the service ending with `Deactivated successfully` after your service's own answer (often `good` or `nochg`). A failure shows curl's error, such as `The requested URL returned error: 401`: the token or the URL is wrong; fix the file with `sudoedit` again. Without systemd, root's crontab can run the same every 5 minutes, one line added in its editor:

> **Not run by afkfleet's tests:** they change the host's crontab and your DNS record.
```sh
sudo crontab -e
```

```text
*/5 * * * * /usr/bin/curl --config /etc/afkfleet-dns/update.curl --ipv4 -sS --fail --max-time 30 >/dev/null
```

### Time

The server's clock decides when an agent's certificate has expired, TLS checks certificates against the real time on both ends, Minecraft sessions are granted for minutes, and 2FA codes are 30-second steps: keep NTP on, on this host and on every agent's host. "a granted session expires within the connect timeout" in an agent's log means its clock or the server's is off by minutes.

### The disk

The database holds chat in plain text; `secure_delete` overwrites only what SQLite itself deletes. Encrypt the disk under Docker's volumes on the host if you can.

## Choosing a proxy path

`COMPOSE_FILE` in `.env` picks how HTTPS reaches the server. Keep one line:

```ini
# afkfleet's own Caddy on 80 and 443, with a Let's Encrypt certificate for AFKFLEET_DOMAIN:
COMPOSE_FILE=compose.yaml:compose.edge.yaml:compose.caddy.yaml
# An existing reverse proxy in a container, which joins afkfleet_edge at AFKFLEET_PROXY_ADDRESS:
COMPOSE_FILE=compose.yaml:compose.edge.yaml
# An existing reverse proxy on the host, which connects to 127.0.0.1:AFKFLEET_HTTP_PORT:
COMPOSE_FILE=compose.yaml:compose.loopback.yaml
```

The server believes `X-Forwarded-For` from that path's proxy alone. `setup.sh`'s `check` says whether 80 and 443 are free here. Success: [Installing](#installing)'s `check` names the path you kept, such as `ok: .env: the caddy proxy path`.

### An existing proxy's duties

With your own proxy, it does what afkfleet's Caddy would:

- **TLS** for your domain, with a certificate the friends' Windows trusts (or a CA you hand them: [First login and inviting friends](#first-login-and-inviting-friends)).
- **Only these routes** to the server's port 8080: `/api/v1/*` (WebSocket upgrades included: `/api/v1/events`), `/health/live` and `/health/ready`. Everything else 404. Never route a metrics port (9480, 9481).
- **`X-Forwarded-For`** set to the client's address, never passed on from the client.
- **HSTS** (`Strict-Transport-Security: max-age=31536000`).
- **No `ticket` and no `q` in its logs:** `/api/v1/events?ticket=…` carries a single-use secret, and `q` on `…/grant-candidates` is what a user typed. Drop both from logged URIs, or log no query strings at all.
- **From a container:** it joins the network `afkfleet_edge` at `AFKFLEET_PROXY_ADDRESS` (`172.30.1.2` by default), the one address the server trusts. **On the host:** it connects to `127.0.0.1:8080` (`AFKFLEET_HTTP_PORT`), and the server trusts the fleet network's gateway.

## Installing

About eleven commands from a host with Docker, each with what success looks like. Afterwards, [First login and inviting friends](#first-login-and-inviting-friends).

1. **A folder for afkfleet.** Success: no output; the prompt shows `~/afkfleet`.
   ```sh
   mkdir ~/afkfleet
   cd ~/afkfleet
   ```
2. **The release's files** and their checksums, from the release's page (put its version for `X.Y.Z`). Success: both files are there (`ls` lists them).
   > **Not run by afkfleet's tests:** they download a release from GitHub, which tests can't reach; `setup.sh`'s `fetch` downloads from the same addresses, and its tests pin them.
   ```sh
   curl --proto '=https' --tlsv1.2 -fLO https://github.com/noah-gabel/afkfleet/releases/download/vX.Y.Z/afkfleet-deploy-X.Y.Z.tar.gz
   curl --proto '=https' --tlsv1.2 -fLO https://github.com/noah-gabel/afkfleet/releases/download/vX.Y.Z/SHA256SUMS
   ```
3. **Check, unpack, and start your settings.** Success: the first line prints `afkfleet-deploy-X.Y.Z.tar.gz: OK`; the folder then holds `compose.yaml`, `setup.sh`, `RELEASE` (`version=X.Y.Z`) and the rest; and `.env` exists.
   ```sh
   grep ' afkfleet-deploy-X.Y.Z.tar.gz$' SHA256SUMS | sha256sum -c -
   tar -xzf afkfleet-deploy-X.Y.Z.tar.gz
   cp env.example .env
   ```
4. **Fill in `.env`** with an editor: the proxy path ([Choosing a proxy path](#choosing-a-proxy-path)), `AFKFLEET_DOMAIN`, `AFKFLEET_ACME_EMAIL` for Let's Encrypt, and anything else you change ([Settings](#settings)). The images need no line: `RELEASE` names this release's, pinned by digest. Success: no line of `env.example`'s placeholders (`example.com`) is left.
5. **Check, then install.** `check` changes nothing and says which proxy path fits; `init` (root only to make the backups' folder for the server's user) checks Docker, `.env` and the files, makes the vault key and the PKI inside the server's image, starts the server, enrolls the local agent and starts the rest. It prints the CA's fingerprint, never a key, and never overwrites anything. Success: `check` ends with `the project afkfleet isn't installed yet`, and `init` with `ok: afkfleet is running` and the next steps. On a failure, it names what it created.
   ```sh
   sh setup.sh check
   sudo sh setup.sh init
   ```
6. **The Owner,** the one account that can do everything: it asks for the password twice. Success: `the Owner "<name>" is created`.
   > **Not run by afkfleet's tests:** it asks for the password at a terminal, which tests don't have, so they run the same command with `--password-stdin`.
   ```sh
   docker compose exec server /usr/local/bin/afkfleet-server user create-owner --config /etc/afkfleet/server.toml --username <name>
   ```
7. **The keys' copy,** before anything else: without it, losing the host loses every linked account ([The keys' copy](#the-keys-copy)). Success: `ok: the keys' copy is in /root/afkfleet-keys-…` and the file names.
   ```sh
   sudo sh setup.sh keys-backup
   ```
8. **Your PC's pull of the backups** ([Keeping backups off the host](#keeping-backups-off-the-host)). Success: `ok: the account afkfleet-backup reads … over SFTP, read-only`.
   ```sh
   sudo sh host/sftp-backups.sh install --key /path/to/your_key.pub
   ```

## First login and inviting friends

1. In the app (installed by [Using the afkfleet app](using-the-app.md)), connect to `https://<your domain>`, log in as the Owner, and set up 2FA. Success: Home opens.
2. **Invites:** under **Admin › Invites**, make one per friend and send it with the address. An invite works once and expires. Success: its code shows this once, and the list shows it pending until the friend registers.
3. **A server without a public domain:** if your HTTPS certificate isn't from a public certificate authority (your own proxy's own CA, say), hand your friends that CA's certificate as a file, and its SHA-256 fingerprint another way (in person, or in another chat). Never hand out `ca.crt` from afkfleet's `ca` volume: that's the agents' CA, not HTTPS's. The friends' guide tells them to compare the fingerprint before they log in. Success: openssl prints one line, `sha256 Fingerprint=` and the fingerprint you hand over.
   > **Not run by afkfleet's tests:** the CA is your own proxy's, not afkfleet's.
   ```sh
   openssl x509 -in your-https-ca.crt -noout -fingerprint -sha256
   ```

## Settings

Every setting is a line in `.env`, which an update never replaces. Never edit a file of the release: `fetch` refuses a release file changed here. `env.example` lists every line with its default:

- the proxy path, the domain and the Let's Encrypt email;
- the backups' folder (`AFKFLEET_BACKUPS_DIR`) and how many backups to keep, how often (`AFKFLEET_SERVER__DATABASE__BACKUP_KEEP`, `AFKFLEET_SERVER__DATABASE__BACKUP_INTERVAL_HOURS`);
- chat's retention (`AFKFLEET_SERVER__CHAT__RETENTION_DAYS`, `AFKFLEET_SERVER__CHAT__MAX_LINES_PER_BOT`);
- the limits (`AFKFLEET_SERVER_MEMORY`, `AFKFLEET_AGENT_MEMORY`, …; memory never swaps), and the networks' subnets if another network on the host has `172.30.0.0/24` (`AFKFLEET_FLEET_SUBNET`, `AFKFLEET_EDGE_SUBNET`);
- any key of the server's or the agent's config, as `AFKFLEET_SERVER__<SECTION>__<KEY>` or `AFKFLEET_AGENT__…`: an unknown one stops the start, so a typo shows.

What `.env` can't express (another network for the agent, say) goes into a `compose.override.yaml` of your own beside it, added last to `COMPOSE_FILE` (`…:compose.override.yaml`); updates never touch it either. After a change, apply it. Success: it ends without an error, every container started and the server and the agent `Healthy`.

```sh
docker compose up --detach --wait
```

## Updating

Two commands. `fetch` downloads a newer release's files and checks them (the sums, every name, that no file of the release was changed here), then puts them in place; it never touches `.env`, `compose.override.yaml` or `ca.crt`, and refuses an older release or one that needs a newer install first. `update` takes a backup with the running server, pulls the new images and starts the server before the rest; it refuses while the running server is newer than the folder's release. Success: `fetch` ends with `ok: this folder holds release X.Y.Z's files` and `update` with `ok: afkfleet is updated`.

```sh
sh setup.sh fetch X.Y.Z
sh setup.sh update
```

- **The server goes first,** then its agents: [Agents on other hosts](#agents-on-other-hosts) update after the server's host, with the same two commands.
- **An agent older than its server's modes** can't read a schedule it doesn't know (`on_respawn`, say): its stream ends as `unreadable` and reconnects in a loop, logging "a message from the server couldn't be read" with the field, while its bots keep their old modes. Update the agent.
- **Going back** is a restore, never an older `fetch`: before an update that adds a mode feature, a backup comes first anyway (update takes it). A server rolled back past a feature reads such modes as corrupt: switch the bots off them, or delete them, before you restore an older backup.
- **When `fetch` stops halfway** it leaves `.afkfleet-fetch` for a look and says why; `RELEASE` still names the old release. Fix what it named, then remove the folder and fetch again. Success: as above.
  ```sh
  rm -r .afkfleet-fetch
  sh setup.sh fetch X.Y.Z
  ```

## Backups

The server backs up its database by itself, `backup_keep` copies (14 by default), one every `backup_interval_hours` (24 by default), at most 720 hours between them.

**What a backup is:**
- One SQLite file, `afkfleet-backup-YYYYMMDDTHHMMSSZ.db`, named for the second it was taken (UTC), in the backups' folder. While it's being written it's the dot file `.afkfleet-backup-YYYYMMDDTHHMMSSZ.db.partial`: never copy that one.
- A consistent copy (`VACUUM INTO`), checked before it counts (this server's migrations, and SQLite's `quick_check`), readable only by the server's user, and never written over another file.

**What a backup holds:**
- Every secret only sealed: TOTP secrets and Microsoft refresh tokens under the vault's keys, passwords as Argon2id hashes, session tokens as SHA-256 hashes.
- In plain text: usernames, the audit log (with the addresses users came from) and the bots' chat. Chat the retention deleted lives on in older backups for up to `backup_keep` × `backup_interval_hours`. Treat a backup as private data.
- **Never the keys.** The vault key and the CA's key live on their own volume: no backup and no copy of the database's volume holds the key that opens it. [The keys' copy](#the-keys-copy) keeps them apart.

**Taking one now,** beside the running server. Success: it prints the new backup's path, `/backups/afkfleet-backup-….db` (the file of that name in the backups' folder). Exit 3 means it was written but older ones couldn't be removed.

```sh
docker compose exec -T server /usr/local/bin/afkfleet-server backup --config /etc/afkfleet/server.toml
```

**When backups fail,** the server logs `a backup failed; the next try is in an hour` at `error`, with why (a full disk, a folder that's missing or not writable), and [Metrics](#metrics)' last-success time stops moving. The backups' folder must be on a file system with hard links (not FAT or exFAT, not every network share): a finished backup is published by one.

## Keeping backups off the host

Your PC pulls the backups over SFTP, through an account on the host that can list and read them and do nothing else. Nothing on the host holds a credential for anywhere else, and no third party is involved. The pull deliberately doesn't carry the keys: [The keys' copy](#the-keys-copy) does that apart.

**On the host,** once, from the folder that holds `compose.yaml`, with your PC's public key copied over (one line, such as `id_ed25519.pub`):

```sh
sudo sh host/sftp-backups.sh install --key /path/to/your_key.pub
```

- It adds the account `afkfleet-backup` with the server's user's uid and gid (65532), the only ones that can read the owner-only backups; no shell, no home, no password. Your key goes into `/etc/ssh/afkfleet-backup.authorized_keys`, marked `restrict`.
- It adds the drop-in `/etc/ssh/sshd_config.d/10-afkfleet-backup.conf`: for that account alone, keys only, confined to the backups' folder's parent, read-only SFTP, no TTY and no forwarding. It never edits `sshd_config`.
- It refuses (exit 1, saying why, changing nothing) when a tool or a way to reload sshd is missing, when `sshd_config` doesn't include the drop-in folder above every `Match` block, when the backups' folder or a folder above it isn't root's alone, when the name, uid or gid is taken, when its files exist already, or when the key file holds anything but one key line.
- If a step fails after it wrote something, it removes exactly what it added, so the host is as it was. A backups' folder other than the default is given with `--backups /path/to/backups`.
- Success: `ok: the account afkfleet-backup reads … over SFTP, read-only`, then this host's key fingerprint. Keep your own SSH session open until a new login works.

`install` checks the account's settings for one test address. If an earlier sshd setting is tied to addresses (a `Match Address` block), check your PC's own address too. Success: `chrootdirectory` names the backups' folder's parent, then `forcecommand internal-sftp -R -d /backups`, `permittty no`, `allowtcpforwarding no` and `disableforwarding yes`; anything else means the earlier block wins for your address: change it, or uninstall.

```sh
sudo sshd -T -C user=afkfleet-backup,host=<your PC's name>,addr=<your PC's address> | grep -E '^(chrootdirectory|forcecommand|permittty|allowtcpforwarding|disableforwarding) '
```

**On your PC** (Windows' built-in OpenSSH client, or any other), compare the host key's fingerprint with the one `install` printed at the first connection, then list and copy. Never copy a `.….db.partial` file: it's still being written. Writing, deleting, a shell or a tunnel is refused. Keep the copies as private as the backups.

```sh
sftp -i ~/.ssh/your_key afkfleet-backup@<your host>
```

```text
sftp> ls
sftp> get afkfleet-backup-20261101T000000Z.db
```

**Removing the account:** it removes the account first, then the drop-in and the key file, reloads sshd, and keeps the folders and the backups. When the account can't be removed (it's logged in), it says so and removes nothing: end its sessions and run it again. Success: `ok: the account, its key and the drop-in are gone; the backups' folders stay`.

```sh
sudo sh host/sftp-backups.sh uninstall
```

## The keys' copy

The vault key opens every TOTP secret and Microsoft token in the database; the CA's key and certificate are what every agent trusts. They live on the `keys` and `ca` volumes alone, never in a backup. Lose the host without a copy and every linked account must be linked again, and every agent enrolled again.

**Making the copy:** a root-only folder of root-only files, `/root/afkfleet-keys-<UTC time>` by default (or a folder you name, which must not exist, under folders only root can change, never the backups' folder). It prints the file names, never a key. Success: `ok: the keys' copy is in /root/afkfleet-keys-YYYYMMDDTHHMMSSZ: keys/ca.key keys/vault-key-1 ca/ca.crt`.

```sh
sudo sh setup.sh keys-backup
```

Make a new copy after every [vault key rotation](#rotating-the-vault-key).

**Getting it off the host,** through your own SSH login (never the backups' account), as one file that exists only for the transfer. On the host, a folder and a file that are yours alone before anything is written (mode 0700 and 0600), then the copy into it and its SHA-256. `sudo` asks for your password on the session's terminal; the copy goes only into the file, never to the terminal or a command line. Success: the first three print nothing, and `sha256sum` prints one hash and the file's name.

> **Not run by afkfleet's tests:** they move the keys between your own SSH login on the host and your PC.
```sh
install -d -m 700 ~/afkfleet-keys-out
install -m 600 /dev/null ~/afkfleet-keys-out/afkfleet-keys.tar
sudo tar -C /root -cf - afkfleet-keys-YYYYMMDDTHHMMSSZ > ~/afkfleet-keys-out/afkfleet-keys.tar
sha256sum ~/afkfleet-keys-out/afkfleet-keys.tar
```

On your PC, in PowerShell, fetch it over the same login and take its SHA-256. Success: `scp` shows the file at 100%, and `Get-FileHash` prints the same hash as the host's `sha256sum` (capital or small letters don't matter). If they differ, fetch it again; never go on with a copy whose hash differs.

> **Not run by afkfleet's tests:** they move the keys between your own SSH login on the host and your PC.
```powershell
scp <you>@<your host>:afkfleet-keys-out/afkfleet-keys.tar .
Get-FileHash afkfleet-keys.tar
```

Only then, on the host, remove the file and its folder. Success: no output, and the folder is gone.

> **Not run by afkfleet's tests:** they move the keys between your own SSH login on the host and your PC.
```sh
rm ~/afkfleet-keys-out/afkfleet-keys.tar
rmdir ~/afkfleet-keys-out
```

**Keep it offline,** apart from the backups, as the updater's key is kept: two copies on two offline media (two USB drives in different places, say), never in a cloud folder, an email or a chat. On your PC, copy it to each medium and take each copy's SHA-256 (put each medium's drive for `<first medium>` and `<second medium>`, such as `E:`). Success: both print the host's hash.

> **Not run by afkfleet's tests:** they handle the keys on your PC and its offline media.
```powershell
Copy-Item afkfleet-keys.tar <first medium>\afkfleet-keys.tar
Copy-Item afkfleet-keys.tar <second medium>\afkfleet-keys.tar
Get-FileHash <first medium>\afkfleet-keys.tar
Get-FileHash <second medium>\afkfleet-keys.tar
```

Only once both match, delete the PC's copy: the keys live on the two media alone. Success: no output, and the file is gone.

> **Not run by afkfleet's tests:** they handle the keys on your PC and its offline media.
```powershell
Remove-Item afkfleet-keys.tar
```

The root-only folder on the host can stay; once the offline copies are made, you may remove it as root ([Uninstalling](#uninstalling) gives the command).

**Bringing it back** to a new host, for [Restoring a backup on a new host](#on-a-new-host): the same staging, then the same comparison, before anything else. On the new host, the folder and the file, yours alone (0700 and 0600) before anything is written. Success: no output.

> **Not run by afkfleet's tests:** they move the keys between your own SSH login on the host and your PC.
```sh
install -d -m 700 ~/afkfleet-keys-in
install -m 600 /dev/null ~/afkfleet-keys-in/afkfleet-keys.tar
```

On your PC, the copy from one of the media, over your login to the new host, and its SHA-256. Success: `scp` shows the file at 100%, and `Get-FileHash` prints the hash you noted when it left the old host.

> **Not run by afkfleet's tests:** they move the keys between your own SSH login on the host and your PC.
```powershell
Copy-Item <first medium>\afkfleet-keys.tar afkfleet-keys.tar
scp afkfleet-keys.tar <you>@<new host>:afkfleet-keys-in/afkfleet-keys.tar
Get-FileHash afkfleet-keys.tar
```

On the new host, its SHA-256, before anything else. Success: the same hash as `Get-FileHash` printed (capital or small letters don't matter). If they differ, stop and copy it again.

> **Not run by afkfleet's tests:** they move the keys between your own SSH login on the host and your PC.
```sh
sha256sum ~/afkfleet-keys-in/afkfleet-keys.tar
```

Once they match, the transfer is done: delete the PC's copy (the media keep theirs). Success: no output, and the file is gone.

> **Not run by afkfleet's tests:** they handle the keys on your PC and its offline media.
```powershell
Remove-Item afkfleet-keys.tar
```

Only then, unpack it as root, where `keys-restore` reads it: the files keep their root owner and owner-only modes. Success: no output, and `/root/afkfleet-keys-YYYYMMDDTHHMMSSZ` exists.

> **Not run by afkfleet's tests:** they move the keys between your own SSH login on the host and your PC.
```sh
sudo tar -C /root -xf ~/afkfleet-keys-in/afkfleet-keys.tar
```

Then `keys-restore` ([On a new host](#on-a-new-host)), and right after it remove the staged file and its folder. Success: no output, and the folder is gone.

> **Not run by afkfleet's tests:** they move the keys between your own SSH login on the host and your PC.
```sh
rm ~/afkfleet-keys-in/afkfleet-keys.tar
rmdir ~/afkfleet-keys-in
```

## Restoring a backup

`restore` puts a backup in place of the database, with the server stopped: the database, its `-wal` and its `-shm` move aside under a time stamp, never deleted, so a leftover `-wal` is never replayed onto the backup; then it migrates the backup, checks every secret opens with this host's vault keys, and lists the agents. With a healthy server when it starts, it also disables again every agent you disabled since the backup was taken, and starts afkfleet. Success: `ok: afkfleet runs on afkfleet-backup-….db`, the reminders below, and the `forget-aside` command.

```sh
sh setup.sh restore afkfleet-backup-YYYYMMDDTHHMMSSZ.db
```

- **After a crash** (the server wasn't running), it ends with the server stopped: disable every agent you disabled since the backup ([Incident response](#incident-response), with `maintenance` for `exec server`, as it prints), then start afkfleet. Success: every service healthy.
  ```sh
  docker compose up --detach --wait
  ```
- **When a step after the stop fails** (a backup from a newer server is refused at the migration, and one whose secrets need a vault key this host doesn't configure, from before a key rotation, at the vault check), the server stays stopped, and it says how to put the old database back. Success: `put back: afkfleet.db`, and the server starts again on the old database.
  ```sh
  docker compose run --rm files put-back YYYYMMDDTHHMMSSZ
  docker compose up --detach --wait
  ```
- **What's aside is never deleted halfway:** a restore marks its stamp finished only once the backup is in place and migrated, and `forget-aside` refuses a stamp that isn't, whatever the server's health: after a failed restore, the old database aside may be the only copy. `put-back` marks its stamp finished too, since what's left then is the undone copy.
- **Then, in the app,** redo what was done after the backup was taken: disable users again, end sessions and revoke grants again; whoever changed their password or 2FA since must change it once more (the old ones work again).
- **Agents:** an agent may still run bots the restored database doesn't give it: they're fenced (`bot.stray`) until it removes them. Right after the start nothing new is placed until every enabled agent is back or 90 s have passed. An agent enrolled after the backup isn't in it: it's refused at its next connect (exit 5) and enrolls again.
- **Once all is well,** delete what's aside: without a stamp it lists what's aside; with one, it deletes exactly that stamp's files, only once the stamp is marked finished and while the server is healthy. Success: `ok: what was aside under … is gone`.
  ```sh
  sh setup.sh forget-aside
  sh setup.sh forget-aside YYYYMMDDTHHMMSSZ
  ```

### On a new host

The old host is gone, and you have a backup (pulled by your PC) and [the keys' copy](#the-keys-copy). On the new host, follow [Installing](#installing) up to its step 4 (files, `.env`, with the same settings and the same `AFKFLEET_GRPC_NAMES`), bring the keys' copy back, then:

1. **The keys and the CA,** into new volumes as the server's user, and a server certificate from that CA for this host's names: agents keep their identities. It refuses unless nothing of the project exists yet, and makes the backups' folder as `init` would. Success: `ok: a server certificate from the same CA`, then the next steps.
   ```sh
   sudo sh setup.sh keys-restore /root/afkfleet-keys-YYYYMMDDTHHMMSSZ
   ```
2. **The backup,** staged as the keys' copy is: a folder and a file yours alone (0700 and 0600) before anything is written, the copy from your PC into it, then into the backups' folder as the server's user, owner-only, and the staged copy deleted. On the new host, the folder and the file. Success: no output.
   > **Not run by afkfleet's tests:** they move a backup between your own SSH login on the host and your PC.
   ```sh
   install -d -m 700 ~/afkfleet-backup-in
   install -m 600 /dev/null ~/afkfleet-backup-in/afkfleet-backup-YYYYMMDDTHHMMSSZ.db
   ```
   On your PC, the copy over your login. Success: `scp` shows the file at 100%.
   > **Not run by afkfleet's tests:** they move a backup between your own SSH login on the host and your PC.
   ```powershell
   scp afkfleet-backup-YYYYMMDDTHHMMSSZ.db <you>@<new host>:afkfleet-backup-in/afkfleet-backup-YYYYMMDDTHHMMSSZ.db
   ```
   On the new host, into the backups' folder. Success: no output.
   ```sh
   sudo install -o 65532 -g 65532 -m 600 ~/afkfleet-backup-in/afkfleet-backup-YYYYMMDDTHHMMSSZ.db /srv/afkfleet-backups/backups/
   ```
   Then the staged copy and its folder deleted. Success: no output, and the folder is gone.
   > **Not run by afkfleet's tests:** they move a backup between your own SSH login on the host and your PC.
   ```sh
   rm ~/afkfleet-backup-in/afkfleet-backup-YYYYMMDDTHHMMSSZ.db
   rmdir ~/afkfleet-backup-in
   ```
3. **The restore,** which ends with no server running (there was none). Success: as above, ending with `Then: sudo sh setup.sh init`; start afkfleet no other way here, since `init` enrolls this host's agent.
   ```sh
   sh setup.sh restore afkfleet-backup-YYYYMMDDTHHMMSSZ.db
   ```
4. **The old host's local agent** is still enabled in the backup and holds the name `local`: disable it, which frees the name for this host's. Success: `agent list` shows it, then `disable` says it's disabled.
   ```sh
   docker compose run --rm maintenance agent list --config /etc/afkfleet/server.toml
   docker compose run --rm maintenance agent disable --config /etc/afkfleet/server.toml --id <its id>
   ```
5. **`init`** sees the restored keys, CA and database, makes no key and no PKI, starts the server, enrolls this host's agent and starts the rest. Success: `ok: afkfleet is running`. Then point your domain's record at the new host, and in the app log in as before.
   ```sh
   sudo sh setup.sh init
   ```

## Metrics

The server and the agent answer `GET /metrics` in Prometheus' text format, each on a port of its own, on the project's `fleet` network alone: nothing publishes either, and no proxy reaches them. The server's listens on its fixed address there (`AFKFLEET_SERVER_FLEET_ADDRESS`, `172.30.0.10` by default) port 9480, the agent's on 9481. The `probe` service (Caddy's pinned image and its `wget`, as the nonroot user) reads them from inside. Success: `# HELP afkfleet_…` and `# TYPE …` lines, then a line per series, such as `afkfleet_refresh_token_reuses_total 0`.

```sh
docker compose run --rm probe
docker compose run --rm probe http://agent:9481/metrics
```

A scraper of your own joins the `fleet` network the same way; never give it `edge`, and never publish either port: neither asks for a login. Every series is there from the start, at 0, and starts again from 0 when the process restarts. No label holds a username, an account or player name, an address, an ID, a token or chat, and none is per user, per bot or per agent; every label's values are fixed.

- **The server's port** serves only `/metrics`: another path is 404 and another method 405, with the API's per-IP limit and request timeout. The API's own port answers 404 at `/metrics`.
- **The agent's port** has a small responder of its own: at most 4 connections at once, 5 s each, and a request head of at most 2 KiB. Another path is 404, another method 405, a malformed or oversized request 400. Nothing from a request is logged.

**What to watch,** as examples for alerts:
- **Any refresh-token reuse** (`afkfleet_refresh_token_reuses_total` above 0): a refresh token came back after its rotation, and its session was revoked. It may have been stolen: the audit log's `security.refresh_token_reuse` entry shows whose ([Incident response](#incident-response)).
- **The last backup older than `backup_interval_hours` plus an hour** (`time() - afkfleet_backup_last_success_timestamp_seconds`): backups fail, and the logs say why ([Backups](#backups)).
- **The retention's last success older than 10 minutes** (`afkfleet_retention_last_success_timestamp_seconds`): it runs every minute, so it fails every run or has stopped; `afkfleet_retention_phase_failures_total` names the phase.
- **Agent connect refusals rising** (`afkfleet_agent_connects_refused_total`): an agent with a revoked, expired or unknown certificate keeps trying, or something probes the gRPC port; the `reason` label says which.
- **Logins refused rising** (`afkfleet_rate_limit_refusals_total{limit="login"}`, `afkfleet_login_lockout_refusals_total`): someone is guessing passwords.
- **On an agent,** `afkfleet_outbox_refused_total` rising: its stream to the server keeps ending on a full outbox.

The key metrics (all counters end in `_total`; times are Unix seconds, 0 until the first):

| Metric | Labels | What it shows |
|---|---|---|
| **Server** | | |
| `afkfleet_refresh_token_reuses_total` | | Refresh tokens presented again after their rotation, each revoking its session: any is a possible token theft |
| `afkfleet_rate_limit_refusals_total` | `limit`: `per_ip`, `per_user`, `login`, `refresh`, `register`, `enroll`, `connect`, `bot_actions`, `chat_per_bot`, `chat_per_user`, `grants_by_name`, `grant_candidates`, `ws_connections`, `fresh_sessions` | Requests a rate limit refused |
| `afkfleet_login_lockout_refusals_total` | `reason`: `locked`, `in_flight` | Logins the per-username lockout refused |
| `afkfleet_agent_connects_refused_total` | `reason`: `revoked`, `expired`, `disabled`, `san_mismatch`, `name_mismatch`, `unknown_certificate`, `no_certificate`, `already_connected` | Agent connections refused, past the audit log's daily cap too |
| `afkfleet_agent_identity_conflicts_total` | | Two processes found holding one agent's identity, every time |
| `afkfleet_agent_reports_dropped_total` | | Agent reports about bots the agent doesn't hold |
| `afkfleet_session_grants_total` | `kind`: `online`, `offline`; `fresh`: `true`, `false` | Minecraft sessions granted to agents |
| `afkfleet_session_denials_total` | `reason`: why, one of 22 fixed values | Session requests denied (the fresh-session limit counts as `limit="fresh_sessions"` above) |
| `afkfleet_ingest_chat_dropped_total` | `reason`: `stream_full`, `queue_full`, `closed` | Chat from agents the ingest didn't take |
| `afkfleet_ingest_rows_total` | `outcome`: `stored`, `skipped`, `lost` | The ingest's chat rows |
| `afkfleet_retention_runs_total` | `outcome`: `success`, `failed`, `stopped` | Retention runs, once a minute |
| `afkfleet_retention_phase_failures_total` | `phase`: `sessions`, `invites`, `enrollment_tokens`, `chat_by_age`, `chat_by_size`, `audit` | Retention phases that failed |
| `afkfleet_retention_rows_deleted_total` | `kind`: `sessions`, `rotations`, `invites`, `enrollments`, `chat_by_age`, `chat_by_size`, `audit_entries` | Rows the retention deleted |
| `afkfleet_retention_last_success_timestamp_seconds`, `afkfleet_retention_last_run_duration_seconds`, `afkfleet_retention_last_run_batches` | | When the last successful run ended; the last run's duration and batches |
| `afkfleet_backup_last_success_timestamp_seconds` | | When the newest finished backup in the backups' folder was taken, read at the start and at each check, so the `backup` command's count too |
| `afkfleet_backup_failures_total`, `afkfleet_backup_prune_failures_total` | | Scheduled backups that failed; backups whose pruning failed |
| `afkfleet_backup_last_duration_seconds`, `afkfleet_backup_last_size_bytes` | | The last scheduled backup's duration and size |
| **Agent** | | |
| `afkfleet_bots` | `state`: each bot state | The fleet's bots in each state |
| `afkfleet_bot_reconnects_total`, `afkfleet_watchdog_trips_total` (`kind`: `tick`, `packet`), `afkfleet_actor_restarts_total` | | The bots' own reconnects, the watchdog's trips, the supervisor's restarts |
| `afkfleet_mc_abandoned_threads_total`, `afkfleet_mc_host_threads`, `afkfleet_mc_worlds`, `afkfleet_mc_dropped_chat_total`, `afkfleet_mc_ignored_action_bar_total` | | The Minecraft adapter's numbers, sampled every 5 s |
| `afkfleet_outbox_chat_dropped_total`, `afkfleet_outbox_refused_total`, `afkfleet_outbox_session_requests_dropped_total` | | A managed agent's outbox: chat dropped for newer chat, items refused full (each ends the stream), session requests replaced or dropped unsent; sampled every 5 s |

## Agents on other hosts

An agent on another host dials the server's gRPC port, 7443, over mTLS. **On the server's host,** add `compose.grpc.yaml` to `COMPOSE_FILE` (before any `compose.override.yaml`), and name what remote agents dial, which the server's certificate must hold:

```ini
COMPOSE_FILE=compose.yaml:compose.edge.yaml:compose.caddy.yaml:compose.grpc.yaml
AFKFLEET_GRPC_NAMES=fleet.example.com
AFKFLEET_GRPC_PUBLISH=7443
```

`init` gives these names to the certificate; on an installed host, renew it for them ([Renewing the server certificate](#renewing-the-server-certificate)). Limit 7443 to the agents' addresses as [The firewall](#the-firewall) says: the host's own firewall doesn't limit a port Docker publishes. Then publish the port, and copy the CA's certificate out (it's public: agents pin it). Success: every service healthy, compose saying `Successfully copied` for `ca.crt`, which now lies beside `.env`.

```sh
docker compose up --detach --wait
docker compose cp server:/ca/ca.crt ./ca.crt
```

**On the agent's host,** unpack the same release's files into a folder of their own (`~/afkfleet-agent`, as in [Installing](#installing)'s steps 1–3), put `ca.crt` beside them, and give it to the agent's user once, as root (the agent refuses a CA file others can read). Success: no output, and `ls -n ca.crt` shows `-rw-------` and `65532 65532`.

```sh
sudo chown 65532:65532 ca.crt
sudo chmod 600 ca.crt
```

That folder's `.env` holds the agent's own lines:

```ini
COMPOSE_FILE=compose.agent.yaml
AFKFLEET_AGENT__NAME=agent-2
AFKFLEET_AGENT__CONTROL_PLANE__URL=https://fleet.example.com:7443
AFKFLEET_CA_CERT_FILE=./ca.crt
```

**Enroll it** with a token from the server's host, piped from there, so the bundle exists only in the pipe: it's single-use and expires in 15 minutes. Success: `enrolled as agent <its ID> (agent-2)`, then when its certificate expires. Exit 3 means the server enrolled an agent this host didn't store: disable it on the server's host ([Incident response](#incident-response)) and enroll again with a new token.

> **Not run by afkfleet's tests:** it crosses two hosts over your SSH login; the same token-to-enroll pipe runs in one host's tests.
```sh
ssh <you>@<server host> 'cd ~/afkfleet && docker compose exec -T server /usr/local/bin/afkfleet-server agent create-enrollment-token --config /etc/afkfleet/server.toml' | docker compose run --rm -T --no-deps agent enroll --config /etc/afkfleet/agent.toml
```

Then start it. Success: healthy, and **Admin › Agents** in the app shows it online.

```sh
docker compose up --detach --wait
```

**Updating it,** after the server's host ([Updating](#updating)): the same two commands, in the agent's folder. There `update` takes no backup (the server's host keeps the database): it refuses while a fetch didn't finish or the running agent is newer than the folder's release, pulls the agent's new image and starts it. Success: `ok: this folder holds release X.Y.Z's files`, then `ok: the agent is updated`, and **Admin › Agents** shows it online again.

```sh
sh setup.sh fetch X.Y.Z
sh setup.sh update
```

## An agent next to a local Minecraft server

The agent's bots dial public Minecraft servers and never the host's own networks or its other services. A Minecraft server in a container on this host is on a private network, so its network must be listed for the agent, and the agent must join it:

1. Find that network's subnet, and list exactly it, the narrowest, in `.env`: every network listed opens to every Manage holder of a bot on this agent (the threat model's accepted risk). At most 32 networks, each inside a private range. Success: one subnet in CIDR form, the address and its prefix length, which goes into the list.
   > **Not run by afkfleet's tests:** they need a Minecraft server's network on this host.
   ```sh
   docker network inspect --format '{{range .IPAM.Config}}{{.Subnet}} {{end}}' <its network>
   ```
   ```ini
   AFKFLEET_AGENT__CONTROL_PLANE__PRIVATE_TARGETS=["<its subnet>"]
   ```
2. Let the agent join that network in a `compose.override.yaml` beside `.env`, and add `:compose.override.yaml` to the end of `COMPOSE_FILE`. Success: `COMPOSE_FILE` ends with `:compose.override.yaml`, and the file holds exactly these lines with your network's name:
   ```yaml
   services:
     agent:
       networks: [fleet, minecraft]
   networks:
     minecraft:
       name: <its network>
       external: true
   ```
3. Apply it. Success: the agent healthy; a bot pointed at the server's address comes online. A bot whose last disconnect is `connect_failed` with `blocked` points at an address its agent refuses: the network isn't listed, or another subnet.
   > **Not run by afkfleet's tests:** they need a Minecraft server's network on this host.
   ```sh
   docker compose up --detach --wait agent
   ```

## Renewing the server certificate

The server's certificate, which agents check, lasts 365 days; after that every agent fails to connect. Renew it before (once a year, or after changing `AFKFLEET_GRPC_NAMES`): it stops the server, moves the old pair aside, issues a new one from the same CA for `server` and `AFKFLEET_GRPC_NAMES`' names, and starts afkfleet; if the new one fails, it puts the old one back. The CA stays, so no agent enrolls again; their streams end at the stop and come back. Success: `ok: the server's certificate is new, from the same CA`, then the `forget-aside` command for the old pair, once the agents are back online.

```sh
sh setup.sh renew-cert
sh setup.sh forget-aside YYYYMMDDTHHMMSSZ
```

## Rotating the vault key

A rotation seals every secret again under a new vault key, with the server stopped, for when a key may have leaked or a host was compromised. Each step one command, with `.env` edits between them:

1. **A backup** with the old key, beside the running server. Success: the new backup's path, `/backups/afkfleet-backup-….db`.
   ```sh
   docker compose exec -T server /usr/local/bin/afkfleet-server backup --config /etc/afkfleet/server.toml
   ```
2. **The new key's file,** named in `.env` first, then made inside the server's image (never printed). Success: `created /keys/vault-key-2`.
   ```ini
   AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__2=/keys/vault-key-2
   ```
   ```sh
   docker compose run --rm setup vault create-key --config /etc/afkfleet/server.toml --id 2
   ```
3. **The new key active,** only now (a server recreated before the file exists would refuse to start). Nothing runs yet: the server still uses key 1.
   ```ini
   AFKFLEET_SERVER__VAULT__ACTIVE_KEY_ID=2
   ```
4. **The secrets sealed again,** with the server stopped, then the status: success is key 2 sealing everything, key 1 nothing.
   ```sh
   docker compose stop agent server
   docker compose run --rm maintenance vault rotate-key --config /etc/afkfleet/server.toml
   docker compose run --rm maintenance vault status --config /etc/afkfleet/server.toml
   ```
5. **Start,** then **a new keys' copy:** the old one lacks key 2. Success: every service healthy, then `ok: the keys' copy is in …` naming `keys/vault-key-2`; move it off the host as [The keys' copy](#the-keys-copy) says.
   ```sh
   docker compose up --detach --wait
   sudo sh setup.sh keys-backup
   ```

Key 1 stays configured and on the volume afterwards: `prod/server.toml` names it, and `.env` can't remove a key. That's harmless, since nothing is sealed with it any more. Keep the old keys' copy offline as long as a backup from before the rotation exists: those backups are sealed with key 1.

- **A backup from before the rotation** restores as any other (key 1 is still here), but its secrets come back sealed with key 1: with the server stopped, run step 4's three commands again, then start.
- **On a new host** whose keys' copy predates the rotation, `restore` refuses a newer backup at its vault check (it needs key 2): bring the newer keys' copy instead.
- **Every username's lockout streak** starts again with a new active key, since the lockout's key derives from it.

## When a service won't start or keeps restarting

The first shows each service's state, the second a service's last lines, which say why:

```sh
docker compose ps
docker compose logs --tail 50 agent
```

- **The agent shows `Restarting (5)`:** the server refused it for good (its certificate unknown, revoked or expired, the agent disabled, or its name not the enrolled one). It loops at Docker's backoff, at most once a minute. Stop it, read `refusal` in its log, fix it (disable it on the server's host and enroll it again, or fix its name), then start it.
  ```sh
  docker compose stop agent
  ```
- **The agent exits 1 at the start:** its config or its identity's files (its key, its certificate, the CA): the log line names the file and what's wrong.
- **An agent whose certificate expired** enrolls again: disable it first, which frees its name.
- **The server exits 1 at the start:** its config, the vault's keys, the PKI, an address or the database, named in the log. Exit 4: it, or the reconciler placing the bots, ended unasked; the log before it says why.
- **The server's certificate expired** (after 365 days, every agent fails to connect): [Renewing the server certificate](#renewing-the-server-certificate).

## Bots that don't come online

- **`Failed` with `session_denied`:** the server refused the bot's Minecraft session for good (the account needs linking again, or Microsoft refused it): the server's log line "a session request was denied" names the reason. Fix the account in the app (link it again), then Reset the bot.
- **"a granted session expires within the connect timeout"** on an agent: its clock or the server's is off by minutes ([Time](#time)).
- **"session-token refreshes were still running…"** at a shutdown, then `reauth_required`: a rotated refresh token was lost; link the account again.
- **Backing off while the minute's summary shows `fresh_sessions` refusals:** the bot asked for more fresh sessions than its hourly limit allows.
- **"the server didn't confirm a respawn; retrying"** at `warn`: the Minecraft server ignored a respawn request for 5 s; twelve in a row end the session `respawn_failed`, and the bot joins again.
- **`connect_failed` with `blocked`:** the bot points at an address its agent refuses ([An agent next to a local Minecraft server](#an-agent-next-to-a-local-minecraft-server)).

## Incident response

**People:** in the app, as an Admin or the Owner: disable a user (every session of theirs ends at once), revoke their grants, or end a session from their sessions list. A refresh-token reuse ([Metrics](#metrics)) already ended the session it hit; the audit log's `security.refresh_token_reuse` entry says whose: have them change their password.

**Agents,** on the server's host beside the running server (with the server stopped, `run --rm maintenance` in place of `exec server`): list them, disable a lost or stolen one for good (its certificates are revoked, its stream ends within 10 s), and once its host and its bots are stopped, release its bots, which the server places on other agents within 30 s. Nothing moves them by time.

```sh
docker compose exec server /usr/local/bin/afkfleet-server agent list --config /etc/afkfleet/server.toml
docker compose exec server /usr/local/bin/afkfleet-server agent disable --config /etc/afkfleet/server.toml --id <its id>
docker compose exec server /usr/local/bin/afkfleet-server agent release --config /etc/afkfleet/server.toml --id <its id>
```

After an agent was compromised, every account whose bot it held counts as exposed until about 24 hours after its last `bot.unassign`, `bot.detach` or `agent.disable`: the server's "a session was granted" lines narrow it where they're kept. Link those accounts again (their Microsoft tokens rotate).

**All public dialling off** at once (U10): set it to `false` in `.env`, then recreate the agent. Its bots then dial no public Minecraft server until you set it back. Success: compose says the agent is `Recreated`, then `Started`; a bot pointed at a public server then ends with `connect_failed` and `blocked`.

```ini
AFKFLEET_AGENT__CONTROL_PLANE__PUBLIC_TARGETS=false
```

```sh
docker compose up --detach agent
```

**A compromised host:** stop afkfleet, then rebuild elsewhere from a backup and the keys' copy ([On a new host](#on-a-new-host)), rotate the vault key there ([Rotating the vault key](#rotating-the-vault-key)), and have every user change their password and replace their authenticator. Success: every container `Stopped`.

```sh
docker compose stop
```

## Uninstalling

Removes everything afkfleet added ([What afkfleet adds to a host](#what-afkfleet-adds-to-a-host)) and nothing else; never Docker's `system prune`, which would touch the host's other projects. **First make sure you have what you want to keep:** the backups and the keys' copy, off the host.

The project, by its label: its containers, networks and volumes (the database, the keys, the CA, the agent's identity: gone for good). Then the SFTP account. Success: compose says `Removed` for each container, network and volume, then `ok: the account, its key and the drop-in are gone; the backups' folders stay`.

```sh
docker compose down --volumes --remove-orphans
sudo sh host/sftp-backups.sh uninstall
```

Then the release's two images, still from the folder: they were pulled by digest, so they carry no tag, and each goes by the digest reference `RELEASE` names. Success: for each, an `Untagged:` line with its `…@sha256:` reference, then `Deleted:` lines.

```sh
docker image rm "$(sed -n 's/^server_image=\([^:@]*\):[^@]*@/\1@/p' RELEASE)" "$(sed -n 's/^agent_image=\([^:@]*\):[^@]*@/\1@/p' RELEASE)"
```

Then the folders, the keys' copies under `/root` (each by its name) and the dynamic-DNS update if you made one. Success: no output.

> **Not run by afkfleet's tests:** they delete the host's backups, folders and keys' copies.
```sh
cd ~
rm -r ~/afkfleet
sudo rm -r /srv/afkfleet-backups
sudo rm -r /root/afkfleet-keys-YYYYMMDDTHHMMSSZ
sudo systemctl disable --now afkfleet-dns.timer
sudo rm /etc/systemd/system/afkfleet-dns.service /etc/systemd/system/afkfleet-dns.timer
sudo rm -r /etc/afkfleet-dns
```

Close the firewall openings you made ([The firewall](#the-firewall)).
