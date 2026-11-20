// SPDX-License-Identifier: CC0-1.0

//! The address a suite addresses as the emulator's OWN endpoint is a port this process HOLDS.
//!
//! RULE: which plane a request belongs to is decided by its HOST, and a distribution's edge is
//! served on a listener of its own whose address — a loopback `host:port` the operating system
//! chose — is the `DomainName` that distribution publishes. So a request addressed to that
//! authority is viewer traffic to that distribution whatever its path spells, and a loopback
//! authority a suite addresses as the emulator's own endpoint has to be one no listener of that
//! engine can be given. A port this process has BOUND is such an authority: the operating
//! system never hands a bound port to a second listener, so which address the suite addresses
//! is decided by the reservation rather than by which ports the host happened to have free.
//!
//! That is what the shared table [`crate::announced_location_uri::addressed_hosts`] hands out,
//! and this suite pins it: every own-endpoint row addresses a port that cannot be bound again,
//! no distribution this engine serves is reported at one of those authorities, and the control
//! plane keeps answering creates there — rooting each announced `Location` at the address the
//! request reached — however many distributions, each with an edge listener of its own, the
//! engine already serves. The complement states why that matters: a create addressed to the
//! authority a distribution's edge really holds is not answered by the control plane at all.
//!
//! The wire harness is [`crate::distribution_lifecycle`]'s and the hosts and documents are the
//! announced-location suites' own, so nothing here is a second spelling of them.

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::announced_location_uri::{addressed_hosts, created_id, origin_access_identity_xml};
use crate::distribution_lifecycle::{
    call_from_host, config, create_distribution, emulator, is_reachable_authority, API_VERSION,
    AUTHORIZATION,
};

/// How many distributions the engine ends up serving: each create opens an edge listener of its
/// own on a port the operating system chooses, which is the only thing that could land on an
/// address a row below addresses.
const ROUNDS: usize = 4;

/// The collection the creates below are issued under: an origin access identity is minted from
/// the request alone and opens no listener, so it is the cheapest create that announces a
/// `Location`.
const COLLECTION: &str = "origin-access-identity/cloudfront";

/// The status one request is answered with, read WITHOUT parsing the body.
///
/// The shared harness reads every answer through its element scanner, which is written for the
/// documents this service serialises; the edge answers viewer traffic it refuses with an HTML
/// page, so the one request below that the edge claims is read as its status alone.
async fn answered_status(router: &MultiServiceRouter, host: &str, uri: &str, body: &str) -> u16 {
    let request = ::http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", host)
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/xml")
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    router.dispatch(request).await.status().as_u16()
}

/// The port a `host:port` names.
fn addressed_port(host: &str, case: &str) -> u16 {
    host.rsplit(':')
        .next()
        .and_then(|port| port.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("case {case}: the own endpoint {host:?} names no port"))
}

/// Every own-endpoint row addresses a loopback port this process holds, no distribution's edge
/// is served at one of those authorities, and the control plane answers creates there whatever
/// the engine already serves — while the authority an edge listener really holds is claimed by
/// the edge instead.
#[tokio::test]
async fn an_addressed_own_endpoint_is_a_port_no_edge_listener_can_take() {
    let hosts = addressed_hosts();
    let router = emulator();

    // The reservation itself: binding the port again — on the loopback interface, which is the
    // interface every listener this emulator opens binds — is refused.
    for row in hosts.rows.iter().filter(|row| row.own_endpoint) {
        let port = addressed_port(&row.host, row.case);
        let rebound = ::std::net::TcpListener::bind((::std::net::Ipv4Addr::LOCALHOST, port));
        assert!(
            rebound.is_err(),
            "case {}: expected {:?} to be a port this process holds, but binding it again \
             succeeded",
            row.case,
            row.host
        );
    }

    let mut served: Vec<String> = Vec::new();
    for round in 0..ROUNDS {
        // One more distribution, so one more edge listener on a port of the operating system's
        // choosing.
        let created = create_distribution(
            &router,
            &config(
                "the distribution whose edge takes a port",
                &format!("ref-held-{round}"),
            )
            .xml(),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "round {round}: expected the distribution to be created, got {} with {}",
            created.status, created.raw
        );
        let domain = created
            .root()
            .find_text("DomainName")
            .unwrap_or_else(|| panic!("round {round}: the create carries no DomainName"))
            .to_string();
        assert!(
            is_reachable_authority(&domain),
            "round {round}: expected the distribution to publish the address of its own \
             listener, got {domain:?}"
        );
        for row in hosts.rows.iter() {
            assert!(
                !domain.eq_ignore_ascii_case(&row.authority),
                "round {round}, case {}: the edge took the addressed authority {:?}, which a \
                 held port cannot be",
                row.case,
                row.authority
            );
        }
        served.push(domain);

        // The control plane still answers on every addressed host, and announces the new
        // resource at the address the request reached.
        for (index, row) in hosts.rows.iter().enumerate() {
            let case = format!("{} (round {round})", row.case);
            let answer = call_from_host(
                &router,
                &row.host,
                "POST",
                &format!("/{API_VERSION}/{COLLECTION}"),
                Some(&origin_access_identity_xml(&format!(
                    "oai-held-{round}-{index}"
                ))),
                &[],
            )
            .await;
            assert_eq!(
                answer.status, 201,
                "case {case}: expected the control plane to answer the create, got {} with {}",
                answer.status, answer.raw
            );
            let id = created_id(&answer, &case);
            let location = answer.header("location").to_string();
            assert_eq!(
                location,
                format!("https://{}/{API_VERSION}/{COLLECTION}/{id}", row.authority),
                "case {case}: expected the new resource's path rooted at the endpoint the \
                 request reached ({}), got {location:?}",
                row.authority
            );
        }
    }

    // And the authority an edge listener really holds is the one case the edge claims: a create
    // addressed there is viewer traffic to that distribution, so nothing is created.
    for (round, domain) in served.iter().enumerate() {
        let status = answered_status(
            &router,
            domain,
            &format!("/{API_VERSION}/{COLLECTION}"),
            &origin_access_identity_xml(&format!("oai-edge-{round}")),
        )
        .await;
        assert!(
            !(200..300).contains(&status),
            "round {round}: expected a create addressed to the edge authority {domain:?} to be \
             answered by the edge, got {status}"
        );
    }
}
