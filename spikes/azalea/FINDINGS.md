# Findings (Phase 1 azalea spike)

Short results per task. ADR-0008 is the curated record; this file keeps the evidence behind it. All runs used azalea `0.16.0+mc26.1`, `nightly-2026-08-21`, and the dev server from `deploy/compose.dev.yaml` (`itzg/minecraft-server:2026.9.2-java25`, vanilla 26.1, offline mode).

## P1.1 Local test server
- `just mc-up` pulls the pinned image, starts vanilla 26.1 and is healthy after about 40 s on the first run (the server jar is downloaded once per world volume).
- Only `127.0.0.1:25565` is published. RCON listens inside the container; `docker compose … exec minecraft rcon-cli <cmd>` works without knowing the random password.
- `LEVEL_TYPE=minecraft:flat` works. The log shows `ERROR: No key layers in MapLike[{}]` because itzg writes `generator-settings={}`, but the default flat preset is used anyway: grass at y −61 and bedrock at y −64 (checked with `execute if block`).
- **In Git Bash, set `MSYS_NO_PATHCONV=1`** before `docker compose exec … <path>`. Otherwise `/data/…` gets rewritten to a Windows path.
- `cargo run -- join 15`: `AfkBot1` logs in and spawns at `(2.5, -60.0, 5.5)`, and `rcon-cli list` shows `AfkBot1`. After `exit()` the process ends.
