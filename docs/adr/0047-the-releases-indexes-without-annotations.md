# 0047. The release's indexes without annotations, and its push and merge rehearsed in CI

- **Status:** Accepted. Decided under ADR-0025
- **Date:** 2026-11-22
- **Related:** [ADR-0022](0022-deployment-and-phase-12-refinements.md) (S22, U31: the release workflow and its `merge` job), [ADR-0025](0025-a-simpler-workflow.md); `scripts/release.mjs`, `scripts/release-images-check.mjs`, `docs/releasing.md`

## Context
v0.2.0's release run failed in its `merge` job, after both push jobs had pushed `0.2.0-arm64` and `0.2.0-amd64`:

```
release merge: refused: creating ghcr.io/<owner>/afkfleet-server:0.2.0 failed: ERROR: annotations are not supported for Docker manifest lists; use "oci-mediatypes=true" when building the source images
```

The runner image of 2026-11-16 brought Buildx 0.39.0, whose `imagetools create` reports an annotation target it can't apply instead of silently dropping it (docker/buildx #4091). The push jobs push with `docker push` images `docker build` made, which are Docker-format manifests, and an index of those is a Docker manifest list, which can't carry annotations. So the `--annotation index:…` flags ADR-0022 gave the merge never did anything: the indexes of 0.1.0 to 0.1.3 have none either, while each image's config carries the same labels (`org.opencontainers.image.*`, from the Dockerfiles' build arguments).

Nothing ran the merge before a tag: release.test.mjs answers docker with a stand-in, and the workflow runs only at a pushed tag. A change on the runners reached the release unseen.

## Decisions

### R1. The push and merge are rehearsed in CI, against a registry of the job's own
`scripts/release-images-check.mjs` (`just release-images-check`) runs `release.mjs push` for both architectures and `release.mjs merge`, unchanged, on stand-in images, against a registry it starts on 127.0.0.1. The merge must make and check both indexes, and a second merge must refuse, as the indexes exist.

- **release.mjs stays as it is.** Its steps take their commands through `io.run`, the seam its tests already use. The rehearsal hands them a `run` that sends every `ghcr.io/` argument to `127.0.0.1:<port>/` and maps every answer back, so the steps see GHCR's names and do exactly what they do at a tag. The script that holds the release's token gets no registry setting.
- **In ci.yml,** as the `release-images` job, on the merge job's runner (`ubuntu-24.04`; release-workflow.test.mjs holds them equal), with read access alone, no token and no secret. The release's guard refuses a tag unless CI's own workflow succeeded on that commit's push to main, so a runner change that breaks the merge now blocks the tag before any image is pushed, instead of failing a release halfway.
- **Stand-ins, not the real images.** The merge depends on how the images are pushed, not on what's in them, and the real ones take an hour to build. Each stand-in is one small layer with the release's labels, built from `scratch` with `docker build` for linux/arm64 and linux/amd64, without default attestations, as the release's build jobs build the real ones. Nothing runs in them, so no emulator is needed for arm64.
- **Left out:** `save` and `load`, which only hand the images from the build runners to the push runners (the rehearsal writes the IDs file `save` would); the login; and the warning while a package is private, since a local registry always answers an anonymous pull (release.test.mjs covers it).
- **Linux alone.** The daemon must reach the registry on 127.0.0.1. Docker Desktop's daemon runs in its own VM: on this PC a push to the published port failed with `dial tcp 127.0.0.1:56426: connect: connection refused`, while Buildx, on the host, reached it. So the recipe isn't part of `check`; it's part of `ci`.
- **The registry** is the official `registry:3.1.2` (CNCF distribution), pinned by its multi-arch index digest, the newest release, built on 2026-11-05 (Plan.md §5). Every name is the run's own (`afkfleet-release-images-<pid>-<random>`), and the registry, its volume and every tag it made are removed at its end.
- **It prints the Docker and Buildx versions** it ran with, first.
- **Red first:** its first CI run, with the merge still giving annotations, failed as v0.2.0's release did: Docker 28.0.4 and Buildx v0.39.0 on `ubuntu-24.04`, all four stand-ins pushed, then `release merge: refused: creating ghcr.io/afkfleet-check/afkfleet-server:0.2.0 failed: ERROR: annotations are not supported for Docker manifest lists; …` (its names are GHCR's, standing for the local registry).

### D1. Each index is made without annotations
`docker buildx imagetools create --tag <name>:X.Y.Z <name>@<arm64 digest> <name>@<amd64 digest>`, and nothing else. The checks before and after stay as ADR-0022 set them.

- **Every release so far shipped without them,** and the images' configs carry the same values and more as labels: `org.opencontainers.image.version`, `revision`, `created`, `source`, `licenses`, `title` and `description`, from the Dockerfiles. Nothing a host, the bundle or the app reads changes.
- **It's the smallest change on the release path.** Buildx's suggestion, OCI media types for the source images, would change how the build jobs export the images and how the push jobs push them (`docker push` sends Docker manifests), in the jobs that hold the registry token, for annotations nobody reads.
- **Annotations can come back** with OCI images, in a change of their own that the rehearsal (R1) runs first.

### D2. The merge prints the Buildx it runs with
`release.mjs merge` asks `docker buildx version` first, before any registry call, and prints it as `Buildx: …`. A Buildx that doesn't answer refuses the merge, since nothing after it would work. The line sits in the merge job's log right above anything Buildx refuses, so the next runner change shows where it bites; each job's "Set up job" group already names its runner image's version, whose software list has the rest. It's printed by the script, not by a workflow step, so the jobs that hold the token keep their pinned steps.

### D3. v0.2.0 is never released; v0.2.1 is
v0.2.0's tag stays where it was pushed (docs/releasing.md: a pushed tag is never moved or pushed again), and nothing is released from it. Its `0.2.0-arm64` and `0.2.0-amd64` images stay in the registry, in no index. v0.2.1 carries v0.2.0's code with D1 and D2, so it's the release after v0.1.3, and its notes are v0.2.0's. docs/releasing.md's paragraph on failed runs names it.
