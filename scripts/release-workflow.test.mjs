// Pins .github/workflows/release.yml (Plan.md P12.5; ADR-0022, S22, U31, PR #98's review): the
// tag-only trigger and a queue that never drops a release, each job's permissions and bounds, no
// secret and no environment, the approved actions alone, the guard asking for CI, the build jobs
// testing exactly as the Stack workflow does with no package permission, and every job that can
// write (push, merge, release) and the bundle job, whose file runs as root on hosts, pinned whole:
// its keys, every step word for word, and no build tool.
// The push jobs load the tested images on a fresh runner and run no repository code but
// release.mjs, which they alone check out. The scripts it runs are tested in
// scripts/release.test.mjs and scripts/app-installer.test.mjs.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (file) => readFileSync(path.join(ROOT, file), "utf8");

const workflow = read(".github/workflows/release.yml");
const stack = read(".github/workflows/stack.yml");
const ci = read(".github/workflows/ci.yml");

/** The artifact actions, approved for this workflow alone (ADR-0022, U31). */
const ARTIFACT_ACTIONS = [
  "actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9 # v7.0.2",
  "actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2",
];

/** The job token, however it's spaced. */
const TOKEN = /\$\{\{\s*github\.token\s*\}\}/g;

/** A build tool no job that can write may run. */
const BUILD_TOOL = /\b(npm|npx|pnpm|cargo|just|rustup)\b/;

/** A top-level key's block: its line and every indented line after it. */
function block(text, key) {
  const lines = text.split("\n");
  const start = lines.indexOf(`${key}:`);
  assert.ok(start >= 0, `no top-level ${key}:`);
  const rest = lines.slice(start + 1);
  const end = rest.findIndex((line) => line !== "" && !line.startsWith(" "));
  return [lines[start], ...(end < 0 ? rest : rest.slice(0, end))].join("\n").trimEnd();
}

/** Each job's text under `jobs:`, by name, in order; the comments between jobs left out. */
function jobs(text) {
  const found = new Map();
  let current = null;
  for (const line of block(text, "jobs").split("\n").slice(1)) {
    if (/^ {2}#/.test(line)) {
      continue;
    }
    const name = /^ {2}([a-z][a-z0-9-]*):$/.exec(line);
    if (name !== null) {
      current = name[1];
      found.set(current, []);
    } else if (current !== null) {
      found.get(current).push(line);
    }
  }
  return new Map([...found].map(([name, lines]) => [name, lines.join("\n")]));
}

/** A job's own keys, in order. */
function keys(job) {
  return job
    .split("\n")
    .map((line) => /^ {4}([a-z][a-z-]*):/.exec(line)?.[1])
    .filter((name) => name !== undefined);
}

/**
 * A job key's text: its one line when it has a value (`runs-on: …`), else its block
 * (`permissions`, `env`, …), its line and the deeper lines after it; null without it.
 */
function key(job, name) {
  const lines = job.split("\n");
  const start = lines.findIndex(
    (line) => line === `    ${name}:` || line.startsWith(`    ${name}: `),
  );
  if (start < 0) {
    return null;
  }
  if (lines[start] !== `    ${name}:`) {
    return lines[start];
  }
  const rest = lines.slice(start + 1);
  const end = rest.findIndex((line) => !line.startsWith("      "));
  const body = (end < 0 ? rest : rest.slice(0, end)).filter((line) => !/^\s+#/.test(line));
  return [lines[start], ...body].join("\n");
}

/** A job's steps, each one's text, comment lines left out. Steps are each job's last key. */
function steps(job) {
  const lines = job.split("\n");
  const start = lines.indexOf("    steps:");
  assert.ok(start >= 0, "a job without steps");
  const found = [];
  for (const line of lines.slice(start + 1)) {
    if (line.trim() === "" || /^ {6}#/.test(line)) {
      continue;
    }
    if (line.startsWith("      - ")) {
      found.push([line]);
    } else {
      assert.ok(line.startsWith("        ") && found.length > 0, `a stray line: ${line}`);
      found.at(-1).push(line);
    }
  }
  return found.map((lines) => lines.join("\n"));
}

/** What a step runs: its `run:` line or block, without the key. */
function runOf(step) {
  const lines = step.split("\n");
  const at = lines.findIndex((line) => /^ {6}- run: |^ {8}run: /.test(line));
  if (at < 0) {
    return "";
  }
  return [lines[at].replace(/^ {6}- run: |^ {8}run: /, ""), ...lines.slice(at + 1)]
    .filter((line) => !/^ {8}[a-z-]+:/.test(line))
    .join("\n");
}

const all = jobs(workflow);
const job = (name) => {
  const text = all.get(name);
  assert.ok(text !== undefined, `no ${name} job`);
  return text;
};

/** A step's lines, joined. */
const step = (...lines) => lines.join("\n");

const CHECKOUT = "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1";
const SETUP_NODE = step(
  "      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0",
  "        with:",
  "          node-version: 24",
);
const UPLOAD =
  "      - uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9 # v7.0.2";
const DOWNLOAD =
  "      - uses: actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2";

/** The checkout of release.mjs alone, for the jobs that hold a registry token. */
const CHECKOUT_RELEASE_MJS = step(
  CHECKOUT,
  "        with:",
  "          persist-credentials: false",
  "          sparse-checkout: scripts/release.mjs",
  "          sparse-checkout-cone-mode: false",
);

/** The logout that runs whatever happened before it. */
const LOGOUT_ALWAYS = step(
  "      - name: Log out, whatever happened",
  "        if: always()",
  "        run: docker logout ghcr.io",
);

/** The release's labels for every build in a job, from the guard. */
const RELEASE_ENV = [
  `      AFKFLEET_IMAGE_VERSION: \${{ needs.guard.outputs.version }}`,
  `      AFKFLEET_IMAGE_REVISION: \${{ needs.guard.outputs.revision }}`,
  `      AFKFLEET_IMAGE_CREATED: \${{ needs.guard.outputs.created }}`,
];

/** Both architectures, each on its native runner. */
const MATRIX = step(
  "    strategy:",
  "      fail-fast: false",
  "      matrix:",
  "        include:",
  "          - runner: ubuntu-24.04-arm",
  "            arch: arm64",
  "          - runner: ubuntu-24.04",
  "            arch: amd64",
);

/** The push job, every step word for word. */
const PUSH_STEPS = [
  CHECKOUT_RELEASE_MJS,
  SETUP_NODE,
  step(
    DOWNLOAD,
    "        with:",
    `          name: images-\${{ matrix.arch }}`,
    `          path: \${{ runner.temp }}/images`,
  ),
  step(
    "      - name: Load the tested images and check them, before any login",
    "        env:",
    `          ARCH: \${{ matrix.arch }}`,
    '        run: node scripts/release.mjs load "$ARCH" "$RUNNER_TEMP/images"',
  ),
  step(
    "      - name: Push by architecture (log in, push, log out)",
    "        env:",
    `          REGISTRY_TOKEN: \${{ github.token }}`,
    `          ARCH: \${{ matrix.arch }}`,
    "        run: |",
    '          mkdir "$RUNNER_TEMP/digests"',
    `          printf '%s' "$REGISTRY_TOKEN" | docker login ghcr.io --username "$GITHUB_ACTOR" --password-stdin`,
    '          node scripts/release.mjs push "$ARCH" "$RUNNER_TEMP/images" "$RUNNER_TEMP/digests"',
    "          docker logout ghcr.io",
  ),
  LOGOUT_ALWAYS,
  step(
    UPLOAD,
    "        with:",
    `          name: digests-\${{ matrix.arch }}`,
    `          path: \${{ runner.temp }}/digests/digests-\${{ matrix.arch }}.txt`,
    "          if-no-files-found: error",
    "          retention-days: 7",
  ),
];

/** The merge job, every step word for word. */
const MERGE_STEPS = [
  CHECKOUT_RELEASE_MJS,
  SETUP_NODE,
  step(
    DOWNLOAD,
    "        with:",
    "          pattern: digests-*",
    `          path: \${{ runner.temp }}/digests`,
    "          merge-multiple: true",
  ),
  step(
    "      - id: merge",
    "        name: The indexes, from the digests (log in, create and check, log out)",
    "        env:",
    `          REGISTRY_TOKEN: \${{ github.token }}`,
    "        run: |",
    `          printf '%s' "$REGISTRY_TOKEN" | docker login ghcr.io --username "$GITHUB_ACTOR" --password-stdin`,
    '          node scripts/release.mjs merge "$RUNNER_TEMP/digests"',
    "          docker logout ghcr.io",
  ),
  LOGOUT_ALWAYS,
  step(
    "      - name: The release's body",
    "        env:",
    `          SERVER_DIGEST: \${{ steps.merge.outputs.server_digest }}`,
    `          AGENT_DIGEST: \${{ steps.merge.outputs.agent_digest }}`,
    "        run: |",
    '          mkdir "$RUNNER_TEMP/notes"',
    '          node scripts/release.mjs notes "$AFKFLEET_IMAGE_VERSION" "$SERVER_DIGEST" "$AGENT_DIGEST" > "$RUNNER_TEMP/notes/notes.md"',
  ),
  step(
    UPLOAD,
    "        with:",
    "          name: notes",
    `          path: \${{ runner.temp }}/notes/notes.md`,
    "          if-no-files-found: error",
    "          retention-days: 7",
  ),
];

/** The bundle job, every step word for word (ADR-0022, S23, S32, R25). */
const BUNDLE_STEPS = [
  step(
    CHECKOUT,
    "        with:",
    "          persist-credentials: false",
    "          sparse-checkout: |",
    "            scripts/deploy-bundle.mjs",
    "            /deploy/",
    "            /LICENSE",
    "            /Cargo.toml",
    "          sparse-checkout-cone-mode: false",
  ),
  SETUP_NODE,
  step(
    "      - name: The host's release files",
    "        run: |",
    '          mkdir "$RUNNER_TEMP/bundle"',
    '          node scripts/deploy-bundle.mjs "$VERSION" "$CREATED" "$SERVER_DIGEST" "$AGENT_DIGEST" "$RUNNER_TEMP/bundle"',
  ),
  step(
    UPLOAD,
    "        with:",
    "          name: bundle",
    `          path: \${{ runner.temp }}/bundle/afkfleet-deploy-\${{ needs.guard.outputs.version }}.tar.gz`,
    "          if-no-files-found: error",
    "          retention-days: 7",
  ),
];

/** The release job, every step word for word. */
const RELEASE_STEPS = [
  step(
    DOWNLOAD,
    "        with:",
    "          name: installer",
    `          path: \${{ runner.temp }}/files`,
  ),
  step(
    DOWNLOAD,
    "        with:",
    "          name: bundle",
    `          path: \${{ runner.temp }}/files`,
  ),
  step(
    DOWNLOAD,
    "        with:",
    "          name: notes",
    `          path: \${{ runner.temp }}/notes`,
  ),
  step(
    "      - name: SHA256SUMS",
    `        working-directory: \${{ runner.temp }}/files`,
    "        run: |",
    '          test "$(ls -A | wc -l)" -eq 2',
    `          test -f "afkfleet_\${VERSION}_x64-setup.exe"`,
    `          test -f "afkfleet-deploy-\${VERSION}.tar.gz"`,
    `          sha256sum -- "afkfleet_\${VERSION}_x64-setup.exe" "afkfleet-deploy-\${VERSION}.tar.gz" > SHA256SUMS`,
    "          cat SHA256SUMS",
    '          cat SHA256SUMS >> "$GITHUB_STEP_SUMMARY"',
  ),
  step(
    "      - name: The draft release, never over one that exists",
    `        working-directory: \${{ runner.temp }}/files`,
    "        env:",
    `          GH_TOKEN: \${{ github.token }}`,
    `          GH_REPO: \${{ github.repository }}`,
    "        run: |",
    `          tags="$(gh release list --limit 1000 --json tagName --jq '.[].tagName')"`,
    `          if printf '%s\\n' "$tags" | grep -Fqx "v$VERSION"; then`,
    '            echo "::error::A release v$VERSION exists already, published or draft: nothing is overwritten."',
    "            exit 1",
    "          fi",
    `          gh release create "v$VERSION" --draft --verify-tag --notes-file "$RUNNER_TEMP/notes/notes.md" "afkfleet_\${VERSION}_x64-setup.exe" "afkfleet-deploy-\${VERSION}.tar.gz" SHA256SUMS`,
  ),
];

describe("the release workflow", () => {
  it("runs on a pushed v* tag alone, reads only by default, and queues releases, never cancelling one", () => {
    assert.equal(block(workflow, "on"), 'on:\n  push:\n    tags: ["v*"]');
    assert.equal(block(workflow, "permissions"), "permissions:\n  contents: read");
    assert.equal(
      block(workflow, "concurrency"),
      "concurrency:\n  group: release\n  cancel-in-progress: false\n  queue: max",
    );
    assert.equal(block(workflow, "env"), block(stack, "env"));
    assert.deepEqual(
      [...all.keys()],
      ["guard", "build", "push", "merge", "bundle", "installer", "release"],
    );
  });

  it("grants each job exactly its permissions, and bounds each", () => {
    const expected = {
      guard: [["contents: read", "actions: read"], 10],
      build: [["contents: read"], 150],
      push: [["contents: read", "packages: write"], 30],
      merge: [["contents: read", "packages: write"], 15],
      bundle: [["contents: read"], 10],
      installer: [["contents: read"], 60],
      release: [["contents: write"], 10],
    };
    for (const [name, [grants, minutes]] of Object.entries(expected)) {
      assert.equal(
        key(job(name), "permissions"),
        ["    permissions:", ...grants.map((grant) => `      ${grant}`)].join("\n"),
        name,
      );
      assert.equal(key(job(name), "timeout-minutes"), `    timeout-minutes: ${minutes}`, name);
    }
  });

  it("holds no secret and no environment, and checks out without credentials", () => {
    for (const banned of [
      "secrets.",
      "environment:",
      "pull_request_target",
      "workflow_run",
      "TAURI_",
      "id-token",
    ]) {
      assert.equal(workflow.includes(banned), false, banned);
    }
    const checkouts = [...workflow.matchAll(/- uses: actions\/checkout@/g)].length;
    assert.equal(checkouts, 6, "every job but release checks out");
    assert.equal([...workflow.matchAll(/^ {10}persist-credentials: false$/gm)].length, checkouts);
    assert.equal(
      [...workflow.matchAll(TOKEN)].length,
      4,
      "the guard's gh, the two logins and the release's gh",
    );
  });

  it("uses only ci.yml's pins and the two artifact actions, which no other workflow uses", () => {
    const uses = [...workflow.matchAll(/uses: (.+)$/gm)].map((match) => match[1]);
    assert.ok(uses.length > 10);
    for (const action of uses) {
      assert.match(action, /^[\w./-]+@[0-9a-f]{40} # v[\d.]+$/, action);
      assert.ok(
        ci.includes(`uses: ${action}`) || ARTIFACT_ACTIONS.includes(action),
        `${action} is neither ci.yml's pin nor an approved artifact action`,
      );
    }
    for (const file of ["ci.yml", "stack.yml", "shots.yml", "slow-tests.yml", "audit.yml"]) {
      const other = read(`.github/workflows/${file}`);
      assert.equal(/upload-artifact|download-artifact/.test(other), false, file);
    }
  });

  it("gives the job token to the jobs that write alone, and runs no build tool in them", () => {
    for (const [name, text] of all) {
      const writes = /: write$/m.test(key(text, "permissions") ?? "");
      const token = text.match(TOKEN) !== null;
      // The bundle job writes nothing, but its file runs as root on every host (S32).
      if (["push", "merge", "release", "bundle"].includes(name)) {
        for (const each of steps(text)) {
          assert.equal(BUILD_TOOL.test(runOf(each)), false, `${name}: ${each}`);
        }
      }
      if (["push", "merge", "release"].includes(name)) {
        assert.ok(writes, `${name} writes`);
      } else {
        assert.equal(writes, false, `${name} writes nothing`);
      }
      if (name !== "guard") {
        assert.equal(token && !["push", "merge", "release"].includes(name), false, name);
      }
    }
  });
});

describe("the guard job", () => {
  it("asks scripts/release.mjs about the tag, with main fetched and the job token for gh", () => {
    const guard = job("guard");
    const found = steps(guard);

    assert.equal(key(guard, "runs-on"), "    runs-on: ubuntu-24.04");
    assert.equal(key(guard, "needs"), null);
    assert.ok(found[0].includes("fetch-depth: 0"));
    assert.ok(
      found.includes(
        "      - name: Fetch main\n        run: git fetch --no-tags origin +refs/heads/main:refs/remotes/origin/main",
      ),
    );
    assert.equal(
      found.at(-1),
      step(
        "      - id: guard",
        "        name: The tag, the version, main and CI",
        "        env:",
        `          GH_TOKEN: \${{ github.token }}`,
        `          GH_REPO: \${{ github.repository }}`,
        '        run: node scripts/release.mjs guard "$GITHUB_REF"',
      ),
    );
    assert.equal(
      key(guard, "outputs"),
      step(
        "    outputs:",
        `      version: \${{ steps.guard.outputs.version }}`,
        `      revision: \${{ steps.guard.outputs.revision }}`,
        `      created: \${{ steps.guard.outputs.created }}`,
      ),
    );
  });

  it("can find CI's run: CI runs on every push to main", () => {
    assert.match(block(ci, "on"), /^ {2}push:\n {4}branches: \[main\]$/m);
    assert.match(ci, /^name: CI$/m);
  });
});

describe("the build jobs", () => {
  const build = job("build");
  const found = steps(build);
  const testStack = found.findIndex((each) => each.includes("run: just test-stack"));

  it("run on both native runners with this release's labels, and can't write anything", () => {
    assert.equal(key(build, "needs"), "    needs: guard");
    assert.equal(key(build, "runs-on"), `    runs-on: \${{ matrix.runner }}`);
    assert.equal(key(build, "strategy"), MATRIX);
    assert.match(stack, /^ {8}runner: \[ubuntu-24\.04-arm, ubuntu-24\.04\]$/m);
    assert.equal(
      key(build, "env"),
      ["    env:", ...RELEASE_ENV, '      BUILDX_NO_DEFAULT_ATTESTATIONS: "1"'].join("\n"),
    );
    assert.equal(build.match(TOKEN), null, "no token");
    assert.equal(build.includes("docker login"), false);
    assert.equal(build.includes("packages:"), false);
  });

  it("build and test exactly as the Stack workflow does: its steps word for word, test-stack in host mode", () => {
    const stackSteps = steps(jobs(stack).get("stack"));

    assert.ok(testStack > 0);
    assert.deepEqual(found.slice(0, testStack + 1), stackSteps);
    assert.equal(
      found[testStack],
      "      - run: just test-stack\n        env:\n          AFKFLEET_SMOKE_SETUP: host",
    );
  });

  it("then save the tested images by their IDs and hand them on", () => {
    assert.deepEqual(found.slice(testStack + 1), [
      SETUP_NODE,
      step(
        "      - name: Save the tested images by their IDs",
        "        env:",
        `          ARCH: \${{ matrix.arch }}`,
        "        run: |",
        '          mkdir "$RUNNER_TEMP/images"',
        '          node scripts/release.mjs save "$ARCH" "$RUNNER_TEMP/images"',
      ),
      step(
        UPLOAD,
        "        with:",
        `          name: images-\${{ matrix.arch }}`,
        `          path: \${{ runner.temp }}/images/`,
        "          if-no-files-found: error",
        "          retention-days: 7",
      ),
    ]);
  });
});

describe("the push jobs", () => {
  const push = job("push");

  it("are pinned whole: their keys, and every step word for word", () => {
    assert.deepEqual(keys(push), [
      "name",
      "needs",
      "strategy",
      "runs-on",
      "timeout-minutes",
      "permissions",
      "env",
      "steps",
    ]);
    assert.equal(key(push, "name"), `    name: push (\${{ matrix.arch }})`);
    assert.equal(key(push, "needs"), "    needs: [guard, build]");
    assert.equal(key(push, "strategy"), MATRIX);
    assert.equal(key(push, "runs-on"), `    runs-on: \${{ matrix.runner }}`);
    assert.equal(key(push, "env"), ["    env:", ...RELEASE_ENV].join("\n"));
    assert.deepEqual(steps(push), PUSH_STEPS);
  });

  it("load and check the images before the login, and log in only to push", () => {
    const found = steps(push);
    const load = found.findIndex((each) => each.includes("release.mjs load"));
    const login = found.findIndex((each) => each.includes("docker login"));

    assert.ok(load > 0 && login === load + 1);
    assert.equal(found[login + 1], LOGOUT_ALWAYS);
    assert.equal([...push.matchAll(/docker login/g)].length, 1);
  });
});

describe("the merge job", () => {
  const merge = job("merge");

  it("is pinned whole: its keys, and every step word for word", () => {
    assert.deepEqual(keys(merge), [
      "name",
      "needs",
      "runs-on",
      "timeout-minutes",
      "permissions",
      "outputs",
      "env",
      "steps",
    ]);
    assert.equal(key(merge, "needs"), "    needs: [guard, push]");
    assert.equal(key(merge, "runs-on"), "    runs-on: ubuntu-24.04");
    assert.equal(key(merge, "env"), ["    env:", ...RELEASE_ENV].join("\n"));
    assert.equal(
      key(merge, "outputs"),
      step(
        "    outputs:",
        `      server_digest: \${{ steps.merge.outputs.server_digest }}`,
        `      agent_digest: \${{ steps.merge.outputs.agent_digest }}`,
      ),
    );
    assert.deepEqual(steps(merge), MERGE_STEPS);
  });

  it("makes each index from the push jobs' digests, never their tags", () => {
    for (const banned of ["-arm64", "-amd64", "docker build"]) {
      assert.equal(merge.includes(banned), false, banned);
    }
  });
});

describe("the bundle job", () => {
  const bundle = job("bundle");

  it("is pinned whole: its keys, and every step word for word, with nothing to write", () => {
    assert.deepEqual(keys(bundle), [
      "name",
      "needs",
      "runs-on",
      "timeout-minutes",
      "permissions",
      "env",
      "steps",
    ]);
    assert.equal(key(bundle, "name"), "    name: bundle");
    assert.equal(key(bundle, "needs"), "    needs: [guard, merge]");
    assert.equal(key(bundle, "runs-on"), "    runs-on: ubuntu-24.04");
    assert.equal(
      key(bundle, "env"),
      step(
        "    env:",
        `      VERSION: \${{ needs.guard.outputs.version }}`,
        `      CREATED: \${{ needs.guard.outputs.created }}`,
        `      SERVER_DIGEST: \${{ needs.merge.outputs.server_digest }}`,
        `      AGENT_DIGEST: \${{ needs.merge.outputs.agent_digest }}`,
      ),
    );
    assert.deepEqual(steps(bundle), BUNDLE_STEPS);
    assert.equal(bundle.match(TOKEN), null, "no token");
  });
});

describe("the installer job", () => {
  const installer = job("installer");
  const found = steps(installer);

  it("builds the installer from scratch with no key, through just app-installer, and hands it on", () => {
    assert.equal(key(installer, "needs"), "    needs: guard");
    assert.equal(key(installer, "runs-on"), "    runs-on: windows-latest");
    assert.ok(found.includes("      - run: just app-installer"));
    assert.equal(installer.includes("rust-cache"), false, "no cache in a release build");
    assert.equal(/^\s+cache:/m.test(installer), false, "no pnpm cache in a release build");
    assert.ok(found.includes("      - run: pnpm install --frozen-lockfile"));
    assert.equal(
      found.at(-1),
      step(
        UPLOAD,
        "        with:",
        "          name: installer",
        `          path: target/release/bundle/nsis/afkfleet_\${{ needs.guard.outputs.version }}_x64-setup.exe`,
        "          if-no-files-found: error",
        "          retention-days: 7",
      ),
    );
  });
});

describe("the release job", () => {
  const release = job("release");

  it("is pinned whole: its keys, and every step word for word, with no checkout", () => {
    assert.deepEqual(keys(release), [
      "name",
      "needs",
      "runs-on",
      "timeout-minutes",
      "permissions",
      "env",
      "steps",
    ]);
    assert.equal(key(release, "needs"), "    needs: [guard, merge, installer, bundle]");
    assert.equal(key(release, "runs-on"), "    runs-on: ubuntu-24.04");
    assert.equal(
      key(release, "env"),
      `    env:\n      VERSION: \${{ needs.guard.outputs.version }}`,
    );
    assert.deepEqual(steps(release), RELEASE_STEPS);
    for (const banned of ["actions/checkout", "node ", "scripts/"]) {
      assert.equal(release.includes(banned), false, banned);
    }
  });
});
