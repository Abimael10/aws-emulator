// SPDX-License-Identifier: CC0-1.0

//! What the ORIGIN says about storing its own answer, end to end over HTTP: a distribution over a
//! Lambda function URL origin whose handler answers a FRESH body every time it is invoked, so the
//! body a viewer receives says whether the edge held the previous answer or asked the origin
//! again.
//!
//! RULE: the edge does not hold an origin answer whose own `Cache-Control` forbids a shared cache
//! from storing it, whatever span the distribution's configuration states for that answer — a
//! cache behaviour's own TTL for content, `CustomErrorResponses`' `ErrorCachingMinTTL` for an
//! error — and it holds an answer that forbids nothing for that span. `CacheBehavior` states the
//! one exception: "if your minimum TTL is greater than 0, CloudFront will cache content for at
//! least the duration specified in the cache policy's minimum TTL, even if the `Cache-Control:
//! no-cache`, `no-store`, or `private` directives are present in the origin headers" — about
//! CONTENT, so an error the origin asks no cache to store is re-fetched however long
//! `ErrorCachingMinTTL` is.
//!
//! The rows differ along what the rule generalises over: the directive the origin stated (none,
//! each of the three that forbid storage, and a freshness lifetime that forbids none), the STATUS
//! the answer carried (content against an error the configuration names an `ErrorCachingMinTTL`
//! for), and the behaviour's minimum TTL (zero against a span, which is the documented
//! exception). Every row asks for its own path twice back to back and reads the two bodies: the
//! handler reports the edge's own per-request identifier, so two equal bodies are one origin
//! answer served twice and two different bodies are two origin invocations.

use crate::distribution_lifecycle::API_VERSION;
use crate::edge_lambda_url_origin::{element, Topology};

/// The function behind the origin, and the distributions in front of it.
const FUNCTION: &str = "cf-edge-origin-cache-directives";

/// How long the behaviour of both distributions holds what it may hold, and how long the
/// configuration asks for an error to be held: a span no case can outlive, so a row that reads
/// two equal bodies read a held answer rather than an expired one.
const HELD_SECONDS: u64 = 3_600;

/// The statuses the origin answers under: content, and the error the configuration names an
/// `ErrorCachingMinTTL` for.
const CONTENT: u16 = 200;
const ERROR: u16 = 502;

/// The handler behind the function URL: it answers the STATUS and the `Cache-Control` the viewer
/// path names, with a body that is fresh on every invocation — the edge's own request identifier,
/// which is unique per viewer request, is part of it — so the body says whether this answer came
/// out of the edge cache or out of the origin. It imports nothing: the answer is the handler's own
/// work.
const HANDLER_SOURCE: &str = r#"
def lambda_handler(event, context):
    headers = event.get("headers") or {}
    directives = {
        "no-store": "no-store",
        "no-cache": "no-cache",
        "private": "private",
        "max-age": "max-age=120",
    }
    segments = [segment for segment in (event.get("rawPath") or "").split("/") if segment]
    stated = segments[0] if segments else "none"
    status = int(segments[1]) if len(segments) > 1 else 200
    answered = {"content-type": "text/plain"}
    if stated in directives:
        answered["cache-control"] = directives[stated]
    return {
        "statusCode": status,
        "headers": answered,
        "body": stated + "-" + str(status) + "-" + headers.get("x-amz-cf-id", ""),
    }
"#;

/// One row: the directive the origin states, the status it answers, and whether the edge holds
/// that answer.
struct Row {
    case: &'static str,
    /// The first path segment, which is what the handler reads the directive off.
    directive: &'static str,
    status: u16,
    /// Whether the immediate repeat is served the answer the edge already has.
    held: bool,
}

/// The `DistributionConfig` of a distribution over the function URL `origin` whose behaviour
/// states the LEGACY per-behaviour TTLs — so the minimum TTL under test is the configuration's own
/// rather than a cache policy's — and whose `CustomErrorResponses` names [`ERROR`] with an
/// `ErrorCachingMinTTL` of [`HELD_SECONDS`] and no page, which states the caching duration alone.
fn config_xml(caller_reference: &str, origin: &str, min_ttl: u64) -> String {
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
         <Items><Method>GET</Method><Method>HEAD</Method></Items>\
         <CachedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items></CachedMethods>\
         </AllowedMethods>\
         <ForwardedValues><QueryString>false</QueryString>\
         <Cookies><Forward>none</Forward></Cookies>\
         <Headers><Quantity>0</Quantity></Headers>\
         <QueryStringCacheKeys><Quantity>0</Quantity></QueryStringCacheKeys>\
         </ForwardedValues>\
         <MinTTL>{min_ttl}</MinTTL><DefaultTTL>{HELD_SECONDS}</DefaultTTL>\
         <MaxTTL>{HELD_SECONDS}</MaxTTL>\
         </DefaultCacheBehavior>\
         <CacheBehaviors><Quantity>0</Quantity></CacheBehaviors>\
         <CustomErrorResponses><Quantity>1</Quantity><Items><CustomErrorResponse>\
         <ErrorCode>{ERROR}</ErrorCode>\
         <ErrorCachingMinTTL>{HELD_SECONDS}</ErrorCachingMinTTL>\
         </CustomErrorResponse></Items></CustomErrorResponses>\
         <Comment>what the origin says about storing its answer</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

/// One VIEWER request through the edge: no credential, addressed to `domain`, which is what
/// resolves the distribution. Answers the status and the body.
async fn view(topology: &Topology, case: &str, domain: &str, path: &str) -> (u16, String) {
    let request = ::http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", domain)
        .body(::bytes::Bytes::new())
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the viewer request is well formed: {error}")
        });
    let response = topology.router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(&response.into_body()).to_string();
    (status, body)
}

/// One deployed distribution over `origin` whose behaviour states `min_ttl` as its minimum TTL:
/// the `DomainName` it published.
async fn deployed(
    topology: &Topology,
    case: &str,
    reference: &str,
    origin: &str,
    min_ttl: u64,
) -> String {
    let (id, _arn, domain) = topology
        .created_distribution(case, &config_xml(reference, origin, min_ttl))
        .await;
    topology.awaited_deployment(case, &id).await;
    domain
}

/// Ask for `path` twice back to back and report the two bodies, each held to carrying the status
/// the origin answered.
async fn asked_twice(
    topology: &Topology,
    case: &str,
    domain: &str,
    path: &str,
    status: u16,
) -> (String, String) {
    let (first_status, first) = view(topology, case, domain, path).await;
    let (second_status, second) = view(topology, case, domain, path).await;
    assert_eq!(
        (first_status, second_status),
        (status, status),
        "case {case:?}: expected the origin's own {status} both times, got {first_status} \
         then {second_status} ({first:?}, {second:?})"
    );
    (first, second)
}

/// The whole table over ONE distribution whose minimum TTL is no span at all: an answer the origin
/// asks no cache to store is fetched again, and one it states nothing (or a freshness lifetime)
/// about is held — for the behaviour's own TTL when it is content, and for
/// `ErrorCachingMinTTL` when it is the error the configuration names.
#[tokio::test]
async fn an_answer_the_origin_asks_no_cache_to_store_is_not_held_at_the_edge() {
    let topology = Topology::new();
    let case = "an origin that states what may be stored";
    let origin = topology
        .public_function_url_host_running(case, FUNCTION, HANDLER_SOURCE)
        .await;
    let domain = deployed(&topology, case, "edge-origin-cache-directives", &origin, 0).await;

    let rows: &[Row] = &[
        Row {
            case: "content the origin states nothing about",
            directive: "none",
            status: CONTENT,
            held: true,
        },
        Row {
            case: "content under a freshness lifetime, which forbids no storage",
            directive: "max-age",
            status: CONTENT,
            held: true,
        },
        Row {
            case: "content the origin asks no cache to store",
            directive: "no-store",
            status: CONTENT,
            held: false,
        },
        Row {
            case: "content under a no-cache directive",
            directive: "no-cache",
            status: CONTENT,
            held: false,
        },
        Row {
            case: "content under a private directive",
            directive: "private",
            status: CONTENT,
            held: false,
        },
        Row {
            case: "an error the configuration states a caching duration for",
            directive: "none",
            status: ERROR,
            held: true,
        },
        Row {
            case: "the same error, the origin asking no cache to store it",
            directive: "no-store",
            status: ERROR,
            held: false,
        },
        Row {
            case: "the same error under a no-cache directive",
            directive: "no-cache",
            status: ERROR,
            held: false,
        },
        Row {
            case: "the same error under a private directive",
            directive: "private",
            status: ERROR,
            held: false,
        },
    ];

    for row in rows {
        let path = format!("/{}/{}", row.directive, row.status);
        let (first, second) = asked_twice(&topology, row.case, &domain, &path, row.status).await;
        if row.held {
            assert_eq!(
                first, second,
                "case {:?}: expected the immediate repeat to be served the answer the edge \
                 already holds, got {first:?} then {second:?}",
                row.case
            );
        } else {
            assert_ne!(
                first, second,
                "case {:?}: expected the immediate repeat to reach the origin again, got the \
                 same body twice ({first:?})",
                row.case
            );
        }
    }
}

/// The documented exception, on a distribution whose behaviour states a minimum TTL that IS a
/// span: content the origin asks no cache to store is held for it all the same, while an ERROR it
/// asks no cache to store is still fetched again — the minimum TTL is about content, and an error
/// is held by nothing but what `CustomErrorResponses` states for it.
#[tokio::test]
async fn a_minimum_ttl_still_holds_content_whose_origin_asked_no_cache_to_store_it() {
    let topology = Topology::new();
    let case = "a behaviour whose minimum TTL is a span";
    let origin = topology
        .public_function_url_host_running(case, FUNCTION, HANDLER_SOURCE)
        .await;
    let domain = deployed(
        &topology,
        case,
        "edge-origin-cache-directives-min-ttl",
        &origin,
        HELD_SECONDS,
    )
    .await;

    let rows: &[Row] = &[
        Row {
            case: "content the origin asks no cache to store, under a minimum TTL",
            directive: "no-store",
            status: CONTENT,
            held: true,
        },
        Row {
            case: "an error the origin asks no cache to store, under the same minimum TTL",
            directive: "no-store",
            status: ERROR,
            held: false,
        },
    ];

    for row in rows {
        let path = format!("/{}/{}", row.directive, row.status);
        let (first, second) = asked_twice(&topology, row.case, &domain, &path, row.status).await;
        if row.held {
            assert_eq!(
                first, second,
                "case {:?}: expected the behaviour's minimum TTL to hold it, got {first:?} then \
                 {second:?}",
                row.case
            );
        } else {
            assert_ne!(
                first, second,
                "case {:?}: expected the immediate repeat to reach the origin again, got the \
                 same body twice ({first:?})",
                row.case
            );
        }
    }
}

/// The configuration that drives the rows above is the one the control plane STORED: the
/// behaviour's minimum TTL and the error caching duration are read back off `GetDistributionConfig`
/// rather than assumed from what the create submitted.
#[tokio::test]
async fn the_stored_configuration_carries_the_spans_the_rows_are_read_under() {
    let topology = Topology::new();
    let case = "the stored configuration";
    let origin = topology
        .public_function_url_host_running(case, FUNCTION, HANDLER_SOURCE)
        .await;
    let (id, _arn, _domain) = topology
        .created_distribution(
            case,
            &config_xml("edge-origin-cache-directives-stored", &origin, HELD_SECONDS),
        )
        .await;
    let (status, raw) = topology
        .cloudfront(
            case,
            "GET",
            &format!("/{API_VERSION}/distribution/{id}/config"),
            None,
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetDistributionConfig must answer 200, got {status} with {raw}"
    );
    for (member, expected) in [
        ("MinTTL", HELD_SECONDS.to_string()),
        ("ErrorCachingMinTTL", HELD_SECONDS.to_string()),
        ("ErrorCode", ERROR.to_string()),
    ] {
        let actual = element(&raw, member);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {member} {expected:?} in the stored configuration, got \
             {actual:?}"
        );
    }
}
