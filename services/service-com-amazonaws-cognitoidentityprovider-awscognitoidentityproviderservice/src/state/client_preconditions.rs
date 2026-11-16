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
