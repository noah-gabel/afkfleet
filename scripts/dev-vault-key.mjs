// Creates a server's vault key when it's missing: the dev server's, or the DoD checks'.
//
//   node scripts/dev-vault-key.mjs <path>
//
// `just dev-server` runs it before the server starts, with the absolute path it passes the
// server as AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__1: target/dev-server/vault-key-1 by
// default, next to the dev database whose secrets it encrypts, so `cargo clean` removes both
// together (ADR-0016). `just dod-server` runs it with target/dod-run/vault-key-1, next to the
// DoD checks' database (ADR-0020, U1, S72). The key is 32 random bytes as standard base64
// with padding and a line break, written owner-only (0600 on Unix; Windows uses ACLs). An
// existing file is never overwritten, read or checked: the server checks it when it starts.
// The key is never printed. Exits with 0 when the key exists or was created, 1 on an error,
// 2 on a usage error. Tests: scripts/dev-vault-key.test.mjs.
import { randomBytes } from "node:crypto";
import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

/** The key's length in bytes: one XChaCha20-Poly1305 key. */
export const KEY_BYTES = 32;

/**
 * Creates the key file at `file` with a fresh key unless it exists, creating its folder too.
 * Returns "created" or "kept". The `wx` flag makes creating atomic and never overwrites.
 */
export function ensureKey(file) {
  mkdirSync(path.dirname(file), { recursive: true });
  try {
    writeFileSync(file, `${randomBytes(KEY_BYTES).toString("base64")}\n`, {
      flag: "wx",
      mode: 0o600,
    });
    return "created";
  } catch (error) {
    if (error.code === "EEXIST") {
      return "kept";
    }
    throw error;
  }
}

function main(args) {
  const [file] = args;
  if (file === undefined || args.length !== 1 || !path.isAbsolute(file)) {
    console.error("usage: node scripts/dev-vault-key.mjs <absolute path>");
    return 2;
  }
  try {
    const result = ensureKey(file);
    if (result === "created") {
      console.log(`created the dev vault key at ${file}`);
    }
    return 0;
  } catch (error) {
    console.error(`can't create the dev vault key at ${file}: ${error.message}`);
    return 1;
  }
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = main(process.argv.slice(2));
}
