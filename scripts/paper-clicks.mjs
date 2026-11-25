// How fast a person's own client clicked on the Paper harness (Plan.md P16.2,
// ADR-0049): reads the harness's log through docker compose and sums up its
// `harness-click` lines: how many clicks, the shortest gap between two, the most
// in one server tick and in any one second, and how many of each kind. It prints
// no player's name. `just paper-clicks`, after `just paper-up` and a session of
// clicking; the user's step alone, and optional.
//
// Usage: node scripts/paper-clicks.mjs
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

/** One click as the harness logged it, or undefined for any other line. */
export function click(line) {
  if (!line.includes("harness-click ")) {
    return undefined;
  }
  const field = (name) => line.match(new RegExp(`\\b${name}=(\\S+)`))?.[1];
  const tick = Number(field("tick"));
  const nanos = Number(field("nanos"));
  const kind = field("click");
  if (!Number.isFinite(tick) || !Number.isFinite(nanos) || kind === undefined) {
    return undefined;
  }
  return { tick, nanos, kind };
}

/** The summary of `lines`' clicks, or undefined when there's none. */
export function summarize(lines) {
  const clicks = lines.map(click).filter((one) => one !== undefined);
  if (clicks.length === 0) {
    return undefined;
  }
  clicks.sort((a, b) => a.nanos - b.nanos);
  let shortestGapMs = Number.POSITIVE_INFINITY;
  for (let i = 1; i < clicks.length; i++) {
    shortestGapMs = Math.min(shortestGapMs, (clicks[i].nanos - clicks[i - 1].nanos) / 1e6);
  }
  const perTick = new Map();
  const kinds = new Map();
  for (const { tick, kind } of clicks) {
    perTick.set(tick, (perTick.get(tick) ?? 0) + 1);
    kinds.set(kind, (kinds.get(kind) ?? 0) + 1);
  }
  let mostInASecond = 0;
  let first = 0;
  for (let last = 0; last < clicks.length; last++) {
    while (clicks[last].nanos - clicks[first].nanos >= 1e9) {
      first++;
    }
    mostInASecond = Math.max(mostInASecond, last - first + 1);
  }
  return {
    clicks: clicks.length,
    shortestGapMs: clicks.length > 1 ? shortestGapMs : undefined,
    mostInATick: Math.max(...perTick.values()),
    mostInASecond,
    kinds: Object.fromEntries([...kinds].sort(([a], [b]) => a.localeCompare(b))),
  };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const log = execFileSync(
    "docker",
    [
      "compose",
      "--file",
      "deploy/compose.paper.yaml",
      "logs",
      "--no-color",
      "--no-log-prefix",
      "paper",
    ],
    { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 },
  );
  const summary = summarize(log.split(/\r?\n/));
  if (summary === undefined) {
    console.log(
      "no clicks in the harness's log yet: join 127.0.0.1:25567 and click (docs/development.md)",
    );
  } else {
    console.log(`clicks: ${summary.clicks}`);
    console.log(`shortest gap between two: ${summary.shortestGapMs?.toFixed(1) ?? "-"} ms`);
    console.log(`most in one server tick: ${summary.mostInATick}`);
    console.log(`most in any one second: ${summary.mostInASecond}`);
    for (const [kind, count] of Object.entries(summary.kinds)) {
      console.log(`  ${kind}: ${count}`);
    }
  }
}
