// The one explanation of a refused certificate (Plan.md P8.6/P8.7, ADR-0017), shown by
// server setup, the start screen and the banner alike.

/** What a refused certificate means and what to do. */
export const CERTIFICATE_HELP =
  "The server's certificate isn't trusted. It may have changed or expired, the server may use its own CA (ask its admin for the CA file and compare the fingerprints), or someone may be intercepting the connection. Don't go on until the server's admin has confirmed it.";
