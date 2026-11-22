// SPDX-License-Identifier: CC0-1.0

//! `CustomErrorResponses`' `ErrorCachingMinTTL` against what the ORIGIN said about storing its
//! own error, where the behaviour's spans come from a CACHE POLICY rather than from the legacy
//! per-behaviour members, end to end over HTTP.
//!
//! RULE: `ErrorCachingMinTTL` is "the minimum amount of time, in seconds, that you want CloudFront
//! to cache the HTTP status code specified in `ErrorCode`" — a FLOOR on how long an error the
//! origin left cacheable is held, not permission to store one the origin forbade a shared cache
//! from storing. So an error answer whose own `Cache-Control` carries a directive that forbids
//! storage is not held however long the configuration asks for it, and the next viewer request
//! for the same object reaches the origin again; an error answer that forbids nothing is held for
//! the span the configuration states even under a cache policy that holds nothing of its own.
//!
//! The sibling suite [`crate::edge_origin_cache_directives`] drives the same rule off a behaviour
//! that states the LEGACY `MinTTL`/`DefaultTTL` members. These rows drive it off the other two
//! places a behaviour's spans come from — a cache policy this account created whose `MinTTL` and
//! `DefaultTTL` are no span at all, and one of CloudFront's own published policies that caches
//! nothing (`Managed-CachingDisabled`) — so the rule is pinned wherever the behaviour's spans are
//! read from rather than for one configuration shape.
//!
//! The rows differ along what the rule generalises over: the directive the origin stated (none, a
//! freshness lifetime that forbids no storage, and each of the three that forbid it), the STATUS
//! the answer carried (two error statuses the configuration names an `ErrorCachingMinTTL` for, an
//! error status it names nothing about, and content), and where the behaviour's spans came from.
//! Every row asks for its own path twice back to back: the handler reports the edge's own
//! per-request identifier, so two equal bodies are one answer served twice and two different
//! bodies are two origin invocations.

use crate::distribution_lifecycle::API_VERSION;
use crate::edge_lambda_url_origin::{element, Topology};
use crate::edge_origin_cache_directives::{view, HANDLER_SOURCE};
use crate::edge_signed_access::CACHING_DISABLED;

/// The function behind the origin of every distribution below.
const FUNCTION: &str = "cf-error-caching-cache-policy";

/// How long the configuration asks for an error to be held: a span no row can outlive, so a row
/// that reads two equal bodies read a held answer rather than an expired one.
const HELD_SECONDS: u64 = 3_600;

/// The longest the cache policy under test admits an object to the cache for. It states a span of
/// its own while `MinTTL` and `DefaultTTL` state none, so nothing but `ErrorCachingMinTTL` can
/// hold an answer of this distribution's.
const MAX_TTL_SECONDS: u64 = 30;

/// The statuses the origin answers under: the two errors the configuration names a caching
/// duration for, an error it names nothing about, and content.
const ERROR: u16 = 502;
const OTHER_ERROR: u16 = 503;
const UNNAMED_ERROR: u16 = 500;
const CONTENT: u16 = 200;

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

/// The `CachePolicyConfig` of a policy whose `MinTTL` and `DefaultTTL` are no span at all: a
/// behaviour under it holds no object of its own, so what holds an error is the distribution's
/// `CustomErrorResponses` and nothing else.
fn cache_policy_xml(name: &str) -> String {
    format!(
        "<CachePolicyConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <Name>{name}</Name>\
         <MinTTL>0</MinTTL><DefaultTTL>0</DefaultTTL><MaxTTL>{MAX_TTL_SECONDS}</MaxTTL>\
         <ParametersInCacheKeyAndForwardedToOrigin>\
         <EnableAcceptEncodingGzip>false</EnableAcceptEncodingGzip>\
         <HeadersConfig><HeaderBehavior>none</HeaderBehavior></HeadersConfig>\
         <CookiesConfig><CookieBehavior>none</CookieBehavior></CookiesConfig>\
         <QueryStringsConfig><QueryStringBehavior>none</QueryStringBehavior>\
         </QueryStringsConfig>\
         </ParametersInCacheKeyAndForwardedToOrigin>\
         </CachePolicyConfig>"
    )
}

/// Create that cache policy and answer the id it minted.
async fn cache_policy(topology: &Topology, case: &str, name: &str) -> String {
    let (status, raw) = topology
        .cloudfront(
            case,
            "POST",
            &format!("/{API_VERSION}/cache-policy"),
            Some(&cache_policy_xml(name)),
        )
        .await;
    assert_eq!(
        status, 201,
        "setup {case}: creating the cache policy {name:?} answered {status} with {raw}"
    );
    let id = element(&raw, "Id");
    assert!(
        !id.is_empty(),
        "setup {case}: the created cache policy reports no id: {raw}"
    );
    id
}

/// The `DistributionConfig` of a distribution over the function URL `origin` whose default
/// behaviour names `cache_policy_id` — so every span under test is the POLICY's — and whose
/// `CustomErrorResponses` name [`ERROR`] and [`OTHER_ERROR`] with an `ErrorCachingMinTTL` of
/// [`HELD_SECONDS`] and no page, which states the caching duration alone.
fn config_xml(caller_reference: &str, origin: &str, cache_policy_id: &str) -> String {
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
         <CachePolicyId>{cache_policy_id}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <CacheBehaviors><Quantity>0</Quantity></CacheBehaviors>\
         <CustomErrorResponses><Quantity>2</Quantity><Items>\
         <CustomErrorResponse><ErrorCode>{ERROR}</ErrorCode>\
         <ErrorCachingMinTTL>{HELD_SECONDS}</ErrorCachingMinTTL></CustomErrorResponse>\
         <CustomErrorResponse><ErrorCode>{OTHER_ERROR}</ErrorCode>\
         <ErrorCachingMinTTL>{HELD_SECONDS}</ErrorCachingMinTTL></CustomErrorResponse>\
         </Items></CustomErrorResponses>\
         <Comment>error caching under a cache policy</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

/// One deployed distribution over `origin` under `cache_policy_id`: the `DomainName` it published.
async fn deployed(
    topology: &Topology,
    case: &str,
    reference: &str,
    origin: &str,
    cache_policy_id: &str,
) -> String {
    let (id, _arn, domain) = topology
        .created_distribution(case, &config_xml(reference, origin, cache_policy_id))
        .await;
    topology.awaited_deployment(case, &id).await;
    domain
}

/// Ask for each row's own path twice back to back and hold the two answers to carrying the status
/// the origin answered and to being one held answer or two origin invocations, as the row states.
async fn asserted(topology: &Topology, domain: &str, rows: &[Row]) {
    for row in rows {
        let path = format!("/{}/{}", row.directive, row.status);
        let (first_status, first) = view(topology, row.case, domain, &path).await;
        let (second_status, second) = view(topology, row.case, domain, &path).await;
        assert_eq!(
            (first_status, second_status),
            (row.status, row.status),
            "case {:?}: expected the origin's own {} both times, got {first_status} then \
             {second_status} ({first:?}, {second:?})",
            row.case,
            row.status
        );
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

/// The whole table over a distribution whose behaviour names a cache policy this account created
/// whose `MinTTL` and `DefaultTTL` are no span at all: the only thing that holds anything is the
/// `ErrorCachingMinTTL` the configuration states, and it holds an error the origin left cacheable
/// while an error the origin forbade a shared cache from storing is fetched again.
#[tokio::test]
async fn error_caching_min_ttl_does_not_hold_an_error_whose_origin_forbids_storing_it() {
    let topology = Topology::new();
    let case = "an error caching duration over a cache policy that holds nothing";
    let origin = topology
        .public_function_url_host_running(case, FUNCTION, HANDLER_SOURCE)
        .await;
    let policy = cache_policy(&topology, case, "error-caching-no-span").await;
    let domain = deployed(
        &topology,
        case,
        "edge-error-caching-cache-policy",
        &origin,
        &policy,
    )
    .await;

    let rows: &[Row] = &[
        Row {
            case: "an error the configuration states a caching duration for",
            directive: "none",
            status: ERROR,
            held: true,
        },
        Row {
            case: "that error under a freshness lifetime, which forbids no storage",
            directive: "max-age",
            status: ERROR,
            held: true,
        },
        Row {
            case: "that error, the origin asking no cache to store it",
            directive: "no-store",
            status: ERROR,
            held: false,
        },
        Row {
            case: "that error under a no-cache directive",
            directive: "no-cache",
            status: ERROR,
            held: false,
        },
        Row {
            case: "that error under a private directive",
            directive: "private",
            status: ERROR,
            held: false,
        },
        Row {
            case: "another error status the configuration states a caching duration for",
            directive: "none",
            status: OTHER_ERROR,
            held: true,
        },
        Row {
            case: "that other error, the origin asking no cache to store it",
            directive: "no-store",
            status: OTHER_ERROR,
            held: false,
        },
        Row {
            case: "an error status the configuration states nothing about",
            directive: "none",
            status: UNNAMED_ERROR,
            held: false,
        },
        Row {
            case: "content under a policy whose spans are no time at all",
            directive: "none",
            status: CONTENT,
            held: false,
        },
    ];

    asserted(&topology, &domain, rows).await;
}

/// The same rule where the behaviour's spans are one of CLOUDFRONT'S OWN published policies that
/// caches nothing: the configuration's `ErrorCachingMinTTL` still holds an error the origin left
/// cacheable, and still holds nothing the origin asked no cache to store.
#[tokio::test]
async fn the_same_holds_where_the_spans_come_from_a_managed_cache_policy() {
    let topology = Topology::new();
    let case = "an error caching duration over a managed policy that caches nothing";
    let origin = topology
        .public_function_url_host_running(case, FUNCTION, HANDLER_SOURCE)
        .await;
    let domain = deployed(
        &topology,
        case,
        "edge-error-caching-managed-policy",
        &origin,
        CACHING_DISABLED,
    )
    .await;

    let rows: &[Row] = &[
        Row {
            case: "an error the configuration states a caching duration for",
            directive: "none",
            status: ERROR,
            held: true,
        },
        Row {
            case: "that error, the origin asking no cache to store it",
            directive: "no-store",
            status: ERROR,
            held: false,
        },
        Row {
            case: "content under a managed policy that caches nothing",
            directive: "none",
            status: CONTENT,
            held: false,
        },
    ];

    asserted(&topology, &domain, rows).await;
}

/// The spans the rows above run under are the ones the control plane STORED: the policy's own
/// `MinTTL`, `DefaultTTL` and `MaxTTL` are read back off `GetCachePolicyConfig`, and the error
/// caching duration off `GetDistributionConfig`, rather than assumed from what the creates
/// submitted.
#[tokio::test]
async fn the_stored_policy_and_configuration_carry_the_spans_the_rows_run_under() {
    let topology = Topology::new();
    let case = "the stored policy and configuration";
    let policy = cache_policy(&topology, case, "error-caching-no-span-stored").await;
    let (status, raw) = topology
        .cloudfront(
            case,
            "GET",
            &format!("/{API_VERSION}/cache-policy/{policy}/config"),
            None,
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetCachePolicyConfig must answer 200, got {status} with {raw}"
    );
    for (member, expected) in [
        ("MinTTL", "0".to_string()),
        ("DefaultTTL", "0".to_string()),
        ("MaxTTL", MAX_TTL_SECONDS.to_string()),
    ] {
        let actual = element(&raw, member);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {member} {expected:?} in the stored cache policy, got \
             {actual:?}"
        );
    }

    let (id, _arn, _domain) = topology
        .created_distribution(
            case,
            &config_xml(
                "edge-error-caching-cache-policy-stored",
                "origin.example.com",
                &policy,
            ),
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
        ("CachePolicyId", policy.clone()),
        ("ErrorCode", ERROR.to_string()),
        ("ErrorCachingMinTTL", HELD_SECONDS.to_string()),
    ] {
        let actual = element(&raw, member);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {member} {expected:?} in the stored configuration, got \
             {actual:?}"
        );
    }
}
