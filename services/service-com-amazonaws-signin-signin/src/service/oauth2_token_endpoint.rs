// SPDX-License-Identifier: CC0-1.0

//! The OAuth 2.0 wire contract of this service's token endpoints: how `/v1/token` reads the
//! parameters a request carries, and the RFC 6749 §5.2 error object it refuses one with.
//!
//! # Why this is a seam and not a line in each handler
//!
//! The two things the recorded wire of these endpoints requires cannot be said from a
//! handler at all:
//!
//! * the refusal body carries `error_description` beside `error` and `message`, and no error
//!   shape of this model declares such a member, so a handler has nothing to put the
//!   sentence in; and
//! * the parameters the endpoint names on the wire are not all modelled members of the
//!   operation it serves (`client_id` is a parameter of every `/v1/token` request, while
//!   `CreateOAuth2TokenWithIAMRequest` declares only `grantType` and `resource`), so which
//!   parameter a request failed to supply cannot be read off a deserialized input.
//!
//! A wrapper is handed the whole request before anything is deserialized, so it can do both:
//! it reads the request's parameters in the endpoint's own wire vocabulary, and answers the
//! OAuth 2.0 error object itself. A request it does not refuse is handed to the generated
//! `ErasedOperation::run`, which owns deserialize -> validate -> handle -> serialize and
//! every modelled answer, so nothing about a request this endpoint accepts changes.
//!
//! # What the endpoint refuses, in order
//!
//! Both endpoints answer HTTP 400 with the error shape the model declares for a rejected
//! request (`ValidationException`), and both state the refusal as an OAuth 2.0 error object:
//! the `error` code a client branches on, the `error_description` sentence for that class,
//! and the same sentence as `message`. A request passes through three layers, and which one
//! refused it decides both the wording and the media type:
//!
//! 1. **The client.** On [`Endpoint::ClientAuthenticated`] — `POST /v1/token`, the anonymous
//!    endpoint — the OAuth 2.0 client credentials in the BODY are what authenticates the
//!    caller, so the client is established first: `client_id` has to name a client this
//!    endpoint knows (`ClientId` is
//!    `^arn:aws:signin:::devtools/(cross-device|same-device)$`, the public CLI clients the
//!    operation documents as needing no signature), and a request that names none is
//!    [`CLIENT_AUTHENTICATION_FAILED`]. On [`Endpoint::IamSigned`] —
//!    `POST /v1/token?x-amz-client-auth-method=iam` — the caller is authenticated by its
//!    SigV4 signature, so there is no client to establish from the body and this layer
//!    judges nothing.
//! 2. **The grant.** Which parameters a request requires is a function of its grant, so the
//!    grant type is read next, and a request that supplies NO grant type leaves the
//!    authorization server with no grant to act on at all: that is the grant-type class on
//!    BOTH forms, [`UNSUPPORTED_GRANT_TYPE`] with RFC 6749 §5.2's own description of it. A
//!    grant type the body does supply is judged against what its member admits. On the
//!    ANONYMOUS endpoint that member admits a CHOICE of grants (`GrantType` is
//!    `^(authorization_code|refresh_token)$`), so a value outside the choice is one more
//!    grant the server does not implement and takes the same class. On the IAM form the model
//!    admits exactly ONE grant (`ClientCredentialsGrantType` is `^client_credentials$`,
//!    `@required`), so there is no choice of grants there: a supplied value away from it is an
//!    invalid parameter VALUE, which is the parameter layer's `invalid_request`
//!    ([`malformed_request`]), reported the way every other modelled-value refusal of that
//!    form is.
//! 3. **The parameters** of the grant it named. A parameter EVERY request of that form has
//!    to carry and the body supplies no value for is reported by name,
//!    [`MISSING_PARAMETER`] plus its OAuth 2.0 name. One the model requires only for the
//!    grant the request named ("Required only when grant_type=authorization_code") is
//!    reported without naming it, as RFC 6749 §5.2's own description of `invalid_request` —
//!    and so is a value outside what its parameter admits.
//!
//! `client_id` is one of the unconditional parameters of the IAM form even though its
//! request shape models no member for it, and that is not an accident of this
//! implementation: `client_id` is how EVERY `/v1/token` request names the OAuth 2.0 client
//! it speaks for, and the IAM form is the one where that client is established from the
//! SigV4 signature instead of from the body
//! ([`crate::state::IAM_CLIENT_CREDENTIALS_CLIENT_ID`] is the client it names). So a body
//! here carries no value for it, and an incomplete client-credentials request is reported
//! against the first of the grant's parameters the BODY itself supplies none for — which is
//! `client_id` for the shape an SDK sends and `resource` for a body that does name a client
//! (both covered by the rows of `oauth2_token_acceptance.rs`).
//!
//! The first two layers render `application/json`; the parameter layer renders the same
//! JSON document in the charset-qualified `application/json;charset=UTF-8` it states.
//!
//! # The status an operation's own refusal is answered at
//!
//! One more thing this layer owns, for the same reason the error document is: the status.
//! `AccessDeniedException` declares no `@httpError` and documents its own mapping — HTTP
//! 401 for `TOKEN_EXPIRED` and `AUTHCODE_EXPIRED`, HTTP 403 for
//! `USER_CREDENTIALS_CHANGED` and `INSUFFICIENT_PERMISSIONS`, "determined at runtime based
//! on the error enum value" — which a handler cannot state, because the status of the
//! answer it returns is the renderer's. [`with_documented_status`] states it, over the
//! answers of the two forms this endpoint serves.
//!
//! # The body this endpoint reads
//!
//! The JSON payload the model binds, and only that. `CreateOAuth2Token` documents a
//! "Content-Type: application/json or application/x-www-form-urlencoded" wire, but the
//! operations behind this endpoint are served by the model's `restJson1` codec, which reads
//! a JSON body alone — a form-encoded body would reach no handler whatever this layer
//! decided about it. So a body that is not a readable JSON object carries no parameter here,
//! and is refused by the first layer that needs one, rather than being admitted and then
//! failing to deserialize.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use aws_smithy_types::body::SdkBody;
use serde_json::Value;
use smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

/// The HTTP status every refusal below is answered under: `ValidationException` is modelled
/// `@httpError(400)`.
const REFUSED_STATUS: u16 = 400;

/// The error shape a refused token request is answered as, named in `x-amzn-errortype` the
/// way `restJson1` names the shape of a failure.
const REFUSED_SHAPE: &str = "ValidationException";

/// The media type an OAuth 2.0 error object is rendered in.
const JSON: &str = "application/json";

/// The media type the endpoint's own PARAMETER validation renders its refusal in: the same
/// JSON document, with the charset the layer that writes it states.
const JSON_WITH_CHARSET: &str = "application/json;charset=UTF-8";

/// The OAuth 2.0 error code of a grant type the endpoint does not implement.
///
/// RFC 6749 §5.2's own code for that class. `OAuth2ErrorCode` — the enum
/// `ValidationException$error` targets — does not declare it, so the recorded wire of these
/// endpoints is the authority for it: AWS answers the RFC's code here rather than one of the
/// enum's members.
pub(crate) const UNSUPPORTED_GRANT_TYPE: &str = "unsupported_grant_type";

/// What the endpoint states about a grant type it does not implement. RFC 6749 §5.2's own
/// description of `unsupported_grant_type`; wholly fixed text, naming nothing from the
/// request.
pub(crate) const UNSUPPORTED_GRANT_TYPE_DESCRIPTION: &str =
    "The authorization grant type is not supported by the authorization server.";

/// What the anonymous endpoint states about a request whose OAuth 2.0 client it could not
/// establish. Wholly fixed text: it names neither the client the request offered nor which
/// half of the client credentials was wrong.
pub(crate) const CLIENT_AUTHENTICATION_FAILED: &str = "Client authentication failed.";

/// The fixed half of what the endpoint states about a parameter the request supplied no
/// value for; the wire name of that parameter follows it.
pub(crate) const MISSING_PARAMETER: &str = "Missing required parameter: ";

/// The error shape whose HTTP status this service determines from the error CODE it
/// carries, rather than from a status bound to the shape.
///
/// `AccessDeniedException` declares no `@httpError` at all, and documents the mapping
/// itself: "Runtime HTTP Status Code Mapping: - HTTP 401 (Unauthorized): TOKEN_EXPIRED,
/// AUTHCODE_EXPIRED - HTTP 403 (Forbidden): USER_CREDENTIALS_CHANGED,
/// INSUFFICIENT_PERMISSIONS. The specific HTTP status code is determined at runtime based
/// on the error enum value." So the status is a function of the `error` member, which is
/// what [`documented_access_denied_status`] states.
const ACCESS_DENIED_SHAPE: &str = "AccessDeniedException";

/// The status that documentation gives `TOKEN_EXPIRED` and `AUTHCODE_EXPIRED`: the
/// credential the request presented is no longer one this service will act on, so the
/// caller has to obtain another.
const UNAUTHORIZED_STATUS: u16 = 401;

/// The status it gives `USER_CREDENTIALS_CHANGED` and `INSUFFICIENT_PERMISSIONS`: the
/// caller was identified and is not allowed the action.
const FORBIDDEN_STATUS: u16 = 403;

/// The HTTP status [`ACCESS_DENIED_SHAPE`]'s documented mapping gives `error_code`, or
/// `None` for a code it does not place.
fn documented_access_denied_status(error_code: &str) -> Option<u16> {
    match error_code {
        crate::state::TOKEN_EXPIRED_ERROR_CODE | crate::state::AUTHCODE_EXPIRED_ERROR_CODE => {
            Some(UNAUTHORIZED_STATUS)
        }
        crate::state::USER_CREDENTIALS_CHANGED_ERROR_CODE
        | crate::state::INSUFFICIENT_PERMISSIONS_ERROR_CODE => Some(FORBIDDEN_STATUS),
        _ => None,
    }
}

/// `resp` at the status the error code it carries is documented to be answered at, for an
/// [`ACCESS_DENIED_SHAPE`] this endpoint's operation rendered.
///
/// Every header and every body byte the renderer wrote is kept: only the status line
/// changes. A success, another error shape, and a code the mapping does not place are
/// returned as they were — as is a response already at that status.
///
/// Applied here, to the answers of the two `/v1/token` forms, because that is the surface
/// this endpoint owns; the other operations of this service answer their own refusals, and
/// what status each is owed is theirs to state from their own recorded wire.
fn with_documented_status(resp: RtResponse) -> RtResponse {
    let shape = resp
        .headers()
        .get(::service_common::error_type_locator::ERROR_TYPE_HEADER)
        .map(::service_common::error_type_locator::header_shape_named);
    if shape != Some(ACCESS_DENIED_SHAPE) {
        return resp;
    }
    // An error body is built in memory by its renderer, so the bytes are readable here; a
    // streamed body is not an error body and keeps the status it was framed with.
    let Some(bytes) = resp.body().bytes() else {
        return resp;
    };
    let code = ::serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|document| {
            document
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let Some(status) = code.as_deref().and_then(documented_access_denied_status) else {
        return resp;
    };
    if resp.status().as_u16() == status {
        return resp;
    }
    let Ok(restated_status) = StatusCode::try_from(status) else {
        return resp;
    };
    let body = bytes.to_vec();
    let mut restated = RtResponse::new(restated_status, SdkBody::from(body));
    for (name, value) in resp.headers().iter() {
        let _ = restated
            .headers_mut()
            .try_append(name.to_string(), value.to_string());
    }
    restated
}

/// One OAuth 2.0 parameter of a token request: the name the endpoint REPORTS it by, and
/// the name the request BINDS it under.
///
/// The two differ, and both come from the service's own statement of them. `reported` is
/// the OAuth 2.0 parameter name — the vocabulary the endpoint documents its wire in ("JSON
/// or form-encoded body with client_id, grant_type=authorization_code, code, redirect_uri,
/// code_verifier") and the one a refusal names, since that is the name a client reads in
/// the OAuth 2.0 specification. `bound` is the `jsonName` the model binds for the member
/// that carries it, which is the key a JSON body has to use for the operation to receive
/// the value at all — so it is the name a parameter is READ under here, and the endpoint
/// cannot accept a parameter the operation behind it would not receive.
struct Parameter {
    reported: &'static str,
    bound: &'static str,
}

/// The OAuth 2.0 client the request speaks for: `CreateOAuth2TokenRequestBody$clientId`.
const CLIENT_ID: Parameter = Parameter {
    reported: "client_id",
    bound: "clientId",
};

/// The grant the request asks the ANONYMOUS endpoint to process:
/// `CreateOAuth2TokenRequestBody$grantType`, whose `jsonName` is the member name.
const ANONYMOUS_GRANT_TYPE: Parameter = Parameter {
    reported: "grant_type",
    bound: "grantType",
};

/// The grant the request asks the IAM form to process:
/// `CreateOAuth2TokenWithIAMRequest$grantType`, whose `jsonName` is already the OAuth 2.0
/// name — the two request shapes bind the same parameter under different keys, so each
/// endpoint reads its own.
const IAM_GRANT_TYPE: Parameter = Parameter {
    reported: "grant_type",
    bound: "grant_type",
};

/// The resource an access token is scoped to: `CreateOAuth2TokenWithIAMRequest$resource`.
const RESOURCE: Parameter = Parameter {
    reported: "resource",
    bound: "resource",
};

/// The authorization code being redeemed: `CreateOAuth2TokenRequestBody$code`, "Required
/// only when grant_type=authorization_code".
const CODE: Parameter = Parameter {
    reported: "code",
    bound: "code",
};

/// The redirect URI the authorization request was made with:
/// `CreateOAuth2TokenRequestBody$redirectUri`, required for the same grant.
const REDIRECT_URI: Parameter = Parameter {
    reported: "redirect_uri",
    bound: "redirectUri",
};

/// The PKCE verifier proving possession of the original challenge:
/// `CreateOAuth2TokenRequestBody$codeVerifier`, required for the same grant.
const CODE_VERIFIER: Parameter = Parameter {
    reported: "code_verifier",
    bound: "codeVerifier",
};

/// The refresh token being presented: `CreateOAuth2TokenRequestBody$refreshToken`,
/// "Required only when grant_type=refresh_token".
const REFRESH_TOKEN: Parameter = Parameter {
    reported: "refresh_token",
    bound: "refreshToken",
};

/// The parameters the authorization-code grant requires, in the order the endpoint names
/// them: `CreateOAuth2TokenRequestBody`'s own documentation of that flow, each "Required
/// only when grant_type=authorization_code".
const AUTHORIZATION_CODE_PARAMETERS: &[Parameter] = &[CODE, REDIRECT_URI, CODE_VERIFIER];

/// The one parameter the refresh-token grant requires, "Required only when
/// grant_type=refresh_token".
const REFRESH_TOKEN_PARAMETERS: &[Parameter] = &[REFRESH_TOKEN];

/// The parameters EVERY client-credentials request of the IAM form carries, in the order the
/// endpoint names them — the only grant that form serves, so both are unconditional and an
/// absent one is reported BY NAME. `client_id` comes first because it is what every
/// `/v1/token` request names its client by; this form carries the client in the signature
/// instead, so a body here supplies no value for it and an incomplete request is reported
/// against it.
const CLIENT_CREDENTIALS_PARAMETERS: &[Parameter] = &[CLIENT_ID, RESOURCE];

/// Which `/v1/token` form an operation serves — how the caller is authenticated, which
/// decides what the endpoint judges first.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Endpoint {
    /// `POST /v1/token`: the anonymous form, authenticated by the OAuth 2.0 client
    /// credentials the body carries.
    ClientAuthenticated,
    /// `POST /v1/token?x-amz-client-auth-method=iam`: authenticated by the caller's SigV4
    /// signature.
    IamSigned,
}

/// One OAuth 2.0 error object: the class a client branches on, the sentence the endpoint
/// states for it, and the media type that sentence is rendered in.
#[derive(Debug)]
pub(crate) struct OAuth2Error {
    /// The `error` member: the OAuth 2.0 error code.
    code: &'static str,
    /// The `error_description` member, and the `message` member — one sentence, reported
    /// under both names, which is what the recorded wire of these endpoints carries.
    description: String,
    /// The media type this class of refusal is rendered in.
    media_type: &'static str,
}

impl OAuth2Error {
    /// The refusal as the endpoint answers it: HTTP 400, the error shape in
    /// `x-amzn-errortype`, and the OAuth 2.0 error object as the body.
    ///
    /// Built here rather than through the framework's refusal renderer because that renderer
    /// serializes the model's own error shape, which declares no `error_description` member
    /// and so cannot carry the sentence under the name a client of an OAuth 2.0 endpoint
    /// reads it from.
    pub(crate) fn response(&self) -> RtResponse {
        let body = ::serde_json::json!({
            // The shape of a `restJson1` failure travels in `x-amzn-errortype`; it is named
            // in the body as well because that is where every other failure of this service
            // names it, so a client that resolves the shape either way reads the same one.
            "__type": REFUSED_SHAPE,
            "error": self.code,
            "error_description": self.description,
            "message": self.description,
        });
        let status =
            StatusCode::try_from(REFUSED_STATUS).expect("the refusal's status is a constant");
        let mut response = RtResponse::new(status, SdkBody::from(body.to_string()));
        let headers = response.headers_mut();
        headers.insert("content-type", self.media_type);
        headers.insert("x-amzn-errortype", REFUSED_SHAPE);
        response
    }
}

/// The refusal of a grant type this endpoint does not implement.
fn unsupported_grant_type() -> OAuth2Error {
    OAuth2Error {
        code: UNSUPPORTED_GRANT_TYPE,
        description: UNSUPPORTED_GRANT_TYPE_DESCRIPTION.to_string(),
        media_type: JSON,
    }
}

/// The refusal of a request whose OAuth 2.0 client could not be established.
fn client_authentication_failed() -> OAuth2Error {
    OAuth2Error {
        code: crate::state::VALIDATION_ERROR_CODE,
        description: CLIENT_AUTHENTICATION_FAILED.to_string(),
        media_type: JSON,
    }
}

/// The refusal of a request that supplied no value for `name`, a parameter EVERY request
/// of this form of the endpoint has to carry.
fn missing_parameter(name: &str) -> OAuth2Error {
    OAuth2Error {
        code: crate::state::VALIDATION_ERROR_CODE,
        description: format!("{MISSING_PARAMETER}{name}"),
        media_type: JSON_WITH_CHARSET,
    }
}

/// The refusal of a request the endpoint's parameter validation cannot accept without
/// naming one parameter: a parameter the model requires only FOR THE GRANT the request
/// named ("Required only when grant_type=authorization_code") that the request does not
/// supply, or a value outside what the parameter admits.
///
/// RFC 6749 §5.2's own description of `invalid_request` covers both — "missing a required
/// parameter, includes an invalid parameter value, or is otherwise malformed" — which is
/// also what the model documents for `OAuth2ErrorCode`'s `INVALID_REQUEST`.
fn malformed_request() -> OAuth2Error {
    OAuth2Error {
        code: crate::state::VALIDATION_ERROR_CODE,
        description: crate::state::INVALID_REQUEST_MESSAGE.to_string(),
        media_type: JSON_WITH_CHARSET,
    }
}

/// One modelled token operation, served behind its endpoint's OAuth 2.0 wire contract.
pub(crate) struct OAuth2TokenEndpoint<O> {
    operation: O,
    endpoint: Endpoint,
}

impl<O> OAuth2TokenEndpoint<O> {
    /// Wrap one operation, already constructed from the shared engine, as the `/v1/token`
    /// form it serves.
    pub(crate) fn new(operation: O, endpoint: Endpoint) -> Self {
        Self {
            operation,
            endpoint,
        }
    }
}

impl<O> ErasedOperation for OAuth2TokenEndpoint<O>
where
    O: ErasedOperation,
{
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            if let Some(refused) = refusal_for(self.endpoint, &parameters(req)) {
                return refused.response();
            }
            // The operation's own answer, at the status the error code it carries is
            // documented to be answered at (see [`with_documented_status`]): this
            // service's `AccessDeniedException` declares no status of its own.
            with_documented_status(self.operation.run(svc, req, ctx).await)
        })
    }

    /// The wrapped operation's modelled auth schemes: wrapping an operation must not change
    /// which schemes sign it — the anonymous endpoint stays anonymous and the IAM one stays
    /// SigV4-signed.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        self.operation.auth_schemes()
    }
}

/// The parameters `req` carries, as the JSON object its body is.
///
/// A body that is absent, not readable as bytes, not JSON or not a JSON object carries NO
/// parameter — which is the answer that makes the first layer that needs one refuse it (see
/// the [module docs](self) on the body this endpoint reads: the codec behind it reads the
/// modelled JSON payload alone, so a body in any other form would reach no handler either).
fn parameters(req: &RtRequest) -> Value {
    req.body()
        .bytes()
        .and_then(|bytes| ::serde_json::from_slice::<Value>(bytes).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(::serde_json::Map::new()))
}

/// The value `parameters` supplies for `parameter`, read under the name the request binds
/// it by, or `None` when the request supplies none.
///
/// A parameter present with an EMPTY value supplies none: RFC 6749 §3.1 — "parameters sent
/// without a value MUST be treated as if they were omitted from the request" — and a
/// `@required` member of this model is likewise unsatisfied by the empty string.
fn supplied<'p>(parameters: &'p Value, parameter: &Parameter) -> Option<&'p str> {
    parameters
        .get(parameter.bound)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

/// The wire name of the first of `required` the request supplies no value for, or `None`
/// when it supplies every one.
fn first_unsupplied(parameters: &Value, required: &[Parameter]) -> Option<&'static str> {
    required
        .iter()
        .find(|parameter| supplied(parameters, parameter).is_none())
        .map(|parameter| parameter.reported)
}

/// How this endpoint refuses `parameters`, or `None` for a request it accepts — which is
/// then served by the operation itself.
///
/// The order is the one the [module docs](self) state: the caller's client where the body is
/// what authenticates it, then the grant type, then the parameters that grant requires.
fn refusal_for(endpoint: Endpoint, parameters: &Value) -> Option<OAuth2Error> {
    match endpoint {
        Endpoint::ClientAuthenticated => {
            if !supplied(parameters, &CLIENT_ID).is_some_and(crate::state::is_known_oauth_client) {
                return Some(client_authentication_failed());
            }
            // `GrantType` is `^(authorization_code|refresh_token)$`: no other grant — and no
            // absent one — is a grant this endpoint implements.
            let required = match supplied(parameters, &ANONYMOUS_GRANT_TYPE) {
                Some(crate::state::GRANT_TYPE_AUTHORIZATION_CODE) => {
                    AUTHORIZATION_CODE_PARAMETERS
                }
                Some(crate::state::GRANT_TYPE_REFRESH_TOKEN) => REFRESH_TOKEN_PARAMETERS,
                _ => return Some(unsupported_grant_type()),
            };
            // The parameters a grant of this form needs are the ones the model requires
            // only FOR that grant, so a request that does not supply one is refused
            // without naming it (see [`malformed_request`]).
            let incomplete = required
                .iter()
                .any(|parameter| supplied(parameters, parameter).is_none());
            incomplete.then(malformed_request)
        }
        Endpoint::IamSigned => {
            match supplied(parameters, &IAM_GRANT_TYPE) {
                // The request names the one grant this form serves, so it is read on as a
                // client-credentials request.
                Some(crate::state::GRANT_TYPE_CLIENT_CREDENTIALS) => {}
                // A grant type the body SUPPLIES that this member does not admit:
                // `ClientCredentialsGrantType` is `^client_credentials$`, so the member takes
                // exactly one value and every other one is an invalid parameter value rather
                // than one of several grants this form declined to implement (see the
                // [module docs](self) on the two forms' grant layers).
                Some(_) => return Some(malformed_request()),
                // The body supplies no grant type at all, so there is no grant for the
                // authorization server to act on — the same class the anonymous form answers
                // for a request that names none, and the one RFC 6749 §5.2 words as
                // `unsupported_grant_type`.
                None => return Some(unsupported_grant_type()),
            }
            let Some(resource) = supplied(parameters, &RESOURCE) else {
                // The request does not state the resource this grant scopes a token to, so
                // it is not a complete client-credentials request; what the endpoint reports
                // is the first of the grant's own parameters the BODY supplies no value for,
                // which for a request carrying no `client_id` is that parameter.
                return Some(missing_parameter(
                    first_unsupplied(parameters, CLIENT_CREDENTIALS_PARAMETERS)
                        .unwrap_or(RESOURCE.reported),
                ));
            };
            if resource.chars().count() > crate::state::MAX_RESOURCE_LENGTH {
                // Supplied, but not a value the endpoint can accept: `resource` is
                // `@length(min: 1, max: 2048)`, counted in CHARACTERS as Smithy counts a
                // string's length.
                return Some(malformed_request());
            }
            None
        }
    }
}
