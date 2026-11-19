// SPDX-License-Identifier: CC0-1.0

//! What a client is served when it DIALS the authority DynamoDB endpoint discovery announces
//! — the announced value itself, over a socket, on the emulator a client STARTS.
//!
//! RULE: an endpoint-discovery operation announces an authority the responder is actually
//! reached at — the `host[:port]` of the endpoint this process serves — so a client that
//! feeds the discovered value straight back into its endpoint resolver and re-targets every
//! later request at it keeps reaching this emulator, over the same state. A name DERIVED
//! from the serving host (a label of this service and the region under it), or that host on
//! a port the process does not accept on, is an authority nothing answers at: the client's
//! next request fails to connect, and a workload that works with discovery disabled breaks
//! the moment it is enabled.
//!
//! `DescribeEndpoints` exists for exactly that flow ("Returns the regional endpoint
//! information"), and the AWS SDKs drive it automatically for DynamoDB when endpoint
//! discovery is enabled, caching what it hands back for `CachePeriodInMinutes`. So the
//! announced value is judged here the way such a client judges it: by DIALLING it.
//!
//! The sibling suite
//! `services/service-com-amazonaws-dynamodb-dynamodb-20120810/tests/all/endpoint_discovery.rs`
//! pins the VALUE over a router assembled inside the test process — one endpoint, the
//! `host[:port]` the emulator publishes, never a name in a zone AWS publishes its own
//! endpoints in, unmoved by the endpoint a request arrives on or the region it addresses.
//! What that cannot show is the half this suite is about: on the emulator as a client starts
//! it — the built binary, on its own port, with request authentication on — the announced
//! authority is one a connection is ACCEPTED on, and the operations a re-targeted client
//! drives there are served out of the state it had already created. A value in a field
//! cannot say that; only a socket can.
//!
//! The calls differ along what a re-targeted client sends to the discovered authority: the
//! control-plane listing, the control-plane read of one table, a data-plane write, the
//! data-plane read of what that write stored, and discovery itself — which at the dialled
//! authority must announce the same authority again, so a client re-targeting itself never
//! walks off this process.
//!
//! Every wait here is a poll for the CONDITION under a generous deadline; nothing asserts
//! how long a step took.
//!
//! Starting the binary costs what starting the emulator costs, so this leg pays no start of
//! its own: [`crate::cfn_deployed_pipeline_on_a_started_emulator`] calls it on the emulator
//! it already started, the way the other legs of that start are run.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::bedrock_knowledge_base_over_an_aoss_collection::at;
use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;

/// The table the probe seeds through the endpoint the client reached, and the one item it
/// writes at the DISCOVERED authority: the state both halves have to agree about.
const PROBE_TABLE: &str = "discovered-endpoint-probe";
const PROBE_KEY: &str = "probe-1";
const PROBE_MARKER: &str = "written at the discovered authority";

/// DynamoDB's wire: the awsJson 1.0 media type and the `X-Amz-Target` prefix every operation
/// of this service is addressed by (`smithy_query(query="service_info")` on the DynamoDB
/// model — `awsJson1_0`, target prefix `DynamoDB_20120810`).
const CONTENT_TYPE: &str = "application/x-amz-json-1.0";
const TARGET_PREFIX: &str = "DynamoDB_20120810";

/// The SigV4 signing name a DynamoDB client signs for, which is what says which service a
/// request dialled at a bare authority speaks to.
const SIGNING_NAME: &str = "dynamodb";

// The DNS zones AWS publishes its OWN endpoints in are NOT re-listed here: the tree states
// that list once (`service_common::data_plane_addressing`'s `AWS_ENDPOINT_DNS_SUFFIXES`, read
// by `addresses_an_aws_endpoint`), and this crate's manifest — which a fix may not edit — does
// not reach it. The owning crate's in-process suite (`endpoint_discovery.rs`'s
// `the_announced_address_is_never_a_public_aws_endpoint`) is where that half of the rule is
// asserted on the VALUE; here the announced authority has to be DIALLED and served, which a
// host AWS publishes an endpoint at can never be from the machine this emulator runs on.

/// How long the table's clock-owed path to `ACTIVE` is waited for, and how often it is looked
/// at. Generous: the wait is on the STATUS, looked for again until it holds.
const ACTIVE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// One DynamoDB call through the endpoint the client already reached this emulator at, which
/// must answer 200.
fn reached(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    let (status, text) = emulator.rpc(
        SIGNING_NAME,
        &format!("{TARGET_PREFIX}.{operation}"),
        CONTENT_TYPE,
        body,
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation} at the endpoint the client reached, \
         got {status} with {text}; the log:\n{}",
        emulator.log()
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// One DynamoDB call a re-targeted client sends by DIALLING `authority` — the discovered
/// value and nothing else, as both the address connected to and the request's own `Host`.
///
/// A dial nothing accepts fails the case naming the authority, because that is precisely the
/// fault a client with endpoint discovery enabled hits: the connection, not the answer.
fn dialled(
    emulator: &Started,
    case: &str,
    authority: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let target = format!("{TARGET_PREFIX}.{operation}");
    let answered = emulator.call_at(
        authority,
        SIGNING_NAME,
        "POST",
        "/",
        &[("content-type", CONTENT_TYPE), ("x-amz-target", &target)],
        body.to_string().into_bytes(),
    );
    let (status, text) = answered.unwrap_or_else(|error| {
        panic!(
            "case {case:?}: the discovered authority {authority:?} must accept what a client \
             re-targeted at it sends — {operation} reached nothing: {error}; the log:\n{}",
            emulator.log()
        )
    });
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// The one authority a `DescribeEndpoints` answer announces, with the requirements on the
/// VALUE that this suite reads checked: exactly one endpoint, a cache TTL a client can
/// honour, a bare `host:port` (no scheme and no path, which is what the modelled `Address`
/// carries), and a port that parses.
fn announced_authority(case: &str, body: &Value) -> String {
    let endpoints = body["Endpoints"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: Endpoints is a list, got {body}"));
    assert_eq!(
        endpoints.len(),
        1,
        "case {case:?}: one endpoint serves this emulator, so one is discovered, got {body}"
    );
    let endpoint = &endpoints[0];
    let address = endpoint["Address"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: Address is a string, got {body}"))
        .to_string();
    let ttl = endpoint["CachePeriodInMinutes"]
        .as_i64()
        .unwrap_or_else(|| panic!("case {case:?}: CachePeriodInMinutes is a long, got {body}"));
    assert!(
        ttl > 0,
        "case {case:?}: expected a cache TTL in minutes a client can honour, got {ttl}"
    );
    assert!(
        !address.is_empty() && !address.contains('/'),
        "case {case:?}: expected an Address that is a host and a port rather than a URL, got \
         {address:?}"
    );
    // The port a client dials, read PAST an IPv6 literal's own colons: a bracketed host keeps
    // its colons inside the brackets, so the port is what follows the last `]` where there is
    // one, and the last `:` otherwise.
    let after_host = address
        .rsplit_once(']')
        .map_or(address.as_str(), |(_bracketed, rest)| rest);
    let port = after_host
        .rsplit_once(':')
        .map(|(_host, port)| port)
        .unwrap_or_else(|| panic!("case {case:?}: expected {address:?} to carry a port"));
    assert!(
        port.parse::<u16>().is_ok_and(|port| port != 0),
        "case {case:?}: expected the port the endpoint is served on, got {address:?}"
    );
    address
}

/// The `CreateTable` body of the probe table, whose only purpose is to be read back at the
/// discovered authority.
fn probe_table() -> Value {
    json!({
        "TableName": PROBE_TABLE,
        "KeySchema": [{ "AttributeName": "Id", "KeyType": "HASH" }],
        "AttributeDefinitions": [{ "AttributeName": "Id", "AttributeType": "S" }],
        "BillingMode": "PAY_PER_REQUEST",
    })
}

/// Poll `DescribeTable` AT the discovered authority until the probe table is `ACTIVE`, which
/// is what a client does before writing to a table it just created. Waits for the STATUS, not
/// for a span.
fn active_at(emulator: &Started, case: &str, authority: &str) {
    let giving_up_at = Instant::now() + ACTIVE_DEADLINE;
    loop {
        let (status, described) = dialled(
            emulator,
            case,
            authority,
            "DescribeTable",
            &json!({ "TableName": PROBE_TABLE }),
        );
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeTable at {authority}, got {status} \
             with {described}"
        );
        if at(&described, "Table.TableStatus") == json!("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected {PROBE_TABLE} to reach ACTIVE at {authority} within \
             {ACTIVE_DEADLINE:?}, last read {described}"
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// One request a client re-targeted at the discovered authority sends there, the status it
/// must be answered with, and the members that answer must carry.
struct Row {
    case: &'static str,
    operation: &'static str,
    body: Value,
    status: u16,
    expected: Vec<(&'static str, Value)>,
}

/// RULE: the authority DynamoDB endpoint discovery announces is one this process is reached
/// at, so a client that re-targets itself at the discovered value is served this service
/// there, over the state it had already created, and discovery at that authority announces
/// the same authority again.
///
/// Called by [`crate::cfn_deployed_pipeline_on_a_started_emulator`] on the emulator it
/// started, so this leg pays no second start.
pub(crate) fn a_client_re_targeted_at_the_discovered_authority_is_served_there(
    emulator: &Started,
) {
    // One table, created through the endpoint the client reached the emulator at: the state
    // the discovered authority has to be able to show.
    let seeding = "the probe table seeded at the endpoint the client reached";
    reached(emulator, seeding, "CreateTable", &probe_table());

    let discovery = "endpoint discovery at the endpoint the client reached";
    let announced = announced_authority(
        discovery,
        &reached(emulator, discovery, "DescribeEndpoints", &json!({})),
    );

    // The table reaches ACTIVE on its own, read at the DISCOVERED authority: the first proof
    // that a connection is accepted there at all, and that it is this process answering.
    let settling = "the probe table settling, read at the discovered authority";
    active_at(emulator, settling, &announced);

    let rows = vec![
        Row {
            case: "the control-plane read of the table seeded through the reached endpoint",
            operation: "DescribeTable",
            body: json!({ "TableName": PROBE_TABLE }),
            status: 200,
            expected: vec![("Table.TableName", json!(PROBE_TABLE))],
        },
        Row {
            case: "a data-plane write sent to the discovered authority",
            operation: "PutItem",
            body: json!({
                "TableName": PROBE_TABLE,
                "Item": { "Id": { "S": PROBE_KEY }, "Marker": { "S": PROBE_MARKER } },
            }),
            status: 200,
            expected: Vec::new(),
        },
        Row {
            case: "the data-plane read of what that write stored",
            operation: "GetItem",
            body: json!({
                "TableName": PROBE_TABLE,
                "Key": { "Id": { "S": PROBE_KEY } },
                "ConsistentRead": true,
            }),
            status: 200,
            expected: vec![("Item.Marker.S", json!(PROBE_MARKER))],
        },
    ];

    for Row {
        case,
        operation,
        body,
        status: expected_status,
        expected,
    } in rows
    {
        let (status, answer) = dialled(emulator, case, &announced, operation, &body);
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected {operation} dialled at {announced} to answer \
             {expected_status}, got {status} with {answer}; the log:\n{}",
            emulator.log()
        );
        for (member, value) in expected {
            assert_eq!(
                at(&answer, member),
                value,
                "case {case:?}: expected {member} to be {value} in the answer to {operation} \
                 dialled at {announced}, got {answer}"
            );
        }
    }

    // The control-plane listing a re-targeted client reads at the discovered authority holds
    // the table that was seeded through the endpoint it reached: ONE store behind the two
    // addresses, which is what says the discovered authority is this process's own.
    let listing = "the table listing a re-targeted client reads";
    let (status, listed) = dialled(emulator, listing, &announced, "ListTables", &json!({}));
    assert_eq!(
        status, 200,
        "case {listing:?}: expected HTTP 200 from ListTables at {announced}, got {status} with \
         {listed}"
    );
    assert!(
        listed["TableNames"]
            .as_array()
            .is_some_and(|names| names.contains(&json!(PROBE_TABLE))),
        "case {listing:?}: expected the listing at {announced} to hold {PROBE_TABLE}, which was \
         created at the endpoint the client reached, got {listed}"
    );

    // Discovery AT the discovered authority announces that same authority: the value is a
    // fixed point, so a client that honours it once and then keeps honouring it never walks
    // off the address this process answers on.
    let again = "endpoint discovery at the discovered authority";
    let (status, rediscovered) =
        dialled(emulator, again, &announced, "DescribeEndpoints", &json!({}));
    assert_eq!(
        status, 200,
        "case {again:?}: expected HTTP 200, got {status} with {rediscovered}"
    );
    assert_eq!(
        announced_authority(again, &rediscovered),
        announced,
        "case {again:?}: expected the same authority to be announced at the authority it \
         announced, got {rediscovered}"
    );
}
