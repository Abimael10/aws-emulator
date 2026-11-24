// SPDX-License-Identifier: CC0-1.0

//! @op CreateOAuth2Token
//! The anonymous OAuth 2.0 token endpoint: `POST /v1/token`, which a Sign-In CLI client
//! redeems an authorization code or refreshes a token at.
//!
//! The operation lists NO auth scheme — it is authenticated by the OAuth 2.0 client
//! credentials in its own body, not by SigV4 — so the request carries no credential scope
//! to resolve a service from and is claimed by its modelled HTTP route instead
//! (`crate::service`).
//!
//! ## Who judges the request
//!
//! The endpoint does, in its own wire vocabulary, before this handler runs:
//! [`crate::service::oauth2_token_endpoint`] establishes the OAuth 2.0 client, then the
//! grant type, then the parameters the named grant requires, and answers an RFC 6749 §5.2
//! error object for anything it cannot accept. A request that reaches here names a client
//! the endpoint knows, one of the two grants `GrantType` admits, and every parameter that
//! grant requires.
//!
//! ## What a call does
//!
//! `CreateOAuth2TokenResponseBody$accessToken` is a STRUCTURE of
//! `accessKeyId`/`secretAccessKey`/`sessionToken` and `tokenType` is `^aws_sigv4$`, so what
//! this endpoint vends is a scoped-down AWS SigV4 credential — minted by the emulator's own
//! credential store, so the caller can actually sign with it — and not a bearer token.
//!
//! * **Token refresh** (`grant_type=refresh_token`) is served from the store: the presented
//!   value has to name a refresh token this service holds that is still active, and the call
//!   vends the credentials that token carries, minting a set and recording it on the token
//!   when it carries none that is still live. The refresh token is reported back unchanged
//!   and is not consumed, which is what makes the flow "idempotent within token validity
//!   window".
//! * **Authorization-code redemption** (`grant_type=authorization_code`) redeems a code the
//!   Sign-In AUTHORIZATION endpoint issued. This service's model binds no such endpoint —
//!   `smithy_query(query="list_operations")` lists no operation on `/v1/authorize` — so no
//!   code this service issued can exist and every presented one is a code it cannot redeem,
//!   which the model's `AccessDeniedException` with `AUTHCODE_EXPIRED` is the refusal for.
//!   Nothing is minted for it: a code that was never issued cannot be exchanged for
//!   credentials.
//!
//! Both refusals below carry an `AccessDeniedException`, which declares no `@httpError` and
//! documents its status as "determined at runtime based on the error enum value" — HTTP 401
//! for `TOKEN_EXPIRED` and `AUTHCODE_EXPIRED`. The status of a rendered answer is the
//! renderer's rather than this handler's, so that mapping is applied where the answer
//! exists: [`crate::service::oauth2_token_endpoint`].
//!
//! ## Reaching this service's state
//!
//! The engine is held on `self` (constructor-injected), NOT passed per request — so
//! engine access is guaranteed by the time any request runs. Reach the store through
//! [`crate::state::service_state`] and read and write it through the guard it hands back.
//!
//! `ctx` ([`smithy_server_runtime::OperationContext`]) carries request metadata only —
//! headers, URI, method, and anything middleware placed in `ctx.parts().extensions` — plus
//! the emulator's credential store, which is what mints the credentials this endpoint vends.
//! The engine is NOT reached through `ctx`.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `CreateOAuth2Token`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-signin-signin/smithy-model/com-amazonaws-signin-signin.json")
//! ```
//!
//! CreateOAuth2Token API Path: /v1/token Request Method: POST Content-Type:
//! application/json or application/x-www-form-urlencoded This API implements OAuth 2.0
//! flows for AWS Sign-In CLI clients, supporting both: 1. Authorization code redemption
//! (grant_type=authorization_code) - NOT idempotent 2. Token refresh
//! (grant_type=refresh_token) - Idempotent within token validity window The operation
//! behavior is determined by the grant_type parameter in the request body: **Authorization
//! Code Flow (NOT Idempotent):** - JSON or form-encoded body with client_id,
//! grant_type=authorization_code, code, redirect_uri, code_verifier - Returns access_token,
//! token_type, expires_in, refresh_token, and id_token - Each authorization code can only
//! be used ONCE for security (prevents replay attacks) **Token Refresh Flow (Idempotent):**
//! - JSON or form-encoded body with client_id, grant_type=refresh_token, refresh_token -
//! Returns access_token, token_type, expires_in, and refresh_token (no id_token) - Multiple
//! calls with same refresh_token return consistent results within validity window
//! Authentication and authorization: - Confidential clients: sigv4 signing required with
//! signin:ExchangeToken permissions - CLI clients (public): authn/authz skipped based on
//! client_id & grant_type Note: This operation cannot be marked as @idempotent because it
//! handles both idempotent (token refresh) and non-idempotent (auth code redemption) flows
//! in a single endpoint.
//!
//! ## Input
//!
//! * `tokenInput` (CreateOAuth2TokenRequestBody) **required** — Flattened token operation
//!   inputs The specific operation is determined by grant_type in the request body
//!
//! ## Output
//!
//! * `tokenOutput` (CreateOAuth2TokenResponseBody) **required** — Flattened token operation
//!   outputs The specific response fields depend on the grant_type used in the request
//!
//! ## Errors
//!
//! * `AccessDeniedException` (HTTP 400) — Error thrown for access denied scenarios with
//!   flexible HTTP status mapping Runtime HTTP Status Code Mapping: - HTTP 401
//!   (Unauthorized): TOKEN_EXPIRED, AUTHCODE_EXPIRED - HTTP 403 (Forbidden):
//!   USER_CREDENTIALS_CHANGED, INSUFFICIENT_PERMISSIONS The specific HTTP status code is
//!   determined at runtime based on the error enum value. Consumers should use the error
//!   field to determine the specific access denial reason.
//! * `InternalServerException` (HTTP 500) — Error thrown when an internal server error occurs
//!   HTTP Status Code: 500 Internal Server Error Used for unexpected server-side errors that
//!   prevent request processing.
//! * `TooManyRequestsError` (HTTP 429) — Error thrown when rate limit is exceeded HTTP Status
//!   Code: 429 Too Many Requests Possible OAuth2ErrorCode values: - INVALID_REQUEST: Rate
//!   limiting, too many requests, abuse prevention Possible causes: - Too many token requests
//!   from the same client - Rate limiting based on client_id or IP address - Abuse prevention
//!   mechanisms triggered - Service protection against excessive token generation
//! * `ValidationException` (HTTP 400) — Error thrown when request validation fails HTTP
//!   Status Code: 400 Bad Request Used for request validation errors such as malformed
//!   parameters, missing required fields, or invalid parameter values.

#[allow(unused_imports)]
use super::types::*;

use emulator_server::aws_credentials::{AWS_SESSION, SECRET_ACCESS_KEY, SESSION_TOKEN};
use std::time::{Duration, SystemTime};

use crate::state::ScopedCredentials;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `CreateOAuth2TokenError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-signin-signin.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `CreateOAuth2TokenError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.signin#CreateOAuth2Token")]
#[derive(Debug)]
pub enum CreateOAuth2TokenErrorModeled {
    AccessDeniedException(AccessDeniedException),
    InternalServerException(InternalServerException),
    TooManyRequestsError(TooManyRequestsError),
    ValidationException(ValidationException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::create_o_auth2_token::CreateOAuth2TokenError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type CreateOAuth2TokenError = crate::scaffolded::Scaffolded<CreateOAuth2TokenErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<CreateOAuth2TokenErrorModeled> for CreateOAuth2TokenError {
    fn from(modeled: CreateOAuth2TokenErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `CreateOAuth2Token` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct CreateOAuth2Token<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> CreateOAuth2Token<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for CreateOAuth2Token<Eng>
{
    type Input = CreateOAuth2TokenInput;
    type Output = CreateOAuth2TokenOutput;
    type Error = CreateOAuth2TokenError;

    const NAME: &'static str = "CreateOAuth2Token";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::CREATE_O_AUTH2_TOKEN_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::CREATE_O_AUTH2_TOKEN_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(server_error("the signin OAuth 2.0 token store is unavailable"));
        };
        let requested = input.token_input;
        let now = SystemTime::now();

        if requested.grant_type == crate::state::GRANT_TYPE_AUTHORIZATION_CODE {
            // A code this service never issued (see the module docs: it binds no
            // authorization endpoint to issue one) is a code it cannot redeem, and
            // nothing is minted for it.
            return Err(CreateOAuth2TokenErrorModeled::AccessDeniedException(
                AccessDeniedException {
                    error: crate::state::AUTHCODE_EXPIRED_ERROR_CODE.to_string(),
                    message: crate::state::AUTHCODE_EXPIRED_MESSAGE.to_string(),
                },
            )
            .into());
        }

        // The other grant the endpoint admits: a refresh, served from the store.
        let presented = requested.refresh_token.unwrap_or_default();
        // The emulator mints every id and secret — a handler never invents one — so the
        // candidate is drawn first and handed to the store, which decides whether this
        // refresh token is vended it or keeps the set it already holds.
        let minted = vended_credentials(ctx, now)
            .ok_or_else(|| server_error("a session credential was created without its secrets"))?;
        let vended = state.lock().refreshed_credentials(
            &presented,
            ::engine::emulator_account_id(),
            now,
            minted.clone(),
        );
        let Some((credentials, spent)) = vended else {
            // Nothing was vended: the presented value names no refresh token of this
            // caller that is still usable, so the candidate is released again.
            let _ = ctx.credential_store().delete(&minted.access_key_id);
            return Err(CreateOAuth2TokenErrorModeled::AccessDeniedException(
                AccessDeniedException {
                    error: crate::state::TOKEN_EXPIRED_ERROR_CODE.to_string(),
                    message: crate::state::TOKEN_EXPIRED_MESSAGE.to_string(),
                },
            )
            .into());
        };
        // The credential the store did not keep — the candidate it declined, or the set
        // this one replaced — is released, so one refresh token holds one credential
        // however often it is presented.
        if let Some(spent) = spent {
            let _ = ctx.credential_store().delete(&spent);
        }

        Ok(CreateOAuth2TokenOutput {
            token_output: CreateOAuth2TokenResponseBody {
                // Reported off what the store vended, so what this call hands back is the
                // credential the refresh token now carries.
                access_token: AccessToken {
                    access_key_id: credentials.access_key_id.clone(),
                    secret_access_key: credentials.secret_access_key.clone(),
                    session_token: credentials.session_token.clone(),
                },
                token_type: crate::state::SIGV4_TOKEN_TYPE.to_string(),
                expires_in: credentials.expires_in(now),
                // The refresh token is not consumed, so the caller keeps presenting the
                // one it has: `refreshToken` is `@required` on both flows' responses.
                refresh_token: presented,
                // "Not included in token refresh responses"
                // (`CreateOAuth2TokenResponseBody$idToken`).
                id_token: None,
            },
        })
    }
}

/// The credentials this call offers the store: a session credential the EMULATOR minted,
/// good for [`crate::state::SCOPED_CREDENTIALS_EXPIRES_IN`] seconds from `now`.
///
/// `None` when the store minted a session without both of its secrets, which is an
/// emulator-side fault and nothing a caller can fix; the half-minted credential is released
/// rather than left behind.
fn vended_credentials(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    now: SystemTime,
) -> Option<ScopedCredentials> {
    let minted = ctx.credential_store().create(&AWS_SESSION);
    let secrets = (
        minted.secret(SECRET_ACCESS_KEY).map(str::to_string),
        minted.secret(SESSION_TOKEN).map(str::to_string),
    );
    let (Some(secret_access_key), Some(session_token)) = secrets else {
        let _ = ctx.credential_store().delete(&minted.id);
        return None;
    };
    Some(ScopedCredentials {
        access_key_id: minted.id.clone(),
        secret_access_key,
        session_token,
        expires_at: now + Duration::from_secs(crate::state::SCOPED_CREDENTIALS_EXPIRES_IN),
    })
}

/// The modelled server fault this operation answers when the emulator cannot serve the
/// request for a reason the caller has no part in.
fn server_error(message: &str) -> CreateOAuth2TokenError {
    CreateOAuth2TokenErrorModeled::InternalServerException(InternalServerException {
        error: crate::state::SERVER_ERROR_CODE.to_string(),
        message: message.to_string(),
    })
    .into()
}
