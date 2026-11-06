# Releasing afkfleet

This is the maintainer's guide: it's for the repository's owner alone, who sets up GitHub, holds the updater's key, tags releases and publishes them. Operators who host afkfleet read [the runbook](runbook.md), and friends who use the app read [Using the afkfleet app](using-the-app.md). The design behind it is [ADR-0022](adr/0022-deployment-and-phase-12-refinements.md) (U31, U32, S22, S24).

A release is three things on one GitHub release page:
- `afkfleet_X.Y.Z_x64-setup.exe`, the Windows installer. It isn't Authenticode-signed (no paid certificate), so Windows warns once at the first install;
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
4. **The images' packages** exist only after the first release run. Then, on your GitHub profile's **Packages** tab, open `afkfleet-server` and `afkfleet-agent` in turn, and in each one's **Package settings**:
   - check that **Manage Actions access** lists this repository with the **Write** role, so later releases can push;
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
1. **The version.** Releases are numbered X.Y.Z. In a pull request of its own, set `[workspace.package] version` in `Cargo.toml` (the app, the server and the agent all take it from there), let `Cargo.lock` follow, and merge it.
2. **Wait for CI on main.** The workflow refuses a tag unless CI succeeded on a push of exactly that commit to main. CI cancels a run when a newer commit lands, so tag the newest commit of main once its CI run is green.
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
   - `image (arm64)` and `image (amd64)`: both images built and tested as the nightly Stack workflow does, then pushed as `X.Y.Z-arm64` and `X.Y.Z-amd64`, never over a tag that exists (an hour or two);
   - `merge`: the `X.Y.Z` index of each image, made from the pushed digests and checked;
   - `installer`: the installer, built with no key; its SHA-256 is in the job's log and summary;
   - `release`: a **draft** release with the installer, `SHA256SUMS` and the body naming both images by digest.

   If a job fails, use **Re-run failed jobs**, never **Re-run all jobs**: a tag that was pushed is never pushed again, so a re-run of a finished image job is refused. If a push failed halfway, its tag stays: fix the cause and release the next patch version instead.
5. **First release only:** make both packages public (the setup's step 4), before you publish.
6. **Download and check the installer.** Open the draft (**Releases**), download `afkfleet_X.Y.Z_x64-setup.exe`, and run `Get-FileHash "$HOME\Downloads\afkfleet_X.Y.Z_x64-setup.exe"` in PowerShell. The hash must equal both the `installer` job's printed SHA-256 and the installer's line in the draft's `SHA256SUMS` (capital or small letters don't matter). If they differ, stop.
7. **Sign it for the updater**, with your offline key attached:
   ```
   just updater-sign <absolute path of afkfleet-updater.key> "$HOME\Downloads\afkfleet_X.Y.Z_x64-setup.exe" <the SHA-256> X.Y.Z
   ```
   It refuses a key inside the repository, a file name that doesn't match the version, a hash that doesn't match, and an output folder that exists. Type the key's password at its hidden prompt. Success: it prints the path of `afkfleet_X.Y.Z_x64-setup.exe.sig` in `target/updater-sign/vX.Y.Z/`, where `latest.json` lies beside it; it has checked that the signature is by the committed public key and names the file and the version. Detach the key's drive.
8. **Upload** `afkfleet_X.Y.Z_x64-setup.exe.sig` and `latest.json` from that folder to the draft's assets (edit the draft, then drop the files on it).
9. **Publish** the release, as the latest release and not a pre-release: installed apps read `releases/latest/download/latest.json`. With immutable releases on, nothing on it can change afterwards.
10. **Check.** On a PC with an older version installed, **Profile › Updates › Check for updates** shows "afkfleet X.Y.Z is available." Operators update their hosts by the runbook.

To rehearse the whole update flow on your PC without the real key, run `just update-rehearsal`: it installs nothing itself, and prints its steps.

## The Minecraft version bump
Comes with P12.6 (group D2), which moves ADR-0003's procedure here.
