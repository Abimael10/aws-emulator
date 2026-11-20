// SPDX-License-Identifier: CC0-1.0

//! VIEWER traffic at the address a distribution PUBLISHES, dialled over a socket the way a
//! client handed that address dials it.
//!
//! RULE: an address an operation publishes for a resource's data plane is one this emulator
//! ANSWERS on, served by the SAME data plane every other addressing of that resource reaches —
//! so a client that takes `Distribution$DomainName` ("the domain name that CloudFront assigns
//! when you create a distribution") and fetches `<scheme>://<DomainName>/<object>` over a
//! connection of its own, with no AWS credential, is answered by the edge: the matched cache
//! behaviour's decisions, the object the origin serves, and the edge's own report of where the
//! answer came from (`X-Cache`). A published address that answered nothing, or that answered
//! out of a cache of its own, would make every edge decision unobservable to the one client
//! that was told to use it.
//!
//! Two suites drive that rule, each over a table rather than over one request:
//!
//! * the CACHE the dialled address serves out of is the behaviour's, keyed on exactly what its
//!   cache policy names: the rows differ along what that generalises over — a repeat of one
//!   request, another value for the query string the policy names, a parameter it names nowhere
//!   added beside that one, another value for such a parameter, how many parameters the request
//!   carries, and the position the named one was sent in — and each row asserts both answers it
//!   drew, so what separates two objects is read off the edge's own reporting;
//! * the dialled address and the addressings a client that cannot resolve a published name uses
//!   (the published authority stated as the `Host`, and the reserved data-plane path on the
//!   emulator's own endpoint) are ONE edge over ONE cache: every ordered pair of two of them
//!   fetches one object, and the second of the pair is served the entry the first populated.
//!
//! The origin is an endpoint this emulator really serves — this service's own control-plane
//! host, whose `GET /2020-05-31/distribution` answers a `DistributionList` — so what the viewer
//! is served is an origin's own answer rather than bytes invented for a fetch nothing made.
//!
//! The harness is the sibling suites': the router, the create and the deployment poll loop are
//! [`crate::distribution_lifecycle`]'s, the cache policy builder is
//! [`crate::edge_request_shaping`]'s, and the socket client that dials a published address is
//! [`crate::published_edge_address`]'s, read through [`crate::edge_lambda_url_origin::Answered`]
//! — each reused rather than copied.

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::announced_location_uri::addressed_hosts;
use crate::distribution_lifecycle::{
    awaited_deployment, create_distribution, emulator, get_distribution, is_reachable_authority,
    API_VERSION, HOST,
};
use crate::edge_lambda_url_origin::Answered;
use crate::edge_request_shaping::cache_policy;

/// An origin this emulator SERVES: this service's own control-plane endpoint.
const SERVED_ORIGIN: &str = HOST;

/// The element the origin's own answer carries, which is how a case tells an origin answer from
/// one the edge produced by itself.
const ORIGIN_DOCUMENT: &str = "DistributionList";

/// `X-Cache` for an answer the origin produced for this request, and for one the edge cache
/// held.
const MISS: &str = "Miss from cloudfront";
const HIT: &str = "Hit from cloudfront";

/// The query-string parameter the cache policy below NAMES, and two it names nowhere.
///
/// None of them is a parameter the served origin reads, so what the origin answers is the same
/// document whichever of them reached it: what a row reads is the EDGE's decision, never the
/// origin's reaction to a parameter.
const KEYED: &str = "a";
const UNKEYED: &str = "b";
const ALSO_UNKEYED: &str = "c";

/// How long the behaviour's cache policy holds an object: long enough that a second request of
/// a row is inside the window on any host, so what a row reads off `X-Cache` is the cache key
/// rather than the clock.
const CACHE_TTL: i64 = 3_600;

/// The viewer path every request below asks for, with `query` after it: the object the served
/// origin answers a [`ORIGIN_DOCUMENT`] for.
fn viewer_path(query: &str) -> String {
    format!("/{API_VERSION}/distribution{query}")
}

/// The `DistributionConfig` these suites serve through: the served origin, and a default
/// behaviour that allows and caches `GET` and `HEAD` under `cache_policy_id`.
fn config_xml(reference: &str, cache_policy_id: &str) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>primary</Id><DomainName>{SERVED_ORIGIN}</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy></CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <AllowedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items>\
         <CachedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items></CachedMethods>\
         </AllowedMethods>\
         <CachePolicyId>{cache_policy_id}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <Comment>viewer traffic at the published domain name</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

/// One deployed distribution over the served origin, under a cache policy that keys on
/// [`KEYED`] alone: its id and the address it PUBLISHED.
///
/// The published address is held to being one a client on this machine can dial, and to being
/// the RECORD's own rather than the minting call's — `GetDistribution` reports the same one —
/// and the distribution is waited for until it reports itself deployed, a configuration that has
/// not reached the edge locations serving no viewer.
async fn deployed(router: &MultiServiceRouter, case: &str, reference: &str) -> (String, String) {
    let policy = cache_policy(
        router,
        &format!("{reference}-policy"),
        "whitelist",
        &[KEYED],
        CACHE_TTL,
        false,
        false,
    )
    .await;
    let created = create_distribution(router, &config_xml(reference, &policy)).await;
    assert_eq!(
        created.status, 201,
        "case {case}: expected the create to be accepted with 201, got {} with {}",
        created.status, created.raw
    );
    let id = created
        .root()
        .find_text("Id")
        .unwrap_or_default()
        .to_string();
    let domain = created
        .root()
        .find_text("DomainName")
        .unwrap_or_default()
        .to_string();
    assert!(
        is_reachable_authority(&domain),
        "case {case}: expected a published DomainName a client can dial, got {domain:?}"
    );
    let read = get_distribution(router, &id).await;
    assert_eq!(
        read.root().find_text("DomainName"),
        Some(domain.as_str()),
        "case {case}: expected the record's own address to be the one the create published \
         ({domain:?}), got {:?}",
        read.root().find_text("DomainName")
    );
    awaited_deployment(router, case, &id).await;
    (id, domain)
}

// ---------------------------------------------------------------------------
// Reading one answer, however it was addressed
// ---------------------------------------------------------------------------

/// What one viewer request was answered with: the status, the edge's own report of where the
/// answer came from, and the bytes, which is where the origin's document is.
struct Served {
    status: u16,
    x_cache: String,
    body: String,
}

impl Served {
    /// One answer as it came off a CONNECTION to the published address.
    fn off_the_wire(answered: &Answered) -> Self {
        let head = answered
            .raw
            .split_once("\r\n\r\n")
            .map(|(head, _body)| head.to_string())
            .unwrap_or_else(|| answered.raw.clone());
        Self {
            status: answered.status,
            x_cache: header_value(&head, "x-cache"),
            body: answered.body.clone(),
        }
    }
}

/// The value of the `name` header out of an answer's head, read case-insensitively as a field
/// name is compared.
fn header_value(head: &str, name: &str) -> String {
    head.lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(field, _)| field.trim().eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim().to_string())
        .unwrap_or_default()
}

/// One VIEWER request placed IN PROCESS, addressed to `host`: no AWS credential, as a client
/// that cannot resolve a published name reaches the emulator it holds the address of.
async fn view(router: &MultiServiceRouter, host: &str, path_and_query: &str) -> Served {
    let request = ::http::Request::builder()
        .method("GET")
        .uri(path_and_query)
        .header("host", host)
        .body(Bytes::new())
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let x_cache = response
        .headers()
        .get("x-cache")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = String::from_utf8_lossy(&response.into_body()).to_string();
    Served {
        status,
        x_cache,
        body,
    }
}

// ---------------------------------------------------------------------------
// What the dialled address decides about one viewer request
// ---------------------------------------------------------------------------

/// One row of the cache table: two viewer requests dialled at the published address, and the
/// `X-Cache` the SECOND of them must be answered with.
struct CacheRow {
    case: &'static str,
    first: String,
    second: String,
    expected: &'static str,
}

/// RULE: a viewer that dials the address a distribution published is served by the matched cache
/// behaviour, out of a cache keyed on exactly what that behaviour's cache policy names — so two
/// requests the policy says are one object share the answer the first drew from the origin, and
/// two it says are different objects each draw their own.
///
/// Every row's FIRST request carries a value of its own for the named parameter, so each row
/// opens a cache entry no other row touches and a row's reading is its own.
#[tokio::test]
async fn the_dialled_published_address_serves_the_behaviours_cache() {
    let router = emulator();
    let case = "viewer traffic dialled at the published address";
    let (_id, domain) = deployed(&router, case, "published-viewer-cache").await;

    let rows = [
        CacheRow {
            case: "the same viewer request again",
            first: format!("?{KEYED}=one"),
            second: format!("?{KEYED}=one"),
            expected: HIT,
        },
        CacheRow {
            case: "another value for the query string the cache policy names",
            first: format!("?{KEYED}=two"),
            second: format!("?{KEYED}=three"),
            expected: MISS,
        },
        CacheRow {
            case: "a parameter no policy names, added beside the named one",
            first: format!("?{KEYED}=four"),
            second: format!("?{KEYED}=four&{UNKEYED}=9"),
            expected: HIT,
        },
        CacheRow {
            case: "another value for a parameter no policy names",
            first: format!("?{KEYED}=five&{UNKEYED}=1"),
            second: format!("?{KEYED}=five&{UNKEYED}=2"),
            expected: HIT,
        },
        CacheRow {
            case: "the named parameter sent in another position among more of them",
            first: format!("?{KEYED}=six&{UNKEYED}=1&{ALSO_UNKEYED}=2"),
            second: format!("?{UNKEYED}=3&{ALSO_UNKEYED}=4&{KEYED}=six"),
            expected: HIT,
        },
        CacheRow {
            case: "no query string at all, then one the policy names nothing of",
            first: String::new(),
            second: format!("?{UNKEYED}=seven"),
            expected: HIT,
        },
    ];

    for row in &rows {
        for (ordinal, query, expected) in [
            ("the first request", &row.first, MISS),
            ("the second request", &row.second, row.expected),
        ] {
            let answered = Answered::of_stating(row.case, &domain, "GET", &viewer_path(query), &[])
                .await;
            let served = Served::off_the_wire(&answered);
            assert_eq!(
                served.status, 200,
                "case {:?}: {ordinal} ({query:?}) expected the origin's 200 at the published \
                 address, got {} with {:?}",
                row.case, served.status, served.body
            );
            assert!(
                served.body.contains(ORIGIN_DOCUMENT),
                "case {:?}: {ordinal} ({query:?}) expected the origin's own {ORIGIN_DOCUMENT}, \
                 got {:?}",
                row.case,
                served.body
            );
            assert_eq!(
                served.x_cache, expected,
                "case {:?}: {ordinal} ({query:?}) expected X-Cache {expected:?}, got {:?}",
                row.case, served.x_cache
            );
        }
    }
}

// ---------------------------------------------------------------------------
// One edge, reached from every addressing of it
// ---------------------------------------------------------------------------

/// How a client reaches one distribution's edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Addressing {
    /// A connection OPENED to the address the distribution published, which is what a client
    /// handed that address does.
    Dialled,
    /// The published authority stated as the `Host` of a request placed in process.
    HostStyle,
    /// The reserved data-plane path, naming the distribution's id, on the emulator's own
    /// endpoint — the addressing a client that resolves no published name has.
    PrefixedPath,
}

impl Addressing {
    /// The row's own name for this addressing, which every assertion names its case by.
    fn case(self) -> &'static str {
        match self {
            Self::Dialled => "dialled at the published address",
            Self::HostStyle => "the published authority as the Host",
            Self::PrefixedPath => "the reserved data-plane path on the emulator's own endpoint",
        }
    }

    /// One viewer request for the object `query` names, placed by this addressing.
    async fn fetch(
        self,
        case: &str,
        router: &MultiServiceRouter,
        domain: &str,
        id: &str,
        own_endpoint: &str,
        query: &str,
    ) -> Served {
        match self {
            Self::Dialled => {
                let answered =
                    Answered::of_stating(case, domain, "GET", &viewer_path(query), &[]).await;
                Served::off_the_wire(&answered)
            }
            Self::HostStyle => view(router, domain, &viewer_path(query)).await,
            Self::PrefixedPath => {
                view(
                    router,
                    own_endpoint,
                    &format!("/_aws/cloudfront/{id}{}", viewer_path(query)),
                )
                .await
            }
        }
    }
}

/// RULE: the address a distribution publishes is served by the SAME edge, over the same cache,
/// as every other addressing of that distribution — one data plane, reached from several
/// addressings — so an object one of them fetched from the origin is served to the next out of
/// the cache that fetch populated.
///
/// The rows are every ORDERED pair of two distinct addressings, so each addressing is driven both
/// as the one that reaches the origin and as the one served the entry another opened, and each
/// pair fetches an object of its own.
#[tokio::test]
async fn every_addressing_of_the_published_edge_shares_one_cache() {
    let router = emulator();
    let case = "one edge behind every addressing";
    let (id, domain) = deployed(&router, case, "published-viewer-addressing").await;
    // The own-endpoint authority is a port THIS process holds, so no edge listener of this
    // engine can be given it and the row addresses the emulator's own endpoint rather than some
    // distribution's edge. The reservation lives as long as `hosts` does.
    let hosts = addressed_hosts();
    let own_endpoint = hosts
        .rows
        .iter()
        .find(|row| row.own_endpoint)
        .map(|row| row.host.clone())
        .expect("the shared table offers an authority of the emulator's own endpoint");

    let addressings = [
        Addressing::Dialled,
        Addressing::HostStyle,
        Addressing::PrefixedPath,
    ];
    for (first_index, first) in addressings.iter().enumerate() {
        for (second_index, second) in addressings.iter().enumerate() {
            if first == second {
                continue;
            }
            let case = format!("{} then {}", first.case(), second.case());
            let query = format!("?{KEYED}=pair{first_index}{second_index}");
            for (addressing, ordinal, expected) in [
                (first, "the first request", MISS),
                (second, "the second request", HIT),
            ] {
                let served = addressing
                    .fetch(&case, &router, &domain, &id, &own_endpoint, &query)
                    .await;
                assert_eq!(
                    served.status, 200,
                    "case {case:?}: {ordinal} expected the origin's 200, got {} with {:?}",
                    served.status, served.body
                );
                assert!(
                    served.body.contains(ORIGIN_DOCUMENT),
                    "case {case:?}: {ordinal} expected the origin's own {ORIGIN_DOCUMENT}, got \
                     {:?}",
                    served.body
                );
                assert_eq!(
                    served.x_cache, expected,
                    "case {case:?}: {ordinal} expected X-Cache {expected:?}, got {:?}",
                    served.x_cache
                );
            }
        }
    }
}
