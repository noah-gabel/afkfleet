//! What the account tests share: the Microsoft port's fake (ADR-0018), the
//! session-token service's setup with a database whose writes can fail
//! (P9.5), and the link flows' setup with the pacing fake (P9.4).

pub(crate) mod http;
pub(crate) mod link;
pub(crate) mod msauth;
pub(crate) mod pace;
pub(crate) mod service;
pub(crate) mod store;
