// Tests for dod-log-check.mjs (ADR-0020, S135, U129, U133). Run with `just scripts-test`.
//
// Every run is planted in a temporary folder, in dod-log.mjs's file names; nothing here reads
// target/dod-run (U33). The check reads this repository's own sources and Cargo.lock, as it
// does at the user's run.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
  chmodSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import process from "node:process";
import { afterEach, beforeEach, describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import {
  BOT_STATES,
  checkLogs,
  chronoTime,
  errorCode,
  lockedCrates,
  main,
  modulePaths,
  REUSE_MARGIN_MS,
  sourceLiterals,
  TEXTS,
} from "./dod-log-check.mjs";
import { recipeBody, recipeParameters } from "./justfile.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const CONTEXT = {
  literals: sourceLiterals(ROOT),
  crates: lockedCrates(readFileSync(path.join(ROOT, "Cargo.lock"), "utf8")),
  paths: modulePaths(ROOT),
};

const MIN = 60_000;
const HOUR = 60 * MIN;
/** The first session's grant: every time the check prints is from here. */
const T0 = Date.parse("2026-11-03T19:30:00Z");
/** When the container restart kicked the bot, 24 h 5 min after the first session. */
const KICK = T0 + 24 * HOUR + 5 * MIN;
/** The first session's expiry. */
const EXPIRES = T0 + 24 * HOUR;
const BOT = "0192f0e0-1111-7aaa-8bbb-000000000001";
const AGENT = "0192f0e0-2222-7aaa-8bbb-000000000002";

/** tracing's JSON timestamp, to the microsecond. */
const stamp = (ms) => new Date(ms).toISOString().replace("Z", "000Z");
/** chrono's `DateTime<Utc>` Display, as `expires_at` carries it. */
const chrono = (ms) => new Date(ms).toISOString().replace("T", " ").replace("Z", "000000 UTC");

/** One JSON line of a binary (ADR-0014's shape). */
function line(ms, level, target, message, fields = {}, span = undefined) {
  return JSON.stringify({
    timestamp: stamp(ms),
    level,
    target,
    message,
    ...fields,
    ...(span === undefined ? {} : { span, spans: [{ name: "agent", agent: "dod-agent" }, span] }),
  });
}

const grant = (ms, expires, bot = BOT) =>
  line(ms, "INFO", "fleet_server::app::agents::session_grants", "a session was granted", {
    request_id: "req",
    agent_id: AGENT,
    bot_id: bot,
    session_request: 1,
    kind: "online",
    fresh: false,
    expires_at: chrono(expires),
  });
const state = (ms, debug, bot = BOT) =>
  line(
    ms,
    "INFO",
    "fleet_runtime::actor::effects",
    "the bot's state changed",
    { state: debug },
    { name: "bot", bot_id: bot },
  );
const ended = (ms, bot = BOT) =>
  line(
    ms,
    "WARN",
    "fleet_runtime::actor::effects",
    "the session ended; the bot connects again",
    { reason: "kicked", key: "multiplayer.disconnect.server_shutdown", state: "Online { .. }" },
    { name: "bot", bot_id: bot },
  );
const reset = (ms) =>
  line(
    ms,
    "ERROR",
    "azalea_client::plugins::connection",
    'Error reading packet from Client: IoError { source: Os { code: 10054, kind: ConnectionReset, message: "An existing connection was forcibly closed by the remote host." } }',
  );
const request = (ms, route, status = 200) =>
  line(
    ms,
    "INFO",
    "fleet_server::http::middleware::trace",
    "request finished",
    { status, latency_ms: 3 },
    { name: "request", request_id: "req", method: "POST", route },
  );
const serverStopped = (ms, code = 0) =>
  line(ms, "INFO", "fleet_server::cli::serve", "the server stopped", { exit_code: code });
const agentStopped = (ms, fields = {}) =>
  line(
    ms,
    "INFO",
    "fleet_agent::run",
    "the agent stopped",
    { exit_code: 0, stopped: 1, aborted: 0, crashed: 0, ...fields },
    undefined,
  );

const ONLINE = "Online { since: 2026-11-03T19:30:02Z, attempt: 0 }";
const BACKOFF = "Backoff { attempt: 1, fresh: false }";
const AWAITING = "AwaitingSession { fresh: false }";
const CONNECTING = "Connecting { attempt: 1, auth_retried: false }";

/**
 * A run that passes: a session at T0, the bot Online, the container restarted after the
 * session's lifetime, a new session at the first reconnect, a refused connect while the
 * server starts, Online again, then the stop at 25 h 40 min.
 */
function goodRun() {
  return {
    server: [
      line(T0 - MIN, "INFO", "fleet_server::cli::serve", "the server is listening"),
      grant(T0, EXPIRES),
      grant(KICK + 6_000, EXPIRES + 24 * HOUR + 6 * MIN),
      grant(KICK + 21_000, EXPIRES + 24 * HOUR + 6 * MIN),
      serverStopped(T0 + 25 * HOUR + 40 * MIN),
    ],
    agent: [
      state(T0 - 1_000, AWAITING),
      state(T0 + 1_000, CONNECTING),
      state(T0 + 2_000, ONLINE),
      ended(KICK),
      state(KICK, BACKOFF),
      state(KICK + 5_000, AWAITING),
      state(KICK + 7_000, CONNECTING),
      state(KICK + 8_000, BACKOFF),
      state(KICK + 20_000, AWAITING),
      state(KICK + 22_000, CONNECTING),
      state(KICK + 30_000, ONLINE),
      state(T0 + 25 * HOUR + 35 * MIN, "Stopping { since: 2026-11-04T21:05:00Z }"),
      state(T0 + 25 * HOUR + 35 * MIN + 1_000, "Stopped"),
      agentStopped(T0 + 25 * HOUR + 36 * MIN),
    ],
    serverStderr:
      "    Finished `release` profile [optimized] target(s) in 0.61s\n     Running `target\\release\\afkfleet-server.exe serve --config deploy/dod/server.toml`\n",
    agentStderr:
      "    Blocking waiting for file lock on build directory\n    Finished `release` profile [optimized] target(s) in 0.70s\n     Running `target\\release\\afkfleet-agent.exe run --config deploy/dod/agent.toml`\n",
    serverExit: '{"code":0,"signal":null}\n',
    agentExit: '{"code":0,"signal":null}\n',
  };
}

let folder;

beforeEach(() => {
  folder = mkdtempSync(path.join(tmpdir(), "dod-log-check-"));
});

afterEach(() => {
  rmSync(folder, { recursive: true, force: true });
});

/** Plants `run` in the folder as dod-log.mjs names one start of each; null leaves a file out. */
function plant(run, { server = "server-20261103T192900Z", agent = "agent-20261103T192901Z" } = {}) {
  const files = {
    [`${server}.jsonl`]: run.server === null ? null : `${run.server.join("\n")}\n`,
    [`${server}.stderr.txt`]: run.serverStderr,
    [`${server}.exit.json`]: run.serverExit,
    [`${agent}.jsonl`]: run.agent === null ? null : `${run.agent.join("\n")}\n`,
    [`${agent}.stderr.txt`]: run.agentStderr,
    [`${agent}.exit.json`]: run.agentExit,
  };
  for (const [name, text] of Object.entries(files)) {
    if (text !== null && text !== undefined) {
      writeFileSync(path.join(folder, name), text);
    }
  }
}

/** The check of the planted run. */
function checked(run, names) {
  plant(run, names);
  return checkLogs(folder, CONTEXT);
}

/** The FAIL reasons in `text`, without their dashes. */
function reasons(text) {
  const lines = text.split("\n");
  const from = lines.indexOf("FAIL:");
  return from === -1
    ? []
    : lines
        .slice(from + 1)
        .filter((said) => said.startsWith("- "))
        .map((said) => said.slice(2));
}

describe("a run that passes", () => {
  it("prints its counts, the bot's sessions and its Online time, then PASS, and exits 0", () => {
    const { code, text } = checked(goodRun());

    assert.equal(code, 0);
    assert.equal(
      text,
      [
        "afkfleet's DoD log check: 1 server start, 1 agent start",
        "",
        "The server's log: 5 lines",
        "By level: ERROR 0, WARN 0, INFO 5",
        "By target:",
        "  fleet_server::app::agents::session_grants  INFO 3  WARN 0  ERROR 0",
        "  fleet_server::cli::serve  INFO 2  WARN 0  ERROR 0",
        "Warnings and errors:",
        "  none",
        "",
        "The agent's log: 14 lines",
        "By level: ERROR 0, WARN 1, INFO 13",
        "By target:",
        "  fleet_agent::run  INFO 1  WARN 0  ERROR 0",
        "  fleet_runtime::actor::effects  INFO 12  WARN 1  ERROR 0",
        "Warnings and errors:",
        '  WARN  fleet_runtime::actor::effects  ×1  "the session ended; the bot connects again"',
        "",
        "Bots (times from the first session):",
        "Bot 1: 2 sessions; 1 renewed after the lifetime at +24:05; 0 renewed early",
        "Bot 1: Online from +0:00 to +25:35; not Online for 0:00:30 in 1 gaps",
        "",
        "PASS: no error, no panic, nothing unhandled, and every bot's session was renewed after its lifetime while it reconnected by itself.",
        "",
      ].join("\n"),
    );
  });

  it("allows azalea's one ConnectionReset read error inside the restart's window (U133)", () => {
    const run = goodRun();
    run.agent.splice(3, 0, reset(KICK - 200));

    const { code, text } = checked(run);

    assert.equal(code, 0, text);
    assert.match(
      text,
      /\n {2}ERROR {2}azalea_client {2}×1 {2}\(message not shown: not afkfleet's own\) \(allowed: the container's restart\)\n/,
    );
    assert.match(text, /By level: ERROR 1, WARN 1, INFO 13/);
  });
});

describe("what it prints (S135)", () => {
  it("prints no field's value, no message with values, no span, no stderr line, no file name and no path", () => {
    const secrets = {
      name: "Zq7SecretPlayer",
      uuid: "5a1f3c2e-9b8d-4e7f-a6b5-c4d3e2f1a0b9",
      address: "192.0.2.77:25599",
      token: "eyJhbGciOiJsecretTOKENvalue",
      chat: "my chat secret says hi",
      bot: "0192f0e0-9999-7aaa-8bbb-00000000beef",
      target: "Zq7SecretTarget",
      segment: "Zq7SecretSegment",
      file: "Zq7SecretFile.txt",
      stderr: "Zq7SecretStderrLine",
      spanValue: "Zq7SecretSpanValue",
      spansValue: "Zq7SecretSpansValue",
      message: "Zq7SecretMessage",
    };
    for (const secret of Object.values(secrets)) {
      assert.ok(![...CONTEXT.literals].some((literal) => literal.includes(secret)), secret);
      assert.ok(!CONTEXT.crates.has(secret), secret);
    }
    const run = goodRun();
    const everything = {
      username: secrets.name,
      uuid: secrets.uuid,
      server: secrets.address,
      access_token: secrets.token,
      text: secrets.chat,
    };
    run.server = run.server.map((said) => said.replaceAll(BOT, secrets.bot));
    run.agent = run.agent.map((said) => said.replaceAll(BOT, secrets.bot));
    run.agent.splice(
      3,
      0,
      line(
        KICK - 5 * HOUR,
        "WARN",
        "fleet_mc::bridge",
        `kicked ${secrets.message} ${secrets.chat}`,
        everything,
        {
          name: "bot",
          bot_id: secrets.bot,
          server: secrets.spanValue,
        },
      ),
      line(
        KICK - 4 * HOUR,
        "WARN",
        "azalea_client::plugins::packet::game",
        `Got disconnect packet ${secrets.name}`,
        everything,
      ),
      line(KICK - 3 * HOUR, "WARN", secrets.target, "a fixed-looking message", everything),
      line(KICK - 3 * HOUR, "WARN", `fleet_mc::${secrets.segment}`, "a fixed-looking message"),
      JSON.stringify({
        timestamp: stamp(KICK - 2 * HOUR),
        level: "WARN",
        target: "fleet_runtime::actor::effects",
        message: "the session ended; the bot connects again",
        ...everything,
        span: { name: "bot", bot_id: secrets.bot },
        spans: [{ name: "bot", bot_id: secrets.bot, extra: secrets.spansValue }],
      }),
      `not json ${secrets.token}`,
    );
    run.serverStderr += `${secrets.stderr} ${secrets.address}\n`;
    plant(run);
    writeFileSync(path.join(folder, secrets.file), secrets.chat);
    mkdirSync(path.join(folder, `${secrets.file}.d`));

    const { code, text } = checkLogs(folder, CONTEXT);

    assert.equal(code, 1);
    for (const secret of [...Object.values(secrets), folder, tmpdir()]) {
      assert.ok(!text.includes(secret), `${secret} in:\n${text}`);
    }
    assert.match(text, /2 other files ignored/);
    assert.match(text, /\(message not shown: not a fixed text of afkfleet's sources\)/);
    assert.match(text, /\(message not shown: not afkfleet's own\)/);
    assert.match(text, /\(another target\)/);
    assert.match(text, /"the session ended; the bot connects again"/);
    assert.match(text, /^Bot 1: /m);
  });

  it("prints a target whole only when it's a module path of afkfleet's sources, else its crate alone", () => {
    const run = goodRun();
    run.agent.splice(
      3,
      0,
      line(KICK - HOUR, "WARN", "fleet_mc::Zq7SecretSegment", "hi"),
      line(KICK - HOUR, "WARN", "fleet_mc::host::Zq7SecretSegment", "hi"),
      line(KICK - HOUR, "WARN", "azalea_client::plugins::Zq7SecretSegment", "hi"),
      line(KICK - HOUR, "WARN", "fleet_runtime::actor::effects", "hi"),
    );

    const { text } = checked(run);

    assert.doesNotMatch(text, /Zq7SecretSegment|plugins/);
    assert.match(text, /\n {2}fleet_mc {2}INFO 0 {2}WARN 2 {2}ERROR 0\n/);
    assert.match(text, /\n {2}azalea_client {2}INFO 0 {2}WARN 1 {2}ERROR 0\n/);
    assert.match(text, /\n {2}fleet_runtime::actor::effects {2}INFO 12 {2}WARN 2 {2}ERROR 0\n/);
  });

  it("counts a target that isn't a crate of Cargo.lock under one placeholder", () => {
    const run = goodRun();
    run.agent.splice(
      3,
      0,
      line(KICK - HOUR, "WARN", "not_a_locked_crate::x", "hi"),
      line(KICK - HOUR, "WARN", "has space", "hi"),
    );

    const { text } = checked(run);

    assert.match(text, /\n {2}\(another target\) {2}INFO 0 {2}WARN 2 {2}ERROR 0\n/);
    assert.doesNotMatch(text, /not_a_locked_crate|has space/);
  });
});

describe("a renewal (U129)", () => {
  it("is required: a run without one fails, naming when the first session's lifetime ended", () => {
    const run = goodRun();
    run.server = run.server.filter((said) => !said.includes(chrono(EXPIRES + 24 * HOUR + 6 * MIN)));
    run.server.splice(2, 0, grant(KICK + 6_000, EXPIRES), grant(KICK + 21_000, EXPIRES));

    const { code, text } = checked(run);

    assert.equal(code, 1);
    assert.match(text, /Bot 1: 1 sessions; 0 renewed after the lifetime; 0 renewed early/);
    assert.deepEqual(reasons(text), [
      "bot 1's session was never renewed after its lifetime, which ended at +24:00",
    ]);
  });

  it("counts a new session only from REUSE_MARGIN before the old one's end: 11 minutes before is early, 10 is the lifetime's", () => {
    for (const [before, early] of [
      [11 * MIN, true],
      [10 * MIN, false],
    ]) {
      const run = goodRun();
      const renewedAt = EXPIRES - before;
      run.server[2] = grant(renewedAt, EXPIRES + 24 * HOUR);
      run.server[3] = grant(renewedAt + 15_000, EXPIRES + 24 * HOUR);
      run.agent = [
        state(T0 - 1_000, AWAITING),
        state(T0 + 1_000, CONNECTING),
        state(T0 + 2_000, ONLINE),
        state(renewedAt - 1_000, BACKOFF),
        state(renewedAt - 500, AWAITING),
        state(renewedAt + 1_000, CONNECTING),
        state(renewedAt + 2_000, ONLINE),
        state(T0 + 25 * HOUR + 35 * MIN, "Stopping { since: 2026-11-04T21:05:00Z }"),
        agentStopped(T0 + 25 * HOUR + 36 * MIN),
      ];

      const { code, text } = checked(run);

      if (early) {
        assert.equal(code, 1, text);
        assert.match(text, /Bot 1: 2 sessions; 0 renewed after the lifetime; 1 renewed early/);
      } else {
        assert.equal(code, 0, text);
        assert.match(
          text,
          /Bot 1: 2 sessions; 1 renewed after the lifetime at \+23:50; 0 renewed early/,
        );
      }
      rmSync(folder, { recursive: true, force: true });
      folder = mkdtempSync(path.join(tmpdir(), "dod-log-check-"));
    }
  });

  it("fails when anything but the bot's own reconnect came between: a lifecycle request or line, a stop, a pause", () => {
    const why = {
      lifecycle: "a Start, Stop, Restart, Reset or Resume came in between",
      state: "it stopped, paused or failed in between",
    };
    for (const [planted, reason] of [
      [
        line(
          KICK + 10_000,
          "INFO",
          "fleet_server::app::bots::lifecycle",
          "a bot's desired state was set",
          {
            bot_id: BOT,
            desired: "running",
          },
        ),
        why.lifecycle,
      ],
      [
        line(
          KICK + 10_000,
          "INFO",
          "fleet_server::app::bots::lifecycle",
          "a command was sent to a bot's agent",
          { bot_id: BOT },
        ),
        why.lifecycle,
      ],
      [
        line(
          KICK + 10_000,
          "INFO",
          "fleet_server::app::bots::lifecycle",
          "a resting bot's sticky state was cleared",
          {
            bot_id: BOT,
          },
        ),
        why.lifecycle,
      ],
      ...["start", "stop", "restart", "reset", "resume"].map((route) => [
        request(KICK + 10_000, `/api/v1/bots/{id}/${route}`, 409),
        why.lifecycle,
      ]),
    ]) {
      const run = goodRun();
      run.server.splice(3, 0, planted);

      const { code, text } = checked(run);

      assert.equal(code, 1, planted);
      assert.deepEqual(
        reasons(text),
        [`bot 1's renewal at +24:05 wasn't a reconnect by itself: ${reason}`],
        planted,
      );
      rmSync(folder, { recursive: true, force: true });
      folder = mkdtempSync(path.join(tmpdir(), "dod-log-check-"));
    }
    for (const debug of [
      "Stopping { since: x }",
      "Stopped",
      "Paused { reason: DuplicateLogin }",
      "Failed { reason: Auth }",
    ]) {
      const run = goodRun();
      run.agent.splice(7, 0, state(KICK + 7_500, debug));

      const { text } = checked(run);

      assert.ok(
        reasons(text).includes(
          `bot 1's renewal at +24:05 wasn't a reconnect by itself: ${why.state}`,
        ),
        debug,
      );
      rmSync(folder, { recursive: true, force: true });
      folder = mkdtempSync(path.join(tmpdir(), "dod-log-check-"));
    }
  });

  it("fails when the session before it didn't end by itself, or the bot wasn't Online again", () => {
    const stopped = goodRun();
    stopped.agent[4] = state(KICK, "Stopping { since: x }");
    assert.ok(
      reasons(checked(stopped).text).includes(
        "bot 1's renewal at +24:05 wasn't a reconnect by itself: it didn't follow a session that ended by itself",
      ),
    );
    rmSync(folder, { recursive: true, force: true });
    folder = mkdtempSync(path.join(tmpdir(), "dod-log-check-"));

    const never = goodRun();
    never.agent = never.agent.slice(0, 10);
    never.agent.push(agentStopped(T0 + 25 * HOUR + 36 * MIN));
    assert.ok(
      reasons(checked(never).text).includes(
        "bot 1's renewal at +24:05 wasn't a reconnect by itself: it wasn't Online again after it",
      ),
    );
  });

  it("numbers the bots by their first session and never prints their IDs", () => {
    const run = goodRun();
    const other = "0192f0e0-3333-7aaa-8bbb-000000000003";
    run.server.splice(1, 0, grant(T0 - 30_000, EXPIRES - 30_000, other));

    const { text } = checked(run);

    assert.match(text, /^Bot 1: 1 sessions; /m);
    assert.match(text, /^Bot 2: 2 sessions; 1 renewed after the lifetime at \+24:05; /m);
    assert.ok(!text.includes(other) && !text.includes(BOT));
  });
});

describe("errors, panics and lines it can't read", () => {
  it("fails on an ERROR line, and on azalea's reset outside the window or a second inside it", () => {
    for (const [planted, said] of [
      [
        [
          line(
            T0 + HOUR,
            "ERROR",
            "fleet_server::app::agents::session_grants",
            "a session request's grant panicked",
          ),
        ],
        "server",
      ],
      [[reset(T0 + 3 * HOUR)], "agent"],
      [[reset(KICK - 200), reset(KICK - 100)], "agent"],
    ]) {
      const run = goodRun();
      run[said].splice(3, 0, ...planted);

      const { code, text } = checked(run);

      assert.equal(code, 1, text);
      assert.deepEqual(reasons(text), [`1 ERROR lines in the ${said}'s log (listed above)`]);
      rmSync(folder, { recursive: true, force: true });
      folder = mkdtempSync(path.join(tmpdir(), "dod-log-check-"));
    }
  });

  it("fails on a panic, as a panic", () => {
    const run = goodRun();
    run.server.splice(
      2,
      0,
      line(T0 + HOUR, "ERROR", "afkfleet::panic", "a thread panicked", { payload: "boom" }),
    );

    const { code, text } = checked(run);

    assert.equal(code, 1);
    assert.deepEqual(reasons(text), ["1 panics in the server's log"]);
    assert.match(text, /ERROR {2}afkfleet::panic {2}×1 {2}"a thread panicked"/);
  });

  it("fails on a log line that isn't one of the binary's JSON lines", () => {
    for (const bad of [
      "not json",
      "[1]",
      '{"level":"INFO","target":"x","message":"m"}',
      JSON.stringify({ timestamp: "yesterday", level: "INFO", target: "x" }),
      JSON.stringify({ timestamp: stamp(T0), level: "LOUD", target: "x" }),
      JSON.stringify({ timestamp: stamp(T0), level: "INFO", target: 5 }),
      JSON.stringify({ timestamp: stamp(T0), level: "INFO", target: "x", message: 5 }),
      JSON.stringify({ timestamp: stamp(T0), level: "INFO", target: "x", span: "s" }),
      JSON.stringify({ timestamp: stamp(T0), level: "INFO", target: "x", spans: {} }),
    ]) {
      const run = goodRun();
      run.agent.splice(3, 0, bad);

      const { code, text } = checked(run);

      assert.equal(code, 1, bad);
      assert.deepEqual(reasons(text), ["1 lines of the agent's log aren't its JSON lines"], bad);
      rmSync(folder, { recursive: true, force: true });
      folder = mkdtempSync(path.join(tmpdir(), "dod-log-check-"));
    }
  });

  it("fails on a grant whose expiry doesn't read", () => {
    const run = goodRun();
    run.server[1] = run.server[1].replace(chrono(EXPIRES), "soon");

    const { text } = checked(run);

    assert.ok(reasons(text).includes("a session grant's expiry isn't readable"));
  });

  it("fails on a stderr line that isn't cargo's", () => {
    const run = goodRun();
    run.serverStderr += "warning: something\n";

    const { code, text } = checked(run);

    assert.equal(code, 1);
    assert.deepEqual(reasons(text), ["1 lines of the server's stderr aren't cargo's"]);
  });
});

describe("how the binaries ended", () => {
  it("fails on a code, a signal, a missing or unreadable record, a last line that isn't the stop, or a bot aborted at the stop", () => {
    const cases = [
      [(run) => (run.serverExit = '{"code":3,"signal":null}'), "the server exited with code 3"],
      [
        (run) => (run.agentExit = '{"code":null,"signal":"SIGTERM"}'),
        "the agent was ended by SIGTERM",
      ],
      [
        (run) => (run.agentExit = '{"code":null,"signal":"sig term"}'),
        "the agent was ended by a signal",
      ],
      [
        (run) => (run.serverExit = undefined),
        "the server hasn't ended, or ended without its exit record: stop it with Ctrl+C, then check again",
      ],
      [(run) => (run.agentExit = "{"), "the agent's exit record isn't readable"],
      [
        (run) => (run.agentExit = '{"code":null,"signal":null}'),
        "the agent's exit record isn't readable",
      ],
      [
        (run) => run.server.pop(),
        "the server's last line isn't \"the server stopped\" with exit code 0",
      ],
      [
        (run) => (run.server[run.server.length - 1] = serverStopped(T0 + 25 * HOUR + 40 * MIN, 1)),
        "the server's last line isn't \"the server stopped\" with exit code 0",
      ],
      [
        (run) =>
          (run.agent[run.agent.length - 1] = agentStopped(T0 + 25 * HOUR + 36 * MIN, {
            crashed: 1,
          })),
        "the agent's stop aborted or crashed a bot",
      ],
      [
        (run) =>
          (run.agent[run.agent.length - 1] = agentStopped(T0 + 25 * HOUR + 36 * MIN, {
            aborted: 1,
          })),
        "the agent's stop aborted or crashed a bot",
      ],
    ];
    for (const [change, reason] of cases) {
      const run = goodRun();
      change(run);

      const { code, text } = checked(run);

      assert.equal(code, 1, reason);
      assert.deepEqual(reasons(text), [reason]);
      rmSync(folder, { recursive: true, force: true });
      folder = mkdtempSync(path.join(tmpdir(), "dod-log-check-"));
    }
  });
});

describe("the run's starts and length", () => {
  it("fails unless there's exactly one start of each, each with its log and stderr", () => {
    const twice = goodRun();
    plant(twice);
    plant(twice, { server: "server-20261104T080000Z", agent: "agent-20261103T192901Z" });
    assert.deepEqual(reasons(checkLogs(folder, CONTEXT).text), [
      "2 server starts in the logs folder; the run is one start of each",
    ]);
    rmSync(folder, { recursive: true, force: true });
    folder = mkdtempSync(path.join(tmpdir(), "dod-log-check-"));

    const noAgent = goodRun();
    noAgent.agent = null;
    noAgent.agentStderr = undefined;
    noAgent.agentExit = undefined;
    assert.deepEqual(reasons(checked(noAgent).text), ["no agent start in the logs folder"]);
    rmSync(folder, { recursive: true, force: true });
    folder = mkdtempSync(path.join(tmpdir(), "dod-log-check-"));

    const noStderr = goodRun();
    noStderr.agentStderr = undefined;
    assert.ok(reasons(checked(noStderr).text).includes("the agent's start has no stderr file"));
    rmSync(folder, { recursive: true, force: true });
    folder = mkdtempSync(path.join(tmpdir(), "dod-log-check-"));

    const noLog = goodRun();
    noLog.server = null;
    assert.ok(reasons(checked(noLog).text).includes("the server's start has no log file"));
  });

  it("fails a bot that was Online under the run's 25 h", () => {
    const run = goodRun();
    const end = T0 + 24 * HOUR + 50 * MIN;
    run.agent.splice(
      -3,
      3,
      state(end, "Stopping { since: x }"),
      state(end + 1_000, "Stopped"),
      agentStopped(end + 2_000),
    );
    run.server[run.server.length - 1] = serverStopped(end + 3_000);

    const { code, text } = checked(run);

    assert.equal(code, 1);
    assert.deepEqual(reasons(text), [
      "bot 1 was Online from +0:00 to +24:50, under the run's 25 h",
    ]);
  });

  it("fails when no bot got a session", () => {
    const run = goodRun();
    run.server = [run.server[0], run.server.at(-1)];

    const { code, text } = checked(run);

    assert.equal(code, 1);
    assert.ok(reasons(text).includes("no bot got a session"));
  });
});

describe("nothing to check", () => {
  it("exits 2 when the folder doesn't exist or holds no start", () => {
    assert.deepEqual(checkLogs(path.join(folder, "logs"), CONTEXT), {
      code: 2,
      text: "No logs to check: the logs folder doesn't exist. just dod-server and just dod-agent write it.\n",
    });
    writeFileSync(path.join(folder, "notes.txt"), "x");
    assert.deepEqual(checkLogs(folder, CONTEXT), {
      code: 2,
      text: "No logs to check: the logs folder holds no server or agent start.\n",
    });
  });

  it("exits 2 with its usage on a wrong command line", async () => {
    const said = [];

    const code = await main([], {
      stdout: (text) => said.push(text),
      stderr: (text) => said.push(text),
    });

    assert.equal(code, 2);
    assert.deepEqual(said, ["usage: node scripts/dod-log-check.mjs <logs folder>\n"]);
  });
});

describe("logs it can't read (PR #93's review)", () => {
  /** What `main` prints for the folder, by stream, and its exit code. */
  async function mainSays() {
    const said = { out: [], err: [] };
    const code = await main([folder], {
      stdout: (text) => said.out.push(text),
      stderr: (text) => said.err.push(text),
    });
    return { code, said };
  }

  /** The text for an error with `code`: no path, nothing judged. */
  const unread = (code) => `The check couldn't read the logs (${code}); nothing was judged.\n`;

  it("says only the error's code and exits 2 when a log is a dangling link", async () => {
    plant(goodRun());
    symlinkSync(
      path.join(folder, "gone"),
      path.join(folder, "agent-20261103T192902Z.jsonl"),
      process.platform === "win32" ? "junction" : "file",
    );

    const { code, said } = await mainSays();

    assert.equal(code, 2);
    assert.deepEqual(said, { out: [], err: [unread("ENOENT")] });
  });

  it("says only the error's code and exits 2 when a log can't be read", {
    skip: process.getuid?.() === 0 && "root reads every file",
  }, async () => {
    plant(goodRun());
    const log = path.join(folder, "server-20261103T192900Z.jsonl");
    // Windows keeps a file readable whatever its mode, so a deny entry takes read away there.
    if (process.platform === "win32") {
      execFileSync("icacls", [log, "/deny", "*S-1-1-0:(R)"], { stdio: "ignore" });
    } else {
      chmodSync(log, 0o000);
    }
    try {
      const { code, said } = await mainSays();

      assert.equal(code, 2);
      assert.deepEqual(said, {
        out: [],
        err: [unread(process.platform === "win32" ? "EPERM" : "EACCES")],
      });
    } finally {
      if (process.platform === "win32") {
        execFileSync("icacls", [log, "/remove:d", "*S-1-1-0"], { stdio: "ignore" });
      } else {
        chmodSync(log, 0o600);
      }
    }
  });

  it("names an error by its code alone, and one without a code as unknown", () => {
    assert.equal(
      errorCode(Object.assign(new Error("C:\\path\\to\\afkfleet"), { code: "EBUSY" })),
      "EBUSY",
    );
    assert.equal(
      errorCode(Object.assign(new Error("x"), { code: "C:\\path\\to\\afkfleet" })),
      "unknown",
    );
    assert.equal(errorCode(new TypeError("C:\\path\\to\\afkfleet")), "unknown");
    assert.equal(errorCode("C:\\path\\to\\afkfleet"), "unknown");
  });
});

describe("its readers", () => {
  it("reads chrono's Display times, with or without fractions", () => {
    assert.equal(
      chronoTime("2026-11-04 19:30:00.123456789 UTC"),
      Date.parse("2026-11-04T19:30:00.123Z"),
    );
    assert.equal(chronoTime("2026-11-04 19:30:00 UTC"), Date.parse("2026-11-04T19:30:00Z"));
    assert.equal(chronoTime("2026-11-04T19:30:00Z"), null);
    assert.equal(chronoTime("soon"), null);
  });

  it("reads string literals the way Rust writes them: escapes, raw strings and continued lines", () => {
    const dir = path.join(folder, "crates", "x", "src");
    mkdirSync(dir, { recursive: true });
    writeFileSync(
      path.join(dir, "lib.rs"),
      'const A: &str = "a \\"quoted\\" word";\nconst B: &str = r#"raw "text""#;\nconst C: &str = "one \\\n    line";\n// "a comment"\n',
    );

    const literals = sourceLiterals(folder);

    assert.ok(literals.has('a "quoted" word'));
    assert.ok(literals.has('raw "text"'));
    assert.ok(literals.has("one line"));
  });

  it("reads Cargo.lock's crate names as targets write them", () => {
    const crates = lockedCrates(
      '[[package]]\nname = "azalea-client"\nversion = "0.16.0"\n\n[[package]]\nname = "fleet-server"\n',
    );

    assert.ok(crates.has("azalea_client"));
    assert.ok(crates.has("fleet_server"));
    assert.ok(
      crates.has("afkfleet_server") && crates.has("afkfleet_agent") && crates.has("afkfleet"),
    );
  });

  it("reads the workspace's module paths as targets write them, and nothing else", () => {
    const paths = modulePaths(ROOT);

    for (const known of [
      "fleet_server",
      "fleet_server::app::agents::session_grants",
      "fleet_mc::host",
      "fleet_mc::connector",
      "fleet_agent::run",
      "fleet_proto::generated",
      "afkfleet_server",
      "afkfleet_agent",
      TEXTS.panicTarget,
    ]) {
      assert.ok(paths.has(known), known);
    }
    for (const unknown of [
      "fleet_server::lib",
      "fleet_server::main",
      "fleet_agent::run::mod",
      "afkfleet_server::app",
      "fleet_mc::host::rs",
      "azalea_client",
    ]) {
      assert.ok(!paths.has(unknown), unknown);
    }
  });
});

describe("what it pins against the sources", () => {
  /** A Rust file of this repository. */
  const rust = (file) => readFileSync(path.join(ROOT, "crates", file), "utf8");

  it("reads the messages each line is logged with", () => {
    for (const [file, message] of [
      ["fleet-server/src/app/agents/session_grants.rs", TEXTS.granted],
      ["fleet-runtime/src/actor/effects.rs", TEXTS.stateChanged],
      ["fleet-server/src/app/bots/lifecycle.rs", TEXTS.desiredSet],
      ["fleet-server/src/app/bots/lifecycle.rs", TEXTS.commandSent],
      ["fleet-server/src/app/bots/lifecycle.rs", TEXTS.stickyCleared],
      ["fleet-server/src/http/middleware/trace.rs", TEXTS.requestFinished],
      ["fleet-server/src/cli/serve.rs", TEXTS.serverStopped],
      ["fleet-agent/src/run/mod.rs", TEXTS.agentStopped],
      ["fleet-startup/src/telemetry/panic.rs", TEXTS.panicked],
    ]) {
      assert.ok(rust(file).includes(JSON.stringify(message)), `${file}: ${message}`);
    }
    assert.match(
      rust("fleet-startup/src/telemetry/panic.rs"),
      new RegExp(`PANIC_TARGET: &str = "${TEXTS.panicTarget}"`),
    );
    assert.match(
      rust("fleet-server/src/app/agents/session_grants.rs"),
      /kind = "online",\n\s+fresh = request\.fresh,\n\s+expires_at = %expires_at,/,
    );
  });

  it("reads the lifecycle routes, BotState's variants and REUSE_MARGIN", () => {
    for (const route of ["start", "stop", "restart", "reset", "resume"]) {
      assert.match(
        rust("fleet-server/src/http/router.rs"),
        new RegExp(`POST /api/v1/bots/\\{id\\}/${route}\``),
      );
    }
    const variants = [
      ...rust("fleet-core/src/bot/state.rs").matchAll(/^ {4}([A-Z][A-Za-z]+)(?: \{|,)$/gm),
    ].map((match) => match[1]);
    assert.deepEqual(variants.slice(0, BOT_STATES.length), BOT_STATES);
    assert.match(
      rust("fleet-server/src/app/session_tokens/mod.rs"),
      /pub const REUSE_MARGIN: Duration = Duration::from_mins\(10\);/,
    );
    assert.equal(REUSE_MARGIN_MS, 10 * MIN);
  });
});

describe("just dod-log-check", () => {
  it("reads target/dod-run/logs and runs only the check", () => {
    assert.deepEqual(recipeParameters("dod-log-check"), {
      dir: '(justfile_directory() / "target" / "dod-run" / "logs")',
    });
    assert.deepEqual(recipeBody("dod-log-check"), ['node scripts/dod-log-check.mjs "{{ dir }}"']);
  });
});
