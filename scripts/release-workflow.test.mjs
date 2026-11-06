// Pins .github/workflows/release.yml (Plan.md P12.5; ADR-0022, S22, U31): the tag-only trigger,
// each job's permissions and bounds, no secret and no environment, the approved actions alone,
// the guard asking for CI, the image jobs testing exactly as the Stack workflow does and logging in
// only after every build and test, the indexes made from digests, the installer built unsigned
// and uncached, and a release job that runs no repository code. The scripts it runs are tested in
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

const all = jobs(workflow);
const job = (name) => {
  const text = all.get(name);
  assert.ok(text !== undefined, `no ${name} job`);
  return text;
};

/** The image job's step that logs in, pushes and logs out, word for word. */
const PUSH_STEP = [
  "      - name: Push by architecture (log in, push, log out)",
  "        env:",
  `          REGISTRY_TOKEN: \${{ github.token }}`,
  `          ARCH: \${{ matrix.arch }}`,
  "        run: |",
  '          mkdir "$RUNNER_TEMP/digests"',
  `          printf '%s' "$REGISTRY_TOKEN" | docker login ghcr.io --username "$GITHUB_ACTOR" --password-stdin`,
  '          node scripts/release.mjs push "$ARCH" "$RUNNER_TEMP/digests"',
  "          docker logout ghcr.io",
].join("\n");

/** The merge job's step that logs in, makes and checks the indexes, and logs out. */
const MERGE_STEP = [
  "      - id: merge",
  "        name: The indexes, from the digests (log in, create and check, log out)",
  "        env:",
  `          REGISTRY_TOKEN: \${{ github.token }}`,
  "        run: |",
  `          printf '%s' "$REGISTRY_TOKEN" | docker login ghcr.io --username "$GITHUB_ACTOR" --password-stdin`,
  '          node scripts/release.mjs merge "$RUNNER_TEMP/digests"',
  "          docker logout ghcr.io",
].join("\n");

/** The logout that runs whatever happened before it. */
const LOGOUT_ALWAYS = [
  "      - name: Log out, whatever happened",
  "        if: always()",
  "        run: docker logout ghcr.io",
].join("\n");

describe("the release workflow", () => {
  it("runs on a pushed v* tag alone, reads only by default, and never cancels a release", () => {
    assert.equal(block(workflow, "on"), 'on:\n  push:\n    tags: ["v*"]');
    assert.equal(block(workflow, "permissions"), "permissions:\n  contents: read");
    assert.equal(
      block(workflow, "concurrency"),
      "concurrency:\n  group: release\n  cancel-in-progress: false",
    );
    assert.equal(block(workflow, "env"), block(stack, "env"));
    assert.deepEqual([...all.keys()], ["guard", "image", "merge", "installer", "release"]);
  });

  it("grants each job exactly its permissions, and bounds each", () => {
    const expected = {
      guard: [["contents", "read"], ["actions", "read"], 10],
      image: [["contents", "read"], ["packages", "write"], 150],
      merge: [["contents", "read"], ["packages", "write"], 15],
      installer: [["contents", "read"], null, 60],
      release: [["contents", "write"], null, 10],
    };
    for (const [name, [first, second, minutes]] of Object.entries(expected)) {
      const lines = ["    permissions:", `      ${first[0]}: ${first[1]}`];
      if (second !== null) {
        lines.push(`      ${second[0]}: ${second[1]}`);
      }
      assert.equal(key(job(name), "permissions"), lines.join("\n"), name);
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
    assert.equal(checkouts, 4, "every job but release checks out");
    assert.equal([...workflow.matchAll(/^ {10}persist-credentials: false$/gm)].length, checkouts);
    assert.equal(
      [...workflow.matchAll(/\$\{\{ github\.token \}\}/g)].length,
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
      [
        "      - id: guard",
        "        name: The tag, the version, main and CI",
        "        env:",
        `          GH_TOKEN: \${{ github.token }}`,
        `          GH_REPO: \${{ github.repository }}`,
        '        run: node scripts/release.mjs guard "$GITHUB_REF"',
      ].join("\n"),
    );
    assert.equal(
      key(guard, "outputs"),
      [
        "    outputs:",
        `      version: \${{ steps.guard.outputs.version }}`,
        `      revision: \${{ steps.guard.outputs.revision }}`,
        `      created: \${{ steps.guard.outputs.created }}`,
      ].join("\n"),
    );
  });

  it("can find CI's run: CI runs on every push to main", () => {
    assert.match(block(ci, "on"), /^ {2}push:\n {4}branches: \[main\]$/m);
    assert.match(ci, /^name: CI$/m);
  });
});

describe("the image jobs", () => {
  const image = job("image");
  const found = steps(image);
  const testStack = found.findIndex((step) => step.includes("run: just test-stack"));
  const push = found.indexOf(PUSH_STEP);

  it("run on both native runners, with this release's labels and no default attestations", () => {
    assert.equal(key(image, "needs"), "    needs: guard");
    assert.equal(key(image, "runs-on"), `    runs-on: \${{ matrix.runner }}`);
    assert.equal(
      key(image, "strategy"),
      [
        "    strategy:",
        "      fail-fast: false",
        "      matrix:",
        "        include:",
        "          - runner: ubuntu-24.04-arm",
        "            arch: arm64",
        "          - runner: ubuntu-24.04",
        "            arch: amd64",
      ].join("\n"),
    );
    assert.match(stack, /^ {8}runner: \[ubuntu-24\.04-arm, ubuntu-24\.04\]$/m);
    assert.equal(
      key(image, "env"),
      [
        "    env:",
        `      AFKFLEET_IMAGE_VERSION: \${{ needs.guard.outputs.version }}`,
        `      AFKFLEET_IMAGE_REVISION: \${{ needs.guard.outputs.revision }}`,
        `      AFKFLEET_IMAGE_CREATED: \${{ needs.guard.outputs.created }}`,
        '      BUILDX_NO_DEFAULT_ATTESTATIONS: "1"',
      ].join("\n"),
    );
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

  it("log in only after every build and test, right before the push, and log out right after", () => {
    assert.ok(push > testStack, "the push comes after test-stack");
    const between = found.slice(testStack + 1, push);
    assert.deepEqual(
      between,
      [
        "      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0\n        with:\n          node-version: 24",
      ],
      "only node's setup between test-stack and the login",
    );
    assert.equal(found[push + 1], LOGOUT_ALWAYS);
    for (const [at, step] of found.entries()) {
      if (at !== push) {
        assert.equal(step.includes("docker login"), false, step);
        assert.equal(step.includes("github.token"), false, step);
        assert.equal(/\bjust\b|cargo|docker build/.test(step) && at > testStack, false, step);
      }
    }
  });

  it("hand their digests on as an artifact, the job's last step", () => {
    assert.equal(found.length, push + 3);
    assert.equal(
      found.at(-1),
      [
        "      - uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9 # v7.0.2",
        "        with:",
        `          name: digests-\${{ matrix.arch }}`,
        `          path: \${{ runner.temp }}/digests/digests-\${{ matrix.arch }}.txt`,
        "          if-no-files-found: error",
        "          retention-days: 7",
      ].join("\n"),
    );
  });
});

describe("the merge job", () => {
  const merge = job("merge");
  const found = steps(merge);
  const at = found.indexOf(MERGE_STEP);

  it("makes each index from the image jobs' digests, never their tags, logged in for that alone", () => {
    assert.equal(key(merge, "needs"), "    needs: [guard, image]");
    assert.ok(at > 0, "the merge step, word for word");
    assert.equal(
      found[at - 1],
      [
        "      - uses: actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2",
        "        with:",
        "          pattern: digests-*",
        `          path: \${{ runner.temp }}/digests`,
        "          merge-multiple: true",
      ].join("\n"),
    );
    assert.equal(found[at + 1], LOGOUT_ALWAYS);
    for (const banned of ["-arm64", "-amd64", "just ", "cargo", "docker build", "pnpm"]) {
      assert.equal(merge.includes(banned), false, banned);
    }
    assert.equal([...merge.matchAll(/docker login/g)].length, 1, "one login, in the merge step");
    assert.equal(
      key(merge, "outputs"),
      [
        "    outputs:",
        `      server_digest: \${{ steps.merge.outputs.server_digest }}`,
        `      agent_digest: \${{ steps.merge.outputs.agent_digest }}`,
      ].join("\n"),
    );
  });

  it("writes the draft's body after the logout, and hands it on", () => {
    assert.deepEqual(found.slice(at + 2), [
      [
        "      - name: The release's body",
        "        env:",
        `          SERVER_DIGEST: \${{ steps.merge.outputs.server_digest }}`,
        `          AGENT_DIGEST: \${{ steps.merge.outputs.agent_digest }}`,
        "        run: |",
        '          mkdir "$RUNNER_TEMP/notes"',
        '          node scripts/release.mjs notes "$AFKFLEET_IMAGE_VERSION" "$SERVER_DIGEST" "$AGENT_DIGEST" > "$RUNNER_TEMP/notes/notes.md"',
      ].join("\n"),
      [
        "      - uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9 # v7.0.2",
        "        with:",
        "          name: notes",
        `          path: \${{ runner.temp }}/notes/notes.md`,
        "          if-no-files-found: error",
        "          retention-days: 7",
      ].join("\n"),
    ]);
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
      [
        "      - uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9 # v7.0.2",
        "        with:",
        "          name: installer",
        `          path: target/release/bundle/nsis/afkfleet_\${{ needs.guard.outputs.version }}_x64-setup.exe`,
        "          if-no-files-found: error",
        "          retention-days: 7",
      ].join("\n"),
    );
  });
});

describe("the release job", () => {
  const release = job("release");
  const found = steps(release);

  it("is the only one that writes, and runs no repository code", () => {
    assert.equal(key(release, "needs"), "    needs: [guard, merge, installer]");
    assert.equal(key(release, "runs-on"), "    runs-on: ubuntu-24.04");
    for (const banned of ["actions/checkout", "node ", "just ", "pnpm", "cargo", "scripts/"]) {
      assert.equal(release.includes(banned), false, banned);
    }
    for (const [name, other] of all) {
      if (name !== "release") {
        assert.equal(other.includes(": write\n") && name !== "image" && name !== "merge", false);
        assert.equal(other.includes("contents: write"), false, name);
      }
    }
  });

  it("writes SHA256SUMS, then a new draft, never over a release that exists", () => {
    assert.equal(
      found[0],
      [
        "      - uses: actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2",
        "        with:",
        "          name: installer",
        `          path: \${{ runner.temp }}/files`,
      ].join("\n"),
    );
    assert.equal(
      found[1],
      [
        "      - uses: actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2",
        "        with:",
        "          name: notes",
        `          path: \${{ runner.temp }}/notes`,
      ].join("\n"),
    );
    assert.equal(
      found[2],
      [
        "      - name: SHA256SUMS",
        `        working-directory: \${{ runner.temp }}/files`,
        "        run: |",
        `          test "$(ls -A)" = "afkfleet_\${VERSION}_x64-setup.exe"`,
        `          sha256sum -- "afkfleet_\${VERSION}_x64-setup.exe" > SHA256SUMS`,
        "          cat SHA256SUMS",
        '          cat SHA256SUMS >> "$GITHUB_STEP_SUMMARY"',
      ].join("\n"),
    );
    assert.equal(
      found[3],
      [
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
        `          gh release create "v$VERSION" --draft --verify-tag --notes-file "$RUNNER_TEMP/notes/notes.md" "afkfleet_\${VERSION}_x64-setup.exe" SHA256SUMS`,
      ].join("\n"),
    );
    assert.equal(found.length, 4);
    assert.equal(
      key(release, "env"),
      `    env:\n      VERSION: \${{ needs.guard.outputs.version }}`,
    );
  });
});
