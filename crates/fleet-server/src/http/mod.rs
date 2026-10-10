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
//!
//! The router arrives with P6.7, the other handlers with the endpoints from
//! P7 on.

pub mod error;
pub mod middleware;
