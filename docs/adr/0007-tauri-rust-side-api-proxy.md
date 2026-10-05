# 0007. The desktop app talks to the server only from Rust

- **Status:** Accepted
- **Date:** 2026-10-05
- **Related:** Plan.md §2, §7.1, §7.6, Phase 8; ADR-0006

## Context
The desktop app is Tauri v2 with a React UI that friends install on their own machines. The UI renders untrusted content (chat from Minecraft servers). If tokens were reachable from JavaScript, any XSS bug would hand an attacker the session. A browser-style `fetch` from the webview to the server would also need a CORS policy on the server.

## Decision
**All server traffic goes through Rust.**
- The React UI (`packages/ui`) never calls `fetch` against the server. It talks to an injected `ApiClient` interface; in the desktop app that's `TauriApiClient`, which calls typed Tauri commands.
- The Rust side (`apps/desktop/src-tauri`) uses `fleet-client` (reqwest + rustls with aws-lc-rs) for HTTP and the WebSocket. Live events reach the UI through a Tauri `Channel`.

**Tokens never reach JavaScript.**
- The refresh token is stored in the **OS credential store** (`keyring-core` + `windows-native-keyring-store`).
- The access token lives only in Rust memory.

**The server has no CORS layer.** The future website will be served from the same origin (Plan.md Phase 14).

**Tauri hardening** (Plan.md §7.6):
- strict CSP with no remote origins, `connect-src` limited to IPC
- capabilities only for our own commands
- `withGlobalTauri: false`
- devtools only in debug builds
- signed updates

**Certificates.** The server profile accepts a custom CA, but invalid certificates are never accepted.

## Consequences
- **Smaller XSS blast radius.** An XSS bug in the UI can't steal tokens. It can still call our commands while the app is open; the server's authorization still applies.
- **One code path for the API client.** `fleet-client` is used by the app and by the integration tests.
- **Reusable UI.** `packages/ui` stays platform-agnostic: a web build only needs another `ApiClient` implementation.
- **Cost.** Every endpoint needs a Rust command and a TypeScript client method. The types are generated (ts-rs), so the DTOs aren't duplicated by hand.

## Alternatives considered
- **`fetch` from the webview with tokens in memory or `localStorage`.** Tokens become reachable by XSS, and the server needs CORS.
- **HttpOnly cookies from the webview.** Needs CORS with credentials and CSRF protection, and Tauri's custom-protocol origin makes cookie handling awkward. That's the right model for the same-origin website later, not for the desktop app.
