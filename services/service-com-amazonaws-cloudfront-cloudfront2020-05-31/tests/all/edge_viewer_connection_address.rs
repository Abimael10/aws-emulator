// SPDX-License-Identifier: CC0-1.0

//! The VIEWER'S CONNECTION as the edge reports it to an origin, end to end at the address a
//! deployed distribution published.
//!
//! RULE: `CloudFront-Viewer-Address` describes the connection one viewer request arrived on —
//! the address the request came from AND the port it came from — so the edge generates it for
//! every viewer request as `<address>:<port>`: the address half is the one the edge itself
//! observes for the viewer (the same value its signed-URL source-address condition, its web
//! ACL's address rules and its log records are read against), and the port half is the
//! viewer's own end of that connection. An origin reading the header therefore reads both
//! halves for every request it serves, whatever the viewer stated.
//!
//! The rows differ along what the rule generalises over: what the request says about where it
//! came from (nothing at all, one address, a proxy chain whose first entry is the viewer, a
//! value that is no address, an IPv6 viewer) and whether the viewer also states a value of its
//! own for the header that reports it — which the edge drops, as it drops every
//! `CloudFront-*` value a viewer states. One row's viewer is asked twice, because one
//! viewer has one address: a reader comparing two requests of the same viewer reads the
//! same address, whichever connection each of them arrived on.
//!
//! The origin is the Lambda function URL that REPORTS the request it was handed, and the
//! distribution, the dial and the reading of one answer are [`crate::edge_lambda_url_origin`]'s,
//! reused rather than copied: its default behaviour already carries the managed origin request
//! policy that forwards the `CloudFront-*` headers, without which no origin sees them.

use crate::edge_lambda_url_origin::{viewer_ip, Answered, Topology, MISS, OPEN_PATH};

/// The header the edge generates for the connection, lower case as an origin request carries
/// it, and the header a viewer that traversed a proxy states its own address in — the one the
/// edge reads the address of a request that crossed no socket of its own from.
const VIEWER_ADDRESS_HEADER: &str = "cloudfront-viewer-address";
const FORWARDED_FOR: &str = "x-forwarded-for";

/// The range RFC 6335 reserves for the end of a connection a CLIENT opens from — "the Dynamic
/// and/or Private Ports" — which is where the port of a viewer's own end of a connection lies.
const FIRST_DYNAMIC_PORT: u16 = 49_152;
const LAST_DYNAMIC_PORT: u16 = 65_535;

/// One row: what the viewer states about its connection, and the ADDRESS half the origin must
/// then be told.
struct ConnectionRow {
    case: &'static str,
    /// The headers the viewer states of its own accord.
    stated: &'static [(&'static str, &'static str)],
    /// The address the origin must be told, or `None` where the address the edge observes for a
    /// viewer that states nothing is what it reports — one value for every such row, which the
    /// rows hold it to.
    address: Option<&'static str>,
}

/// The rows, each dialled in the clear at the address the distribution published.
const CONNECTION_ROWS: &[ConnectionRow] = &[
    ConnectionRow {
        case: "a viewer that states nothing about where it came from",
        stated: &[],
        address: None,
    },
    ConnectionRow {
        case: "a viewer whose proxy states its address",
        stated: &[(FORWARDED_FOR, "203.0.113.7")],
        address: Some("203.0.113.7"),
    },
    ConnectionRow {
        case: "a proxy chain, whose first entry is the viewer",
        stated: &[(FORWARDED_FOR, "198.51.100.2, 203.0.113.7")],
        address: Some("198.51.100.2"),
    },
    ConnectionRow {
        case: "a stated value that is no address at all",
        stated: &[(FORWARDED_FOR, "unknown")],
        address: None,
    },
    ConnectionRow {
        case: "an IPv6 viewer",
        stated: &[(FORWARDED_FOR, "2001:db8::1")],
        address: Some("2001:db8::1"),
    },
    ConnectionRow {
        case: "a viewer that states a connection of its own for the header",
        stated: &[
            (VIEWER_ADDRESS_HEADER, "203.0.113.9:1234"),
            (FORWARDED_FOR, "203.0.113.7"),
        ],
        address: Some("203.0.113.7"),
    },
];

/// The two halves of a reported connection: the address and the port, read the way an origin
/// reads them — the address off the front of the value ([`viewer_ip`], which keeps an IPv6
/// literal's brackets) and the port after the separator that follows it.
fn halves(case: &str, reported: &str) -> (String, String) {
    let address = viewer_ip(reported);
    let port = reported
        .get(address.len()..)
        .and_then(|rest| rest.strip_prefix(':'))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the viewer's port after its address in {reported:?}")
        })
        .to_string();
    (address, port)
}

/// Assert that `reported` is one connection as the rule has the edge report it: the address
/// `expected` and a port of the viewer's own end of a connection.
fn assert_connection(case: &str, reported: &str, expected: &str) {
    let (address, port) = halves(case, reported);
    let bare = address.trim_start_matches('[').trim_end_matches(']');
    assert_eq!(
        bare, expected,
        "case {case:?}: expected the origin to be told the address the edge observes \
         ({expected:?}), got {address:?} in {reported:?}"
    );
    assert!(
        bare.parse::<::std::net::IpAddr>().is_ok(),
        "case {case:?}: expected an IP address in {reported:?}, got {address:?}"
    );
    let port = port.parse::<u16>().unwrap_or_else(|error| {
        panic!("case {case:?}: expected a port number in {reported:?} ({error})")
    });
    assert!(
        (FIRST_DYNAMIC_PORT..=LAST_DYNAMIC_PORT).contains(&port),
        "case {case:?}: expected the viewer's own end of the connection to be a dynamic port \
         ({FIRST_DYNAMIC_PORT}-{LAST_DYNAMIC_PORT}), got {port} in {reported:?}"
    );
}

/// The connection the origin was told about, off the origin's own report of the request it
/// served.
async fn reported_connection(
    case: &str,
    domain: &str,
    stated: &[(&str, &str)],
) -> String {
    let answered = Answered::of_stating(case, domain, "GET", OPEN_PATH, stated).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected the origin's answer through the edge, got {} with {:?}",
        answered.status, answered.raw
    );
    assert!(
        answered.reports_cache(MISS),
        "case {case:?}: expected an answer the origin produced for this request ({MISS:?}), got \
         {:?}",
        answered.raw
    );
    let report = answered.report(case);
    let reported = report["address"].as_str().unwrap_or_default().to_string();
    assert!(
        !reported.is_empty(),
        "case {case:?}: the origin was told no viewer connection at all ({report})"
    );
    reported
}

/// RULE: the edge tells an origin the viewer's whole CONNECTION — `<address>:<port>` — for
/// every viewer request, with the address half the one it observes itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_edge_reports_the_viewers_address_and_port_to_the_origin() {
    let topology = Topology::new();
    let setup = "a function URL origin reporting the connection the edge observed";
    let function = "cf-edge-viewer-connection";
    let origin = topology.public_function_url_host(setup, function).await;
    let domain = topology
        .deployed_distribution(setup, "edge-viewer-connection", &origin, "")
        .await;

    // The address the edge observes for a viewer that states none: read off the first such row
    // and held to being the same for every other one.
    let mut observed = String::new();
    for row in CONNECTION_ROWS {
        let reported = reported_connection(row.case, &domain, row.stated).await;
        match row.address {
            Some(expected) => assert_connection(row.case, &reported, expected),
            None => {
                if observed.is_empty() {
                    observed = halves(row.case, &reported).0;
                }
                assert_connection(row.case, &reported, &observed);
            }
        }
    }

    // One viewer has ONE address: the same viewer asked twice is reported the same address,
    // whichever connection each request arrived on, which is what an origin comparing two of
    // its requests reads. The PORT is the other half of a connection rather than of a viewer,
    // so nothing here holds two requests to one port.
    let again = "the same viewer asked a second time";
    let first = reported_connection(again, &domain, &[(FORWARDED_FOR, "203.0.113.7")]).await;
    let second = reported_connection(again, &domain, &[(FORWARDED_FOR, "203.0.113.7")]).await;
    assert_connection(again, &first, "203.0.113.7");
    assert_connection(again, &second, "203.0.113.7");
    assert_eq!(
        halves(again, &first).0,
        halves(again, &second).0,
        "case {again:?}: expected one viewer to be reported one address, got {first:?} then \
         {second:?}"
    );
}
