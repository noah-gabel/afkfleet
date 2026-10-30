---
paths:
  - "packages/**"
  - "apps/desktop/**"
  - "crates/fleet-api-types/src/app.rs"
---

# Frontend rules
Moved from `CLAUDE.md` word for word; it points here.

**Structure**
- `packages/ui` doesn't know about the platform: it **never imports `@tauri-apps/*`**.
- `apps/desktop` implements `ApiClient` as `TauriApiClient`, with `invoke` injected, and passes it to `<App apiClient={…} />`. It imports only `@tauri-apps/api/core`: any other Tauri API needs a capability grant and an ADR. Biome's `noRestrictedImports` enforces both rules (ADR-0017).
- A new command changes build.rs's `COMMANDS`, `lib.rs`'s `generate_handler!`, `capabilities/main.json`, `scripts/tauri-hardening.mjs`, `src-tauri/tests/ipc-contract.json`, `ApiClient` with `API_METHODS`, `TauriApiClient`, `FakeApiClient`, `UnavailableApiClient`, `validated.ts`, `app/watched.ts` and the e2e fake's `COMMANDS` together.
- Organize by feature: `src/features/<feature>/{components,hooks,routes}` with tests alongside. Shared primitives go in `src/components/ui` (shadcn).
- `apps/desktop/e2e` is the e2e build: the app's UI with the real `TauriApiClient` over a fetch-based `invoke`, its fake backend answering through MSW in the Playwright process. It never ships: Biome refuses any import of it in `apps/desktop/src`, and `just e2e`'s bundle guard checks both shipped bundles (ADR-0017).
- **shadcn components are copied from the registry** (`https://ui.shadcn.com/r/styles/radix-nova/<name>.json`), never added with the CLI, which installs packages of its own choosing. The copy gets the CLI's edits (aliases, no `"use client"`), its `radix-ui` import becomes the single `@radix-ui/react-*` package, and a lint finding gets a minimal code fix, never an ignore comment; the file's header lists every edit (ADR-0017).

**Data and state**
- Components use hooks (`useBots()`, …) built on TanStack Query over the injected `ApiClient`. **Never call `fetch` or `invoke` inside a component.**
- TanStack Query holds server state; small `zustand` stores hold UI-only state, one set per `App` (`app/stores.ts`).
- **A call whose request or answer carries a secret** (a password, a TOTP secret or QR code, recovery codes, an invite code, a Microsoft link's user code) **is a direct call** through `useApiClient()`, never a TanStack query or mutation, which would keep it in their caches: `login`, `register`, `loginMfa`, `mfaEnroll`, `mfaConfirm`, `mfaReset`, `stepUp`, `changePassword`, `createInvite`, `startLink`, `relinkAccount`, `sendChat`, `sendCommand` (a typed command can hold a password). The secret lives only in the component's state and is dropped when its step ends (ADR-0017, ADR-0018). The chat history (`botChat`) stays a cached query: a command's arguments reach only Manage holders (S6, S9), it lives in memory only, and it's dropped at the session's end and at `bot_removed` (the user's choice, ADR-0020, U34).
- Every call goes through the watched client (`app/watched.ts`): it raises the banner on `unreachable`/`timed_out`/`certificate` and ends a signed-in session on `no_session`. Don't handle those codes again in a screen.
- Forms use react-hook-form with zod schemas that mirror the server's limits (`src/api/schemas.ts`). The server stays authoritative.
- `App` wraps its client in `validated()`, so every input passes its zod schema before any call. `src/api/limits.ts` holds only numeric constants named like the Rust ones; fleet-api-types' `tests/limits.rs` keeps them equal.
- Every API call rejects with an `ApiError` holding a `CommandError`. A switch on its `code` is exhaustive (it's closed: the Rust side ships with the UI); a switch on `api.code`, or on any other open value, always has a default branch that shows the message and the request ID (`describeError`).
- API types come only from `src/generated/` (ts-rs). **Never edit generated files**; run `just gen`. Biome ignores `src/generated/`.

**Safety**
- Never use `dangerouslySetInnerHTML`, `eval` or `new Function`. Nor may a library at runtime: the release CSP has no 'unsafe-eval'. zod is imported only from `src/api/zod.ts`, which makes it jitless (Biome enforces it; `src/api/zod.test.ts` checks it).
- **No runtime styles.** The release CSP has `style-src 'self'`, so a `<style>` element or a constructed stylesheet added at runtime is blocked in the real build while `just dev-app` shows nothing wrong. Every Vitest test ends with a check that the document holds none (`src/test/styleGuard.ts`). sonner is patched for this (`patches/sonner@<version>.patch`); a sonner bump redoes the patch and runs `Toaster.test.tsx` (ADR-0017). Check a new UI dependency in `just app-build --debug` too.
- Chat is rendered as plain text.
- External links open only through the Rust-side opener command.

**Quality**
- TypeScript strict; no `any`; no non-null `!` without a comment.
- Tests use Testing Library (query by role or label), with a `FakeApiClient` or MSW standing in for the API. Playwright covers flows.

## The desktop app's IPC types
- The desktop app's IPC types (`CommandError`, the login step, the logout outcome, the server profile's types, the live events' updates) live in `fleet-api-types::app`: exported by `just gen` with the DTOs, left out of the `OpenAPI` document (ADR-0017).
