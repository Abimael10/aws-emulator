// SPDX-License-Identifier: CC0-1.0

//! The NAMES of the response headers an answer the edge serves carries on the wire.
//!
//! The rule: a header name is case-insensitive (RFC 9110), but what a client reads off the
//! connection is the bytes the server wrote — and the answers AWS's own front doors write
//! name their headers in the canonical HTTP form (`Location`, `Content-Type`, `X-Cache`,
//! `X-Amz-Cf-Id`), which is what a client that reads the names AS SENT looks for. So the
//! address a distribution publishes writes them that way too: an answer whose redirect target
//! arrived spelled `location` is one such a client finds no redirect target in, and an answer
//! whose cache outcome arrived spelled `x-cache` is one it cannot tell a hit from a miss by.
//!
//! The rows differ along the KIND of answer, because the headers come from different places
//! in each: one the origin produced (the edge restates the origin's own), one the edge cache
//! held, one CloudFront generated itself for a redirect, and one CloudFront generated itself
//! for a refusal. Each row names the headers that answer carries and holds every one of them
//! to the canonical spelling.
//!
//! The harness is the lifecycle suites': the wire client is
//! [`crate::distribution_lifecycle`]'s, the viewer dial [`crate::published_edge_address`]'s.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    awaited_deployment, create_distribution, emulator, is_reachable_authority, API_VERSION, HOST,
};
use crate::published_edge_address::dial;

/// A viewer path the origin these rows point at — this service's own control-plane endpoint —
/// answers with a `DistributionList`.
const SERVED_PATH: &str = "/2020-05-31/distribution";

/// The AWS-managed cache policy `Managed-CachingOptimized`, whose `DefaultTTL` is a day: a
/// behaviour under it caches what it may, so a repeat of the same request is a cache hit.
const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// One `DistributionConfig`: one origin and one default cache behaviour under
/// `viewer_protocol_policy`, caching through the managed policy above.
fn config_xml(reference: &str, viewer_protocol_policy: &str) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items>\
         <Origin><Id>primary</Id><DomainName>{HOST}</DomainName></Origin>\
         </Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>{viewer_protocol_policy}</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_OPTIMIZED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <Comment>{reference}</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

/// Create one deployed distribution and answer the address it published.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    reference: &str,
    viewer_protocol_policy: &str,
) -> String {
    let created = create_distribution(router, &config_xml(reference, viewer_protocol_policy)).await;
    assert_eq!(
        created.status, 201,
        "case {case}: expected the create to be accepted, got {} with {}",
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
        "case {case}: expected an address a viewer can dial, got {domain:?}"
    );
    awaited_deployment(router, case, &id).await;
    domain
}

/// The head of one raw answer: the status line and the header lines, as they came off the
/// connection.
fn head(case: &str, raw: &str) -> String {
    assert!(
        raw.starts_with("HTTP/1.1 "),
        "case {case}: expected an HTTP/1.1 answer, got {raw:?}"
    );
    raw.split_once("\r\n\r\n")
        .map(|(head, _body)| head.to_string())
        .unwrap_or_else(|| raw.to_string())
}

/// Hold every name in `names` to its canonical spelling in `head`: present as written, and
/// not present in the lower-case spelling a client reading the names as sent would miss.
fn carries_canonical_names(case: &str, head: &str, names: &[&str]) {
    for name in names {
        assert!(
            head.contains(&format!("\r\n{name}:")),
            "case {case}: expected the header {name:?} named in the canonical form, got {head:?}"
        );
        let lowercase = name.to_ascii_lowercase();
        if lowercase != *name {
            assert!(
                !head.contains(&format!("\r\n{lowercase}:")),
                "case {case}: expected no second, lower-case spelling of {name:?}, got {head:?}"
            );
        }
    }
}

/// Every header name the four kinds of answer below carry, canonical as AWS writes them.
/// `Content-Length`, `Date` and `Connection` are the framing the connection itself states;
/// `Content-Type` describes the bytes; `X-Cache` and `X-Amz-Cf-Id` are what the EDGE says
/// about the request it just served; `Server` and `Location` are CloudFront's own on an answer
/// it generated.
const ORIGIN_ANSWER_HEADERS: &[&str] = &[
    "Content-Type",
    "Content-Length",
    "Date",
    "X-Cache",
    "X-Amz-Cf-Id",
];

/// See [`ORIGIN_ANSWER_HEADERS`]: an answer CloudFront generated itself names itself in
/// `Server` as well.
const GENERATED_ANSWER_HEADERS: &[&str] = &[
    "Content-Type",
    "Content-Length",
    "Date",
    "Server",
    "X-Cache",
    "X-Amz-Cf-Id",
];

/// The answer an ORIGIN produced, the answer the edge CACHE held, and the two answers
/// CloudFront generated itself all name their headers canonically.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_answer_the_published_address_serves_names_its_headers_canonically() {
    let router = emulator();

    // One: the answer the ORIGIN produced for this request, and two: the same request again,
    // which the edge cache holds the answer to.
    let case = "an answer the origin produced";
    let domain = deployed(&router, case, "header-case-served", "allow-all").await;
    let fetched = dial(&domain, "GET", SERVED_PATH).await;
    let fetched = head(case, &fetched);
    carries_canonical_names(case, &fetched, ORIGIN_ANSWER_HEADERS);
    assert!(
        fetched.contains("\r\nX-Cache: Miss from cloudfront"),
        "case {case}: expected the edge to report a cache miss, got {fetched:?}"
    );

    let case = "an answer the edge cache held";
    let held = dial(&domain, "GET", SERVED_PATH).await;
    let held = head(case, &held);
    carries_canonical_names(case, &held, ORIGIN_ANSWER_HEADERS);
    assert!(
        held.contains("\r\nX-Cache: Hit from cloudfront"),
        "case {case}: expected the edge to report a cache hit, got {held:?}"
    );

    // Three: the REDIRECT CloudFront generates for a plaintext viewer of a behaviour whose
    // `ViewerProtocolPolicy` is `redirect-to-https`, whose target is the one header this whole
    // rule is about.
    let case = "the redirect CloudFront generated";
    let domain = deployed(&router, case, "header-case-redirect", "redirect-to-https").await;
    let redirected = dial(&domain, "GET", "/index.html").await;
    let redirected = head(case, &redirected);
    carries_canonical_names(case, &redirected, GENERATED_ANSWER_HEADERS);
    carries_canonical_names(case, &redirected, &["Location"]);
    assert!(
        redirected.contains(&format!("\r\nLocation: https://{domain}/index.html")),
        "case {case}: expected the HTTPS URL of the same request in Location, got {redirected:?}"
    );

    // Four: the refusal CloudFront generates itself for a method the matched behaviour does
    // not allow, which carries no origin bytes at all.
    let case = "the refusal CloudFront generated";
    let domain = deployed(&router, case, "header-case-refused", "allow-all").await;
    let refused = dial(&domain, "DELETE", SERVED_PATH).await;
    let refused = head(case, &refused);
    assert!(
        refused.starts_with("HTTP/1.1 403"),
        "case {case}: expected CloudFront's own refusal, got {refused:?}"
    );
    carries_canonical_names(case, &refused, GENERATED_ANSWER_HEADERS);
}
