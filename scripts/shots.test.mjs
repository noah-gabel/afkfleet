// The screenshot harness (ADR-0020, U97, U118, U125, S130, R226): the image pinned once, at the
// repository's Playwright; a build context that only the frontend's sources can enter; a container
// with no network, no capabilities and two mounts; the recipes; and the workflow.
import assert from "node:assert/strict";
import {
  existsSync,
  linkSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmdirSync,
  rmSync,
  symlinkSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { recipeBody, recipeDependencies } from "./justfile.mjs";
import {
  BASELINES,
  buildArgs,
  DOCKERFILE,
  designFolder,
  diffs,
  IMAGE,
  mountProblem,
  pruneArgs,
  RESULTS,
  runArgs,
  shots,
  strays,
} from "./shots.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (file) => readFileSync(path.join(ROOT, file), "utf8");
const PIN = "mcr.microsoft.com/playwright";
const dockerfile = read(DOCKERFILE);
const ignore = read(`${DOCKERFILE}.dockerignore`);
const workflow = read(".github/workflows/shots.yml");

/** A `.dockerignore`'s rules, in order, comments and blank lines left out. */
function rules(text) {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line !== "" && !line.startsWith("#"));
}

/** Docker's `*` (no `/`) and `**` (any depth) as a regular expression for a whole path. */
function pattern(glob) {
  const source = glob
    .replace(/\/$/, "")
    .split(/(\*\*\/|\*\*|\*)/)
    .map((part) => {
      if (part === "**/") {
        return "(?:.*/)?";
      }
      if (part === "**") {
        return ".*";
      }
      if (part === "*") {
        return "[^/]*";
      }
      return part.replace(/[.+?^${}()|[\]\\]/g, "\\$&");
    })
    .join("");
  return new RegExp(`^${source}$`);
}

/** Whether `file` stays out of the context: the last rule matching it, or a folder above it. */
function ignored(text, file) {
  const prefixes = file.split("/").map((_, at, parts) => parts.slice(0, at + 1).join("/"));
  let out = false;
  for (const rule of rules(text)) {
    const negated = rule.startsWith("!");
    const glob = pattern(negated ? rule.slice(1) : rule);
    if (prefixes.some((prefix) => glob.test(prefix))) {
      out = !negated;
    }
  }
  return out;
}

describe("the image", () => {
  it("is pinned once, by digest, at the repository's Playwright", () => {
    const froms = dockerfile.split("\n").filter((line) => line.startsWith("FROM "));
    const version = JSON.parse(read("apps/desktop/package.json")).devDependencies[
      "@playwright/test"
    ];
    assert.match(version, /^\d+\.\d+\.\d+$/);
    assert.deepEqual(froms.length, 1);
    assert.match(
      froms[0],
      new RegExp(`^FROM ${PIN}:v${version.replaceAll(".", "\\.")}-noble@sha256:[0-9a-f]{64}$`),
    );
  });

  it("is named nowhere else", () => {
    for (const file of [".github/workflows/shots.yml", "justfile", "scripts/shots.mjs"]) {
      assert.equal(read(file).includes(PIN), false, file);
    }
  });

  it("sets the marker the shots' config reads, and pnpm comes from the root's packageManager", () => {
    assert.match(dockerfile, /^ENV AFKFLEET_SHOTS_IMAGE="1"$/m);
    assert.match(read("apps/desktop/e2e/shots.ts"), /IMAGE_MARKER = "AFKFLEET_SHOTS_IMAGE"/);
    const manager = JSON.parse(read("package.json")).packageManager;
    assert.match(manager, /^pnpm@\d+\.\d+\.\d+$/);
    const lock =
      /packageManagerDependencies:\n\s+pnpm:\n\s+specifier: (\S+)\n\s+version: (\S+)/.exec(
        read("pnpm-lock.yaml"),
      );
    assert.deepEqual([lock?.[1], lock?.[2]], [manager.slice(5), manager.slice(5)]);
    assert.match(
      dockerfile,
      /npm install --global --ignore-scripts "\$\(node --print "require\('\.\/package\.json'\)\.packageManager"\)"/,
    );
  });

  it("is labelled with the title its prune removes by", () => {
    assert.match(dockerfile, /^LABEL org\.opencontainers\.image\.title="afkfleet-shots"$/m);
    assert.deepEqual(pruneArgs(), [
      "image",
      "prune",
      "--force",
      "--filter",
      "label=org.opencontainers.image.title=afkfleet-shots",
    ]);
  });
});

describe("the build context", () => {
  it("lets in exactly the frontend's manifests, configs and sources", () => {
    assert.deepEqual(rules(ignore), [
      "*",
      "!package.json",
      "!pnpm-lock.yaml",
      "!pnpm-workspace.yaml",
      "!patches/",
      "!tsconfig.base.json",
      "!packages/ui/package.json",
      "!packages/ui/tsconfig.json",
      "!packages/ui/src/",
      "!apps/desktop/package.json",
      "!apps/desktop/tsconfig.json",
      "!apps/desktop/tsconfig.e2e.json",
      "!apps/desktop/vite.e2e.config.ts",
      "!apps/desktop/playwright.shots.config.ts",
      "!apps/desktop/src/",
      "!apps/desktop/e2e/",
      "!apps/desktop/src-tauri/tauri.conf.json",
      "apps/desktop/e2e/baselines",
      "**/node_modules",
      "**/dist",
      "**/dist-e2e",
      "**/test-results",
      "**/.env*",
      "**/*.pem",
      "**/*.key",
      "**/secrets",
      "**/*.db*",
    ]);
  });

  for (const file of [
    "secrets/p1.8-account.txt",
    "target/dod-run/afkfleet.db",
    "target/dev-server/vault-key-1",
    ".git/config",
    ".env",
    "apps/desktop/.env.local",
    "apps/desktop/src/.env",
    "packages/ui/src/a.key",
    "apps/desktop/e2e/server.pem",
    "packages/ui/src/secrets/x.ts",
    "apps/desktop/e2e/baselines/theme-sweep.spec.ts/login-light.png",
    "packages/ui/node_modules/react/index.js",
    "apps/desktop/dist-e2e/index.html",
    "apps/desktop/src-tauri/src/lib.rs",
    "apps/desktop/src-tauri/tauri.check.conf.json",
    "crates/fleet-server/src/lib.rs",
    "Cargo.toml",
  ]) {
    it(`keeps out ${file}`, () => assert.equal(ignored(ignore, file), true));
  }

  for (const file of [
    "package.json",
    "pnpm-lock.yaml",
    "patches/sonner@2.0.8.patch",
    "apps/desktop/src/main.tsx",
    "apps/desktop/e2e/specs/theme-sweep.spec.ts",
    "apps/desktop/src-tauri/tauri.conf.json",
    "packages/ui/src/App.tsx",
  ]) {
    it(`lets in ${file}`, () => assert.equal(ignored(ignore, file), false));
  }

  it("no exception reaches target, secrets or every depth", () => {
    for (const rule of rules(ignore).filter((one) => one.startsWith("!"))) {
      assert.doesNotMatch(rule, /^!(?:target|secrets|\*\*)/, rule);
    }
  });

  it("copies nothing the context keeps out", () => {
    const sources = dockerfile
      .split("\n")
      .filter((line) => line.startsWith("COPY "))
      .flatMap((line) => line.split(/\s+/).slice(1, -1));
    assert.ok(sources.length >= 8, sources.join(" "));
    for (const source of sources) {
      // A folder gets in through the exceptions inside it.
      const within = rules(ignore).some((rule) => rule.startsWith(`!${source}/`));
      assert.ok(!ignored(ignore, source) || within, `COPY ${source}`);
    }
  });
});

describe("the docker commands", () => {
  it("build the one Dockerfile for amd64 under the local tag, writing its ID", () => {
    assert.deepEqual(buildArgs("/repo/target/shots/image-id"), [
      "build",
      "--platform",
      "linux/amd64",
      "--file",
      "deploy/docker/shots.Dockerfile",
      "--tag",
      IMAGE,
      "--iidfile",
      "/repo/target/shots/image-id",
      ".",
    ]);
  });

  /** The ID a build wrote: what runs, never the shared tag. */
  const ID = `sha256:${"0123456789abcdef".repeat(4)}`;

  const base = (user) => [
    "run",
    "--rm",
    "--init",
    "--platform",
    "linux/amd64",
    "--network",
    "none",
    "--user",
    user,
    "--cap-drop",
    "ALL",
    "--security-opt",
    "no-new-privileges",
    "--shm-size",
    "1g",
    "--env",
    "HOME=/tmp",
    "--env",
    "CI",
  ];
  const tail = (update) => [
    ID,
    "node_modules/.bin/playwright",
    "test",
    "--config",
    "playwright.shots.config.ts",
    `--update-snapshots=${update}`,
  ];

  it("compare on Linux as the host's own user, the baselines read-only", () => {
    assert.deepEqual(
      runArgs({
        mode: "compare",
        root: "/repo",
        platform: "linux",
        uid: 1001,
        gid: 118,
        image: ID,
      }),
      [
        ...base("1001:118"),
        "--mount",
        `type=bind,source=/repo/${BASELINES},target=/app/${BASELINES},readonly`,
        "--mount",
        `type=bind,source=/repo/${RESULTS},target=/app/${RESULTS}`,
        ...tail("none"),
      ],
    );
  });

  it("update on Windows as the image's user, the baselines writable", () => {
    assert.deepEqual(runArgs({ mode: "update", root: "C:\\repo", platform: "win32", image: ID }), [
      ...base("pwuser"),
      "--mount",
      "type=bind,source=C:\\repo\\apps\\desktop\\e2e\\baselines,target=/app/apps/desktop/e2e/baselines",
      "--mount",
      "type=bind,source=C:\\repo\\target\\shots\\results,target=/app/target/shots/results",
      ...tail("changed"),
    ]);
  });

  it("design mounts its folder, never the baselines", () => {
    assert.deepEqual(
      runArgs({
        mode: "design",
        root: "/repo",
        platform: "linux",
        uid: 1,
        gid: 2,
        design: "/repo/target/design-l4/before",
        image: ID,
      }),
      [
        ...base("1:2"),
        "--env",
        "AFKFLEET_SHOTS_DESIGN=1",
        "--mount",
        "type=bind,source=/repo/target/design-l4/before,target=/app/target/design",
        "--mount",
        `type=bind,source=/repo/${RESULTS},target=/app/${RESULTS}`,
        ...tail("all"),
      ],
    );
  });

  it("run as the image's user when the host's is root, never as root", () => {
    const args = runArgs({
      mode: "compare",
      root: "/repo",
      platform: "linux",
      uid: 0,
      gid: 0,
      image: ID,
    });
    assert.equal(args[args.indexOf("--user") + 1], "pwuser");
  });

  it("never widen what the container may do", () => {
    for (const mode of ["compare", "update"]) {
      const args = runArgs({ mode, root: "/repo", platform: "linux", uid: 1, gid: 1 }).join(" ");
      for (const flag of [
        "--ipc",
        "--privileged",
        "--cap-add",
        " -v ",
        "--volume",
        "--network host",
        "push",
      ]) {
        assert.equal(args.includes(flag), false, `${mode}: ${flag}`);
      }
    }
  });
});

describe("the folders it mounts", () => {
  const temp = () => mkdtempSync(path.join(tmpdir(), "afkfleet-shots-"));
  /** Removes the link at `where` itself, never what it points to. */
  const unlink = (where) => {
    try {
      unlinkSync(where);
    } catch {
      rmdirSync(where);
    }
  };

  it("takes a real folder inside the repository", () => {
    const root = temp();
    try {
      mkdirSync(path.join(root, "target", "shots", "results"), { recursive: true });
      assert.equal(mountProblem(root, path.join(root, "target", "shots", "results")), null);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("refuses a link or junction on the way, and a folder outside", () => {
    const root = temp();
    const elsewhere = temp();
    try {
      symlinkSync(
        elsewhere,
        path.join(root, "target"),
        process.platform === "win32" ? "junction" : "dir",
      );
      mkdirSync(path.join(elsewhere, "shots"));
      assert.match(
        mountProblem(root, path.join(root, "target", "shots")) ?? "",
        /link or junction/,
      );
      assert.match(mountProblem(root, elsewhere) ?? "", /isn't inside the repository/);
    } finally {
      unlink(path.join(root, "target"));
      rmSync(root, { recursive: true, force: true });
      rmSync(elsewhere, { recursive: true, force: true });
    }
  });

  it("puts a design only under target/design-*", () => {
    const root = path.resolve("/repo");
    assert.equal(
      designFolder(root, "target/design-l4/before"),
      path.join(root, "target", "design-l4", "before"),
    );
    for (const given of [
      "target/dod-run",
      "target/dev-server/x",
      "target",
      "design-l4",
      "target/design-l4/../../x",
      "/elsewhere/target/design-x",
    ]) {
      assert.throws(() => designFolder(root, given), /target\/design-<name>/, given);
    }
  });

  it("lists a failed run's diffs, relative and in order", () => {
    const root = temp();
    try {
      const results = path.join(root, "target", "shots", "results");
      for (const [folder, name] of [
        ["theme-sweep-login", "login-light-diff.png"],
        ["theme-sweep-login", "login-light-actual.png"],
        ["theme-sweep-home", "home-dark-diff.png"],
      ]) {
        mkdirSync(path.join(results, folder), { recursive: true });
        writeFileSync(path.join(results, folder, name), "");
      }
      assert.deepEqual(diffs(root, results), [
        "target/shots/results/theme-sweep-home/home-dark-diff.png",
        "target/shots/results/theme-sweep-login/login-light-diff.png",
      ]);
      assert.deepEqual(diffs(root, path.join(root, "missing")), []);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});

describe("what the container leaves", () => {
  const temp = () => mkdtempSync(path.join(tmpdir(), "afkfleet-shots-"));
  /** `files` under `root`, each with its folders, empty. */
  const plant = (root, ...files) => {
    for (const file of files) {
      mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
      writeFileSync(path.join(root, file), "");
    }
  };

  it("in shots' folders: only theme-sweep.spec.ts/<slug>-(dark|light).png, regular, one link each", () => {
    const folder = temp();
    const elsewhere = temp();
    try {
      plant(
        folder,
        "theme-sweep.spec.ts/login-dark.png",
        "theme-sweep.spec.ts/the-audit-log-a-long-list-stacked-end-light.png",
        "theme-sweep.spec.ts/login-dim.png",
        "theme-sweep.spec.ts/Upper-dark.png",
        "theme-sweep.spec.ts/pwned.spec.ts",
        "theme-sweep.spec.ts/nested/login-dark.png",
        "pwned.spec.ts",
        "other.spec.ts/login-dark.png",
      );
      plant(elsewhere, "secret.png");
      symlinkSync(
        elsewhere,
        path.join(folder, "theme-sweep.spec.ts", "linked-dark.png"),
        process.platform === "win32" ? "junction" : "dir",
      );
      linkSync(
        path.join(folder, "theme-sweep.spec.ts", "login-dark.png"),
        path.join(folder, "theme-sweep.spec.ts", "login-light.png"),
      );

      assert.deepEqual(strays(folder, "shots"), [
        "other.spec.ts/",
        "pwned.spec.ts",
        "theme-sweep.spec.ts/Upper-dark.png",
        "theme-sweep.spec.ts/linked-dark.png",
        "theme-sweep.spec.ts/login-dark.png",
        "theme-sweep.spec.ts/login-dim.png",
        "theme-sweep.spec.ts/login-light.png",
        "theme-sweep.spec.ts/nested/",
        "theme-sweep.spec.ts/pwned.spec.ts",
      ]);
      assert.deepEqual(strays(path.join(folder, "missing"), "shots"), []);
    } finally {
      try {
        unlinkSync(path.join(folder, "theme-sweep.spec.ts", "linked-dark.png"));
      } catch {
        rmdirSync(path.join(folder, "theme-sweep.spec.ts", "linked-dark.png"));
      }
      rmSync(folder, { recursive: true, force: true });
      rmSync(elsewhere, { recursive: true, force: true });
    }
  });

  it("in the results: only Playwright's folders of PNGs and error contexts, and its last run", () => {
    const folder = temp();
    try {
      plant(
        folder,
        ".last-run.json",
        "theme-sweep-login-shots/login-dark-actual.png",
        "theme-sweep-login-shots/login-dark-diff.png",
        "theme-sweep-login-shots/error-context.md",
        "theme-sweep-login-shots/pwned.spec.ts",
        "theme-sweep-login-shots/deeper/x.png",
        "pwned.spec.ts",
      );

      assert.deepEqual(strays(folder, "results"), [
        "pwned.spec.ts",
        "theme-sweep-login-shots/deeper/",
        "theme-sweep-login-shots/pwned.spec.ts",
      ]);
    } finally {
      rmSync(folder, { recursive: true, force: true });
    }
  });

  it("stops an update the moment the run ends, listing and keeping what it left", () => {
    const root = temp();
    const said = [];
    try {
      const docker = (args) => {
        if (args[0] === "build") {
          writeFileSync(args[args.indexOf("--iidfile") + 1], `sha256:${"a".repeat(64)}`);
        }
        if (args[0] === "run") {
          plant(root, `${BASELINES}/theme-sweep.spec.ts/login-dark.png`);
          plant(root, `${BASELINES}/pwned.spec.ts`, `${RESULTS}/x-shots/x-dark-diff.png`);
          return 1;
        }
        return 0;
      };
      const status = shots(["update"], {
        root,
        docker,
        platform: "linux",
        uid: 1,
        gid: 1,
        env: {},
        error: (message) => said.push(message),
      });

      assert.equal(status, 1);
      assert.equal(said.length, 1, said.join("\n"));
      assert.match(said[0] ?? "", /apps\/desktop\/e2e\/baselines\/pwned\.spec\.ts/);
      assert.doesNotMatch(said[0] ?? "", /login-dark\.png/);
      assert.equal(existsSync(path.join(root, BASELINES, "pwned.spec.ts")), true);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});

describe("the image it runs", () => {
  const host = (root, docker, said) => ({
    root,
    docker,
    platform: "linux",
    uid: 1,
    gid: 1,
    env: {},
    error: (message) => said.push(message),
  });

  it("is the one its build just made, by the ID the build wrote, never the shared tag", () => {
    const root = mkdtempSync(path.join(tmpdir(), "afkfleet-shots-"));
    const id = `sha256:${"b".repeat(64)}`;
    const runs = [];
    try {
      const docker = (args) => {
        if (args[0] === "build") {
          writeFileSync(args[args.indexOf("--iidfile") + 1], `${id}\n`);
        }
        if (args[0] === "run") {
          runs.push(args);
        }
        return 0;
      };

      assert.equal(shots(["compare"], host(root, docker, [])), 0);
      assert.equal(runs.length, 1);
      assert.equal(runs[0]?.includes(id), true);
      assert.equal(runs[0]?.includes(IMAGE), false);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("isn't run when the build wrote no ID it can read", () => {
    const root = mkdtempSync(path.join(tmpdir(), "afkfleet-shots-"));
    const said = [];
    let ran = false;
    try {
      const docker = (args) => {
        if (args[0] === "build") {
          writeFileSync(args[args.indexOf("--iidfile") + 1], "afkfleet-shots:local");
        }
        ran ||= args[0] === "run";
        return 0;
      };

      assert.equal(shots(["compare"], host(root, docker, said)), 1);
      assert.equal(ran, false);
      assert.match(said.join("\n"), /image ID/);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});

describe("the recipes", () => {
  it("compare and update, one line each, outside check and ci", () => {
    assert.deepEqual(recipeBody("shots"), ["node scripts/shots.mjs compare"]);
    assert.deepEqual(recipeBody("shots-update"), ["node scripts/shots.mjs update"]);
    for (const recipe of ["check", "ci"]) {
      const needs = recipeDependencies(recipe);
      assert.equal(needs.includes("shots") || needs.includes("shots-update"), false, recipe);
    }
  });
});

describe("the workflow", () => {
  it("runs on the pull requests that touch what the image builds or renders, on main and on demand", () => {
    const paths = [...workflow.matchAll(/^ {6}- "([^"]+)"$/gm)].map((match) => match[1]);
    assert.deepEqual(paths, [
      "packages/**",
      "apps/desktop/**",
      "deploy/docker/shots.Dockerfile",
      "deploy/docker/shots.Dockerfile.dockerignore",
      "scripts/shots.mjs",
      ".github/workflows/shots.yml",
      "package.json",
      "pnpm-lock.yaml",
      "pnpm-workspace.yaml",
      "patches/**",
      "tsconfig.base.json",
    ]);
    assert.match(workflow, /^ {2}push:\n {4}branches: \[main\]\n {2}workflow_dispatch:$/m);
    for (const allowed of rules(ignore).filter((rule) => rule.startsWith("!"))) {
      const entry = allowed.slice(1).replace(/\/$/, "");
      assert.ok(
        paths.some((one) => pattern(one).test(entry) || pattern(one).test(`${entry}/x`)),
        `${entry} enters the image, so a change to it must run the workflow`,
      );
    }
  });

  it("reads only, holds no credential or secret, and isn't triggered by a fork's target", () => {
    assert.match(workflow, /^permissions:\n {2}contents: read$/m);
    assert.match(workflow, /persist-credentials: false/);
    assert.equal(workflow.includes("secrets."), false);
    assert.equal(workflow.includes("pull_request_target"), false);
  });

  it("uses only actions ci.yml pins, by full SHA, and runs just shots, never the update", () => {
    const ci = read(".github/workflows/ci.yml");
    const uses = [...workflow.matchAll(/uses: (\S+)/g)].map((match) => match[1]);
    assert.ok(uses.length >= 2);
    for (const action of uses) {
      assert.match(action, /@[0-9a-f]{40}$/, action);
      assert.ok(ci.includes(`uses: ${action}`), `${action} isn't ci.yml's pin`);
    }
    assert.deepEqual(
      [...workflow.matchAll(/- run: (.+)$/gm)].map((match) => match[1]),
      ["just shots"],
    );
  });
});

describe("the shots", () => {
  it("are never run as tests on the host: Edge ignores the baselines, and Vitest can't reach them", () => {
    const edge = read("apps/desktop/playwright.config.ts");
    assert.match(edge, /^ {2}testDir: "e2e",$/m);
    assert.match(edge, /^ {2}testIgnore: \["baselines\/\*\*"\],$/m);
    // The desktop app's Vitest takes only its src/; the UI package's runs inside packages/ui.
    assert.match(
      read("apps/desktop/vitest.config.ts"),
      /^ {4}include: \["src\/\*\*\/\*\.test\.ts"\],$/m,
    );
    assert.equal(read("packages/ui/vite.config.ts").includes("root:"), false);
  });

  it("are compared exactly, once, from one fixed rendering, and refused anywhere else", () => {
    const config = read("apps/desktop/playwright.shots.config.ts");
    assert.match(
      config,
      /^const refused = refusal\(process\.platform, process\.env\);\nif \(refused !== undefined\) \{\n {2}throw new Error\(refused\);\n\}$/m,
    );
    assert.match(config, /^ {2}expect: \{ toHaveScreenshot: COMPARE \},$/m);
    assert.match(config, /^ {2}retries: 0,$/m);
    assert.match(config, /^ {2}updateSnapshots: "none",$/m);
    // R234: software rasterization in one pass, in sRGB, with no subpixel text.
    const flags = /args: \[([^\]]*)\]/.exec(config)?.[1] ?? "";
    assert.deepEqual(
      [...flags.matchAll(/"([^"]+)"/g)].map((flag) => flag[1]),
      [
        "--disable-gpu",
        "--disable-gpu-rasterization",
        "--disable-partial-raster",
        "--force-color-profile=srgb",
        "--disable-lcd-text",
      ],
    );
    // One set of baselines for every machine: no platform's or project's own.
    const templates = [...config.matchAll(/"([^"]*\{arg\}[^"]*)"/g)].map((found) => found[1]);
    assert.deepEqual(templates, [
      "../../target/design/{testFileName}/{arg}{ext}",
      "{testDir}/../baselines/{testFileName}/{arg}{ext}",
    ]);
    assert.doesNotMatch(config, /\{platform\}|\{projectName\}/);
  });

  it("take one argument in the shoot helper, never a style, a style path or a mask", () => {
    const shoot = read("apps/desktop/e2e/specs/shoot.ts");
    const calls = shoot.split("toHaveScreenshot(").slice(1);
    assert.ok(calls.length > 0);
    for (const call of calls) {
      let depth = 0;
      let commas = 0;
      for (const character of call) {
        if ("([{".includes(character)) {
          depth += 1;
        } else if (")]}".includes(character)) {
          if (depth === 0) {
            break;
          }
          depth -= 1;
        } else if (character === "," && depth === 0) {
          commas += 1;
        }
      }
      assert.equal(commas, 0, `toHaveScreenshot(${call.slice(0, 40)}…`);
    }
    assert.doesNotMatch(shoot, /\bstylePath\b|\bmask\b|\bstyle\s*:/);
  });

  it("are taken only by the shoot helper, and nothing installs a browser", () => {
    const specs = path.join(ROOT, "apps", "desktop", "e2e");
    for (const entry of readdirSync(specs, { recursive: true, withFileTypes: true })) {
      if (entry.isFile() && entry.name.endsWith(".ts") && entry.name !== "shoot.ts") {
        const source = readFileSync(path.join(entry.parentPath, entry.name), "utf8");
        assert.equal(source.includes("toHaveScreenshot"), false, entry.name);
      }
    }
    for (const file of [
      DOCKERFILE,
      "justfile",
      ".github/workflows/ci.yml",
      ".github/workflows/shots.yml",
      "scripts/shots.mjs",
    ]) {
      assert.equal(read(file).includes("playwright install"), false, file);
    }
  });
});
