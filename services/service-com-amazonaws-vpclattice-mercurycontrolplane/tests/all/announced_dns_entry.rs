// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the DNS NAME this service ANNOUNCES in a `DnsEntry`.
//!
//! Every case drives the emulator the way a client does — a restJson1 request through the
//! router that mounts the service — and reads what it asserts back out of later requests
//! (`GetService`, `ListServices`, `GetServiceNetworkServiceAssociation`, the association
//! listing), so a create-time artifact and a read path are both under test.
//!
//! The rule under test, asserted over TABLES rather than one row: a `DnsEntry.domainName`
//! is a DOMAIN NAME — dot-separated labels of letters, digits and hyphens, more than one
//! of them, carrying no `:` and no port — made of the SERVICE's own name and id in the
//! zone this emulator answers the request in: the NAME the request reached it on, or the
//! reserved loopback zone when it was reached at an address, which names no zone. It is
//! never a host in a zone AWS serves its own endpoints or its lattice data plane in, which
//! resolves nowhere near this process, and never the endpoint authority the request
//! arrived on, which no resolver, `Host` header or TLS server name accepts. The model
//! places no pattern or length constraint on `domainName`
//! (`smithy_query(query="shape",shape="DnsEntry")`), so what is required of the value is
//! its form — a DNS name of this resource — and that the zone it names be one of this
//! process.
//!
//! The rows differ in WHAT the client addressed, which is what the zone half generalises
//! over (an IPv4 socket address with a port, a loopback name with a port, a bracketed IPv6
//! literal, an address without a port, and the service's own published AWS host — the one
//! addressing that names a service, which is answered the zone this process is served in
//! instead), and in the service's own NAME and id, which is what the first label
//! generalises over.

use crate::vpclattice_service_slice::{Answer, Lattice, REGION};

/// The zone this emulator answers in for a caller that addressed a host naming some
/// service: these tests drive a router served at no address of its own, so it is the host
/// of the unserved base (`smithy_server_runtime::UNSERVED_URL_BASE`), a name RFC 2606
/// reserves so that nothing resolves it.
pub(crate) fn unserved_zone() -> &'static str {
    ::service_common::data_plane_addressing::url_authority(
        ::smithy_server_runtime::UNSERVED_URL_BASE,
    )
}

/// The DNS name a service called `name` with id `id` is announced under in `zone`: the
/// service's own label — its name and the distinguishing part of its id — and the zone.
/// Shared with the lifecycle suite, so one reading of the announced name is asserted.
pub(crate) fn announced_name(zone: &str, name: &str, id: &str) -> String {
    let distinguisher = id
        .strip_prefix("svc-")
        .unwrap_or_else(|| panic!("expected a modelled service id, got {id:?}"));
    format!("{name}-{distinguisher}.{zone}")
}

/// The addressings the table spans, each with the zone the service must announce its name
/// in for a client that reached this emulator there.
fn addressing_cases() -> Vec<(&'static str, String, String)> {
    vec![
        (
            "an IPv4 socket address with a port",
            "127.0.0.1:39355".to_string(),
            // An address names no zone, so the name is minted in the reserved loopback
            // zone, which resolves to the interface the core binds its listeners on.
            "localhost".to_string(),
        ),
        (
            "the loopback name with a port",
            "localhost:9297".to_string(),
            "localhost".to_string(),
        ),
        (
            "a bracketed IPv6 literal with a port",
            "[::1]:9297".to_string(),
            "localhost".to_string(),
        ),
        (
            "an address carrying no port",
            "127.0.0.1".to_string(),
            "localhost".to_string(),
        ),
        (
            "the service's own published AWS host",
            format!("vpc-lattice.{REGION}.amazonaws.com"),
            unserved_zone().to_string(),
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

/// Assert `domain` is a well-formed DNS name — the form a client can resolve, send as a
/// `Host` header and use as a TLS server name — and in no zone AWS publishes an endpoint
/// or its lattice data plane in.
fn assert_dns_name(case: &str, domain: &str) {
    assert!(
        !domain.contains(':'),
        "case {case}: {domain:?} carries a `:`, so it is an endpoint authority and not a \
         DNS name; the port a client connects on comes from the service's listener"
    );
    assert!(
        domain
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-'),
        "case {case}: {domain:?} carries a character no DNS name does"
    );
    let labels: Vec<&str> = domain.split('.').collect();
    assert!(
        labels.len() >= 2,
        "case {case}: {domain:?} is a single label, not a domain name"
    );
    assert!(
        domain.len() <= 253,
        "case {case}: {domain:?} is longer than a DNS name may be"
    );
    for label in &labels {
        assert!(
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-'),
            "case {case}: {label:?} in {domain:?} is not a DNS label"
        );
    }
    // The zones AWS publishes its own endpoints in
    // (`service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES`), plus the zone
    // the real VPC Lattice data plane hands services out in.
    assert!(
        !::service_common::data_plane_addressing::addresses_an_aws_endpoint(domain),
        "case {case}: {domain:?} is a host in a zone AWS serves its own endpoints in, \
         which this emulator does not answer in"
    );
    assert!(
        !domain.contains("vpc-lattice-svcs"),
        "case {case}: {domain:?} names the real lattice data plane, \
         which this emulator does not answer in"
    );
}

/// Assert the announced name is this service's own, in the zone this emulator answers the
/// call in.
fn assert_announced(case: &str, expected: &str, domain: &str) {
    assert_eq!(
        domain, expected,
        "case {case}: expected this service's own name in the zone this emulator answers \
         in ({expected:?}), got {domain:?}"
    );
    assert_dns_name(case, domain);
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

/// Every operation that serializes a `DnsEntry` announces the service's own DNS name in
/// the zone this emulator answers the call in: the service create and its read-back and
/// listing, and the association create, its read-back and its listing.
#[tokio::test]
async fn every_dns_entry_announces_the_service_name_in_the_zone_this_emulator_answers_in() {
    let lattice = Lattice::new();

    for (index, (case, host, zone)) in addressing_cases().into_iter().enumerate() {
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
        let expected = announced_name(&zone, &name, &service);
        let (domain, hosted_zone) = dns_entry_of(case, &created.body);
        assert_announced(&format!("{case}, CreateService"), &expected, &domain);
        assert_hosted_zone(&format!("{case}, CreateService"), &hosted_zone);

        // GetService: the read path announces the same name, and reports the service's
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

        // ListServices: every summary of the listing announces its own service's name.
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
            let listed_name = item["name"].as_str().unwrap_or_default();
            let listed_id = item["id"].as_str().unwrap_or_default();
            assert_announced(
                &format!("{case}, ListServices"),
                &announced_name(&zone, listed_name, listed_id),
                &listed_domain,
            );
        }

        // CreateServiceNetworkServiceAssociation: the association announces the
        // ASSOCIATED service's name.
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

/// The first label of the announced name is the SERVICE's: its own name and id. The rows
/// span the modelled `ServiceName` (len 3..40, `[a-z0-9-]`) — the shortest name, a
/// hyphenated one, a digit-carrying one and the longest — so no two services of one
/// emulator are announced under one name.
#[tokio::test]
async fn the_announced_name_is_the_services_own_name_and_id() {
    let lattice = Lattice::new();
    // The reached host is one addressing; the name half is what these rows vary.
    let host = "127.0.0.1:39355";
    let zone = "localhost";
    let names: Vec<(&str, String)> = vec![
        ("the shortest modelled name", "abc".to_string()),
        ("a hyphenated name", "my-lattice-service".to_string()),
        ("a name carrying digits", "svc2-payments-7".to_string()),
        ("the longest modelled name (40 characters)", "a".repeat(40)),
    ];

    let mut announced: Vec<String> = Vec::new();
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
        let (domain, _) = dns_entry_of(case, &created.body);
        assert_announced(case, &announced_name(zone, name, &id), &domain);
        assert!(
            domain.starts_with(name.as_str()),
            "case {case}: the first label must name the service {name:?}, got {domain:?}"
        );
        announced.push(domain);
    }

    announced.sort();
    let before = announced.len();
    announced.dedup();
    assert_eq!(
        announced.len(),
        before,
        "no two services may be announced under one name, got {announced:?}"
    );
}

/// One resource, read from several addressings: each caller is announced the service's
/// name in the zone IT reaches this emulator in, and the resource's own hosted zone never
/// moves.
#[tokio::test]
async fn one_service_announces_each_caller_the_zone_that_caller_reaches() {
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

    for (case, host, zone) in &cases {
        let read = lattice
            .call_from_host(host, "GET", &format!("/services/{service}"), None)
            .await;
        assert_eq!(
            read.status, 200,
            "case {case}: GetService expected 200, got {} / {}",
            read.status, read.body
        );
        let (domain, read_zone) = dns_entry_of(case, &read.body);
        assert_announced(case, &announced_name(zone, name, &service), &domain);
        assert_eq!(
            &read_zone, &hosted_zone,
            "case {case}: the service's hosted zone {hosted_zone:?} must not move with \
             the addressing, got {read_zone:?}"
        );
    }
}

/// Reading a service twice from one addressing announces exactly the same name: the value
/// is a function of the stored resource and the request, not of when it was asked for.
#[tokio::test]
async fn the_announced_name_is_stable_across_reads_from_one_addressing() {
    let lattice = Lattice::new();
    for (index, (case, host, zone)) in addressing_cases().into_iter().enumerate() {
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
        let expected = announced_name(&zone, &name, &service);
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
            "case {case}: every read must announce one name, got {seen:?}"
        );
    }
}
