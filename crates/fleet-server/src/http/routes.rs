//! The route registry (Plan.md P7.11, security rule 1, ADR-0016): the only
//! way a route gets into the router.
//!
//! [`Routes`] wraps utoipa-axum's `OpenApiRouter`, so every route keeps its
//! entry in the `OpenAPI` document, and offers exactly two ways in:
//! - [`Routes::public_route`]: a route anyone may call, such as the health
//!   checks;
//! - [`Routes::authed_route`]: a route whose handler takes `AuthUser`, so a
//!   request without valid credentials gets 401.
//!
//! Both take the route's [`RateClass`], so no route can exist without one,
//! and both record every method and path the route serves as a
//! [`RouteEntry`]. The route-coverage test reads the entries: it calls every
//! authed route without credentials and expects 401, and checks that the
//! public routes are exactly the allowlist, in dev mode and outside it.
//!
//! [`Routes::finish`] adds the dev-only `GET /api/openapi.json` (public,
//! recorded, not in the document) and the middleware stack.

use core::fmt;
use std::sync::Arc;

use axum::Router;
use axum::http::Method;
use axum::routing::get;
use utoipa::openapi::path::PathItem;
use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouter};

use super::middleware::{self, RequestIds};
use super::openapi::{self, ApiDocs, OPENAPI_PATH};
use super::router::AppState;
use crate::config::HttpConfig;

/// Who may call a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Access {
    /// Anyone: no credentials are needed.
    Public,
    /// Only a request with valid credentials: the handler takes `AuthUser`,
    /// so anything else gets 401.
    Authed,
}

/// Which of §7.4's request limits a route spends, besides the global
/// per-IP limit every request spends (Plan.md P7.10). Every authed route
/// also spends its user's limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RateClass {
    /// Only the global per-IP limit.
    Standard,
    /// `POST /auth/login` and `/auth/login/mfa`, which share one per-IP
    /// bucket: 5 a minute by default.
    Login,
    /// `POST /auth/refresh`: 30 a minute per IP by default.
    Refresh,
    /// `POST /auth/register`: 5 an hour per IP by default.
    Register,
}

/// One method and path the router serves, with who may call it and the
/// limit it spends.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RouteEntry {
    /// The method.
    pub method: Method,
    /// The path template, as the router matches it (`/api/v1/bots/{id}`).
    pub path: String,
    /// Who may call it.
    pub access: Access,
    /// The limit it spends.
    pub class: RateClass,
}

/// The routes being registered, and their entries; see the
/// [module docs](self).
pub struct Routes {
    router: OpenApiRouter<AppState>,
    entries: Vec<RouteEntry>,
}

/// The entries; utoipa-axum's router has no `Debug`.
impl fmt::Debug for Routes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Routes")
            .field("entries", &self.entries)
            .finish_non_exhaustive()
    }
}

impl Default for Routes {
    fn default() -> Self {
        Self::new()
    }
}

impl Routes {
    /// No routes yet, over the `OpenAPI` document's fixed part.
    #[must_use]
    pub fn new() -> Self {
        Self {
            router: OpenApiRouter::with_openapi(openapi::base()),
            entries: Vec::new(),
        }
    }

    /// Adds `route`, which anyone may call, spending `class`.
    #[must_use]
    pub fn public_route(self, class: RateClass, route: UtoipaMethodRouter<AppState>) -> Self {
        self.register(Access::Public, class, route)
    }

    /// Adds `route`, whose handler takes `AuthUser`, spending `class`.
    #[must_use]
    pub fn authed_route(self, class: RateClass, route: UtoipaMethodRouter<AppState>) -> Self {
        self.register(Access::Authed, class, route)
    }

    /// The entries recorded so far.
    #[must_use]
    pub fn entries(&self) -> &[RouteEntry] {
        &self.entries
    }

    /// The router: every registered route with `state`, the `OpenAPI`
    /// document's route when `docs` says so, behind the middleware stack
    /// with the timeout and the body limit from `http` and request IDs from
    /// `ids`. Also every entry, the document's route included.
    pub fn finish(
        self,
        state: AppState,
        http: &HttpConfig,
        ids: RequestIds,
        docs: ApiDocs,
    ) -> (Router, Vec<RouteEntry>) {
        let Self {
            router,
            mut entries,
        } = self;
        let (routes, document) = router.with_state(state).split_for_parts();
        let routes = match docs {
            ApiDocs::Served => {
                entries.push(RouteEntry {
                    method: Method::GET,
                    path: OPENAPI_PATH.to_owned(),
                    access: Access::Public,
                    class: RateClass::Standard,
                });
                let document = Arc::new(document);
                routes.route(
                    OPENAPI_PATH,
                    get(move || core::future::ready(openapi::document(&document))),
                )
            }
            ApiDocs::Hidden => routes,
        };
        (middleware::apply(routes, http, ids), entries)
    }

    /// Records every method and path of `route` and adds it.
    fn register(
        mut self,
        access: Access,
        class: RateClass,
        route: UtoipaMethodRouter<AppState>,
    ) -> Self {
        let (_, paths, _) = &route;
        for (path, item) in &paths.paths {
            self.entries
                .extend(methods(item).into_iter().map(|method| RouteEntry {
                    method,
                    path: path.clone(),
                    access,
                    class,
                }));
        }
        self.router = self.router.routes(route);
        self
    }
}

/// The methods `item` has an operation for, in a fixed order. utoipa's
/// `query` and additional operations aren't methods axum routes; a route
/// that had one would differ from the document, which the registry's test
/// compares.
fn methods(item: &PathItem) -> Vec<Method> {
    [
        (Method::GET, &item.get),
        (Method::PUT, &item.put),
        (Method::POST, &item.post),
        (Method::DELETE, &item.delete),
        (Method::OPTIONS, &item.options),
        (Method::HEAD, &item.head),
        (Method::PATCH, &item.patch),
        (Method::TRACE, &item.trace),
    ]
    .into_iter()
    .filter_map(|(method, operation)| operation.is_some().then_some(method))
    .collect()
}

#[cfg(test)]
mod tests {
    use utoipa::openapi::HttpMethod;
    use utoipa::openapi::path::Operation;

    use super::*;

    #[test]
    fn every_method_of_a_path_is_found_in_a_fixed_order() {
        let item = PathItem::from_http_methods(
            [
                HttpMethod::Trace,
                HttpMethod::Patch,
                HttpMethod::Head,
                HttpMethod::Options,
                HttpMethod::Delete,
                HttpMethod::Post,
                HttpMethod::Put,
                HttpMethod::Get,
            ],
            Operation::new(),
        );

        assert_eq!(
            methods(&item),
            [
                Method::GET,
                Method::PUT,
                Method::POST,
                Method::DELETE,
                Method::OPTIONS,
                Method::HEAD,
                Method::PATCH,
                Method::TRACE,
            ]
        );
    }

    #[test]
    fn a_path_with_one_method_has_just_that_one() {
        let item = PathItem::new(HttpMethod::Post, Operation::new());

        assert_eq!(methods(&item), [Method::POST]);
    }
}
