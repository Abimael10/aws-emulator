// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ADDRESS a function URL attributes a call to, and for the
//! forwarded list the handler is handed beside it.
//!
//! The rule under test: `requestContext.http.sourceIp` of the payload-format-2.0 event is the
//! address of the TCP connection the request arrived on, so it is the same address for every
//! request one client makes and no header the client writes can change it; the client's own
//! `X-Forwarded-For` travels on to the handler verbatim among `headers`, and the front door
//! writes the `X-Forwarded-*` family itself — the connection's address, the scheme a function
//! URL is reached over and that scheme's own port — only for a request that carried none of
//! that name. The tracing header beside them is the one name the front door STATES: the chain
//! the invocation runs under, the same value the run's environment reports.
//!
//! The rows span the forms a client's own forwarded list takes (none at all, one claimed
//! address, a chain of hops, the header sent twice, a value that is no address at all, an empty
//! field, an address of the other family) against both `AuthType`s a URL is served under, since
//! which caller is admitted decides nothing about where the call came from. Every row drives a
//! real HTTP request through the router against a function whose deployment package RUNS, so
//! the address asserted is the one the HANDLER was told.

use std::sync::Arc;

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::function_url_data_plane::{
    given_configured_url, given_function, grant, router_on, send, statement, Endpoint,
};

/// A SigV4 credential scope naming this service's signing name (`lambda`) in `us-east-1`,
/// which is what a SIGNED request to a function URL presents. Nothing in it is verified — the
/// harness turns request authentication off — so what it decides here is only that the request
/// presents a credential at all.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// The action a function URL's own invocation is granted under, for the `NONE` endpoint whose
/// callers the function's resource-based policy admits.
const INVOKE_FUNCTION_URL: &str = "lambda:InvokeFunctionUrl";

/// The header a client's claim about the hops ahead of this one travels in.
const FORWARDED_FOR: &str = "x-forwarded-for";

/// The address a request this emulator observed no connection address for came from: this
/// emulator is reached in process and over the loopback interface, so that is where such a
/// request arrived from. It is what every row below must report, because none of them opens a
/// connection from anywhere else — the point of the rule is that the rows differ only in the
/// header they write.
const CONNECTION_ADDRESS: &str = "127.0.0.1";

/// The handler every function below runs: it answers what the EVENT said about where the call
/// came from, and the chain its own ENVIRONMENT says it runs under, so a value this suite
/// asserts is one the handler was handed.
fn handler() -> String {
    r#"
import json
import os


def lambda_handler(event, context):
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps(
            {
                "sourceIp": event["requestContext"]["http"]["sourceIp"],
                "forwardedFor": event["headers"].get("x-forwarded-for"),
                "forwardedProto": event["headers"].get("x-forwarded-proto"),
                "forwardedPort": event["headers"].get("x-forwarded-port"),
                "traceHeader": event["headers"].get("x-amzn-trace-id"),
                "traceEnvironment": os.environ.get("_X_AMZN_TRACE_ID"),
                "domainName": event["requestContext"]["domainName"],
            }
        ),
    }
"#
    .to_string()
}

/// What the handler reported for one request.
struct Reported {
    /// `requestContext.http.sourceIp`.
    source_ip: String,
    /// `headers['x-forwarded-for']`, or `None` for an event carrying none.
    forwarded_for: Option<String>,
    /// `headers['x-forwarded-proto']`, or `None` for an event carrying none.
    forwarded_proto: Option<String>,
    /// `headers['x-forwarded-port']`, or `None` for an event carrying none.
    forwarded_port: Option<String>,
    /// `headers['x-amzn-trace-id']`: the chain the event says the invocation runs under.
    trace_header: Option<String>,
    /// `_X_AMZN_TRACE_ID`: the chain the RUN's own environment reports.
    trace_environment: Option<String>,
    /// `requestContext.domainName`: the endpoint the forwarded port must belong to.
    domain_name: String,
}

/// One GET to `endpoint`, carrying one `X-Forwarded-For` field per entry of `claimed`, the
/// headers `also` names, and a credential when `credentialled`, answering what the handler
/// reported.
async fn probe(
    router: &MultiServiceRouter,
    case: &str,
    endpoint: &Endpoint,
    claimed: &[&str],
    also: &[(&str, &str)],
    credentialled: bool,
) -> Reported {
    let mut headers: Vec<(&str, &str)> = claimed
        .iter()
        .map(|value| (FORWARDED_FOR, *value))
        .collect();
    headers.extend(also.iter().copied());
    if credentialled {
        headers.push(("authorization", AUTHORIZATION));
        headers.push(("x-amz-date", "20260101T000000Z"));
    }
    let answer = send(router, "GET", &endpoint.host, "/probe", &[], &headers).await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: the function URL must serve the request, got {}",
        answer.summary()
    );
    let reported: Value = answer.json();
    let source_ip = reported
        .get("sourceIp")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the handler must report a sourceIp, got {reported}")
        })
        .to_string();
    let text = |member: &str| -> Option<String> {
        reported
            .get(member)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    Reported {
        source_ip,
        forwarded_for: text("forwardedFor"),
        forwarded_proto: text("forwardedProto"),
        forwarded_port: text("forwardedPort"),
        trace_header: text("traceHeader"),
        trace_environment: text("traceEnvironment"),
        domain_name: text("domainName").unwrap_or_else(|| {
            panic!("case {case:?}: the handler must report a domainName, got {reported}")
        }),
    }
}

/// The scheme a function URL is reached over, which the front door states for every request it
/// serves: the scheme the announced `FunctionUrl` carries.
const ENDPOINT_SCHEME: &str = "https";

/// The port of that scheme, which the front door states beside it: a function URL endpoint is
/// published under no port of its own, and this is the port such an address is reached at.
const ENDPOINT_PORT: &str = "443";

/// Assert the scheme and the port one answer reported: both are the ENDPOINT's own, whatever
/// the request carried and whatever address this process serves that endpoint at — which the
/// event states separately, as its `domainName`.
fn assert_endpoint_hop(case: &str, reported: &Reported) {
    assert_eq!(
        reported.forwarded_proto.as_deref(),
        Some(ENDPOINT_SCHEME),
        "case {case:?}: expected the forwarded scheme {ENDPOINT_SCHEME:?}, got {:?}",
        reported.forwarded_proto
    );
    assert_eq!(
        reported.forwarded_port.as_deref(),
        Some(ENDPOINT_PORT),
        "case {case:?}: expected the forwarded port {ENDPOINT_PORT:?} of the endpoint {:?}, \
         got {:?}",
        reported.domain_name,
        reported.forwarded_port
    );
}

/// One row: the client's own forwarded fields, and the list the handler must be handed.
struct Row {
    case: &'static str,
    /// One `X-Forwarded-For` field per entry, in the order the client wrote them.
    claimed: &'static [&'static str],
    /// The value `headers['x-forwarded-for']` must carry: the client's own fields joined the
    /// way the format joins a repeated header name, and the connection's address for a request
    /// that wrote none.
    forwarded_for: &'static str,
}

/// The rows, spanning the forms a client's own forwarded list takes.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no forwarded list at all",
            claimed: &[],
            forwarded_for: CONNECTION_ADDRESS,
        },
        Row {
            case: "one claimed address",
            claimed: &["203.0.113.9"],
            forwarded_for: "203.0.113.9",
        },
        Row {
            case: "a chain of claimed hops",
            claimed: &["198.51.100.7, 203.0.113.9"],
            forwarded_for: "198.51.100.7, 203.0.113.9",
        },
        Row {
            case: "the header sent twice",
            claimed: &["203.0.113.9", "198.51.100.7"],
            forwarded_for: "203.0.113.9,198.51.100.7",
        },
        Row {
            case: "a claimed value that is no address",
            claimed: &["not-an-address"],
            forwarded_for: "not-an-address",
        },
        Row {
            case: "an empty field",
            claimed: &[""],
            forwarded_for: "",
        },
        Row {
            case: "a claimed address of the other family",
            claimed: &["2001:db8::1"],
            forwarded_for: "2001:db8::1",
        },
        Row {
            case: "the connection's own address claimed as well",
            claimed: &[CONNECTION_ADDRESS],
            forwarded_for: CONNECTION_ADDRESS,
        },
    ]
}

/// RULE: the address a function URL event attributes a call to is the one its CONNECTION came
/// from — identical for every request of one client, whatever `X-Forwarded-For` it writes —
/// while the forwarded list the handler is handed is the client's own, verbatim, and is written
/// by the front door only when the client sent none.
#[tokio::test]
async fn the_reported_address_is_the_connections_and_never_the_clients_header() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    // Both `AuthType`s, because which caller an endpoint admits decides nothing about where
    // the call came from: the `AWS_IAM` URL is served only to a request presenting a
    // credential, and the `NONE` URL to the anonymous caller its policy admits. One function
    // per endpoint, because a function carries one URL configuration per qualifier.
    for (auth_type, function, credentialled) in [
        ("AWS_IAM", "url-source-address-iam", true),
        ("NONE", "url-source-address-public", false),
    ] {
        given_function(&router, function, &handler()).await;
        let endpoint = given_configured_url(&router, function, None, auth_type, &Value::Null).await;
        if auth_type == "NONE" {
            grant(
                &router,
                function,
                None,
                &statement("public-url", INVOKE_FUNCTION_URL, "*", "NONE"),
            )
            .await;
        }
        // Every row of one endpoint is the SAME client making another call, so the address
        // they all report has to be one address.
        let mut reported_addresses: Vec<(&str, String)> = Vec::new();
        for row in rows() {
            let case = format!("{auth_type}: {}", row.case);
            let reported =
                probe(&router, &case, &endpoint, row.claimed, &[], credentialled).await;
            assert_eq!(
                reported.source_ip, CONNECTION_ADDRESS,
                "case {case:?}: expected sourceIp {CONNECTION_ADDRESS:?} (the address the \
                 connection came from), got {:?}",
                reported.source_ip
            );
            assert_eq!(
                reported.forwarded_for.as_deref(),
                Some(row.forwarded_for),
                "case {case:?}: expected headers['{FORWARDED_FOR}'] {:?}, got {:?}",
                row.forwarded_for,
                reported.forwarded_for
            );
            reported_addresses.push((row.case, reported.source_ip.clone()));
            // The hop the front door terminated is the ENDPOINT's, in every row: the scheme a
            // function URL is reached over and the port of the endpoint the event named.
            assert_endpoint_hop(&case, &reported);
        }
        let first = reported_addresses
            .first()
            .map(|(_, address)| address.clone())
            .unwrap_or_default();
        for (case, address) in &reported_addresses {
            assert_eq!(
                address, &first,
                "case {auth_type}: {case:?}: sourceIp must be stable across calls from one \
                 client, got {address:?} after {first:?}"
            );
        }
    }
}

/// RULE, read the other way round: the claim a client writes reaches the handler as a header
/// and nothing else — two calls one client makes, one of them claiming another address, are
/// attributed to the same place, and the claim is still there for the handler to read.
#[tokio::test]
async fn a_claimed_address_reaches_the_handler_without_moving_the_call() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&router, "url-source-claim", &handler()).await;
    let endpoint =
        given_configured_url(&router, "url-source-claim", None, "AWS_IAM", &Value::Null).await;

    let plain = probe(&router, "a plain signed call", &endpoint, &[], &[], true).await;
    let claiming = probe(
        &router,
        "the same client claiming another address",
        &endpoint,
        &["203.0.113.9"],
        &[],
        true,
    )
    .await;

    assert_eq!(
        plain.source_ip, claiming.source_ip,
        "case \"two calls from one client\": sourceIp must be stable across calls from one \
         client, got {:?} and {:?}",
        plain.source_ip, claiming.source_ip
    );
    assert_eq!(
        plain.forwarded_for.as_deref(),
        Some(plain.source_ip.as_str()),
        "case \"a call claiming nothing\": the front door's own forwarded list must name the \
         address the call came from alone, got {:?}",
        plain.forwarded_for
    );
    assert_eq!(
        claiming.forwarded_for.as_deref(),
        Some("203.0.113.9"),
        "case \"a call claiming another address\": the claim must reach the handler verbatim, \
         got {:?}",
        claiming.forwarded_for
    );
    assert_endpoint_hop("a plain signed call", &plain);
    assert_endpoint_hop("the same client claiming another address", &claiming);
}

/// The header the X-Ray tracing context travels on.
const TRACE_HEADER: &str = "x-amzn-trace-id";

/// RULE: the front door tells the handler which CHAIN the invocation runs under, on the
/// tracing header beside the request's own — the same value the run's environment reports in
/// `_X_AMZN_TRACE_ID`, written in the form X-Ray documents (`Root=1-<8 hex>-<24 hex>`), and
/// continuing the chain the client named where it named one.
#[tokio::test]
async fn the_handler_is_told_the_chain_the_invocation_runs_under() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&router, "url-source-trace", &handler()).await;
    let endpoint =
        given_configured_url(&router, "url-source-trace", None, "AWS_IAM", &Value::Null).await;

    // (case, the chain the client named on its own request, the root it must be continued under)
    let rows: &[(&str, Option<&str>, Option<&str>)] = &[
        ("no chain named at all", None, None),
        (
            "a chain the client named",
            Some("Root=1-5759e988-bd862e3fe1be46a994272793"),
            Some("1-5759e988-bd862e3fe1be46a994272793"),
        ),
        (
            "a chain named with its own parent and decision",
            Some("Root=1-5759e988-aaaaaaaaaaaaaaaaaaaaaaaa;Parent=53995c3f42cd8ad8;Sampled=1"),
            Some("1-5759e988-aaaaaaaaaaaaaaaaaaaaaaaa"),
        ),
        (
            "a value that names no chain X-Ray reads",
            Some("not-a-chain"),
            None,
        ),
    ];

    for (case, named, continued) in rows {
        let also: Vec<(&str, &str)> = named
            .map(|value| vec![(TRACE_HEADER, value)])
            .unwrap_or_default();
        let reported = probe(&router, case, &endpoint, &[], &also, true).await;
        let trace = reported.trace_header.clone().unwrap_or_else(|| {
            panic!("case {case:?}: the handler must be told the chain it runs under")
        });
        // The chain is stated in the form X-Ray writes one: the root field first, naming a
        // version-1 id of an 8-hex epoch and a 24-hex unique part.
        let root = trace
            .split(';')
            .next()
            .and_then(|field| field.strip_prefix("Root="))
            .unwrap_or_default()
            .to_string();
        let parts: Vec<&str> = root.split('-').collect();
        let well_formed = parts.len() == 3
            && parts[0] == "1"
            && parts[1].len() == 8
            && parts[2].len() == 24
            && parts[1..]
                .iter()
                .all(|part| part.bytes().all(|digit| digit.is_ascii_hexdigit()));
        assert!(
            well_formed,
            "case {case:?}: expected the documented Root=1-<8 hex>-<24 hex> form, got {trace:?}"
        );
        // The handler reads one chain, whichever way it asks for it.
        assert_eq!(
            reported.trace_environment.as_deref(),
            Some(trace.as_str()),
            "case {case:?}: the chain the event states and the one the environment reports \
             must be one value, got {:?} and {:?}",
            reported.trace_header,
            reported.trace_environment
        );
        if let Some(continued) = continued {
            assert_eq!(
                root, *continued,
                "case {case:?}: expected the chain the client named to be continued under \
                 {continued:?}, got {trace:?}"
            );
        }
    }
}
