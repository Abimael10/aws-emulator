// SPDX-License-Identifier: CC0-1.0

//! Every address this service PUBLISHES for a client to DIAL leads back to this emulator —
//! the regression guard for the one rule those members have oscillated on.
//!
//! RULE: a member that publishes an address a client then dials — `Distribution$DomainName`
//! ("the distribution's CloudFront domain name") and `ConnectionGroup$RoutingEndpoint` ("the
//! routing endpoint, also known as the DNS name, that is assigned to the connection group") —
//! carries an address THIS emulator answers on: the authority of the listener this service
//! opened for that resource ([`crate::distribution_lifecycle::is_reachable_authority`]), and
//! never a name in a zone Amazon Web Services publishes endpoints in — its own service zones,
//! or the `cloudfront.net` zone CloudFront assigns its resources names in — which resolves
//! nowhere near the machine this process runs on, so a client handed one reaches no resource
//! and may leave the emulated environment carrying the credentials it holds.
//!
//! The rows span what the rule generalises over: WHICH member publishes the address (two
//! resources, each given a listener of its own by a different create) and the address the
//! create was REACHED at (the emulator's own endpoint as a loopback LITERAL and as a loopback
//! NAME, each on a port of its own, and this service's published host in its global and its
//! regional spelling, which is the caller that resolves AWS's own hostnames) — because the
//! published value is a property of the RESOURCE and not of the request that created it.
//!
//! Each published value is read back through a GET of the resource as well, so the rule holds
//! for the record and not only for the create's own answer.
//!
//! What a viewer that DIALS a distribution's published value is served is
//! [`crate::published_edge_domain_on_the_endpoint`]'s and
//! [`crate::published_edge_address`]'s; this suite pins the FORM every one of those members
//! takes, which is what a client reads out of the control plane.
//!
//! The hosts are the shared table [`crate::announced_location_uri::addressed_hosts`] hands
//! out, which HOLDS the loopback ports its own-endpoint rows address, and the wire harness is
//! [`crate::distribution_lifecycle`]'s — each reused rather than copied.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::CLOUDFRONT_DOMAIN_SUFFIX;
use smithy_server_runtime::MultiServiceRouter;

use crate::announced_location_uri::addressed_hosts;
use crate::distribution_lifecycle::{
    call_from_host, config, emulator, is_reachable_authority, Answer, API_VERSION,
};

/// Whether `host` names a zone no client of this emulator resolves to this process: an
/// endpoint Amazon Web Services publishes its own services at
/// (`service_common::data_plane_addressing::addresses_an_aws_endpoint`, the tree's one
/// reading of that question), or the zone CloudFront assigns its own resources names in
/// (`state::CLOUDFRONT_DOMAIN_SUFFIX`), which that reading deliberately leaves out because it
/// lists endpoint zones alone.
fn names_a_zone_aws_serves(host: &str) -> bool {
    ::service_common::data_plane_addressing::addresses_an_aws_endpoint(host)
        || host
            .to_ascii_lowercase()
            .ends_with(CLOUDFRONT_DOMAIN_SUFFIX)
}

/// The text of the first `name` element of an answer, as the suites read one.
fn text(answer: &Answer, name: &str) -> String {
    answer
        .root()
        .find_text(name)
        .unwrap_or_default()
        .to_string()
}

/// A `CreateConnectionGroupRequest` naming `name` and nothing optional.
fn connection_group_xml(name: &str) -> String {
    format!(
        "<CreateConnectionGroupRequest \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <Name>{name}</Name></CreateConnectionGroupRequest>"
    )
}

/// One create, addressed to `host`, answered whole so a case reads both the published member
/// and the id the resource is read back by.
async fn created(
    router: &MultiServiceRouter,
    case: &str,
    host: &str,
    collection: &str,
    body: &str,
) -> Answer {
    let answer = call_from_host(
        router,
        host,
        "POST",
        &format!("/{API_VERSION}/{collection}"),
        Some(body),
        &[],
    )
    .await;
    assert_eq!(
        answer.status, 201,
        "case {case}: expected the create of {collection} to be accepted with 201, got {} \
         with {}",
        answer.status, answer.raw
    );
    answer
}

/// Assert `published` is an address this emulator answers on and names no zone AWS serves.
fn assert_leads_back(case: &str, member: &str, published: &str) {
    assert!(
        is_reachable_authority(published),
        "case {case}: expected {member} to publish an address this emulator answers on, got \
         {published:?}"
    );
    assert!(
        !names_a_zone_aws_serves(published),
        "case {case}: expected {member} to publish no name in a zone AWS serves, got \
         {published:?}"
    );
    // A published address is a HOST, never a URI: no path inside the member.
    assert!(
        !published.contains('/') && !published.is_empty(),
        "case {case}: expected {member} to be a host and nothing else, got {published:?}"
    );
}

/// Every create publishes an address this emulator answers on, whatever address the create
/// itself was reached at, and every later read of the resource reports that same value.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_published_address_leads_back_to_this_emulator() {
    let router = emulator();
    // Held for the whole test: the ports the own-endpoint rows address stay this process's
    // own, so no listener this engine opens can be given one of them.
    let addressed_hosts = addressed_hosts();

    for (row, addressed) in addressed_hosts.rows.iter().enumerate() {
        let case = addressed.case;

        // A DISTRIBUTION publishes the authority of the listener its edge is served on.
        let distribution = created(
            &router,
            case,
            &addressed.host,
            "distribution",
            &config("published address", &format!("ref-published-address-{row}")).xml(),
        )
        .await;
        let domain = text(&distribution, "DomainName");
        assert_leads_back(case, "Distribution$DomainName", &domain);

        // And every later read of the record reports the value the create published, so a
        // client that read it back dials the same address.
        let id = text(&distribution, "Id");
        let read = call_from_host(
            &router,
            &addressed.host,
            "GET",
            &format!("/{API_VERSION}/distribution/{id}"),
            None,
            &[],
        )
        .await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected the distribution to read back, got {} with {}",
            read.status, read.raw
        );
        assert_eq!(
            text(&read, "DomainName"),
            domain,
            "case {case}: expected the read to report {domain:?}, got {:?}",
            text(&read, "DomainName")
        );

        // A CONNECTION GROUP publishes the address a tenant's own domain is pointed at, which
        // is the same kind of value and so follows the same rule.
        let group = created(
            &router,
            case,
            &addressed.host,
            "connection-group",
            &connection_group_xml(&format!("published-address-group-{row}")),
        )
        .await;
        let routing_endpoint = text(&group, "RoutingEndpoint");
        assert_leads_back(case, "ConnectionGroup$RoutingEndpoint", &routing_endpoint);
        let group_id = text(&group, "Id");
        let read_group = call_from_host(
            &router,
            &addressed.host,
            "GET",
            &format!("/{API_VERSION}/connection-group/{group_id}"),
            None,
            &[],
        )
        .await;
        assert_eq!(
            read_group.status, 200,
            "case {case}: expected the connection group to read back, got {} with {}",
            read_group.status, read_group.raw
        );
        assert_eq!(
            text(&read_group, "RoutingEndpoint"),
            routing_endpoint,
            "case {case}: expected the read to report {routing_endpoint:?}, got {:?}",
            text(&read_group, "RoutingEndpoint")
        );

        // Two resources of this service never publish one another's address: each is served
        // on a listener of its own.
        assert_ne!(
            domain, routing_endpoint,
            "case {case}: expected each resource to publish an address of its own, got \
             {domain:?} twice"
        );
    }
}
