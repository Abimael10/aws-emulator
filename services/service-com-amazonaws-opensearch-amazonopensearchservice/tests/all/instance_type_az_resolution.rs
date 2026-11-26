// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what happens when `ListInstanceTypeDetails` is asked to resolve a
//! NAMED instance type against Availability Zones.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The harness comes from [`super::domain_lifecycle`]
//! and the listing helper from [`super::instance_type_catalog`], which owns this
//! operation's suite; this one adds no second copy of either.
//!
//! The rule under test: `RetrieveAZs` is "an optional parameter that specifies the
//! Availability Zones for the domain" and `InstanceType` "an optional parameter that lists
//! information for a given instance type"
//! (`smithy_query(query="operation",operation="ListInstanceTypeDetails")`), whose shape
//! `InstanceTypeString` is constrained only by `@length(10..40)` and
//! `@pattern("^.*\\..*\\.search$")` — the catalog membership of a value is not a boundary
//! constraint. So a read that asks for the zones of a named instance type asks the service
//! to resolve that type against the Region's zones, and a value no published instance type
//! answers to is offered by no zone: the read is refused with the modelled
//! `ValidationException` (HTTP 400) and the service's sentence for a zone that does not
//! support the type, which echoes the value exactly as the request carried it. A read that
//! does NOT ask for zone information resolves nothing against a zone, so there the same
//! value is not graded at all: it only narrows what is reported, and the listing it names
//! no entry of is empty.
//!
//! The assertions therefore span SETS of inputs: a table of values the catalog publishes no
//! instance type for — a family it does not carry, a size a family is not published in, a
//! size no family is published in, a value that is not an instance type at all, and a
//! published type spelled in another case — each on releases of both engines, scoped to a
//! domain and unscoped, and beside the other optional members; the same values with the
//! flag absent and sent false, which are answered; and the published types, which are
//! answered with their zones.

use serde_json::json;

use super::domain_lifecycle::{error_code, Answer, Emulator};
use super::instance_type_catalog::list;

/// The engine releases every table below spans: a release of each engine line, and a
/// current one — the catalog a read is taken for differs per release, and this refusal does
/// not.
const VERSIONS: [&str; 3] = ["Elasticsearch_7.10", "OpenSearch_2.11", "OpenSearch_2.19"];

/// Values of `InstanceTypeString`'s own shape (10..40 characters, `<family>.<size>.search`)
/// that the published catalog carries no instance type for, so no Availability Zone offers
/// one: a family the catalog does not carry, a size the family is not published in, a size
/// no family is published in, a value that is not an instance type at all, and a published
/// type spelled in another case.
const NO_ZONE_OFFERS: [&str; 6] = [
    "nosuch.large.search",
    "m5.nosuch.search",
    "m7i.48xlarge.search",
    "r6g.large.search.search",
    "T3.SMALL.search",
    "not.a.real.search",
];

/// Instance types the catalog publishes, one per table row below: a general-purpose type,
/// a burstable one and the warm tier's. Source: the catalog `ListInstanceTypeDetails`
/// serves, read back through the operation itself by
/// [`the_zones_of_a_published_instance_type_are_reported`].
const PUBLISHED: [&str; 3] = [
    "m5.large.search",
    "t3.small.search",
    "ultrawarm1.medium.search",
];

/// The query members a read is probed beside, so the rule is not about one shape of
/// request: none, a page size, and a paging token.
const BESIDE: [&str; 3] = ["", "maxResults=5", "nextToken=m5.large.search"];

/// This suite's OWN statement of the refusal: the fixed text, whose one variable part is
/// the instance type, appended immediately after the colon. Restated here rather than
/// imported from the service, so changing it in the service fails this suite instead of
/// being asserted against itself.
fn expected_message(instance_type: &str) -> String {
    format!("The AZ doesn't support instance type :{instance_type}")
}

/// Assert that `answer` is the modelled zone refusal for `instance_type` and carries no
/// listing.
fn assert_zone_refusal(case: &str, answer: &Answer, instance_type: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case}: expected HTTP 400, got {} {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        error_code(answer),
        "ValidationException",
        "case {case}: expected the modelled validation refusal, got {:?}",
        answer.body
    );
    let actual = answer.body["message"].as_str().unwrap_or_default();
    assert_eq!(
        actual,
        expected_message(instance_type),
        "case {case}: expected the sentence for a zone that does not support the type, got \
         {actual:?}"
    );
    assert!(
        answer.body["InstanceTypeDetails"].is_null(),
        "case {case}: a refused read carries no listing, got {:?}",
        answer.body["InstanceTypeDetails"]
    );
}

/// The `InstanceTypeDetails` list of an answered read, with the case named on failure.
fn listed(case: &str, answer: &Answer) -> Vec<serde_json::Value> {
    assert_eq!(
        answer.status, 200,
        "case {case}: expected HTTP 200, got {} {:?}",
        answer.status, answer.body
    );
    answer.body["InstanceTypeDetails"]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case {case}: expected an InstanceTypeDetails array, got {:?}",
                answer.body
            )
        })
        .clone()
}

#[tokio::test]
async fn an_instance_type_no_zone_offers_is_refused_when_the_zones_are_asked_for() {
    let emulator = Emulator::new();

    for version in VERSIONS {
        for instance_type in NO_ZONE_OFFERS {
            for beside in BESIDE {
                let query = [
                    format!("instanceType={instance_type}"),
                    "retrieveAZs=true".to_owned(),
                ]
                .into_iter()
                .chain(
                    Some(beside.to_owned())
                        .into_iter()
                        .filter(|part| !part.is_empty()),
                )
                .collect::<Vec<String>>()
                .join("&");
                let case = format!("{version} ?{query}");
                let answer = list(&emulator, version, &query).await;
                assert_zone_refusal(&case, &answer, instance_type);
            }
        }
    }
}

#[tokio::test]
async fn a_domain_scoped_read_is_refused_the_same_way() {
    let emulator = Emulator::new();
    let held = "az-resolution-domain";
    let created = emulator.create(json!({ "DomainName": held })).await;
    assert_eq!(
        created.status, 200,
        "case create {held}: expected HTTP 200, got {} {:?}",
        created.status, created.body
    );

    // The zones a scoped read reports are the domain's Region's, so the type is resolved
    // against those: the refusal is the same sentence, scoped or unscoped.
    for instance_type in NO_ZONE_OFFERS {
        let query = format!("domainName={held}&retrieveAZs=true&instanceType={instance_type}");
        let case = format!("OpenSearch_2.11 ?{query}");
        let answer = list(&emulator, "OpenSearch_2.11", &query).await;
        assert_zone_refusal(&case, &answer, instance_type);
    }
}

#[tokio::test]
async fn the_same_value_is_not_graded_when_no_zone_information_is_asked_for() {
    let emulator = Emulator::new();

    // The flag absent, and sent false: neither read resolves anything against a zone, so
    // the value is not graded. It still decides what is REPORTED, and it names no entry of
    // the listing, so the answer is an empty listing rather than a refusal.
    for version in VERSIONS {
        for instance_type in NO_ZONE_OFFERS {
            for flag in ["", "retrieveAZs=false"] {
                let query = [format!("instanceType={instance_type}"), flag.to_owned()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<String>>()
                    .join("&");
                let case = format!("{version} ?{query}");
                let answer = list(&emulator, version, &query).await;
                let entries = listed(&case, &answer);
                assert!(
                    entries.is_empty(),
                    "case {case}: expected an empty listing, got {entries:?}"
                );
                assert!(
                    answer.body["NextToken"].is_null(),
                    "case {case}: an empty listing carries no NextToken, got {:?}",
                    answer.body["NextToken"]
                );
            }
        }
    }
}

#[tokio::test]
async fn the_zones_of_a_published_instance_type_are_reported() {
    let emulator = Emulator::new();

    // The control: a type the catalog publishes is offered by the Region's zones, so a
    // read asking for its zones is answered with them rather than refused.
    for instance_type in PUBLISHED {
        let query = format!("instanceType={instance_type}&retrieveAZs=true");
        let case = format!("OpenSearch_2.11 ?{query}");
        let answer = list(&emulator, "OpenSearch_2.11", &query).await;
        let entries = listed(&case, &answer);
        assert_eq!(
            entries.len(),
            1,
            "case {case}: the filter names one instance type, got {entries:?}"
        );
        assert_eq!(
            entries[0]["InstanceType"].as_str(),
            Some(instance_type),
            "case {case}: expected the entry the filter names, got {:?}",
            entries[0]
        );
        let zones = entries[0]["AvailabilityZones"]
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: expected the AZ information the read asked for, got {:?}",
                    entries[0]
                )
            });
        assert!(
            !zones.is_empty(),
            "case {case}: expected the Region's zones, got {zones:?}"
        );
    }
}

#[tokio::test]
async fn a_published_type_the_release_does_not_offer_is_answered_rather_than_refused() {
    let emulator = Emulator::new();

    // A type the catalog PUBLISHES is hardware the Region's zones have, whichever release
    // the read is taken for, so asking for its zones on a release that does not offer it is
    // not the zone refusal: the listing it narrows simply reports no entry.
    for (instance_type, version) in [
        ("t2.micro.search", "OpenSearch_2.11"),
        ("or1.medium.search", "Elasticsearch_7.10"),
        ("c8g.8xlarge.search", "OpenSearch_2.11"),
    ] {
        let query = format!("instanceType={instance_type}&retrieveAZs=true");
        let case = format!("{version} ?{query}");
        let answer = list(&emulator, version, &query).await;
        let entries = listed(&case, &answer);
        assert!(
            entries.is_empty(),
            "case {case}: expected an empty listing for a release that does not offer the \
             type, got {entries:?}"
        );
    }
}
