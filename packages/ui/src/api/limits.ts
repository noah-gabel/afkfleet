// The server's limits on the app's inputs, which the zod schemas mirror (schemas.ts).
// Each name is the Rust constant's; fleet-api-types' `tests/limits.rs` checks every value
// against it, so they can't drift. The server stays authoritative. Only `export const
// NAME = <number>;` lines belong here.
export const USERNAME_MIN_CHARS = 3;
export const USERNAME_MAX_CHARS = 32;
export const PASSWORD_MAX_CHARS = 1024;
export const NEW_PASSWORD_MIN_CHARS = 12;
export const NEW_PASSWORD_MAX_CHARS = 128;
export const CODE_MAX_CHARS = 64;
export const TOKEN_MAX_CHARS = 64;
export const ID_CHARS = 36;
export const PAGE_MAX = 100;
export const SERVER_URL_MAX_BYTES = 2048;
export const CA_PEM_MAX_BYTES = 65536;
