//! The HTTP API (Plan.md §4, P6.5 on): the router, its middleware, the
//! extractors, one handler module per resource, the error type and the
//! WebSocket.
//!
//! - [`error`]: [`ApiError`](error::ApiError), how every failed request
//!   answers, and [`render_errors`](error::render_errors), the middleware
//!   that writes the body of every error response.
//! - [`extractors`]: [`AuthUser`](extractors::AuthUser), who a request comes
//!   from, looked up on every request (P7.8).
//! - [`middleware`]: the stack every request passes through, from the
//!   request ID and the security headers to the timeout and the body limit
//!   (P6.6).
//! - [`openapi`]: the `OpenAPI` document, served only in dev mode (P6.10).
//! - [`rate_limit`]: §7.4's request limits per client IP, route class and
//!   user, and who a request comes from behind the trusted proxies (P7.10).
//! - [`router`]: the server's routes behind that stack (P6.7 on).
//! - [`routes`]: the route registry, the only way a route gets into the
//!   router: public or authed, each with its rate-limit class (P7.11).
//! - `handlers`: one module per resource, starting with the health checks
//!   (P6.7); the other handlers arrive with the endpoints from P7 on.

pub mod error;
pub mod extractors;
mod handlers;
pub mod middleware;
pub mod openapi;
pub mod rate_limit;
pub mod router;
pub mod routes;
