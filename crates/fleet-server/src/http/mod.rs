//! The HTTP API (Plan.md §4, P6.5 on): the router, its middleware, the
//! extractors, one handler module per resource, the error type and the
//! WebSocket.
//!
//! - [`error`]: [`ApiError`](error::ApiError), how every failed request
//!   answers, and [`render_errors`](error::render_errors), the middleware
//!   that writes its body.
//!
//! The router and the middleware stack arrive with P6.6 and P6.7, the
//! handlers with the endpoints from P7 on.

pub mod error;
