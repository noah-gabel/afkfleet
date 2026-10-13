// The e2e build's marker (Plan.md P8.9, ADR-0017): a text only the e2e bundle holds.
// scripts/e2e-bundle-guard.mjs requires it there and refuses it in the shipped bundles, so a
// shipped build can't carry the e2e entry; the guard repeats the value on purpose.
export const E2E_MARKER = "afkfleet-e2e-build-only";
