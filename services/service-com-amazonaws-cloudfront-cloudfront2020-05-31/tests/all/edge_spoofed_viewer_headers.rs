// SPDX-License-Identifier: CC0-1.0

//! What a viewer that STATES a `CloudFront-*` request header of its own gets, end to end at
//! the address a deployed distribution published.
//!
//! The `CloudFront-*` request-header family describes the VIEWER'S CONNECTION — where the
//! viewer is, which scheme it dialled, the address it came from — so the edge resolves every
//! one of those headers itself and a value the viewer states for one is dropped before the
//! origin request is built. Two consequences, and this suite drives both over a table:
//!
//! * what the ORIGIN is told for such a header is the EDGE's own value, never the viewer's:
//!   the scheme reported is the one really dialled, the address reported is the one the edge
//!   observes for the connection, and the country reported is the edge's own resolution of
//!   it — a two-letter uppercase code whatever the viewer spelled. A member of the family the
//!   edge does not resolve reaches the origin NOT AT ALL, even under an origin request policy
//!   that forwards every viewer header and names that header: the viewer's value is not that
//!   header's value, so there is nothing to forward. And a `CloudFront-*` header reaches the
//!   origin only where the matched behaviour's origin request policy names it, so a behaviour
//!   whose policy names none sees none of them while still seeing the viewer's own headers;
//! * what the CACHE KEY carries for such a header is that same edge-resolved value, so a
//!   viewer cannot split or share a cached object by stating one: two requests the edge
//!   resolves to the same value are ONE object (the second is served from the cache and the
//!   origin is not asked), two it resolves differently are two, and a member of the family
//!   the edge does not resolve separates nothing at all.
//!
//! The rows differ along what those rules generalise over: which member of the family the
//! viewer states (the three the edge resolves, and others it does not), the case it spells the
//! name in, the value it states (a plausible one, one that is no country, a code ISO reserves
//! for user assignment, a scheme it did not dial, an address it did not come from), how many
//! it states at once, whether the request also carries the proxy header the edge DOES read the
//! viewer's address from, and the origin request policy of the behaviour it matches.
//!
//! The origin is a Lambda function URL that REPORTS the request it was handed, and Lambda's own
//! `Invocations` series says whether the origin was reached at all — so "the origin never saw
//! the viewer's value" is read off the origin's own report rather than from an absence in the
//! answer. The function, the distribution and the dial are
//! [`crate::edge_lambda_url_origin`]'s, reused rather than copied.
//!
//! ## The one channel a viewer of a LOOPBACK emulator still moves
//!
//! Of the three members this edge resolves, two are read off the connection and nothing a
//! viewer sends can move them. The COUNTRY is the exception, and deliberately so: every viewer
//! here dials a loopback address, which is in no country any geographic database places, so the
//! edge resolves the country from the one header a viewer of this emulator has to express it in
//! and normalizes what it finds there — the addressing convention stated in
//! `src/service/edge/generated_headers.rs` and relied on by
//! [`crate::edge_geo_restriction`], where the same channel is how a row says which country its
//! viewer is in. So the rows below hold that member to the edge's own RESOLUTION (a two-letter
//! uppercase code, the edge's own wherever what the viewer stated is no country) rather than to
//! being unmovable, and the cache rows hold the key to carrying that resolution rather than the
//! viewer's text. Against real CloudFront the country comes from the viewer's address instead,
//! so there a viewer moves neither; the limitation is this emulator's addressing, not a
//! property of the family, and it is named here rather than asserted as if it were AWS's.

use serde_json::Value;

use crate::distribution_lifecycle::API_VERSION;
use crate::edge_lambda_url_origin::{
    element, header_names, viewer_ip, Answered, Topology, ALL_VIEWER_AND_CLOUDFRONT,
    ALL_VIEWER_EXCEPT_HOST, MISS, OPEN_PATH, ORIGIN_MARKER,
};
// `X-Cache` for an answer the edge cache held, beside the `MISS` the origin produced: the one
// the sibling suites already share rather than a copy of the string.
use crate::edge_origin_response_headers::HIT;
use crate::edge_signed_access::CACHING_DISABLED;

/// The prefix of the header family the edge resolves for itself.
const FAMILY: &str = "cloudfront-";

/// The three members of that family this edge resolves, lower case as an origin request carries
/// them: the viewer's country, the scheme it dialled and the address it came from.
const RESOLVED: [&str; 3] = [
    "cloudfront-viewer-country",
    "cloudfront-forwarded-proto",
    "cloudfront-viewer-address",
];

/// The header a viewer that traversed a proxy states its own address in, which is where the
/// edge reads the address of a request that crossed no socket of its own.
const FORWARDED_FOR: &str = "x-forwarded-for";

/// A header of the viewer's OWN, carried by every row: a policy that forwards viewer headers
/// forwards this one, so a row that finds no `CloudFront-*` header at the origin is reading a
/// request that did carry the viewer's headers rather than one that carried nothing.
const VIEWER_HEADER: (&str, &str) = ("accept-language", "en-GB");

/// The scheme every dial below really uses: these viewers dial the plaintext half of the one
/// address the distribution published.
const DIALLED_SCHEME: &str = "http";

/// Values no viewer decides, stated for members of the family the viewer does not decide.
/// Each is distinctive, so a row can hold the WHOLE answer to carrying none of it.
const SPOOFED_CITY: &str = "Spoofville";
const SPOOFED_ADDRESS: &str = "203.0.113.9";
const SPOOFED_TLS: &str = "TLSv1.3:TLS_AES_128_GCM_SHA256:spoofedHandshake";

/// The TTLs the cache rows pin their own cache policy to, so an answer the origin produced can
/// only mean the edge computed a different cache key for that request — the suite's own
/// pinning, not a duration the service owes anyone.
const PINNED_TTL: i64 = 3600;

/// The header the cache rows' policy keys on: the one member of the family whose edge-resolved
/// value a row can vary from the viewer's side.
const KEYED_HEADER: &str = "CloudFront-Viewer-Country";

/// One `DistributionConfig` over the function URL `origin` whose default behaviour carries
/// `cache_policy` and `origin_request_policy` — the two policies that decide what the cache key
/// carries and what reaches the origin beside it.
fn config_xml(
    caller_reference: &str,
    origin: &str,
    cache_policy: &str,
    origin_request_policy: &str,
) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>function-url</Id><DomainName>{origin}</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy>\
         <OriginSslProtocols><Quantity>1</Quantity>\
         <Items><SslProtocol>TLSv1.2</SslProtocol></Items></OriginSslProtocols>\
         </CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>function-url</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <AllowedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items></AllowedMethods>\
         <CachePolicyId>{cache_policy}</CachePolicyId>\
         <OriginRequestPolicyId>{origin_request_policy}</OriginRequestPolicyId>\
         </DefaultCacheBehavior>\
         <Comment>a function url behind a distribution</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

/// One deployed distribution over `origin` carrying those two policies: the `DomainName` a
/// viewer dials.
async fn deployed(
    topology: &Topology,
    case: &str,
    caller_reference: &str,
    origin: &str,
    cache_policy: &str,
    origin_request_policy: &str,
) -> String {
    let (id, _arn, domain) = topology
        .created_distribution(
            case,
            &config_xml(caller_reference, origin, cache_policy, origin_request_policy),
        )
        .await;
    topology.awaited_deployment(case, &id).await;
    domain
}

/// The `CloudFront-*` header names the origin was handed.
fn family_at_the_origin(report: &Value) -> Vec<String> {
    header_names(report)
        .into_iter()
        .filter(|name| name.starts_with(FAMILY))
        .collect()
}

/// One spoofing row: what the viewer states beside its own headers, and what the ORIGIN must
/// then be told about the connection.
struct SpoofRow {
    case: &'static str,
    /// The headers the viewer states of its own accord.
    stated: &'static [(&'static str, &'static str)],
    /// The country the origin must be told, or `None` where the edge's OWN country is what it
    /// reports — one value for every such row, which the rows hold it to.
    country: Option<&'static str>,
    /// The address the origin must be told, or `None` where the address the edge observes for a
    /// viewer that states nothing is what it reports.
    address: Option<&'static str>,
    /// The distinctive values this row stated that must appear NOWHERE in what the edge
    /// answered — neither relayed to the origin nor echoed back.
    absent: &'static [&'static str],
}

/// The rows: every one of them dials the published address in the clear and states
/// [`VIEWER_HEADER`] beside what it spoofs.
const SPOOF_ROWS: &[SpoofRow] = &[
    SpoofRow {
        case: "a viewer that states nothing of the family",
        stated: &[],
        country: None,
        address: None,
        absent: &[],
    },
    SpoofRow {
        case: "a spoofed forwarded scheme",
        stated: &[("cloudfront-forwarded-proto", "https")],
        country: None,
        address: None,
        absent: &[],
    },
    SpoofRow {
        case: "a spoofed viewer address",
        stated: &[("cloudfront-viewer-address", SPOOFED_ADDRESS)],
        country: None,
        address: None,
        absent: &[SPOOFED_ADDRESS],
    },
    SpoofRow {
        case: "a spoofed address beside the proxy header the edge does read",
        stated: &[
            ("cloudfront-viewer-address", SPOOFED_ADDRESS),
            (FORWARDED_FOR, "198.51.100.5"),
        ],
        country: None,
        address: Some("198.51.100.5"),
        absent: &[SPOOFED_ADDRESS],
    },
    SpoofRow {
        case: "a country stated in lower case, which the edge resolves and normalizes",
        stated: &[("cloudfront-viewer-country", "de")],
        country: Some("DE"),
        address: None,
        absent: &[],
    },
    SpoofRow {
        case: "a code ISO reserves for user assignment, which is no country",
        stated: &[("cloudfront-viewer-country", "ZZ")],
        country: None,
        address: None,
        absent: &[],
    },
    SpoofRow {
        case: "a value that is no country code at all",
        stated: &[("cloudfront-viewer-country", "Elbonia")],
        country: None,
        address: None,
        absent: &["Elbonia"],
    },
    SpoofRow {
        case: "members of the family the edge does not resolve",
        stated: &[
            ("cloudfront-viewer-city", SPOOFED_CITY),
            ("cloudfront-is-mobile-viewer", "true"),
            ("cloudfront-viewer-tls", SPOOFED_TLS),
        ],
        country: None,
        address: None,
        absent: &[SPOOFED_CITY, SPOOFED_TLS],
    },
    SpoofRow {
        case: "the whole set, in the spelling a browser sends a header name in",
        stated: &[
            ("CloudFront-Forwarded-Proto", "https"),
            ("CloudFront-Viewer-Country", "de"),
            ("CloudFront-Viewer-Address", SPOOFED_ADDRESS),
            ("CloudFront-Viewer-City", SPOOFED_CITY),
        ],
        country: Some("DE"),
        address: None,
        absent: &[SPOOFED_ADDRESS, SPOOFED_CITY],
    },
];

/// The headers one row's viewer sends: what it spoofs, and its own [`VIEWER_HEADER`].
fn stated_headers(row: &SpoofRow) -> Vec<(&'static str, &'static str)> {
    let mut headers: Vec<(&str, &str)> = row.stated.to_vec();
    headers.push(VIEWER_HEADER);
    headers
}

/// RULE: a `CloudFront-*` request header describes the viewer's connection rather than anything
/// the viewer asked for, so the edge resolves each one it knows itself and DROPS whatever the
/// viewer stated for any member of the family — the origin is told the edge's own value for a
/// member the edge resolves, and nothing at all for one it does not, and only where the matched
/// behaviour's origin request policy names that header.
///
/// Driven at the address a `Deployed` distribution published, over a Lambda function URL origin
/// that reports the request it was handed; the rows span the members of the family, the
/// spellings and values a viewer can state, and the two origin request policies that do and do
/// not name the family.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_edge_resolves_the_cloudfront_headers_a_viewer_tries_to_state() {
    let topology = Topology::new();
    let setup = "a function URL origin reporting the request the edge made";
    let function = "cf-edge-spoofed-headers";
    let origin = topology.public_function_url_host(setup, function).await;

    // One behaviour whose origin request policy NAMES the whole family (and forwards every
    // viewer header), and one whose policy names none of it: what reaches the origin differs
    // between them, and in neither does a value the viewer stated for a family member reach it.
    let naming = deployed(
        &topology,
        setup,
        "edge-spoofed-headers-named",
        &origin,
        CACHING_DISABLED,
        ALL_VIEWER_AND_CLOUDFRONT,
    )
    .await;
    let unnaming = deployed(
        &topology,
        setup,
        "edge-spoofed-headers-unnamed",
        &origin,
        CACHING_DISABLED,
        ALL_VIEWER_EXCEPT_HOST,
    )
    .await;

    // The two values the edge resolves for a viewer that tells it nothing: read off the first
    // row and held to being the same for every row that states nothing of its own.
    let mut edge_country = String::new();
    let mut observed_address = String::new();

    for row in SPOOF_ROWS {
        let case = row.case;
        let stated = stated_headers(row);
        let before = topology.invocations(case, function).await;
        let answered = Answered::of_stating(case, &naming, "GET", OPEN_PATH, &stated).await;
        let after = topology.invocations(case, function).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the origin's answer through the edge, got {} with {:?}",
            answered.status, answered.raw
        );
        assert!(
            answered.reports_cache(MISS),
            "case {case:?}: expected an answer the origin produced for this request ({MISS:?}), \
             got {:?}",
            answered.raw
        );
        assert!(
            answered.carries_the_origins_bytes(),
            "case {case:?}: expected the origin's own bytes, got {:?}",
            answered.body
        );
        assert!(
            after > before,
            "case {case:?}: the fetch must reach the origin, but the function's invocations \
             stayed at {before} (now {after})"
        );
        let report = answered.report(case);

        // Only the members the edge resolves reach the origin: a member the viewer stated and
        // the edge does not resolve is no header of the origin request, whatever the policy
        // names.
        let mut family = family_at_the_origin(&report);
        family.sort();
        let mut expected = RESOLVED.map(str::to_string).to_vec();
        expected.sort();
        assert_eq!(
            family, expected,
            "case {case:?}: expected exactly the family members the edge resolves at the \
             origin, got {family:?} ({report})"
        );

        // The scheme is the one really dialled, never the one a viewer stated for it.
        let proto = report["proto"].as_str().unwrap_or_default();
        assert_eq!(
            proto, DIALLED_SCHEME,
            "case {case:?}: expected the origin to be told the scheme the viewer dialled \
             ({DIALLED_SCHEME:?}), got {proto:?} ({report})"
        );

        // The country is the edge's own resolution: a two-letter uppercase code whatever the
        // viewer spelled, and the edge's own wherever what it spelled is no country.
        let country = report["country"].as_str().unwrap_or_default().to_string();
        assert!(
            country.len() == 2 && country.chars().all(|letter| letter.is_ascii_uppercase()),
            "case {case:?}: expected a two-letter uppercase country code at the origin, got \
             {country:?} ({report})"
        );
        match row.country {
            Some(expected) => assert_eq!(
                country, expected,
                "case {case:?}: expected the origin to be told the country {expected:?}, got \
                 {country:?}"
            ),
            None => {
                if edge_country.is_empty() {
                    edge_country = country.clone();
                }
                assert_eq!(
                    country, edge_country,
                    "case {case:?}: expected the edge's own country {edge_country:?} for a \
                     viewer it was told no code for, got {country:?}"
                );
            }
        }

        // The address is the one the edge observes for the connection: the proxy header where
        // the request carries one, and the address a viewer that states nothing came from
        // otherwise — never an address stated for the family member that reports it.
        let reported = report["address"].as_str().unwrap_or_default().to_string();
        let address = viewer_ip(&reported);
        assert!(
            address.parse::<::std::net::IpAddr>().is_ok(),
            "case {case:?}: expected the origin to be told the viewer's address, got \
             {reported:?} ({report})"
        );
        match row.address {
            Some(expected) => assert_eq!(
                address, expected,
                "case {case:?}: expected the origin to be told the address the edge observes \
                 ({expected:?}), got {address:?}"
            ),
            None => {
                if observed_address.is_empty() {
                    observed_address = address.clone();
                }
                assert_eq!(
                    address, observed_address,
                    "case {case:?}: expected the address the edge observes for a viewer that \
                     states none ({observed_address:?}), got {address:?}"
                );
            }
        }

        // Nothing distinctive the viewer stated for a family member is anywhere in what the
        // origin reported or in what the edge answered.
        for value in row.absent {
            assert!(
                !answered.raw.contains(*value),
                "case {case:?}: the value {value:?} the viewer stated for a CloudFront header \
                 must reach neither the origin nor the answer, got {:?}",
                answered.raw
            );
        }

        // The viewer's OWN headers do travel under this policy, so the absences above are the
        // family's and not the whole request's.
        let names = header_names(&report);
        assert!(
            names.iter().any(|name| name == VIEWER_HEADER.0),
            "case {case:?}: expected the viewer's own {:?} at the origin, got {names:?}",
            VIEWER_HEADER.0
        );

        // The same viewer against the behaviour whose origin request policy names NO member of
        // the family: the origin sees none of them, and still sees the viewer's own headers.
        let answered = Answered::of_stating(case, &unnaming, "GET", OPEN_PATH, &stated).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?} under a policy naming no CloudFront header: expected the origin's \
             answer, got {} with {:?}",
            answered.status, answered.raw
        );
        let report = answered.report(case);
        let family = family_at_the_origin(&report);
        assert!(
            family.is_empty(),
            "case {case:?} under a policy naming no CloudFront header: expected the origin to \
             see none of the family, got {family:?} ({report})"
        );
        let names = header_names(&report);
        assert!(
            names.iter().any(|name| name == VIEWER_HEADER.0),
            "case {case:?} under a policy naming no CloudFront header: expected the viewer's \
             own {:?} at the origin, got {names:?}",
            VIEWER_HEADER.0
        );
    }
}

/// One cache row: what the viewer states, whether the edge must serve the answer it already
/// holds, and whether the origin must be asked at all.
struct CacheRow {
    case: &'static str,
    path: &'static str,
    stated: &'static [(&'static str, &'static str)],
    /// `true` where the edge must serve this request from the object it already holds.
    cached: bool,
}

/// The rows, in order: each one is sent after the ones above it, so what the edge already holds
/// is what those rows put there.
const CACHE_ROWS: &[CacheRow] = &[
    CacheRow {
        case: "the first request for the object",
        path: OPEN_PATH,
        stated: &[],
        cached: false,
    },
    CacheRow {
        case: "the same request again",
        path: OPEN_PATH,
        stated: &[],
        cached: true,
    },
    CacheRow {
        case: "a viewer stating a value that is no country, which the edge resolves the same way",
        path: OPEN_PATH,
        stated: &[("cloudfront-viewer-country", "Elbonia")],
        cached: true,
    },
    CacheRow {
        case: "a viewer stating a member of the family the edge does not resolve",
        path: OPEN_PATH,
        stated: &[("cloudfront-viewer-city", SPOOFED_CITY)],
        cached: true,
    },
    CacheRow {
        case: "a viewer the edge resolves to another country, which is another object",
        path: OPEN_PATH,
        stated: &[("cloudfront-viewer-country", "DE")],
        cached: false,
    },
    CacheRow {
        case: "the same country spelled in lower case, which is that same object",
        path: OPEN_PATH,
        stated: &[("cloudfront-viewer-country", "de")],
        cached: true,
    },
    CacheRow {
        case: "another path, which is another object again",
        path: "/elsewhere",
        stated: &[],
        cached: false,
    },
];

/// RULE: a cache key carries the edge's OWN value for a `CloudFront-*` header the cache policy
/// keys on, so stating one cannot split or share a cached object: requests the edge resolves to
/// the same value are one object, requests it resolves differently are different objects, and a
/// member of the family the edge does not resolve takes no part in the key at all.
///
/// The behaviour's cache policy pins `MinTTL`, `DefaultTTL` and `MaxTTL` to one hour, so an
/// answer the origin produced can only mean the edge computed a different cache key for that
/// request; whether the origin was asked is read off Lambda's own `Invocations` series for the
/// function rather than off the answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cloudfront_header_a_viewer_states_takes_no_part_in_the_cache_key() {
    let topology = Topology::new();
    let setup = "a cached function URL origin keyed on the viewer's country";
    let function = "cf-edge-spoofed-header-key";
    let origin = topology.public_function_url_host(setup, function).await;

    // A cache policy that KEYS on the one family member whose resolved value a viewer can move,
    // with every TTL pinned to the same hour.
    let (status, raw) = topology
        .cloudfront(
            setup,
            "POST",
            &format!("/{API_VERSION}/cache-policy"),
            Some(&format!(
                "<CachePolicyConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
                 <Name>edge-spoofed-header-key</Name>\
                 <MinTTL>{PINNED_TTL}</MinTTL><DefaultTTL>{PINNED_TTL}</DefaultTTL>\
                 <MaxTTL>{PINNED_TTL}</MaxTTL>\
                 <ParametersInCacheKeyAndForwardedToOrigin>\
                 <EnableAcceptEncodingGzip>false</EnableAcceptEncodingGzip>\
                 <EnableAcceptEncodingBrotli>false</EnableAcceptEncodingBrotli>\
                 <HeadersConfig><HeaderBehavior>whitelist</HeaderBehavior>\
                 <Headers><Quantity>1</Quantity><Items><Name>{KEYED_HEADER}</Name></Items>\
                 </Headers></HeadersConfig>\
                 <CookiesConfig><CookieBehavior>none</CookieBehavior></CookiesConfig>\
                 <QueryStringsConfig><QueryStringBehavior>none</QueryStringBehavior>\
                 </QueryStringsConfig>\
                 </ParametersInCacheKeyAndForwardedToOrigin>\
                 </CachePolicyConfig>"
            )),
        )
        .await;
    assert_eq!(
        status, 201,
        "setup {setup:?}: CreateCachePolicy must be accepted, got {status} with {raw}"
    );
    let cache_policy = element(&raw, "Id");
    assert!(
        !cache_policy.is_empty(),
        "setup {setup:?}: the created cache policy reports no id: {raw}"
    );

    let domain = deployed(
        &topology,
        setup,
        "edge-spoofed-header-key",
        &origin,
        &cache_policy,
        ALL_VIEWER_AND_CLOUDFRONT,
    )
    .await;

    for row in CACHE_ROWS {
        let case = row.case;
        let mut stated: Vec<(&str, &str)> = row.stated.to_vec();
        stated.push(VIEWER_HEADER);
        let before = topology.invocations(case, function).await;
        let answered = Answered::of_stating(case, &domain, "GET", row.path, &stated).await;
        let after = topology.invocations(case, function).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the object to be served, got {} with {:?}",
            answered.status, answered.raw
        );
        assert!(
            answered.body.contains(ORIGIN_MARKER),
            "case {case:?}: expected the origin's own bytes, served or cached, got {:?}",
            answered.body
        );
        let expected_cache = if row.cached { HIT } else { MISS };
        assert!(
            answered.reports_cache(expected_cache),
            "case {case:?}: expected X-Cache {expected_cache:?}, got {:?}",
            answered.raw
        );
        if row.cached {
            assert!(
                (after - before).abs() < 0.5,
                "case {case:?}: an answer the edge already holds must ask the origin for \
                 nothing, but the function's invocations moved from {before} to {after}"
            );
        } else {
            assert!(
                after > before,
                "case {case:?}: a different cache key must be fetched from the origin, but the \
                 function's invocations stayed at {before} (now {after})"
            );
        }
    }
}
