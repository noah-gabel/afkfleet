//! The route registry (Plan.md P7.11, security rule 1, ADR-0016): the only
//! way a route gets into the router.
//!
//! [`Routes`] wraps utoipa-axum's `OpenApiRouter`, so every route keeps its
//! entry in the `OpenAPI` document, and offers exactly four ways in, one per
//! [`Access`]:
//! - [`Routes::public_route`]: a route anyone may call, such as the health
//!   checks and the login's steps;
//! - [`Routes::authed_route`]: a route whose handler takes `AuthUser`, so a
//!   request without a valid access token gets 401;
//! - [`Routes::step_up_route`]: an authed route that also needs the password
//!   entered again within 5 minutes; its handler takes `SteppedUpUser`, so
//!   a session without a fresh stamp gets 403 `step_up_required` (§7.2);
//! - [`Routes::setup_route`]: a route for enrolling a TOTP authenticator,
//!   whose handler takes `SetupToken`, so anything but a live setup token
//!   gets 401.
//!
//! Each takes the route's [`RateClass`], so no route can exist without one,
//! and adds its limit as a route layer (only a matched route spends it,
//! never a 404 or 405), and each records every method and path the route
//! serves as a [`RouteEntry`]. The route-coverage test reads the entries: it
//! calls every route that needs credentials without them and expects 401,
//! every step-up route with a live session but no stamp and expects 403,
//! and checks that the public routes are exactly the allowlist, in dev mode
//! and outside it.
//!
//! The registration also writes who may call each operation into the
//! document: the `access_token` or `setup_token` bearer scheme, and
//! `x-step-up: true` on a step-up route, from the [`Access`] alone, so the
//! document can't disagree with the router.
//!
//! [`Routes::finish`] adds the dev-only `GET /api/openapi.json` (public,
//! recorded, not in the document) and the middleware stack.

use core::fmt;
use std::sync::Arc;

use axum::Router;
use axum::http::Method;
use axum::middleware::from_fn_with_state;
use axum::routing::get;
use utoipa::openapi::extensions::Extensions;
use utoipa::openapi::path::{Operation, PathItem};
use utoipa::openapi::security::SecurityRequirement;
use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouter};

use super::middleware::{self, RequestIds};
use super::openapi::{self, ApiDocs, OPENAPI_PATH};
use super::rate_limit::{self, ClassLayer, RateLimits};
use super::router::AppState;
use crate::config::HttpConfig;

/// Who may call a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Access {
    /// Anyone: no credentials are needed.
    Public,
    /// Only a request with a valid access token: the handler takes
    /// `AuthUser`, so anything else gets 401.
    Authed,
    /// Only a request with a valid access token whose session stepped up
    /// within 5 minutes: the handler takes `SteppedUpUser`, so anything else
    /// gets 401, or 403 `step_up_required` without a fresh stamp.
    AuthedStepUp,
    /// Only a request with a live setup token: the handler takes
    /// `SetupToken`, so anything else gets 401.
    Setup,
}

impl Access {
    /// The bearer scheme the document names for it, if it needs one.
    #[must_use]
    pub const fn scheme(self) -> Option<&'static str> {
        match self {
            Self::Public => None,
            Self::Authed | Self::AuthedStepUp => Some(ACCESS_TOKEN_SCHEME),
            Self::Setup => Some(SETUP_TOKEN_SCHEME),
        }
    }
}

/// The document's name for the access token's bearer scheme.
pub const ACCESS_TOKEN_SCHEME: &str = "access_token";

/// The document's name for the setup token's bearer scheme.
pub const SETUP_TOKEN_SCHEME: &str = "setup_token";

/// The extension that marks a step-up route's operations in the document.
pub const STEP_UP_EXTENSION: &str = "x-step-up";

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
    limits: RateLimits,
}

/// The entries; utoipa-axum's router has no `Debug`.
impl fmt::Debug for Routes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Routes")
            .field("entries", &self.entries)
            .finish_non_exhaustive()
    }
}

impl Routes {
    /// No routes yet, over the `OpenAPI` document's fixed part; each route
    /// will spend its class's limit of `limits`.
    #[must_use]
    pub fn new(limits: &RateLimits) -> Self {
        Self {
            router: OpenApiRouter::with_openapi(openapi::base()),
            entries: Vec::new(),
            limits: limits.clone(),
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

    /// Adds `route`, whose handler takes `SteppedUpUser`, spending `class`.
    #[must_use]
    pub fn step_up_route(self, class: RateClass, route: UtoipaMethodRouter<AppState>) -> Self {
        self.register(Access::AuthedStepUp, class, route)
    }

    /// Adds `route`, whose handler takes `SetupToken`, spending `class`.
    #[must_use]
    pub fn setup_route(self, class: RateClass, route: UtoipaMethodRouter<AppState>) -> Self {
        self.register(Access::Setup, class, route)
    }

    /// The entries recorded so far.
    #[must_use]
    pub fn entries(&self) -> &[RouteEntry] {
        &self.entries
    }

    /// The router: every registered route with `state`, the `OpenAPI`
    /// document's route when `docs` says so, behind the middleware stack
    /// with the timeout and the body limit from `http` and request IDs from
    /// `ids`, and the per-IP limit of the limits [`Routes::new`] got. Also
    /// every entry, the document's route included.
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
            limits,
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
        (middleware::apply(routes, http, ids, &limits), entries)
    }

    /// Records every method and path of `route`, writes `access` into its
    /// operations in the document, and adds it, behind its class's limit.
    fn register(
        mut self,
        access: Access,
        class: RateClass,
        route: UtoipaMethodRouter<AppState>,
    ) -> Self {
        let (schemas, mut paths, method_router) = route;
        for item in paths.paths.values_mut() {
            for operation in operations(item) {
                document_access(operation, access);
            }
        }
        for (path, item) in &paths.paths {
            self.entries
                .extend(methods(item).into_iter().map(|method| RouteEntry {
                    method,
                    path: path.clone(),
                    access,
                    class,
                }));
        }
        let method_router = match class {
            RateClass::Standard => method_router,
            RateClass::Login | RateClass::Refresh | RateClass::Register => method_router
                .route_layer(from_fn_with_state(
                    ClassLayer::new(class, self.limits.clone()),
                    rate_limit::per_class,
                )),
        };
        self.router = self.router.routes((schemas, paths, method_router));
        self
    }
}

/// Every operation `item` has, for the methods [`methods`] lists.
fn operations(item: &mut PathItem) -> impl Iterator<Item = &mut Operation> {
    [
        &mut item.get,
        &mut item.put,
        &mut item.post,
        &mut item.delete,
        &mut item.options,
        &mut item.head,
        &mut item.patch,
        &mut item.trace,
    ]
    .into_iter()
    .filter_map(Option::as_mut)
}

/// Writes who may call `operation` into the document: its bearer scheme,
/// and the step-up mark.
fn document_access(operation: &mut Operation, access: Access) {
    operation.security = access
        .scheme()
        .map(|scheme| vec![SecurityRequirement::new(scheme, Vec::<String>::new())]);
    if access == Access::AuthedStepUp {
        operation
            .extensions
            .get_or_insert_with(Extensions::default)
            .merge(Extensions::from_iter([(STEP_UP_EXTENSION, true)]));
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
    fn each_access_names_its_scheme() {
        assert_eq!(Access::Public.scheme(), None);
        assert_eq!(Access::Authed.scheme(), Some("access_token"));
        assert_eq!(Access::AuthedStepUp.scheme(), Some("access_token"));
        assert_eq!(Access::Setup.scheme(), Some("setup_token"));
    }

    #[test]
    fn an_operation_is_marked_with_its_access() {
        for (access, scheme, step_up) in [
            (Access::Public, None, false),
            (Access::Authed, Some("access_token"), false),
            (Access::AuthedStepUp, Some("access_token"), true),
            (Access::Setup, Some("setup_token"), false),
        ] {
            let mut operation = Operation::new();

            document_access(&mut operation, access);

            let json = serde_json::to_value(&operation).unwrap();
            assert_eq!(
                json.get("security"),
                scheme
                    .map(|scheme| serde_json::json!([{ scheme: [] }]))
                    .as_ref(),
                "{access:?}"
            );
            assert_eq!(
                json.get("x-step-up"),
                step_up.then_some(&serde_json::json!(true)),
                "{access:?}"
            );
        }
    }

    #[test]
    fn a_path_with_one_method_has_just_that_one() {
        let item = PathItem::new(HttpMethod::Post, Operation::new());

        assert_eq!(methods(&item), [Method::POST]);
    }
}
