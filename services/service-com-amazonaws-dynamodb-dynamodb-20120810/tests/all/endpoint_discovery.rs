// SPDX-License-Identifier: CC0-1.0

//! Endpoint discovery: what `DescribeEndpoints` reports, and that what it names is the
//! address this emulator is actually reached on.
//!
//! The rule these tests pin:
//!
//! > endpoint discovery reports exactly one endpoint, whose `Address` is THIS EMULATOR'S
//! > OWN serving address — the host and port of the base URL this process is reached at,
//! > with no scheme and no path — with a positive cache TTL in minutes; the address is a
//! > property of the process, so neither the endpoint a request arrives on nor the region it
//! > addresses moves it, and it is the same across an immediate repeat call.
//!
//! One form of arrival is one row of [`ARRIVALS`]; the rows differ along the FORM of the
//! endpoint a client reaches this emulator through (the regional host, its FIPS sibling, the
//! dual-stack endpoint as an HTTP/2 `:authority` with no `Host` header at all, the legacy
//! global host, loopback with a port, a bare name on a port of the caller's choosing, no
//! host at all, a `Host` that is not an address, a `Host` carrying a path) and along the
//! REGION each addresses — regions of six different AWS partitions — because neither may
//! change the address reported back. [`the_announced_address_is_the_serving_address`] drives
//! the other half: a set of serving addresses this emulator is published at, each of which
//! the answer must name back, which is what makes the announced address one a client can
//! actually dial.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use crate::emulator::emulator;

/// How one request names the endpoint it arrived on and the region it addresses.
struct Arrival {
    /// What this row is, for an assertion message.
    case: &'static str,
    /// The region the request's credential scope names, which is the region it addresses.
    region: &'static str,
    /// The `Host` header the request carries, or `None` for a request that carries none.
    host: Option<&'static str>,
    /// The absolute-form URI the request carries, whose authority is the endpoint the
    /// request arrived on when it carries no `Host` header; `None` for the origin form.
    authority_uri: Option<&'static str>,
}

/// Every form of endpoint a client reaches this emulator through, each addressing a region
/// of a different AWS partition (commercial, GovCloud, China and the four ISO partitions —
/// the partitions of the AWS SDKs' endpoint metadata), none of which may change the address
/// reported back.
const ARRIVALS: &[Arrival] = &[
    Arrival {
        case: "the regional endpoint host of the region it is signed for",
        region: "us-west-2",
        host: Some("dynamodb.us-west-2.amazonaws.com"),
        authority_uri: None,
    },
    Arrival {
        case: "the FIPS regional endpoint host",
        region: "us-gov-west-1",
        host: Some("dynamodb-fips.us-gov-west-1.amazonaws.com"),
        authority_uri: None,
    },
    Arrival {
        case: "the dual-stack endpoint as an HTTP/2 :authority, with no Host header",
        region: "ap-southeast-2",
        host: None,
        authority_uri: Some("https://dynamodb.ap-southeast-2.api.aws/"),
    },
    Arrival {
        case: "the legacy global endpoint host",
        region: "eu-central-1",
        host: Some("dynamodb.amazonaws.com"),
        authority_uri: None,
    },
    Arrival {
        case: "loopback on the port the emulator serves, signed for a China region",
        region: "cn-north-1",
        host: Some("127.0.0.1:9297"),
        authority_uri: None,
    },
    Arrival {
        case: "a bare name on a port of the caller's choosing, signed for an ISO region",
        region: "us-iso-east-1",
        host: Some("dynamodb.test.internal:34567"),
        authority_uri: Some("https://dynamodb.test.internal:34567/"),
    },
    Arrival {
        case: "no Host header and no authority, signed for an ISO-B region",
        region: "us-isob-east-1",
        host: None,
        authority_uri: None,
    },
    Arrival {
        case: "a Host that is not an address at all, signed for an ISO-E region",
        region: "eu-isoe-west-1",
        host: Some("dynamodb us-east-1"),
        authority_uri: None,
    },
    Arrival {
        case: "a Host carrying a path, signed for an ISO-F region",
        region: "us-isof-south-1",
        host: Some("dynamodb.us-isof-south-1.csp.hci.ic.gov/shard"),
        authority_uri: None,
    },
];

/// The base URL this suite's emulator is published at, and the `Address` discovery must
/// then announce: the authority of that URL, which is the host and port a client dials.
const SERVING_BASE: &str = "https://127.0.0.1:34567";
const SERVING_ADDRESS: &str = "127.0.0.1:34567";

/// One emulator published at `base`, the way the data port publishes the URL it bound.
fn emulator_served_at(base: &str) -> MultiServiceRouter {
    let mut router = emulator();
    router.set_url_base(EndpointUrl::parse(base).unwrap_or_else(|| {
        panic!("{base:?} is a usable endpoint URL");
    }));
    router
}

/// Send one operation that arrives on `host`/`authority_uri` signed for `region`, and
/// answer its HTTP status and parsed body.
async fn call_through(
    router: &MultiServiceRouter,
    region: &str,
    host: Option<&str>,
    authority_uri: Option<&str>,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let mut builder = http::Request::builder()
        .method("POST")
        .uri(authority_uri.unwrap_or("/"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{region}/dynamodb/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        );
    if let Some(host) = host {
        builder = builder.header("host", host);
    }
    let request = builder
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// The addresses a `DescribeEndpoints` body reports, in order, with every entry checked for
/// shape: a host and a port and nothing else (no scheme, no path), with a positive cache
/// TTL in minutes.
fn reported_addresses(case: &str, body: &Value) -> Vec<String> {
    let endpoints = body["Endpoints"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: Endpoints is a list, got {body}"));
    assert_eq!(
        endpoints.len(),
        1,
        "case {case:?}: this emulator serves one endpoint, so one is discovered, got {body}"
    );
    endpoints
        .iter()
        .map(|endpoint| {
            let address = endpoint["Address"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: Address is a string, got {body}"))
                .to_string();
            assert!(
                !address.contains("://") && !address.contains('/') && !address.is_empty(),
                "case {case:?}: an Address is a host and a port, not a URL, got {address:?}"
            );
            let ttl = endpoint["CachePeriodInMinutes"].as_i64().unwrap_or_else(|| {
                panic!("case {case:?}: CachePeriodInMinutes is a long, got {body}")
            });
            assert!(
                ttl > 0,
                "case {case:?}: a cache TTL in minutes a client can honour, got {ttl}"
            );
            address
        })
        .collect()
}

/// The `CreateTable` body for a probe table whose only purpose is to be read back
/// through a discovered address.
fn probe_table(name: &str) -> Value {
    json!({
        "TableName": name,
        "KeySchema": [{"AttributeName": "Id", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "Id", "AttributeType": "S"}],
        "BillingMode": "PAY_PER_REQUEST",
    })
}

#[tokio::test]
async fn discovery_reports_this_emulators_serving_address() {
    let router = emulator_served_at(SERVING_BASE);

    for arrival in ARRIVALS {
        // One table in this row's region, so the address the answer names can be shown to
        // still place a request on this service, over THIS region's store.
        let probe = format!("Probe{}", arrival.region.replace('-', ""));
        let (status, created) = call_through(
            &router,
            arrival.region,
            arrival.host,
            arrival.authority_uri,
            "CreateTable",
            &probe_table(&probe),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: seeding {probe} expected HTTP 200, got {status} with {created}",
            arrival.case
        );

        let (status, body) = call_through(
            &router,
            arrival.region,
            arrival.host,
            arrival.authority_uri,
            "DescribeEndpoints",
            &json!({}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200, got {status} with {body}",
            arrival.case
        );
        let addresses = reported_addresses(arrival.case, &body);
        assert_eq!(
            addresses,
            vec![SERVING_ADDRESS.to_string()],
            "case {:?}: the address this emulator is served on, whatever endpoint the \
             request arrived on and whatever region it addresses, expected \
             {SERVING_ADDRESS:?}, got {addresses:?}",
            arrival.case
        );

        for address in &addresses {
            // A request sent to the announced address reaches this service, over the store
            // of the region the request addresses — which is also what shows discovery
            // changed nothing a later caller sees, the probe table still being the one and
            // only table of that region.
            let (status, listed) = call_through(
                &router,
                arrival.region,
                Some(address.as_str()),
                None,
                "ListTables",
                &json!({}),
            )
            .await;
            assert_eq!(
                (status, listed["TableNames"].clone()),
                (200, json!([probe])),
                "case {:?}: the reported address {address:?} places a request on this service \
                 and {}'s store, got {status} with {listed}",
                arrival.case,
                arrival.region
            );
        }
    }
}

#[tokio::test]
async fn the_announced_address_is_the_serving_address() {
    // Each row is an address this emulator is published at — a loopback address with a
    // port, a port the OS could hand out, an IPv6 literal, a host name with a port and one
    // without — and the `Address` discovery must then announce: the authority of that URL,
    // whatever it is, because that is where this process answers.
    let rows: [(&str, &str, &str); 5] = [
        ("loopback on a fixed port", SERVING_BASE, SERVING_ADDRESS),
        (
            "loopback on an ephemeral port",
            "https://127.0.0.1:49512",
            "127.0.0.1:49512",
        ),
        (
            "an IPv6 loopback literal",
            "https://[::1]:9297",
            "[::1]:9297",
        ),
        (
            "a host name with a port",
            "https://emulator.internal:8443",
            "emulator.internal:8443",
        ),
        (
            "a host name on the default port",
            "https://emulator.internal",
            "emulator.internal",
        ),
    ];

    for (case, base, expected) in rows {
        let router = emulator_served_at(base);
        let (status, body) =
            call_through(&router, "us-west-2", None, None, "DescribeEndpoints", &json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body}"
        );
        let addresses = reported_addresses(case, &body);
        assert_eq!(
            addresses,
            vec![expected.to_string()],
            "case {case:?}: the announced address is the one this emulator is served at, \
             expected {expected:?}, got {addresses:?}"
        );
    }
}

#[tokio::test]
async fn the_announced_address_is_never_a_public_aws_endpoint() {
    let router = emulator_served_at(SERVING_BASE);
    // Whatever region a request addresses, and whatever endpoint host it arrives on, the
    // address it is handed is this emulator's own: never a real AWS endpoint, so a client
    // that re-targets itself at a discovered endpoint cannot be sent off this emulator with
    // the credentials it is holding.
    //
    // The DNS suffixes ruled out here are those of the AWS partitions (the `dnsSuffix`
    // values of the AWS SDKs' endpoint metadata), one per partition.
    let partition_suffixes = [
        "amazonaws.com",
        "amazonaws.com.cn",
        "c2s.ic.gov",
        "sc2s.sgov.gov",
        "cloud.adc-e.uk",
        "csp.hci.ic.gov",
        "api.aws",
    ];
    for region in [
        "us-west-2",
        "cn-north-1",
        "us-gov-east-1",
        "us-iso-east-1",
        "us-isob-east-1",
        "eu-isoe-west-1",
        "us-isof-south-1",
    ] {
        let (status, body) = call_through(
            &router,
            region,
            Some(&format!("dynamodb.{region}.amazonaws.com")),
            None,
            "DescribeEndpoints",
            &json!({}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {region:?}: expected HTTP 200, got {status} with {body}"
        );
        for address in reported_addresses(region, &body) {
            let host = address
                .rsplit_once(':')
                .map_or(address.clone(), |(host, _)| host.to_string());
            for suffix in partition_suffixes {
                assert!(
                    !host.ends_with(suffix),
                    "case {region:?}: a reported address is never a public AWS endpoint host, \
                     but {address:?} ends in {suffix:?}"
                );
            }
            assert_eq!(
                address, SERVING_ADDRESS,
                "case {region:?}: the address is this emulator's own serving address, got \
                 {address:?}"
            );
        }
    }
}

#[tokio::test]
async fn discovery_is_stable_across_calls_through_one_endpoint() {
    let router = emulator_served_at(SERVING_BASE);
    // Two calls through the same endpoint report the same endpoints: a client that caches
    // what it was handed for the TTL it was handed is not being handed a moving target.
    for arrival in ARRIVALS {
        let mut answers = Vec::new();
        for _ in 0..2 {
            let (status, body) = call_through(
                &router,
                arrival.region,
                arrival.host,
                arrival.authority_uri,
                "DescribeEndpoints",
                &json!({}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?}: expected HTTP 200, got {status} with {body}",
                arrival.case
            );
            answers.push(body["Endpoints"].clone());
        }
        assert_eq!(
            answers[0], answers[1],
            "case {:?}: endpoint discovery is stable across reads, got {:?} then {:?}",
            arrival.case, answers[0], answers[1]
        );
    }
}

#[tokio::test]
async fn a_request_that_names_no_region_is_told_the_same_address() {
    let router = emulator_served_at(SERVING_BASE);
    // A request whose credential scope names nothing region-shaped, on no endpoint host, is
    // answered the serving address like every other request: the address this process
    // answers at is no more a property of the region than of the arrival, and it is never
    // an empty list.
    for scope in ["US_WEST_2", "not a region"] {
        let (status, body) =
            call_through(&router, scope, None, None, "DescribeEndpoints", &json!({})).await;
        assert_eq!(
            status, 200,
            "case {scope:?}: expected HTTP 200, got {status} with {body}"
        );
        let addresses = reported_addresses(scope, &body);
        assert_eq!(
            addresses,
            vec![SERVING_ADDRESS.to_string()],
            "case {scope:?}: a request that addresses no region is served this emulator's \
             own address, expected {SERVING_ADDRESS:?}, got {addresses:?}"
        );
    }
}
