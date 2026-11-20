// SPDX-License-Identifier: CC0-1.0

//! The USER AUTHENTICATION a listener's rule performs in front of the action behind it:
//! what an Application Load Balancer answers a request whose user is not authenticated.
//!
//! "Optionally, a rule for an HTTPS listener can also include one of the following user
//! authentication actions: `authenticate-oidc`, `authenticate-cognito`, or
//! `jwt-validation`" (`smithy_query(query="doc",shape="Action")`), and such an action runs
//! BEFORE the routing action it guards — "[t]he action with the lowest value for order is
//! performed first" (`Action$Order`). What it does with a request whose user is not
//! authenticated is the one member that states it, `OnUnauthenticatedRequest`, which
//! `AuthenticateCognitoActionConfig` and `AuthenticateOidcActionConfig` carry with the same
//! three values and the same documentation:
//!
//! * `deny` — "Return an HTTP 401 Unauthorized error" ([`UNAUTHORIZED_STATUS`]);
//! * `allow` — "Allow the request to be forwarded to the target", so the routing action
//!   behind this one answers;
//! * `authenticate` — "Redirect the request to the IdP authorization endpoint. This is the
//!   default value" ([`authorization_redirect`]).
//!
//! WHICH user is authenticated is a question nothing here can answer for anybody. The
//! session an Application Load Balancer recognises travels in a cookie IT sealed
//! (`SessionCookieName`, "[t]he name of the cookie used to maintain session information"),
//! with keys it hands to nothing, and the only thing that mints one is the code exchange
//! the load balancer performs against the IdP's token endpoint — an outbound connection
//! this emulator makes none of. So no cookie a client presents under that name is a session
//! that verifies here, whoever wrote it, and every request such an action guards is an
//! UNAUTHENTICATED one: a forged session cookie is answered exactly as a request carrying
//! none, which is the one thing a client must never be able to get past the edge with.
//!
//! # The redirect
//!
//! The redirect is an OAuth 2.0 authorization request addressed to the IdP's own
//! authorization endpoint, so the client arrives there knowing which application asked,
//! where to come back to and what was asked for: the client identifier, the load balancer's
//! own callback as the redirect URI, the authorization-CODE response type, the scope the
//! action states and an opaque state value tying the answer to this request, plus "[t]he
//! query parameters (up to 10) to include in the redirect request to the authorization
//! endpoint" the action carries. Beside it travels the nonce cookie a load balancer that
//! authenticates users sets on the way out and reads back on the callback, so the two legs
//! belong to one another.
//!
//! WHERE that endpoint is differs by provider, and each is read from the action alone — no
//! other service is asked, exactly as no outbound connection is made:
//!
//! * `authenticate-oidc` names it outright: `AuthorizationEndpoint`, "[t]he authorization
//!   endpoint of the IdP. This must be a full URL, including the HTTPS protocol, the
//!   domain, and the path";
//! * `authenticate-cognito` names the user pool's domain, `UserPoolDomain`, "[t]he domain
//!   prefix or fully-qualified domain name of the Amazon Cognito user pool" — and a prefix
//!   resolves to Amazon Cognito's own regional name for it, "a prefix of `myprefix` in the
//!   us-east-1 Region results in a domain of `myprefix.auth.us-east-1.amazoncognito.com`"
//!   (`CreateUserPoolDomain$Domain`), in the Region the pool's own ARN names.
//!
//! A `jwt-validation` action is not performed here: it carries no `OnUnauthenticatedRequest`
//! at all, and judging the token a client presented means fetching its
//! `JwksEndpoint` — "[t]he JSON Web Key Set (JWKS) endpoint ... used to validate signatures
//! from the provider" — which is the outbound connection this emulator does not make. Such
//! a rule is answered as an action that was not performed rather than by letting the request
//! through to what it guards.
//!
//! Nothing here is written: the state value and the nonce belong to the one request they
//! were produced for, so serving a request moves no status, mints no session and leaves
//! nothing a later caller can read. Nothing is dialled either — the client is TOLD where
//! the provider is, exactly as Elastic Load Balancing tells it, and this process opens no
//! connection to it. The URL it is told is bounded by what the action persisted plus the one
//! host the request carried: each component appears in it once, so no stored template can
//! multiply the request's own text into it.

use std::collections::BTreeMap;

use super::data_plane::{DataPlaneRequest, Served};
use crate::ops::types::{Action, AuthenticateCognitoActionConfig};

/// The two `ActionTypeEnum` values whose action authenticates the user of a request and
/// states what to do when it is not authenticated — the first two of the three
/// [`crate::state::AUTHENTICATION_ACTION_TYPES`] quotes.
const AUTHENTICATE_COGNITO_ACTION: &str = "authenticate-cognito";
const AUTHENTICATE_OIDC_ACTION: &str = "authenticate-oidc";

/// The `OnUnauthenticatedRequest` values, from
/// `AuthenticateCognitoActionConditionalBehaviorEnum` and the identical OIDC enum beside
/// it. A stored action naming none of them is at the documented default, `authenticate`.
const DENY_UNAUTHENTICATED: &str = "deny";
const ALLOW_UNAUTHENTICATED: &str = "allow";

/// The status `deny` answers with: "deny - Return an HTTP 401 Unauthorized error".
pub(super) const UNAUTHORIZED_STATUS: u16 = 401;

/// The status the redirect to the authorization endpoint is sent with: the client is sent to
/// the IdP for this request only and the address it asked for is unchanged, which is HTTP
/// 302 Found — the temporary of the two codes `RedirectActionConfig$StatusCode` admits.
const FOUND_STATUS: u16 = 302;

/// The path on the load balancer's own name that the authorization answer comes back to:
/// the endpoint a load balancer that authenticates users serves the IdP's callback at, and
/// so the one an IdP's client has registered as its callback URL.
const CALLBACK_PATH: &str = "/oauth2/idpresponse";

/// The path a user pool's domain serves its authorization endpoint at.
const HOSTED_UI_AUTHORIZE_PATH: &str = "/oauth2/authorize";

/// The ARN vendor of the user pool an `authenticate-cognito` action names, which is where
/// the Region of a domain PREFIX comes from.
const COGNITO_VENDOR: &str = "cognito-idp";

/// The parameters of the authorization request the client is redirected with, from OAuth
/// 2.0's authorization-code request: who is asking, where to come back to, what kind of
/// answer is wanted, which claims are asked for, and the value tying the answer to this
/// request.
const CLIENT_ID_PARAMETER: &str = "client_id";
const REDIRECT_URI_PARAMETER: &str = "redirect_uri";
const RESPONSE_TYPE_PARAMETER: &str = "response_type";
const SCOPE_PARAMETER: &str = "scope";
const STATE_PARAMETER: &str = "state";

/// Every parameter above, which an action's own extra parameters do not replace: what the
/// load balancer writes into the authorization request is the load balancer's.
const WRITTEN_PARAMETERS: [&str; 5] = [
    CLIENT_ID_PARAMETER,
    REDIRECT_URI_PARAMETER,
    RESPONSE_TYPE_PARAMETER,
    SCOPE_PARAMETER,
    STATE_PARAMETER,
];

/// The `response_type` an authorization-code request asks for, which is the flow a load
/// balancer completes with the IdP's token endpoint.
const AUTHORIZATION_CODE_RESPONSE_TYPE: &str = "code";

/// The default the model gives `Scope`: "The set of user claims to be requested from the
/// IdP. The default is `openid`."
const DEFAULT_SCOPE: &str = "openid";

/// The default the model gives `SessionCookieName`: "The name of the cookie used to
/// maintain session information. The default is AWSELBAuthSessionCookie."
const DEFAULT_SESSION_COOKIE_NAME: &str = "AWSELBAuthSessionCookie";

/// The cookie a load balancer that authenticates users sets on the redirect to the
/// authorization endpoint, and reads back on the callback: it ties the answer the IdP sends
/// to the request that was redirected.
const NONCE_COOKIE: &str = "AWSALBAuthNonce";

/// How many of an action's own extra parameters travel with the redirect: "[t]he query
/// parameters (up to 10) to include in the redirect request to the authorization endpoint"
/// (`AuthenticationRequestExtraParams`).
const MAX_EXTRA_PARAMETERS: usize = 10;

/// How many bytes of randomness the nonce and the state value each carry — enough that
/// neither is guessable by whoever holds the other.
const OPAQUE_VALUE_BYTES: usize = 16;

/// What a URL QUERY component may carry unescaped: the unreserved characters of RFC 3986,
/// so every value this module puts in the authorization request is one parameter whatever
/// it holds.
const QUERY_VALUE: &::percent_encoding::AsciiSet = &::percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// What one user-authentication action decides about the request it guards.
#[derive(Debug)]
pub(super) enum Guarded {
    /// The request goes through to the routing action behind this one.
    Allowed,
    /// The action answers the client itself.
    Answered(Served),
    /// An action this emulator cannot perform at all, so what it guards is not served
    /// either.
    NotPerformed,
}

/// What `action` does with `request`, whose user is not authenticated — as no user of a
/// request reaching this emulator is (see this module's own documentation).
pub(super) fn guarded(action: &Action, request: &DataPlaneRequest) -> Guarded {
    let Some(provider) = provider(action) else {
        return Guarded::NotPerformed;
    };
    if carries_a_session(request, &provider.session_cookie_name) {
        // Named, never read: the cookie's VALUE is whatever the client wrote, and the
        // session it claims verifies against nothing here, so the request is the
        // unauthenticated one the action's own behaviour is about.
        tracing::debug!(
            cookie = %provider.session_cookie_name,
            "the request carries a session cookie this load balancer did not seal, so its \
             user is not authenticated"
        );
    }
    match provider.on_unauthenticated_request.as_str() {
        ALLOW_UNAUTHENTICATED => Guarded::Allowed,
        DENY_UNAUTHENTICATED => Guarded::Answered(Served::status(UNAUTHORIZED_STATUS)),
        // "authenticate ... This is the default value", which is what an action stating no
        // behaviour at all is at.
        _ => Guarded::Answered(authorization_redirect(&provider, request)),
    }
}

/// What the action says about the identity provider it authenticates against, read from
/// the action alone.
#[derive(Debug)]
struct Provider {
    /// The IdP's authorization endpoint, as a full `https` URL.
    authorization_endpoint: String,
    /// The client identifier the authorization request asks as.
    client_id: String,
    /// The claims the authorization request asks for.
    scope: String,
    /// The cookie this action's session would travel in.
    session_cookie_name: String,
    /// The action's own extra query parameters, in name order so one configuration renders
    /// one URL.
    extra_parameters: BTreeMap<String, String>,
    /// What the action does with an unauthenticated request, and the empty string for an
    /// action stating nothing — which is the documented default.
    on_unauthenticated_request: String,
}

/// The provider one authentication action names, or `None` for an action this plane does
/// not perform: a `jwt-validation` action, and one whose configuration is absent or names
/// no endpoint a client could be sent to.
fn provider(action: &Action) -> Option<Provider> {
    match action.r#type.as_str() {
        AUTHENTICATE_COGNITO_ACTION => {
            let config = action.authenticate_cognito_config.as_ref()?;
            Some(Provider {
                authorization_endpoint: cognito_authorization_endpoint(config)?,
                client_id: config.user_pool_client_id.clone(),
                scope: stated(config.scope.as_deref()).unwrap_or(DEFAULT_SCOPE).to_string(),
                session_cookie_name: stated(config.session_cookie_name.as_deref())
                    .unwrap_or(DEFAULT_SESSION_COOKIE_NAME)
                    .to_string(),
                extra_parameters: extra_parameters(
                    config.authentication_request_extra_params.as_ref(),
                ),
                on_unauthenticated_request: stated(config.on_unauthenticated_request.as_deref())
                    .unwrap_or_default()
                    .to_string(),
            })
        }
        AUTHENTICATE_OIDC_ACTION => {
            let config = action.authenticate_oidc_config.as_ref()?;
            Some(Provider {
                authorization_endpoint: full_https_url(&config.authorization_endpoint)?,
                client_id: config.client_id.clone(),
                scope: stated(config.scope.as_deref()).unwrap_or(DEFAULT_SCOPE).to_string(),
                session_cookie_name: stated(config.session_cookie_name.as_deref())
                    .unwrap_or(DEFAULT_SESSION_COOKIE_NAME)
                    .to_string(),
                extra_parameters: extra_parameters(
                    config.authentication_request_extra_params.as_ref(),
                ),
                on_unauthenticated_request: stated(config.on_unauthenticated_request.as_deref())
                    .unwrap_or_default()
                    .to_string(),
            })
        }
        _ => None,
    }
}

/// The value a member STATES, and `None` where it states nothing — an absent member and one
/// holding only space are both a value the default stands in for.
fn stated(held: Option<&str>) -> Option<&str> {
    held.map(str::trim).filter(|value| !value.is_empty())
}

/// The authorization endpoint of the user pool an `authenticate-cognito` action names.
///
/// `UserPoolDomain` is "[t]he domain prefix or fully-qualified domain name of the Amazon
/// Cognito user pool", so a value carrying labels of its own IS the name, and a single label
/// is a PREFIX under Amazon Cognito's own regional name for a domain — "a prefix of
/// `myprefix` in the us-east-1 Region results in a domain of
/// `myprefix.auth.us-east-1.amazoncognito.com`" (`CreateUserPoolDomain$Domain`) — in the
/// Region `UserPoolArn` names, which is the Region the pool itself lives in.
///
/// `None` where that resolves to no name a client could be sent to: a domain stating
/// nothing, a prefix whose pool ARN names no Region, or a value carrying what would
/// re-point or truncate the URL's authority rather than being a host in it.
fn cognito_authorization_endpoint(config: &AuthenticateCognitoActionConfig) -> Option<String> {
    let domain = stated(Some(&config.user_pool_domain))?;
    let host = if domain.contains('.') {
        domain.to_string()
    } else {
        let region = ::service_common::arn::region_of(&config.user_pool_arn, COGNITO_VENDOR)?;
        format!("{domain}.auth.{region}.amazoncognito.com")
    };
    if host
        .contains(|character: char| !matches!(character, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '.'))
    {
        return None;
    }
    Some(format!("https://{host}{HOSTED_UI_AUTHORIZE_PATH}"))
}

/// `endpoint` where it is what its own documentation requires — "a full URL, including the
/// HTTPS protocol, the domain, and the path" — and `None` for anything else, which names no
/// endpoint a client could be sent to.
fn full_https_url(endpoint: &str) -> Option<String> {
    let endpoint = stated(Some(endpoint))?;
    let parsed = endpoint.parse::<::http::Uri>().ok()?;
    let https = parsed.scheme_str() == Some("https") && parsed.authority().is_some();
    https.then(|| endpoint.to_string())
}

/// The action's own extra query parameters, bounded by the number its documentation admits
/// and ordered by name, with the ones the load balancer itself writes left out.
///
/// Ordered BEFORE the bound is applied, so which of them travel is the configuration's own
/// first ten by name rather than whichever ten a map's iteration order happened to yield:
/// one configuration renders one URL.
fn extra_parameters(
    held: Option<&std::collections::HashMap<String, String>>,
) -> BTreeMap<String, String> {
    let ordered: BTreeMap<String, String> = held
        .map(|held| {
            held.iter()
                .filter(|(name, _)| !WRITTEN_PARAMETERS.contains(&name.as_str()))
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default();
    ordered.into_iter().take(MAX_EXTRA_PARAMETERS).collect()
}

/// Whether the request presents a cookie of the name this action's session would travel in
/// — a session that verifies against nothing here, whoever wrote it.
///
/// The cookies are read through the one reading of that header
/// ([`super::target_group_selection::carried_cookies`]), which bounds how many of them one
/// request is looked through.
fn carries_a_session(request: &DataPlaneRequest, session_cookie_name: &str) -> bool {
    super::target_group_selection::carried_cookies(
        request.header(super::target_group_selection::COOKIE_HEADER),
    )
    .any(|(name, _)| {
        // An Application Load Balancer splits a session over as many cookies as it needs,
        // each the configured name with its own index behind it, so every one of them is
        // that session's.
        name == session_cookie_name
            || name
                .strip_prefix(session_cookie_name)
                .is_some_and(|rest| rest.starts_with('-'))
    })
}

/// The redirect an unauthenticated request is answered with: the IdP's authorization
/// endpoint carrying this request's authorization-code request, and the nonce cookie that
/// ties the answer to it.
fn authorization_redirect(provider: &Provider, request: &DataPlaneRequest) -> Served {
    let mut parameters: Vec<(&str, String)> = vec![
        (CLIENT_ID_PARAMETER, provider.client_id.clone()),
        (REDIRECT_URI_PARAMETER, callback_url(request)),
        (
            RESPONSE_TYPE_PARAMETER,
            AUTHORIZATION_CODE_RESPONSE_TYPE.to_string(),
        ),
        (SCOPE_PARAMETER, provider.scope.clone()),
        (STATE_PARAMETER, opaque_value()),
    ];
    parameters.extend(
        provider
            .extra_parameters
            .iter()
            .map(|(name, value)| (name.as_str(), value.clone())),
    );
    let query = parameters
        .iter()
        .map(|(name, value)| format!("{}={}", encoded(name), encoded(value)))
        .collect::<Vec<String>>()
        .join("&");
    // The endpoint is a full URL and may carry a query of its own, which the authorization
    // request's parameters are added to rather than replacing.
    let separator = if provider.authorization_endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    Served {
        status: FOUND_STATUS,
        location: Some(format!(
            "{endpoint}{separator}{query}",
            endpoint = provider.authorization_endpoint
        )),
        headers: vec![("set-cookie".to_string(), nonce_cookie())],
        ..Served::default()
    }
}

/// The callback the IdP sends the authorization answer back to: the load balancer's own
/// authentication endpoint on the ADDRESS the CLIENT addressed, which is the URL an IdP's
/// client registers as its callback.
///
/// An authentication action is an HTTPS listener's — "[HTTPS listeners] Information for
/// using Amazon Cognito to authenticate users" (`Action$AuthenticateCognitoConfig`) — so
/// the callback is an `https` URL, carrying the port the client addressed where that is not
/// the one `https` implies.
///
/// The whole authority is the one the client DIALLED — its host and its own port
/// ([`DataPlaneRequest::client_port`]) — because this URL is the one the client comes back
/// to: an authority assembled out of two different addresses, the client's host on the port
/// of the listener the connection reached, names an address nothing need answer on, and the
/// address a load balancer of this emulator is reached at is the `host:port` it published
/// ([`crate::state::open_load_balancer_endpoint`]) rather than the port its listener
/// declares.
fn callback_url(request: &DataPlaneRequest) -> String {
    let port = request
        .client_port
        .filter(|port| *port != HTTPS_PORT)
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    format!(
        "https://{host}{port}{CALLBACK_PATH}",
        host = request.host
    )
}

/// The port an `https` URL implies, and so leaves out.
const HTTPS_PORT: i32 = 443;

/// The `Set-Cookie` field carrying this redirect's nonce: it covers the whole load
/// balancer's paths, travels only over TLS and is kept from any script, because what it
/// protects is one leg of a sign-in.
fn nonce_cookie() -> String {
    format!(
        "{NONCE_COOKIE}={value}; Path=/; Secure; HttpOnly; SameSite=Lax",
        value = opaque_value()
    )
}

/// One opaque value of this request's own: the state the authorization request carries, and
/// the nonce beside it. Drawn per request and stored nowhere, so two requests never share
/// one.
fn opaque_value() -> String {
    ::hex::encode(::rand::random::<[u8; OPAQUE_VALUE_BYTES]>())
}

/// One query component as a URL carries it.
fn encoded(value: &str) -> String {
    ::percent_encoding::utf8_percent_encode(value, QUERY_VALUE).to_string()
}

#[cfg(test)]
mod tests {
    //! The pure rules of this module: where each provider's authorization endpoint is, what
    //! the redirect carries, and which cookie counts as a session presented. What the plane
    //! SERVES is pinned end to end in `tests/all/alb_authenticate_users.rs`.

    use super::*;
    use crate::ops::types::AuthenticateOidcActionConfig;

    /// The user pool ARN the rows below name, whose Region is the one a domain PREFIX
    /// resolves under.
    const POOL_ARN: &str = "arn:aws:cognito-idp:eu-west-1:111111111111:userpool/eu-west-1_abc123";

    /// An `authenticate-cognito` action over one domain, scope and behaviour.
    fn cognito(domain: &str, pool_arn: &str, behaviour: Option<&str>) -> Action {
        Action {
            r#type: AUTHENTICATE_COGNITO_ACTION.to_string(),
            authenticate_cognito_config: Some(AuthenticateCognitoActionConfig {
                user_pool_arn: pool_arn.to_string(),
                user_pool_client_id: "4example client".to_string(),
                user_pool_domain: domain.to_string(),
                on_unauthenticated_request: behaviour.map(str::to_string),
                scope: Some("openid email".to_string()),
                ..AuthenticateCognitoActionConfig::default()
            }),
            ..Action::default()
        }
    }

    /// The request every row is answered for: a client that addressed one host, on one port.
    /// A client's own address is ONE authority, so the port it dialled is both the port that
    /// decides which listener took the request and the port of the address it came from.
    fn addressed(host: &str, port: Option<i32>, cookie: Option<&str>) -> DataPlaneRequest {
        DataPlaneRequest {
            method: "GET".to_string(),
            path: "/".to_string(),
            host: host.to_string(),
            port,
            client_port: port,
            headers: cookie
                .map(|carried| vec![("cookie".to_string(), carried.to_string())])
                .unwrap_or_default(),
            ..DataPlaneRequest::default()
        }
    }

    /// The `Location` of an answer that is a redirect, and a panic for anything else.
    fn location(guarded: Guarded, case: &str) -> String {
        match guarded {
            Guarded::Answered(served) => served
                .location
                .unwrap_or_else(|| panic!("case {case:?}: the answer carries no Location")),
            other => panic!("case {case:?}: expected a redirect, got {other:?}"),
        }
    }

    /// RULE: the authorization endpoint a client is sent to is the one the action names — a
    /// user pool's fully-qualified domain as it stands, a prefix under Amazon Cognito's
    /// regional name for it in the Region the pool's ARN names, and an OIDC provider's own
    /// full URL.
    #[test]
    fn the_client_is_sent_to_the_endpoint_the_action_names() {
        let oidc = Action {
            r#type: AUTHENTICATE_OIDC_ACTION.to_string(),
            authenticate_oidc_config: Some(AuthenticateOidcActionConfig {
                issuer: "https://idp.example.com".to_string(),
                authorization_endpoint: "https://idp.example.com/oauth2/v1/authorize?audience=a"
                    .to_string(),
                token_endpoint: "https://idp.example.com/token".to_string(),
                user_info_endpoint: "https://idp.example.com/userinfo".to_string(),
                client_id: "oidc-client".to_string(),
                ..AuthenticateOidcActionConfig::default()
            }),
            ..Action::default()
        };
        let rows: Vec<(&str, Action, &str)> = vec![
            (
                "a domain prefix",
                cognito("sign-in", POOL_ARN, Some("authenticate")),
                "https://sign-in.auth.eu-west-1.amazoncognito.com/oauth2/authorize?",
            ),
            (
                "a fully-qualified domain name",
                cognito("auth.example.com", POOL_ARN, Some("authenticate")),
                "https://auth.example.com/oauth2/authorize?",
            ),
            (
                "an OIDC endpoint carrying a query of its own",
                oidc,
                "https://idp.example.com/oauth2/v1/authorize?audience=a&",
            ),
        ];
        for (case, action, expected) in rows {
            let answered = location(guarded(&action, &addressed("app.example.com", None, None)), case);
            assert!(
                answered.starts_with(expected),
                "case {case:?}: expected a Location under {expected:?}, got {answered:?}"
            );
        }
    }

    /// RULE: the redirect carries this request's own authorization-code request — the
    /// action's client id and scope, the callback on the host the CLIENT addressed, the code
    /// response type and an opaque state — and a state no other request shares.
    #[test]
    fn the_redirect_carries_this_requests_authorization_code_request() {
        let action = cognito("sign-in", POOL_ARN, None);
        let rows: &[(&str, &str, Option<i32>, &str)] = &[
            (
                "the host alone",
                "app.example.com",
                None,
                "redirect_uri=https%3A%2F%2Fapp.example.com%2Foauth2%2Fidpresponse",
            ),
            (
                "the host on the port https implies",
                "app.example.com",
                Some(443),
                "redirect_uri=https%3A%2F%2Fapp.example.com%2Foauth2%2Fidpresponse",
            ),
            (
                "the host on another port",
                "app.example.com",
                Some(8443),
                "redirect_uri=https%3A%2F%2Fapp.example.com%3A8443%2Foauth2%2Fidpresponse",
            ),
        ];
        for (case, host, port, expected_callback) in rows {
            let answered = location(guarded(&action, &addressed(host, *port, None)), case);
            for expected in [
                "client_id=4example%20client",
                "response_type=code",
                "scope=openid%20email",
                expected_callback,
            ] {
                assert!(
                    answered.contains(expected),
                    "case {case:?}: expected {expected:?} in the Location, got {answered:?}"
                );
            }
        }
        // Two requests of the same configuration carry two states: the value belongs to the
        // request, and nothing of it is stored.
        let request = addressed("app.example.com", None, None);
        let first = location(guarded(&action, &request), "the first request");
        let second = location(guarded(&action, &request), "the second request");
        assert_ne!(
            first, second,
            "case two requests: each must carry its own state value, got {first:?} twice"
        );
    }

    /// RULE: the action's own extra query parameters travel with the redirect, and do not
    /// replace the parameters the load balancer itself writes.
    #[test]
    fn the_actions_extra_parameters_travel_with_the_redirect() {
        let mut action = cognito("sign-in", POOL_ARN, Some("authenticate"));
        let extra: std::collections::HashMap<String, String> = [
            ("prompt", "login"),
            ("ui_locales", "en-GB fr"),
            // A parameter the load balancer writes itself: the action does not replace it.
            ("client_id", "not-this-one"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
        if let Some(config) = action.authenticate_cognito_config.as_mut() {
            config.authentication_request_extra_params = Some(extra);
        }
        let case = "an action carrying extra parameters";
        let answered = location(
            guarded(&action, &addressed("app.example.com", None, None)),
            case,
        );
        for expected in ["prompt=login", "ui_locales=en-GB%20fr", "client_id=4example%20client"] {
            assert!(
                answered.contains(expected),
                "case {case:?}: expected {expected:?} in the Location, got {answered:?}"
            );
        }
        assert!(
            !answered.contains("not-this-one"),
            "case {case:?}: an extra parameter must not replace one the load balancer writes, \
             got {answered:?}"
        );
    }

    /// RULE: a session cookie is one carrying the configured name, whole or with the index
    /// of one part behind it, and nothing else is read as a session.
    #[test]
    fn a_session_is_the_cookie_the_configuration_names() {
        let rows: &[(&str, Option<&str>, bool)] = &[
            ("no cookie at all", None, false),
            ("another cookie", Some("AWSALB=abc"), false),
            ("the session cookie", Some("AWSELBAuthSessionCookie=sealed"), true),
            (
                "one part of a split session",
                Some("first=1; AWSELBAuthSessionCookie-0=forged; last=2"),
                true,
            ),
            (
                "a name that only starts the same",
                Some("AWSELBAuthSessionCookieX=no"),
                false,
            ),
        ];
        for (case, carried, expected) in rows {
            let request = addressed("app.example.com", None, *carried);
            let read = carries_a_session(&request, DEFAULT_SESSION_COOKIE_NAME);
            assert_eq!(
                read, *expected,
                "case {case:?}: expected {expected}, got {read}"
            );
            // Whatever the client carried, the answer is the same: a session it wrote
            // itself verifies against nothing here.
            let answered = guarded(&cognito("sign-in", POOL_ARN, None), &request);
            assert!(
                matches!(&answered, Guarded::Answered(served) if served.status == FOUND_STATUS),
                "case {case:?}: expected the redirect, got {answered:?}"
            );
        }
    }

    /// RULE: an action naming no endpoint a client could be sent to, and a
    /// `jwt-validation` action, are not performed.
    #[test]
    fn an_action_naming_no_reachable_endpoint_is_not_performed() {
        let jwt = Action {
            r#type: "jwt-validation".to_string(),
            ..Action::default()
        };
        let rows: Vec<(&str, Action)> = vec![
            ("a jwt-validation action", jwt),
            (
                "a prefix whose pool ARN names no Region",
                cognito("sign-in", "arn:aws:cognito-idp::111111111111:userpool/p", None),
            ),
            (
                "a prefix whose pool ARN is no ARN",
                cognito("sign-in", "not-an-arn", None),
            ),
            ("no domain at all", cognito("  ", POOL_ARN, None)),
            (
                "a domain that would re-point the URL",
                cognito("auth.example.com/evil", POOL_ARN, None),
            ),
            (
                "an action carrying no configuration",
                Action {
                    r#type: AUTHENTICATE_COGNITO_ACTION.to_string(),
                    ..Action::default()
                },
            ),
        ];
        for (case, action) in rows {
            let answered = guarded(&action, &addressed("app.example.com", None, None));
            assert!(
                matches!(answered, Guarded::NotPerformed),
                "case {case:?}: expected an action not performed, got {answered:?}"
            );
        }
    }

    /// RULE: `OnUnauthenticatedRequest` decides the answer — `deny` the 401, `allow` the
    /// action behind it, and `authenticate` (the default, and so also an action stating
    /// nothing) the redirect.
    #[test]
    fn on_unauthenticated_request_decides_the_answer() {
        let rows: &[(&str, Option<&str>, Option<u16>)] = &[
            ("deny", Some("deny"), Some(UNAUTHORIZED_STATUS)),
            ("allow", Some("allow"), None),
            ("authenticate", Some("authenticate"), Some(FOUND_STATUS)),
            ("nothing stated", None, Some(FOUND_STATUS)),
        ];
        for (case, behaviour, expected) in rows {
            let answered = guarded(
                &cognito("sign-in", POOL_ARN, *behaviour),
                &addressed("app.example.com", None, None),
            );
            match (expected, &answered) {
                (Some(status), Guarded::Answered(served)) => assert_eq!(
                    served.status, *status,
                    "case {case:?}: expected HTTP {status}, got {}",
                    served.status
                ),
                (None, Guarded::Allowed) => {}
                (expected, answered) => panic!(
                    "case {case:?}: expected {expected:?}, got {answered:?}"
                ),
            }
            // Only the redirect hands out a nonce: a 401 carries none.
            if let Guarded::Answered(served) = &answered {
                let nonces = served
                    .headers
                    .iter()
                    .filter(|(_, value)| value.starts_with(NONCE_COOKIE))
                    .count();
                let expected_nonces = usize::from(served.status == FOUND_STATUS);
                assert_eq!(
                    nonces, expected_nonces,
                    "case {case:?}: expected {expected_nonces} nonce cookie(s), got {nonces}"
                );
            }
        }
    }
}
