//! The `OpenAPI` schemas of the API's types (Plan.md P6.10, ADR-0015).
//!
//! The server's dev-only `/api/openapi.json` describes the same wire types
//! as the TypeScript export, and a test renders every schema as TypeScript
//! and compares it with ts-rs's output, so the two can't drift:
//! - [`SafeInt`](crate::SafeInt) is a component of its own, an integer
//!   within ±(2^53 − 1), as `SafeInt` is a type of its own in TypeScript.
//! - [`BoundedText<MAX>`](crate::BoundedText) is a string of at most `MAX`
//!   characters, written inline: a field holding one carries
//!   `#[schema(schema_with = BoundedText::<MAX>::schema)]`.
//! - An [`Open<E>`](crate::Open) field carries `#[schema(value_type = E)]`
//!   next to its `#[ts(as = "E")]`, so the schema lists exactly the values
//!   this version knows.
//! - An optional field (`#[ts(optional)]`) carries `#[schema(nullable =
//!   false)]`: it's left out when empty, never sent as `null`.
//!
//! [`components`] lists the root DTOs, like the TypeScript export: the
//! server's document holds exactly these schemas.

use std::collections::BTreeMap;

use utoipa::ToSchema;
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::Schema;

use crate::ErrorResponse;
use crate::accounts::{LinkFlowResponse, LinkStartedResponse};
use crate::auth::{
    LoginMfaRequest, LoginRequest, LoginResponse, RefreshRequest, RegisterRequest,
    RegisterResponse, StepUpRequest, TokenResponse,
};
use crate::invites::{CreateInviteRequest, CreatedInviteResponse, InvitePage};
use crate::me::{MeResponse, PasswordChangeRequest};
use crate::mfa::{MfaConfirmRequest, MfaConfirmResponse, MfaEnrollResponse, MfaResetResponse};
use crate::sessions::SessionPage;
use crate::users::{UserPage, UserPatchRequest, UserResponse};

/// Every named schema of the API, by name: the root DTOs and every schema
/// they refer to.
#[must_use]
pub fn components() -> BTreeMap<String, RefOr<Schema>> {
    let mut schemas = Vec::new();
    // The root DTOs, as in `typescript::export`: each adds every schema it
    // refers to. A DTO no listed type uses goes here.
    add::<ErrorResponse>(&mut schemas);
    add::<MfaEnrollResponse>(&mut schemas);
    add::<MfaConfirmRequest>(&mut schemas);
    add::<MfaConfirmResponse>(&mut schemas);
    add::<MfaResetResponse>(&mut schemas);
    add::<LoginRequest>(&mut schemas);
    add::<LoginResponse>(&mut schemas);
    add::<LoginMfaRequest>(&mut schemas);
    add::<TokenResponse>(&mut schemas);
    add::<RefreshRequest>(&mut schemas);
    add::<StepUpRequest>(&mut schemas);
    add::<UserPage>(&mut schemas);
    add::<UserResponse>(&mut schemas);
    add::<UserPatchRequest>(&mut schemas);
    add::<InvitePage>(&mut schemas);
    add::<CreateInviteRequest>(&mut schemas);
    add::<CreatedInviteResponse>(&mut schemas);
    add::<RegisterRequest>(&mut schemas);
    add::<RegisterResponse>(&mut schemas);
    add::<MeResponse>(&mut schemas);
    add::<PasswordChangeRequest>(&mut schemas);
    add::<SessionPage>(&mut schemas);
    add::<LinkStartedResponse>(&mut schemas);
    add::<LinkFlowResponse>(&mut schemas);
    schemas.into_iter().collect()
}

/// Adds `T`'s schema and every schema it refers to.
fn add<T: ToSchema>(schemas: &mut Vec<(String, RefOr<Schema>)>) {
    T::schemas(schemas);
    schemas.push((T::name().into_owned(), T::schema()));
}
