// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the HOST this service ANNOUNCES in a `DnsEntry`.
//!
//! Every case drives the emulator the way a client does — a restJson1 request through the
//! router that mounts the service — and reads what it asserts back out of later requests
//! (`GetService`, `ListServices`, `GetServiceNetworkServiceAssociation`, the association
//! listing), so a create-time artifact and a read path are both under test.
//!
//! The rule under test, asserted over TABLES rather than one row: a `DnsEntry.domainName` is
//! a host a client RESOLVES and then DIALS, so it is the host THIS EMULATOR IS REACHED AT —
//! the host of the authority the request being answered addressed, or of the one address this
//! process is served at for a caller that addressed a host naming some service — and never a
//! name derived from it, which nothing resolves, and never a host in a zone AWS serves its own
//! endpoints or its lattice data plane in, which resolves nowhere near this process. No port
//! travels in the value: the port a client connects on is the service's listener's. The model
//! places no pattern or length constraint on `domainName`
//! (`smithy_query(query="shape",shape="DnsEntry")`), so what is required of the value is that
//! it be the host this process answers at, in the form a client writes a host in.
//!
//! The rows differ in WHAT the client addressed, which is what the reading generalises over
//! (an IPv4 socket address with and without a port, a routable address, a loopback name with a
//! port, a bracketed IPv6 literal, an authority carrying userinfo, and the service's own
//! published AWS host — the one addressing that names a service, which is answered the address
//! this process is served at instead), and in the SERVICE the entry is announced for, which
//! the value may not depend on.

use crate::vpclattice_service_slice::{Answer, Lattice, REGION};

/// The host this emulator is served at for a caller that addressed a host naming some
/// service: these tests drive a router served at no address of its own, so it is the host of
/// the unserved base (`smithy_server_runtime::UNSERVED_URL_BASE`), a name RFC 2606 reserves
/// so that nothing resolves it.
pub(crate) fn unserved_host() -> &'static str {
    ::service_common::data_plane_addressing::url_authority(
        ::smithy_server_runtime::UNSERVED_URL_BASE,
    )
}

/// The addressings the table spans, each with the host the service must announce for a client
/// that reached this emulator there.
fn addressing_cases() -> Vec<(&'static str, String, String)> {
    vec![
        (
            "an IPv4 socket address with a port",
            "127.0.0.1:39355".to_string(),
            "127.0.0.1".to_string(),
        ),
        (
            "an IPv4 address carrying no port",
            "127.0.0.1".to_string(),
            "127.0.0.1".to_string(),
        ),
        (
            "a routable address with a port",
            "10.1.2.3:9297".to_string(),
            "10.1.2.3".to_string(),
        ),
        (
            "the loopback name with a port",
            "localhost:9297".to_string(),
            "localhost".to_string(),
        ),
        (
            "a bracketed IPv6 literal with a port",
            "[::1]:9297".to_string(),
            "[::1]".to_string(),
        ),
        (
            // A value carrying userinfo is not shaped like a host, so it is not taken as the
            // address the request reached: such a caller is answered the address this process
            // is served at.
            "an authority carrying userinfo",
            "operator@localhost:9297".to_string(),
            unserved_host().to_string(),
        ),
        (
            // The one addressing that names a SERVICE, so it is a caller that resolves AWS's
            // own hosts: it is answered the address this process is served at.
            "the service's own published AWS host",
            format!("vpc-lattice.{REGION}.amazonaws.com"),
            unserved_host().to_string(),
        ),
    ]
}

/// The `dnsEntry` of `value`, with its `domainName` and `hostedZoneId`.
fn dns_entry_of(case: &str, value: &serde_json::Value) -> (String, String) {
    let entry = &value["dnsEntry"];
    let domain = entry["domainName"].as_str().unwrap_or_else(|| {
        panic!("case {case}: expected a dnsEntry.domainName, got {value}")
    });
    let zone = entry["hostedZoneId"].as_str().unwrap_or_else(|| {
        panic!("case {case}: expected a dnsEntry.hostedZoneId, got {value}")
    });
    (domain.to_string(), zone.to_string())
}

/// Assert `domain` is a host in the form a client writes one — resolvable, writable in a
/// `Host` header and usable as a TLS server name — carrying no port, no scheme and no path,
/// and in no zone AWS publishes an endpoint or its lattice data plane in.
fn assert_host_shaped(case: &str, domain: &str) {
    assert!(
        ::service_common::data_plane_addressing::is_host_shaped(domain),
        "case {case}: {domain:?} is not shaped like a host"
    );
    assert!(
        !domain.contains("://") && !domain.contains('/'),
        "case {case}: {domain:?} carries a scheme or a path, so it is not a host"
    );
    assert!(
        !domain.contains('@'),
        "case {case}: {domain:?} carries userinfo, which no host does"
    );
    assert_eq!(
        ::service_common::data_plane_addressing::addressed_port(Some(domain)),
        None,
        "case {case}: {domain:?} carries a port; the port a client connects on comes from \
         the service's listener"
    );
    assert!(
        domain.len() <= 253,
        "case {case}: {domain:?} is longer than a host may be"
    );
    // The zones AWS publishes its own endpoints in
    // (`service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES`), plus the zone the
    // real VPC Lattice data plane hands services out in.
    assert!(
        !::service_common::data_plane_addressing::addresses_an_aws_endpoint(domain),
        "case {case}: {domain:?} is a host in a zone AWS serves its own endpoints in, \
         which this emulator does not answer at"
    );
    assert!(
        !domain.contains("vpc-lattice-svcs"),
        "case {case}: {domain:?} names the real lattice data plane, \
         which this emulator does not answer at"
    );
}

/// Assert the announced value is exactly the host this emulator was reached at — not a name
/// derived from it, under it or beside it.
fn assert_announced(case: &str, expected: &str, domain: &str) {
    assert_eq!(
        domain, expected,
        "case {case}: expected the host this emulator is reached at ({expected:?}), got \
         {domain:?}"
    );
    assert_host_shaped(case, domain);
}

/// A hosted-zone id is the per-resource value Route 53 shape: `Z` and 19 `[0-9A-Z]`
/// characters. It is the resource's own, so it must NOT change with the addressing.
fn assert_hosted_zone(case: &str, zone: &str) {
    assert!(
        zone.len() == 20
            && zone.starts_with('Z')
            && zone[1..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase()),
        "case {case}: expected a hosted zone id of `Z` and 19 [0-9A-Z] characters, \
         got {zone:?}"
    );
}

/// Every operation that serializes a `DnsEntry` announces the host this emulator is reached
/// at: the service create and its read-back and listing, and the association create, its
/// read-back and its listing.
#[tokio::test]
async fn every_dns_entry_announces_the_host_this_emulator_is_reached_at() {
    let lattice = Lattice::new();

    for (index, (case, host, expected)) in addressing_cases().into_iter().enumerate() {
        // One service and one service network per row, so each row's association is a
        // fresh pair (a pair may be associated only once).
        let network = {
            let created = lattice
                .call_from_host(
                    &host,
                    "POST",
                    "/servicenetworks",
                    Some(serde_json::json!({ "name": format!("announced-net-{index}") })),
                )
                .await;
            assert_eq!(
                created.status, 201,
                "case {case}: the fixture service network expected 201, got {} / {}",
                created.status, created.body
            );
            created.str_field("id")
        };

        // CreateService.
        let name = format!("announced-host-{index}");
        let created = lattice
            .call_from_host(
                &host,
                "POST",
                "/services",
                Some(serde_json::json!({ "name": name })),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {case}: CreateService expected 201, got {} / {}",
            created.status, created.body
        );
        let service = created.str_field("id");
        let (domain, hosted_zone) = dns_entry_of(case, &created.body);
        assert_announced(&format!("{case}, CreateService"), &expected, &domain);
        assert_hosted_zone(&format!("{case}, CreateService"), &hosted_zone);

        // GetService: the read path announces the same host, and reports the service's
        // own hosted zone.
        let read = lattice
            .call_from_host(&host, "GET", &format!("/services/{service}"), None)
            .await;
        assert_eq!(
            read.status, 200,
            "case {case}: GetService expected 200, got {} / {}",
            read.status, read.body
        );
        let (read_domain, read_zone) = dns_entry_of(case, &read.body);
        assert_announced(&format!("{case}, GetService"), &expected, &read_domain);
        assert_eq!(
            read_zone, hosted_zone,
            "case {case}: GetService must report the service's own hosted zone \
             {hosted_zone:?}, got {read_zone:?}"
        );

        // ListServices: every summary of the listing announces the same host, whichever
        // service it reports.
        let listed = lattice
            .call_from_host(&host, "GET", "/services", None)
            .await;
        assert_eq!(
            listed.status, 200,
            "case {case}: ListServices expected 200, got {} / {}",
            listed.status, listed.body
        );
        let items = listed.body["items"].as_array().cloned().unwrap_or_default();
        assert!(
            !items.is_empty(),
            "case {case}: ListServices must report the persisted services, got {}",
            listed.body
        );
        for item in &items {
            let (listed_domain, _) = dns_entry_of(case, item);
            assert_announced(&format!("{case}, ListServices"), &expected, &listed_domain);
        }

        // CreateServiceNetworkServiceAssociation: the association reports the ASSOCIATED
        // service's DNS information, which is the same host and that service's hosted zone.
        let associated = lattice
            .call_from_host(
                &host,
                "POST",
                "/servicenetworkserviceassociations",
                Some(serde_json::json!({
                    "serviceIdentifier": service,
                    "serviceNetworkIdentifier": network,
                })),
            )
            .await;
        assert_eq!(
            associated.status, 200,
            "case {case}: the association expected 200, got {} / {}",
            associated.status, associated.body
        );
        let association = associated.str_field("id");
        let (association_domain, association_zone) = dns_entry_of(case, &associated.body);
        assert_announced(
            &format!("{case}, CreateServiceNetworkServiceAssociation"),
            &expected,
            &association_domain,
        );
        assert_eq!(
            association_zone, hosted_zone,
            "case {case}: the association reports the associated service's hosted zone \
             {hosted_zone:?}, got {association_zone:?}"
        );

        // GetServiceNetworkServiceAssociation.
        let read_association = lattice
            .call_from_host(
                &host,
                "GET",
                &format!("/servicenetworkserviceassociations/{association}"),
                None,
            )
            .await;
        assert_eq!(
            read_association.status, 200,
            "case {case}: the association read-back expected 200, got {} / {}",
            read_association.status, read_association.body
        );
        let (read_association_domain, _) = dns_entry_of(case, &read_association.body);
        assert_announced(
            &format!("{case}, GetServiceNetworkServiceAssociation"),
            &expected,
            &read_association_domain,
        );

        // ListServiceNetworkServiceAssociations.
        let listed_associations = lattice
            .call_from_host(
                &host,
                "GET",
                &format!(
                    "/servicenetworkserviceassociations?serviceNetworkIdentifier={network}"
                ),
                None,
            )
            .await;
        assert_eq!(
            listed_associations.status, 200,
            "case {case}: the association listing expected 200, got {} / {}",
            listed_associations.status, listed_associations.body
        );
        let association_items = listed_associations.body["items"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            association_items.len(),
            1,
            "case {case}: the listing must report the one persisted association, got {}",
            listed_associations.body
        );
        let (listed_association_domain, _) = dns_entry_of(case, &association_items[0]);
        assert_announced(
            &format!("{case}, ListServiceNetworkServiceAssociations"),
            &expected,
            &listed_association_domain,
        );
    }
}

/// The announced host is the one the request reached this emulator at, read back through the
/// tree's own reading of that question rather than through this suite's table — and the
/// emulator answers a request addressed at it, which is what a client that resolves and dials
/// the announced value does.
///
/// The rows span the addressings a client reaches this emulator on; each row's own announced
/// value is fed straight back as the next request's host, and the service is read through it.
#[tokio::test]
async fn the_announced_host_is_an_address_this_emulator_is_reached_at() {
    let lattice = Lattice::new();
    for (index, (case, host, _)) in addressing_cases().into_iter().enumerate() {
        let name = format!("answered-at-{index}");
        let created = lattice
            .call_from_host(
                &host,
                "POST",
                "/services",
                Some(serde_json::json!({ "name": name })),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {case}: CreateService expected 201, got {} / {}",
            created.status, created.body
        );
        let service = created.str_field("id");
        let (announced, _) = dns_entry_of(case, &created.body);

        // One of the emulator's OWN hosts, by the tree's reading of what a client reaches
        // this emulator at: the host of the authority the request addressed, or the host of
        // the address this process is served at. A name minted under either is neither.
        let addressed = ::service_common::data_plane_addressing::reached_emulator_endpoint(
            &format!("https://{host}/services"),
            Some(host.as_str()),
        );
        let own: Vec<String> = addressed
            .iter()
            .map(|authority| {
                ::service_common::data_plane_addressing::host_only(authority).to_string()
            })
            .chain([unserved_host().to_string()])
            .collect();
        assert!(
            own.iter().any(|expected| expected == &announced),
            "case {case}: the announced host must be one this emulator is reached at \
             ({own:?}), got {announced:?}"
        );

        // The announced host, dialled: the emulator answers the read at the very address it
        // handed the client.
        let read = lattice
            .call_from_host(&announced, "GET", &format!("/services/{service}"), None)
            .await;
        assert_eq!(
            read.status, 200,
            "case {case}: a request addressed at the announced host {announced:?} expected \
             200, got {} / {}",
            read.status, read.body
        );
        assert_eq!(
            read.str_field("id"),
            service,
            "case {case}: the read at the announced host must report the service it was \
             asked for, got {}",
            read.body
        );
    }
}

/// One resource, read from several addressings: each caller is announced the host IT reaches
/// this emulator at, and the resource's own hosted zone never moves.
#[tokio::test]
async fn one_service_announces_each_caller_the_host_that_caller_reaches() {
    let lattice = Lattice::new();
    let cases = addressing_cases();
    let (_, first_host, _) = &cases[0];
    let name = "announced-per-caller";

    let created = lattice
        .call_from_host(
            first_host,
            "POST",
            "/services",
            Some(serde_json::json!({ "name": name })),
        )
        .await;
    assert_eq!(
        created.status, 201,
        "the fixture service expected 201, got {} / {}",
        created.status, created.body
    );
    let service = created.str_field("id");
    let (_, hosted_zone) = dns_entry_of("the fixture service", &created.body);

    for (case, host, expected) in &cases {
        let read = lattice
            .call_from_host(host, "GET", &format!("/services/{service}"), None)
            .await;
        assert_eq!(
            read.status, 200,
            "case {case}: GetService expected 200, got {} / {}",
            read.status, read.body
        );
        let (domain, read_zone) = dns_entry_of(case, &read.body);
        assert_announced(case, expected, &domain);
        assert_eq!(
            &read_zone, &hosted_zone,
            "case {case}: the service's hosted zone {hosted_zone:?} must not move with \
             the addressing, got {read_zone:?}"
        );
    }
}

/// The announced host is a property of the REQUEST, not of the service: two services created
/// and read through one addressing are announced the same host, and each keeps its own
/// hosted zone.
///
/// The rows span the modelled `ServiceName` (len 3..40, `[a-z0-9-]`) — the shortest name, a
/// hyphenated one, a digit-carrying one and the longest — so no property of a service's own
/// name or id can reach the announced value.
#[tokio::test]
async fn the_announced_host_does_not_depend_on_the_service_it_is_announced_for() {
    let lattice = Lattice::new();
    let host = "127.0.0.1:39355";
    let expected = "127.0.0.1";
    let names: Vec<(&str, String)> = vec![
        ("the shortest modelled name", "abc".to_string()),
        ("a hyphenated name", "my-lattice-service".to_string()),
        ("a name carrying digits", "svc2-payments-7".to_string()),
        ("the longest modelled name (40 characters)", "a".repeat(40)),
    ];

    let mut hosted_zones: Vec<String> = Vec::new();
    for (case, name) in &names {
        let created = lattice
            .call_from_host(
                host,
                "POST",
                "/services",
                Some(serde_json::json!({ "name": name })),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {case}: CreateService expected 201, got {} / {}",
            created.status, created.body
        );
        let id = created.str_field("id");
        let (domain, hosted_zone) = dns_entry_of(case, &created.body);
        assert_announced(case, expected, &domain);
        assert!(
            !domain.contains(name.as_str()) && !domain.contains(id.trim_start_matches("svc-")),
            "case {case}: the announced host must be this emulator's address rather than a \
             name minted from the service {name:?} / {id:?}, got {domain:?}"
        );
        hosted_zones.push(hosted_zone);
    }

    hosted_zones.sort();
    let before = hosted_zones.len();
    hosted_zones.dedup();
    assert_eq!(
        hosted_zones.len(),
        before,
        "no two services may share a hosted zone, got {hosted_zones:?}"
    );
}

/// Reading a service twice from one addressing announces exactly the same host: the value is
/// a function of the request, not of when it was asked for.
#[tokio::test]
async fn the_announced_host_is_stable_across_reads_from_one_addressing() {
    let lattice = Lattice::new();
    for (index, (case, host, expected)) in addressing_cases().into_iter().enumerate() {
        let name = format!("stable-host-{index}");
        let created = lattice
            .call_from_host(
                &host,
                "POST",
                "/services",
                Some(serde_json::json!({ "name": name })),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {case}: CreateService expected 201, got {} / {}",
            created.status, created.body
        );
        let service = created.str_field("id");
        let mut seen: Vec<String> = Vec::new();
        for _ in 0..3 {
            let read: Answer = lattice
                .call_from_host(&host, "GET", &format!("/services/{service}"), None)
                .await;
            assert_eq!(
                read.status, 200,
                "case {case}: GetService expected 200, got {} / {}",
                read.status, read.body
            );
            let (domain, _) = dns_entry_of(case, &read.body);
            assert_announced(case, &expected, &domain);
            seen.push(domain);
        }
        assert!(
            seen.windows(2).all(|pair| pair[0] == pair[1]),
            "case {case}: every read must announce one host, got {seen:?}"
        );
    }
}
