// SPDX-License-Identifier: CC0-1.0

//! The address a distribution publishes as its `DomainName`, against the address the create
//! that minted it was REACHED at.
//!
//! RULE: `Distribution$DomainName` is "the domain name that CloudFront assigns when you create
//! a distribution" — the value a viewer composes `https://<DomainName>/<object>` from — so it
//! has to be an address THIS emulator answers at, for the client that was handed it. A create
//! that reached the emulator's own endpoint is published on the HOST of that endpoint, with the
//! distribution named in the reserved data-plane path beneath it, because that host is the one
//! such a client resolves and the whole surface is served there; a domain name states no port,
//! so the port a viewer dials is the one its own client reaches this emulator on. A create
//! there is no endpoint of this emulator to name on is published on an address of its own.
//! Either way the composed viewer URL reaches that distribution's edge, and every read of the
//! distribution reports the same value.
//!
//! The rows span what the rule generalises over — the address the create was reached at: the
//! emulator's own endpoint as a loopback LITERAL and as a loopback NAME, each on a port of its
//! own, and this service's published host in its global and its regional spelling, which is
//! the caller that resolves AWS's own hostnames. Each row holds its published domain to the
//! address the create reached, composes a viewer URL from it and serves an object through it,
//! and reads the same value back through `GetDistribution` and `ListDistributions`.
//!
//! The hosts are the shared table [`crate::announced_location_uri::addressed_hosts`] hands
//! out, which HOLDS the loopback ports its own-endpoint rows address, and the wire harness is
//! [`crate::distribution_lifecycle`]'s.

use smithy_server_runtime::MultiServiceRouter;

use crate::announced_location_uri::addressed_hosts;
use crate::distribution_lifecycle::{
    awaited_deployment, call_from_host, emulator, get_distribution, is_served_edge_address,
    list_distributions, Answer, API_VERSION, HOST,
};

/// The reserved data-plane path a distribution published on the emulator's own endpoint is
/// named under, ahead of its id: `{host}/_aws/cloudfront/{id}`.
///
/// The addressing `crate::service::edge` reads on whatever host a request arrives at, spelled
/// here as a client reading the published name sees it.
const EDGE_PATH_PREFIX: &str = "/_aws/cloudfront";

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

/// The `Host` and the request path a viewer composing `https://<domain>/<object>` sends: the
/// authority of the published name, and whatever path it carries with the object behind it.
///
/// The one composition a client performs, so a published name carrying a path is dialled
/// exactly as one that carries none.
fn viewer_request(domain: &str, object: &str) -> (String, String) {
    match domain.split_once('/') {
        Some((authority, path)) => (authority.to_string(), format!("/{path}{object}")),
        None => (domain.to_string(), object.to_string()),
    }
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

/// Every distribution publishes, as its `DomainName`, an address this emulator answers at for
/// the client the create came from — the endpoint that create reached, with the distribution
/// in the reserved path, or an address of the distribution's own — and the viewer URL composed
/// from that value reaches the distribution's edge.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_distribution_publishes_the_address_the_create_was_reached_at() {
    let router = emulator();
    // Held for the whole test: the ports the own-endpoint rows address stay this process's
    // own, so no listener this engine opens can be given one of them.
    let addressed_hosts = addressed_hosts();

    for (row, addressed) in addressed_hosts.rows.iter().enumerate() {
        let case = addressed.case;
        let (id, domain) = created(
            &router,
            case,
            &addressed.host,
            &format!("ref-published-edge-{row}"),
        )
        .await;

        if addressed.own_endpoint {
            // The HOST of the endpoint the create reached, with this distribution named
            // beneath it: no port, as a domain name states none, and the path names the
            // distribution the viewer's object sits under.
            let host = addressed
                .authority
                .split(':')
                .next()
                .unwrap_or_default()
                .to_string();
            let expected = format!("{host}{EDGE_PATH_PREFIX}/{id}");
            assert_eq!(
                domain, expected,
                "case {case}: expected the distribution published on the host of the endpoint \
                 the create reached, got {domain:?}"
            );
        } else {
            // A caller that resolves AWS's own hostnames reached an emulator with no endpoint
            // of its own to name, so the distribution publishes an address it can be dialled
            // at: a `host:port` carrying no path.
            assert!(
                is_served_edge_address(&domain) && domain.contains(':'),
                "case {case}: expected an address a viewer can dial, got {domain:?}"
            );
        }

        awaited_deployment(&router, case, &id).await;

        // The whole point of the published name: a viewer composes a URL from it and the edge
        // of THAT distribution answers. An answer the edge served states what it did with the
        // request in `X-Cache`; a control-plane answer states nothing there.
        let (viewer_host, viewer_path) = viewer_request(&domain, SERVED_PATH);
        let served = call_from_host(&router, &viewer_host, "GET", &viewer_path, None, &[]).await;
        assert_eq!(
            served.status, 200,
            "case {case}: expected the edge to serve the object through {domain:?}, got {} \
             with {}",
            served.status, served.raw
        );
        assert_eq!(
            served.header("x-cache"),
            "Miss from cloudfront",
            "case {case}: expected the answer through {domain:?} to come from the edge, got \
             X-Cache {:?}",
            served.header("x-cache")
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
