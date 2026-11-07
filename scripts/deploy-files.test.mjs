// Tests for deploy/files.sh, the volumes' file operations that deploy/setup.sh runs in the `files`
// service (ADR-0022, S25, S38, S40, S41). Run with `just scripts-test`. Each case runs the script
// under POSIX sh against a stub `stat`, with FILES_ROOT pointing at a temporary folder that stands
// in for the container's /vol (data/, tls/, backups/): never a real volume. Windows has no POSIX
// sh under PowerShell, so these run in CI's Linux job, under dash; the slow tests run the script
// in the real service (fleet-integration's smoke::ops).
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  existsSync,
  lstatSync,
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
import { join } from "node:path";
import process from "node:process";
import { afterEach, describe, it } from "node:test";
import { fileURLToPath } from "node:url";

const SCRIPT = fileURLToPath(new URL("../deploy/files.sh", import.meta.url));

/** The cases that run sh: not on Windows. */
const SH =
  process.platform === "win32" ? "no POSIX sh under PowerShell; CI runs them under dash" : false;

const STAMP = "20261107T120000Z";
const OTHER = "20261108T090000Z";
const BACKUP = "afkfleet-backup-20261106T000000Z.db";

/** The temporary folders made so far, removed after each test. */
const made = [];

afterEach(() => {
  for (const dir of made.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
});

/**
 * A stand-in for the container's /vol: data/ with a database, its -wal and -shm, tls/ with the
 * server's pair, backups/ with one backup; `change(root)` edits it before the run.
 */
function volumes(change = () => {}) {
  const root = mkdtempSync(join(tmpdir(), "afkfleet-files-"));
  made.push(root);
  for (const folder of ["data", "tls", "backups", "bin"]) {
    mkdirSync(join(root, folder));
  }
  writeFileSync(join(root, "data", "afkfleet.db"), "live database\n");
  writeFileSync(join(root, "data", "afkfleet.db-wal"), "live wal\n");
  writeFileSync(join(root, "data", "afkfleet.db-shm"), "live shm\n");
  writeFileSync(join(root, "tls", "server.crt"), "old certificate\n");
  writeFileSync(join(root, "tls", "server.key"), "old key\n");
  writeFileSync(join(root, "backups", BACKUP), "the backup\n");
  change(root);
  return root;
}

/**
 * Runs `sh files.sh <args>` over `root`, the stub `stat` answering `roots` (a folder's owner and
 * mode) for each volume's root: 65532:65532 700 unless a case says otherwise.
 */
function run(root, args, roots = {}) {
  const answers = {
    data: "65532:65532 700",
    tls: "65532:65532 700",
    backups: "65532:65532 700",
    ...roots,
  };
  const cases = Object.entries(answers)
    .map(([folder, answer]) => `"${join(root, folder)}") echo '${answer}' ;;\n`)
    .join("");
  const stat = join(root, "bin", "stat");
  writeFileSync(
    stat,
    `#!/bin/sh\nfor last; do :; done\ncase $last in\n${cases}*) exec /usr/bin/stat "$@" ;;\nesac\n`,
  );
  chmodSync(stat, 0o755);
  const result = spawnSync("/bin/sh", [SCRIPT, ...args], {
    env: { PATH: `${join(root, "bin")}:${process.env.PATH}`, FILES_ROOT: root },
    encoding: "utf8",
  });
  return { code: result.status, stdout: result.stdout, stderr: result.stderr };
}

/** A folder's entries, sorted. */
const listing = (root, folder) => readdirSync(join(root, folder)).sort();
const text = (root, ...path) => readFileSync(join(root, ...path), "utf8");

describe("files.sh, under POSIX sh against a stub stat", { skip: SH }, () => {
  it("refuses a usage error with exit 2: no verb, another verb, a malformed stamp or name", () => {
    const root = volumes();
    for (const args of [
      [],
      ["rm", STAMP],
      ["restore-in", STAMP],
      ["restore-in", "2026-11-07", BACKUP],
      ["restore-in", STAMP, "../afkfleet.db"],
      ["restore-in", STAMP, "afkfleet-backup-20261106T000000Z.db.partial"],
      ["forget", "*"],
      ["tls-aside", `${STAMP}x`],
      ["list", STAMP],
    ]) {
      const result = run(root, args);

      assert.equal(result.code, 2, args.join(" "));
    }
    assert.deepEqual(listing(root, "data"), ["afkfleet.db", "afkfleet.db-shm", "afkfleet.db-wal"]);
  });

  it("refuses a volume whose root isn't the server's user's, owner-only, changing nothing", () => {
    for (const [folder, answer] of [
      ["data", "0:0 755"],
      ["data", "65532:65532 755"],
      ["backups", "1000:1000 700"],
      ["tls", "0:0 700"],
    ]) {
      const root = volumes();
      const verb = folder === "tls" ? ["tls-aside", STAMP] : ["restore-in", STAMP, BACKUP];
      const result = run(root, verb, { [folder]: answer });

      assert.equal(result.code, 1, `${folder} ${answer}`);
      assert.match(result.stderr, /must belong to 65532:65532 with mode 700/, folder);
      assert.deepEqual(listing(root, "data"), [
        "afkfleet.db",
        "afkfleet.db-shm",
        "afkfleet.db-wal",
      ]);
      assert.deepEqual(listing(root, "tls"), ["server.crt", "server.key"]);
    }
  });

  it("restore-in moves the -wal, then the -shm, then the database aside, and copies the backup in, owner-only", () => {
    const root = volumes();

    const result = run(root, ["restore-in", STAMP, BACKUP]);

    assert.equal(result.code, 0, result.stderr);
    assert.deepEqual(listing(root, "data"), [
      "afkfleet.db",
      `afkfleet.db-shm.aside-${STAMP}`,
      `afkfleet.db-wal.aside-${STAMP}`,
      `afkfleet.db.aside-${STAMP}`,
    ]);
    assert.equal(text(root, "data", "afkfleet.db"), "the backup\n");
    assert.equal(statSync(join(root, "data", "afkfleet.db")).mode & 0o777, 0o600);
    assert.equal(text(root, "data", `afkfleet.db-wal.aside-${STAMP}`), "live wal\n");
    assert.equal(text(root, "data", `afkfleet.db.aside-${STAMP}`), "live database\n");
    const moves = result.stdout.split("\n").filter((line) => line.startsWith("moved aside"));
    assert.deepEqual(moves, [
      `moved aside: afkfleet.db-wal as afkfleet.db-wal.aside-${STAMP}`,
      `moved aside: afkfleet.db-shm as afkfleet.db-shm.aside-${STAMP}`,
      `moved aside: afkfleet.db as afkfleet.db.aside-${STAMP}`,
    ]);
    assert.match(result.stdout, new RegExp(`restored: afkfleet\\.db from ${BACKUP}`));
    assert.deepEqual(listing(root, "backups"), [BACKUP], "the backup stays");
  });

  it("restore-in on a new host, with no database, copies the backup alone", () => {
    const root = volumes((dir) => {
      for (const file of ["afkfleet.db", "afkfleet.db-wal", "afkfleet.db-shm"]) {
        rmSync(join(dir, "data", file));
      }
    });

    const result = run(root, ["restore-in", STAMP, BACKUP]);

    assert.equal(result.code, 0, result.stderr);
    assert.deepEqual(listing(root, "data"), ["afkfleet.db"]);
    assert.doesNotMatch(result.stdout, /moved aside/);
  });

  it("restore-in refuses, changing nothing: an aside name taken, a backup missing or a link, a partial copy there", () => {
    for (const [label, change, refusal] of [
      [
        "an aside name taken",
        (dir) => writeFileSync(join(dir, "data", `afkfleet.db.aside-${STAMP}`), "x"),
        /exists already/,
      ],
      ["no such backup", (dir) => rmSync(join(dir, "backups", BACKUP)), /no backup/],
      [
        "a backup that's a link",
        (dir) => {
          rmSync(join(dir, "backups", BACKUP));
          symlinkSync(join(dir, "data", "afkfleet.db"), join(dir, "backups", BACKUP));
        },
        /no backup/,
      ],
      [
        "a partial copy there",
        (dir) => writeFileSync(join(dir, "data", `.afkfleet-restore-${STAMP}.partial`), "x"),
        /exists already/,
      ],
    ]) {
      const root = volumes(change);
      const before = listing(root, "data");

      const result = run(root, ["restore-in", STAMP, BACKUP]);

      assert.equal(result.code, 1, label);
      assert.match(result.stderr, refusal, label);
      assert.deepEqual(listing(root, "data"), before, label);
      assert.equal(text(root, "data", "afkfleet.db"), "live database\n", label);
    }
  });

  it("put-back undoes a restore: the restored files moved to .undone, the aside ones back", () => {
    const root = volumes();
    run(root, ["restore-in", STAMP, BACKUP]);

    const result = run(root, ["put-back", STAMP]);

    assert.equal(result.code, 0, result.stderr);
    assert.deepEqual(listing(root, "data"), [
      "afkfleet.db",
      "afkfleet.db-shm",
      "afkfleet.db-wal",
      `afkfleet.db.undone-${STAMP}`,
    ]);
    assert.equal(text(root, "data", "afkfleet.db"), "live database\n");
    assert.equal(text(root, "data", `afkfleet.db.undone-${STAMP}`), "the backup\n");
  });

  it("put-back refuses a stamp with no database aside", () => {
    const root = volumes();

    const result = run(root, ["put-back", OTHER]);

    assert.equal(result.code, 1);
    assert.match(result.stderr, /nothing is aside under 20261108T090000Z/);
    assert.deepEqual(listing(root, "data"), ["afkfleet.db", "afkfleet.db-shm", "afkfleet.db-wal"]);
  });

  it("tls-aside moves the server's pair aside, and tls-back puts it back, the new pair to .undone", () => {
    const root = volumes();

    const aside = run(root, ["tls-aside", STAMP]);
    const asideListing = listing(root, "tls");
    writeFileSync(join(root, "tls", "server.crt"), "half-made certificate\n");
    const back = run(root, ["tls-back", STAMP]);

    assert.equal(aside.code, 0, aside.stderr);
    assert.deepEqual(asideListing, [`server.crt.aside-${STAMP}`, `server.key.aside-${STAMP}`]);
    assert.equal(back.code, 0, back.stderr);
    assert.deepEqual(listing(root, "tls"), [
      "server.crt",
      `server.crt.undone-${STAMP}`,
      "server.key",
    ]);
    assert.equal(text(root, "tls", "server.crt"), "old certificate\n");
  });

  it("tls-aside refuses an aside name taken, changing nothing", () => {
    const root = volumes((dir) =>
      writeFileSync(join(dir, "tls", `server.key.aside-${STAMP}`), "x"),
    );

    const result = run(root, ["tls-aside", STAMP]);

    assert.equal(result.code, 1);
    assert.deepEqual(
      listing(root, "tls"),
      ["server.crt", `server.key.aside-${STAMP}`, "server.key"].sort(),
    );
  });

  it("forget deletes exactly its stamp's aside and undone files, and nothing else", () => {
    const root = volumes();
    run(root, ["restore-in", STAMP, BACKUP]);
    run(root, ["tls-aside", STAMP]);
    const decoys = [
      `afkfleet.db.aside-${OTHER}`,
      `afkfleet.db.aside-${STAMP}x`,
      `xafkfleet.db.aside-${STAMP}`,
      `afkfleet.db.aside-${STAMP.slice(0, -1)}`,
      `afkfleet.db.backup-${STAMP}`,
    ];
    for (const decoy of decoys) {
      writeFileSync(join(root, "data", decoy), "keep\n");
    }

    const result = run(root, ["forget", STAMP]);

    assert.equal(result.code, 0, result.stderr);
    assert.deepEqual(listing(root, "data"), ["afkfleet.db", ...decoys].sort());
    assert.deepEqual(listing(root, "tls"), []);
    assert.deepEqual(listing(root, "backups"), [BACKUP]);
    assert.match(result.stdout, new RegExp(`deleted: afkfleet\\.db\\.aside-${STAMP}`));
  });

  it("forget refuses a link or a folder with one of its exact names, deleting nothing", () => {
    for (const [label, change] of [
      [
        "a link",
        (dir) =>
          symlinkSync(
            join(dir, "data", "afkfleet.db"),
            join(dir, "data", `afkfleet.db-wal.aside-${STAMP}`),
          ),
      ],
      ["a folder", (dir) => mkdirSync(join(dir, "tls", `server.key.aside-${STAMP}`))],
    ]) {
      const root = volumes((dir) => {
        writeFileSync(join(dir, "data", `afkfleet.db.aside-${STAMP}`), "old\n");
        change(dir);
      });

      const result = run(root, ["forget", STAMP]);

      assert.equal(result.code, 1, label);
      assert.match(result.stderr, /isn't a regular file/, label);
      assert.ok(existsSync(join(root, "data", `afkfleet.db.aside-${STAMP}`)), label);
      assert.ok(lstatSync(join(root, "data", "afkfleet.db")).isFile(), label);
    }
  });

  it("list names every aside and undone file, by its volume", () => {
    const root = volumes();
    run(root, ["restore-in", STAMP, BACKUP]);
    run(root, ["tls-aside", OTHER]);

    const result = run(root, ["list"]);

    assert.equal(result.code, 0, result.stderr);
    assert.deepEqual(result.stdout.split("\n").filter(Boolean), [
      `data: afkfleet.db-shm.aside-${STAMP}`,
      `data: afkfleet.db-wal.aside-${STAMP}`,
      `data: afkfleet.db.aside-${STAMP}`,
      `tls: server.crt.aside-${OTHER}`,
      `tls: server.key.aside-${OTHER}`,
    ]);
  });
});
