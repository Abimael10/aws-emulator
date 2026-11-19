// SPDX-License-Identifier: CC0-1.0

//! What an ALIAS record set whose target names an ADDRESS is answered with.
//!
//! The rule under test: **an alias record set routes to another resource, and
//! `RecordDataEntry` documents the answer for an alias to an Amazon Web Services
//! resource as "an IP address or a domain name for the … resource" — so a target
//! that names an ADDRESS is answered with that address, and the record data of an
//! `A` record set is an IPv4 address while that of an `AAAA` record set is an
//! IPv6 one.** The port an address is dialled on is no part of a DNS answer, and
//! a query for the other address family is answered with no record data at all
//! rather than with a value that is not of the type asked for.
//!
//! Why an alias target names an address here at all: a data plane this emulator
//! serves is reached at the `host:port` the service that opened it published (a
//! REST custom domain's `regionalDomainName`, a load balancer's `DNSName`), so
//! that `host:port` is what a record set aliasing it carries, and a client that
//! resolves the record has to get an address out of it.
//!
//! The rows span what the rule generalises over: the address family (IPv4,
//! IPv6), whether the target carries a port, the case and the trailing dot it is
//! written in, whether the address is loopback or routable, the queried type
//! against the family of the target, a target that names a HOST rather than an
//! address (the documented domain-name answer, unchanged), and the alias CHAIN
//! that ends at an address.
//!
//! The harness (`router`, `call`, `Reply`) and the change builders come from
//! `hosted_zone_records` and `record_set_validation`, so every call goes through
//! the same router a socket-served request goes through.

use smithy_server_runtime::MultiServiceRouter;

use crate::hosted_zone_records::{call, create_zone_ok, router, Reply};
use crate::record_set_validation::{change, list, submit, Rrset};

/// The zone every row below is queried inside.
const ZONE: &str = "example.com";

/// The hosted zone id an alias to a resource OUTSIDE this store names — the one
/// the publishing service hands a caller for its own endpoint, which is not a
/// zone this account holds. An alias targeting an address is resolved whatever
/// this value is, which is the point of including it.
const FOREIGN_ZONE_ID: &str = "Z2OJLYMUO9EFXC";

/// The response code of a query the zone answers, record data or not.
const NOERROR: &str = "NOERROR";

/// Ask `TestDNSAnswer` for one name and type in `zone_id`.
async fn ask(
    router: &MultiServiceRouter,
    zone_id: &str,
    name: &str,
    rr_type: &str,
) -> Reply {
    call(
        router,
        "GET",
        &format!(
            "/2013-04-01/testdnsanswer?hostedzoneid={zone_id}&recordname={name}\
             &recordtype={rr_type}"
        ),
        "",
    )
    .await
}

/// Seed one record set and assert the batch was accepted.
async fn seed(router: &MultiServiceRouter, zone_id: &str, record: &Rrset) {
    let reply = submit(router, zone_id, &change("CREATE", record)).await;
    assert_eq!(
        reply.status, 200,
        "seeding {record:?}: expected 200, got {} with body {}",
        reply.status, reply.body
    );
}

/// One row: the record set to seed, the query to ask, and the record data the
/// answer must carry.
struct Row {
    case: &'static str,
    /// The name the alias record set stands at.
    name: &'static str,
    /// The record set's own type, which is also the type queried.
    rr_type: &'static str,
    /// The `AliasTarget.DNSName` the record set routes to.
    target: &'static str,
    /// The record data the query must be answered with.
    answer: &'static [&'static str],
}

#[tokio::test]
async fn an_alias_to_an_address_is_answered_with_that_address() {
    let router = router();
    let zone_id = create_zone_ok(&router, ZONE, "alias-address-answers").await;

    let rows: &[Row] = &[
        Row {
            case: "an IPv4 data-plane authority (the form a published endpoint carries)",
            name: "regional.example.com",
            rr_type: "A",
            target: "127.0.0.1:34567",
            answer: &["127.0.0.1"],
        },
        Row {
            case: "an IPv4 address with no port",
            name: "bare.example.com",
            rr_type: "A",
            target: "127.0.0.1",
            answer: &["127.0.0.1"],
        },
        Row {
            case: "a routable IPv4 authority",
            name: "routable.example.com",
            rr_type: "A",
            target: "203.0.113.7:8443",
            answer: &["203.0.113.7"],
        },
        Row {
            case: "an IPv4 authority written with a trailing dot",
            name: "dotted.example.com",
            rr_type: "A",
            target: "198.51.100.12:443.",
            answer: &["198.51.100.12"],
        },
        Row {
            case: "a bracketed IPv6 authority",
            name: "six.example.com",
            rr_type: "AAAA",
            target: "[::1]:34568",
            answer: &["::1"],
        },
        Row {
            case: "an IPv6 address with no port or brackets",
            name: "six-bare.example.com",
            rr_type: "AAAA",
            target: "2001:db8::5",
            answer: &["2001:db8::5"],
        },
        Row {
            case: "an IPv6 authority written in upper case",
            name: "six-upper.example.com",
            rr_type: "AAAA",
            target: "[2001:DB8::A]:9443",
            answer: &["2001:db8::a"],
        },
        Row {
            case: "an A query whose target is an IPv6 address carries no record data",
            name: "mismatched-a.example.com",
            rr_type: "A",
            target: "[::1]:34569",
            answer: &[],
        },
        Row {
            case: "an AAAA query whose target is an IPv4 address carries no record data",
            name: "mismatched-six.example.com",
            rr_type: "AAAA",
            target: "127.0.0.1:34570",
            answer: &[],
        },
        Row {
            // Unchanged by the address reading: `RecordDataEntry`'s other
            // documented answer for an alias to a resource outside this store.
            case: "a target that names a host rather than an address",
            name: "to-a-host.example.com",
            rr_type: "A",
            target: "d111abcdef8.cloudfront.net",
            answer: &["d111abcdef8.cloudfront.net."],
        },
    ];

    for row in rows {
        seed(
            &router,
            &zone_id,
            &Rrset::new(row.name, row.rr_type).alias(FOREIGN_ZONE_ID, row.target),
        )
        .await;
    }

    for row in rows {
        let reply = ask(&router, &zone_id, row.name, row.rr_type).await;
        assert_eq!(
            reply.status, 200,
            "case {:?}: expected 200, got {} with body {}",
            row.case, reply.status, reply.body
        );
        let code = reply.first("ResponseCode").unwrap_or("");
        assert_eq!(
            code, NOERROR,
            "case {:?}: expected {NOERROR}, got {code:?} with body {}",
            row.case, reply.body
        );
        let data = reply.all("RecordData/RecordDataEntry");
        assert_eq!(
            data, row.answer,
            "case {:?}: expected {:?}, got {data:?}",
            row.case, row.answer
        );
    }

    // The answer is composed from the record set, which the query leaves exactly
    // as it was created: the alias target a listing reports still carries the
    // authority — port included — that the publishing service handed the caller,
    // character for character.
    let listed = list(&router, &zone_id).await;
    let targets = listed.all("AliasTarget/DNSName");
    assert!(
        targets.contains(&"127.0.0.1:34567."),
        "expected the stored alias target to keep the published authority, got {targets:?}"
    );
}

#[tokio::test]
async fn an_alias_chain_that_ends_at_an_address_answers_with_that_address() {
    // The other half of the rule: an address ends the alias walk, so a chain of
    // aliases inside the zone that leads to a record set aliasing an address is
    // answered with that address — one value, from the one record set the walk
    // ends at.
    let router = router();
    let zone_id = create_zone_ok(&router, ZONE, "alias-address-chain").await;

    seed(
        &router,
        &zone_id,
        &Rrset::new("endpoint.example.com", "A").alias(FOREIGN_ZONE_ID, "127.0.0.1:41000"),
    )
    .await;
    seed(
        &router,
        &zone_id,
        &Rrset::new("front.example.com", "A").alias(&zone_id, "endpoint.example.com"),
    )
    .await;
    seed(
        &router,
        &zone_id,
        &Rrset::new("apex-ward.example.com", "A").alias(&zone_id, "front.example.com"),
    )
    .await;

    for (case, name) in [
        ("one hop to the address", "endpoint.example.com"),
        ("two hops to the address", "front.example.com"),
        ("three hops to the address", "apex-ward.example.com"),
    ] {
        let reply = ask(&router, &zone_id, name, "A").await;
        let data = reply.all("RecordData/RecordDataEntry");
        assert_eq!(
            data,
            ["127.0.0.1"],
            "case {case:?}: expected the address the chain ends at, got {data:?} with body {}",
            reply.body
        );
    }
}
