// SPDX-License-Identifier: CC0-1.0

//! The addresses this service publishes for a client to FETCH, read off answers delivered over
//! a real SOCKET: the hop a client holding one of those addresses actually takes.
//!
//! The rule under test is the addressing's, not one operation's: an address a response carries
//! for a client to dereference — a SaaS fulfillment option's `fulfillmentUrl` and `launchUrl`,
//! and a record's `logoThumbnailUrl` — is published on the address the ANSWERING REQUEST
//! ARRIVED ON, which is
//!
//! * the address a FRONT END states the client dialled it at, where a front end re-addressed
//!   the request to this process (the `Host` such a request carries is that front end's own
//!   upstream, so it is the one thing about the request that is NOT an address the client
//!   has), over the scheme that front end states;
//! * the address the request itself states, for a client that dialled this process directly;
//! * the address this process is SERVED at, for a request stating one that cannot lead a fetch
//!   back here.
//!
//! [`crate::announced_address_authority`] pins that reading over the authorities a client can
//! state, dispatched in process. This suite adds the one thing in-process dispatch cannot
//! show: that the answer a client reads off a socket announces the address IT dialled, and
//! that a GET of the announced path at the socket that answered serves the page the address
//! names — so the round trip a buyer takes after a subscribe (follow `fulfillmentUrl`, land on
//! the registration page) closes against a served emulator rather than only against a router
//! in the same process.

use std::io::{Read, Write};
use std::net::TcpStream;

use serde_json::{json, Value};

use crate::announced_address_authority::first_saas_listing;
use crate::catalog_drilldown::{array, text, AUTHORIZATION};

/// The model's route prefix for every operation of this service (`OPERATIONS.md`).
const ROUTE_PREFIX: &str = "/2026-02-05";

/// The largest page `SearchListingsInput$maxResults` admits (`@range 1..100`), so one call
/// reports as much of the catalog as a page can carry.
const MAX_PAGE: u64 = 100;

/// The longest a published `URL` may be (`@length(1..2048)`).
const MAX_URL_LENGTH: usize = 2048;

/// One answer read back off a socket: the status line's code and the body's bytes.
struct Answered {
    status: u16,
    body: Vec<u8>,
}

impl Answered {
    /// The answer's body as text, for a page.
    fn as_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The answer's body as JSON, for an operation's output.
    fn as_json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// One request written onto a socket of this emulator, with the whole answer read back.
///
/// `connection: close` so the server closes the socket after answering and the read ends on
/// EOF, which needs nothing of the response but its status line and its body.
fn exchange(
    port: u16,
    method: &str,
    target: &str,
    host: &str,
    stated: &[(&str, &str)],
    body: Option<&str>,
) -> Answered {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to the served port");
    let body = body.unwrap_or_default();
    let mut request = format!("{method} {target} HTTP/1.1\r\nhost: {host}\r\n");
    for (name, value) in stated {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str(&format!(
        "content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    ));
    stream
        .write_all(request.as_bytes())
        .expect("write the request");
    let mut answered = Vec::new();
    stream
        .read_to_end(&mut answered)
        .expect("read the whole answer");
    let head_end = answered
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("an answer with a head: {:?}", String::from_utf8_lossy(&answered)));
    let head = String::from_utf8_lossy(&answered[..head_end]).into_owned();
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("an answer with a status line: {head:?}"));
    Answered {
        status,
        body: answered[head_end + 4..].to_vec(),
    }
}

/// Call `operation` over the socket as a client that dialled `host` and whatever a front end
/// `stated` about where that client dialled it, requiring HTTP 200.
async fn called_over_the_socket(
    port: u16,
    case: &str,
    host: &str,
    stated: &'static [(&'static str, &'static str)],
    operation: &str,
    input: &Value,
) -> Value {
    let target = format!("{ROUTE_PREFIX}/{operation}");
    let host = host.to_string();
    let payload = input.to_string();
    let answered = tokio::task::spawn_blocking(move || {
        exchange(
            port,
            "POST",
            &target,
            &host,
            &[("authorization", AUTHORIZATION)]
                .into_iter()
                .chain(stated.iter().copied())
                .collect::<Vec<_>>(),
            Some(&payload),
        )
    })
    .await
    .expect("the socket exchange runs to completion");
    assert_eq!(
        answered.status,
        200,
        "case {case}: expected {operation} over the socket to answer HTTP 200, got {} ({:?})",
        answered.status,
        answered.as_text()
    );
    answered.as_json()
}

/// Fetch `path` at the socket that answered, the way a browser holding a published address
/// does: anonymously, over the path alone, with the announced authority as the `Host`.
async fn fetched_over_the_socket(port: u16, path: &str, host: &str) -> Answered {
    let path = path.to_string();
    let host = host.to_string();
    tokio::task::spawn_blocking(move || exchange(port, "GET", &path, &host, &[], None))
        .await
        .expect("the socket exchange runs to completion")
}

/// The authority and the path of a published absolute URL, which is how a client addresses it.
fn split_announced(url: &str) -> (String, String) {
    let without_scheme = url.split_once("://").map_or(url, |(_scheme, rest)| rest);
    match without_scheme.find('/') {
        Some(at) => (
            without_scheme[..at].to_string(),
            without_scheme[at..].to_string(),
        ),
        None => (without_scheme.to_string(), "/".to_string()),
    }
}

/// RULE: a published address satisfies the `URL` shape's own constraints — `@length(1..2048)`
/// and `@pattern ^https?://.*$` — and is published on `expected_base` and on nothing else.
fn assert_announced_on(case: &str, member: &str, expected_base: &str, url: &str) {
    assert!(
        (url.starts_with("https://") || url.starts_with("http://"))
            && (1..=MAX_URL_LENGTH).contains(&url.len()),
        "case {case} {member}: a published address must be a URL of 1..{MAX_URL_LENGTH} \
         characters matching ^https?://.*$, got {url:?} ({} characters)",
        url.len()
    );
    assert!(
        url.starts_with(&format!("{expected_base}/")),
        "case {case} {member}: expected the address published on {expected_base:?}, got {url:?}"
    );
}

/// One row: how a client reaches the served socket, and the base the addresses of the answer
/// it gets have to be published on.
struct Reached {
    /// What this row is, for every assertion of it.
    case: &'static str,
    /// Whether the `Host` the request carries is the served socket's own — which is what a
    /// front end re-addressing the request writes — or a host of the client's own choosing.
    host: Host,
    /// What a front end states about the address the client dialled it at, if anything.
    stated: &'static [(&'static str, &'static str)],
    /// The base the published addresses must carry, with `{served}` standing for the
    /// authority this process is served at (an ephemeral port, known only at run time).
    expected_base: &'static str,
}

/// Which host a row's request carries.
enum Host {
    /// The served socket's own authority, by its address.
    ServedSocket,
    /// The served socket by its loopback NAME, with the port the OS assigned.
    ServedByName,
    /// A host of the client's own, carried verbatim.
    Stated(&'static str),
}

/// The rows differ along what the rule generalises over: a client that dialled this socket
/// directly (by address and by name), a client that resolves one of AWS's own endpoint hosts,
/// and a request a front end re-addressed here — stating the address the client dialled with
/// its port, with the port stated separately, with the scheme the client dialled over, and
/// stating an address that cannot lead a fetch back here.
const ROWS: &[Reached] = &[
    Reached {
        case: "dialled this socket by address",
        host: Host::ServedSocket,
        stated: &[],
        expected_base: "https://{served}",
    },
    Reached {
        case: "dialled this socket by its loopback name",
        host: Host::ServedByName,
        stated: &[],
        expected_base: "https://localhost:{port}",
    },
    Reached {
        case: "a caller that resolves AWS's own endpoint hosts",
        host: Host::Stated("discovery-marketplace.us-east-1.amazonaws.com"),
        stated: &[],
        expected_base: "https://{served}",
    },
    Reached {
        case: "re-addressed here by a front end the client dialled at another port",
        host: Host::ServedSocket,
        stated: &[("x-forwarded-host", "127.0.0.1:38377")],
        expected_base: "https://127.0.0.1:38377",
    },
    Reached {
        case: "the same, with the dialled port stated on its own",
        host: Host::ServedSocket,
        stated: &[
            ("x-forwarded-host", "localhost"),
            ("x-forwarded-port", "38377"),
        ],
        expected_base: "https://localhost:38377",
    },
    Reached {
        case: "a front end the client dialled over plaintext",
        host: Host::ServedSocket,
        stated: &[
            ("x-forwarded-host", "127.0.0.1:38377"),
            ("x-forwarded-proto", "http"),
        ],
        expected_base: "http://127.0.0.1:38377",
    },
    Reached {
        case: "a front end stating an address that cannot lead a fetch back here",
        host: Host::ServedSocket,
        stated: &[("x-forwarded-host", "198.51.100.7:443")],
        expected_base: "https://{served}",
    },
];

/// RULE, end to end over a socket: every published address of an answer is on the address the
/// answering request arrived on, and the page that address names is served at the socket that
/// answered.
///
/// One served emulator, every row of [`ROWS`] walked through `ListFulfillmentOptions` (the
/// operation whose SaaS option publishes the buyer-facing pages) and `GetListing` (whose
/// `logoThumbnailUrl` is published by the same reading), with a GET of each announced path
/// taken at that socket.
#[tokio::test(flavor = "multi_thread")]
async fn a_served_answer_announces_its_addresses_on_the_address_the_request_arrived_on() {
    let router = crate::catalog_drilldown::router();
    let (endpoint, mut router) = emulator_server::serve_on_ephemeral_port(router).await;
    let port: u16 = endpoint
        .rsplit(':')
        .next()
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("an endpoint with a port, got {endpoint:?}"));
    // The base the data port publishes itself at, which the long-lived serve path hands the
    // router when it binds (`EmulatorService::serving_at`): every listener of a served
    // emulator terminates TLS, so that base is the `https` URL of the port bound above.
    let served = format!("127.0.0.1:{port}");
    router.set_url_base(
        smithy_server_runtime::EndpointUrl::parse(&format!("https://{served}"))
            .expect("a base URL the router accepts"),
    );

    // Which product's options to read is a property of the catalog, not of the row, so the
    // walk down to it is taken once, by a client that dialled this socket.
    let page = called_over_the_socket(
        port,
        "setup",
        &served,
        &[],
        "searchListings",
        &json!({ "maxResults": MAX_PAGE }),
    )
    .await;
    let summaries = array(&page, "listingSummaries");
    assert!(
        !summaries.is_empty(),
        "case setup: the seeded catalog must report listings, got {page}"
    );
    let listing_id = text(&first_saas_listing(&summaries), "listingId");

    let mut fetched_pages = 0_usize;
    for row in ROWS {
        let host = match row.host {
            Host::ServedSocket => served.clone(),
            Host::ServedByName => format!("localhost:{port}"),
            Host::Stated(host) => host.to_string(),
        };
        let expected_base = row
            .expected_base
            .replace("{served}", &served)
            .replace("{port}", &port.to_string());

        let listing = called_over_the_socket(
            port,
            row.case,
            &host,
            row.stated,
            "getListing",
            &json!({ "listingId": listing_id.clone() }),
        )
        .await;
        assert_announced_on(
            row.case,
            "GetListing logoThumbnailUrl",
            &expected_base,
            &text(&listing, "logoThumbnailUrl"),
        );

        let mut options = 0_usize;
        for entity in array(&listing, "associatedEntities") {
            let Some(product) = entity.get("product") else {
                continue;
            };
            let product_id = text(product, "productId");
            let fulfillment = called_over_the_socket(
                port,
                row.case,
                &host,
                row.stated,
                "listFulfillmentOptions",
                &json!({ "productId": product_id.clone(), "maxResults": 50 }),
            )
            .await;
            for option in array(&fulfillment, "fulfillmentOptions") {
                let Some(saas) = option.get("saasFulfillmentOption") else {
                    continue;
                };
                let option_id = text(saas, "fulfillmentOptionId");
                for member in ["fulfillmentUrl", "launchUrl"] {
                    let Some(url) = saas.get(member).and_then(Value::as_str) else {
                        continue;
                    };
                    assert_announced_on(row.case, member, &expected_base, url);
                    options += 1;

                    // And the page that address names is served at the socket that answered:
                    // the path is the announced one, and the authority stated for it is the
                    // announced authority — which is what a front end forwards here.
                    let (authority, path) = split_announced(url);
                    let page = fetched_over_the_socket(port, &path, &authority).await;
                    assert_eq!(
                        page.status, 200,
                        "case {} {member}: expected the page announced at {url:?} to be served \
                         at the socket that answered, got {} ({:?})",
                        row.case,
                        page.status,
                        page.as_text()
                    );
                    assert!(
                        page.as_text().contains(option_id.as_str()),
                        "case {} {member}: expected the page to restate the fulfillment option \
                         it is the page of ({option_id:?}), got {:?}",
                        row.case,
                        page.as_text()
                    );
                    fetched_pages += 1;
                }
            }
        }
        assert!(
            options > 0,
            "case {}: the walk must cover a fulfillment option that publishes a page",
            row.case
        );
    }
    assert!(
        fetched_pages >= ROWS.len(),
        "case setup: every row must have fetched at least one announced page, got \
         {fetched_pages} over {} rows",
        ROWS.len()
    );
}
