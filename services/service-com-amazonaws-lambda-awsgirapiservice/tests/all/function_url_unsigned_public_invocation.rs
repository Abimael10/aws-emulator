// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the UNSIGNED client of a function URL whose `AuthType` is `NONE`, on a
//! router that VERIFIES request signatures.
//!
//! The rule under test: the function-URL data plane admits a request by the URL's OWN
//! `AuthType` rather than by the control plane's signature verification, so an endpoint
//! configured `NONE` — "Bypass IAM authentication to create a public endpoint" — serves a
//! request that presents no SigV4 credential at all the function's own response, and the
//! address the event attributes that call to (`requestContext.http.sourceIp`) is the one its
//! CONNECTION came from and never a `X-Forwarded-For` the unsigned client wrote.
//!
//! Both halves are asserted on the SAME request, because they are one contract: a caller that
//! is refused for its missing signature never reaches the handler, so nothing of what the
//! handler is told about the call is observable at all. A browser, a webhook sender and every
//! other stock client of such an endpoint signs nothing, and the answer each of them reads is
//! the handler's.
//!
//! Every row drives a real HTTP request through a router with request authentication ON — as
//! the emulator runs when it is started with it — against a function whose deployment package
//! RUNS, so what is asserted is what the HANDLER was handed. The rows differ along what the
//! rule generalises over: the method, path, query and body the unsigned request carries, the
//! form of the forwarded list it writes (none, one claimed address, a chain of hops, the header
//! sent twice, an address of the other family, a value that is no address), and the addressing
//! the endpoint is reached by.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_url_data_plane::{
    given_configured_url, given_function, grant, router_on, send, statement, Answer, Endpoint,
};

/// The action a public function URL's own invocation is granted under: what makes a `NONE`
/// endpoint public is the function's resource-based policy granting it to everyone.
const INVOKE_FUNCTION_URL: &str = "lambda:InvokeFunctionUrl";

/// The header a client's claim about the hops ahead of this one travels in.
const FORWARDED_FOR: &str = "x-forwarded-for";

/// The header a Lambda failure names its error class in, which no answer of the handler's own
/// carries.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The address every request below is attributed to: this emulator is reached in process and
/// over loopback, so that is where the connection each row opens came from. No row opens one
/// from anywhere else — what the rows vary is the header they write about other hops.
const CONNECTION_ADDRESS: &str = "127.0.0.1";

/// The emulator's own endpoint, which a client that resolves no AWS hostname reaches.
const OWN_ENDPOINT: &str = "127.0.0.1:9297";

/// The reserved path segment a data plane is named behind on that endpoint
/// (`service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE`), and the label naming
/// this plane under it.
const EMULATOR_NAMESPACE: &str = "_aws";
/// See [`EMULATOR_NAMESPACE`].
const PLANE_LABEL: &str = "lambda-url";

/// The handler every function below runs: it answers what the EVENT said about the call — where
/// it came from, which caller the endpoint authenticated, and the request it was turned from —
/// so every value this suite asserts is one the handler was handed.
fn handler() -> String {
    r#"
import json


def lambda_handler(event, context):
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps(
            {
                "sourceIp": event["requestContext"]["http"]["sourceIp"],
                "forwardedFor": event["headers"].get("x-forwarded-for"),
                "method": event["requestContext"]["http"]["method"],
                "rawPath": event["rawPath"],
                "rawQueryString": event["rawQueryString"],
                "body": event.get("body") or "",
                "authorizer": event["requestContext"].get("authorizer"),
            }
        ),
    }
"#
    .to_string()
}

/// What the handler reported about one call.
struct Reported {
    source_ip: String,
    forwarded_for: Option<String>,
    method: String,
    raw_path: String,
    raw_query: String,
    body: String,
    /// `requestContext.authorizer`, which a `NONE` URL carries none of: nothing authenticated
    /// the caller, so no identity may be reported for it.
    authorizer: Value,
}

/// Read what the handler answered, insisting the answer IS the handler's: the status it
/// described, no failure class of the front door's, and the members it reported.
fn reported(case: &str, answer: &Answer) -> Reported {
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: an unsigned caller of a NONE function URL must be served the handler's \
         answer, got {}",
        answer.summary()
    );
    assert_eq!(
        answer.header(ERROR_TYPE_HEADER),
        None,
        "case {case:?}: expected no failure class at all, got {:?} in {}",
        answer.header(ERROR_TYPE_HEADER),
        answer.summary()
    );
    let payload = answer.json();
    let text = |member: &str| -> Option<String> {
        payload
            .get(member)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let required = |member: &str| -> String {
        text(member).unwrap_or_else(|| {
            panic!("case {case:?}: the handler must report {member}, got {payload}")
        })
    };
    Reported {
        source_ip: required("sourceIp"),
        forwarded_for: text("forwardedFor"),
        method: required("method"),
        raw_path: required("rawPath"),
        raw_query: required("rawQueryString"),
        body: required("body"),
        authorizer: payload.get("authorizer").cloned().unwrap_or(Value::Null),
    }
}

/// A router over the same engine that VERIFIES every request signature, as the emulator does
/// when it is started with request authentication on.
fn authenticating_router(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = router_on(engine);
    router.set_request_authentication(true);
    router
}

/// A function with a `NONE` function URL granted to the public, answering the endpoint it
/// published. Built over the router that does not verify signatures, so this setup says nothing
/// about what the rows below are admitted by.
async fn given_public_url(router: &MultiServiceRouter, function: &str) -> Endpoint {
    given_function(router, function, &handler()).await;
    let endpoint = given_configured_url(router, function, None, "NONE", &Value::Null).await;
    grant(
        router,
        function,
        None,
        &statement("public-url", INVOKE_FUNCTION_URL, "*", "NONE"),
    )
    .await;
    endpoint
}

/// One row: the unsigned request a stock client sends, and the forwarded list it writes.
struct Row {
    case: &'static str,
    method: &'static str,
    /// The path within the function URL the client asks for.
    path: &'static str,
    /// The query it carries, empty for none.
    query: &'static str,
    /// The body it sends, empty for none.
    body: &'static str,
    /// The media type the body is sent under, empty for a request sending none.
    media_type: &'static str,
    /// One `X-Forwarded-For` field per entry, in the order the client wrote them.
    claimed: &'static [&'static str],
    /// The value `headers['x-forwarded-for']` must carry: the client's own fields joined the
    /// way a repeated header name is joined, and the connection's address for a client that
    /// wrote none.
    forwarded_for: &'static str,
}

/// The rows, spanning the requests an unsigned client makes and the forwarded lists it writes.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a plain GET on the endpoint's own root, claiming nothing",
            method: "GET",
            path: "/",
            query: "",
            body: "",
            media_type: "",
            claimed: &[],
            forwarded_for: CONNECTION_ADDRESS,
        },
        Row {
            case: "a GET on a nested path carrying a query",
            method: "GET",
            path: "/items/one",
            query: "page=2&via=browser",
            body: "",
            media_type: "",
            claimed: &[],
            forwarded_for: CONNECTION_ADDRESS,
        },
        Row {
            case: "a GET claiming one other address",
            method: "GET",
            path: "/items/one",
            query: "",
            body: "",
            media_type: "",
            claimed: &["203.0.113.9"],
            forwarded_for: "203.0.113.9",
        },
        Row {
            case: "a GET claiming a chain of hops",
            method: "GET",
            path: "/items/two",
            query: "",
            body: "",
            media_type: "",
            claimed: &["198.51.100.7, 203.0.113.9"],
            forwarded_for: "198.51.100.7, 203.0.113.9",
        },
        Row {
            case: "a GET writing the forwarded header twice",
            method: "GET",
            path: "/items/three",
            query: "",
            body: "",
            media_type: "",
            claimed: &["203.0.113.9", "198.51.100.7"],
            forwarded_for: "203.0.113.9,198.51.100.7",
        },
        Row {
            case: "a GET claiming an address of the other family",
            method: "GET",
            path: "/items/four",
            query: "",
            body: "",
            media_type: "",
            claimed: &["2001:db8::1"],
            forwarded_for: "2001:db8::1",
        },
        Row {
            case: "a GET claiming a value that is no address at all",
            method: "GET",
            path: "/items/five",
            query: "",
            body: "",
            media_type: "",
            claimed: &["not-an-address"],
            forwarded_for: "not-an-address",
        },
        Row {
            case: "a webhook POST carrying a body and claiming its sender's address",
            method: "POST",
            path: "/hooks/delivery",
            query: "",
            body: r#"{"event":"delivered"}"#,
            media_type: "application/json",
            claimed: &["203.0.113.9"],
            forwarded_for: "203.0.113.9",
        },
        Row {
            case: "a PUT on a percent-encoded segment",
            method: "PUT",
            path: "/my%20items/one",
            query: "replace=yes",
            body: "replacement",
            media_type: "text/plain",
            claimed: &[],
            forwarded_for: CONNECTION_ADDRESS,
        },
        Row {
            case: "a DELETE claiming the connection's own address as well",
            method: "DELETE",
            path: "/items/one",
            query: "",
            body: "",
            media_type: "",
            claimed: &[CONNECTION_ADDRESS],
            forwarded_for: CONNECTION_ADDRESS,
        },
    ]
}

/// One unsigned request of `row` to `endpoint`, answering what the handler reported.
async fn probe(router: &MultiServiceRouter, endpoint: &Endpoint, row: &Row) -> Reported {
    let target = if row.query.is_empty() {
        row.path.to_string()
    } else {
        format!("{path}?{query}", path = row.path, query = row.query)
    };
    let mut headers: Vec<(&str, &str)> = row
        .claimed
        .iter()
        .map(|value| (FORWARDED_FOR, *value))
        .collect();
    if !row.media_type.is_empty() {
        headers.push(("content-type", row.media_type));
    }
    // Nothing else: no `Authorization`, no `X-Amz-Date`, no session token. This is the request
    // a stock client sends.
    let answer = send(
        router,
        row.method,
        &endpoint.host,
        &target,
        row.body.as_bytes(),
        &headers,
    )
    .await;
    reported(row.case, &answer)
}

/// RULE: a `NONE` function URL applies its OWN auth type to the request, so a client that
/// presents no SigV4 credential is served the function's answer — and the call is attributed to
/// the address its CONNECTION came from, whatever forwarded list the client wrote.
#[tokio::test]
async fn an_unsigned_client_of_a_public_function_url_is_served_by_the_function() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let setup = router_on(&engine);
    let endpoint = given_public_url(&setup, "url-unsigned-public").await;
    let router = authenticating_router(&engine);

    // Every row is the SAME client making another call, so one address is what they all have
    // to be attributed to.
    let mut attributed: Vec<(&str, String)> = Vec::new();
    for row in rows() {
        let case = row.case;
        let reported = probe(&router, &endpoint, &row).await;
        assert_eq!(
            reported.source_ip, CONNECTION_ADDRESS,
            "case {case:?}: expected sourceIp {CONNECTION_ADDRESS:?} (the address the connection \
             came from), got {:?}",
            reported.source_ip
        );
        assert_eq!(
            reported.forwarded_for.as_deref(),
            Some(row.forwarded_for),
            "case {case:?}: expected headers['{FORWARDED_FOR}'] {:?}, got {:?}",
            row.forwarded_for,
            reported.forwarded_for
        );
        // The handler ran on THIS request, and on a call nothing authenticated: a `NONE` URL
        // reports no IAM caller, because nothing verified one.
        assert_eq!(
            reported.method, row.method,
            "case {case:?}: expected the handler to be told the method {:?}, got {:?}",
            row.method, reported.method
        );
        assert_eq!(
            reported.raw_path, row.path,
            "case {case:?}: expected the handler to be told the path {:?}, got {:?}",
            row.path, reported.raw_path
        );
        assert_eq!(
            reported.raw_query, row.query,
            "case {case:?}: expected the handler to be told the query {:?}, got {:?}",
            row.query, reported.raw_query
        );
        assert_eq!(
            reported.body, row.body,
            "case {case:?}: expected the handler to be told the body {:?}, got {:?}",
            row.body, reported.body
        );
        assert_eq!(
            reported.authorizer,
            Value::Null,
            "case {case:?}: expected no authorizer for a caller nothing authenticated, got {:?}",
            reported.authorizer
        );
        attributed.push((case, reported.source_ip.clone()));
    }
    let first = attributed
        .first()
        .map(|(_, address)| address.clone())
        .unwrap_or_default();
    for (case, address) in &attributed {
        assert_eq!(
            address, &first,
            "case {case:?}: sourceIp must be stable across the calls of one client, got \
             {address:?} after {first:?}"
        );
    }
}

/// The host and path one addressing reaches `endpoint` at, for the path `within` the URL.
fn addressed(endpoint: &Endpoint, addressing: &str, within: &str) -> (String, String) {
    match addressing {
        "the published host" => (endpoint.host.clone(), within.to_string()),
        "the local-endpoint path" => (
            OWN_ENDPOINT.to_string(),
            format!(
                "/{EMULATOR_NAMESPACE}/{PLANE_LABEL}/{id}{within}",
                id = endpoint.url_id
            ),
        ),
        "path-style on the emulator's own endpoint" => (
            OWN_ENDPOINT.to_string(),
            format!("/{id}{within}", id = endpoint.url_id),
        ),
        other => panic!("no such addressing: {other}"),
    }
}

/// RULE, over the addressings: which auth type admits a caller is the URL's own, so every
/// addressing a `NONE` function URL resolves under serves the unsigned client the same way —
/// the handler's answer, and the connection's address for the call.
#[tokio::test]
async fn every_addressing_of_a_public_function_url_serves_the_unsigned_client() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let setup = router_on(&engine);
    let endpoint = given_public_url(&setup, "url-unsigned-addressed").await;
    let router = authenticating_router(&engine);

    for addressing in [
        "the published host",
        "the local-endpoint path",
        "path-style on the emulator's own endpoint",
    ] {
        for (label, claimed) in [
            ("claiming nothing", Vec::new()),
            ("claiming another address", vec!["203.0.113.9"]),
        ] {
            let case = format!("{addressing}, {label}");
            let (host, path) = addressed(&endpoint, addressing, "/probe");
            let headers: Vec<(&str, &str)> = claimed
                .iter()
                .map(|value| (FORWARDED_FOR, *value))
                .collect();
            let answer = send(&router, "GET", &host, &path, &[], &headers).await;
            let reported = reported(&case, &answer);
            assert_eq!(
                reported.source_ip, CONNECTION_ADDRESS,
                "case {case:?}: expected sourceIp {CONNECTION_ADDRESS:?} (the address the \
                 connection came from), got {:?}",
                reported.source_ip
            );
            assert_eq!(
                reported.raw_path, "/probe",
                "case {case:?}: expected the handler to be told the path within the URL, got {:?}",
                reported.raw_path
            );
        }
    }
}

/// RULE, read the other way round: the auth type the URL was configured with is what decides,
/// so the same unsigned client reaching an `AWS_IAM` endpoint is refused before the function
/// runs — Lambda's own front-door `403`, with none of the handler's answer.
#[tokio::test]
async fn the_same_unsigned_client_is_refused_by_an_aws_iam_function_url() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let setup = router_on(&engine);
    let public = given_public_url(&setup, "url-unsigned-both-public").await;
    given_function(&setup, "url-unsigned-both-iam", &handler()).await;
    let restricted = given_configured_url(
        &setup,
        "url-unsigned-both-iam",
        None,
        "AWS_IAM",
        &Value::Null,
    )
    .await;
    grant(
        &setup,
        "url-unsigned-both-iam",
        None,
        &json!({
            "StatementId": "account-url",
            "Action": INVOKE_FUNCTION_URL,
            "Principal": "*",
            "FunctionUrlAuthType": "AWS_IAM",
        }),
    )
    .await;
    let router = authenticating_router(&engine);

    // The public endpoint: served, by the handler.
    let served = send(&router, "GET", &public.host, "/probe", &[], &[]).await;
    let reported = reported("the public endpoint", &served);
    assert_eq!(
        reported.source_ip, CONNECTION_ADDRESS,
        "case \"the public endpoint\": expected sourceIp {CONNECTION_ADDRESS:?}, got {:?}",
        reported.source_ip
    );

    // The restricted endpoint, reached by the very same unsigned client: refused, with nothing
    // of the handler's answer in it.
    let refused = send(&router, "GET", &restricted.host, "/probe", &[], &[]).await;
    assert_eq!(
        refused.status,
        403,
        "case \"the AWS_IAM endpoint\": an unsigned caller must be refused, got {}",
        refused.summary()
    );
    assert!(
        !refused.text().contains("sourceIp"),
        "case \"the AWS_IAM endpoint\": expected none of the handler's answer, got {}",
        refused.summary()
    );
}
