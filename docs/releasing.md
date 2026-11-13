# Releasing afkfleet

This is the maintainer's guide: it's for the repository's owner alone, who sets up GitHub, holds the updater's key, tags releases and publishes them. Operators who host afkfleet read [the runbook](runbook.md), and friends who use the app read [Using the afkfleet app](using-the-app.md). The design behind it is [ADR-0022](adr/0022-deployment-and-phase-12-refinements.md) (U31, U32, S22, S24).

A release is four things on one GitHub release page:
- `afkfleet_X.Y.Z_x64-setup.exe`, the Windows installer. It isn't Authenticode-signed (no paid certificate), so Windows warns once at the first install;
- `afkfleet-deploy-X.Y.Z.tar.gz`, the host's files: the compose project, `setup.sh` and `RELEASE`, which pins both images by digest;
- the server's and the agent's images on GitHub's container registry, `ghcr.io/noah-gabel/afkfleet-server:X.Y.Z` and `ghcr.io/noah-gabel/afkfleet-agent:X.Y.Z`, each for linux/arm64 and linux/amd64;
- the updater's files: the installer's signature (`.sig`) and `latest.json`, which installed apps read to find the update.

CI builds the installer and the images and makes a draft release, all without any key. You sign the installer for the updater on your own PC, upload the signature and `latest.json` to the draft, and publish it. The private key never leaves your PC, and neither it nor its password goes into the repository, CI, a command line or any output.

## One-time setup on GitHub
1. **A tag ruleset**, so only you create release tags and none is ever moved or deleted. In the repository's **Settings › Rules › Rulesets**, choose **New ruleset › New tag ruleset**:
   - **Enforcement status:** Active.
   - **Bypass list:** Repository admin.
   - **Target tags:** include by pattern `v*`.
   - **Rules:** Restrict creations, Restrict updates, Restrict deletions, Block force pushes.
2. **Immutable releases**, so a published release's files and tag can't change afterwards. In **Settings › General › Releases**, tick **Enable release immutability**. You add the updater's files to the draft before you publish, so this never gets in the way.
3. **Workflow permissions** stay at **Read repository contents and packages permissions** (**Settings › Actions › General**). The release workflow asks for more per job: `packages: write` for the images, and `contents: write` for its last job alone, which runs no repository code. No repository secret is used.
4. **The images' packages** appear only once the first release run's `push` jobs have finished, and on your GitHub profile, not the repository: on your profile's **Packages** tab, open `afkfleet-server` and `afkfleet-agent` in turn, and in each one's **Package settings** (in the right-hand column of the package's page):
   - check that **Manage Actions access** lists this repository with the **Admin** role (or **Write**), so later releases can push;
   - under **Danger Zone**, choose **Change visibility › Public**, so operators can pull without a login. This can't be undone.

   Until both are public, the workflow's `merge` job warns "can't be pulled without a login yet".

## The updater's key pair
The app installs only updates signed by the key whose public half it carries (`plugins.updater.pubkey` in `apps/desktop/src-tauri/tauri.conf.json`, pinned by `scripts/tauri-hardening.mjs`). The committed public key's ID is `D2C6CEB25F9F2CB0`.

**Generating it** (done once, before the first release; already done for this repository):
1. On your PC, outside this repository, for example on a drive you keep offline, run `pnpm --dir apps/desktop tauri signer generate -w <path>\afkfleet-updater.key`. It asks for a password twice: choose a long one. Never pass `-p`, which would put the password on a command line. If it doesn't ask, stop.
2. It writes `afkfleet-updater.key` (the private key, encrypted with the password) and `afkfleet-updater.key.pub`. Only the `.pub` file's one line goes into the repository.

**Its offline backup:**
- Keep two copies of `afkfleet-updater.key` on two separate offline media, such as two USB drives kept in different places. Never in a cloud folder, an email or a chat.
- Keep the password apart from the key, in your password manager.
- Check once a year that a copy and the password still sign: `just update-rehearsal` proves the flow with a throwaway key, and a real release proves the real one.

**If it's lost** (the file or its password): installed apps can't take another update. A new key pair means a new public key in the app, which only a new installer carries, so every friend downloads and runs the next release's installer by hand once (the friends' guide's "Installing the app"). Say so in that release's notes.

**If it leaks** (someone may have the file and the password): make a new key pair at once and release a version with its public key, which friends install by hand. An old app trusts the leaked key for as long as it runs an old version, but it only installs what this repository's latest release offers, so tags and releases stay yours alone (the tag ruleset above).

## Each release
1. **The version.** Releases are numbered X.Y.Z. In a pull request of its own, set `[workspace.package] version` in `Cargo.toml` (the app, the server and the agent all take it from there), let `Cargo.lock` follow (any cargo command does it: exactly the workspace's own members change), set README's status line to the new version (`just scripts-test` fails while the two differ), and merge it. In the same file, `[workspace.metadata.afkfleet] updates_from` is the oldest release a host may update from to this one with `setup.sh`'s `fetch`: raise it only when this release can't take over an older host's files or volumes, and say so in the release's notes. **For the first release,** the version `Cargo.toml` already holds (`0.1.0`) is the one to tag: nothing to change.
2. **Wait for CI on main.** The workflow refuses a tag unless CI succeeded on a push of exactly that commit to main. CI cancels a run when a newer commit lands, so tag the newest commit of main once its CI run is green. **A tag pushed too early** (CI on main still running) fails the `guard` job: wait until CI on main finishes green, then use **Re-run failed jobs** on the Release run. Never delete or move the tag to try again, even though your admin role could: a release's tag stays where it was pushed, and the re-run checks CI afresh.
3. **Tag it** from an up-to-date main:
   ```
   git switch main
   git pull
   git tag -a vX.Y.Z -m "afkfleet X.Y.Z"
   git push origin vX.Y.Z
   ```
   Success: the **Release** workflow starts in the repository's **Actions** tab.
4. **The workflow** (`.github/workflows/release.yml`) runs, in order:
   - `guard`: the tag is vX.Y.Z, equals the workspace version, is on main, and CI succeeded on it;
   - `build (arm64)` and `build (amd64)`: both images built and tested as the nightly Stack workflow does, then saved by their image IDs (an hour or two). No registry token exists in these jobs;
   - `push (arm64)` and `push (amd64)`: on fresh runners, the saved images loaded and checked, then pushed as `X.Y.Z-arm64` and `X.Y.Z-amd64`, never over a tag that exists;
   - `merge`: the `X.Y.Z` index of each image, made from the pushed digests and checked;
   - `bundle`: the host's files, `afkfleet-deploy-X.Y.Z.tar.gz`, from the tag's `deploy/` with read access alone; its `RELEASE` names both images by their index digests, so a host pulls exactly these;
   - `installer`: the installer, built with no key; its SHA-256 is on the Release run's **Summary** page under "The installer", and in the `installer` job's log;
   - `release`: a **draft** release with the installer, the bundle, `SHA256SUMS` and the body naming both images by digest and linking the runbook at this tag.

   If a job fails, use **Re-run failed jobs**, never **Re-run all jobs**: a tag that was pushed is never pushed again, so a re-run of a finished push job is refused. If a push failed halfway, its tag stays: fix the cause and release the next patch version instead. If the `release` job failed after its draft was made, delete that leftover draft (**Releases**, the draft, **Delete**) before you re-run it: it never makes a second draft for the same tag.
5. **First release only:** make both packages public (the setup's step 4), before you publish.
6. **Download and check the installer.** Open the draft (**Releases**), download `afkfleet_X.Y.Z_x64-setup.exe`, and run `Get-FileHash "$HOME\Downloads\afkfleet_X.Y.Z_x64-setup.exe"` in PowerShell. The hash must equal both the SHA-256 under "The installer" on the Release run's **Summary** page and the installer's line in the draft's `SHA256SUMS` (capital or small letters don't matter). If they differ, stop.
7. **Sign it for the updater**, with your offline key attached:
   ```
   just updater-sign <absolute path of afkfleet-updater.key> "$HOME\Downloads\afkfleet_X.Y.Z_x64-setup.exe" <the SHA-256> X.Y.Z
   ```
   It refuses a key inside the repository, a file name that doesn't match the version, a hash that doesn't match, and an output folder that exists. Type the key's password at its hidden prompt. Success: it prints the path of `afkfleet_X.Y.Z_x64-setup.exe.sig` in `target/updater-sign/vX.Y.Z/`, where `latest.json` lies beside it; it has checked that the signature is by the committed public key and names the file and the version. Detach the key's drive.
8. **Upload** `afkfleet_X.Y.Z_x64-setup.exe.sig` and `latest.json` from that folder to the draft's assets (edit the draft, then drop the files on it).
9. **Publish** the release, as the latest release and not a pre-release: installed apps read `releases/latest/download/latest.json`. With immutable releases on, nothing on it can change afterwards.
10. **Check.** On a PC with an older version installed, **Profile › Updates › Check for updates** shows "afkfleet X.Y.Z is available." Operators update their hosts by the runbook's [Updating](runbook.md#updating). **For the first release,** no older app exists: install this one by the friends' guide ([Installing the app](using-the-app.md#installing-the-app)); then **Profile › Updates** shows "Installed: afkfleet X.Y.Z." and **Check for updates** says "afkfleet is up to date."

To rehearse the whole update flow on your PC without the real key, run `just update-rehearsal`: it installs nothing itself, and prints its steps.

## The Minecraft version bump
afkfleet's bots speak one Minecraft version, azalea's: today Java Edition 26.1 with azalea 0.16.0 and `nightly-2026-08-21` (README's status line, [ADR-0003](adr/0003-azalea-and-pinned-nightly.md)). Moving to a newer one waits for an azalea release, and is one pull request that changes all of the following together, never one alone. The steps keep ADR-0003's numbers, which the other ADRs cite ("ADR-0003's procedure, step 5"). Any change to `deny.toml`, a lint, a CI job or a coverage gate that the bump seems to need is yours to approve, with an ADR, before it lands.

1. **azalea:** pick the new azalea release on crates.io and note its Minecraft version.
2. **Pins:** update `azalea` (and `azalea-chat` and `azalea-language`, which fleet-mc uses directly) in Plan.md §5, `.claude/rules/dependencies.md`, `Cargo.toml` and `spikes/toolchain-check/Cargo.toml`. No member depends on azalea-auth directly; its pin in §5 follows azalea's ([ADR-0018](adr/0018-microsoft-accounts-and-vault-conventions-and-phase-9-refinements.md)).
3. **Nightly:** start with the newest nightly whose `rustfmt`, `clippy` and `llvm-tools-preview` exist for `x86_64-pc-windows-msvc` and `x86_64-unknown-linux-gnu`. If azalea fails to build, bisect back to the newest nightly that works, counting only the azalea error as "bad". Set it in `rust-toolchain.toml`.
4. **Test server:** update `VERSION` (and, if needed, the pinned `itzg/minecraft-server` image) in `deploy/compose.dev.yaml`, and the same pins in fleet-mc's slow-test harness (`crates/fleet-mc/tests/minecraft/harness.rs`), Phase 10's managed check (`crates/fleet-integration/tests/integration/managed_check/minecraft.rs`) and the DoD checks' container (`deploy/compose.dod.yaml`). Each crate's fast `pins` test fails while its copy differs from compose's; fleet-integration's also reads `compose.dod.yaml`.
5. **Verify,** in four groups. Each item names where it comes from, so a failure finds its decision.

   **The builds.**
   - `spikes/toolchain-check` builds on Windows and in a Linux container.
   - `cargo deny` passes against its graph.

   **azalea's API, as fleet-mc uses it.**
   - azalea's `Event` variants against fleet-mc's mapping (`crates/fleet-mc/src/events/map.rs`): a new variant lands in the catch-all and is only logged at `debug` (ADR-0011).
   - fleet-mc's renderer (`crates/fleet-mc/src/events/render.rs`) against azalea-chat's `TranslatableComponent::read` and `FormattedText` rendering; its tests compare the two on ordinary text (ADR-0011).
   - What fleet-mc's connector relies on (ADR-0008 §1, §8, Consequences): `#[doc(hidden)] start_ecs_runner` and its return values, the plugin list it disables plugins from, `RawConnection::write_raw`, and whether `Client::attack` still needs the `attack_raw` workaround.
   - The chat and the command tree (ADR-0020, S2): whether azalea-client's chat handler still sends commands unsigned (`plugins/chat/handler.rs`, "TODO: commands that require chat signing"), and that `ClientboundCommands`, `BrigadierNodeStub`, `NodeType` and `BrigadierParser::Message` keep their shape: fleet-mc's `command_tree` reads them to refuse commands a server would reject. If azalea signs commands, revisit S2 and the real-account check's `/me` step.
   - The respawn check (ADR-0020, S22, S27): azalea-client's Respawn packet handler (`plugins/packet/game/mod.rs`) still removes `Dead` and `HasClientLoaded` together; nothing else removes `Dead`, and nothing else removes `HasClientLoaded` but a disconnect; only `player_loaded_packet` (`plugins/loading.rs`, `GameTick`) adds `HasClientLoaded` back, and `start_ecs_runner`'s loop runs `GameTick` after the frame's `Update`; whether azalea-entity's `add_dead` (`plugin/mod.rs`, `Update`) still puts `Dead` back when the old life's health 0 comes in the Respawn's frame; and every death still writes `DeathEvent` (`player_combat_kill`, and `death_event_on_0_health` in `Update`). fleet-mc's stale-`Dead` App tests (`connector::app`), `slow_actions_scenario` and fleet-integration's `slow_on_respawn` case run on the new version.
   - Riding (ADR-0026): whether azalea-client still has no riding of its own (`set_passengers` and `move_vehicle` empty in `plugins/packet/game/mod.rs`, `is_passenger` hard-coded in `plugins/movement.rs`, the own vehicle's branch commented out in `plugins/interact/pick.rs`); once it rides by itself, fleet-mc's `riding` module goes. Until then: `send_position` still decides by `LastSentPosition` and `PhysicsState::position_remainder` and runs after `PhysicsSystems`, `handle_attack_queued` and `send_sprinting_if_needed`, and `MiningSystems` still run after azalea's pick (`connector::app`'s schedule-order test fails on a pair Bevy would warn about); `update_hit_result_component`, `pick_block`, `get_entities` and `MaybePickableEntityQuery` keep their shape, and `riding/pick.rs` still matches azalea's `pick`; the `Component` derive still resolves `bevy_ecs` as `azalea::ecs`. `slow_riding_scenario` runs on the new version: it also holds the seat's offset against the server's.
   - Transfers and resource packs (ADR-0019, S46, S69): azalea still ignores `ClientboundTransfer` in the configuration and the game state (azalea-client's `plugins/packet/{config,game}/mod.rs`) and still downloads no resource pack (`ClientboundResourcePackPush` only emits `ResourcePackEvent`, and `accept_resource_packs` fetches nothing). The managed agent's SSRF check covers only the address fleet-mc resolves before the connect: if azalea starts following transfers or fetching packs, the same check must cover that dial before the bump merges.
   - The session server's URL (ADR-0020, S133): whether the new azalea-auth still hard-codes it in `sessionserver.rs`; one that can be set reopens a local fake for `online::slow_online_mode_scenario`.

   **Auth and `cargo deny`.**
   - `just deny`: a new azalea crate that depends on azalea-auth is named there, and `deny.toml`'s azalea-auth wrapper list is updated together with a note in ADR-0018.
   - ADR-0012's re-checks: the `rsa` advisory still holds (azalea never decrypts with RSA), the fourteen hickory ignores go once azalea uses hickory 0.26.2 or newer, and the license exceptions move to the new versions after a review.
   - fleet-server's Microsoft adapter (`crates/fleet-server/src/infra/msauth/`) against the new azalea-auth's `auth.rs`: its endpoints, client ID, scope, fields and headers. The adapter sends its own requests with azalea-auth's protocol, so a change there isn't picked up by the bump. Then you run `just test-ms-account` against your own account (the AI never does).

   **The runs.**
   - `just ci` and `just test-slow`, green.
   - `just demo-agent`: an hour with one server restart (ADR-0014). Its `summary.txt` goes into the bump's pull request.
   - `online::slow_online_mode_scenario` depends on Mojang's session server on purpose: when it fails on Mojang's side (an unexpected answer to its garbage token, or the container's `authservers_down`), report the class it saw and run it again later. It never counts as passed, and gets no retry.
   - The Stack workflow runs on that pull request too: the production images on the new toolchain, every proxy path and the runbook's operations.
6. **Record:** ADR-0003's Decision (or a new ADR that supersedes it) with the new azalea, Minecraft version and nightly, and how the nightly was found; ADR-0012's table; README's status line, with its Minecraft version; and, when what operators see changes (the Minecraft version servers must run), the runbook's [Bots that don't come online](runbook.md#bots-that-dont-come-online).
7. **Release it** as any other release ([Each release](#each-release)), with the new Minecraft version in its notes: hosts take it with the runbook's [Updating](runbook.md#updating), and friends' apps need nothing.
