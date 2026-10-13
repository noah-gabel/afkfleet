// Where `vite preview` serves the e2e build (Plan.md P8.9): playwright.config.ts's base URL,
// vite.e2e.config.ts's preview, and the origin the fake's handlers answer on.
export const HOST = "127.0.0.1";
export const PORT = 4173;
export const BASE_URL = `http://${HOST}:${PORT}`;
