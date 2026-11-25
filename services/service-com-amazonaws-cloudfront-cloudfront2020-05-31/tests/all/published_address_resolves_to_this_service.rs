// SPDX-License-Identifier: CC0-1.0

//! WHICH service answers at an address this one PUBLISHED, for exactly as long as it answers
//! there.
//!
//! The rule under test: **every address this service OPENED a listener at for a resource — the
//! address a viewer dials a distribution's published `DomainName` at, and a connection group's
//! `RoutingEndpoint` — is recorded, in the one engine-wide register of published data planes,
//! as an address THIS service answers at, and that record is withdrawn when the resource is
//! deleted and its port closes.** Nobody can read a service off a `host:port`, so that register
//! is what another plane asks before reading such a request's PATH as one of its own
//! addressings: without it, a viewer request to a distribution's edge whose path happens to
//! spell another plane's resource is claimed twice, and a record left behind after a delete
//! would claim a port this service no longer answers on.
//!
//! The rows differ along what the rule generalises over: the KIND of resource the address was
//! opened for (a distribution and a connection group, the two addresses this service
//! publishes), several addresses alive at once, and the two sides of the lifetime — published
//! while the port is open, withdrawn once it is closed, with the other addresses untouched.
//!
//! The wire harness is [`crate::distribution_lifecycle`]'s and the connection-group create
//! [`crate::connection_groups`]', reused rather than copied; the engine is stood up here
//! because the register is read off the SAME engine the service's own records live on.

use std::sync::Arc;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService;
use service_common::published_data_planes::PublishedPlanes;
use smithy_server_runtime::MultiServiceRouter;

use crate::connection_groups::create_connection_group;
use crate::distribution_lifecycle::{
    awaited_deployment, config, create_distribution, delete_distribution, edge_listener_authority,
    is_reachable_authority, update_distribution, Config, API_VERSION,
};

/// The signing name every address of this service is recorded under, which is the key the
/// emulator's front door routes a request by.
const CLOUDFRONT: &str = "cloudfront";

/// One emulator on an engine this suite KEEPS: the register of published addresses lives on
/// the same shared engine as this service's own records, so reading it needs that handle.
fn emulator_with_its_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// Create one distribution and answer `(Id, the address its edge is served at, ETag)`.
///
/// The address is not a modelled member: what the distribution PUBLISHES is the DNS name
/// CloudFront assigned it, and the address that name resolves to is the listener this emulator
/// opened for it ([`edge_listener_authority`]) — which is the address the register under test
/// records.
async fn created_distribution(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    row: &Config,
) -> (String, String, String) {
    let created = create_distribution(router, &row.xml()).await;
    assert_eq!(
        created.status,
        201,
        "case {}: expected HTTP 201 from CreateDistribution, got {} with {}",
        row.case,
        created.status,
        created.raw
    );
    let id = created.root().text_at("Id").unwrap_or_default().to_string();
    let address = edge_listener_authority(engine, row.case, &id);
    (id, address, created.header("etag").to_string())
}

/// Create one connection group and answer its `RoutingEndpoint`.
async fn created_group_endpoint(router: &MultiServiceRouter, name: &str) -> String {
    let request = format!(
        "<CreateConnectionGroupRequest \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <Name>{name}</Name></CreateConnectionGroupRequest>"
    );
    let created = create_connection_group(router, &request).await;
    assert_eq!(
        created.status, 201,
        "case {name:?}: expected HTTP 201 from CreateConnectionGroup, got {} with {}",
        created.status, created.raw
    );
    created
        .root()
        .find_text("RoutingEndpoint")
        .unwrap_or_else(|| {
            panic!("case {name:?}: the created group reports no RoutingEndpoint: {}", created.raw)
        })
        .to_string()
}

/// Every address this service publishes resolves, in the engine-wide register, to this
/// service — and only while the port behind it is open.
#[tokio::test]
async fn a_published_address_resolves_to_this_service_until_its_port_closes() {
    let (router, engine) = emulator_with_its_engine();
    let published = PublishedPlanes::of(&*engine).expect("the engine vends the register");

    // Two distributions, so one delete can be shown not to disturb the other, and a
    // connection group, which publishes the other kind of address this service opens.
    let first = config("the first distribution", "published-address-first");
    let second = config("the second distribution", "published-address-second");
    let (first_id, first_domain, first_etag) = created_distribution(&router, &engine, &first).await;
    let (_second_id, second_domain, _second_etag) =
        created_distribution(&router, &engine, &second).await;
    let group_endpoint = created_group_endpoint(&router, "published-address-group").await;

    // Each address is one a client can DIAL — the `host:port` of a listener of this
    // emulator's own — which is the class of address nothing can read a service off, and so
    // the one the register exists for.
    let addresses: [(&str, &str); 3] = [
        ("the address a distribution's edge is served at", first_domain.as_str()),
        (
            "the address another distribution's edge is served at",
            second_domain.as_str(),
        ),
        ("a connection group's RoutingEndpoint", group_endpoint.as_str()),
    ];
    for (case, address) in addresses {
        assert!(
            is_reachable_authority(address),
            "case {case:?}: expected a dialable host:port, got {address:?}"
        );
        let answering = published.answering_service(address);
        assert_eq!(
            answering.as_deref(),
            Some(CLOUDFRONT),
            "case {case:?}: expected {address:?} to resolve to {CLOUDFRONT:?}, got {answering:?}"
        );
    }

    // Addresses are per listener, so nothing this service did not open resolves: neither a
    // host nothing was published at, nor a port of the same host that is none of the ones
    // this test opened.
    let opened: [&str; 3] = [
        first_domain.as_str(),
        second_domain.as_str(),
        group_endpoint.as_str(),
    ];
    let unpublished: [(&str, String); 2] = [
        ("a host nothing was published at", "10.255.255.1:443".to_string()),
        (
            "another port of the same host",
            a_port_none_of_these_answer_on(&first_domain, &opened),
        ),
    ];
    for (case, address) in &unpublished {
        let answering = published.answering_service(address);
        assert_eq!(
            answering, None,
            "case {case:?}: expected {address:?} to resolve to nothing, got {answering:?}"
        );
    }

    // The delete closes the port, so the record goes with it — while every other address
    // this service still answers on keeps resolving.
    let disabled = Config {
        enabled: false,
        ..first.clone()
    };
    let updated = update_distribution(&router, &first_id, &disabled.update_xml(), Some(&first_etag))
        .await;
    assert_eq!(
        updated.status, 200,
        "case \"disable before the delete\": expected HTTP 200 from UpdateDistribution, got {} \
         with {}",
        updated.status, updated.raw
    );
    let disabled_etag = updated.header("etag").to_string();
    // A delete lands only once the disable has PROPAGATED, which is the clock's to owe: the
    // wait is on that condition, looked for again until it holds or the suite's own generous
    // deadline passes.
    awaited_deployment(&router, "disable before the delete", &first_id).await;
    let removed = delete_distribution(&router, &first_id, Some(&disabled_etag)).await;
    assert_eq!(
        removed.status, 204,
        "case \"the deleted distribution\": expected HTTP 204 from DeleteDistribution, got {} \
         with {}",
        removed.status, removed.raw
    );

    let answering = published.answering_service(&first_domain);
    assert_eq!(
        answering, None,
        "case \"the deleted distribution\": expected {first_domain:?} to resolve to nothing once \
         its port is closed, got {answering:?}"
    );
    for (case, address) in [
        (
            "the address another distribution's edge is served at",
            second_domain.as_str(),
        ),
        ("a connection group's RoutingEndpoint", group_endpoint.as_str()),
    ] {
        let answering = published.answering_service(address);
        assert_eq!(
            answering.as_deref(),
            Some(CLOUDFRONT),
            "case {case:?}: expected the surviving address {address:?} to still resolve to \
             {CLOUDFRONT:?}, got {answering:?}"
        );
    }
}

/// The same host as `authority` at a port none of `opened` answers on — the addresses this
/// test itself opened being every one this engine has published.
///
/// The port is walked upward rather than taken as `+1`: the operating system hands out
/// ephemeral ports in runs, so the next port along can be a sibling listener's, and a probe
/// that happened to name one would fail for something other than the rule under test.
fn a_port_none_of_these_answer_on(authority: &str, opened: &[&str]) -> String {
    let (host, port) = authority
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("a published address carries a port, got {authority:?}"));
    let port: u32 = port
        .parse()
        .unwrap_or_else(|_| panic!("a published address carries a numeric port, got {port:?}"));
    // Bounded by one more than the addresses there are: each can rule out at most one port.
    for step in 1..=(opened.len() as u32 + 1) {
        let probe = format!("{host}:{}", port.wrapping_add(step));
        if !opened
            .iter()
            .any(|taken| taken.eq_ignore_ascii_case(&probe))
        {
            return probe;
        }
    }
    panic!("a port none of {opened:?} answers on exists within {} steps", opened.len() + 1)
}
