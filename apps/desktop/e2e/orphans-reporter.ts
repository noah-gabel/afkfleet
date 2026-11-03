// The baselines' orphans (Plan.md P11.11; ADR-0020, U125, PR #92's review): a baseline no row shoots
// compares nothing, so a renamed or removed row would leave its old PNGs behind unnoticed. In a
// compare that otherwise passed, this reporter lists every baseline no test shot (each shot leaves
// its file's name as an annotation, e2e/specs/shoot.ts) and fails the run. An update writes what's
// new and leaves the old ones; the next compare names them for deleting. A design run has no
// baselines to check. Only the shots' config (playwright.shots.config.ts) loads it.
import { readdirSync } from "node:fs";
import path from "node:path";
import process from "node:process";

import type {
  FullConfig,
  FullResult,
  Reporter,
  TestCase,
  TestResult,
} from "@playwright/test/reporter";

import { DESIGN_MARKER, orphans, SHOT_ANNOTATION } from "./shots";

/** The theme sweep's baselines, from the config's folder: the only spec that shoots them. */
const BASELINES = path.join("e2e", "baselines", "theme-sweep.spec.ts");

/** Fails a passing compare that left a baseline no row shot. */
export default class OrphansReporter implements Reporter {
  #compare = false;
  #folder = "";
  readonly #taken = new Set<string>();

  onBegin(config: FullConfig): void {
    this.#compare = config.updateSnapshots === "none" && process.env[DESIGN_MARKER] !== "1";
    this.#folder = path.join(path.dirname(config.configFile ?? ""), BASELINES);
  }

  onTestEnd(test: TestCase, result: TestResult): void {
    for (const note of [...test.annotations, ...result.annotations]) {
      if (note.type === SHOT_ANNOTATION && note.description !== undefined) {
        this.#taken.add(note.description);
      }
    }
  }

  onEnd(result: FullResult): Promise<{ status?: FullResult["status"] } | undefined> {
    if (!this.#compare || result.status !== "passed") {
      return Promise.resolve(undefined);
    }
    const left = orphans(
      readdirSync(this.#folder).filter((file) => file.endsWith(".png")),
      [...this.#taken],
    );
    if (left.length === 0) {
      return Promise.resolve(undefined);
    }
    console.error(
      `These baselines belong to no row, so they compare nothing. Delete each, or bring back its row:\n${left.join("\n")}`,
    );
    return Promise.resolve({ status: "failed" });
  }
}
