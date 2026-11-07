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
import { appendFileSync, readFileSync } from "node:fs";
const args = process.argv.slice(2);
let stdin = "";
try { stdin = readFileSync(0, "utf8"); } catch {}
appendFileSync(process.env.STUB_LOG, JSON.stringify({ args, stdin }) + "\\n");
const responses = JSON.parse(readFileSync(process.env.STUB_RESPONSES, "utf8"));
const joined = args.join(" ");
const found = responses.find((response) => joined.startsWith(response.when)) ?? {};
process.stdout.write(found.stdout ?? "");
process.stderr.write(found.stderr ?? "");
process.exit(found.code ?? 0);
`;

/** The answers a healthy host gives; a case puts its own first. */
const HEALTHY = [
  { when: "compose version --short", stdout: "2.38.2\n" },
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
    stat: `#!/bin/sh\nfor last; do :; done\ncase $last in\n"${backups}") echo '${stat}' ;;\n${named}*) echo '${parentStat}' ;;\nesac\n`,
    install: `#!/bin/sh\necho "install $*" >> "${join(dir, "install.log")}"\n`,
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

    rmSync(join(folder, ".afkfleet-fetch"), { recursive: true, force: true });
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
