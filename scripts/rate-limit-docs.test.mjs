// Every `[rate_limits]` key the server reads is in Plan.md's sample config (Appendix A) and in the
// developers' guide's config list (`docs/development.md`), and every limit `RateLimits` checks is
// in its module's list of where each one is spent (ADR-0020, PR #90's review: the suggestions'
// limit reached none of the three).
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

/** The keys of `RawRateLimits` in `raw`, in order. */
function rateLimitKeys(raw) {
  const body = /pub\(crate\) struct RawRateLimits \{([\s\S]*?)\n\}/.exec(raw)?.[1] ?? "";
  return [...body.matchAll(/pub\(crate\) ([a-z0-9_]+):/g)].map((match) => match[1]);
}

/** The keys Plan.md's sample `[rate_limits]` table sets. */
function sampleKeys(plan) {
  const table = /^\[rate_limits\]\n([\s\S]*?)\n\n\[/m.exec(plan)?.[1] ?? "";
  return table.split("\n").map((line) => /^([a-z0-9_]+) =/.exec(line)?.[1]);
}

/** The limits `RateLimits` checks: its public `check_*` methods and `open_socket`. */
function checks(source) {
  return [...source.matchAll(/^ {4}pub fn ((?:check_[a-z_]+)|open_socket)\(/gm)].map(
    (match) => match[1],
  );
}

/** The module header of `source`, its `//!` lines. */
function header(source) {
  return source
    .split("\n")
    .filter((line) => line.startsWith("//!"))
    .join("\n");
}

const raw = read("crates/fleet-server/src/config/raw.rs");
const keys = rateLimitKeys(raw);

describe("the rate limits' docs", () => {
  it("reads the server's keys", () => {
    assert.ok(keys.length > 10, `only ${keys.length} keys read`);
    assert.ok(keys.includes("grant_candidates_per_user_per_hour"));
  });

  it("Plan.md's sample config sets every key", () => {
    const sample = sampleKeys(read("Plan.md"));
    assert.deepEqual(
      keys.filter((key) => !sample.includes(key)),
      [],
    );
  });

  it("the developers' guide names every key", () => {
    const guide = read("docs/development.md");
    assert.deepEqual(
      keys.filter((key) => !new RegExp(`[\`\\s]${key}\``).test(guide)),
      [],
    );
  });

  it("the module's list names every limit it checks", () => {
    const source = read("crates/fleet-server/src/http/rate_limit/mod.rs");
    const named = header(source);
    const all = checks(source);
    assert.ok(all.includes("check_grant_candidates"), all.join(", "));
    assert.deepEqual(
      all.filter((name) => !named.includes(`RateLimits::${name}\``)),
      [],
    );
  });
});
