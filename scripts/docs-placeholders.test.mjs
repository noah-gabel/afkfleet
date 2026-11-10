// Tests that the operators' and the friends' guides use placeholders alone (CLAUDE.md
// rule 11; ADR-0022, U2, S47): host names under example.com, example.net or example.org,
// beside the repository's own github.com and ghcr.io, Rust's crates.io and the compose
// project's one-label service names; the documentation addresses (RFC 5737), loopback, 0.0.0.0 and afkfleet's
// own default subnets under 172.30.0.0/16; emails at those example domains; and no home
// folder of anyone's. That no hosting provider, cloud or DNS service is named isn't
// something a list here could check without naming them: review keeps it. Run with
// `just scripts-test`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, it } from "node:test";

import { ROOT } from "./threat-model-citations.mjs";

/**
 * The guides checked, from the repository's root: README, the docs' index, the operators', the
 * friends', the maintainer's and the DoD's kit. `docs/development.md` is the developers', naming
 * their tools' sites and the private networks an agent may dial.
 */
const GUIDES = [
  "README.md",
  "docs/README.md",
  "docs/runbook.md",
  "docs/using-the-app.md",
  "docs/releasing.md",
  "docs/dod/phase-12.md",
];

/**
 * A host name ending in a common top-level domain, and not part of a longer dotted name
 * (`compose.dev.yaml` is a file's): a file's name never ends in one.
 */
const HOST_NAME =
  "(?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\\.)+(?:com|net|org|io|dev|app|de|uk|eu|info|me|xyz|cloud|co|us|fr|nl|ch|se)";
const HOST = new RegExp(`\\b${HOST_NAME}\\b(?!\\.[a-z0-9])`, "gi");

/** Whether a whole string is such a host name. */
const IS_HOST = new RegExp(`^${HOST_NAME}$`, "i");

/** A URL's host. */
const URL_HOST = /\bhttps?:\/\/([^\s/:)>\]`'"]+)/g;

/** An IPv4 address. */
const IPV4 = /\b(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})\b/g;

/** An email address, its domain captured. */
const EMAIL = /[A-Za-z0-9._%+-]+@([A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+)/g;

/** Someone's home folder: on Windows, or a named one on Linux or macOS. */
const HOME = /[A-Za-z]:\\Users\\(?!<)[^\\\s]*|\/(?:home|Users)\/[^\s/<][^\s/]*/g;

/** Whether `host` is a placeholder's or the repository's own. */
function allowedHost(host) {
  const name = host.toLowerCase();
  return (
    /^(?:[a-z0-9-]+\.)*example\.(?:com|net|org)$/.test(name) ||
    name === "github.com" ||
    name === "ghcr.io" ||
    name === "crates.io"
  );
}

/** Whether the address `parts` is a documentation, loopback or afkfleet default one. */
function allowedAddress(parts) {
  const [a, b, c] = parts.map(Number);
  return (
    (a === 192 && b === 0 && c === 2) ||
    (a === 198 && b === 51 && c === 100) ||
    (a === 203 && b === 0 && c === 113) ||
    a === 127 ||
    parts.every((part) => part === "0") ||
    (a === 172 && b === 30)
  );
}

/** What in `text` isn't a placeholder, as `line N: token`. */
export function placeholderProblems(text) {
  const problems = [];
  text.split("\n").forEach((line, index) => {
    const at = `line ${index + 1}`;
    for (const [host] of line.matchAll(HOST)) {
      if (!allowedHost(host)) {
        problems.push(`${at}: ${host}`);
      }
    }
    for (const [, host] of line.matchAll(URL_HOST)) {
      const placeholder = host.startsWith("<") || !host.includes(".");
      const address = /^\d+\.\d+\.\d+\.\d+$/.test(host);
      // A host name HOST knows was named above already.
      if (!placeholder && !address && !allowedHost(host) && !IS_HOST.test(host)) {
        problems.push(`${at}: ${host}`);
      }
    }
    for (const match of line.matchAll(IPV4)) {
      if (!allowedAddress(match.slice(1, 5))) {
        problems.push(`${at}: ${match[0]}`);
      }
    }
    for (const [email, domain] of line.matchAll(EMAIL)) {
      if (!/^(?:[a-z0-9-]+\.)*example\.(?:com|net|org)$/i.test(domain)) {
        problems.push(`${at}: ${email}`);
      }
    }
    for (const [home] of line.matchAll(HOME)) {
      problems.push(`${at}: ${home}`);
    }
  });
  return problems;
}

describe("placeholderProblems", () => {
  it("passes the placeholders, the repository's own names and afkfleet's defaults", () => {
    const text = [
      "Point fleet.example.com, a.b.example.net and example.org at 192.0.2.10 or 203.0.113.7.",
      "Images on ghcr.io, sources on https://github.com/noah-gabel/afkfleet.",
      "Metrics at http://agent:9481/metrics, 172.30.0.10 and 127.0.0.1:8080, never 0.0.0.0.",
      "Mail admin@example.com; files setup.sh, compose.dev.yaml, env.example and ca.crt.",
      "Crates from crates.io.",
      "Folders ~/afkfleet, C:\\path\\to\\afkfleet and /home/<you>; https://<your domain>.",
    ].join("\n");

    assert.deepEqual(placeholderProblems(text), []);
  });

  it("names a host outside the example domains, in prose and in a URL", () => {
    const text = "Point fleet.my-own-domain.net at it.\nOpen https://panel.my-own-domain.dev/x.";

    assert.deepEqual(placeholderProblems(text), [
      "line 1: fleet.my-own-domain.net",
      "line 2: panel.my-own-domain.dev",
    ]);
  });

  it("names an address outside the documentation, loopback and default ranges", () => {
    assert.deepEqual(placeholderProblems("Reach 100.64.1.2 or 10.1.2.3, not 192.0.2.1."), [
      "line 1: 100.64.1.2",
      "line 1: 10.1.2.3",
    ]);
  });

  it("names an email outside the example domains", () => {
    assert.deepEqual(placeholderProblems("Write to ops@my-own-domain.net."), [
      "line 1: my-own-domain.net",
      "line 1: ops@my-own-domain.net",
    ]);
  });

  it("names a home folder", () => {
    const text = "C:\\Users\\someone\\afkfleet and /home/someone/afkfleet";

    assert.deepEqual(placeholderProblems(text), [
      "line 1: C:\\Users\\someone",
      "line 1: /home/someone",
    ]);
  });
});

describe("the guides", () => {
  for (const guide of GUIDES) {
    it(`${guide} uses placeholders alone`, () => {
      const text = readFileSync(join(ROOT, guide), "utf8");

      assert.deepEqual(placeholderProblems(text), []);
    });
  }
});
