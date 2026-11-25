# 0049. The Paper harness: Paper 26.1.2 and a test plugin, every input pinned, the plugin built offline

- **Status:** Accepted. Decided under ADR-0025; the supply chain is the user's reviewer's note (ADR-0036, M9)
- **Date:** 2026-11-25
- **Related:** Plan.md Phase 16, group A (P16.1, P16.2); [ADR-0036](0036-scripted-modes.md) (M9: the harness; the amendment "What group A measured" for the numbers); ADR-0003 (the pinned Minecraft version), ADR-0011 (the slow tests), ADR-0022 (U12, U13: pins at least 7 days old; the test images); `deploy/docker/paper.Dockerfile`, `harness/paper/`, `deploy/compose.paper.yaml`, `scripts/harness-pins.mjs`, `scripts/paper-clicks.mjs`, `crates/fleet-mc/tests/minecraft/menus.rs`

## Context
Phase 16 lets scripts read inventories and click in menus. Its sell macro and auction scan must run against a server with plugin menus, which the vanilla test server doesn't have: an auction whose listings carry prices and sellers in their lore, a confirm screen, pages, a sell menu, a second player who buys. ADR-0036 (M9) called for a Paper server in the slow suite with a small test plugin, test-only, Java being a new language in the repository, and the user's reviewer asked that it be pinned and built like the rest of the supply chain: Paper's download and the plugin's toolchain (the JDK image, the build tool, every dependency) pinned by checksum, and the plugin built without network from what was fetched and verified before, with a check that fails when a checksum doesn't match. Group A also measures what a click brings (P16.2), on Paper and on the vanilla test server.

## Decisions

### H1. Paper 26.1.2, which speaks 26.1's protocol
**Paper has no build for 26.1 itself,** only for 26.1.1 and 26.1.2 (PaperMC's download API, 2026-11-25). All three are protocol 775 (PrismarineJS's `protocolVersions.json`), azalea 0.16.0's, so a bot of the pinned azalea joins 26.1.2 as it joins 26.1. **Pinned: Paper 26.1.2, build 74** (2026-07-06, channel STABLE), the last build of 26.1.2. A Minecraft-version bump changes it with azalea, the nightly and the vanilla server's `VERSION` (`docs/releasing.md`).

### H2. Every input pinned, and checked before it's used
`deploy/docker/paper.Dockerfile`, built with `harness/paper/` as its whole context:

| Input | Pinned by | Checked by |
|---|---|---|
| The JDK image, `eclipse-temurin:25-jdk-noble` (Temurin 25.0.5+8) | Its multi-arch index digest of 2026-11-12, from docker-library's `repo-info` history: the newest at least 7 days old (every tag of it was rebuilt on 2026-11-20) | Docker, at the pull |
| Paper's jar, `paper-26.1.2-74.jar` | Its SHA-256, as PaperMC's API publishes it | `ADD --checksum`: any other bytes fail the build |
| Mojang's server jar for 26.1.2 | Its SHA-256, as Paper's jar names it in its `META-INF/download-context` | `ADD --checksum` |
| Paper's 104 libraries, `paper-api` among them | 66 are inside Paper's jar, the rest inside Mojang's: both pinned above | The two checksums; Paper's launcher also checks each against the SHA-256 its jar lists (`META-INF/libraries.list`) |
| The plugin's toolchain | `javac` and `jar` of the JDK image above | The image's digest |
| The plugin's one dependency, `paper-api` 26.1.2 build 74 | It's the library Paper's launcher extracted from the pinned jar | As above |

- **Nothing after the two downloads reaches the network.** Paper's launcher patches the server and extracts its libraries with `RUN --network=none` (it finds Mojang's jar where it looks first, with the right SHA-256, and downloads nothing), and the plugin compiles with `RUN --network=none`: had either needed a download, the build would have failed.
- **No build tool,** so no wrapper and no repository: the plugin is four Java files and a `plugin.yml`, compiled by `javac --release 25` with every lint an error but `classfile` (paper-api's classes name JetBrains' annotations, which aren't among the server's libraries and which nothing needs at run time) and packed by `jar` with a fixed date.
- **The check that fails:** Docker's own, at the build. Shown: Paper's SHA-256 with one digit changed stops the build at the download, `ERROR: failed to build: failed to solve: digest mismatch sha256:1d70b1da…: sha256:0d70b1da…`, and no image is made. And `scripts/harness-pins.mjs`, in `just scripts-test` (so `just check`) and first in `just paper-image`, refuses a Dockerfile with an image by tag, a download without `--checksum=sha256:` and 64 hex digits, plain http, a RUN after the downloads without `--network=none`, a fetching tool (curl, wget, a package manager, a build tool, git) or a copy from an image instead of a stage.
- **Test-only, and nothing ships it:** the image is `afkfleet-paper:test`, built here by `just paper-image` (which `just test-slow` runs first), never pushed; no shipped image, binary or bundle holds any of it, and the repository's Rust and npm graphs don't change. The slow tests start it only after `docker image inspect` finds it, so testcontainers never pulls an image of that name.

### H3. The server's settings
`harness/paper/server/`, copied over the server: offline mode, a flat world with its layers written out (Paper reads the default empty setting as an error), no spawn protection, peaceful; **Paper's update check off** (it called PaperMC's API at every start) and **bStats off**, so the server sends nothing out; Bukkit's connection throttle off, since the tests connect several players from one address within seconds; the EULA accepted as the vanilla test server's `EULA=TRUE` does, for an image that's never published. **Every other setting is Paper's default,** the packet limiter among them: what's measured is what a server ships with. RCON is on inside the container with a password drawn at every start (`start.sh`), never published; the tests run commands with the image's `rcon`, a Java client of 76 lines in the plugin's jar, as they run `rcon-cli` in the vanilla one. The container runs as uid 1000, never root.

### H4. The test plugin
`afkfleet-harness` (`harness/paper/plugin/`) builds the menus a server's plugins build, as plain chest screens whose buttons are items, and a small economy:
- **The auction house** (`/ah`, 54 slots): listings in 0 to 44, each the item with "Price: $1,234.50", "Seller: <name>" and "Expires in: 59m 30s" added to its lore; previous page (45), the page (48), expired listings (49), refresh (50), next page (53). A page turn redraws the same screen in place; a listing opens **Confirm Purchase** on the next tick, as most plugins do (never inside the click's own event). `/ah sell <price>` lists the item in hand after **Confirm Listing**; listings expire, and **Expired Listings** gives them back. A purchase checks and removes its listing in one step on the main thread, so a listing is bought at most once.
- **The sell menu** (`/sell`, 54 slots): 0 to 44 take any item, shift-clicks included; Sell (49) pays for what has a price (Turtle Master potions of each kind by default) and leaves the rest; closing gives back what's left.
- **Real items:** listings made with the game's own item syntax, components included, so its potions carry their potion component and its sword and book their enchantments; `/balance` and `/pay`.
- **A log line for every click** (`harness-click`, after every handler, with whether it was cancelled), every drag, open, close and kick, each with the server's tick and `System.nanoTime()`, and every purchase, listing, expiry, collection and sale.
- **The tests' controls,** from the console or RCON only (`harness …`): balances, listings in the game's syntax or `fill <count>` of assorted ones, expiry, `buy <player> <id>` (an online player buys through the confirm button's own code: the second player who buys first), prices, opening a menu for a player, and `reset`.

### H5. A person's own client, optional
`just paper-up` builds the image and starts it on `127.0.0.1:25567` alone (`deploy/compose.paper.yaml`, project `afkfleet-paper`; 25566 is the DoD checks'), `just paper-clicks` sums up the clicks its log holds (how many, the shortest gap, the most in one server tick and in any one second, by kind, never a player's name), and `just paper-down` removes it with its world. It's the user's step, and optional (ADR-0036, M5): the bots' numbers don't wait for it.

### H6. The measurements' own harness
`crates/fleet-mc/tests/minecraft/menus.rs` drives a bot's screens through fleet-mc's test-only App hook (`fault-injection`) at its game ticks, with azalea's own clicks and prediction, and books every screen packet the bot hears and every click it sends with the game tick it came in. Its four `slow_` scenarios run in `just test-slow`, print `MEASURE` lines and assert the facts ADR-0036's amendment rests on; the numbers are there.

## Licences
| Part | Licence | Where it is |
|---|---|---|
| Paper (the server's patches, `paper-api`) | GPL-3.0, with parts contributors released under MIT ("Paper inherits its licensing from upstream projects", Paper's `LICENSE.md`) | Downloaded at the build into a test-only image |
| Paper's launcher (Paperclip), jbsdiff, commons-compress | MIT, BSD, Apache-2.0 (their licence files ship in Paper's jar) | The same |
| Minecraft's server | Mojang's EULA (https://aka.ms/MinecraftEULA) | Downloaded from Mojang at the build and patched there, never redistributed |
| Eclipse Temurin 25 | GPL-2.0 with the Classpath Exception | The image's base, pulled by digest |
| The test plugin and `rcon` | GPL-3.0-or-later, the repository's | `harness/paper/` |

The plugin compiles against `paper-api` (GPL-3.0), which the repository's GPL-3.0-or-later takes. Nothing of these reaches a shipped artifact, so `THIRD-PARTY-NOTICES.md` and `deny.toml` don't change.

## Consequences
- **Java is a second language in the repository,** test-only: four files under `harness/paper/plugin/src/`, built only inside the image, checked by `javac`'s lints and by the slow scenarios that use them.
- **`just test-slow` builds one more image:** its two downloads are about 110 MB, and with them cached a build without Docker's layer cache took 8 s here; the image is 977 MB, most of it the JDK.
- **A Minecraft-version bump** changes the two jars, their checksums and the plugin's `api-version` here too.

## Alternatives considered
- **Gradle or Maven with a wrapper and dependency verification.** A wrapper jar or script, a distribution and a plugin graph to pin for a plugin of four files whose one dependency is already inside the pinned server. Rejected for the image's own `javac`.
- **paper-api from PaperMC's Maven repository.** A second download to pin, and an API that could differ from the server's. Rejected: the launcher extracts exactly the one the server runs.
- **itzg's image with `TYPE=PAPER`.** It downloads Paper at every start, unpinned by checksum, and has no JDK to build the plugin. Rejected.
- **A JDK image by tag, or the newest digest.** Every Temurin 25 tag was rebuilt five days before; the 7-day rule (ADR-0022, U13) picks the digest of 2026-11-12.
