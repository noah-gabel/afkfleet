// Tests for deploy/setup.sh (ADR-0022, S18, U8), and the POSIX syntax of every script an
// operator runs on the host. Run with `just scripts-test`.
//
// The cases that run the script use POSIX sh against stubs of `docker`, `id`, `stat`, `install`
// and `curl` first on PATH, in a temporary folder: never a real Docker, never a real host and
// never the network (the user's rule). The docker stub records every call's arguments and the
// bytes it got on stdin; the curl stub serves a folder's files by the URL's last part. Release
// files are real bundles deploy-bundle.mjs builds, unpacked with the system's tar, and the
// runbook's install lines run as written (ADR-0022, S35, S44). Windows has no POSIX sh under
// PowerShell, so those cases run in CI's Linux job, under dash; the check of the scripts' syntax
// runs everywhere. env.example and the compose files have their
// own tests in fleet-server's tests/compose.rs (U18); deploy/host/sftp-backups.sh runs in a
// throwaway sshd container in fleet-integration's slow_sftp_backups_account (S15).
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import process from "node:process";
import { afterEach, describe, it } from "node:test";
import { fileURLToPath } from "node:url";
import { gunzipSync, gzipSync } from "node:zlib";

import { BUNDLE_FILES, buildBundle, tarOf } from "./deploy-bundle.mjs";
import { SOURCE } from "./release.mjs";

const ROOT = fileURLToPath(new URL("..", import.meta.url));
const DEPLOY = join(ROOT, "deploy");
const SETUP = readFileSync(join(DEPLOY, "setup.sh"), "utf8");
/** Every script an operator runs on the host, as (name, text). */
const HOST_SCRIPTS = [
  ["setup.sh", SETUP],
  ["host/sftp-backups.sh", readFileSync(join(DEPLOY, "host", "sftp-backups.sh"), "utf8")],
  ["files.sh", readFileSync(join(DEPLOY, "files.sh"), "utf8")],
];

/** The cases that run sh: not on Windows. */
const SH =
  process.platform === "win32" ? "no POSIX sh under PowerShell; CI runs them under dash" : false;

/** The enrollment bundle the stub's token command prints: a secret. */
const BUNDLE = "AFKFLEET-FAKE-BUNDLE-0123456789abcdef";

/** The temporary folders made so far, removed after each test. */
const made = [];

afterEach(() => {
  for (const dir of made.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
});

/** The stub `docker`: records each call, answers by the first response whose words it starts with. */
const STUB = `
import { appendFileSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
const args = process.argv.slice(2);
let stdin = "";
try { stdin = readFileSync(0, "utf8"); } catch {}
appendFileSync(process.env.STUB_LOG, JSON.stringify({ args, stdin }) + "\\n");
const responses = JSON.parse(readFileSync(process.env.STUB_RESPONSES, "utf8"));
const joined = args.join(" ");
const found = responses.find((response) => joined.startsWith(response.when)) ?? {};
// A copy out of a container writes its files into the last argument, a folder.
for (const [name, text] of Object.entries(found.writes ?? {})) {
  writeFileSync(join(args.at(-1), name), text);
}
process.stdout.write(found.stdout ?? "");
process.stderr.write(found.stderr ?? "");
process.exit(found.code ?? 0);
`;

/** The answers a healthy host gives; a case puts its own first. */
const HEALTHY = [
  { when: "compose version --short", stdout: "2.38.2\n" },
  { when: "version --format {{.Server.Version}}", stdout: "29.0.1\n" },
  { when: "info --format", stdout: "[name=seccomp,profile=builtin]\n" },
  {
    when: "compose config --format json",
    stdout: '{\n  "name": "afkfleet",\n  "services": {}\n}\n',
  },
  {
    when: "compose exec -T server /usr/local/bin/afkfleet-server agent create-enrollment-token",
    stdout: `${BUNDLE}\n`,
  },
  { when: "compose ps -q server", stdout: "0123abcd\n" },
  { when: "compose ps -a -q server", stdout: "0123abcd\n" },
  { when: "inspect --format {{.State.Health.Status}}", stdout: "healthy\n" },
  {
    when: "compose exec -T server /usr/local/bin/afkfleet-server backup",
    stdout: "/backups/afkfleet-backup-20261106T120000Z.db\n",
  },
];

/** The release's files compose mounts into the containers, copied beside the script. */
const MOUNTED = ["Caddyfile", "files.sh", "prod/server.toml", "prod/agent.toml"];

/** The stub `curl`: serves `--output`'s file from STUB_SERVED by the URL's last part, or 22. */
const CURL = `
import { appendFileSync, copyFileSync, existsSync } from "node:fs";
import { join } from "node:path";
const args = process.argv.slice(2);
appendFileSync(process.env.CURL_LOG, JSON.stringify(args) + "\\n");
const output = args[args.indexOf("--output") + 1];
const url = args.at(-1);
const file = join(process.env.STUB_SERVED, url.split("/").at(-1));
if (args.indexOf("--output") < 0 || !existsSync(file)) {
  process.stderr.write("curl: (22) The requested URL returned error: 404\\n");
  process.exit(22);
}
copyFileSync(file, output);
`;

/** A `.env` for the Caddy path with real-looking values; `change` edits its lines. */
function env(change = (lines) => lines) {
  return `${change([
    "COMPOSE_FILE=compose.yaml:compose.edge.yaml:compose.caddy.yaml",
    "AFKFLEET_SERVER_IMAGE=afkfleet-server:dev",
    "AFKFLEET_AGENT_IMAGE=afkfleet-agent:dev",
    "AFKFLEET_DOMAIN=fleet.afkfleet.test",
    "AFKFLEET_TLS=letsencrypt",
    "AFKFLEET_ACME_EMAIL=ops@afkfleet.test",
    "AFKFLEET_GRPC_NAMES=fleet.afkfleet.test,192.0.2.10",
  ]).join("\n")}\n`;
}

/**
 * Runs `sh setup.sh <args>` in a fresh folder holding the script, the release's mounted files
 * and `envText` as `.env`, with the stubs first on PATH: `responses` before the healthy ones,
 * `uid` for `id -u`, `stat` for the backups' folder's `stat`, `modes[path]` for a path as the
 * script names it, and `parentStat` for any other. The backups' folder is `backups` in the
 * folder, or `backupsDir(folder)`; `makeBackups` makes it. `prepare(folder)` runs before the
 * script (a release's files unpacked, say); `served` is the stub curl's folder; `extraEnv` joins
 * the script's environment. Returns the exit code, the output, every docker and curl call and
 * the folder.
 */
function run(
  args,
  {
    envText = env(),
    responses = [],
    uid = "1000",
    stat = "65532:65532 700",
    modes = {},
    parentStat = "0:0 755",
    noDocker = false,
    makeBackups = true,
    backupsDir = (folder) => join(folder, "backups"),
    prepare = () => {},
    served = null,
    extraEnv = {},
    noCurl = false,
    patterns = [],
  } = {},
) {
  const dir = mkdtempSync(join(tmpdir(), "afkfleet-setup-"));
  made.push(dir);
  const bin = join(dir, "bin");
  mkdirSync(bin);
  copyFileSync(join(DEPLOY, "setup.sh"), join(dir, "setup.sh"));
  mkdirSync(join(dir, "prod"));
  for (const file of MOUNTED) {
    copyFileSync(join(DEPLOY, file), join(dir, file));
  }
  prepare(dir);
  // The backups' folder: there (the stub `stat` says whose), unless the case leaves it out.
  const backups = backupsDir(dir);
  if (makeBackups) {
    mkdirSync(backups);
  }
  if (envText !== null) {
    writeFileSync(join(dir, ".env"), `${envText}AFKFLEET_BACKUPS_DIR=${backups}\n`);
  }
  writeFileSync(join(dir, "stub.mjs"), STUB);
  writeFileSync(join(dir, "curl.mjs"), CURL);
  writeFileSync(join(dir, "responses.json"), JSON.stringify([...responses, ...HEALTHY]));
  // The mounted files are readable by everyone, as fetch leaves them, unless a case says not.
  const readable = Object.fromEntries(MOUNTED.map((file) => [file, "1000:1000 644"]));
  const named = Object.entries({ ...readable, ...modes })
    .map(([path, answer]) => `"${path}") echo '${answer}' ;;\n`)
    .join("");
  const scripts = {
    id: `#!/bin/sh\necho ${uid}\n`,
    // The last argument is the path: the backups' folder gets `stat`, a path `modes` names its
    // answer, any other `parentStat`.
    // `patterns` are [glob, answer] pairs, unquoted, after the exact paths.
    stat: `#!/bin/sh\nfor last; do :; done\ncase $last in\n"${backups}") echo '${stat}' ;;\n${named}${patterns.map(([glob, answer]) => `${glob}) echo '${answer}' ;;\n`).join("")}*) echo '${parentStat}' ;;\nesac\n`,
    install: `#!/bin/sh\necho "install $*" >> "${join(dir, "install.log")}"\n`,
    chown: `#!/bin/sh\necho "chown $*" >> "${join(dir, "install.log")}"\n`,
  };
  if (!noCurl) {
    scripts.curl = `#!/bin/sh\nexec "${process.execPath}" "${join(dir, "curl.mjs")}" "$@"\n`;
  }
  if (noDocker || noCurl) {
    // PATH holds the stubs alone, so no docker or curl anywhere on the runner is found; the
    // script needs only `dirname` before it looks for either.
    scripts.dirname = `#!/bin/sh\nexec /usr/bin/dirname "$@"\n`;
  }
  if (!noDocker) {
    scripts.docker = `#!/bin/sh\nexec "${process.execPath}" "${join(dir, "stub.mjs")}" "$@"\n`;
  }
  for (const [name, text] of Object.entries(scripts)) {
    writeFileSync(join(bin, name), text);
    chmodSync(join(bin, name), 0o755);
  }
  const log = join(dir, "calls.jsonl");
  const curlLog = join(dir, "curl.jsonl");
  writeFileSync(log, "");
  writeFileSync(curlLog, "");
  const path = noDocker || noCurl ? bin : `${bin}:${process.env.PATH}`;
  const result = spawnSync("/bin/sh", ["setup.sh", ...args], {
    cwd: dir,
    env: {
      PATH: path,
      STUB_LOG: log,
      STUB_RESPONSES: join(dir, "responses.json"),
      CURL_LOG: curlLog,
      STUB_SERVED: served ?? join(dir, "nothing-served"),
      HOME: dir,
      ...extraEnv,
    },
    encoding: "utf8",
  });
  const lines = (file) =>
    readFileSync(file, "utf8")
      .split("\n")
      .filter((line) => line !== "")
      .map((line) => JSON.parse(line));
  return {
    code: result.status,
    stdout: result.stdout,
    stderr: result.stderr,
    calls: lines(log),
    curls: lines(curlLog),
    dir,
  };
}

/** The calls' arguments, joined. */
function joined(calls) {
  return calls.map((call) => call.args.join(" "));
}

/** The shell scripts' syntax rules: POSIX sh, as dash and busybox run it (S18). */
const BANNED = [
  /\[\[/,
  /(^|[;&|])\s*local\s/m,
  /pipefail/,
  /echo -e/,
  /read -s/,
  /\bmktemp\b/,
  /readlink -f/,
  /sed -i/,
  /\w+=\(/,
];

/** `text`'s lines that aren't comments. */
function code(text) {
  return text
    .split("\n")
    .filter((line) => !line.trimStart().startsWith("#"))
    .join("\n");
}

describe("the host's scripts", () => {
  it("use POSIX sh only, as dash and busybox run them", () => {
    for (const [name, text] of HOST_SCRIPTS) {
      assert.ok(text.startsWith("#!/bin/sh\n"), name);
      for (const banned of BANNED) {
        assert.doesNotMatch(code(text), banned, `${name}: ${banned}`);
      }
    }
  });

  it("the syntax rules catch what they ban, and not a message that names a word", () => {
    for (const line of [
      "  local name=x",
      "a=(1 2)",
      "if [[ -n x ]]; then",
      "set -o pipefail",
      "x; local y",
    ]) {
      assert.ok(
        BANNED.some((banned) => banned.test(line)),
        line,
      );
    }
    assert.ok(!BANNED.some((banned) => banned.test('ok "the local agent is enrolled"')));
  });
});

describe("setup.sh, run against stubs", { skip: SH }, () => {
  it("refuses a usage error with exit 2", () => {
    assert.equal(run([]).code, 2);
    assert.equal(run(["install"]).code, 2);
    assert.equal(run(["init", "now"]).code, 2);
  });

  it("refuses without docker, without Compose v2.24, with rootless Docker or user namespaces", () => {
    const missing = run(["check"], { noDocker: true });
    const old = run(["check"], {
      responses: [{ when: "compose version --short", stdout: "2.20.3\n" }],
    });
    const rootless = run(["check"], {
      responses: [{ when: "info --format", stdout: "[name=rootless]\n" }],
    });
    // userns-remap maps the containers' 65532 to another host uid (PR #97's review).
    const userns = run(["check"], {
      responses: [
        { when: "info --format", stdout: "[name=seccomp,profile=builtin name=userns]\n" },
      ],
    });

    assert.equal(missing.code, 1);
    assert.match(missing.stderr, /docker isn't installed/);
    assert.equal(old.code, 1);
    assert.match(old.stderr, /2\.20\.3 is too old/);
    for (const refused of [rootless, userns]) {
      assert.equal(refused.code, 1);
      assert.match(refused.stderr, /rootless or with user namespaces/);
      assert.doesNotMatch(refused.stdout, /ok: Docker/);
    }
  });

  it("refuses a missing .env, another COMPOSE_FILE and env.example's placeholders", () => {
    const none = run(["init"], { envText: null });
    const path = run(["init"], {
      envText: env((lines) => [...lines, "COMPOSE_FILE=compose.yaml:other.yaml"]),
    });
    const domain = run(["init"], {
      envText: env((lines) => [...lines, "AFKFLEET_DOMAIN=fleet.example.com"]),
    });
    const email = run(["init"], {
      envText: env((lines) => [...lines, "AFKFLEET_ACME_EMAIL=admin@example.com"]),
    });
    const injected = run(["init"], {
      envText: env((lines) => [...lines, "AFKFLEET_DOMAIN=a.test b"]),
    });

    assert.match(none.stderr, /copy env\.example to \.env/);
    assert.match(path.stderr, /COMPOSE_FILE/);
    assert.match(domain.stderr, /placeholder/);
    assert.match(email.stderr, /placeholder/);
    assert.match(injected.stderr, /plain host name/);
    for (const result of [none, path, domain, email, injected]) {
      assert.equal(result.code, 1);
      assert.ok(
        !joined(result.calls).some((call) => call.startsWith("compose run")),
        "nothing was created",
      );
    }
  });

  it("refuses the Caddy path when port 80 or 443 is taken, naming why", () => {
    const result = run(["init"], {
      responses: [
        {
          when: "run --rm -p 443:8080",
          stderr: "Bind for 0.0.0.0:443 failed: port is already allocated\n",
          code: 125,
        },
      ],
    });

    assert.equal(result.code, 1);
    assert.match(result.stdout, /port 443 is taken by another container/);
    assert.match(result.stderr, /80 and 443 aren't both free/);
  });

  it("refuses an install whose project has containers or volumes already", () => {
    // Either half alone refuses (PR #97's review): a container left without its volumes, and
    // volumes left without a container.
    const containers = run(["init"], {
      responses: [
        {
          when: "ps -a -q --filter label=com.docker.compose.project=afkfleet",
          stdout: "0123abcd\n",
        },
      ],
    });
    const volumes = run(["init"], {
      responses: [
        {
          when: "volume ls -q --filter label=com.docker.compose.project=afkfleet",
          stdout: "afkfleet_keys\n",
        },
      ],
    });

    for (const result of [containers, volumes]) {
      assert.equal(result.code, 1);
      assert.match(result.stderr, /init is for a first install only/);
      assert.ok(!joined(result.calls).some((call) => call.includes("create-key")));
    }
  });

  it("refuses a missing backups folder without root, printing the commands", () => {
    const result = run(["init"], { makeBackups: false });

    assert.equal(result.code, 1);
    assert.match(result.stderr, /only root can create it/);
    assert.match(result.stderr, /sudo install -d -o 65532 -g 65532 -m 700/);
    assert.ok(!joined(result.calls).some((call) => call.includes("create-key")));
  });

  it("refuses a backups folder that isn't the server's user's, owner-only", () => {
    const result = run(["init"], { stat: "1000:1000 755" });

    assert.equal(result.code, 1);
    assert.match(result.stderr, /must belong to 65532:65532 with mode 700/);
  });

  it("creates a missing backups folder as root, saying why", () => {
    const result = run(["init"], { makeBackups: false, uid: "0" });

    assert.equal(result.code, 0, result.stderr);
    assert.match(result.stdout, /must own it to write the backups/);
    const installed = readFileSync(join(result.dir, "install.log"), "utf8");
    assert.match(installed, /install -d -o 65532 -g 65532 -m 700 .*backups/);
  });

  /** What the `install` stub was asked to do, one line each. */
  function installed(result) {
    try {
      return readFileSync(join(result.dir, "install.log"), "utf8").split("\n").filter(Boolean);
    } catch {
      return [];
    }
  }

  /** A response that makes the vault key's step fail: a failure after the folders. */
  const KEY_FAILS = [{ when: "compose run --rm -T setup vault create-key", code: 1 }];

  it("refuses an existing parent that isn't root's alone, and leaves it as it is", () => {
    for (const parentStat of ["1000:1000 750", "0:0 775", "0:0 757", "0:0 2", "0:0 6"]) {
      const result = run(["init"], { makeBackups: false, uid: "0", parentStat });

      assert.equal(result.code, 1, parentStat);
      assert.match(result.stderr, /must belong to root and be writable by root alone/, parentStat);
      assert.match(result.stderr, /left as it is/, parentStat);
      assert.deepEqual(installed(result), [], `${parentStat}: nothing was changed or made`);
    }
  });

  it("uses an existing root parent as it is, and names only what it made", () => {
    const result = run(["init"], { makeBackups: false, uid: "0", responses: KEY_FAILS });

    assert.equal(result.code, 1);
    assert.deepEqual(installed(result), [
      `install -d -o 65532 -g 65532 -m 700 ${join(result.dir, "backups")}`,
    ]);
    assert.match(result.stderr, new RegExp(`this run created: ${join(result.dir, "backups")} `));
  });

  it("creates a missing parent as root's, and names it among what it made", () => {
    const result = run(["init"], {
      makeBackups: false,
      uid: "0",
      responses: KEY_FAILS,
      backupsDir: (folder) => join(folder, "srv", "backups"),
    });

    assert.equal(result.code, 1);
    const parent = join(result.dir, "srv");
    assert.deepEqual(installed(result), [
      `install -d -o root -g root -m 755 ${parent}`,
      `install -d -o 65532 -g 65532 -m 700 ${join(parent, "backups")}`,
    ]);
    assert.match(
      result.stderr,
      new RegExp(`this run created: ${parent} ${join(parent, "backups")} `),
    );
  });

  it("without root, prints the parent's command only when the parent is missing", () => {
    const missing = run(["init"], {
      makeBackups: false,
      backupsDir: (folder) => join(folder, "srv", "backups"),
    });
    const there = run(["init"], { makeBackups: false });

    assert.equal(missing.code, 1);
    assert.match(missing.stderr, /sudo install -d -o root -g root -m 755 /);
    assert.equal(there.code, 1);
    assert.doesNotMatch(there.stderr, /-o root -g root/);
    assert.match(there.stderr, /sudo install -d -o 65532 -g 65532 -m 700/);
  });

  it("refuses a backups path in /, or with //, . or .. parts, before anything else", () => {
    for (const path of [
      "/backups",
      "//backups",
      "/srv/./backups",
      "/srv/../backups",
      "/srv/backups/",
    ]) {
      const result = run(["init"], { makeBackups: false, uid: "0", backupsDir: () => path });

      assert.equal(result.code, 1, path);
      assert.match(result.stderr, /AFKFLEET_BACKUPS_DIR/, path);
      assert.deepEqual(installed(result), [], path);
      assert.ok(!joined(result.calls).some((call) => /^compose (run|up|exec)/.test(call)), path);
    }
  });

  it("takes an image from a registry path, and refuses one with a space, a glob or a leading -", () => {
    const image = "ghcr.io/owner/afkfleet-server:1.0";
    const registry = run(["init"], {
      envText: env((lines) => [...lines, `AFKFLEET_SERVER_IMAGE=${image}`]),
    });

    assert.equal(registry.code, 0, registry.stderr);
    assert.ok(joined(registry.calls).includes(`image inspect ${image}`));
    for (const bad of ["-oops", "afkfleet server", "afkfleet-*", "afkfleet?", "afkfleet[1]"]) {
      const result = run(["init"], {
        envText: env((lines) => [...lines, `AFKFLEET_SERVER_IMAGE=${bad}`]),
      });

      assert.equal(result.code, 1, bad);
      assert.match(result.stderr, /AFKFLEET_SERVER_IMAGE/, bad);
      assert.ok(!joined(result.calls).some((call) => call.startsWith("image")), bad);
    }
  });

  it("refuses env.example's AFKFLEET_GRPC_NAMES placeholder", () => {
    for (const names of ["fleet.example.com", "192.0.2.10,fleet.example.com"]) {
      const result = run(["init"], {
        envText: env((lines) => [...lines, `AFKFLEET_GRPC_NAMES=${names}`]),
      });

      assert.equal(result.code, 1, names);
      assert.match(
        result.stderr,
        /AFKFLEET_GRPC_NAMES in \.env is still env\.example's placeholder/,
      );
    }
  });

  it("init runs the one-time steps in order, the bundle only through the pipe", () => {
    const result = run(["init"]);

    assert.equal(result.code, 0, result.stderr);
    const steps = joined(result.calls).filter((call) => /^compose (run|up|exec)/.test(call));
    assert.deepEqual(steps, [
      "compose run --rm -T setup vault create-key --config /etc/afkfleet/server.toml",
      "compose run --rm -T setup pki init --config /etc/afkfleet/server.toml --dns server --dns fleet.afkfleet.test --ip 192.0.2.10",
      "compose up --detach --wait server",
      "compose exec -T server /usr/local/bin/afkfleet-server agent create-enrollment-token --config /etc/afkfleet/server.toml",
      "compose run --rm -T --no-deps agent enroll --config /etc/afkfleet/agent.toml",
      "compose up --detach --wait",
    ]);
    const enroll = result.calls.find((call) => call.args.includes("enroll"));
    assert.equal(enroll.stdin, `${BUNDLE}\n`);
    for (const call of result.calls) {
      assert.ok(!call.args.join(" ").includes(BUNDLE), "the bundle is never an argument");
    }
    assert.ok(!result.stdout.includes(BUNDLE) && !result.stderr.includes(BUNDLE));
    for (const file of readdirSync(result.dir)) {
      // The stub's own files hold the bundle by design: its answer, and what enroll read.
      if (
        file === "calls.jsonl" ||
        file === "responses.json" ||
        !statSync(join(result.dir, file)).isFile()
      ) {
        continue;
      }
      assert.ok(!readFileSync(join(result.dir, file), "utf8").includes(BUNDLE), file);
    }
    assert.match(result.stdout, /afkfleet is running/);
    assert.match(result.stdout, /create-owner/);
  });

  it("init stops at enroll's exit 3 with the disable step, and names what it created", () => {
    const result = run(["init"], {
      responses: [{ when: "compose run --rm -T --no-deps agent enroll", code: 3 }],
    });

    assert.equal(result.code, 1);
    assert.match(result.stderr, /agent disable/);
    assert.match(result.stderr, /this run created/);
    assert.ok(!joined(result.calls).includes("compose up --detach --wait"));
  });

  it("init stops when the token command fails: enroll gets no bundle, and nothing more starts", () => {
    // No pipefail (R18): the pipeline's status is enroll's, so the token command's failure
    // reaches the script as enroll's refusal of an empty stdin (PR #97's review).
    const result = run(["init"], {
      responses: [
        {
          when: "compose exec -T server /usr/local/bin/afkfleet-server agent create-enrollment-token",
          stderr: "the server couldn't be reached\n",
          code: 1,
        },
        { when: "compose run --rm -T --no-deps agent enroll", code: 1 },
      ],
    });

    assert.equal(result.code, 1);
    assert.match(result.stderr, /the local agent couldn't be enrolled/);
    assert.match(result.stderr, /this run created/);
    const enroll = result.calls.find((call) => call.args.includes("enroll"));
    assert.equal(enroll.stdin, "", "enroll got nothing to read");
    assert.ok(!joined(result.calls).includes("compose up --detach --wait"));
    assert.doesNotMatch(result.stdout, /the local agent is enrolled/);
  });

  it("update backs up with the running server first, then pulls and starts the server before the rest", () => {
    const result = run(["update"]);

    assert.equal(result.code, 0, result.stderr);
    const steps = joined(result.calls).filter((call) => /^compose (exec|pull|up)/.test(call));
    assert.deepEqual(steps, [
      "compose exec -T server /usr/local/bin/afkfleet-server backup --config /etc/afkfleet/server.toml",
      "compose pull",
      "compose up --detach --wait server",
      "compose up --detach --wait",
    ]);
  });

  it("update changes nothing when the backup fails, and goes on with a warning when only pruning did", () => {
    const failed = run(["update"], {
      responses: [
        { when: "compose exec -T server /usr/local/bin/afkfleet-server backup", code: 1 },
      ],
    });
    const pruned = run(["update"], {
      responses: [
        { when: "compose exec -T server /usr/local/bin/afkfleet-server backup", code: 3 },
      ],
    });

    assert.equal(failed.code, 1);
    assert.ok(!joined(failed.calls).includes("compose pull"));
    assert.equal(pruned.code, 0, pruned.stderr);
    assert.match(pruned.stdout, /couldn't be pruned/);
  });

  it("update refuses a server that isn't healthy", () => {
    const result = run(["update"], {
      responses: [{ when: "inspect --format {{.State.Health.Status}}", stdout: "starting\n" }],
    });

    assert.equal(result.code, 1);
    assert.ok(!joined(result.calls).some((call) => call.includes("backup")));
  });

  it("check says which proxy path fits, and changes nothing", () => {
    const free = run(["check"]);
    const taken = run(["check"], {
      responses: [
        {
          when: "run --rm -p 80:8080",
          stderr: "listen tcp4 0.0.0.0:80: bind: address already in use\n",
          code: 125,
        },
      ],
    });

    assert.equal(free.code, 0, free.stderr);
    assert.match(free.stdout, /the Caddy path fits/);
    assert.match(taken.stdout, /a program on the host/);
    assert.match(taken.stdout, /behind your existing proxy/);
    for (const result of [free, taken]) {
      assert.ok(!joined(result.calls).some((call) => /^compose (run|up|exec)/.test(call)));
    }
  });
});

/** A release's `server_image` and `agent_image`, pinned by digest. */
const RELEASED = {
  server: `ghcr.io/noah-gabel/afkfleet-server:0.1.0@sha256:${"a".repeat(64)}`,
  agent: `ghcr.io/noah-gabel/afkfleet-agent:0.1.0@sha256:${"b".repeat(64)}`,
};

/** A RELEASE file of version 0.1.0 with `images`, listing no file. */
function releaseText(images = RELEASED, version = "0.1.0") {
  return [
    "# afkfleet's release files (ADR-0022, S23), written by scripts/deploy-bundle.mjs. Never edit it.",
    `version=${version}`,
    `updates_from=${version}`,
    `server_image=${images.server}`,
    `agent_image=${images.agent}`,
    "",
  ].join("\n");
}

/** `.env` without the image lines, as `cp env.example .env` leaves it. */
const RELEASE_ENV = () =>
  env((lines) =>
    lines.filter(
      (line) =>
        !line.startsWith("AFKFLEET_SERVER_IMAGE=") && !line.startsWith("AFKFLEET_AGENT_IMAGE="),
    ),
  );

describe("setup.sh with a release's files, against stubs", { skip: SH }, () => {
  /** A folder holding RELEASE (its text `text`). */
  const withRelease =
    (text = releaseText()) =>
    (dir) =>
      writeFileSync(join(dir, "RELEASE"), text);

  it("takes the images from RELEASE, with no image line in .env", () => {
    const result = run(["init"], { envText: RELEASE_ENV(), prepare: withRelease() });

    assert.equal(result.code, 0, result.stderr);
    assert.ok(joined(result.calls).includes(`image inspect ${RELEASED.server}`));
    assert.ok(joined(result.calls).includes(`image inspect ${RELEASED.agent}`));
    assert.doesNotMatch(result.stdout, /warning/);
  });

  it("runs a .env image instead of RELEASE's, saying so", () => {
    const other = "ghcr.io/owner/afkfleet-server:9.9.9";
    const result = run(["init"], {
      envText: env((lines) => [
        ...lines.filter((line) => !line.startsWith("AFKFLEET_SERVER_IMAGE=")),
        `AFKFLEET_SERVER_IMAGE=${other}`,
      ]),
      prepare: withRelease(),
    });

    assert.equal(result.code, 0, result.stderr);
    assert.match(
      result.stdout,
      /warning: \.env's AFKFLEET_SERVER_IMAGE runs ghcr\.io\/owner\/afkfleet-server:9\.9\.9/,
    );
    assert.ok(joined(result.calls).includes(`image inspect ${other}`));
    assert.ok(!joined(result.calls).includes(`image inspect ${RELEASED.server}`));
  });

  it("refuses an image variable in its own environment, which compose would prefer", () => {
    for (const name of ["AFKFLEET_SERVER_IMAGE", "AFKFLEET_AGENT_IMAGE"]) {
      const result = run(["check"], {
        envText: RELEASE_ENV(),
        prepare: withRelease(),
        extraEnv: { [name]: "afkfleet-server:dev" },
      });

      assert.equal(result.code, 1, name);
      assert.match(result.stderr, new RegExp(`${name} is set in this shell's environment`), name);
    }
  });

  it("refuses a RELEASE whose image isn't pinned by its digest", () => {
    for (const server of [
      "ghcr.io/noah-gabel/afkfleet-server:0.1.0",
      `ghcr.io/noah-gabel/afkfleet-server:0.1.0@sha256:${"a".repeat(63)}`,
      `ghcr.io/noah-gabel/afkfleet-server:0.1.0@sha256:${"A".repeat(64)}`,
      "",
    ]) {
      const result = run(["check"], {
        envText: RELEASE_ENV(),
        prepare: withRelease(releaseText({ ...RELEASED, server })),
      });

      assert.equal(result.code, 1, server);
      assert.match(
        result.stderr,
        /RELEASE's server_image isn't an image pinned by its digest/,
        server,
      );
    }
  });

  it("without RELEASE, still needs the image lines in .env", () => {
    const result = run(["check"], { envText: RELEASE_ENV() });

    assert.equal(result.code, 1);
    assert.match(result.stderr, /AFKFLEET_SERVER_IMAGE in \.env is missing/);
  });

  it("check and init refuse a mounted file the containers' user can't read, as unpacked under umask 077", () => {
    for (const file of MOUNTED) {
      for (const command of ["check", "init"]) {
        const result = run([command], { modes: { [file]: "1000:1000 600" } });

        assert.equal(result.code, 1, `${command} ${file}`);
        assert.match(result.stderr, new RegExp(`${file} must be readable by everyone`), file);
        assert.ok(!joined(result.calls).some((call) => /^compose (run|up|exec)/.test(call)));
      }
    }
  });

  it("update refuses while a fetch's folder is there, changing nothing", () => {
    const result = run(["update"], {
      prepare: (dir) => mkdirSync(join(dir, ".afkfleet-fetch")),
    });

    assert.equal(result.code, 1);
    assert.match(result.stderr, /\.afkfleet-fetch/);
    assert.ok(!joined(result.calls).some((call) => /^compose (exec|pull|up)/.test(call)));
  });

  /** The answer of `docker inspect`'s version label for the running server. */
  const label = (version) => ({
    when: 'inspect --format {{index .Config.Labels "org.opencontainers.image.version"}}',
    stdout: `${version}\n`,
  });

  it("update refuses a running server newer than RELEASE, before its backup", () => {
    const result = run(["update"], {
      envText: RELEASE_ENV(),
      prepare: withRelease(),
      responses: [label("0.2.0")],
    });

    assert.equal(result.code, 1);
    assert.match(
      result.stderr,
      /the running server is 0\.2\.0, newer than this folder's release 0\.1\.0/,
    );
    assert.match(result.stderr, /sh setup\.sh fetch 0\.2\.0/);
    assert.ok(!joined(result.calls).some((call) => /^compose (exec|pull|up)/.test(call)));
  });

  it("update goes on for a running server of this release or older, or a local build", () => {
    for (const [running, said] of [
      ["0.1.0", null],
      ["0.0.9", null],
      ["", /no release version \(a local build\): not compared/],
    ]) {
      const result = run(["update"], {
        envText: RELEASE_ENV(),
        prepare: withRelease(),
        responses: [label(running)],
      });

      assert.equal(result.code, 0, `${running}: ${result.stderr}`);
      if (said !== null) {
        assert.match(result.stdout, said);
      }
      assert.ok(joined(result.calls).includes("compose pull"), running);
    }
  });

  it("update refuses a running server whose version label isn't X.Y.Z", () => {
    const result = run(["update"], {
      envText: RELEASE_ENV(),
      prepare: withRelease(),
      responses: [label("latest")],
    });

    assert.equal(result.code, 1);
    assert.match(result.stderr, /version label/);
    assert.ok(!joined(result.calls).includes("compose pull"));
  });
});

/** A copy of what the bundle reads, its Cargo.toml set to `version` and `updates_from`. */
function bundleRoot(version, updatesFrom = version) {
  const root = mkdtempSync(join(tmpdir(), "afkfleet-root-"));
  made.push(root);
  for (const [, source] of BUNDLE_FILES) {
    mkdirSync(dirname(join(root, source)), { recursive: true });
    copyFileSync(join(ROOT, source), join(root, source));
  }
  const cargo = readFileSync(join(ROOT, "Cargo.toml"), "utf8")
    .replace(/^(\[workspace\.package\]\nversion = )"[^"]*"/m, `$1"${version}"`)
    .replace(/^updates_from = "[^"]*"/m, `updates_from = "${updatesFrom}"`);
  writeFileSync(join(root, "Cargo.toml"), cargo);
  return root;
}

/** A release's bundle, built by deploy-bundle.mjs, for `version`. */
function bundleOf(version, updatesFrom = version) {
  return buildBundle({
    root: bundleRoot(version, updatesFrom),
    version,
    created: "2026-11-07T12:00:00Z",
    serverDigest: `sha256:${"a".repeat(64)}`,
    agentDigest: `sha256:${"b".repeat(64)}`,
    owner: "noah-gabel",
  });
}

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

/** A folder serving `bundle` under `name` and a SHA256SUMS whose lines are `sums` (or its own). */
function serve(name, bytes, sums = [`${sha256(bytes)}  ${name}`]) {
  const dir = mkdtempSync(join(tmpdir(), "afkfleet-served-"));
  made.push(dir);
  writeFileSync(join(dir, name), bytes);
  // The installer's line first, as the release job writes it.
  writeFileSync(
    join(dir, "SHA256SUMS"),
    [`${"0".repeat(64)}  afkfleet_0.0.0_x64-setup.exe`, ...sums, ""].join("\n"),
  );
  return dir;
}

/** Unpacks `bundle` into `dir` with the system's tar, as the runbook's install does. */
function unpack(bundle, dir) {
  const file = join(dir, "installed.tar.gz");
  writeFileSync(file, bundle.bytes);
  const done = spawnSync("tar", ["-xzf", "installed.tar.gz"], { cwd: dir, encoding: "utf8" });
  assert.equal(done.status, 0, done.stderr);
  rmSync(file);
}

/** A tar.gz of `entries` ({ name, data? }), its names exactly as given. */
function crafted(entries) {
  return gzipSync(tarOf(entries, 1_793_793_600));
}

/** The release files in `dir`: each file's SHA-256, by name (the stubs' own files left out). */
function installedFiles(dir, release) {
  const names = release
    .split("\n")
    .filter((line) => line.startsWith("file="))
    .map((line) => line.slice("file=".length + 66));
  return Object.fromEntries(names.map((name) => [name, sha256(readFileSync(join(dir, name)))]));
}

describe("setup.sh fetch, against a stub curl", { skip: SH }, () => {
  const OLD = "0.1.0";
  const NEW = "0.2.0";
  const name = (version) => `afkfleet-deploy-${version}.tar.gz`;

  /** Fetches NEW over an install of OLD; `change(dir)` runs after the unpacking. */
  function fetchNew({
    offered = bundleOf(NEW, OLD),
    servedAs = NEW,
    change = () => {},
    sums,
  } = {}) {
    const installed = bundleOf(OLD);
    const served = serve(name(servedAs), offered.bytes, sums);
    const result = run(["fetch", servedAs], {
      prepare: (dir) => {
        unpack(installed, dir);
        change(dir);
      },
      served,
    });
    return { result, installed, offered };
  }

  it("fetches a newer release: checks its sum, moves every file in, RELEASE last, and says what's next", () => {
    const keep = {
      "compose.override.yaml": "services: {}\n",
      "ca.crt": "-----BEGIN CERTIFICATE-----\n",
    };
    const { result, offered } = fetchNew({
      change: (dir) => {
        for (const [file, text] of Object.entries(keep)) {
          writeFileSync(join(dir, file), text);
        }
      },
    });
    // .env is what run() wrote before the script ran.
    keep[".env"] = `${env()}AFKFLEET_BACKUPS_DIR=${join(result.dir, "backups")}\n`;

    assert.equal(result.code, 0, result.stderr);
    const release = readFileSync(join(result.dir, "RELEASE"), "utf8");
    assert.equal(release, offered.release);
    const expected = Object.fromEntries(
      offered.release
        .split("\n")
        .filter((line) => line.startsWith("file="))
        .map((line) => [
          line.slice("file=".length + 66),
          line.slice("file=".length, "file=".length + 64),
        ]),
    );
    assert.deepEqual(installedFiles(result.dir, release), expected);
    for (const [file, text] of Object.entries(keep)) {
      assert.equal(readFileSync(join(result.dir, file), "utf8"), text, file);
    }
    assert.match(result.stdout, /Next: sh setup\.sh update/);
    assert.ok(!existsSync(join(result.dir, ".afkfleet-fetch")), "its folder is gone");
    assert.equal(statSync(join(result.dir, "prod", "server.toml")).mode & 0o777, 0o644);
    assert.equal(statSync(join(result.dir, "prod")).mode & 0o777, 0o755);
  });

  it("downloads over HTTPS alone from the repository's releases, bounded", () => {
    const { result } = fetchNew();
    const urls = result.curls.map((args) => args.at(-1));

    assert.deepEqual(urls, [
      `${SOURCE}/releases/download/v${NEW}/SHA256SUMS`,
      `${SOURCE}/releases/download/v${NEW}/${name(NEW)}`,
    ]);
    for (const args of result.curls) {
      for (const flag of [
        ["--proto", "=https"],
        ["--proto-redir", "=https"],
        ["--tlsv1.2"],
        ["--fail"],
        ["--max-time", "300"],
        ["--max-filesize", "10485760"],
        ["--connect-timeout", "20"],
      ]) {
        const at = args.indexOf(flag[0]);
        assert.ok(at >= 0, flag[0]);
        if (flag.length === 2) {
          assert.equal(args[at + 1], flag[1], flag[0]);
        }
      }
    }
  });

  it("refuses an older or the same version, and a version that isn't X.Y.Z, downloading nothing", () => {
    for (const version of [
      "0.1.0",
      "0.0.9",
      "1.0",
      "01.2.0",
      "1.2.3.4",
      "1.2.x",
      "1234567890.0.0",
    ]) {
      const result = run(["fetch", version], { prepare: (dir) => unpack(bundleOf(OLD), dir) });

      assert.equal(result.code, 1, version);
      assert.deepEqual(result.curls, [], version);
    }
  });

  it("refuses a folder without RELEASE: a checkout isn't a release's files", () => {
    const result = run(["fetch", NEW]);

    assert.equal(result.code, 1);
    assert.match(result.stderr, /isn't a release's files/);
    assert.deepEqual(result.curls, []);
  });

  it("refuses while an earlier fetch's folder is there", () => {
    const { result } = fetchNew({ change: (dir) => mkdirSync(join(dir, ".afkfleet-fetch")) });

    assert.equal(result.code, 1);
    assert.match(result.stderr, /\.afkfleet-fetch exists/);
    assert.deepEqual(result.curls, []);
  });

  it("refuses a release that needs a newer install first (updates_from)", () => {
    const { result, installed } = fetchNew({ offered: bundleOf(NEW, "0.1.5") });

    assert.equal(result.code, 1);
    assert.match(result.stderr, /0\.2\.0 updates only from 0\.1\.5 or newer/);
    assert.equal(readFileSync(join(result.dir, "RELEASE"), "utf8"), installed.release);
  });

  it("refuses a download whose SHA-256 isn't SHA256SUMS', or isn't listed once, changing nothing", () => {
    const offered = bundleOf(NEW, OLD);
    for (const [label, sums] of [
      ["another sum", [`${"f".repeat(64)}  ${name(NEW)}`]],
      ["no line", []],
      [
        "two lines",
        [`${sha256(offered.bytes)}  ${name(NEW)}`, `${sha256(offered.bytes)}  ${name(NEW)}`],
      ],
    ]) {
      const { result, installed } = fetchNew({ offered, sums });

      assert.equal(result.code, 1, label);
      assert.match(result.stderr, /SHA256SUMS/, label);
      assert.equal(readFileSync(join(result.dir, "RELEASE"), "utf8"), installed.release, label);
      assert.match(result.stderr, /left in \.afkfleet-fetch/, label);
    }
  });

  it("refuses a failed download, leaving its folder and saying so", () => {
    const installed = bundleOf(OLD);
    const result = run(["fetch", NEW], {
      prepare: (dir) => unpack(installed, dir),
      served: serve("unrelated.tar.gz", Buffer.from("x")),
    });

    assert.equal(result.code, 1);
    assert.match(result.stderr, /couldn't be downloaded/);
    assert.match(result.stderr, /left in \.afkfleet-fetch/);
    assert.equal(readFileSync(join(result.dir, "RELEASE"), "utf8"), installed.release);
  });

  it("refuses an archive holding a link, a path that leaves the folder or a hidden name", () => {
    const linked = mkdtempSync(join(tmpdir(), "afkfleet-link-"));
    made.push(linked);
    writeFileSync(join(linked, "Caddyfile"), "x\n");
    symlinkSync("/etc/passwd", join(linked, "compose.yaml"));
    const link = spawnSync("tar", ["-czf", "-", "Caddyfile", "compose.yaml"], { cwd: linked });
    assert.equal(link.status, 0);
    for (const [label, bytes] of [
      ["a link", link.stdout],
      ["a parent's path", crafted([{ name: "../escape", data: Buffer.from("x") }])],
      ["an absolute path", crafted([{ name: "/etc/escape", data: Buffer.from("x") }])],
      ["a hidden name", crafted([{ name: ".env", data: Buffer.from("x") }])],
      ["a deeper folder", crafted([{ name: "prod/deeper/file", data: Buffer.from("x") }])],
      ["another folder", crafted([{ name: "other/" }])],
    ]) {
      const { result, installed } = fetchNew({ offered: { bytes } });

      assert.equal(result.code, 1, label);
      assert.match(result.stderr, /the bundle holds/, label);
      assert.equal(readFileSync(join(result.dir, "RELEASE"), "utf8"), installed.release, label);
      assert.ok(!existsSync(join(result.dir, ".afkfleet-fetch", "new", "escape")), label);
    }
  });

  /** NEW's bundle with `edit` applied to its entries, gzipped again. */
  function editedBundle(edit) {
    const entries = readEntries(gunzipSync(bundleOf(NEW, OLD).bytes));
    return { bytes: crafted(edit(entries)) };
  }

  it("refuses a RELEASE of another version, or one that doesn't list exactly its files", () => {
    for (const [label, edit, refusal] of [
      ["another version", (entries) => entries, /says version 0\.2\.0, not 0\.3\.0/],
      [
        "an unlisted file",
        (entries) => [...entries, { name: "extra.txt", data: Buffer.from("x\n") }],
        /doesn't list exactly its files/,
      ],
    ]) {
      const servedAs = label === "another version" ? "0.3.0" : NEW;
      const { result, installed } = fetchNew({ offered: editedBundle(edit), servedAs });

      assert.equal(result.code, 1, label);
      assert.match(result.stderr, refusal, label);
      assert.equal(readFileSync(join(result.dir, "RELEASE"), "utf8"), installed.release, label);
    }
  });

  it("refuses a release that would write .env, compose.override.yaml or ca.crt", () => {
    for (const file of ["compose.override.yaml", "ca.crt"]) {
      const { result } = fetchNew({
        offered: editedBundle((entries) => {
          const data = Buffer.from("x\n");
          const release = entries.find((entry) => entry.name === "RELEASE");
          release.data = Buffer.from(`${release.data}file=${sha256(data)}  ${file}\n`);
          return [...entries, { name: file, data }].sort((a, b) => (a.name < b.name ? -1 : 1));
        }),
      });

      assert.equal(result.code, 1, file);
      assert.match(
        result.stderr,
        new RegExp(`never replaces ${file.replaceAll(".", "\\.")}`),
        file,
      );
    }
  });

  it("refuses a release file changed here, naming it, and moves nothing", () => {
    const { result, installed } = fetchNew({
      change: (dir) => writeFileSync(join(dir, "prod", "server.toml"), "# changed here\n"),
    });

    assert.equal(result.code, 1);
    assert.match(result.stderr, /prod\/server\.toml differs from release 0\.1\.0's and 0\.2\.0's/);
    assert.equal(readFileSync(join(result.dir, "RELEASE"), "utf8"), installed.release);
    assert.equal(readFileSync(join(result.dir, "prod", "server.toml"), "utf8"), "# changed here\n");
  });

  it("refuses a link where a release file goes", () => {
    const { result } = fetchNew({
      change: (dir) => {
        rmSync(join(dir, "Caddyfile"));
        symlinkSync(join(dir, "env.example"), join(dir, "Caddyfile"));
      },
    });

    assert.equal(result.code, 1);
    assert.match(result.stderr, /Caddyfile is a link here/);
  });

  it("leaves a file the new release no longer ships, and names it", () => {
    const { result } = fetchNew({
      change: (dir) => {
        const data = "old\n";
        writeFileSync(join(dir, "old.txt"), data);
        writeFileSync(
          join(dir, "RELEASE"),
          `${readFileSync(join(dir, "RELEASE"), "utf8")}file=${sha256(Buffer.from(data))}  old.txt\n`,
        );
      },
    });

    assert.equal(result.code, 0, result.stderr);
    assert.equal(readFileSync(join(result.dir, "old.txt"), "utf8"), "old\n");
    assert.match(result.stdout, /old\.txt/);
  });

  it("an interrupted fetch leaves RELEASE as it was, and runs again once its folder is gone", () => {
    const installed = bundleOf(OLD);
    const offered = bundleOf(NEW, OLD);
    const served = serve(name(NEW), offered.bytes);
    let folder = null;
    const first = run(["fetch", NEW], {
      prepare: (dir) => {
        unpack(installed, dir);
        // The move into prod/ fails: its folder can't be written.
        chmodSync(join(dir, "prod"), 0o555);
        folder = dir;
      },
      served,
    });
    chmodSync(join(folder, "prod"), 0o755);

    assert.equal(first.code, 1);
    assert.equal(readFileSync(join(folder, "RELEASE"), "utf8"), installed.release);
    assert.match(first.stderr, /RELEASE still says 0\.1\.0/);

    // The runbook's line, run where the operator runs it.
    const removed = spawnSync("/bin/sh", ["-c", "rm -r .afkfleet-fetch"], {
      cwd: folder,
      encoding: "utf8",
    });
    assert.equal(removed.status, 0, removed.stderr);
    const again = spawnSync("/bin/sh", ["setup.sh", "fetch", NEW], {
      cwd: folder,
      env: {
        PATH: `${join(folder, "bin")}:${process.env.PATH}`,
        CURL_LOG: join(folder, "curl.jsonl"),
        STUB_SERVED: served,
        HOME: folder,
      },
      encoding: "utf8",
    });

    assert.equal(again.status, 0, again.stderr);
    assert.equal(readFileSync(join(folder, "RELEASE"), "utf8"), offered.release);
  });

  it("refuses without curl, downloading nothing", () => {
    const result = run(["fetch", NEW], {
      prepare: (dir) => unpack(bundleOf(OLD), dir),
      noCurl: true,
    });

    assert.equal(result.code, 1);
    assert.match(result.stderr, /curl isn't installed/);
    assert.ok(!existsSync(join(result.dir, ".afkfleet-fetch")));
  });
});

/** A tar's entries, { name, data } for files and { name } for folders: the bundle test's reader. */
function readEntries(tar) {
  const found = [];
  let at = 0;
  while (at + 512 <= tar.length) {
    const head = tar.subarray(at, at + 512);
    if (head.every((byte) => byte === 0)) {
      break;
    }
    const text = (offset, length) =>
      head
        .subarray(offset, offset + length)
        .toString("latin1")
        .replace(/\0.*$/s, "");
    const size = Number.parseInt(text(124, 12), 8);
    const name = text(0, 100);
    found.push(
      name.endsWith("/")
        ? { name }
        : { name, data: Buffer.from(tar.subarray(at + 512, at + 512 + size)) },
    );
    at += 512 + Math.ceil(size / 512) * 512;
  }
  return found;
}

describe("the runbook's install lines, on a real bundle", { skip: SH }, () => {
  /** The runbook's first lines: the install's folder, made and entered (S37). */
  const FOLDER_LINES = ["mkdir ~/afkfleet", "cd ~/afkfleet"];

  it("the runbook's folder lines make ~/afkfleet and enter it", () => {
    const home = mkdtempSync(join(tmpdir(), "afkfleet-home-"));
    made.push(home);
    const done = spawnSync("/bin/sh", ["-c", [...FOLDER_LINES, "pwd"].join("\n")], {
      cwd: tmpdir(),
      env: { ...process.env, HOME: home },
      encoding: "utf8",
    });

    assert.equal(done.status, 0, done.stderr);
    assert.equal(done.stdout, `${join(home, "afkfleet")}\n`);
    assert.ok(statSync(join(home, "afkfleet")).isDirectory());
  });

  /** The lines the runbook's install runs on the host, X.Y.Z and its version in place (S37). */
  const INSTALL_LINES = (version) => [
    `grep ' afkfleet-deploy-${version}.tar.gz$' SHA256SUMS | sha256sum -c -`,
    `tar -xzf afkfleet-deploy-${version}.tar.gz`,
    "cp env.example .env",
  ];

  it("check the bundle's sum, unpack it, and leave a folder whose check and init take RELEASE's images", () => {
    const version = "0.1.0";
    const bundle = bundleOf(version);
    const served = serve(`afkfleet-deploy-${version}.tar.gz`, bundle.bytes);
    const result = run(["init"], {
      envText: null,
      prepare: (dir) => {
        for (const file of ["SHA256SUMS", `afkfleet-deploy-${version}.tar.gz`]) {
          copyFileSync(join(served, file), join(dir, file));
        }
        for (const line of INSTALL_LINES(version)) {
          const done = spawnSync("/bin/sh", ["-c", line], { cwd: dir, encoding: "utf8" });
          assert.equal(done.status, 0, `${line}: ${done.stderr}`);
          if (line.startsWith("grep")) {
            assert.equal(done.stdout, `afkfleet-deploy-${version}.tar.gz: OK\n`);
          }
        }
        // "Edit .env": the operator's values in place of env.example's placeholders.
        const text = readFileSync(join(dir, ".env"), "utf8")
          .replace("AFKFLEET_DOMAIN=fleet.example.com", "AFKFLEET_DOMAIN=fleet.afkfleet.test")
          .replace(
            "AFKFLEET_ACME_EMAIL=admin@example.com",
            "AFKFLEET_ACME_EMAIL=ops@afkfleet.test",
          );
        writeFileSync(join(dir, ".env"), `${text}AFKFLEET_BACKUPS_DIR=${join(dir, "backups")}\n`);
      },
    });

    assert.equal(result.code, 0, result.stderr);
    assert.ok(joined(result.calls).includes(`image inspect ${RELEASED.server}`));
    assert.ok(joined(result.calls).includes(`image inspect ${RELEASED.agent}`));
    assert.match(result.stdout, /afkfleet is running/);
  });

  it("the runbook's lines refuse a bundle whose sum isn't SHA256SUMS'", () => {
    const version = "0.1.0";
    const bundle = bundleOf(version);
    const served = serve(`afkfleet-deploy-${version}.tar.gz`, bundle.bytes, [
      `${"f".repeat(64)}  afkfleet-deploy-${version}.tar.gz`,
    ]);
    const done = spawnSync("/bin/sh", ["-c", INSTALL_LINES(version)[0]], {
      cwd: served,
      encoding: "utf8",
    });

    assert.notEqual(done.status, 0);
  });
});

/** `agent list`'s header, as fleet-server prints it (cli::agent::LIST_HEADER). */
const AGENTS_HEADER =
  "id\tname\tstate\tbots\tcapacity\tversion\tlast_seen_at\tnot_after\tcreated_at";

/** An `agent list` of `rows`, each [id, name, state]. */
function agentList(rows) {
  const lines = rows.map(([id, name, state]) => `${id}\t${name}\t${state}\t0\t50\t-\t-\t-\t-`);
  return `${[AGENTS_HEADER, ...lines].join("\n")}\n`;
}

const NAME = "afkfleet-backup-20261106T000000Z.db";
/** A stamp as setup.sh makes one. */
const STAMPED = /\b\d{8}T\d{6}Z\b/;

/** The compose calls of `result` that run, start, stop or exec something. */
function steps(result) {
  return joined(result.calls).filter((call) => /^compose (run|up|stop|exec|kill)/.test(call));
}

/** `steps(result)` with each stamp as STAMP. */
function stamped(result) {
  return steps(result).map((call) => call.replace(STAMPED, "STAMP"));
}

describe("setup.sh restore, forget-aside and renew-cert, against stubs", { skip: SH }, () => {
  it("restore with the server healthy: saves the agents, stops, restores, re-disables, starts", () => {
    const result = run(["restore", NAME], {
      responses: [
        {
          when: "compose exec -T server /usr/local/bin/afkfleet-server agent list",
          stdout: agentList([
            ["a1", "lost", "disabled"],
            ["a2", "local", "enabled"],
          ]),
        },
        {
          when: "compose run --rm -T maintenance agent list",
          stdout: agentList([
            ["a1", "lost", "enabled"],
            ["a2", "local", "enabled"],
          ]),
        },
      ],
    });

    assert.equal(result.code, 0, result.stderr);
    assert.deepEqual(stamped(result), [
      "compose exec -T server /usr/local/bin/afkfleet-server agent list --config /etc/afkfleet/server.toml",
      "compose run --rm -T maintenance --version",
      "compose stop agent server",
      `compose run --rm -T files restore-in STAMP ${NAME}`,
      "compose run --rm -T maintenance migrate --config /etc/afkfleet/server.toml",
      "compose run --rm -T files finished STAMP",
      "compose run --rm -T maintenance vault status --config /etc/afkfleet/server.toml",
      "compose run --rm -T maintenance agent list --config /etc/afkfleet/server.toml",
      "compose run --rm -T maintenance agent disable --config /etc/afkfleet/server.toml --id a1",
      "compose up --detach --wait",
    ]);
    assert.match(result.stdout, /sh setup\.sh forget-aside \d{8}T\d{6}Z/);
    assert.match(result.stdout, /disable users again/);
  });

  it("restore after a crash, with no healthy server, ends stopped and says how to start", () => {
    const result = run(["restore", `/backups/${NAME}`], {
      responses: [{ when: "compose ps -q server", stdout: "" }],
    });

    assert.equal(result.code, 0, result.stderr);
    assert.deepEqual(stamped(result), [
      "compose run --rm -T maintenance --version",
      "compose stop agent server",
      `compose run --rm -T files restore-in STAMP ${NAME}`,
      "compose run --rm -T maintenance migrate --config /etc/afkfleet/server.toml",
      "compose run --rm -T files finished STAMP",
      "compose run --rm -T maintenance vault status --config /etc/afkfleet/server.toml",
      "compose run --rm -T maintenance agent list --config /etc/afkfleet/server.toml",
    ]);
    assert.match(result.stdout, /docker compose up --detach --wait/);
    assert.match(result.stdout, /agent disable/);
    assert.match(result.stdout, /sh setup\.sh forget-aside \d{8}T\d{6}Z/);
  });

  it("restore refuses an agent list it can't read, changing nothing", () => {
    const result = run(["restore", NAME], {
      responses: [
        {
          when: "compose exec -T server /usr/local/bin/afkfleet-server agent list",
          stdout: "id\tname\n",
        },
      ],
    });

    assert.equal(result.code, 1);
    assert.match(result.stderr, /agent list can't be read/);
    assert.deepEqual(stamped(result), [
      "compose exec -T server /usr/local/bin/afkfleet-server agent list --config /etc/afkfleet/server.toml",
    ]);
  });

  it("restore refuses another name, a partial copy or a path, changing nothing", () => {
    for (const name of [
      "afkfleet.db",
      `${NAME}.partial`,
      `.${NAME}`,
      `/srv/${NAME}`,
      `../${NAME}`,
      "afkfleet-backup-2026110T000000Z.db",
    ]) {
      const result = run(["restore", name]);

      assert.equal(result.code, 1, name);
      assert.match(result.stderr, /a backup's name/, name);
      assert.deepEqual(steps(result), [], name);
    }
  });

  it("restore refuses while a fetch's folder is there, or without the key volumes", () => {
    const fetching = run(["restore", NAME], {
      prepare: (dir) => mkdirSync(join(dir, ".afkfleet-fetch")),
    });
    const noKeys = run(["restore", NAME], {
      responses: [{ when: "volume inspect afkfleet_keys", code: 1 }],
    });

    for (const result of [fetching, noKeys]) {
      assert.equal(result.code, 1);
      assert.deepEqual(steps(result), []);
    }
    assert.match(noKeys.stderr, /keys/);
  });

  /** The healthy server's agent list: one enabled agent. */
  const SAVED = {
    when: "compose exec -T server /usr/local/bin/afkfleet-server agent list",
    stdout: agentList([["a2", "local", "enabled"]]),
  };

  it("restore stops with the server down when migrate refuses, naming put-back", () => {
    const result = run(["restore", NAME], {
      responses: [SAVED, { when: "compose run --rm -T maintenance migrate", code: 1 }],
    });

    assert.equal(result.code, 1);
    assert.match(result.stderr, /newer server/);
    assert.match(result.stderr, /files put-back \d{8}T\d{6}Z/);
    assert.ok(!steps(result).some((call) => call.startsWith("compose up")));
    assert.ok(!steps(result).some((call) => call.includes("files finished")), "marked finished");
  });

  it("restore's failures after the stop keep the server stopped and give put-back, never forget-aside", () => {
    const disabledBefore = {
      when: "compose exec -T server /usr/local/bin/afkfleet-server agent list",
      stdout: agentList([["a1", "lost", "disabled"]]),
    };
    const enabledInBackup = {
      when: "compose run --rm -T maintenance agent list",
      stdout: agentList([["a1", "lost", "enabled"]]),
    };
    for (const [label, responses] of [
      ["restore-in", [SAVED, { when: "compose run --rm -T files restore-in", code: 1 }]],
      ["vault status", [SAVED, { when: "compose run --rm -T maintenance vault status", code: 1 }]],
      ["the agent list", [SAVED, { when: "compose run --rm -T maintenance agent list", code: 1 }]],
      [
        "an agent's disable",
        [
          disabledBefore,
          enabledInBackup,
          { when: "compose run --rm -T maintenance agent disable", code: 1 },
        ],
      ],
    ]) {
      const result = run(["restore", NAME], { responses });

      assert.equal(result.code, 1, label);
      assert.match(result.stderr, /the server stays stopped/, label);
      assert.match(result.stderr, /docker compose run --rm files put-back \d{8}T\d{6}Z/, label);
      assert.doesNotMatch(result.stderr, /forget-aside/, label);
      assert.ok(!steps(result).some((call) => call.startsWith("compose up")), label);
    }
  });

  it("restore on a new host, with no server container, ends with init as its next step", () => {
    const result = run(["restore", NAME], {
      responses: [
        { when: "compose ps -q server", stdout: "" },
        { when: "compose ps -a -q server", stdout: "" },
      ],
    });

    assert.equal(result.code, 0, result.stderr);
    assert.match(result.stdout, /sudo sh setup\.sh init/);
    assert.doesNotMatch(result.stdout, /docker compose up/);
    assert.ok(!steps(result).some((call) => call.startsWith("compose up")));
  });

  it("restore stops with the server down when a vault key is missing (vault status's exit 3)", () => {
    const result = run(["restore", NAME], {
      responses: [SAVED, { when: "compose run --rm -T maintenance vault status", code: 3 }],
    });

    assert.equal(result.code, 1);
    assert.match(result.stderr, /vault key/);
    assert.ok(!steps(result).some((call) => call.startsWith("compose up")));
  });

  it("forget-aside lists the aside files without a stamp, and forgets one stamp's with the server healthy", () => {
    const list = run(["forget-aside"]);
    const forget = run(["forget-aside", "20261107T120000Z"]);

    assert.equal(list.code, 0, list.stderr);
    assert.deepEqual(steps(list), ["compose run --rm -T files list"]);
    assert.equal(forget.code, 0, forget.stderr);
    assert.deepEqual(steps(forget), ["compose run --rm -T files forget 20261107T120000Z"]);
  });

  it("forget-aside refuses a malformed stamp, and a server that isn't healthy", () => {
    const malformed = run(["forget-aside", "2026-11-07"]);
    const unhealthy = run(["forget-aside", "20261107T120000Z"], {
      responses: [{ when: "inspect --format {{.State.Health.Status}}", stdout: "starting\n" }],
    });

    for (const result of [malformed, unhealthy]) {
      assert.equal(result.code, 1);
      assert.deepEqual(steps(result), []);
    }
  });

  it("renew-cert moves the pair aside, issues a new one for the same names, and starts", () => {
    const result = run(["renew-cert"]);

    assert.equal(result.code, 0, result.stderr);
    assert.deepEqual(stamped(result), [
      "compose stop server",
      "compose run --rm -T files tls-aside STAMP",
      "compose run --rm -T setup pki server-cert --config /etc/afkfleet/server.toml --dns server --dns fleet.afkfleet.test --ip 192.0.2.10",
      "compose run --rm -T files finished STAMP",
      "compose up --detach --wait",
    ]);
    assert.match(result.stdout, /sh setup\.sh forget-aside \d{8}T\d{6}Z/);
  });

  it("renew-cert puts the old pair back and starts the server when the new one fails", () => {
    const result = run(["renew-cert"], {
      responses: [{ when: "compose run --rm -T setup pki server-cert", code: 1 }],
    });

    assert.equal(result.code, 1);
    assert.deepEqual(stamped(result).slice(-2), [
      "compose run --rm -T files tls-back STAMP",
      "compose up --detach --wait server",
    ]);
    assert.match(result.stderr, /the old certificate is back/);
  });

  it("renew-cert refuses without the server's container or the certificate's volume", () => {
    const noServer = run(["renew-cert"], {
      responses: [{ when: "compose ps -a -q server", stdout: "" }],
    });
    const noTls = run(["renew-cert"], {
      responses: [{ when: "volume inspect afkfleet_tls", code: 1 }],
    });

    for (const result of [noServer, noTls]) {
      assert.equal(result.code, 1);
      assert.deepEqual(steps(result), []);
    }
  });
});

describe("Docker Engine 28.0 or newer (U40), against stubs", { skip: SH }, () => {
  it("check and init refuse an older Engine, naming its version, and one whose version can't be read", () => {
    for (const [answer, said] of [
      ["27.5.1\n", /Docker Engine 27\.5\.1 is too old: afkfleet needs 28\.0 or newer/],
      ["\n", /Docker Engine's version can't be read/],
      ["dev\n", /Docker Engine's version can't be read/],
    ]) {
      for (const command of ["check", "init"]) {
        const result = run([command], {
          responses: [{ when: "version --format {{.Server.Version}}", stdout: answer }],
        });

        assert.equal(result.code, 1, `${command} ${answer}`);
        assert.match(result.stderr, said, answer);
        assert.ok(!joined(result.calls).some((call) => /^compose (run|up)/.test(call)));
      }
    }
  });

  it("takes 28.0", () => {
    const result = run(["check"], {
      responses: [{ when: "version --format {{.Server.Version}}", stdout: "28.0.0\n" }],
    });

    assert.equal(result.code, 0, result.stderr);
    assert.match(result.stdout, /ok: Docker 28\.0\.0 and Compose 2\.38\.2/);
  });
});

/** The keys' copy's files, as `docker compose cp` writes them for the stub. */
const KEYS = {
  "ca.key": "-----BEGIN PRIVATE KEY-----\nsecret-ca-key\n",
  "vault-key-1": "secret-vault-key\n",
};
const CA = { "ca.crt": "-----BEGIN CERTIFICATE-----\npublic-ca-crt\n" };

/** The copy's answers: each volume's files written into the destination. */
const COPIES = [
  { when: "compose cp server:/keys/.", writes: KEYS },
  { when: "compose cp server:/ca/.", writes: CA },
];

describe("setup.sh keys-backup and keys-restore, against stubs", { skip: SH }, () => {
  /** keys-backup as root into `dest(folder)`, everything in the copy root's 600/700 by default. */
  function backupInto(dest, options = {}) {
    return run(["keys-backup", ...(dest === null ? [] : [dest])], {
      uid: "0",
      responses: [...(options.responses ?? []), ...COPIES],
      // A case's own patterns first: the stub's `case` takes the first match.
      patterns: [
        ...(options.patterns ?? []),
        ["*/copy", "0:0 700"],
        ["*/copy/keys", "0:0 700"],
        ["*/copy/ca", "0:0 700"],
        ["*/copy/*/*", "0:0 600"],
      ],
      ...options.run,
    });
  }

  it("keys-backup copies the keys and the CA from the server's container, never to stdout, and prints names alone", () => {
    const dir = mkdtempSync(join(tmpdir(), "afkfleet-keys-"));
    made.push(dir);
    const copied = backupInto(join(dir, "copy"));

    assert.equal(copied.code, 0, copied.stderr);
    const cps = joined(copied.calls).filter((call) => call.startsWith("compose cp"));
    assert.deepEqual(cps, [
      `compose cp server:/keys/. ${join(dir, "copy", "keys")}`,
      `compose cp server:/ca/. ${join(dir, "copy", "ca")}`,
    ]);
    assert.ok(!copied.calls.some((call) => call.args.at(-1) === "-"), "never to stdout");
    assert.match(copied.stdout, /keys\/ca\.key/);
    assert.match(copied.stdout, /keys\/vault-key-1/);
    assert.match(copied.stdout, /ca\/ca\.crt/);
    assert.doesNotMatch(copied.stdout + copied.stderr, /secret-/, "a key's text was printed");
  });

  it("keys-backup refuses without root, an existing folder, or a folder above that others can write", () => {
    const dir = mkdtempSync(join(tmpdir(), "afkfleet-keys-"));
    made.push(dir);
    mkdirSync(join(dir, "taken"));
    for (const [label, result, said] of [
      ["no root", run(["keys-backup", join(dir, "copy")], { responses: COPIES }), /runs as root/],
      ["an existing folder", backupInto(join(dir, "taken")), /exists already/],
      [
        "a folder above others can write",
        backupInto(join(dir, "copy"), { run: { parentStat: "0:0 777" } }),
        /every folder above/,
      ],
      [
        "a folder above that isn't root's",
        backupInto(join(dir, "copy"), { run: { parentStat: "1000:1000 755" } }),
        /every folder above/,
      ],
    ]) {
      assert.equal(result.code, 1, label);
      assert.match(result.stderr, said, label);
      assert.ok(!joined(result.calls).some((call) => call.startsWith("compose cp")), label);
    }
  });

  it("keys-backup refuses a folder inside the backups' SFTP folder, and an install without a server", () => {
    const dir = mkdtempSync(join(tmpdir(), "afkfleet-keys-"));
    made.push(dir);
    const chrootInside = run(["keys-backup", join(dir, "srv", "keys")], {
      uid: "0",
      responses: COPIES,
      backupsDir: () => join(dir, "srv", "backups"),
      makeBackups: false,
    });
    const noServer = backupInto(join(dir, "copy"), {
      responses: [{ when: "compose ps -a -q server", stdout: "" }],
    });

    assert.equal(chrootInside.code, 1);
    assert.match(chrootInside.stderr, /inside the backups' SFTP folder/);
    assert.equal(noServer.code, 1);
    assert.match(noServer.stderr, /no container/);
  });

  it("keys-backup refuses a copied file that isn't root's 600, or a copy without a needed file", () => {
    const dir = mkdtempSync(join(tmpdir(), "afkfleet-keys-"));
    made.push(dir);
    const loose = backupInto(join(dir, "copy"), {
      patterns: [["*/copy/keys/ca.key", "0:0 644"]],
    });
    const missing = run(["keys-backup", join(dir, "copy2")], {
      uid: "0",
      responses: [{ when: "compose cp server:/keys/.", writes: { "ca.key": "x\n" } }, COPIES[1]],
      patterns: [
        ["*/copy2", "0:0 700"],
        ["*/copy2/keys", "0:0 700"],
        ["*/copy2/ca", "0:0 700"],
        ["*/copy2/*/*", "0:0 600"],
      ],
    });

    assert.equal(loose.code, 1);
    assert.match(loose.stderr, /keys\/ca\.key must belong to root with mode 600/);
    assert.equal(missing.code, 1);
    assert.match(missing.stderr, /has no keys\/vault-key-1/);
  });

  /** A root-only copy in a folder of its own, as keys-backup leaves one; `change` edits it. */
  function copyFolder(change = () => {}) {
    const dir = mkdtempSync(join(tmpdir(), "afkfleet-keys-"));
    made.push(dir);
    const copy = join(dir, "copy");
    mkdirSync(join(copy, "keys"), { recursive: true });
    mkdirSync(join(copy, "ca"));
    for (const [name, text] of Object.entries(KEYS)) {
      writeFileSync(join(copy, "keys", name), text);
    }
    for (const [name, text] of Object.entries(CA)) {
      writeFileSync(join(copy, "ca", name), text);
    }
    change(copy);
    return copy;
  }

  /** keys-restore as root of `copy`, every file root's 600 and folder 700 unless `patterns` say. */
  function restoreFrom(copy, options = {}) {
    return run(["keys-restore", copy], {
      uid: "0",
      responses: [
        ...(options.responses ?? []),
        { when: "compose ps -a -q setup", stdout: "5e7up000\n" },
      ],
      patterns: [
        ...(options.patterns ?? []),
        [`${copy}`, "0:0 700"],
        [`${copy}/keys`, "0:0 700"],
        [`${copy}/ca`, "0:0 700"],
        [`${copy}/*/*`, "0:0 600"],
      ],
    });
  }

  it("keys-restore stages the copy as the server's user, copies it in with -a, then makes a server certificate", () => {
    const copy = copyFolder();

    const result = restoreFrom(copy);

    assert.equal(result.code, 0, result.stderr);
    const docker = joined(result.calls).filter((call) =>
      /^(compose --profile setup create|cp -a|rm -f|compose run)/.test(call),
    );
    assert.deepEqual(docker, [
      "compose --profile setup create setup",
      "cp -a .afkfleet-keys-restore/keys/. 5e7up000:/keys",
      "cp -a .afkfleet-keys-restore/ca/. 5e7up000:/ca",
      "rm -f 5e7up000",
      "compose run --rm -T setup pki server-cert --config /etc/afkfleet/server.toml --dns server --dns fleet.afkfleet.test --ip 192.0.2.10",
    ]);
    const changed = readFileSync(join(result.dir, "install.log"), "utf8");
    assert.match(changed, /chown 65532:65532 .*\.afkfleet-keys-restore\/keys\/ca\.key/);
    assert.ok(
      !existsSync(join(result.dir, ".afkfleet-keys-restore")),
      "the staging folder is gone",
    );
    assert.match(result.stdout, /sh setup\.sh restore/);
  });

  it("keys-restore refuses without root, a loose copy, an unknown file, a missing one, or a project that isn't clean", () => {
    const cases = [
      ["no root", run(["keys-restore", copyFolder()]), /runs as root/],
      [
        "a loose folder",
        restoreFrom(copyFolder(), { patterns: [["*/copy/keys", "0:0 755"]] }),
        /must belong to root with mode 700/,
      ],
      [
        "a loose file",
        restoreFrom(copyFolder(), { patterns: [["*/copy/ca/ca.crt", "1000:1000 600"]] }),
        /must belong to root with mode 600/,
      ],
      [
        "an unknown file",
        restoreFrom(copyFolder((copy) => writeFileSync(join(copy, "keys", "server.key"), "x"))),
        /keys\/server\.key isn't a file of the keys' copy/,
      ],
      [
        "a missing file",
        restoreFrom(copyFolder((copy) => rmSync(join(copy, "ca", "ca.crt")))),
        /has no ca\/ca\.crt/,
      ],
      [
        "a project with volumes",
        restoreFrom(copyFolder(), {
          responses: [
            {
              when: "volume ls -q --filter label=com.docker.compose.project=afkfleet",
              stdout: "afkfleet_keys\n",
            },
          ],
        }),
        /a first install only|not a clean/,
      ],
    ];
    for (const [label, result, said] of cases) {
      assert.equal(result.code, 1, label);
      assert.match(result.stderr, said, label);
      assert.ok(!joined(result.calls).some((call) => call.startsWith("cp -a")), label);
      assert.ok(!existsSync(join(result.dir, ".afkfleet-keys-restore")), `${label}: staging left`);
    }
  });

  it("keys-restore removes its staging folder when the copy into the volume fails", () => {
    const result = restoreFrom(copyFolder(), {
      responses: [{ when: "cp -a .afkfleet-keys-restore/keys/.", code: 1 }],
    });

    assert.equal(result.code, 1);
    assert.ok(!existsSync(join(result.dir, ".afkfleet-keys-restore")));
    assert.match(result.stderr, /this run created/);
  });

  it("init on a restored state skips the vault key and the PKI, and enrolls the local agent", () => {
    const result = run(["init"], {
      responses: [
        {
          when: "volume ls -q --filter label=com.docker.compose.project=afkfleet",
          stdout: "afkfleet_tls\nafkfleet_keys\nafkfleet_data\nafkfleet_ca\n",
        },
      ],
    });

    assert.equal(result.code, 0, result.stderr);
    assert.deepEqual(
      joined(result.calls).filter((call) => /^compose (run|up|exec)/.test(call)),
      [
        "compose up --detach --wait server",
        "compose exec -T server /usr/local/bin/afkfleet-server agent create-enrollment-token --config /etc/afkfleet/server.toml",
        "compose run --rm -T --no-deps agent enroll --config /etc/afkfleet/agent.toml",
        "compose up --detach --wait",
      ],
    );
    assert.match(result.stdout, /restored/);
  });

  it("init refuses another set of volumes, or containers beside them", () => {
    for (const [label, responses] of [
      [
        "with the agent's identity",
        [
          {
            when: "volume ls -q",
            stdout:
              "afkfleet_ca\nafkfleet_data\nafkfleet_keys\nafkfleet_tls\nafkfleet_agent-data\n",
          },
        ],
      ],
      [
        "without the database",
        [{ when: "volume ls -q", stdout: "afkfleet_ca\nafkfleet_keys\nafkfleet_tls\n" }],
      ],
      [
        "with a container",
        [
          {
            when: "volume ls -q",
            stdout: "afkfleet_ca\nafkfleet_data\nafkfleet_keys\nafkfleet_tls\n",
          },
          { when: "ps -a -q --filter", stdout: "0123abcd\n" },
        ],
      ],
    ]) {
      const result = run(["init"], { responses });

      assert.equal(result.code, 1, label);
      assert.ok(!joined(result.calls).some((call) => /^compose (run|up|exec)/.test(call)), label);
    }
  });
});

/** A remote agent's `.env`, as the runbook's Agents on other hosts gives it. */
const AGENT_ENV = () =>
  [
    "COMPOSE_FILE=compose.agent.yaml",
    "AFKFLEET_AGENT__NAME=agent-2",
    "AFKFLEET_AGENT__CONTROL_PLANE__URL=https://fleet.afkfleet.test:7443",
    "AFKFLEET_CA_CERT_FILE=./ca.crt",
    "",
  ].join("\n");

/** The docker answers for a remote agent installed here, running the release `version`. */
const AGENT_RUNS = (version = "0.1.0") => [
  { when: "compose ps -a -q agent", stdout: "4567cdef\n" },
  { when: "compose ps -q agent", stdout: "4567cdef\n" },
  {
    when: 'inspect --format {{index .Config.Labels "org.opencontainers.image.version"}}',
    stdout: `${version}\n`,
  },
];

describe("setup.sh in a remote agent's folder (PR #99's review), against stubs", {
  skip: SH,
}, () => {
  /** A remote agent's folder: release 0.1.0's RELEASE, and the agent's config. */
  const agentFolder = (dir) => {
    writeFileSync(join(dir, "RELEASE"), releaseText());
    copyFileSync(join(DEPLOY, "prod", "agent.remote.toml"), join(dir, "prod", "agent.remote.toml"));
  };

  it("update in a remote agent's folder pulls and starts the agent, with no backup", () => {
    const result = run(["update"], {
      envText: AGENT_ENV(),
      prepare: agentFolder,
      responses: AGENT_RUNS(),
    });

    assert.equal(result.code, 0, result.stderr);
    assert.deepEqual(
      joined(result.calls).filter((call) => /^compose (exec|pull|up|run|stop)/.test(call)),
      ["compose pull", "compose up --detach --wait"],
    );
    assert.match(result.stdout, /ok: the agent is updated/);
  });

  it("update in a remote agent's folder refuses a running agent newer than RELEASE, changing nothing", () => {
    const result = run(["update"], {
      envText: AGENT_ENV(),
      prepare: agentFolder,
      responses: AGENT_RUNS("0.2.0"),
    });

    assert.equal(result.code, 1);
    assert.match(
      result.stderr,
      /the running agent is 0\.2\.0, newer than this folder's release 0\.1\.0/,
    );
    assert.ok(!joined(result.calls).some((call) => /^compose (pull|up)/.test(call)));
  });

  it("update in a remote agent's folder refuses where no agent is installed, and while a fetch's folder is there", () => {
    const none = run(["update"], {
      envText: AGENT_ENV(),
      prepare: agentFolder,
      responses: [{ when: "compose ps -a -q agent", stdout: "" }],
    });
    const fetching = run(["update"], {
      envText: AGENT_ENV(),
      prepare: (dir) => {
        agentFolder(dir);
        mkdirSync(join(dir, ".afkfleet-fetch"));
      },
      responses: AGENT_RUNS(),
    });

    assert.equal(none.code, 1);
    assert.match(none.stderr, /the agent isn't installed here/);
    assert.equal(fetching.code, 1);
    assert.match(fetching.stderr, /\.afkfleet-fetch/);
    for (const result of [none, fetching]) {
      assert.ok(!joined(result.calls).some((call) => /^compose (pull|up)/.test(call)));
    }
  });

  it("check in a remote agent's folder checks the agent's image alone, and no port", () => {
    const result = run(["check"], {
      envText: AGENT_ENV(),
      prepare: agentFolder,
      responses: AGENT_RUNS(),
    });

    assert.equal(result.code, 0, result.stderr);
    assert.ok(joined(result.calls).includes(`image inspect ${RELEASED.agent}`));
    assert.ok(!joined(result.calls).includes(`image inspect ${RELEASED.server}`));
    assert.ok(!joined(result.calls).some((call) => call.startsWith("run --rm -p")));
    assert.match(
      result.stdout,
      /a remote agent is installed here: update it with sh setup\.sh update/,
    );
  });

  it("a remote agent's folder refuses what only the server's host does", () => {
    for (const args of [
      ["init"],
      ["restore", "afkfleet-backup-20261106T000000Z.db"],
      ["forget-aside"],
      ["renew-cert"],
      ["keys-backup"],
      ["keys-restore", "/root/afkfleet-keys-20261107T120000Z"],
    ]) {
      const result = run(args, {
        envText: AGENT_ENV(),
        prepare: agentFolder,
        responses: AGENT_RUNS(),
        uid: "0",
      });

      assert.equal(result.code, 1, args[0]);
      assert.match(result.stderr, /this folder runs a remote agent/, args[0]);
      assert.ok(
        !joined(result.calls).some((call) => /^compose (exec|pull|up|run|stop|cp)/.test(call)),
        args[0],
      );
    }
  });

  it("update refuses a RELEASE whose version can't be read, before anything changes", () => {
    for (const [label, text] of [
      ["no version line", releaseText().replace("version=0.1.0\n", "")],
      ["an empty version", releaseText().replace("version=0.1.0", "version=")],
      ["not X.Y.Z", releaseText().replace("version=0.1.0", "version=1.0")],
    ]) {
      const result = run(["update"], {
        envText: RELEASE_ENV(),
        prepare: (dir) => writeFileSync(join(dir, "RELEASE"), text),
        responses: [
          {
            when: 'inspect --format {{index .Config.Labels "org.opencontainers.image.version"}}',
            stdout: "0.1.0\n",
          },
        ],
      });

      assert.equal(result.code, 1, label);
      assert.match(result.stderr, /RELEASE's version can't be read/, label);
      assert.ok(!joined(result.calls).some((call) => /^compose (exec|pull|up)/.test(call)), label);
    }
  });
});
