// SPDX-License-Identifier: CC0-1.0

//! The name a distribution publishes as its `DomainName`, against the address the create that
//! minted it was REACHED at.
//!
//! RULE: `Distribution$DomainName` is "the distribution's CloudFront domain name. For example:
//! `d111111abcdef8.cloudfront.net`" — the DNS NAME CloudFront assigns the distribution. So the
//! value is one label under CloudFront's own zone whatever address the create was reached at,
//! and it carries nothing else: no address of this emulator, no port, no URL path. What makes
//! that name reach the distribution is that the emulator ANSWERS on it — a viewer request is
//! resolved by the name the distribution published, stated as the `Host` it addressed or
//! carried in the first path segment where a client cannot carry it in the host — and every
//! read of the distribution reports that one value.
//!
//! The rows span what the rule generalises over — the address the create was reached at: the
//! emulator's own endpoint as a loopback LITERAL and as a loopback NAME, each on a port of its
//! own, and this service's published host in its global and its regional spelling, which is
//! the caller that resolves AWS's own hostnames. Each row holds the published name to the form
//! CloudFront assigns, serves an object through it both ways a client can address it, and reads
//! the same value back through `GetDistribution` and `ListDistributions`.
//!
//! The hosts are the shared table [`crate::announced_location_uri::addressed_hosts`] hands
//! out, which HOLDS the loopback ports its own-endpoint rows address, and the wire harness is
//! [`crate::distribution_lifecycle`]'s.

use smithy_server_runtime::MultiServiceRouter;

use crate::announced_location_uri::addressed_hosts;
use crate::distribution_lifecycle::{
    awaited_deployment, call_from_host, emulator, get_distribution, is_assigned_edge_domain,
    list_distributions, Answer, API_VERSION, HOST,
};

/// A viewer path the origin these distributions point at — this service's own control-plane
/// endpoint, reached over the in-process transport — answers with a `DistributionList`.
const SERVED_PATH: &str = "/2020-05-31/distribution";

/// The AWS-managed cache policy `Managed-CachingOptimized`, so the behaviour caches what it
/// may and the first viewer request is a miss the ORIGIN answered.
const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// One `DistributionConfig`: one origin — this service's own control-plane host, which the
/// edge can fetch in process — and one default cache behaviour serving viewers either scheme.
fn config_xml(reference: &str) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items>\
         <Origin><Id>primary</Id><DomainName>{HOST}</DomainName></Origin>\
         </Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_OPTIMIZED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <Comment>{reference}</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

/// Create one distribution from `host` and answer its `(id, DomainName)`.
async fn created(
    router: &MultiServiceRouter,
    case: &str,
    host: &str,
    reference: &str,
) -> (String, String) {
    let answer: Answer = call_from_host(
        router,
        host,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(&config_xml(reference)),
        &[],
    )
    .await;
    assert_eq!(
        answer.status, 201,
        "case {case}: expected the create to be accepted, got {} with {}",
        answer.status, answer.raw
    );
    let id = answer
        .root()
        .find_text("Id")
        .unwrap_or_default()
        .to_string();
    let domain = answer
        .root()
        .find_text("DomainName")
        .unwrap_or_default()
        .to_string();
    assert!(
        !id.is_empty() && !domain.is_empty(),
        "case {case}: expected an Id and a DomainName, got {}",
        answer.raw
    );
    (id, domain)
}

/// Every distribution publishes, as its `DomainName`, the DNS name CloudFront assigned it —
/// whatever address the create came from — and the emulator answers on that name.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_distribution_publishes_the_name_cloudfront_assigned_it() {
    let router = emulator();
    // Held for the whole test: the ports the own-endpoint rows address stay this process's
    // own, so no listener this engine opens can be given one of them.
    let addressed_hosts = addressed_hosts();

    let mut published: Vec<String> = Vec::new();
    for (row, addressed) in addressed_hosts.rows.iter().enumerate() {
        let case = addressed.case;
        let (id, domain) = created(
            &router,
            case,
            &addressed.host,
            &format!("ref-published-edge-{row}"),
        )
        .await;

        // The name CloudFront assigns, and nothing of the address the create was reached at:
        // the value is the same FORM whichever address that was.
        assert!(
            is_assigned_edge_domain(&domain),
            "case {case}: expected the DNS name CloudFront assigns a distribution \
             (d<label>.cloudfront.net), got {domain:?}"
        );
        assert!(
            !published.contains(&domain),
            "case {case}: expected a name of this distribution's own, got {domain:?} again"
        );
        published.push(domain.clone());

        awaited_deployment(&router, case, &id).await;

        // The whole point of the published name: the emulator ANSWERS on it. A viewer that
        // states it as the host it addressed is served by THAT distribution's edge, and an
        // answer the edge served states what it did with the request in `X-Cache`; a
        // control-plane answer states nothing there.
        let served = call_from_host(&router, &domain, "GET", SERVED_PATH, None, &[]).await;
        assert_eq!(
            served.status, 200,
            "case {case}: expected the edge to serve the object for a viewer addressing \
             {domain:?}, got {} with {}",
            served.status, served.raw
        );
        assert_eq!(
            served.header("x-cache"),
            "Miss from cloudfront",
            "case {case}: expected the answer for {domain:?} to come from the edge, got \
             X-Cache {:?}",
            served.header("x-cache")
        );

        // And a client that cannot carry the name in the host carries it in the first path
        // segment instead, reaching the same distribution's edge.
        let in_the_path = call_from_host(
            &router,
            &addressed.host,
            "GET",
            &format!("/{domain}{SERVED_PATH}"),
            None,
            &[],
        )
        .await;
        assert_eq!(
            in_the_path.status, 200,
            "case {case}: expected the edge to serve the object for {domain:?} carried in the \
             path, got {} with {}",
            in_the_path.status, in_the_path.raw
        );
        assert!(
            in_the_path.header("x-cache").ends_with("from cloudfront"),
            "case {case}: expected the answer for {domain:?} in the path to come from the \
             edge, got X-Cache {:?}",
            in_the_path.header("x-cache")
        );

        // And every read of the distribution reports that one value, so a client that read it
        // back from any of them holds the same name.
        let read = get_distribution(&router, &id).await;
        let read_domain = read
            .root()
            .find_text("DomainName")
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            read_domain, domain,
            "case {case}: expected GetDistribution to report {domain:?}, got {read_domain:?}"
        );
        let listed = list_distributions(&router, "").await;
        let listed_domain = listed
            .root()
            .child("Items")
            .map(|items| items.children_named("DistributionSummary"))
            .unwrap_or_default()
            .into_iter()
            .find(|summary| summary.text_at("Id").unwrap_or_default() == id)
            .and_then(|summary| summary.text_at("DomainName"))
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            listed_domain, domain,
            "case {case}: expected the ListDistributions summary to report {domain:?}, got \
             {listed_domain:?}"
        );
    }
}
