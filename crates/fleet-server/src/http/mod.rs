//! The HTTP API (Plan.md §4, P6.5 on): the router, its middleware, the
//! extractors, one handler module per resource, the error type and the
//! WebSocket.
//!
//! - [`error`]: [`ApiError`](error::ApiError), how every failed request
//!   answers, and [`render_errors`](error::render_errors), the middleware
//!   that writes the body of every error response.
//! - [`middleware`]: the stack every request passes through, from the
//!   request ID and the security headers to the timeout and the body limit
//!   (P6.6).
//! - [`router`]: the server's routes behind that stack (P6.7 on).
//! - `handlers`: one module per resource, starting with the health checks
//!   (P6.7); the other handlers arrive with the endpoints from P7 on.

pub mod error;
mod handlers;
pub mod middleware;
pub mod router;
