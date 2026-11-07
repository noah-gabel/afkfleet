// Ties docs/runbook.md's commands to the tests that run them (Plan.md P12.6; ADR-0022,
// S45, S46).
//
//   node scripts/runbook-commands.mjs
//
// The runbook gives every command an operator runs in a fenced block. Each line of an `sh`
// block (continuation lines joined, spaces collapsed, blank lines and full-line comments
// skipped) must be a row of TESTED: the line verbatim, placeholders and all; the test that
// runs it; and a file holding the literal by which that test runs it (tests add `-T` and
// their own names, so the row's literal, not the line, is what the file holds). A block no
// test can run (a download from GitHub, the keys' way to the operator's PC, the host's
// firewall) is marked instead, by its last non-blank line before it:
//
//   > **Not run by afkfleet's tests:** <why, one sentence>.
//
// and then holds no tabled line. A `powershell` block runs on the operator's PC, so it is
// always marked. Output goes in `text` blocks, settings in `ini`, `toml` or `yaml`; a
// fence without one of these languages fails, and so does inline code that starts with
// `docker `, `sudo `, `sh ` or `curl `: a command outside a block.
//
// A row's test is either a Rust path, resolved as the threat model's citations are
// (threat-model-citations.mjs), where a `slow_` test must be one `just test-stack` runs (its
// filter, read from the justfile) and a `manual_` one is never cited, since only the user
// runs those; or `scripts/<name>.test.mjs › <case>`, whose file must hold `it("<case>"`.
// A row whose line the runbook no longer gives fails too. Exits with 1, naming each
// problem.
//
// `just scripts-test` runs the check on the real files through the tests, so CI runs it.
// Tests: scripts/runbook-commands.test.mjs.
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

import { recipeBody } from "./justfile.mjs";
import { fences, inlineCode, prose } from "./markdown.mjs";
import { indexTests, ROOT, resolves, rustFiles } from "./threat-model-citations.mjs";

/** The fences' languages: commands first, then output and settings. */
const COMMANDS = new Set(["sh", "powershell"]);
const LANGUAGES = ["sh", "powershell", "text", "ini", "toml", "yaml"];

/** A marker, whole. */
export const MARKER = /^> \*\*Not run by afkfleet's tests:\*\* [^\s].*\.$/;

/** A line that means to be a marker. */
const MARKER_START = "> **Not run by afkfleet's tests:**";

/** Inline code that is a command. */
const INLINE_COMMAND = /^(?:docker|sudo|sh|curl) /;

/** A Rust test's path. */
const RUST_PATH = /^[a-z_][a-z0-9_]*(?:::[a-z_][a-z0-9_]*)*$/;

/** A Node test's case: its file and its name. */
const NODE_CASE = /^(scripts\/[a-z0-9-]+\.test\.mjs) › (.+)$/;

const STEPS = "crates/fleet-integration/tests/integration/smoke/ops/steps.rs";
const OPS = "crates/fleet-integration/tests/integration/smoke/ops/mod.rs";
const SMOKE = "crates/fleet-integration/tests/integration/smoke/mod.rs";
const STACK = "crates/fleet-integration/tests/integration/smoke/stack.rs";
const SFTP = "crates/fleet-integration/tests/integration/sftp/mod.rs";
const SETUP_TESTS = "scripts/deploy-setup.test.mjs";

const CRASH = "smoke::ops::slow_ops_restore_after_a_crash";
const NEW_HOST = "smoke::ops::slow_ops_new_host_from_the_keys_copy";
const SFTP_TEST = "sftp::slow_sftp_backups_account";
const METRICS = "smoke::slow_smoke_with_caddy";

/** A Node case in deploy-setup.test.mjs. */
const setupCase = (name) => `${SETUP_TESTS} › ${name}`;

/**
 * The runbook's tested lines: `line` as the runbook gives it, `test` the test that runs it,
 * and `literal` the code by which `file` (from the repository's root) runs it.
 */
export const TESTED = [
  {
    line: "mkdir ~/afkfleet",
    test: setupCase("the runbook's folder lines make ~/afkfleet and enter it"),
    file: SETUP_TESTS,
    literal: '"mkdir ~/afkfleet"',
  },
  {
    line: "cd ~/afkfleet",
    test: setupCase("the runbook's folder lines make ~/afkfleet and enter it"),
    file: SETUP_TESTS,
    literal: '"cd ~/afkfleet"',
  },
  {
    line: "grep ' afkfleet-deploy-X.Y.Z.tar.gz$' SHA256SUMS | sha256sum -c -",
    test: setupCase(
      "check the bundle's sum, unpack it, and leave a folder whose check and init take RELEASE's images",
    ),
    file: SETUP_TESTS,
    literal: ".tar.gz$' SHA256SUMS | sha256sum -c -`",
  },
  {
    line: "tar -xzf afkfleet-deploy-X.Y.Z.tar.gz",
    test: setupCase(
      "check the bundle's sum, unpack it, and leave a folder whose check and init take RELEASE's images",
    ),
    file: SETUP_TESTS,
    literal: "`tar -xzf afkfleet-deploy-",
  },
  {
    line: "cp env.example .env",
    test: setupCase(
      "check the bundle's sum, unpack it, and leave a folder whose check and init take RELEASE's images",
    ),
    file: SETUP_TESTS,
    literal: '"cp env.example .env"',
  },
  {
    line: "sh setup.sh check",
    test: CRASH,
    file: STEPS,
    literal: 'self.setup_sh_ok(&["check"], Root::No)',
  },
  {
    line: "sudo sh setup.sh init",
    test: CRASH,
    file: STACK,
    literal: 'self.setup_sh_ok(&["init"], root)',
  },
  {
    line: "sudo sh setup.sh keys-backup",
    test: NEW_HOST,
    file: STEPS,
    literal: 'self.setup_sh_ok(&["keys-backup"], Root::Sudo)',
  },
  {
    line: "sudo sh host/sftp-backups.sh install --key /path/to/your_key.pub",
    test: SFTP_TEST,
    file: SFTP,
    literal: "sh /root/sftp-backups.sh install --key /root/pull.pub",
  },
  {
    line: "docker compose up --detach --wait",
    test: CRASH,
    file: STEPS,
    literal: 'self.ok(&["up", "--detach", "--wait", "--wait-timeout", "180"], SLOW)',
  },
  {
    line: "sh setup.sh fetch X.Y.Z",
    test: setupCase(
      "fetches a newer release: checks its sum, moves every file in, RELEASE last, and says what's next",
    ),
    file: SETUP_TESTS,
    literal: 'run(["fetch", NEW]',
  },
  {
    line: "sh setup.sh update",
    test: CRASH,
    file: STEPS,
    literal: 'self.setup_sh_ok(&["update"], Root::No)',
  },
  {
    line: "rm -r .afkfleet-fetch",
    test: setupCase(
      "an interrupted fetch leaves RELEASE as it was, and runs again once its folder is gone",
    ),
    file: SETUP_TESTS,
    literal: '"rm -r .afkfleet-fetch"',
  },
  {
    line: "docker compose exec -T server /usr/local/bin/afkfleet-server backup --config /etc/afkfleet/server.toml",
    test: CRASH,
    file: STEPS,
    literal: '"backup",',
  },
  {
    line: "sudo sshd -T -C user=afkfleet-backup,host=<your PC's name>,addr=<your PC's address> | grep -E '^(chrootdirectory|forcecommand|permittty|allowtcpforwarding|disableforwarding) '",
    test: SFTP_TEST,
    file: SFTP,
    literal:
      "sshd -T -C user=afkfleet-backup,host=pc.example.com,addr=192.0.2.20 | grep -E '^(chrootdirectory|forcecommand|permittty|allowtcpforwarding|disableforwarding) '",
  },
  {
    line: "sftp -i ~/.ssh/your_key afkfleet-backup@<your host>",
    test: SFTP_TEST,
    file: SFTP,
    literal: "sftp {SSH} -b /root/batch afkfleet-backup@127.0.0.1",
  },
  {
    line: "sudo sh host/sftp-backups.sh uninstall",
    test: SFTP_TEST,
    file: SFTP,
    literal: 'exec_ok("sh /root/sftp-backups.sh uninstall")',
  },
  {
    line: "sh setup.sh restore afkfleet-backup-YYYYMMDDTHHMMSSZ.db",
    test: CRASH,
    file: STEPS,
    literal: 'self.setup_sh_ok(&["restore", name], Root::No)',
  },
  {
    line: "docker compose run --rm files put-back YYYYMMDDTHHMMSSZ",
    test: CRASH,
    file: OPS,
    literal: '"put-back"',
  },
  {
    line: "sh setup.sh forget-aside",
    test: setupCase(
      "forget-aside lists the aside files without a stamp, and forgets one stamp's with the server healthy",
    ),
    file: SETUP_TESTS,
    literal: 'run(["forget-aside"])',
  },
  {
    line: "sh setup.sh forget-aside YYYYMMDDTHHMMSSZ",
    test: CRASH,
    file: STEPS,
    literal: 'self.setup_sh_ok(&["forget-aside", stamp], Root::No)',
  },
  {
    line: "sudo sh setup.sh keys-restore /root/afkfleet-keys-YYYYMMDDTHHMMSSZ",
    test: NEW_HOST,
    file: STEPS,
    literal: 'self.setup_sh_ok(&["keys-restore", &folder], Root::Sudo)',
  },
  {
    line: "sudo install -o 65532 -g 65532 -m 600 afkfleet-backup-YYYYMMDDTHHMMSSZ.db /srv/afkfleet-backups/backups/",
    test: NEW_HOST,
    file: STEPS,
    literal: '.args(["-n", "install", "-o", "65532", "-g", "65532", "-m", "600"])',
  },
  {
    line: "docker compose run --rm maintenance agent list --config /etc/afkfleet/server.toml",
    test: NEW_HOST,
    file: OPS,
    literal: 'new.maintenance(&["agent", "list"])',
  },
  {
    line: "docker compose run --rm maintenance agent disable --config /etc/afkfleet/server.toml --id <its id>",
    test: NEW_HOST,
    file: OPS,
    literal: 'new.maintenance(&["agent", "disable", "--id", &old_agent.id])',
  },
  {
    line: "docker compose run --rm probe",
    test: METRICS,
    file: SMOKE,
    literal: 'stack.ok(&["run", "--rm", "probe"], SLOW)',
  },
  {
    line: "docker compose run --rm probe http://agent:9481/metrics",
    test: METRICS,
    file: SMOKE,
    literal: 'stack.ok(&["run", "--rm", "probe", "http://agent:9481/metrics"], SLOW)',
  },
  {
    line: "docker compose cp server:/ca/ca.crt ./ca.crt",
    test: NEW_HOST,
    file: OPS,
    literal: '&["cp", "server:/ca/ca.crt", target.to_str().unwrap()]',
  },
  {
    line: "sudo chown 65532:65532 ca.crt",
    test: NEW_HOST,
    file: OPS,
    literal: '&["-n", "chown", "65532:65532"][..]',
  },
  {
    line: "sudo chmod 600 ca.crt",
    test: NEW_HOST,
    file: OPS,
    literal: '&["-n", "chmod", "600"][..]',
  },
  {
    line: "sh setup.sh renew-cert",
    test: CRASH,
    file: STEPS,
    literal: 'self.setup_sh_ok(&["renew-cert"], Root::No)',
  },
  {
    line: "docker compose run --rm setup vault create-key --config /etc/afkfleet/server.toml --id 2",
    test: CRASH,
    file: STEPS,
    literal: '"create-key",',
  },
  {
    line: "docker compose stop agent server",
    test: CRASH,
    file: STEPS,
    literal: 'self.ok(&["stop", "agent", "server"], SLOW)',
  },
  {
    line: "docker compose run --rm maintenance vault rotate-key --config /etc/afkfleet/server.toml",
    test: CRASH,
    file: STEPS,
    literal: 'self.maintenance(&["vault", "rotate-key"])',
  },
  {
    line: "docker compose run --rm maintenance vault status --config /etc/afkfleet/server.toml",
    test: CRASH,
    file: STEPS,
    literal: 'self.maintenance(&["vault", "status"])',
  },
  {
    line: "docker compose ps",
    test: CRASH,
    file: OPS,
    literal: 'stack.ok(&["ps"], QUICK)',
  },
  {
    line: "docker compose logs --tail 50 agent",
    test: CRASH,
    file: OPS,
    literal: 'stack.ok(&["logs", "--tail", "50", "agent"], QUICK)',
  },
  {
    line: "docker compose stop agent",
    test: CRASH,
    file: OPS,
    literal: 'stack.ok(&["stop", "agent"], SLOW)',
  },
  {
    line: "docker compose exec server /usr/local/bin/afkfleet-server agent list --config /etc/afkfleet/server.toml",
    test: CRASH,
    file: OPS,
    literal: '"list",',
  },
  {
    line: "docker compose exec server /usr/local/bin/afkfleet-server agent disable --config /etc/afkfleet/server.toml --id <its id>",
    test: CRASH,
    file: OPS,
    literal: 'for command in ["disable", "release"]',
  },
  {
    line: "docker compose exec server /usr/local/bin/afkfleet-server agent release --config /etc/afkfleet/server.toml --id <its id>",
    test: CRASH,
    file: OPS,
    literal: 'for command in ["disable", "release"]',
  },
  {
    line: "docker compose up --detach agent",
    test: CRASH,
    file: OPS,
    literal: 'stack.ok(&["up", "--detach", "agent"], SLOW)',
  },
  {
    line: "docker compose stop",
    test: NEW_HOST,
    file: OPS,
    literal: 'old.ok(&["stop"], SLOW)',
  },
  {
    line: "docker compose down --volumes --remove-orphans",
    test: CRASH,
    file: STEPS,
    literal: 'self.ok(&["down", "--volumes", "--remove-orphans"], SLOW)',
  },
];

/**
 * The command lines of a block's `body` (`{ line, text }`): continuation lines joined onto
 * the line that starts them, spaces collapsed, blank lines and full-line comments skipped.
 */
export function commandLines(body) {
  const found = [];
  let pending = null;
  for (const { line, text } of body) {
    const trimmed = text.trim();
    if (pending === null && (trimmed === "" || trimmed.startsWith("#"))) {
      continue;
    }
    const continues = trimmed.endsWith("\\");
    const part = continues ? trimmed.slice(0, -1) : trimmed;
    pending =
      pending === null
        ? { line, text: part }
        : { line: pending.line, text: `${pending.text} ${part}` };
    if (!continues) {
      found.push({ line: pending.line, text: pending.text.replace(/\s+/g, " ").trim() });
      pending = null;
    }
  }
  if (pending !== null) {
    found.push({ line: pending.line, text: pending.text.replace(/\s+/g, " ").trim() });
  }
  return found;
}

/** test-stack's nextest filter, `test(/…/)` in its recipe's `body`, as a RegExp. */
export function stackFilter(body) {
  for (const line of body) {
    const match = /test\(\/(.+?)\/\)/.exec(line);
    if (match !== null) {
      return new RegExp(match[1]);
    }
  }
  throw new Error("test-stack's recipe has no test(/…/) filter");
}

/**
 * The problems with `tested`'s citation `row`, given `context`: `rust` (the Rust files, as
 * `rustFiles` gives them), `read(path)` (a file's text from the repository's root, or
 * null) and `stack` (test-stack's filter).
 */
function citationProblems(row, context, index) {
  const at = `the table's \`${row.line}\``;
  const problems = [];
  const node = NODE_CASE.exec(row.test);
  if (node !== null) {
    const source = context.read(node[1]);
    if (source === null) {
      problems.push(`${at}: ${node[1]} doesn't exist`);
    } else if (!source.includes(`it(${JSON.stringify(node[2])}`)) {
      problems.push(`${at}: ${node[1]} has no it(${JSON.stringify(node[2])}`);
    }
  } else if (RUST_PATH.test(row.test)) {
    const name = row.test.split("::").at(-1) ?? "";
    if (!resolves(row.test, index)) {
      problems.push(
        `${at}: \`${row.test}\` names no test under crates/ or apps/desktop/src-tauri/`,
      );
    } else if (name.startsWith("manual_")) {
      problems.push(`${at}: \`${row.test}\` is a manual test, which only the user runs`);
    } else if (name.startsWith("slow_") && !context.stack.test(row.test)) {
      problems.push(`${at}: \`${row.test}\` isn't one just test-stack runs`);
    }
  } else {
    problems.push(
      `${at}: \`${row.test}\` is neither a Rust path nor scripts/<name>.test.mjs › <case>`,
    );
  }
  const source = context.read(row.file);
  if (source === null) {
    problems.push(`${at}: ${row.file} doesn't exist`);
  } else if (!source.includes(row.literal)) {
    problems.push(`${at}: ${row.file} doesn't hold ${row.literal}`);
  }
  return problems;
}

/**
 * Checks `markdown`'s commands against `tested` (rows as TESTED's) in `context` (see
 * `citationProblems`). Returns one message per problem: the document's in line order,
 * then the table's in row order.
 */
export function check(markdown, tested, context) {
  const tabled = new Set(tested.map((row) => row.line));
  const found = [];
  const given = new Set();
  let commands = 0;

  const blocks = fences(markdown);
  const markedLines = new Set();
  for (const block of blocks) {
    const at = `line ${block.line}`;
    if (block.info === "") {
      found.push({ line: block.line, text: `${at}: a fence without a language` });
    } else if (!LANGUAGES.includes(block.info)) {
      found.push({
        line: block.line,
        text: `${at}: a fence in \`${block.info}\`, not ${LANGUAGES.slice(0, -1).join(", ")} or ${LANGUAGES.at(-1)}`,
      });
    }
    if (!block.closed) {
      found.push({ line: block.line, text: `${at}: a fence that's never closed` });
    }
    if (!COMMANDS.has(block.info)) {
      continue;
    }
    const marked = block.before !== null && MARKER.test(block.before.text);
    if (marked) {
      markedLines.add(block.before.line);
    }
    if (block.info === "powershell") {
      commands += commandLines(block.body).length;
      if (!marked) {
        found.push({
          line: block.line,
          text: `${at}: a powershell block runs on the operator's PC, which no test reaches: mark it`,
        });
      }
      continue;
    }
    for (const command of commandLines(block.body)) {
      commands += 1;
      const inTable = tabled.has(command.text);
      if (marked && inTable) {
        found.push({
          line: command.line,
          text: `line ${command.line}: \`${command.text}\` is in the table, but its block is marked as run by no test`,
        });
      } else if (!marked && !inTable) {
        found.push({
          line: command.line,
          text: `line ${command.line}: \`${command.text}\` is run by no test in the table, and its block isn't marked`,
        });
      } else if (!marked) {
        given.add(command.text);
      }
    }
  }

  for (const { line, text } of prose(markdown)) {
    const trimmed = text.trim();
    if (!trimmed.startsWith(MARKER_START)) {
      continue;
    }
    if (!MARKER.test(trimmed)) {
      found.push({
        line,
        text: `line ${line}: a marker is \`${MARKER_START} <why, one sentence>.\``,
      });
    } else if (!markedLines.has(line)) {
      found.push({ line, text: `line ${line}: a marker with no command block after it` });
    }
  }

  for (const { code, line } of inlineCode(markdown)) {
    if (INLINE_COMMAND.test(code)) {
      found.push({
        line,
        text: `line ${line}: \`${code}\` is a command in inline code: give it a block`,
      });
    }
  }

  const problems = found
    .map((problem, order) => ({ ...problem, order }))
    .sort((a, b) => a.line - b.line || a.order - b.order)
    .map((problem) => problem.text);

  if (commands === 0) {
    problems.push("the runbook gives no command, so the check proves nothing");
  }

  const index = indexTests(context.rust);
  const rows = new Set();
  for (const row of tested) {
    if (rows.has(row.line)) {
      problems.push(`the table holds \`${row.line}\` twice`);
      continue;
    }
    rows.add(row.line);
    if (!given.has(row.line)) {
      problems.push(`the table's \`${row.line}\` isn't a command of the runbook's any more`);
    }
    problems.push(...citationProblems(row, context, index));
  }
  return problems;
}

/** Checks the real runbook against the real tests and the justfile. */
export function checkRepository(root = ROOT) {
  const markdown = readFileSync(join(root, "docs", "runbook.md"), "utf8");
  const read = (path) => {
    const full = join(root, path);
    return existsSync(full) ? readFileSync(full, "utf8") : null;
  };
  return check(markdown, TESTED, {
    rust: rustFiles(root),
    read,
    stack: stackFilter(recipeBody("test-stack")),
  });
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  const problems = checkRepository();
  for (const problem of problems) {
    console.error(`runbook-commands: ${problem}`);
  }
  process.exit(problems.length === 0 ? 0 : 1);
}
