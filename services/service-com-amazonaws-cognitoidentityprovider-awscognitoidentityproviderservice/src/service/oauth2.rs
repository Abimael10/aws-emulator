// SPDX-License-Identifier: CC0-1.0

//! The OAuth2 endpoints a user pool serves on its DOMAIN: the token endpoint, the user-info
//! endpoint and the revocation endpoint.
//!
//! A pool's hosted authorization server is reached on the domain `CreateUserPoolDomain`
//! created — "a user pool domain hosts managed login, an authorization server and web server
//! for authentication in your application" (that operation's own doc) — and the OpenID
//! Connect discovery document of the pool is what names each endpoint of it
//! ([`crate::state::UserPool::openid_configuration`]). Those endpoints are not operations of
//! this service's Smithy model: they are the OAuth2 wire (RFC 6749 §3.2, RFC 7009 §2) and the
//! OpenID Connect UserInfo endpoint (OpenID Connect Core §5.3), each spoken in
//! `application/x-www-form-urlencoded` and answered in JSON, so no routing key names one and
//! they are answered through [`super::EmulatedService::override_routing_error`], the seam the
//! router consults for a request that named no modelled operation.
//!
//! ## Where they are served
//!
//! On THIS emulator's endpoint, under the domain the pool holds:
//! `<endpoint>/<domain>/oauth2/token` and its two siblings. The domain string resolves to one
//! pool ([`crate::state::ComAmazonawsCognitoidentityproviderAwscognitoidentityproviderserviceState::domain`]),
//! which is what makes the path the pool's own, exactly as `<domain>.auth.<region>.amazoncognito.com`
//! does on AWS — and the document that announces them names this address, because a URL a
//! service hands a caller to fetch later has to lead back to the emulator the caller reached.
//!
//! ## What each one does
//!
//! * **token** (`POST`): the grants an app client of the pool may exchange. `refresh_token`
//!   answers a new access and id token for the sign-in the presented refresh token names and
//!   issues NO new refresh token — the one the caller holds stays in force, which is the same
//!   reading `InitiateAuth`'s refresh flow applies
//!   ([`crate::state::ComAmazonawsCognitoidentityproviderAwscognitoidentityproviderserviceState::refresh_tokens`]).
//!   `client_credentials` answers the machine-to-machine access token of the app client
//!   itself, carrying the scopes the grant authorizes
//!   ([`crate::state::ComAmazonawsCognitoidentityproviderAwscognitoidentityproviderserviceState::issue_client_token`]).
//! * **userInfo** (`GET` or `POST`): the claims of the user whose access token the request
//!   presents as a bearer credential, bounded by the attributes the app client that token was
//!   issued for may read — the same bound `GetUser` applies to the same user.
//! * **revoke** (`POST`): revokes the sign-in the presented refresh token belongs to, which is
//!   the same revocation `RevokeToken` performs.
//!
//! Every refusal is the OAuth2 error of its endpoint: the `error` code of RFC 6749 §5.2 and
//! RFC 7009 §2.2.1 for the token and revocation endpoints, and the `invalid_token` of RFC 6750
//! §3.1 for the user-info endpoint — the codes a client branches on, and no invented prose
//! beside them.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use ::aws_smithy_types::body::SdkBody;

/// The path segment the three endpoints of a pool's authorization server live under, and the
/// last segment of each one.
const OAUTH2_SEGMENT: &str = "oauth2";
/// See [`OAUTH2_SEGMENT`].
const TOKEN_SEGMENT: &str = "token";
/// See [`OAUTH2_SEGMENT`]. Spelled as the discovery document announces it.
const USER_INFO_SEGMENT: &str = "userInfo";
/// See [`OAUTH2_SEGMENT`].
const REVOKE_SEGMENT: &str = "revoke";

/// The field a token request states the grant it is making in (RFC 6749 §4), and the two
/// grants an app client of a pool exchanges here.
const GRANT_TYPE_FIELD: &str = "grant_type";
/// See [`GRANT_TYPE_FIELD`].
const REFRESH_TOKEN_GRANT: &str = "refresh_token";
/// See [`GRANT_TYPE_FIELD`].
const CLIENT_CREDENTIALS_GRANT: &str = "client_credentials";
/// The grant the hosted sign-in pages hand a caller a code for (RFC 6749 §4.1).
const AUTHORIZATION_CODE_GRANT: &str = "authorization_code";

/// The fields a request states its app client, its secret, the refresh token and the scopes
/// in.
const CLIENT_ID_FIELD: &str = "client_id";
/// See [`CLIENT_ID_FIELD`].
const CLIENT_SECRET_FIELD: &str = "client_secret";
/// See [`CLIENT_ID_FIELD`].
const REFRESH_TOKEN_FIELD: &str = "refresh_token";
/// See [`CLIENT_ID_FIELD`].
const SCOPE_FIELD: &str = "scope";
/// The field a revocation states the token it revokes in (RFC 7009 §2.1).
const TOKEN_FIELD: &str = "token";

/// How the scopes of one grant travel: "a list of space-delimited, case-sensitive strings"
/// (RFC 6749 §3.3).
const SCOPE_SEPARATOR: char = ' ';

/// The OAuth2 flow an app client's `AllowedOAuthFlows` has to carry for the
/// `client_credentials` grant: "`client_credentials` Issue the access token from the
/// /oauth2/token endpoint directly to a non-person user" (`CreateUserPoolClient` in this
/// service's model).
const CLIENT_CREDENTIALS_FLOW: &str = "client_credentials";

/// The header a request authenticates its app client in, and the scheme the token endpoint
/// takes there: "client_secret_basic", one of the two methods the pool's discovery document
/// advertises (OpenID Connect Core §9).
const AUTHORIZATION_HEADER: &str = "authorization";

/// One of the two authentication schemes these endpoints read a credential under, each with
/// the spelling the `Authorization` header and a `WWW-Authenticate` refusal state it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthScheme {
    /// HTTP Basic, which carries an app client's id and secret (RFC 6749 §2.3.1).
    Basic,
    /// Bearer, which an access token is presented under (RFC 6750 §2.1).
    Bearer,
}

impl AuthScheme {
    /// The scheme token as a request spells it, compared without case (RFC 9110 §11.1).
    fn presented(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Bearer => "bearer",
        }
    }

    /// The scheme as a `WWW-Authenticate` challenge states it.
    fn stated(self) -> &'static str {
        match self {
            Self::Basic => "Basic",
            Self::Bearer => "Bearer",
        }
    }
}

/// The header a refusal of a credential names the scheme it expected in (RFC 6750 §3).
const AUTHENTICATE_HEADER: &str = "www-authenticate";

/// The media type every answer of these endpoints carries.
const JSON_MEDIA_TYPE: &str = "application/json";

/// The error codes a token or revocation request is refused with (RFC 6749 §5.2, RFC 7009
/// §2.2.1).
const INVALID_REQUEST: &str = "invalid_request";
/// See [`INVALID_REQUEST`].
const INVALID_CLIENT: &str = "invalid_client";
/// See [`INVALID_REQUEST`].
const INVALID_GRANT: &str = "invalid_grant";
/// See [`INVALID_REQUEST`].
const INVALID_SCOPE: &str = "invalid_scope";
/// See [`INVALID_REQUEST`].
const UNSUPPORTED_GRANT_TYPE: &str = "unsupported_grant_type";
/// The code a revocation of a token the app client does not revoke is refused with (RFC 7009
/// §2.2.1).
const UNSUPPORTED_TOKEN_TYPE: &str = "unsupported_token_type";
/// The code a bearer credential that does not authorize the request is refused with (RFC 6750
/// §3.1).
const INVALID_TOKEN: &str = "invalid_token";

/// The type an access token of this authorization server is presented as, which every answer
/// that carries one states (RFC 6749 §7.1).
const BEARER_TOKEN_TYPE: &str = "Bearer";

/// This service's state, as these endpoints hold it under the lock.
type State =
    crate::state::ComAmazonawsCognitoidentityproviderAwscognitoidentityproviderserviceState;

/// Which endpoint of a pool's authorization server a request addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Endpoint {
    /// `/<domain>/oauth2/token`.
    Token,
    /// `/<domain>/oauth2/userInfo`.
    UserInfo,
    /// `/<domain>/oauth2/revoke`.
    Revoke,
}

impl Endpoint {
    /// The endpoint the last path segment names, or `None` for a segment that names none.
    fn of(segment: &str) -> Option<Self> {
        match segment {
            TOKEN_SEGMENT => Some(Self::Token),
            USER_INFO_SEGMENT => Some(Self::UserInfo),
            REVOKE_SEGMENT => Some(Self::Revoke),
            _ => None,
        }
    }

    /// Whether this endpoint answers a request made with `method`: the token and revocation
    /// endpoints take the `POST` their form-encoded request body travels in (RFC 6749 §3.2,
    /// RFC 7009 §2.1), and the user-info endpoint takes "either HTTP GET or HTTP POST"
    /// (OpenID Connect Core §5.3.1).
    fn answers(self, method: &str) -> bool {
        match self {
            Self::Token | Self::Revoke => method.eq_ignore_ascii_case("POST"),
            Self::UserInfo => {
                method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("POST")
            }
        }
    }
}

/// The domain and the endpoint `path` addresses: `/<domain>/oauth2/<endpoint>` and nothing
/// else, so a path of a different shape is no endpoint of a pool's authorization server.
fn addressed(path: &str) -> Option<(&str, Endpoint)> {
    let mut segments = path.trim_start_matches('/').split('/');
    let domain = segments.next().filter(|domain| !domain.is_empty())?;
    if segments.next()? != OAUTH2_SEGMENT {
        return None;
    }
    let endpoint = Endpoint::of(segments.next()?)?;
    // Nothing may follow: `/<domain>/oauth2/token/anything` is not the token endpoint.
    if segments.next().is_some() {
        return None;
    }
    Some((domain, endpoint))
}

/// The path of a request, read the one way this service's non-modelled seams read it
/// ([`super::request_path::of`]).
fn request_path(uri: &str) -> &str {
    super::request_path::of(uri)
}

/// Whether `parts` addresses one of these endpoints — the request [`answer`] answers.
///
/// Read over the buffered parts, which is what the claiming seam
/// ([`super::EmulatedService::can_handle`]) is handed, by the same rule [`answer`] applies to
/// the request dispatch builds: both readings are here so the request this service CLAIMS and
/// the request it ANSWERS cannot drift apart.
pub fn is_endpoint(parts: &::http::request::Parts) -> bool {
    addressed(request_path(&parts.uri.to_string()))
        .is_some_and(|(_, endpoint)| endpoint.answers(parts.method.as_str()))
}

/// What this service answers the OAuth2 request `req`, or `None` for a request that addresses
/// none of these endpoints, or addresses one with a method it does not answer, or names a
/// domain this emulator holds no pool under — each of which keeps whatever answer the caller
/// would otherwise give.
pub fn answer<Eng: Send + Sync + 'static>(engine: &Eng, req: &RtRequest) -> Option<RtResponse> {
    let (domain, endpoint) = addressed(request_path(req.uri()))?;
    if !endpoint.answers(req.method()) {
        return None;
    }
    // A request a BROWSER sent from another site is refused before any of these endpoints
    // acts on it. Two of them mint or revoke a credential from a form-encoded body, which a
    // page may send cross-site with no preflight to stop it, and this seam is reached before
    // the core applies that rule to the requests it serves itself — so the rule is applied
    // here, read by the core's OWN classification of the headers a browser sets
    // ([`::emulator_server::classify_browser_request`]) over the same same-origin-only
    // posture, rather than by a second reading of those headers written out here.
    if let Some(refused) = cross_site_browser(req) {
        return Some(refused);
    }
    let state = crate::state::service_state(engine)?;
    let now = ::std::time::SystemTime::now();
    // The pool the domain belongs to, resolved before anything of the request is read: a
    // domain no pool of this emulator holds serves no authorization server at all.
    let user_pool_id = state
        .lock()
        .domain(domain)
        .map(|stored| stored.user_pool_id.clone())?;
    let fields = fields_of(req);
    let presented = presented_credential(req, AuthScheme::Bearer);
    let mut guard = state.lock();
    Some(match endpoint {
        Endpoint::Token => token(&mut guard, &user_pool_id, req, &fields, now),
        Endpoint::UserInfo => user_info(&guard, &user_pool_id, presented.as_deref(), now),
        Endpoint::Revoke => revoke(&mut guard, &user_pool_id, req, &fields),
    })
}

/// The refusal a request a browser sent from ANOTHER SITE earns on these endpoints, or `None`
/// for one no browser sent cross-site.
///
/// A web page cannot be allowed to drive an authorization server running on the developer's
/// own machine: a form-encoded `POST` is a request a page may send cross-site without a
/// preflight, and the token and revocation endpoints act on it. The verdict is the CORE's own
/// reading of the headers a browser sets (`Origin` and `Sec-Fetch-Site`), under the
/// same-origin-only posture the core applies to every request it serves itself, so this seam
/// cannot be a hole in that rule; a client that is no browser sets neither header and is
/// served.
fn cross_site_browser(req: &RtRequest) -> Option<RtResponse> {
    let header = |name: &str| req.headers().get(name).map(str::trim).filter(|v| !v.is_empty());
    let verdict = ::emulator_server::classify_browser_request(
        header("origin"),
        header("sec-fetch-site"),
        &::emulator_server::RemoteOrigins::SameOriginOnly,
        false,
    );
    match verdict {
        ::emulator_server::BrowserVerdict::Allow => None,
        // RFC 6749 §5.2 `invalid_request`: the request is not one this endpoint serves. It
        // carries no CORS permission, so the page cannot read it either.
        ::emulator_server::BrowserVerdict::Forbid(_) => {
            Some(error(403, INVALID_REQUEST, None))
        }
    }
}

/// The form fields a request carries, in the body every one of these endpoints takes its
/// request in (`application/x-www-form-urlencoded`, RFC 6749 §3.2 and RFC 7009 §2.1).
///
/// A body this emulator cannot read as a form carries no field, which each endpoint then
/// refuses for the field it required.
fn fields_of(req: &RtRequest) -> Vec<(String, String)> {
    let body = req.body().bytes().unwrap_or_default();
    ::url::form_urlencoded::parse(body)
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect()
}

/// The value of the field `name`, where the request states a non-empty one.
fn field<'f>(fields: &'f [(String, String)], name: &str) -> Option<&'f str> {
    fields
        .iter()
        .find(|(stated, _)| stated == name)
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.is_empty())
}

/// The credential `req` presents in the `Authorization` header under `scheme`, where it
/// presents one.
fn presented_credential(req: &RtRequest, scheme: AuthScheme) -> Option<String> {
    let header = req.headers().get(AUTHORIZATION_HEADER)?.trim();
    let (stated, credential) = header.split_once(' ')?;
    stated
        .eq_ignore_ascii_case(scheme.presented())
        .then(|| credential.trim().to_string())
        .filter(|credential| !credential.is_empty())
}

/// The app client a request authenticates as: its id and the secret it presents, if any.
///
/// Both of the methods the pool's discovery document advertises are read, the header first:
/// `client_secret_basic` carries the pair as HTTP Basic credentials (RFC 6749 §2.3.1) and
/// `client_secret_post` carries them as form fields. A request that names no app client at all
/// is `None`.
fn authenticated_client(req: &RtRequest, fields: &[(String, String)]) -> Option<ClientCredential> {
    if let Some(encoded) = presented_credential(req, AuthScheme::Basic) {
        if let Some(pair) = basic_credentials(&encoded) {
            return Some(pair);
        }
    }
    Some(ClientCredential {
        client_id: field(fields, CLIENT_ID_FIELD)?.to_string(),
        client_secret: field(fields, CLIENT_SECRET_FIELD).map(ToString::to_string),
    })
}

/// The app client id and secret HTTP Basic credentials carry: `base64(client_id ":"
/// client_secret)` (RFC 6749 §2.3.1). `None` for a value that is not that encoding of a pair.
fn basic_credentials(encoded: &str) -> Option<ClientCredential> {
    use ::base64::Engine as _;
    let decoded = ::base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (client_id, client_secret) = decoded.split_once(':')?;
    (!client_id.is_empty()).then(|| ClientCredential {
        client_id: client_id.to_string(),
        client_secret: (!client_secret.is_empty()).then(|| client_secret.to_string()),
    })
}

/// The app client a request authenticates as.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClientCredential {
    client_id: String,
    client_secret: Option<String>,
}

/// The token endpoint's answer to `req`.
///
/// The app client is authenticated FIRST, because every grant below is one an app client of
/// this pool makes: a request naming none, or one whose secret the stored client does not
/// accept, is refused before the grant is looked at, so a refusal says nothing about which
/// grants exist.
fn token(
    state: &mut State,
    user_pool_id: &str,
    req: &RtRequest,
    fields: &[(String, String)],
    now: ::std::time::SystemTime,
) -> RtResponse {
    let Some(credential) = authenticated_client(req, fields) else {
        return error(400, INVALID_REQUEST, None);
    };
    let accepted = state
        .pool(user_pool_id)
        .and_then(|pool| pool.clients.get(&credential.client_id))
        .is_some_and(|client| client.accepts_client_secret(credential.client_secret.as_deref()));
    if !accepted {
        return error(401, INVALID_CLIENT, Some(AuthScheme::Basic));
    }
    let client_id = credential.client_id;
    let Some(grant) = field(fields, GRANT_TYPE_FIELD) else {
        return error(400, INVALID_REQUEST, None);
    };
    match grant {
        REFRESH_TOKEN_GRANT => refresh_grant(state, user_pool_id, &client_id, fields, now),
        CLIENT_CREDENTIALS_GRANT => {
            client_credentials_grant(state, user_pool_id, &client_id, fields, now)
        }
        // A code is handed out by the hosted sign-in pages, and this authorization server has
        // issued none: the code the request presents authorizes nothing here, which is the
        // same answer a code that was never issued earns anywhere (RFC 6749 §5.2
        // `invalid_grant`).
        AUTHORIZATION_CODE_GRANT => error(400, INVALID_GRANT, None),
        _ => error(400, UNSUPPORTED_GRANT_TYPE, None),
    }
}

/// The `refresh_token` grant: new ID and access tokens for the sign-in the presented refresh
/// token names, and no new refresh token.
///
/// The token is resolved by the store's own shared reading of a refresh
/// ([`crate::state::ComAmazonawsCognitoidentityproviderAwscognitoidentityproviderserviceState::resolve_refresh_token`]),
/// the one `InitiateAuth`'s refresh flow and `GetTokensFromRefreshToken` apply: a token this
/// service never issued, one issued for another app client, one revoked, one expired and one a
/// rotation has replaced are all `invalid_grant` here, since none of them authorizes the
/// request.
fn refresh_grant(
    state: &mut State,
    user_pool_id: &str,
    client_id: &str,
    fields: &[(String, String)],
    now: ::std::time::SystemTime,
) -> RtResponse {
    let Some(refresh_token) = field(fields, REFRESH_TOKEN_FIELD) else {
        return error(400, INVALID_REQUEST, None);
    };
    let Ok(refreshed) = state.resolve_refresh_token(user_pool_id, client_id, refresh_token, now)
    else {
        return error(400, INVALID_GRANT, None);
    };
    let username = refreshed.username;
    // No pre-token-generation trigger is fired here: this seam is answered synchronously,
    // before the router's own request walk, so the tokens carry the claims the pool's stored
    // configuration states and nothing a function would have overridden.
    let overrides = crate::state::lambda_triggers::TokenOverrides::default();
    let Some(result) = state.refresh_tokens(
        user_pool_id,
        client_id,
        &username,
        now,
        refresh_token,
        &overrides,
    ) else {
        return error(400, INVALID_GRANT, None);
    };
    let mut answered = ::serde_json::Map::new();
    if let Some(access_token) = result.access_token {
        answered.insert("access_token".to_string(), access_token.into());
    }
    if let Some(id_token) = result.id_token {
        answered.insert("id_token".to_string(), id_token.into());
    }
    answered.insert("token_type".to_string(), BEARER_TOKEN_TYPE.into());
    answered.insert("expires_in".to_string(), result.expires_in.into());
    answered_json(200, ::serde_json::Value::Object(answered))
}

/// The `client_credentials` grant: the machine-to-machine access token of the app client
/// itself.
///
/// "`client_credentials` Issue the access token from the /oauth2/token endpoint directly to a
/// non-person user" (`CreateUserPoolClient.AllowedOAuthFlows`), so an app client whose stored
/// flows do not carry it does not serve this grant. The scopes are the ones the request asks
/// for, which have to be ones the client was permitted — "the ... scopes that you want to
/// permit your app client to authorize access with" (`AllowedOAuthScopes`) — and the client's
/// own permitted scopes where the request asks for none.
fn client_credentials_grant(
    state: &mut State,
    user_pool_id: &str,
    client_id: &str,
    fields: &[(String, String)],
    now: ::std::time::SystemTime,
) -> RtResponse {
    let Some(client) = state
        .pool(user_pool_id)
        .and_then(|pool| pool.clients.get(client_id))
    else {
        return error(401, INVALID_CLIENT, Some(AuthScheme::Basic));
    };
    let serves_grant = client
        .allowed_oauth_flows
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|flow| flow == CLIENT_CREDENTIALS_FLOW);
    if !serves_grant {
        // The grant is not one this app client was configured for, which is the
        // "authorization grant type ... not authorized for this client" of RFC 6749 §5.2.
        return error(400, "unauthorized_client", None);
    }
    let granted = client.allowed_oauth_scopes.clone().unwrap_or_default();
    let requested: Vec<String> = field(fields, SCOPE_FIELD)
        .unwrap_or_default()
        .split(SCOPE_SEPARATOR)
        .filter(|scope| !scope.is_empty())
        .map(ToString::to_string)
        .collect();
    let scopes = if requested.is_empty() {
        granted
    } else {
        if requested
            .iter()
            .any(|scope| !granted.iter().any(|permitted| permitted == scope))
        {
            return error(400, INVALID_SCOPE, None);
        }
        requested
    };
    let Some(result) = state.issue_client_token(user_pool_id, client_id, &scopes, now) else {
        return error(400, INVALID_GRANT, None);
    };
    let mut answered = ::serde_json::Map::new();
    if let Some(access_token) = result.access_token {
        answered.insert("access_token".to_string(), access_token.into());
    }
    answered.insert("token_type".to_string(), BEARER_TOKEN_TYPE.into());
    answered.insert("expires_in".to_string(), result.expires_in.into());
    if !scopes.is_empty() {
        answered.insert("scope".to_string(), scopes.join(" ").into());
    }
    answered_json(200, ::serde_json::Value::Object(answered))
}

/// The user-info endpoint's answer: the claims of the user whose access token the request
/// presents.
///
/// "The UserInfo Endpoint is an OAuth 2.0 Protected Resource ... Access Tokens ... SHOULD be
/// sent using the Authorization Request Header Field" (OpenID Connect Core §5.3), so the
/// credential is the bearer access token, and a request presenting none or presenting one this
/// pool does not accept is refused `invalid_token` (RFC 6750 §3.1).
///
/// The claims are the STORED user's: the subject the pool minted for it, its user name, and
/// each attribute the app client that token was issued for may read — the same bound `GetUser`
/// applies to the same user ([`crate::state::UserPoolClient::reads_attribute`]).
///
/// A pure READ: the token is resolved against the clock and nothing is written.
fn user_info(
    state: &State,
    user_pool_id: &str,
    presented: Option<&str>,
    now: ::std::time::SystemTime,
) -> RtResponse {
    let Some(access_token) = presented else {
        return error(401, INVALID_TOKEN, Some(AuthScheme::Bearer));
    };
    let Ok((pool, user)) = state.signed_in_user(access_token, now) else {
        return error(401, INVALID_TOKEN, Some(AuthScheme::Bearer));
    };
    // The token has to be one of THIS pool's: a token of another pool of the same emulator
    // authorizes nothing on this domain.
    if pool.id != user_pool_id {
        return error(401, INVALID_TOKEN, Some(AuthScheme::Bearer));
    }
    let client = state
        .token(access_token)
        .and_then(|record| pool.clients.get(&record.client_id));
    let mut claims = ::serde_json::Map::new();
    claims.insert("sub".to_string(), user.sub.clone().into());
    for attribute in user.attribute_list() {
        if attribute.name == "sub" {
            continue;
        }
        if client.is_none_or(|client| client.reads_attribute(&attribute.name)) {
            if let Some(value) = attribute.value.clone() {
                claims.insert(attribute.name.clone(), value.into());
            }
        }
    }
    // "username": the claim Amazon Cognito's user-info document names the user by, beside the
    // subject every OpenID Connect claim set carries.
    claims.insert("username".to_string(), user.username.clone().into());
    answered_json(200, ::serde_json::Value::Object(claims))
}

/// The revocation endpoint's answer: the sign-in the presented refresh token belongs to is
/// revoked, which is the revocation `RevokeToken` performs.
///
/// "The authorization server responds with HTTP status code 200 if the token has been revoked
/// successfully or if the client submitted an invalid token" (RFC 7009 §2.2), so a value this
/// app client issued no token for is served and revokes nothing. An app client whose token
/// revocation is off revokes no token at all, which is the `unsupported_token_type` of RFC
/// 7009 §2.2.1.
fn revoke(
    state: &mut State,
    user_pool_id: &str,
    req: &RtRequest,
    fields: &[(String, String)],
) -> RtResponse {
    let Some(credential) = authenticated_client(req, fields) else {
        return error(400, INVALID_REQUEST, None);
    };
    let client = state
        .pool(user_pool_id)
        .and_then(|pool| pool.clients.get(&credential.client_id));
    let Some(client) = client
        .filter(|client| client.accepts_client_secret(credential.client_secret.as_deref()))
    else {
        return error(401, INVALID_CLIENT, Some(AuthScheme::Basic));
    };
    if !client.enable_token_revocation {
        return error(400, UNSUPPORTED_TOKEN_TYPE, None);
    }
    let Some(token) = field(fields, TOKEN_FIELD) else {
        return error(400, INVALID_REQUEST, None);
    };
    let revoked = state
        .token(token)
        .filter(|record| {
            record.user_pool_id == user_pool_id && record.client_id == credential.client_id
        })
        .map(|record| record.origin.clone());
    if let Some(origin) = revoked {
        state.revoke_token_lineage(user_pool_id, &origin);
    }
    // A served revocation carries no body at all.
    let mut response = RtResponse::new(
        StatusCode::try_from(200).unwrap_or_else(|_| server_fault_status()),
        SdkBody::empty(),
    );
    set_header(&mut response, "content-length", "0");
    response
}

/// One JSON answer of these endpoints.
fn answered_json(status: u16, document: ::serde_json::Value) -> RtResponse {
    let status = StatusCode::try_from(status).unwrap_or_else(|_| server_fault_status());
    let mut response = RtResponse::new(status, SdkBody::from(document.to_string()));
    set_header(&mut response, "content-type", JSON_MEDIA_TYPE);
    response
}

/// One refusal of these endpoints: the `error` code alone, which is what a client branches on,
/// with the `WWW-Authenticate` header naming the scheme whose credential was not accepted
/// where one was being authenticated (RFC 6749 §5.2, RFC 6750 §3).
fn error(status: u16, code: &str, scheme: Option<AuthScheme>) -> RtResponse {
    let mut response = answered_json(status, ::serde_json::json!({ "error": code }));
    if let Some(scheme) = scheme {
        set_header(&mut response, AUTHENTICATE_HEADER, scheme.stated());
    }
    response
}

/// The status a response whose own status could not be built carries, which the fixed statuses
/// above cannot actually provoke.
fn server_fault_status() -> StatusCode {
    StatusCode::try_from(500).expect("500 is a status")
}

/// Set one response header, dropping a value the wire cannot carry rather than turning an
/// answered request into a transport failure.
fn set_header(response: &mut RtResponse, name: &'static str, value: &str) {
    if let Err(refused) = response.headers_mut().try_insert(name, value.to_string()) {
        ::tracing::info!(
            header = name,
            error = %refused,
            "this header cannot be sent on the wire and is left off the OAuth2 answer"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of [`only_the_three_published_paths_name_a_domain_and_an_endpoint`]: the case,
    /// the path, and the domain and endpoint it addresses.
    type AddressRow<'r> = (&'r str, &'r str, Option<(&'r str, Endpoint)>);

    /// Which paths address an endpoint of a pool's authorization server: the rows differ along
    /// the shape of the path and the endpoint its last segment names.
    #[test]
    fn only_the_three_published_paths_name_a_domain_and_an_endpoint() {
        let rows: [AddressRow<'_>; 10] = [
            (
                "the token endpoint",
                "/auth-example/oauth2/token",
                Some(("auth-example", Endpoint::Token)),
            ),
            (
                "the user-info endpoint",
                "/auth-example/oauth2/userInfo",
                Some(("auth-example", Endpoint::UserInfo)),
            ),
            (
                "the revocation endpoint",
                "/auth-example/oauth2/revoke",
                Some(("auth-example", Endpoint::Revoke)),
            ),
            (
                "an absolute URI of the token endpoint",
                "https://127.0.0.1:9297/auth-example/oauth2/token",
                Some(("auth-example", Endpoint::Token)),
            ),
            (
                "the token endpoint with a query",
                "/auth-example/oauth2/token?x=1",
                Some(("auth-example", Endpoint::Token)),
            ),
            ("no domain at all", "/oauth2/token", None),
            ("another segment in place of oauth2", "/d/oauth/token", None),
            (
                "an endpoint this server does not serve",
                "/auth-example/oauth2/authorize",
                None,
            ),
            (
                "a segment after the endpoint",
                "/auth-example/oauth2/token/extra",
                None,
            ),
            (
                "a pool's key-set document",
                "/us-east-1_abc123def/.well-known/jwks.json",
                None,
            ),
        ];
        for (case, path, expected) in rows {
            let resolved = addressed(request_path(path));
            assert_eq!(
                resolved, expected,
                "case {case:?}: expected {expected:?}, got {resolved:?}"
            );
        }
    }

    /// One row of [`each_endpoint_answers_the_methods_its_request_is_made_with`]: the case,
    /// the endpoint, the method and whether it answers it.
    type MethodRow<'r> = (&'r str, Endpoint, &'r str, bool);

    /// Which method each endpoint answers: the form-encoded grants are posted, and the
    /// user-info endpoint takes either of the two OpenID Connect names for it.
    #[test]
    fn each_endpoint_answers_the_methods_its_request_is_made_with() {
        let rows: [MethodRow<'_>; 6] = [
            ("a posted token request", Endpoint::Token, "POST", true),
            ("a token request made with GET", Endpoint::Token, "GET", false),
            ("a posted revocation", Endpoint::Revoke, "POST", true),
            ("a fetched user-info document", Endpoint::UserInfo, "GET", true),
            ("a posted user-info request", Endpoint::UserInfo, "POST", true),
            (
                "a user-info request made with DELETE",
                Endpoint::UserInfo,
                "DELETE",
                false,
            ),
        ];
        for (case, endpoint, method, expected) in rows {
            let answers = endpoint.answers(method);
            assert_eq!(
                answers, expected,
                "case {case:?}: expected the endpoint to answer {method}: {expected}, got \
                 {answers}"
            );
        }
    }

    /// One row of [`an_app_client_is_authenticated_from_the_header_or_from_the_form`]: the
    /// case, the `Authorization` header it carries, its form fields, and the app client it
    /// authenticates as.
    type CredentialRow<'r> = (
        &'r str,
        Option<String>,
        &'r [(&'r str, &'r str)],
        Option<(&'r str, Option<&'r str>)>,
    );

    /// The two methods a request authenticates its app client with, and the values that
    /// authenticate none.
    #[test]
    fn an_app_client_is_authenticated_from_the_header_or_from_the_form() {
        use ::base64::Engine as _;
        let encoded = ::base64::engine::general_purpose::STANDARD.encode("client-1:secret-1");
        let rows: [CredentialRow<'_>; 6] = [
            (
                "basic credentials carrying a pair",
                Some(format!("Basic {encoded}")),
                &[],
                Some(("client-1", Some("secret-1"))),
            ),
            (
                "form fields carrying a pair",
                None,
                &[("client_id", "client-2"), ("client_secret", "secret-2")],
                Some(("client-2", Some("secret-2"))),
            ),
            (
                "a public client naming no secret",
                None,
                &[("client_id", "client-3")],
                Some(("client-3", None)),
            ),
            (
                "basic credentials the header spells in another case",
                Some(format!("basic {encoded}")),
                &[],
                Some(("client-1", Some("secret-1"))),
            ),
            (
                "a header that is no base64 pair, with the form naming the client",
                Some("Basic not-base64!!".to_string()),
                &[("client_id", "client-4")],
                Some(("client-4", None)),
            ),
            ("a request naming no app client", None, &[], None),
        ];
        for (case, header, form, expected) in rows {
            let mut req = RtRequest::new(SdkBody::empty());
            req.set_method("POST").expect("a valid method");
            req.set_uri("/d/oauth2/token").expect("a valid URI");
            if let Some(header) = header {
                req.headers_mut()
                    .append(AUTHORIZATION_HEADER.to_string(), header);
            }
            let fields: Vec<(String, String)> = form
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect();
            let resolved = authenticated_client(&req, &fields);
            let expected = expected.map(|(client_id, secret)| ClientCredential {
                client_id: client_id.to_string(),
                client_secret: secret.map(ToString::to_string),
            });
            assert_eq!(
                resolved, expected,
                "case {case:?}: expected {expected:?}, got {resolved:?}"
            );
        }
    }
}
