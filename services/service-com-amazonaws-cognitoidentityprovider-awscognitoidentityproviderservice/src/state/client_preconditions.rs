// SPDX-License-Identifier: CC0-1.0

//! The cross-member preconditions an app-client write has to satisfy before the
//! client is stored.
//!
//! Each rule below holds over the configuration a request would LEAVE the app
//! client with, not over one member in isolation, so the operations that write an
//! app client — `CreateUserPoolClient` and `UpdateUserPoolClient` — ask the same
//! question of the same values and answer it with the same sentence.

/// The message an app-client write is refused with when it turns the OAuth
/// authorization-server features on without the configuration they rest on
/// ([`oauth_configuration_incomplete`]). Fixed text: it names the two members the
/// request is missing a value for, which are the same two for every such request.
pub const OAUTH_CONFIGURATION_REQUIRED_MESSAGE: &str =
    "AllowedOAuthFlows and AllowedOAuthScopes are required if user pool client is \
     allowed to use OAuth flows.";

/// Whether the configuration a write would leave the app client with turns the
/// OAuth authorization server on while configuring no grant or no scope for it.
///
/// "Set to `true` to use OAuth 2.0 authorization server features in your app
/// client. This parameter must have a value of `true` before you can configure
/// the following features in your app client ... `AllowedOAuthScopes`: OAuth 2.0
/// scopes[,] `AllowedOAuthFlows`: support for authorization code, implicit, and
/// client credentials OAuth 2.0 grants"
/// (`AllowedOAuthFlowsUserPoolClient` in this service's model) — the flag and
/// those two members describe ONE authorization server, which issues a token only
/// for a grant it allows and only with a scope it may authorize. A request that
/// switches it on while leaving either list empty asks for a server that can
/// issue nothing, so it is refused rather than stored; a request that leaves the
/// flag off configures no such server and is judged by neither member.
///
/// Both arguments are the EFFECTIVE values of the request — what the app client
/// would hold once it is written — so the triple is judged as the caller left it,
/// whichever operation wrote it.
#[must_use]
pub fn oauth_configuration_incomplete(
    allowed_oauth_flows_user_pool_client: bool,
    allowed_oauth_flows: Option<&[String]>,
    allowed_oauth_scopes: Option<&[String]>,
) -> bool {
    allowed_oauth_flows_user_pool_client
        && (allowed_oauth_flows.is_none_or(<[String]>::is_empty)
            || allowed_oauth_scopes.is_none_or(<[String]>::is_empty))
}

/// The `OAuthFlowType` values whose response the authorization server delivers by
/// REDIRECTING the browser back to the application: `code`, which "provides an
/// authorization code as the response", and `implicit`, which issues "the access
/// token, and the ID token when scopes like `openid` and `profile` are requested,
/// directly to your user" (`CreateUserPoolClient.AllowedOAuthFlows` in this
/// service's model). The third value of that enumeration is the
/// machine-to-machine grant, issued "from the `/oauth2/token` endpoint directly to
/// a non-person user", which redirects nobody.
const REDIRECT_OAUTH_FLOWS: [&str; 2] = ["code", "implicit"];

/// The message an app-client write is refused with when it allows a
/// redirect-based grant while registering no URL to redirect to
/// ([`redirect_grant_without_a_callback_url`]). Fixed text: it names the member
/// the request is missing a value for and the grants that require it, which are
/// the same for every such request.
pub const CALLBACK_URL_REQUIRED_MESSAGE: &str =
    "CallbackUrls can not be empty when code flow or implicit flow is selected";

/// Whether the configuration a write would leave the app client with allows a
/// grant whose response is delivered by redirect ([`REDIRECT_OAUTH_FLOWS`]) while
/// registering no callback URL to deliver it to.
///
/// A callback URL is where "you want to send your users' browsers after they
/// complete authentication", and "Amazon Cognito doesn't accept authorization
/// requests with `redirect_uri` values that aren't in the list of `CallbackURLs`
/// that you provide" (`CreateUserPoolClient.CallbackURLs` in this service's
/// model) — so an authorization server that allows the code or implicit grant and
/// registers no such URL can answer no authorization request at all, and the
/// write is refused rather than stored.
///
/// Judged over the configuration as the caller left it, whichever operation wrote
/// it: an app client whose authorization server is off
/// (`AllowedOAuthFlowsUserPoolClient`, "this parameter must have a value of `true`
/// before you can configure … `AllowedOAuthFlows`") runs no such server, so it is
/// judged by neither member, exactly as [`oauth_configuration_incomplete`] leaves
/// it alone.
#[must_use]
pub fn redirect_grant_without_a_callback_url(
    allowed_oauth_flows_user_pool_client: bool,
    allowed_oauth_flows: Option<&[String]>,
    callback_urls: Option<&[String]>,
) -> bool {
    allowed_oauth_flows_user_pool_client
        && allowed_oauth_flows.is_some_and(|flows| {
            flows
                .iter()
                .any(|flow| REDIRECT_OAUTH_FLOWS.contains(&flow.as_str()))
        })
        && callback_urls.is_none_or(<[String]>::is_empty)
}

/// Every URL a write would leave the app client registered for a browser redirect
/// with, in the order the request listed them: the callback URLs, then the sign-out
/// redirect URLs, then the default redirect URI.
///
/// The ONE definition of "every redirect URL this write would leave the app client
/// with", so each rule below judges the same values in the same order whichever
/// operation wrote them, and the refusal names the first offending URL wherever
/// the caller put it. The three members are the ones an app client registers a
/// browser redirect in (`CreateUserPoolClient`'s `CallbackURLs`, `LogoutURLs` and
/// `DefaultRedirectURI`).
pub fn registered_redirect_urls<'write>(
    callback_urls: Option<&'write [String]>,
    logout_urls: Option<&'write [String]>,
    default_redirect_uri: Option<&'write str>,
) -> impl Iterator<Item = &'write str> {
    callback_urls
        .into_iter()
        .flatten()
        .chain(logout_urls.into_iter().flatten())
        .map(String::as_str)
        .chain(default_redirect_uri)
}

/// The message an app-client write is refused with when a URL it registers for a
/// browser redirect is no absolute URI because it names no scheme
/// ([`redirect_uri_without_a_scheme`]).
///
/// The offending URL is the one value such a refusal reports, taken verbatim from
/// the member the request carried it in; the rest is fixed text.
#[must_use]
pub fn scheme_less_redirect_uri_message(url: &str) -> String {
    format!("{url} must contain a scheme.")
}

/// The first URL among `urls` that names no scheme, and so is no absolute URI,
/// and `None` when every one of them names one.
///
/// "A redirect URI must meet the following requirements: … Be an absolute URI"
/// (`CreateUserPoolClient.CallbackURLs` in this service's model): the
/// authorization server sends the browser to the URL as the app client registered
/// it, with no base to resolve it against, so a bare host, a path, or any other
/// relative reference is not a URL an app client can register, however the request
/// spelled it.
///
/// The URL is read by the tree's URL parser rather than scanned here, so what
/// counts as a scheme is what a browser would resolve: a value the parser can read
/// only as relative to some base names none, while one it reads on its own names
/// one — whichever it is, the application schemes "such as `myapp://example`" the
/// same member admits included. A value that names a scheme and is malformed for
/// some other reason is not about this rule, and is left to the rules that are.
///
/// `urls` is every URL the write would leave the app client with, in the order the
/// request listed them, so the refusal names the first offending one wherever the
/// caller put it.
#[must_use]
pub fn redirect_uri_without_a_scheme<'a>(
    urls: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    urls.into_iter().find(|url| {
        matches!(
            ::url::Url::parse(url),
            Err(::url::ParseError::RelativeUrlWithoutBase)
        )
    })
}

/// The message an app-client write is refused with when a URL it registers for a
/// browser redirect carries a fragment component
/// ([`redirect_uri_with_a_fragment`]).
///
/// The offending URL is the one value such a refusal reports, taken verbatim from
/// the member the request carried it in; the rest is fixed text.
#[must_use]
pub fn fragment_redirect_uri_message(url: &str) -> String {
    format!("{url} cannot use fragment")
}

/// The first URL among `urls` that the authorization server could not redirect a
/// browser to because it carries a fragment component, and `None` when none of
/// them does.
///
/// "A redirect URI must meet the following requirements: … Not include a fragment
/// component" (`CreateUserPoolClient.CallbackURLs` in this service's model, citing
/// OAuth 2.0's redirection endpoint): the authorization server appends its
/// response to the URI it redirects to, and a browser keeps a fragment to itself,
/// so a URI carrying one is not one an app client can register, however the
/// request spelled it.
///
/// The URL is read by the tree's URL parser rather than scanned here, so what
/// counts as the fragment is what a browser would resolve. A value that is no
/// absolute URL at all carries no component to judge, and is left to the member's
/// own constraints. `urls` is every URL the write would leave the app client with,
/// in the order the request listed them, so the refusal names the first offending
/// one wherever the caller put it.
#[must_use]
pub fn redirect_uri_with_a_fragment<'a>(
    urls: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    urls.into_iter().find(|url| {
        ::url::Url::parse(url).is_ok_and(|parsed| parsed.fragment().is_some())
    })
}

/// The message an app-client write is refused with when a URL it registers for a
/// browser redirect is a cleartext one ([`http_redirect_uri`]).
///
/// The offending URL is the one value such a refusal reports, taken verbatim from
/// the member the request carried it in; the rest is fixed text.
#[must_use]
pub fn http_redirect_uri_message(url: &str) -> String {
    format!("{url} cannot use the HTTP protocol.")
}

/// The hosts a cleartext redirect URL may name: "Amazon Cognito requires HTTPS
/// over HTTP except for callback URLs to `http://localhost`, `http://127.0.0.1`
/// and `http://[::1]`. These callback URLs are for testing purposes only. You can
/// specify custom TCP ports for your callback URLs"
/// (`CreateUserPoolClient.CallbackURLs` in this service's model) — the loopback
/// interface, by name and by address in either family.
const LOOPBACK_REDIRECT_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// The scheme a redirect URL may not reach a non-loopback host over, as a URL
/// parser normalises it (lowercase, no trailing separator).
const HTTP_SCHEME: &str = "http";

/// The first URL among `urls` that a browser would be redirected to over
/// cleartext, and `None` when every one of them is a URL this service registers.
///
/// "Amazon Cognito requires HTTPS over HTTP except for callback URLs to
/// `http://localhost`, `http://127.0.0.1` and `http://[::1]`"
/// (`CreateUserPoolClient.CallbackURLs`), so a URL whose scheme is `http` and
/// whose host is not the loopback interface is one the service does not register,
/// however the request spelled it — and a URL in any other scheme, including the
/// application schemes "such as `myapp://example`" the same member admits, is not
/// about this rule at all.
///
/// `urls` is every URL the write would leave the app client with, in the order the
/// request listed them, so the refusal names the first offending one wherever the
/// caller put it.
#[must_use]
pub fn http_redirect_uri<'a>(urls: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    urls.into_iter().find(|url| is_cleartext_redirect(url))
}

/// Whether `url` would redirect a browser over cleartext: the `http` scheme to a
/// host that is not the loopback interface ([`LOOPBACK_REDIRECT_HOSTS`]).
///
/// The URL is read by the tree's URL parser rather than scanned here, so the
/// scheme and the host are the ones a browser would resolve — the scheme without
/// case, and the host without the userinfo before it or the port after it ("you
/// can specify custom TCP ports for your callback URLs"). A value that is no
/// absolute URL at all names no host to judge, and is left to the member's own
/// constraints.
fn is_cleartext_redirect(url: &str) -> bool {
    let Ok(parsed) = ::url::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != HTTP_SCHEME {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    // An IPv6 host is serialized in brackets, which are not part of the address.
    let host = host
        .strip_prefix('[')
        .and_then(|inside| inside.strip_suffix(']'))
        .unwrap_or(host);
    !LOOPBACK_REDIRECT_HOSTS
        .iter()
        .any(|loopback| host.eq_ignore_ascii_case(loopback))
}

/// The message an app-client write is refused with when the default redirect URI
/// it names is none of the callback URLs the client would register
/// ([`unregistered_default_redirect_uri`]).
///
/// The default redirect URI is the one value such a refusal reports, taken
/// verbatim from the request; the rest is fixed text.
#[must_use]
pub fn default_redirect_uri_not_registered_message(url: &str) -> String {
    format!("The default redirect URI {url} is not in the callback URIs list.")
}

/// The default redirect URI a write would leave the app client with when it is
/// none of the callback URLs that same write would leave it with, and `None` when
/// the request names no default redirect URI or names one that is registered.
///
/// "The default redirect URI. In app clients with one assigned IdP, replaces
/// `redirect_uri` in authentication requests. Must be in the `CallbackURLs` list"
/// (`CreateUserPoolClient.DefaultRedirectURI` in this service's model), and
/// "Amazon Cognito doesn't accept authorization requests with `redirect_uri`
/// values that aren't in the list of `CallbackURLs` that you provide"
/// (`CallbackURLs`) — the default stands in for a `redirect_uri` the request
/// omits, so one the app client does not register is a default no authorization
/// request could be answered with.
///
/// The two arguments are the EFFECTIVE values of the write — what the app client
/// would hold once it is stored — so the pair is judged as the caller left it,
/// whichever operation wrote it, and the URI is compared to the registered text as
/// it stands, which is the text an authorization request is matched against.
#[must_use]
pub fn unregistered_default_redirect_uri<'a>(
    default_redirect_uri: Option<&'a str>,
    callback_urls: Option<&[String]>,
) -> Option<&'a str> {
    let default_redirect_uri = default_redirect_uri?;
    let registered = callback_urls
        .unwrap_or_default()
        .iter()
        .any(|callback| callback == default_redirect_uri);
    (!registered).then_some(default_redirect_uri)
}
