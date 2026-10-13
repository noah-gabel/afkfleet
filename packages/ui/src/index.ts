// The UI package's public entry: the desktop app (and later the web build) render `App`
// with its platform's `ApiClient`. The API's types come from `@afkfleet/ui/generated/<Type>`,
// one import per type.

export { App } from "./App";
export type { ApiClient, ApiMethod } from "./api/ApiClient";
export { API_METHODS } from "./api/ApiClient";
export { ApiError, isCommandError, toApiError } from "./api/ApiError";
