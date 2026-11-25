// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS the control plane ANNOUNCES, answered as the service's own traffic.
//!
//! `CreateService`'s `dnsEntry` is "The public DNS name of the service" and `DnsEntry`'s
//! `domainName` is "the domain name of the service", paired with the hosted zone it lives in
//! (`smithy_query(query="shape",shape="DnsEntry")`), so a client that holds nothing but that
//! value resolves it and sends an ordinary request to it on the service's LISTENER port — the
//! member carries no port of its own. The announced host is this emulator's own
//! (`crate::state::announced_dns_hosts`, whose rule the `announced_dns_entry` suite owns) and
//! names no service, so what makes the address servable is the RECORD a listener keeps of the
//! addresses it answers at (`crate::state::data_plane_addresses`).
//!
//! THE RULE under test, in one sentence: an address the control plane announced for a service
//! is an address this emulator ANSWERS that service's traffic at — the announced host on the
//! listener's port — and nowhere does that claim take a request this service's own MODEL binds
//! a route for, which is its control plane's at the very same host.
//!
//! Every case drives the real wire path, with a real deployment package behind the listener:
//! an HTTP request to the announced address carrying no AWS credential, exactly as a client in
//! an associated VPC sends it, answered by a function created through Lambda's OWN API and RUN
//! by the Lambda emulator on this suite's engine — so a row that is served is served by the
//! target, and the path the function echoes is the one this plane read off the request.
//!
//! The rows differ along what the rule generalises over: the form the client writes the
//! address in (the announced host alone, and with the listener's port), the request inside it
//! (the root path, a path of several segments, a query, the verb), the listener's PORT (a
//! documented default and a port of its own, which a URL has to write), and what leaves the
//! address unserved (no VPC association, a deleted listener, two services announced at one
//! address, and a path this model binds a route for).

use std::sync::Arc;

use crate::listeners::{a_target_function, create_listener, settled_active};
use crate::targets::{a_target_group, register_targets};
use crate::vpclattice_service_slice::{Fetched, Lattice, REGION};

/// The address the client of this suite reaches the emulator at, so what it is announced is
/// that host (`crate::state::announced_dns_host`) rather than the unserved base an in-process
/// caller addressing an AWS host is answered.
const CLIENT_HOST: &str = "127.0.0.1:39355";

/// What the target function answers a request it was handed: its own 200, carrying the event
/// this plane built as JSON.
const SERVED_STATUS: u16 = 200;

/// What the front door answers for an address nothing of this emulator serves: its own answer
/// for a request no service claimed.
const UNSERVED_STATUS: u16 = 404;

/// One service, announced at one address, with a listener forwarding to a `LAMBDA` target
/// group that holds a real function.
struct Announced {
    service_id: String,
    /// The host `GetService` announces for it, which is the address a client is handed.
    announced: String,
    listener_id: String,
    /// The target group the listener's default action forwards to, which the clock owes its
    /// own `ACTIVE` status.
    group_id: String,
}

/// Create that estate and answer it.
async fn announced_service(
    lattice: &Lattice,
    engine: &Arc<engine::InMemoryEngine>,
    name: &str,
    protocol: &str,
    port: i64,
) -> Announced {
    let created = lattice
        .call_from_host(
            CLIENT_HOST,
            "POST",
            "/services",
            Some(serde_json::json!({ "name": name })),
        )
        .await;
    assert_eq!(
        created.status, 201,
        "setup CreateService {name:?}: expected 201, got {} / {}",
        created.status, created.body
    );
    let service_id = created.str_field("id");
    let function_arn = a_target_function(lattice, engine, &format!("{name}-target")).await;
    let (group_id, _group_arn) = a_target_group(lattice, &format!("{name}-group"), "LAMBDA", None).await;
    let registered =
        register_targets(lattice, &group_id, serde_json::json!([{ "id": function_arn }])).await;
    assert_eq!(
        registered.status, 200,
        "setup RegisterTargets for {name:?}: expected 200, got {} / {}",
        registered.status, registered.body
    );
    let listener = create_listener(
        lattice,
        &service_id,
        serde_json::json!({
            "name": "front",
            "protocol": protocol,
            "port": port,
            "defaultAction": {
                "forward": { "targetGroups": [{ "targetGroupIdentifier": group_id }] },
            },
        }),
    )
    .await;
    assert_eq!(
        listener.status, 201,
        "setup CreateListener for {name:?}: expected 201, got {} / {}",
        listener.status, listener.body
    );
    let read = lattice
        .call_from_host(CLIENT_HOST, "GET", &format!("/services/{service_id}"), None)
        .await;
    assert_eq!(
        read.status, 200,
        "setup GetService {service_id}: expected 200, got {} / {}",
        read.status, read.body
    );
    let announced = read.body["dnsEntry"]["domainName"]
        .as_str()
        .unwrap_or_else(|| panic!("setup GetService {service_id}: announced no domain name"))
        .to_string();
    Announced {
        service_id,
        announced,
        listener_id: listener.str_field("id"),
        group_id,
    }
}

/// A service network holding the service, with `vpc` associated with it — the pair that puts a
/// client in a position to reach the service at all.
async fn reachable_through(lattice: &Lattice, name: &str, service_id: &str, vpc: &str) {
    let network = lattice
        .call_from_host(
            CLIENT_HOST,
            "POST",
            "/servicenetworks",
            Some(serde_json::json!({ "name": name })),
        )
        .await;
    assert_eq!(
        network.status, 201,
        "setup CreateServiceNetwork {name:?}: expected 201, got {} / {}",
        network.status, network.body
    );
    let network_id = network.str_field("id");
    let associated = lattice.associate(service_id, &network_id).await;
    assert_eq!(
        associated.status, 200,
        "setup CreateServiceNetworkServiceAssociation: expected 200, got {} / {}",
        associated.status, associated.body
    );
    let vpc_association = lattice
        .call_from_host(
            CLIENT_HOST,
            "POST",
            "/servicenetworkvpcassociations",
            Some(serde_json::json!({
                "serviceNetworkIdentifier": network_id,
                "vpcIdentifier": vpc,
            })),
        )
        .await;
    assert_eq!(
        vpc_association.status, 200,
        "setup CreateServiceNetworkVpcAssociation: expected 200, got {} / {}",
        vpc_association.status, vpc_association.body
    );
}

/// Wait out the `ACTIVE` statuses the clock owes the service and its target group by LOOKING
/// AGAIN: a resource the clock still owes one is not serving traffic yet.
async fn settled(lattice: &Lattice, case: &str, estate: &Announced) {
    settled_active(lattice, case, &format!("/services/{}", estate.service_id)).await;
    settled_active(lattice, case, &format!("/targetgroups/{}", estate.group_id)).await;
}

/// The event the function echoed back, which is the one this plane built from the request.
fn echoed(case: &str, answered: &Fetched) -> serde_json::Value {
    serde_json::from_str(&answered.body).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: expected the function's own JSON answer, got {} / {:?} ({error})",
            answered.status, answered.body
        )
    })
}

/// The rule over the table in the module header, for a listener on the DOCUMENTED DEFAULT port
/// of its protocol — the port a URL writes no port for.
#[tokio::test]
async fn the_announced_address_answers_the_services_own_traffic() {
    let (lattice, engine) = Lattice::with_lambda();
    let estate = announced_service(&lattice, &engine, "announced-shop", "HTTP", 80).await;
    let announced = estate.announced.clone();

    // Nothing is in a position to reach the service yet: no VPC is associated with a service
    // network it is associated with, so the announced address is served by nothing.
    let unreachable = lattice.fetch("GET", &announced, "/", "", &[]).await;
    assert_eq!(
        unreachable.status, UNSERVED_STATUS,
        "case \"no VPC associated with the service network\": expected the announced address \
         to be served by nothing, got {} with body {:?}",
        unreachable.status, unreachable.body
    );

    reachable_through(
        &lattice,
        "announced-net",
        &estate.service_id,
        "vpc-0123456789abcdef0",
    )
    .await;
    settled(&lattice, "the announced service", &estate).await;

    // (case, verb, the address the client writes, the request inside it, the path the function
    // must have been handed)
    let rows: Vec<(&str, &str, String, &str, &str)> = vec![
        (
            "the announced host alone, at the root path",
            "GET",
            announced.clone(),
            "/",
            "/",
        ),
        (
            "the announced host alone, at a path of several segments",
            "GET",
            announced.clone(),
            "/probe/deep/path",
            "/probe/deep/path",
        ),
        (
            "the announced host alone, with a query",
            "GET",
            announced.clone(),
            "/orders?shelf=front",
            "/orders",
        ),
        (
            "the announced host with the listener's port written out",
            "GET",
            format!("{announced}:80"),
            "/probe/1",
            "/probe/1",
        ),
        (
            "another verb to the same address",
            "POST",
            announced.clone(),
            "/orders",
            "/orders",
        ),
    ];
    for (case, verb, address, target, path) in rows {
        let answered = lattice.fetch(verb, &address, target, "", &[]).await;
        assert_eq!(
            answered.status, SERVED_STATUS,
            "case {case:?}: expected the listener of the service announced at {address:?} to \
             answer {SERVED_STATUS}, got {} with body {:?}",
            answered.status, answered.body
        );
        let event = echoed(case, &answered);
        assert_eq!(
            (
                event["method"].as_str(),
                event["raw_path"].as_str(),
                event["host"].as_str()
            ),
            (Some(verb), Some(path), Some(address.as_str())),
            "case {case:?}: expected the target to be handed {verb} {path:?} at {address:?}, \
             got {event}"
        );
    }

    // The CONTROL plane answers at that very host: a request this service's own model binds a
    // route for is never read as a service's traffic, however the address was announced.
    let read = lattice
        .call_from_host(
            CLIENT_HOST,
            "GET",
            &format!("/services/{}", estate.service_id),
            None,
        )
        .await;
    assert_eq!(
        (read.status, read.str_field("id")),
        (200, estate.service_id.clone()),
        "case \"a modelled route at the announced host\": expected the control plane's own \
         answer, got {} / {}",
        read.status,
        read.body
    );

    // The listener is what answers the address: with it deleted, nothing serves it.
    let deleted = lattice
        .call_from_host(
            CLIENT_HOST,
            "DELETE",
            &format!(
                "/services/{}/listeners/{}",
                estate.service_id, estate.listener_id
            ),
            None,
        )
        .await;
    assert_eq!(
        deleted.status, 204,
        "setup DeleteListener: expected 204, got {} / {}",
        deleted.status, deleted.body
    );
    let unserved = lattice.fetch("GET", &announced, "/probe/1", "", &[]).await;
    assert_eq!(
        unserved.status, UNSERVED_STATUS,
        "case \"the listener deleted\": expected the announced address to be served by \
         nothing, got {} with body {:?}",
        unserved.status, unserved.body
    );
}

/// The PORT half of the rule: a listener on a port of its own is reached at the address that
/// writes that port, and the bare announced host — which names the default port of the
/// client's scheme — is not that address.
#[tokio::test]
async fn a_listener_on_its_own_port_answers_only_at_the_address_that_writes_it() {
    let (lattice, engine) = Lattice::with_lambda();
    let estate = announced_service(&lattice, &engine, "announced-side-door", "HTTP", 8080).await;
    reachable_through(
        &lattice,
        "announced-side-net",
        &estate.service_id,
        "vpc-0123456789abcdef1",
    )
    .await;
    settled(&lattice, "the announced service", &estate).await;

    let rows: &[(&str, &str, u16)] = &[
        ("the address the control plane published", ":8080", SERVED_STATUS),
        ("the announced host with no port", "", UNSERVED_STATUS),
        ("the announced host on another port", ":9090", UNSERVED_STATUS),
    ];
    for (case, suffix, expected) in rows {
        let address = format!("{}{suffix}", estate.announced);
        let answered = lattice.fetch("GET", &address, "/probe/1", "", &[]).await;
        assert_eq!(
            answered.status, *expected,
            "case {case:?}: expected {expected} at {address:?}, got {} with body {:?}",
            answered.status, answered.body
        );
    }
}

/// Where the records leave DOUBT: two services announced at ONE address on one port. Neither
/// is served there — nothing about such a request says which service the client was handed
/// that address for — while each is still reached by its own host-style name and by the
/// reserved data-plane path, the two addressings that name the service in the request.
#[tokio::test]
async fn one_announced_address_shared_by_two_services_serves_neither() {
    let (lattice, engine) = Lattice::with_lambda();
    let first = announced_service(&lattice, &engine, "announced-first", "HTTP", 80).await;
    let second = announced_service(&lattice, &engine, "announced-second", "HTTP", 80).await;
    assert_eq!(
        first.announced, second.announced,
        "the two services are announced at one host, got {:?} and {:?}",
        first.announced, second.announced
    );
    for (index, estate) in [&first, &second].into_iter().enumerate() {
        reachable_through(
            &lattice,
            &format!("announced-shared-net-{index}"),
            &estate.service_id,
            &format!("vpc-0123456789abcdef{}", index + 2),
        )
        .await;
        settled(&lattice, "a service announced at the shared address", estate).await;
    }

    let shared = lattice
        .fetch("GET", &first.announced, "/probe/1", "", &[])
        .await;
    assert_eq!(
        shared.status, UNSERVED_STATUS,
        "case \"two services announced at one address\": expected neither to be served there, \
         got {} with body {:?}",
        shared.status, shared.body
    );

    for (case, estate, name) in [
        ("the first service", &first, "announced-first"),
        ("the second service", &second, "announced-second"),
    ] {
        let label = format!(
            "{}.{}",
            service_com_amazonaws_vpclattice_mercurycontrolplane::state::service_label(
                name,
                &estate.service_id
            ),
            ::service_common::data_plane_addressing::LOOPBACK_ZONE
        );
        let answered = lattice.fetch("GET", &label, "/probe/1", "", &[]).await;
        assert_eq!(
            answered.status, SERVED_STATUS,
            "case {case:?}: expected its own published name {label:?} to be served, got {} \
             with body {:?}",
            answered.status, answered.body
        );
        let prefixed = lattice
            .fetch(
                "GET",
                &format!("vpc-lattice.{REGION}.amazonaws.com"),
                &format!("/_aws/vpc-lattice/{}/probe/1", estate.service_id),
                "",
                &[],
            )
            .await;
        assert_eq!(
            prefixed.status, SERVED_STATUS,
            "case {case:?}: expected the reserved data-plane path to be served, got {} with \
             body {:?}",
            prefixed.status, prefixed.body
        );
    }
}
