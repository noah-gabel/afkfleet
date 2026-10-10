//! [`HealthService`]: whether the server can serve requests (Plan.md P6.7,
//! ADR-0015).

use std::sync::Arc;

use crate::ports::store::{Store, StoreError};

/// The readiness check behind `GET /health/ready`.
#[derive(Debug, Clone)]
pub struct HealthService {
    store: Arc<dyn Store>,
}

impl HealthService {
    /// A service that checks `store`.
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }

    /// Whether the server is ready to serve requests: the database answers
    /// ([`Store::ping`]).
    ///
    /// # Errors
    /// The ping's [`StoreError`]: [`StoreError::Busy`] when no read
    /// connection is free in time, [`StoreError::Backend`] when the database
    /// fails.
    pub async fn ready(&self) -> Result<(), StoreError> {
        self.store.ping().await
    }
}
