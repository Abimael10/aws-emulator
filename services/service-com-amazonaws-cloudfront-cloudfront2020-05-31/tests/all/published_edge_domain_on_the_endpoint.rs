// SPDX-License-Identifier: CC0-1.0

//! The address a distribution publishes as its `DomainName`, against the address the create
//! that minted it was REACHED at.
//!
//! RULE: `Distribution$DomainName` is "the distribution's CloudFront domain name. For example:
//! `d111111abcdef8.cloudfront.net`" — the value a client DIALS: it composes
//! `https://<DomainName>/<object>` from it, points a DNS record at it, hands it to a TLS client
//! as the name to present. So the published value is a HOST, optionally carrying the port it
//! answers on, and never one with a URI path inside it; and it has to be an address THIS
//! emulator answers on, because a name in a zone this emulator does not serve resolves to
//! nothing on the machine it runs on. The one such value a client here can dial as given,
//! without resolving a name, is the loopback `host:port` of the listener that distribution's
//! edge is served on — so every create publishes that authority, whatever address the create
//! itself was reached at, the edge answers a viewer that dials it, and every read of the
//! distribution reports the same value.
//!
//! The rows span what the rule generalises over — the address the create was reached at: the
//! emulator's own endpoint as a loopback LITERAL and as a loopback NAME, each on a port of its
//! own, and this service's published host in its global and its regional spelling, which is
//! the caller that resolves AWS's own hostnames. Each row holds its published value to that
//! form, DIALS it over a socket of its own and reads an object through it, carries the value as
//! the host of a request placed in process as well, and reads the same value back through
//! `GetDistribution` and `ListDistributions`.
//!
//! The hosts are the shared table [`crate::announced_location_uri::addressed_hosts`] hands
//! out, which HOLDS the loopback ports its own-endpoint rows address, the wire harness is
//! [`crate::distribution_lifecycle`]'s and the viewer dial [`crate::published_edge_address`]'s
//! — each reused rather than copied.

use smithy_server_runtime::MultiServiceRouter;

use crate::announced_location_uri::addressed_hosts;
use crate::distribution_lifecycle::{
    awaited_deployment, call_from_host, emulator, get_distribution, is_published_edge_address,
    list_distributions, Answer, API_VERSION, HOST,
};
use crate::published_edge_address::dial_as;

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

/// Every distribution publishes, as its `DomainName`, an address this emulator answers on — a
/// host carrying the port it answers on, which is the authority of the listener its edge is
/// served on — whatever address the create that minted it was reached at, and a viewer that
/// dials that value is served by that distribution's edge.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_distribution_publishes_a_dialable_address_whatever_the_create_reached() {
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

        // A host carrying the port it answers on and nothing else: no URI path inside the
        // member, and an address a client dials as given. The FORM is the same whichever
        // address the create was reached at, and each distribution publishes one of its own.
        assert!(
            is_published_edge_address(&domain),
            "case {case}: expected an address this emulator answers on (host:port), got \
             {domain:?}"
        );
        assert!(
            !published.contains(&domain),
            "case {case}: expected an address of this distribution's own, got {domain:?} again"
        );
        published.push(domain.clone());

        awaited_deployment(&router, case, &id).await;

        // The whole point of the published value: a viewer DIALS it — over a socket of its
        // own, as a client handed that value does — and the edge of THAT distribution answers.
        // An answer the edge served states what it did with the request in `X-Cache`; a
        // control-plane answer states nothing there.
        let served = dial_as(&domain, "GET", SERVED_PATH, &domain).await;
        let lowercase = served.to_ascii_lowercase();
        assert!(
            lowercase.starts_with("http/1.1 200"),
            "case {case}: expected the edge to serve the object through {domain:?}, got \
             {served:?}"
        );
        assert!(
            lowercase.contains("x-cache: miss from cloudfront"),
            "case {case}: expected the answer through {domain:?} to come from the edge, got \
             {served:?}"
        );

        // And a client that states that value as the host it addressed reaches the same
        // distribution's edge, which is the addressing a request placed in process carries.
        let as_the_host = call_from_host(&router, &domain, "GET", SERVED_PATH, None, &[]).await;
        assert_eq!(
            as_the_host.status, 200,
            "case {case}: expected the edge to serve the object for a viewer addressing \
             {domain:?}, got {} with {}",
            as_the_host.status, as_the_host.raw
        );
        assert!(
            as_the_host.header("x-cache").ends_with("from cloudfront"),
            "case {case}: expected the answer for {domain:?} to come from the edge, got X-Cache \
             {:?}",
            as_the_host.header("x-cache")
        );

        // And every read of the distribution reports that one value, so a client that read it
        // back from any of them dials the same address.
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
