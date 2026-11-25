// The Paper harness's supply chain, checked in its Dockerfile (Plan.md P16.1,
// ADR-0049): every base image pinned by digest, every download by SHA-256 through
// `ADD --checksum` (which Docker checks, failing the build on any other bytes),
// nothing else that reaches the network, and every RUN after the first download
// without network. `just scripts-test` runs it on deploy/docker/paper.Dockerfile.
//
// Usage: node scripts/harness-pins.mjs [Dockerfile]
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

/** The Dockerfile it checks by default. */
export const DOCKERFILE = "deploy/docker/paper.Dockerfile";

/**
 * Joins a Dockerfile's continued lines and drops its comments: one entry per
 * instruction, as Docker reads them.
 */
export function instructions(text) {
  const out = [];
  let current = "";
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (current === "" && (line === "" || line.startsWith("#"))) {
      continue;
    }
    if (line.endsWith("\\")) {
      current += `${line.slice(0, -1).trim()} `;
      continue;
    }
    current += line;
    out.push(current.trim());
    current = "";
  }
  if (current.trim() !== "") {
    out.push(current.trim());
  }
  return out;
}

/** What's wrong with the harness's Dockerfile `text`, one line each; empty when nothing is. */
export function problems(text) {
  const found = [];
  const stages = new Set();
  let downloaded = false;
  for (const instruction of instructions(text)) {
    const [keyword] = instruction.split(/\s+/, 1);
    const word = keyword.toUpperCase();
    if (word === "FROM") {
      const parts = instruction.split(/\s+/);
      const image = parts[1] ?? "";
      const alias = parts[2]?.toUpperCase() === "AS" ? parts[3] : undefined;
      if (!stages.has(image) && !/@sha256:[0-9a-f]{64}$/.test(image)) {
        found.push(`FROM ${image} isn't pinned by digest`);
      }
      if (alias !== undefined) {
        stages.add(alias);
      }
    } else if (word === "ADD") {
      const url = instruction.match(/\bhttps?:\/\/\S+/);
      if (url !== null) {
        if (!url[0].startsWith("https://")) {
          found.push(`ADD ${url[0]} isn't https`);
        }
        if (!/--checksum=sha256:[0-9a-f]{64}(\s|$)/.test(instruction)) {
          found.push(`ADD ${url[0]} has no --checksum=sha256:<64 hex digits>`);
        }
        downloaded = true;
      } else if (/\bgit@|\.git(\s|$)/.test(instruction)) {
        found.push(`ADD from git: ${instruction}`);
      }
    } else if (word === "RUN") {
      if (/\b(curl|wget|apt-get|apt|apk|pip|npm|gradle|mvn|git)\b/.test(instruction)) {
        found.push(`RUN fetches with a tool of its own: ${instruction}`);
      }
      if (downloaded && !/^RUN\s+--network=none\b/.test(instruction)) {
        found.push(`RUN after a download without --network=none: ${instruction}`);
      }
    } else if (word === "COPY" && /--from=\S*[/:@]/.test(instruction)) {
      found.push(`COPY from an image instead of a stage: ${instruction}`);
    }
  }
  if (!downloaded) {
    found.push("no checked download: Paper's jar comes in by ADD --checksum");
  }
  return found;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const file = process.argv[2] ?? DOCKERFILE;
  const found = problems(readFileSync(file, "utf8"));
  if (found.length > 0) {
    for (const problem of found) {
      console.error(`${file}: ${problem}`);
    }
    process.exit(1);
  }
  console.log(`${file}: every input pinned, nothing fetched after the downloads`);
}
