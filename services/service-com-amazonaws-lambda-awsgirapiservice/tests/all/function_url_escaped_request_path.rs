// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a function URL request whose PATH carries percent-escapes, and for
//! the two places the payload-format-2.0 event reports a path.
//!
//! The rule, one level above the recorded case that filed the issue (a GET for
//! `…/some/pa%20th/x` with a query and a mixed-case header):
//!
//! > A `%HH` sequence is a legal path escape, so a function URL serves a request whose path
//! > carries one exactly as it serves any other, and the event reports that path TWICE:
//! > `rawPath` is the request target as the client WROTE it, byte for byte, while
//! > `requestContext.http.path` is the path it NAMES, with its escapes resolved — the same
//! > division the format makes between the verbatim `rawQueryString` and the decoded
//! > `queryStringParameters`. A `+` is a literal plus in a path, not the space a
//! > form-encoded query writes that way.
//!
//! And, for what the event says about WHERE the request arrived:
//!
//! > The `host` the event reports, and `requestContext.domainName` beside it, name the HOST
//! > of the address this service published the URL at — a domain carries no transport port —
//! > while the url id is reported where the format reports the endpoint's identity, as
//! > `apiId` and `domainPrefix`.
//!
//! The rows of the first test span what the path rule generalises over: no escape at all, an
//! escaped space, an escaped separator, lower-case and upper-case hex digits, a multi-byte
//! character, escaped reserved characters, an escaped percent sign, a literal plus, a `%`
//! that no two hex digits follow, and the URL's own root. The rows of the last test span the
//! addressings a client reaches the published endpoint by, because what the event says about
//! the endpoint is the same however it was addressed; the forms the ADDRESS itself takes — a
//! name, an IPv4 literal, a bracketed IPv6 literal, with and without a port — are spanned by
//! the unit rows of `state::function_url`, because which address this emulator can bind is
//! not the test's to choose.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_url_data_plane::{
    given_configured_url_at, given_function, grant, router_on, send, statement, Endpoint,
};

/// The control-plane endpoint of this service, which a client that resolves AWS's own hosts
/// reaches it at, as the shared harness addresses it.
const SERVICE_HOST: &str = "lambda.us-east-1.amazonaws.com";

/// The emulator's own loopback endpoint, which a client that resolves no AWS hostname is
/// configured with — and which the URL created through it is therefore published at.
const OWN_ENDPOINT: &str = "127.0.0.1:9297";

/// The handler every row runs: it answers the members of the event this suite is about, so
/// every assertion below is about a document the RUN produced.
fn echoing_handler() -> String {
    r#"
import json


def lambda_handler(event, context):
    context_ = event["requestContext"]
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps(
            {
                "rawPath": event["rawPath"],
                "path": context_["http"]["path"],
                "rawQueryString": event["rawQueryString"],
                "queryStringParameters": event.get("queryStringParameters", {}),
                "headers": event["headers"],
                "domainName": context_["domainName"],
                "domainPrefix": context_["domainPrefix"],
                "apiId": context_["apiId"],
            }
        ),
    }
"#
    .to_string()
}

/// Create a PUBLIC function URL on `function` through a request addressed at `host`, and
/// answer the endpoint it announces.
///
/// `AuthType=NONE` bypasses IAM authentication, and the function's resource-based policy is
/// what admits the anonymous caller — so a public endpoint is that `AuthType` plus the grant
/// to the wildcard principal, both through the shared harness.
async fn given_public_url(router: &MultiServiceRouter, function: &str, host: &str) -> Endpoint {
    let endpoint =
        given_configured_url_at(router, host, function, None, "NONE", &Value::Null).await;
    grant(
        router,
        function,
        None,
        &statement("public-url", "lambda:InvokeFunctionUrl", "*", "NONE"),
    )
    .await;
    endpoint
}

/// The document one GET below the announced endpoint answered.
async fn fetched(
    router: &MultiServiceRouter,
    case: &str,
    endpoint: &Endpoint,
    within: &str,
    headers: &[(&str, &str)],
) -> Value {
    let answer = send(
        router,
        "GET",
        &endpoint.host,
        &format!("{}{within}", endpoint.base_path),
        &[],
        headers,
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: the function URL must serve the request, got {}",
        answer.summary()
    );
    answer.json()
}

/// The member `member` of one answered document, as text.
fn text(case: &str, document: &Value, member: &str) -> String {
    document
        .get(member)
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the handler must report {member}, got {document}")
        })
}

/// A percent-escaped request path is SERVED, and the event reports it raw and resolved.
#[tokio::test]
async fn an_escaped_request_path_is_served_and_reported_raw_and_resolved() {
    let engine = std::sync::Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&router, "url-escaped-path", &echoing_handler()).await;
    let endpoint = given_public_url(&router, "url-escaped-path", SERVICE_HOST).await;

    // (case, the path below the endpoint as the client writes it, the `rawPath` the event
    // must carry, the `requestContext.http.path` it must carry)
    let rows: &[(&str, &str, &str, &str)] = &[
        ("no escape at all", "plain/path", "/plain/path", "/plain/path"),
        (
            "the recorded case: an escaped space",
            "some/pa%20th/x",
            "/some/pa%20th/x",
            "/some/pa th/x",
        ),
        (
            "an escaped separator, which is no separator",
            "a%2Fb/c",
            "/a%2Fb/c",
            "/a/b/c",
        ),
        (
            "the same escape in lower-case hex",
            "a%2fb/c",
            "/a%2fb/c",
            "/a/b/c",
        ),
        (
            "a multi-byte character",
            "caf%C3%A9/men%C3%BC",
            "/caf%C3%A9/men%C3%BC",
            "/café/menü",
        ),
        (
            "escaped reserved characters",
            "items/%23one%3Ftwo%26three",
            "/items/%23one%3Ftwo%26three",
            "/items/#one?two&three",
        ),
        (
            "an escaped percent sign",
            "discount/100%25",
            "/discount/100%25",
            "/discount/100%",
        ),
        (
            "a literal plus, which is a plus in a path and not a space",
            "a+b/c",
            "/a+b/c",
            "/a+b/c",
        ),
        (
            "a `%` that no two hex digits follow, which is no escape",
            "50%-off",
            "/50%-off",
            "/50%-off",
        ),
        ("the endpoint's own root", "", "/", "/"),
    ];

    for (case, within, raw_path, resolved) in rows {
        let document = fetched(&router, case, &endpoint, within, &[]).await;
        assert_eq!(
            text(case, &document, "rawPath"),
            *raw_path,
            "case {case:?}: expected the rawPath {raw_path:?} the client wrote, got {document}"
        );
        assert_eq!(
            text(case, &document, "path"),
            *resolved,
            "case {case:?}: expected requestContext.http.path {resolved:?}, got {document}"
        );
    }
}

/// Beside the escaped path, one request's query is reported verbatim and decoded, and its
/// header names arrive lower-cased.
#[tokio::test]
async fn the_query_is_reported_verbatim_and_decoded_and_header_names_arrive_lower_cased() {
    let engine = std::sync::Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&router, "url-escaped-query", &echoing_handler()).await;
    let endpoint = given_public_url(&router, "url-escaped-query", SERVICE_HOST).await;

    let case = "the recorded case: an escaped path with a repeated and an escaped query \
                parameter, under a mixed-case header";
    let document = fetched(
        &router,
        case,
        &endpoint,
        "some/pa%20th/x?a=1&a=2&b=%20y&e=",
        &[("X-Mixed-Case", "kept")],
    )
    .await;
    assert_eq!(
        text(case, &document, "rawQueryString"),
        "a=1&a=2&b=%20y&e=",
        "case {case:?}: expected the query string as written, got {document}"
    );
    assert_eq!(
        document.get("queryStringParameters"),
        Some(&json!({ "a": "1,2", "b": " y", "e": "" })),
        "case {case:?}: expected the decoded parameters, got {document}"
    );
    let headers = document
        .get("headers")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("case {case:?}: the handler must report headers, got {document}"));
    assert_eq!(
        headers.get("x-mixed-case"),
        Some(&json!("kept")),
        "case {case:?}: expected every header name lower-cased, got {headers:?}"
    );
    assert!(
        !headers.keys().any(|name| name != &name.to_ascii_lowercase()),
        "case {case:?}: expected every header name lower-cased, got {headers:?}"
    );
}

/// The event names the HOST of the address the URL was published at — never the port it is
/// served on — and reports the url id as the endpoint's identity.
///
/// The address itself is the emulator's to choose (it is the one the URL is SERVED at), so
/// what every row asserts is the relation between the published value and the event: the
/// host the event reports is the published authority with its port removed, and the url id
/// is reported where the format reports the endpoint's identity. The rows span the
/// addressings a client reaches the endpoint by, because the answer is the URL's own however
/// it was addressed. The forms an address itself takes — a name, a literal, a bracketed IPv6
/// literal — are spanned by the unit rows of `state::function_url`.
#[tokio::test]
async fn the_event_names_the_published_endpoints_host_and_the_url_id() {
    let engine = std::sync::Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&router, "url-published-host", &echoing_handler()).await;
    // A create addressed at the emulator's own loopback endpoint, which is the endpoint a
    // client that resolves no AWS hostname is configured with.
    let endpoint = given_public_url(&router, "url-published-host", OWN_ENDPOINT).await;
    let published = endpoint.host.clone();
    let (host, port) = published
        .rsplit_once(':')
        .filter(|(_host, port)| port.parse::<u16>().is_ok())
        .unwrap_or_else(|| {
            panic!("the URL must be published at a `host:port` a client dials, got {published:?}")
        });
    assert!(
        !host.is_empty(),
        "the published address must name a host, got {published:?}"
    );
    assert_eq!(
        host,
        endpoint.domain(),
        "the host the event must report is the published authority's own host"
    );

    // (case, the path below the endpoint the request addresses)
    let rows: &[(&str, String)] = &[
        ("the endpoint's own root", endpoint.base_path.clone()),
        ("a path below it", format!("{}probe/one", endpoint.base_path)),
        (
            "an escaped path below it",
            format!("{}pa%20th", endpoint.base_path),
        ),
    ];
    for (case, addressed) in rows {
        let answer = send(&router, "GET", &published, addressed, &[], &[]).await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: the function URL must serve the request, got {}",
            answer.summary()
        );
        let document = answer.json();
        // (the member the event reports the endpoint's host in, the value it carries)
        let reported: &[(&str, String)] = &[
            ("domainName", text(case, &document, "domainName")),
            (
                "the host header",
                document
                    .get("headers")
                    .and_then(|headers| headers.get("host"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: the event must carry a host header, got {document}")
                    }),
            ),
        ];
        for (member, value) in reported {
            assert_eq!(
                value, host,
                "case {case:?}: expected {member} to name the published host {host:?}, got \
                 {value:?}"
            );
            assert!(
                !value.contains(&format!(":{port}")),
                "case {case:?}: expected {member} to carry no transport port, got {value:?}"
            );
        }
        for member in ["apiId", "domainPrefix"] {
            assert_eq!(
                text(case, &document, member),
                endpoint.url_id,
                "case {case:?}: expected {member} to be the url id {:?}, got {document}",
                endpoint.url_id
            );
        }
    }
}
