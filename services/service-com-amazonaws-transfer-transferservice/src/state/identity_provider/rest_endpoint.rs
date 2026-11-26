// SPDX-License-Identifier: CC0-1.0

//! The authentication request made against an `API_GATEWAY` identity provider: the REST
//! endpoint the server's `IdentityProviderDetails.Url` names, invoked over the in-process
//! transport, and what it answered.
//!
//! `TestIdentityProvider` reports the OUTCOME of an authentication request — `StatusCode` is
//! "the HTTP status code that is the response from your API Gateway or your Lambda function"
//! and `Response` "the response that is returned from your API Gateway or your Lambda
//! function" — so for a server that delegates authentication to a REST endpoint the answer is
//! read off a request really made to it.
//!
//! WHICH request: a custom identity provider serves ONE authentication resource, and Transfer
//! Family documents it as `GET /servers/{serverId}/users/{username}/config` under the
//! configured `Url`, with the protocol and the source address of the authentication as query
//! parameters and the offered password in the `Password` header — the same method and resource
//! this crate's invocation-role verification authorizes the provider against
//! ([`crate::state::authorization`]). Every value in it is the request's own, and a member the
//! request did not name is left out rather than invented.
//!
//! The request goes over the IN-PROCESS transport
//! ([`::smithy_server_runtime::EmbeddedEmulator::http_connector`]) and nothing else: no socket
//! is opened and no name is resolved, so it reaches only an endpoint this emulator serves
//! itself — a deployed stage of a REST API the API Gateway emulation beside this service
//! holds, which is what a client's `Url` names when the API is one it created here. It is
//! SIGNED, as every in-process caller signs ([`::service_common::in_process_sigv4`]) and under
//! the signing name an invoke endpoint authenticates, because a method authorized by IAM
//! serves no unsigned caller.
//!
//! An endpoint this emulator serves NOTHING at answers the transport's own refusal
//! ([`::service_common::in_process_refusal`]), which is a request that reached no provider
//! rather than an answer from one: this module reports `None` for it and the caller answers
//! with the no-response answer, so no status and no body are claimed for a request nothing
//! served.

use ::std::time::Duration;

use ::url::Url;

/// The signing name an invoke of a deployed REST API stage is signed under: the
/// `execute-api` service AWS publishes the invoke endpoint and the `execute-api:Invoke`
/// action under, which is also the label naming that data plane in the host it is reached at.
const INVOKE_SIGNING_NAME: &str = "execute-api";

/// The media type the authentication answer is served in, as a custom identity provider's
/// documented answer (`{"Role": …, "HomeDirectory": …, "Policy": …}`) is a JSON document.
const APPLICATION_JSON: &str = "application/json";

/// The resource a custom identity provider serves the authentication of ONE user at, under
/// the endpoint the provider names: `GET /servers/{serverId}/users/{username}/config`, the
/// API Gateway method request Transfer Family documents for a custom identity provider. The
/// two variable segments are the server and the user of this authentication.
const SERVERS_SEGMENT: &str = "servers";
const USERS_SEGMENT: &str = "users";
const CONFIG_SEGMENT: &str = "config";

/// The header the offered password travels in, which is how that resource is handed it — the
/// password is never part of the path or the query.
const PASSWORD_HEADER: &str = "Password";

/// The query parameters the authentication resource takes beside its path: the protocol the
/// authentication is for and the address it is coming from.
const PROTOCOL_PARAMETER: &str = "protocol";
const SOURCE_IP_PARAMETER: &str = "sourceIp";

/// The longest this operation waits for the provider's endpoint to answer.
///
/// A CAP on one request rather than a wait: an endpoint that answers sooner costs what it
/// costs, and one that never answers still leaves this operation able to answer its caller.
const ENDPOINT_CEILING: Duration = Duration::from_secs(30);

/// How much of the provider's response body is read and reported.
///
/// The EMULATOR's own bound: the documented answer is a handful of members, so the bytes a
/// test of a provider costs are fixed here rather than by whatever that endpoint answers.
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// Make the authentication request `request` against the REST endpoint `endpoint` names and
/// answer what it answered.
///
/// `None` when no provider answered at all: an `endpoint` that is no HTTP endpoint, a request
/// that could not be rendered or signed, and an address this emulator serves nothing at —
/// each of which leaves the caller reporting the no-response answer rather than an outcome
/// nothing produced.
pub(super) async fn authenticate(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    request_region: &str,
    endpoint: &str,
    request: &super::AuthenticationRequest<'_>,
) -> Option<super::ProviderAnswer> {
    use ::aws_smithy_runtime_api::client::http::HttpConnector as _;

    let url = authentication_url(endpoint, request)?;
    let mut authentication = match authentication_request(&url, request) {
        Ok(authentication) => authentication,
        Err(error) => {
            ::tracing::warn!(
                %error,
                "the identity provider's authentication request could not be rendered, so the \
                 test reports no response for it"
            );
            return None;
        }
    };
    // The emulator verifies every request signature, and a method authorized by IAM serves no
    // unsigned caller, so the request carries the one in-process signature this emulator has,
    // under the signing name the invoke endpoint authenticates against.
    if let Err(error) = ::service_common::in_process_sigv4::sign_in_process_request(
        &mut authentication,
        INVOKE_SIGNING_NAME,
        &endpoint_region(&url, request_region),
        ctx,
    )
    .await
    {
        ::tracing::warn!(
            %error,
            "the identity provider's authentication request could not be signed, so the test \
             reports no response for it"
        );
        return None;
    }
    let (parts, body) = authentication.into_parts();
    let dispatched = ::aws_smithy_runtime_api::http::Request::try_from(::http::Request::from_parts(
        parts,
        ::aws_smithy_types::body::SdkBody::from(::bytes::Bytes::from(body)),
    ))
    .ok()?;
    let transport = ctx.callback().http_connector();
    let answered = match ::tokio::time::timeout(ENDPOINT_CEILING, transport.call(dispatched)).await
    {
        Ok(Ok(answered)) => answered,
        // The request could not be dispatched at all, which is a fact about this process
        // rather than an answer from the endpoint: nothing served it.
        Ok(Err(error)) => {
            ::tracing::warn!(
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the identity provider's endpoint could not be reached, so the test reports \
                 no response for it"
            );
            return None;
        }
        // The request WAS made and the endpoint did not answer it within the time one
        // request is given, so there is no status and no body to report.
        Err(_elapsed) => {
            ::tracing::warn!(
                ceiling_secs = ENDPOINT_CEILING.as_secs(),
                "the identity provider's endpoint did not answer the authentication request, \
                 so the test reports no status for it"
            );
            return Some(super::ProviderAnswer::unanswered());
        }
    };
    let status = answered.status().as_u16();
    // Only the prefix one answer may occupy is copied: everything past the cap is dropped,
    // which leaves a long body an answer this reader reports in part rather than an
    // allocation the endpoint chose the size of.
    let body: Vec<u8> = answered
        .body()
        .bytes()
        .unwrap_or_default()
        .iter()
        .take(MAX_RESPONSE_BYTES)
        .copied()
        .collect();
    // A refusal the TRANSPORT itself made is no answer from a provider: this emulator serves
    // nothing at the endpoint the server names, so the request reached no provider at all.
    if ::service_common::in_process_refusal::is_transport_refusal(&body) {
        ::tracing::debug!(
            "this emulator serves nothing at the endpoint the identity provider names, so no \
             authentication request was answered"
        );
        return None;
    }
    Some(super::ProviderAnswer {
        // "The HTTP status code that is the response from your API Gateway": the status the
        // endpoint answered the authentication request with.
        status_code: i32::from(status),
        // "The response that is returned from your API Gateway": the endpoint's own body,
        // reported verbatim, and absent for an endpoint that answered with none.
        response: (!body.is_empty()).then(|| String::from_utf8_lossy(&body).to_string()),
        // "A message that indicates whether the test was successful or not. If an empty
        // string is returned…": the endpoint answered for itself, and its status and body
        // are the outcome, so nothing else is stated about it.
        message: String::new(),
    })
}

/// WHERE the authentication request goes: the authentication resource of ONE user under the
/// endpoint the provider names, carrying the protocol and source address the request named.
///
/// `None` for a `Url` that is no HTTP endpoint — one that parses as no URL at all, names a
/// scheme nothing is served over, or has no path to extend — because such a value addresses
/// no provider to authenticate against.
fn authentication_url(endpoint: &str, request: &super::AuthenticationRequest<'_>) -> Option<Url> {
    let mut url = Url::parse(endpoint).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    {
        let mut path = url.path_segments_mut().ok()?;
        // A `Url` keeps the empty last segment a trailing `/` leaves, which would put an
        // empty segment in the middle of the resource path.
        path.pop_if_empty();
        path.extend([
            SERVERS_SEGMENT,
            request.server_id,
            USERS_SEGMENT,
            request.user_name,
            CONFIG_SEGMENT,
        ]);
    }
    // Each parameter is the REQUEST's own and only for a member it named; a request naming
    // neither carries no query at all rather than an empty one.
    if request.protocol.is_some() || request.source_ip.is_some() {
        let mut query = url.query_pairs_mut();
        if let Some(protocol) = request.protocol {
            query.append_pair(PROTOCOL_PARAMETER, protocol);
        }
        if let Some(source_ip) = request.source_ip {
            query.append_pair(SOURCE_IP_PARAMETER, source_ip);
        }
    }
    Some(url)
}

/// The authentication request itself: the documented verb at [`authentication_url`], the
/// password the caller offered in the header that resource takes it in, and the media type
/// its answer is served in.
///
/// `Err` carries what the `http` types refused, for a caller reporting a request it could not
/// make: every value here comes from the stored provider and the caller's own request, so a
/// value no header or URI admits fails here rather than panicking.
fn authentication_request(
    url: &Url,
    request: &super::AuthenticationRequest<'_>,
) -> Result<::http::Request<Vec<u8>>, ::http::Error> {
    let mut builder = ::http::Request::builder()
        // "GET /servers/{serverId}/users/{username}/config": the authentication resource is
        // READ, so the request carries no body of its own.
        .method(::http::Method::GET)
        .uri(url.as_str())
        .header(::http::header::ACCEPT, APPLICATION_JSON);
    if let Some(password) = request.user_password {
        builder = builder.header(PASSWORD_HEADER, password);
    }
    builder.body(Vec::new())
}

/// The Region the authentication request is signed for: the one the endpoint's own host names
/// when it is an invoke host AWS publishes (`{api-id}.execute-api.{region}.amazonaws.com`),
/// and `request_region` — the Region the calling request was signed for — for every other
/// address, which carries none.
///
/// The same reading the invocation-role verification takes of the two addressings a deployed
/// stage is published at ([`crate::state::authorization`]): a provider in another Region is
/// reached as being in that Region, and the emulator's own endpoint, which names no Region at
/// all, as being in the caller's.
fn endpoint_region(url: &Url, request_region: &str) -> String {
    let labels: Vec<&str> = url.host_str().unwrap_or_default().split('.').collect();
    match labels.as_slice() {
        // `{api-id}.execute-api.{region}.…`: the Region is the label after the one naming
        // the data plane.
        [_api_id, plane, region, ..] if *plane == INVOKE_SIGNING_NAME && !region.is_empty() => {
            (*region).to_string()
        }
        _ => request_region.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `ServerId` satisfying the modelled `^s-([0-9a-f]{17})$`.
    const SERVER: &str = "s-0123456789abcdef0";

    /// One authentication request, as the operation's input names it.
    fn probe(
        user_name: &'static str,
        protocol: Option<&'static str>,
        source_ip: Option<&'static str>,
    ) -> super::super::AuthenticationRequest<'static> {
        super::super::AuthenticationRequest {
            server_id: SERVER,
            user_name,
            user_password: None,
            source_ip,
            protocol,
        }
    }

    /// RULE: the authentication request addresses the one resource a custom identity provider
    /// serves — `/servers/{serverId}/users/{username}/config` under the endpoint the provider
    /// names — carrying the protocol and the source address the request named, and nothing
    /// for a member it did not.
    #[test]
    fn the_authentication_request_addresses_the_documented_resource() {
        /// (case, the provider's Url, the request, the URL invoked)
        type Row = (
            &'static str,
            &'static str,
            super::super::AuthenticationRequest<'static>,
            Option<&'static str>,
        );
        // The rows differ along the addressing of the endpoint (the invoke host AWS
        // publishes, the path form this emulator publishes, a plain-HTTP endpoint on a
        // port), whether the stored Url ends in a `/`, how deep its own path is, which
        // optional members the request named, and what puts a value outside an HTTP
        // endpoint altogether.
        let rows: Vec<Row> = vec![
            (
                "the invoke host AWS publishes, the required members alone",
                "https://abc123.execute-api.us-east-1.amazonaws.com/prod",
                probe("test.user", None, None),
                Some(
                    "https://abc123.execute-api.us-east-1.amazonaws.com/prod/servers/\
                     s-0123456789abcdef0/users/test.user/config",
                ),
            ),
            (
                "the same endpoint with a trailing slash",
                "https://abc123.execute-api.us-east-1.amazonaws.com/prod/",
                probe("test.user", None, None),
                Some(
                    "https://abc123.execute-api.us-east-1.amazonaws.com/prod/servers/\
                     s-0123456789abcdef0/users/test.user/config",
                ),
            ),
            (
                "the path form this emulator publishes, with a protocol",
                "https://127.0.0.1:9297/_aws/execute-api/abc123/test",
                probe("sim2realuser", Some("SFTP"), None),
                Some(
                    "https://127.0.0.1:9297/_aws/execute-api/abc123/test/servers/\
                     s-0123456789abcdef0/users/sim2realuser/config?protocol=SFTP",
                ),
            ),
            (
                "a protocol and a source address",
                "https://127.0.0.1:9297/_aws/execute-api/abc123/test",
                probe("other_user", Some("FTPS"), Some("10.0.0.4")),
                Some(
                    "https://127.0.0.1:9297/_aws/execute-api/abc123/test/servers/\
                     s-0123456789abcdef0/users/other_user/config\
                     ?protocol=FTPS&sourceIp=10.0.0.4",
                ),
            ),
            (
                "a source address alone, on a plain-HTTP endpoint",
                "http://localhost:8080/auth",
                probe("partner.account", None, Some("2001:db8::1")),
                Some(
                    "http://localhost:8080/auth/servers/s-0123456789abcdef0/users/\
                     partner.account/config?sourceIp=2001%3Adb8%3A%3A1",
                ),
            ),
            (
                "an endpoint that names no path of its own",
                "https://idp.example.test",
                probe("test.user", None, None),
                Some(
                    "https://idp.example.test/servers/s-0123456789abcdef0/users/test.user/\
                     config",
                ),
            ),
            (
                "a scheme nothing is served over",
                "ftp://idp.example.test/auth",
                probe("test.user", None, None),
                None,
            ),
            (
                "a value that is no URL at all",
                "not-an-endpoint",
                probe("test.user", None, None),
                None,
            ),
        ];
        for (case, endpoint, request, expected) in rows {
            let actual = authentication_url(endpoint, &request).map(|url| url.to_string());
            assert_eq!(
                actual.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the request is signed for the Region the endpoint's own invoke host names, and
    /// for the Region the calling request was signed for when the address names none.
    #[test]
    fn the_request_is_signed_for_the_region_the_endpoint_names() {
        let request_region = "us-west-2";
        // (case, the provider's Url, the Region the request is signed for)
        let rows: &[(&str, &str, &str)] = &[
            (
                "the invoke host AWS publishes",
                "https://abc123.execute-api.us-east-1.amazonaws.com/prod",
                "us-east-1",
            ),
            (
                "the same host in another Region",
                "https://abc123.execute-api.eu-west-1.amazonaws.com/prod",
                "eu-west-1",
            ),
            (
                "the path form this emulator publishes, which names no Region",
                "https://127.0.0.1:9297/_aws/execute-api/abc123/test",
                request_region,
            ),
            (
                "an endpoint of the caller's own",
                "https://idp.example.test/auth",
                request_region,
            ),
            (
                "a host whose second label is not that data plane's",
                "https://abc123.elsewhere.us-east-1.amazonaws.com/prod",
                request_region,
            ),
        ];
        for (case, endpoint, expected) in rows {
            let url = Url::parse(endpoint).expect("the row's endpoint is a URL");
            let actual = endpoint_region(&url, request_region);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the offered password travels in the header the authentication resource takes it
    /// in, and nowhere else — never in the path or the query a request carries.
    #[test]
    fn the_offered_password_travels_in_its_own_header() {
        let offered = "not-a-real-password";
        let request = super::super::AuthenticationRequest {
            server_id: SERVER,
            user_name: "test.user",
            user_password: Some(offered),
            source_ip: Some("10.0.0.4"),
            protocol: Some("SFTP"),
        };
        let url =
            authentication_url("https://127.0.0.1:9297/_aws/execute-api/abc123/test", &request)
                .expect("the endpoint is an HTTP endpoint");
        let built = authentication_request(&url, &request).expect("the request renders");
        assert_eq!(
            built.headers().get(PASSWORD_HEADER).map(|value| value
                .to_str()
                .expect("the header carries the offered password")),
            Some(offered),
            "case \"a password offered\": expected the password in its own header, got {:?}",
            built.headers()
        );
        assert!(
            !built.uri().to_string().contains(offered),
            "case \"a password offered\": expected no password in the URL, got {:?}",
            built.uri()
        );
        assert_eq!(
            built.method(),
            ::http::Method::GET,
            "case \"a password offered\": expected the documented verb, got {:?}",
            built.method()
        );

        let without = super::super::AuthenticationRequest {
            user_password: None,
            ..request
        };
        let built = authentication_request(&url, &without).expect("the request renders");
        assert!(
            built.headers().get(PASSWORD_HEADER).is_none(),
            "case \"no password offered\": expected no password header at all, got {:?}",
            built.headers()
        );
    }
}
