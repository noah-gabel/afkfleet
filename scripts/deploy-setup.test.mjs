// Tests for deploy/setup.sh (ADR-0022, S18, U8), and the POSIX syntax of every script an
// operator runs on the host. Run with `just scripts-test`.
//
// The cases that run the script use POSIX sh against stubs of `docker`, `id`, `stat` and
// `install` first on PATH, in a temporary folder: never a real Docker, never a real host (the
// user's rule). The stub records every call's arguments and the bytes it got on stdin. Windows
// has no POSIX sh under PowerShell, so those cases run in CI's Linux job, under dash; the
// check of the scripts' syntax runs everywhere. env.example and the compose files have their
// own tests in fleet-server's tests/compose.rs (U18); deploy/host/sftp-backups.sh runs in a
// throwaway sshd container in fleet-integration's slow_sftp_backups_account (S15).
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import process from "node:process";
import { afterEach, describe, it } from "node:test";
import { fileURLToPath } from "node:url";

const ROOT = fileURLToPath(new URL("..", import.meta.url));
const DEPLOY = join(ROOT, "deploy");
const SETUP = readFileSync(join(DEPLOY, "setup.sh"), "utf8");
/** Every script an operator runs on the host, as (name, text). */
const HOST_SCRIPTS = [
  ["setup.sh", SETUP],
  ["host/sftp-backups.sh", readFileSync(join(DEPLOY, "host", "sftp-backups.sh"), "utf8")],
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
 * Runs `sh setup.sh <args>` in a fresh folder holding the script and `envText` as `.env`, with
 * the stubs first on PATH: `responses` before the healthy ones, `uid` for `id -u`, `stat` for
 * the backups' folder's `stat` and `parentStat` for any other path's. The backups' folder is
 * `backups` in the folder, or `backupsDir(folder)`; `makeBackups` makes it. Returns the exit
 * code, the output, every docker call and the folder.
 */
function run(
  args,
  {
    envText = env(),
    responses = [],
    uid = "1000",
    stat = "65532:65532 700",
    parentStat = "0:0 755",
    noDocker = false,
    makeBackups = true,
    backupsDir = (folder) => join(folder, "backups"),
  } = {},
) {
  const dir = mkdtempSync(join(tmpdir(), "afkfleet-setup-"));
  made.push(dir);
  const bin = join(dir, "bin");
  mkdirSync(bin);
  copyFileSync(join(DEPLOY, "setup.sh"), join(dir, "setup.sh"));
  // The backups' folder: there (the stub `stat` says whose), unless the case leaves it out.
  const backups = backupsDir(dir);
  if (makeBackups) {
    mkdirSync(backups);
  }
  if (envText !== null) {
    writeFileSync(join(dir, ".env"), `${envText}AFKFLEET_BACKUPS_DIR=${backups}\n`);
  }
  writeFileSync(join(dir, "stub.mjs"), STUB);
  writeFileSync(join(dir, "responses.json"), JSON.stringify([...responses, ...HEALTHY]));
  const scripts = {
    id: `#!/bin/sh\necho ${uid}\n`,
    // The last argument is the path: the backups' folder gets `stat`, any other `parentStat`.
    stat: `#!/bin/sh\nfor last; do :; done\ncase $last in\n"${backups}") echo '${stat}' ;;\n*) echo '${parentStat}' ;;\nesac\n`,
    install: `#!/bin/sh\necho "install $*" >> "${join(dir, "install.log")}"\n`,
  };
  if (noDocker) {
    // PATH holds the stubs alone, so no docker anywhere on the runner is found; the script
    // needs only `dirname` before it looks for docker.
    scripts.dirname = `#!/bin/sh\nexec /usr/bin/dirname "$@"\n`;
  } else {
    scripts.docker = `#!/bin/sh\nexec "${process.execPath}" "${join(dir, "stub.mjs")}" "$@"\n`;
  }
  for (const [name, text] of Object.entries(scripts)) {
    writeFileSync(join(bin, name), text);
    chmodSync(join(bin, name), 0o755);
  }
  const log = join(dir, "calls.jsonl");
  writeFileSync(log, "");
  const path = noDocker ? bin : `${bin}:${process.env.PATH}`;
  const result = spawnSync("/bin/sh", ["setup.sh", ...args], {
    cwd: dir,
    env: { PATH: path, STUB_LOG: log, STUB_RESPONSES: join(dir, "responses.json"), HOME: dir },
    encoding: "utf8",
  });
  const calls = readFileSync(log, "utf8")
    .split("\n")
    .filter((line) => line !== "")
    .map((line) => JSON.parse(line));
  return { code: result.status, stdout: result.stdout, stderr: result.stderr, calls, dir };
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

  it("refuses without docker, without Compose v2.24, or with rootless Docker", () => {
    const missing = run(["check"], { noDocker: true });
    const old = run(["check"], {
      responses: [{ when: "compose version --short", stdout: "2.20.3\n" }],
    });
    const rootless = run(["check"], {
      responses: [{ when: "info --format", stdout: "[name=rootless]\n" }],
    });

    assert.equal(missing.code, 1);
    assert.match(missing.stderr, /docker isn't installed/);
    assert.equal(old.code, 1);
    assert.match(old.stderr, /2\.20\.3 is too old/);
    assert.equal(rootless.code, 1);
    assert.match(rootless.stderr, /rootless/);
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
    const result = run(["init"], {
      responses: [
        {
          when: "volume ls -q --filter label=com.docker.compose.project=afkfleet",
          stdout: "afkfleet_keys\n",
        },
      ],
    });

    assert.equal(result.code, 1);
    assert.match(result.stderr, /init is for a first install only/);
    assert.ok(!joined(result.calls).some((call) => call.includes("create-key")));
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
