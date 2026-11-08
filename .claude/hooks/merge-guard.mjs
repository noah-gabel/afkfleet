// The merge guard: a PreToolUse hook on the Bash and PowerShell tools (ADR-0023).
//
//   node .claude/hooks/merge-guard.mjs < hook-input.json
//
// `.claude/settings.json` runs it before every Bash and PowerShell call, with `onFailure: "block"`,
// so a guard that can't start or crashes blocks the call instead of letting it through (D8). It
// reads the hook's JSON on stdin and exits 0 (go ahead) or 2 (refused, the reason on stderr).
// - A subagent's `git push`, `gh pr create` or `gh pr merge` is refused: only the main session
//   pushes, opens pull requests and merges (U2).
// - Any other command that holds a merge (`gh … pr … merge` in one segment, after quotes and
//   escapes are removed, so it over-matches rather than misses: D9) must be exactly
//   `gh pr merge <n> --squash --match-head-commit <sha> [--delete-branch]`, alone (D1), and passes
//   only when every one of these holds, checked in this order (D11), local checks first:
//   1. it runs inside this repository (D3), on a branch that isn't `main`, whose HEAD is `<sha>`;
//   2. `git diff --name-status --no-renames -z origin/main...HEAD` touches no sensitive path (D5,
//      D6, D10), or it prints `NEEDS_HUMAN: <paths>`;
//   3. target/review/<branch, "/" → "-">/verdict.json says APPROVE for `<sha>`;
//   4. origin's branch is at `<sha>` (`git ls-remote`, never the local copy: D4);
//   5. PR <n> is open, into main, from this repository, on this branch at `<sha>` (D2);
//   6. `gh pr checks <n>` lists at least one check and every one passed (D7).
// Any error on the way refuses the merge (D8). Only Node's own modules: no dependency.
// Tests: scripts/merge-guard.test.mjs, with every git and gh call injected.
import { spawnSync } from "node:child_process";
import { readFileSync, realpathSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

/**
 * The paths a merge never takes without the user (ADR-0023, D10): `files` exactly, everything in
 * `folders`, and every file named one of `names` anywhere. Paths compare without case, so a
 * `Justfile` is the justfile. Cargo.toml's dependency and lints sections (D6) and a modified or
 * deleted ADR are judged apart. The fleet-server, fleet-agent, fleet-core, fleet-api-types and
 * fleet-startup entries are the security coverage gates' (scripts/coverage-gates.mjs: auth, vault,
 * msauth, control-plane, agent-control-plane), plus the auth code no gate measures.
 */
export const SENSITIVE = {
  files: [
    "CLAUDE.md",
    "deny.toml",
    "biome.json",
    "rust-toolchain.toml",
    "justfile",
    "pnpm-lock.yaml",
    "pnpm-workspace.yaml",
    "docs/threat-model.md",
    "scripts/tree-fingerprint.mjs",
    "scripts/threat-model-citations.mjs",
    "scripts/merge-guard.test.mjs",
    "scripts/coverage-gates.mjs",
    "scripts/tauri-hardening.mjs",
    "apps/desktop/src-tauri/tauri.conf.json",
    "apps/desktop/src-tauri/tauri.check.conf.json",
    "apps/desktop/src-tauri/build.rs",
    // public_route's allowlist and the route-coverage test (security rules 1 and 3).
    "crates/fleet-server/tests/routes.rs",
    "crates/fleet-server/src/config/rate_limits.rs",
    // Auth code the gates don't measure.
    "crates/fleet-server/src/config/auth.rs",
    "crates/fleet-server/src/http/handlers/auth.rs",
    "crates/fleet-server/src/http/handlers/mfa.rs",
    // The auth gate.
    "crates/fleet-server/src/app/auth.rs",
    "crates/fleet-server/src/app/users.rs",
    "crates/fleet-server/src/infra/crypto/passwords.rs",
    "crates/fleet-server/src/infra/crypto/tokens.rs",
    "crates/fleet-server/src/ports/tokens.rs",
    "crates/fleet-server/src/http/routes.rs",
    "crates/fleet-server/src/app/lockout.rs",
    "crates/fleet-server/src/infra/crypto/attempt_keys.rs",
    "crates/fleet-server/src/ports/login_attempts.rs",
    "crates/fleet-server/src/app/challenges.rs",
    "crates/fleet-server/src/app/event_tickets.rs",
    "crates/fleet-server/src/http/ws.rs",
    "crates/fleet-server/src/http/handlers/events.rs",
    "crates/fleet-server/src/app/events.rs",
    "crates/fleet-server/src/app/mfa.rs",
    "crates/fleet-server/src/app/login.rs",
    "crates/fleet-server/src/app/invites.rs",
    "crates/fleet-server/src/infra/crypto/totp.rs",
    "crates/fleet-server/src/infra/crypto/recovery_codes.rs",
    "crates/fleet-server/src/ports/totp.rs",
    "crates/fleet-server/src/ports/recovery_codes.rs",
    "crates/fleet-api-types/src/secret.rs",
    // The vault gate.
    "crates/fleet-server/src/infra/crypto/vault.rs",
    "crates/fleet-server/src/ports/vault.rs",
    "crates/fleet-startup/src/secret_file.rs",
    "crates/fleet-server/src/app/vault.rs",
    "crates/fleet-server/src/ports/envelopes.rs",
    "crates/fleet-server/src/infra/sqlite/envelopes.rs",
    // The msauth gate.
    "crates/fleet-server/src/ports/msauth.rs",
    // The control-plane gate.
    "crates/fleet-server/src/app/enrollment.rs",
  ],
  folders: [
    ".github/",
    ".claude/",
    ".cargo/",
    ".config/",
    "deploy/",
    "patches/",
    "apps/desktop/src-tauri/capabilities/",
    "apps/desktop/src-tauri/permissions/",
    "apps/desktop/e2e/baselines/",
    "crates/fleet-core/src/auth/",
    "crates/fleet-server/src/http/extractors/",
    "crates/fleet-server/src/http/rate_limit/",
    "crates/fleet-server/src/infra/msauth/",
    "crates/fleet-server/src/app/session_tokens/",
    "crates/fleet-server/src/app/link_flows/",
    "crates/fleet-server/src/infra/pki/",
    "crates/fleet-server/src/app/agents/",
    "crates/fleet-server/src/grpc/",
    "crates/fleet-agent/src/managed/",
    "crates/fleet-agent/src/enroll/",
  ],
  names: ["clippy.toml", "Cargo.lock"],
};

/** An ADR: modified or deleted, it needs the user; a new one doesn't. */
const ADR = /^docs\/adr\/\d{4}-[^/]*\.md$/;

/** The tools the guard judges. */
const SHELLS = new Set(["Bash", "PowerShell"]);

/** A refusal, whose message is the reason the hook prints. */
class Refusal extends Error {}

const ALLOW = Object.freeze({ allow: true });

const FORM =
  'a merge is exactly "gh pr merge <number> --squash --match-head-commit <sha> --delete-branch", alone (ADR-0023, D1)';

/** The first seven characters of a SHA. */
function short(sha) {
  return String(sha).slice(0, 7);
}

/** A refusal with the guard's prefix. */
function refusal(reason) {
  return new Refusal(`merge-guard refused: ${reason}`);
}

/** Whether `word` names `program`, by itself, as `program.exe` or at the end of a path. */
function names(word, program) {
  const base = word.split("/").pop();
  return base === program || base === `${program}.exe`;
}

/** Whether `words` hold `program` followed, later on, by each of `rest` in order. */
function follows(words, program, ...rest) {
  return words.some((word, at) => {
    if (!names(word, program)) {
      return false;
    }
    let next = at + 1;
    for (const wanted of rest) {
      const found = words.indexOf(wanted, next);
      if (found === -1) {
        return false;
      }
      next = found + 1;
    }
    return true;
  });
}

/**
 * Which writes `command` holds: a merge, a push or a PR's creation, in any one segment between
 * shell separators. Quotes and PowerShell's backtick escapes are removed and case is ignored;
 * a backslash is read both as an escape (removed) and as a path's separator, so either spelling
 * of a program counts. It over-matches by design: `echo gh pr merge` holds a merge (D9).
 */
export function scan(command) {
  const found = { merge: false, push: false, create: false };
  const plain = command.toLowerCase().replace(/['"`]/g, "");
  for (const text of [plain.replace(/\\/g, ""), plain.replace(/\\/g, "/")]) {
    for (const segment of text.split(/[;&|<>(){}$\r\n]+/)) {
      const words = segment.split(/\s+/).filter((word) => word !== "");
      found.merge ||= follows(words, "gh", "pr", "merge");
      found.create ||= follows(words, "gh", "pr", "create");
      found.push ||= follows(words, "git", "push");
    }
  }
  return found;
}

/**
 * The PR's number and the SHA of a merge written exactly as ADR-0023 allows (D1). Throws a
 * refusal that names what it found otherwise.
 */
function parseMerge(command) {
  const text = command.trim();
  const special = /[;&|<>(){}$`'"\\\r\n]/.exec(text);
  if (special !== null) {
    const found = special[0] === "\n" || special[0] === "\r" ? "a line break" : `"${special[0]}"`;
    throw refusal(
      `the merge must be the whole command, alone and written plainly (ADR-0023, D1); found ${found}.`,
    );
  }
  const words = text.split(/\s+/);
  for (const flag of ["--admin", "--auto"]) {
    if (words.some((word) => word.toLowerCase().startsWith(flag))) {
      throw refusal(`${flag} is never used (ADR-0023).`);
    }
  }
  if (words[0] !== "gh" || words[1] !== "pr" || words[2] !== "merge") {
    throw refusal(`${FORM}; this command doesn't start with "gh pr merge".`);
  }
  const seen = new Set();
  let pr;
  let sha;
  for (let at = 3; at < words.length; at += 1) {
    const word = words[at];
    if (word === "--squash" || word === "--delete-branch" || word === "--match-head-commit") {
      if (seen.has(word)) {
        throw refusal(`${word} is given twice.`);
      }
      seen.add(word);
      if (word === "--match-head-commit") {
        at += 1;
        sha = words[at];
        if (sha === undefined || !/^[0-9a-f]{40}$/.test(sha)) {
          throw refusal("--match-head-commit needs HEAD's full 40-character SHA, in lower case.");
        }
      }
    } else if (pr === undefined && /^[1-9][0-9]*$/.test(word)) {
      pr = word;
    } else if (pr === undefined && !word.startsWith("-")) {
      throw refusal("name the pull request by its number (ADR-0023, D1).");
    } else {
      throw refusal(`"${word}" isn't part of the merge ADR-0023 allows (D1).`);
    }
  }
  if (pr === undefined) {
    throw refusal("name the pull request by its number (ADR-0023, D1).");
  }
  if (!seen.has("--squash")) {
    throw refusal("--squash is required (main takes squash merges only).");
  }
  if (sha === undefined) {
    throw refusal("--match-head-commit needs HEAD's full 40-character SHA, in lower case.");
  }
  return { pr, sha };
}

/** The tables whose entries are dependencies. */
const DEPENDENCY_TABLES = new Set([
  "dependencies",
  "dev-dependencies",
  "build-dependencies",
  "dev_dependencies",
  "build_dependencies",
]);

/** Whether a key path names a dependency, lints, patch or replace entry (D6). */
function sensitiveKey(parts) {
  const [first, second, third] = parts;
  if (DEPENDENCY_TABLES.has(first) || ["lints", "patch", "replace"].includes(first)) {
    return true;
  }
  if (first === "workspace" && (DEPENDENCY_TABLES.has(second) || second === "lints")) {
    return true;
  }
  return first === "target" && DEPENDENCY_TABLES.has(third);
}

/** A TOML key's parts, split at the dots outside quotes, the quotes removed. */
function keyParts(key) {
  const parts = [];
  let part = "";
  let quote = null;
  for (const char of key) {
    if (quote !== null) {
      if (char === quote) {
        quote = null;
      } else {
        part += char;
      }
    } else if (char === '"' || char === "'") {
      quote = char;
    } else if (char === ".") {
      parts.push(part.trim());
      part = "";
    } else {
      part += char;
    }
  }
  if (quote !== null) {
    throw new Error(`an unclosed quote in the key ${key}`);
  }
  parts.push(part.trim());
  return parts;
}

/** The key a TOML line starts, before its first `=` outside quotes, or null for any other line. */
function keyOf(line) {
  let quote = null;
  for (let at = 0; at < line.length; at += 1) {
    const char = line[at];
    if (quote !== null) {
      if (char === quote) {
        quote = null;
      }
    } else if (char === '"' || char === "'") {
      quote = char;
    } else if (char === "=") {
      return line.slice(0, at);
    } else if ("#[{,]}".includes(char)) {
      return null;
    }
  }
  return null;
}

/** How many times `fence` occurs in `line`. */
function count(line, fence) {
  return line.split(fence).length - 1;
}

/**
 * The lines of a Cargo.toml's dependency and lints sections, each with its table and key, in
 * order (D6): every `dependencies`, `dev-dependencies` and `build-dependencies` table, a target's
 * and the workspace's included, `[lints]` and `[workspace.lints]` with their subtables, `[patch]`
 * and `[replace]`, in header, dotted-key or inline form. A line that continues a key (an array,
 * a multi-line string) belongs to that key. Throws on a header it can't read, which the caller
 * counts as a change.
 */
export function dependencyLines(text) {
  const lines = [];
  let table = [];
  let current = null;
  let fence = null;
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (fence !== null) {
      if (current !== null) {
        lines.push(`${current}: ${line}`);
      }
      if (count(line, fence) % 2 === 1) {
        fence = null;
      }
      continue;
    }
    if (line === "") {
      continue;
    }
    if (line.startsWith("[")) {
      const header = /^\[\[?([^[\]]+)\]\]?\s*(#.*)?$/.exec(line);
      if (header === null) {
        throw new Error(`a table header it can't read: ${line}`);
      }
      table = keyParts(header[1]);
      current = sensitiveKey(table) ? table.join(".") : null;
      if (current !== null) {
        lines.push(`[${current}]`);
      }
      continue;
    }
    const key = keyOf(line);
    if (key !== null) {
      const parts = [...table, ...keyParts(key)];
      current = sensitiveKey(parts) ? parts.join(".") : null;
    }
    if (current !== null) {
      lines.push(`${current}: ${line}`);
    }
    for (const quotes of ['"""', "'''"]) {
      if (count(line, quotes) % 2 === 1) {
        fence = quotes;
        break;
      }
    }
  }
  return lines;
}

/** Whether `lower`, a path in lower case, is in `list`'s files or folders. */
function listed(list, lower) {
  return (
    list.files.some((file) => file.toLowerCase() === lower) ||
    list.folders.some((folder) => lower.startsWith(folder.toLowerCase()))
  );
}

/** `git diff --name-status --no-renames -z`'s changes. */
function changesOf(output) {
  const fields = output.split("\0");
  if (fields.at(-1) === "") {
    fields.pop();
  }
  if (fields.length % 2 !== 0) {
    throw refusal("couldn't read git diff's list of changes.");
  }
  const changes = [];
  for (let at = 0; at < fields.length; at += 2) {
    changes.push({ status: fields[at], file: fields[at + 1] });
  }
  return changes;
}

/** Compares two paths as the same folder, whatever their slashes or, on Windows, their case. */
function samePath(left, right) {
  const normal = (value) => {
    const slashes = value.replace(/\\/g, "/").replace(/\/+$/, "");
    return process.platform === "win32" ? slashes.toLowerCase() : slashes;
  };
  return normal(left) === normal(right);
}

/** The file's JSON, or null when it's missing or isn't JSON. */
function readJson(deps, file) {
  try {
    return JSON.parse(deps.readFile(file));
  } catch {
    return null;
  }
}

/** The checks a merge must pass, throwing a refusal at the first that fails. */
function judge(input, deps) {
  if (!SHELLS.has(input?.tool_name)) {
    return ALLOW;
  }
  const command = input.tool_input?.command;
  if (typeof command !== "string") {
    throw refusal("a shell call without a command (ADR-0023, D8).");
  }
  const found = scan(command);
  if ((input.agent_id || input.agent_type) && (found.merge || found.push || found.create)) {
    throw refusal(
      "only the main session pushes, opens pull requests and merges; a subagent stays read-only (ADR-0023, U2).",
    );
  }
  if (!found.merge) {
    return ALLOW;
  }
  const { pr, sha } = parseMerge(command);
  const cwd = input.cwd;
  const call = (program, ...args) => {
    const reply = deps.run(program, args, cwd);
    if (reply.status !== 0) {
      throw refusal(`"${[program, ...args].join(" ")}" failed (exit ${reply.status ?? "none"}).`);
    }
    return reply.stdout;
  };

  // 1. This repository, a task branch, HEAD at the SHA (D3, D4).
  const top = deps.run("git", ["rev-parse", "--show-toplevel"], cwd);
  if (top.status !== 0 || !samePath(top.stdout.trim(), deps.projectDir)) {
    throw refusal("the merge must run inside this repository (ADR-0023, D3).");
  }
  const branchReply = deps.run("git", ["symbolic-ref", "--quiet", "--short", "HEAD"], cwd);
  if (branchReply.status !== 0) {
    throw refusal("HEAD is detached; merge from the task branch.");
  }
  const branch = branchReply.stdout.trim();
  if (branch === "main") {
    throw refusal("the task branch can't be main.");
  }
  const head = call("git", "rev-parse", "HEAD").trim();
  if (head !== sha) {
    throw refusal(`--match-head-commit ${short(sha)} isn't HEAD (${short(head)}).`);
  }

  // 2. No sensitive path (D5, D6, D10).
  const changes = changesOf(
    call("git", "diff", "--name-status", "--no-renames", "-z", "origin/main...HEAD"),
  );
  let base;
  const manifestChanged = ({ status, file }) => {
    try {
      base ??= call("git", "merge-base", "origin/main", "HEAD").trim();
      const before = status === "A" ? "" : call("git", "show", `${base}:${file}`);
      const after = status === "D" ? "" : call("git", "show", `${sha}:${file}`);
      return JSON.stringify(dependencyLines(before)) !== JSON.stringify(dependencyLines(after));
    } catch {
      return true;
    }
  };
  const sensitive = changes
    .filter((change) => {
      const lower = change.file.toLowerCase();
      const name = lower.split("/").pop();
      return (
        listed(SENSITIVE, lower) ||
        SENSITIVE.names.some((entry) => entry.toLowerCase() === name) ||
        (ADR.test(lower) && change.status !== "A") ||
        (name === "cargo.toml" && manifestChanged(change))
      );
    })
    .map((change) => change.file);
  if (sensitive.length > 0) {
    throw new Refusal(`NEEDS_HUMAN: ${sensitive.join(", ")}`);
  }

  // 3. The review's verdict for this head.
  const slug = branch.replaceAll("/", "-");
  const record = readJson(
    deps,
    path.join(deps.projectDir, "target", "review", slug, "verdict.json"),
  );
  if (record === null || typeof record !== "object") {
    throw refusal(`no readable verdict.json in target/review/${slug}/ (ADR-0023).`);
  }
  if (record.verdict !== "APPROVE" || record.head !== sha) {
    throw refusal(
      `verdict.json says ${record.verdict} for ${short(record.head)}; a merge needs APPROVE for ${short(sha)}.`,
    );
  }

  // 4. Origin's branch at the SHA (D4).
  const ref = `refs/heads/${branch}`;
  const remote = call("git", "ls-remote", "origin", ref)
    .split(/\r?\n/)
    .map((line) => line.split("\t"))
    .find(([, name]) => name === ref);
  if (remote === undefined) {
    throw refusal(`${branch} isn't on origin; push it first.`);
  }
  if (remote[0] !== sha) {
    throw refusal(`origin's ${branch} is at ${short(remote[0])}, not ${short(sha)}; push first.`);
  }

  // 5. The PR is this branch's (D2).
  let view;
  try {
    view = JSON.parse(
      call(
        "gh",
        "pr",
        "view",
        pr,
        "--json",
        "headRefName,headRefOid,baseRefName,state,isCrossRepository",
      ),
    );
  } catch (error) {
    if (error instanceof Refusal) {
      throw error;
    }
    throw refusal(`couldn't read PR #${pr}.`);
  }
  if (view.state !== "OPEN") {
    throw refusal(`PR #${pr} is ${view.state}, not OPEN.`);
  }
  if (view.baseRefName !== "main") {
    throw refusal(`PR #${pr} goes into ${view.baseRefName}, not main.`);
  }
  if (view.isCrossRepository !== false) {
    throw refusal(`PR #${pr} comes from another repository.`);
  }
  if (view.headRefName !== branch) {
    throw refusal(`PR #${pr} is ${view.headRefName}'s, not ${branch}'s.`);
  }
  if (view.headRefOid !== sha) {
    throw refusal(`PR #${pr}'s head is ${short(view.headRefOid)}, not ${short(sha)}.`);
  }

  // 6. Every check passed (D7).
  const checksReply = deps.run("gh", ["pr", "checks", pr, "--json", "name,bucket"], cwd);
  let checks;
  try {
    checks = JSON.parse(checksReply.stdout);
  } catch {
    checks = null;
  }
  if (!Array.isArray(checks)) {
    throw refusal(`couldn't read PR #${pr}'s checks.`);
  }
  if (checks.length === 0) {
    throw refusal(`PR #${pr} has no checks yet.`);
  }
  const open = checks.filter((check) => check.bucket !== "pass");
  if (open.length > 0) {
    const list = open.map((check) => `${check.name} (${check.bucket})`).join(", ");
    throw refusal(`not every check passed: ${list}.`);
  }
  if (checksReply.status !== 0) {
    throw refusal(`gh pr checks exited ${checksReply.status}.`);
  }
  return ALLOW;
}

/**
 * The hook's decision for `input` (the PreToolUse JSON): `{ allow: true }` or
 * `{ allow: false, reason }`. `deps` holds `projectDir`, `run(program, args, cwd)` answering
 * `{ status, stdout, stderr }`, and `readFile(path)`. Any error refuses (D8).
 */
export function decide(input, deps) {
  try {
    return judge(input, deps);
  } catch (error) {
    if (error instanceof Refusal) {
      return { allow: false, reason: error.message };
    }
    const message = error instanceof Error ? error.message : String(error);
    return {
      allow: false,
      reason: `merge-guard refused: the guard failed (${message}); it never lets a merge through on an error (ADR-0023, D8).`,
    };
  }
}

/** The hook's exit code and stderr for its stdin text. */
export function main(stdin, deps) {
  let input;
  try {
    input = JSON.parse(stdin);
  } catch {
    return {
      code: 2,
      stderr: "merge-guard refused: it couldn't read the hook's input (ADR-0023, D8).",
    };
  }
  const result = decide(input, deps);
  return result.allow ? { code: 0, stderr: "" } : { code: 2, stderr: result.reason };
}

/** Runs `program` without a shell, bounded in time, never prompting. */
function run(program, args, cwd) {
  const reply = spawnSync(program, args, {
    cwd,
    encoding: "utf8",
    env: { ...process.env, GH_PROMPT_DISABLED: "1" },
    maxBuffer: 64 * 1024 * 1024,
    timeout: 60_000,
    windowsHide: true,
  });
  return { status: reply.status, stdout: reply.stdout ?? "", stderr: reply.stderr ?? "" };
}

// Node runs a script by its real path, so the entry check compares with process.argv[1]'s.
if (
  process.argv[1] !== undefined &&
  import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href
) {
  let result;
  try {
    result = main(readFileSync(0, "utf8"), {
      projectDir: path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", ".."),
      run,
      readFile: (file) => readFileSync(file, "utf8"),
    });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    result = {
      code: 2,
      stderr: `merge-guard refused: the guard failed (${message}) (ADR-0023, D8).`,
    };
  }
  if (result.stderr !== "") {
    process.stderr.write(`${result.stderr}\n`);
  }
  process.exitCode = result.code;
}
