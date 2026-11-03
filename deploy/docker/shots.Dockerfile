# The screenshot baselines' image (Plan.md P11.11; ADR-0020, U97, S130).
#
# `just shots` and `just shots-update` build and run it through scripts/shots.mjs, from the
# repository root, which shots.Dockerfile.dockerignore narrows to the frontend's sources:
#   docker build --platform linux/amd64 --file deploy/docker/shots.Dockerfile --tag afkfleet-shots:local .
#
# Its one FROM pins Playwright's own Linux image by its multi-arch index digest, at the version
# apps/desktop/package.json pins @playwright/test to (scripts/shots.test.mjs keeps them equal). A
# Playwright bump moves the tag and the digest in the same pull request, then `just shots-update`
# makes the baselines again. Nothing here is shipped; the image runs with no network.
FROM mcr.microsoft.com/playwright:v1.64.0-noble@sha256:06a9939e57531807f8d5fd76ce44b53165ffb7d7501d87ab10e285c20b1e971f

LABEL org.opencontainers.image.title="afkfleet-shots"
# The shots' config refuses to load without it (apps/desktop/e2e/shots.ts).
ENV AFKFLEET_SHOTS_IMAGE="1"

WORKDIR /app

# pnpm at the root's `packageManager` version, the one the lockfile records.
COPY package.json ./
RUN npm install --global --ignore-scripts "$(node --print "require('./package.json').packageManager")"

# The install, cached until a manifest, the lockfile or a patch changes.
COPY pnpm-lock.yaml pnpm-workspace.yaml ./
COPY patches ./patches
COPY packages/ui/package.json ./packages/ui/
COPY apps/desktop/package.json ./apps/desktop/
RUN pnpm install --frozen-lockfile

# The sources, then the e2e bundle that `vite preview` serves. The mounts' folders belong to the
# image's own user, and Vite's folder for its bundled config takes any user, as /tmp does: on Linux
# the container runs as the host's user.
COPY tsconfig.base.json ./
COPY packages/ui ./packages/ui
COPY apps/desktop ./apps/desktop
RUN pnpm --filter @afkfleet/desktop run e2e:build \
    && mkdir -p apps/desktop/e2e/baselines target/shots/results target/design \
    && chown -R pwuser:pwuser apps/desktop/e2e/baselines target \
    && mkdir -p apps/desktop/node_modules/.vite-temp \
    && chmod 1777 apps/desktop/node_modules/.vite-temp

WORKDIR /app/apps/desktop
USER pwuser
