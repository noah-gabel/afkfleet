//! What the account tests share: the Microsoft port's fake (ADR-0018), and
//! the session-token service's setup with a database whose writes can fail
//! (P9.5).

pub(crate) mod msauth;
pub(crate) mod service;
pub(crate) mod store;
