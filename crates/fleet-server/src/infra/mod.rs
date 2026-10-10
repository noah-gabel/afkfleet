//! The adapters that implement the ports: the outside world as the services
//! see it (Plan.md §4, ADR-0015).
//!
//! - [`system`]: the real clock and the OS's secure randomness.
//!
//! Later tasks add `sqlite` (P6.3), `crypto` (P7) and `msauth` (P9).

pub mod system;
