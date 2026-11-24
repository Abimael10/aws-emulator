// SPDX-License-Identifier: CC0-1.0

//! What the request an `HTTP_PROXY` integration FORWARDS says about the hop it is making, on
//! the `execute-api` data plane of an HTTP api.
//!
//! The rule every table below is one slice of:
//!
//! > **A request a gateway forwards across an HTTP proxy integration states THIS hop and not
//! > the client's claims about earlier ones: `Via` is the gateway's own
//! > (`HTTP/1.1 AmazonAPIGateway`), replacing whatever the client sent — "a proxy MUST send an
//! > appropriate Via header field … in each message that it forwards" (RFC 9110 §7.6.3) — and
//! > the client's `X-Forwarded-For` is not forwarded at all, so the address the upstream reads
//! > is the one the upstream itself observed the connection from and never a chain the client
//! > composed. Everything else the client sent — its method, its body, its query string and
//! > every other header — travels on, which is what "the client request passed through as-is"
//! > (`Integration$IntegrationType`) means.**
//!
//! The CONTRAST leg is driven from the same table: the same client headers sent straight to
//! the origin api, with no proxy hop in between, reach its backend unchanged (`Via` verbatim,
//! `X-Forwarded-For` the client's own chain with the address the gateway observed appended) —
//! so what the first table asserts is attributable to the hop and to nothing else. That
//! second rule is `http_api_integration_response_mapping.rs`'s own and is restated here only
//! as the control.
//!
//! Nothing here is a value this emulator fills in: every assertion is on the headers a
//! function RAN and echoed back, which is why AWS Lambda is mounted beside this service on
//! one engine.
//!
//! What this suite does NOT restate: which upstreams are forwarded to at all, and the
//! redirect every other one earns, which `http_api_backend_dispatch.rs` owns — one row below
//! drives an upstream nothing here serves only to show the hop's fields belong to a hop that
//! was made.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::http_api_backend_dispatch::{
    emulator_with_lambda, given_api, given_function, lambda_integration,
};
use crate::websocket_handshake_data_plane::{send_with_body, Answer};

/// The `Via` API Gateway's own hop writes onto the request it forwards: fixed text that
/// interpolates nothing about the request, the api or the stage.
const GATEWAY_VIA: &str = "HTTP/1.1 AmazonAPIGateway";

/// The address this emulator's front door observes a request FROM: a client reaches it over
/// loopback, so that is the peer a gateway of this emulator states about the connection it
/// accepted.
const OBSERVED_PEER: &str = "127.0.0.1";

/// The status an `HTTP_PROXY` upstream this emulator serves nothing at is answered with: the
/// caller is sent there, because this emulator makes no request of its own to it.
const SENT_TO_THE_UPSTREAM: u16 = 307;

/// An upstream NOTHING in this emulator serves, for the row that drives one.
const UNSERVED_UPSTREAM: &str = "https://nothing.example.test/echo";

/// The response header the origin's handler sets, which is what shows an answer came from the
/// origin's own backend.
const ORIGIN_HEADER: &str = "x-origin";

/// The status an HTTP api answers for an integration that produced no response, and the one
/// it answers for a backend that did not answer in time: the two a FIRST invocation can earn
/// while the function's runtime is still starting, which the wait below looks past.
const INTEGRATION_FAULT: u16 = 500;
const TIMED_OUT: u16 = 503;

/// How long a request keeps being made while the origin's function is still starting, and how
/// often it is made: generous, and bounding no behaviour any row asserts on. Nothing here
/// measures how long anything took.
const ANSWER_WAIT: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// The handler behind the origin api's route: it echoes what it was handed — the request's
/// headers (lower-cased, as a `2.0` proxy event states them), its path, its query string and
/// its verb — under a response header of its own.
const ECHO_RECEIVED_REQUEST: &str = r#"
import json


def lambda_handler(event, context):
    context_http = (event.get("requestContext") or {}).get("http") or {}
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json", "x-origin": "seen"},
        "body": json.dumps(
            {
                "headers": event.get("headers") or {},
                "rawPath": event.get("rawPath"),
                "rawQueryString": event.get("rawQueryString"),
                "method": context_http.get("method"),
            }
        ),
    }
"#;

/// One row: what a failing assertion calls it, the request the client makes, and what the
/// upstream must read for the two hop fields.
struct Row {
    case: &'static str,
    method: &'static str,
    /// The path within the front stage, query string included.
    path: &'static str,
    body: &'static [u8],
    /// The headers the client sends, one entry per field line.
    headers: &'static [(&'static str, &'static str)],
    /// The query string the upstream must read, which is the client's own.
    forwarded_query: &'static str,
    /// What the backend BEHIND the hop must read for `x-forwarded-for`: the address the
    /// origin observed, alone — the client's claim reaches it not at all.
    forwarded_for_through_the_hop: &'static str,
    /// What the SAME backend reads for `x-forwarded-for` when the client addresses it
    /// directly, with no hop in between: the client's own chain with that address appended.
    forwarded_for_direct: &'static str,
    /// What that backend reads for `via` when the client addresses it directly: the client's
    /// own value, verbatim.
    via_direct: &'static str,
}

/// The rows: they differ along what the CLIENT claims about hops — a chain of two `Via`
/// tokens beside a two-element forwarded chain, one token and one address, the two fields
/// sent twice each, an empty forwarded chain, and neither field at all — and along the verb,
/// the body and the query string the request carries.
const ROWS: [Row; 6] = [
    Row {
        case: "a client claiming two hops in Via and a chain of two addresses",
        method: "GET",
        path: "/front",
        body: b"",
        headers: &[
            ("via", "2.0 client-claimed-hop, 1.1 another-claimed-hop"),
            ("x-forwarded-for", "203.0.113.9, 198.51.100.7"),
            ("x-tenant", "acme"),
        ],
        forwarded_query: "",
        forwarded_for_through_the_hop: OBSERVED_PEER,
        forwarded_for_direct: "203.0.113.9, 198.51.100.7, 127.0.0.1",
        via_direct: "2.0 client-claimed-hop, 1.1 another-claimed-hop",
    },
    Row {
        case: "a client claiming one hop and one address, POSTing a body",
        method: "POST",
        path: "/front",
        body: b"{\"sent\":true}",
        headers: &[
            ("via", "1.1 solitary-claimed-hop"),
            ("x-forwarded-for", "192.0.2.44"),
            ("x-tenant", "beta"),
        ],
        forwarded_query: "",
        forwarded_for_through_the_hop: OBSERVED_PEER,
        forwarded_for_direct: "192.0.2.44, 127.0.0.1",
        via_direct: "1.1 solitary-claimed-hop",
    },
    Row {
        case: "a client sending each field twice, with a query string",
        method: "GET",
        path: "/front?page=2&tenant=gamma",
        body: b"",
        headers: &[
            ("via", "1.0 first-claimed-hop"),
            ("via", "1.1 second-claimed-hop"),
            ("x-forwarded-for", "198.51.100.7"),
            ("x-forwarded-for", "203.0.113.9"),
            ("x-tenant", "gamma"),
        ],
        forwarded_query: "page=2&tenant=gamma",
        forwarded_for_through_the_hop: OBSERVED_PEER,
        forwarded_for_direct: "198.51.100.7, 203.0.113.9, 127.0.0.1",
        via_direct: "1.0 first-claimed-hop,1.1 second-claimed-hop",
    },
    Row {
        case: "a client whose forwarded chain is empty",
        method: "GET",
        path: "/front",
        body: b"",
        headers: &[
            ("via", "HTTP/1.1 AmazonAPIGateway"),
            ("x-forwarded-for", ""),
            ("x-tenant", "delta"),
        ],
        forwarded_query: "",
        forwarded_for_through_the_hop: OBSERVED_PEER,
        forwarded_for_direct: OBSERVED_PEER,
        via_direct: "HTTP/1.1 AmazonAPIGateway",
    },
    Row {
        case: "a client claiming no hop at all",
        method: "PUT",
        path: "/front",
        body: b"replaced",
        headers: &[("x-tenant", "epsilon")],
        forwarded_query: "",
        forwarded_for_through_the_hop: OBSERVED_PEER,
        forwarded_for_direct: OBSERVED_PEER,
        via_direct: "",
    },
    Row {
        case: "a client claiming a hop whose token is not a protocol at all",
        method: "DELETE",
        path: "/front?id=9",
        body: b"",
        headers: &[
            ("via", "proxy-without-a-protocol"),
            ("x-forwarded-for", "2001:db8::1"),
            ("x-tenant", "zeta"),
        ],
        forwarded_query: "id=9",
        forwarded_for_through_the_hop: OBSERVED_PEER,
        forwarded_for_direct: "2001:db8::1, 127.0.0.1",
        via_direct: "proxy-without-a-protocol",
    },
];

/// The ORIGIN api: one `AWS_PROXY` route running [`ECHO_RECEIVED_REQUEST`] on the stage that
/// serves its root. Answers `(the host its ApiEndpoint names, the invoke URL of that route)`.
async fn given_origin_api(
    emulator: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    case: &str,
) -> (String, String) {
    let arn = given_function(emulator, engine, case, "hop-origin", ECHO_RECEIVED_REQUEST).await;
    let (_, host) = given_api(
        emulator,
        case,
        lambda_integration(&arn, "2.0"),
        "ANY /echo",
        json!({ "stageName": "$default", "autoDeploy": true }),
    )
    .await;
    let invoke_url = format!("https://{host}/echo");
    (host, invoke_url)
}

/// A FRONT api: one `HTTP_PROXY` route over `upstream` on the stage that serves its root.
/// Answers the host its `ApiEndpoint` names.
async fn given_front_api(emulator: &MultiServiceRouter, case: &str, upstream: &str) -> String {
    let (_, host) = given_api(
        emulator,
        case,
        json!({
            "integrationType": "HTTP_PROXY",
            "integrationUri": upstream,
            "integrationMethod": "ANY",
            "payloadFormatVersion": "1.0"
        }),
        "ANY /front",
        json!({ "stageName": "$default", "autoDeploy": true }),
    )
    .await;
    host
}

/// One request a row makes, asked again until the origin's own answer arrives or
/// [`ANSWER_WAIT`] passes: the FIRST invocation of a function starts its runtime, which the
/// gateway answers its own [`INTEGRATION_FAULT`] (or [`TIMED_OUT`]) for while it is starting.
/// The wait is for the answer — a condition — and nothing here asserts on the clock.
async fn answered(
    emulator: &MultiServiceRouter,
    case: &str,
    host: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Answer {
    let deadline = Instant::now() + ANSWER_WAIT;
    loop {
        let answer = send_with_body(emulator, host, method, path, headers, body.to_vec()).await;
        if answer.status == 200 {
            return answer;
        }
        assert!(
            matches!(answer.status, INTEGRATION_FAULT | TIMED_OUT) && Instant::now() < deadline,
            "case {case:?}: expected the origin's own answer, got {}",
            answer.summary()
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// What the origin's handler echoed: the value it read for the header `name`, as a `2.0` event
/// states it (lower-cased, occurrences joined), or the empty string for one it read no value
/// for.
fn read(answer: &Answer, name: &str) -> String {
    let echoed: Value = serde_json::from_str(&answer.text).unwrap_or(Value::Null);
    echoed
        .pointer(&format!("/headers/{name}"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A member of what the origin's handler echoed about the request it received.
fn echoed(answer: &Answer, member: &str) -> String {
    let echoed: Value = serde_json::from_str(&answer.text).unwrap_or(Value::Null);
    echoed
        .pointer(&format!("/{member}"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn the_http_proxy_hop_states_its_own_via_and_withholds_the_clients_forwarded_chain() {
    let case = "an HTTP_PROXY route over an origin api that echoes what it received";
    let (emulator, engine) = emulator_with_lambda();
    let (_origin_host, upstream) = given_origin_api(&emulator, &engine, case).await;
    let front = given_front_api(&emulator, case, &upstream).await;

    for row in &ROWS {
        let case = row.case;
        let answer = answered(
            &emulator,
            case,
            &front,
            row.method,
            row.path,
            row.headers,
            row.body,
        )
        .await;
        assert_eq!(
            answer.header(ORIGIN_HEADER).as_deref(),
            Some("seen"),
            "case {case:?}: expected the origin's own answer through the hop, got {}",
            answer.summary()
        );
        // The hop's OWN fields, whatever the client claimed.
        let via = read(&answer, "via");
        assert_eq!(
            via, GATEWAY_VIA,
            "case {case:?}: the hop must state its own Via {GATEWAY_VIA:?}, got {via:?}"
        );
        let forwarded_for = read(&answer, "x-forwarded-for");
        assert_eq!(
            forwarded_for, row.forwarded_for_through_the_hop,
            "case {case:?}: expected the upstream to read {:?} for x-forwarded-for, got \
             {forwarded_for:?}",
            row.forwarded_for_through_the_hop
        );
        for (name, value) in row.headers {
            if *name == "via" || *name == "x-forwarded-for" {
                continue;
            }
            let carried = read(&answer, name);
            assert_eq!(
                carried, *value,
                "case {case:?}: expected {name} to travel on as {value:?}, got {carried:?}"
            );
        }
        // And the rest of the client's request travelled with it.
        let method = echoed(&answer, "method");
        assert_eq!(
            method, row.method,
            "case {case:?}: expected the upstream to be called with {:?}, got {method:?}",
            row.method
        );
        let query = echoed(&answer, "rawQueryString");
        assert_eq!(
            query, row.forwarded_query,
            "case {case:?}: expected the upstream to read the query {:?}, got {query:?}",
            row.forwarded_query
        );
        let path = echoed(&answer, "rawPath");
        assert_eq!(
            path, "/echo",
            "case {case:?}: expected the upstream's own path, got {path:?}"
        );
    }
}

#[tokio::test]
async fn the_same_client_headers_reach_the_origin_unchanged_with_no_hop_in_between() {
    // The control for the table above, over the SAME rows: with no proxy hop between the
    // client and the backend, the gateway relays the client's `Via` verbatim and PRESERVES its
    // forwarded chain, appending the address it observed the request from.
    let case = "the origin api addressed by the client itself";
    let (emulator, engine) = emulator_with_lambda();
    let (origin, _upstream) = given_origin_api(&emulator, &engine, case).await;

    for row in &ROWS {
        let case = row.case;
        let answer = answered(
            &emulator,
            case,
            &origin,
            row.method,
            "/echo",
            row.headers,
            row.body,
        )
        .await;
        let via = read(&answer, "via");
        assert_eq!(
            via, row.via_direct,
            "case {case:?}: expected the client's own Via {:?} with no hop in between, got \
             {via:?}",
            row.via_direct
        );
        let forwarded_for = read(&answer, "x-forwarded-for");
        assert_eq!(
            forwarded_for, row.forwarded_for_direct,
            "case {case:?}: expected the client's chain with the observed address appended \
             ({:?}), got {forwarded_for:?}",
            row.forwarded_for_direct
        );
    }
}

#[tokio::test]
async fn an_upstream_this_emulator_serves_nothing_at_is_still_the_callers_to_reach() {
    // No hop is made to an address this emulator serves nothing at, so there are no hop
    // fields to state: the caller is sent there with the client's own request intact.
    let case = "an HTTP_PROXY route over an upstream nothing in this emulator serves";
    let (emulator, _engine) = emulator_with_lambda();
    let front = given_front_api(&emulator, case, UNSERVED_UPSTREAM).await;
    for row in &ROWS {
        let case = row.case;
        let answer = send_with_body(
            &emulator,
            &front,
            row.method,
            row.path,
            row.headers,
            row.body.to_vec(),
        )
        .await;
        assert_eq!(
            answer.status, SENT_TO_THE_UPSTREAM,
            "case {case:?}: expected the caller to be sent to the upstream, got {}",
            answer.summary()
        );
        assert_eq!(
            answer.location.as_deref(),
            Some(UNSERVED_UPSTREAM),
            "case {case:?}: expected {UNSERVED_UPSTREAM:?} in Location, got {}",
            answer.summary()
        );
        let answered = answer.header(ORIGIN_HEADER);
        assert_eq!(
            answered, None,
            "case {case:?}: no answer of an upstream's own may reach the client, got \
             {answered:?}"
        );
    }
}
