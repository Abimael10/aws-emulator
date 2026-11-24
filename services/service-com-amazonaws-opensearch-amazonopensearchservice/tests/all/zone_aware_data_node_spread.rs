// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusal a zone-aware cluster earns when its data nodes cannot
//! be spread across the Availability Zones it is placed in, on both paths that configure a
//! domain's cluster: `CreateDomain` and `UpdateDomainConfig`.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The harness comes from [`super::domain_lifecycle`];
//! this suite adds no second copy of it.
//!
//! The rule under test: `ClusterConfig$ZoneAwarenessEnabled` "indicates whether multiple
//! Availability Zones are enabled" and `ClusterConfig$InstanceCount` is the "number of data
//! nodes in the cluster" (`smithy_query(query="shape",shape="ClusterConfig")`), with
//! `ZoneAwarenessConfig$AvailabilityZoneCount` publishing "valid values are `2` and `3`" —
//! so a zone-aware cluster runs a WHOLE NUMBER of data nodes per zone, and one that does not
//! is refused with the modelled `ValidationException` (HTTP 400) stating the requirement its
//! count is outside of: the floor (one node per zone) for a cluster running fewer nodes than
//! zones, the step for one running at least one per zone but not a whole number of them, and
//! — for a cluster asking for a standby zone — the requirement that deployment is published
//! with, which states the rule and interpolates nothing.
//!
//! So the assertions span a SET of inputs rather than one: both supported zone counts, named
//! and defaulted, at counts below the floor and off the step, with and without a standby
//! zone. The counts that DO divide are created and read back for each zone count, so the
//! refusal belongs to the spread and not to zone awareness itself, and the update path is
//! held to the same rules with the stored cluster left alone.

use serde_json::{json, Value};

use super::domain_lifecycle::{
    error_code, settled_domain, Answer, Emulator, RECORDED_NOT_FOUND_STATUS,
};

/// The instance type every row provisions its data nodes on — one the service offers, held
/// constant so the only thing varying across rows is the topology under test.
const INSTANCE_TYPE: &str = "m5.large.search";

/// This suite's OWN statement of the sentence a standby deployment's spread requirement is
/// published with: fixed text, with nothing of the request in it. Restated here rather than
/// imported from the service, so changing it in the service fails this suite instead of
/// being asserted against itself.
const STANDBY_MESSAGE: &str = "The number of data nodes must be a multiple of the number of \
                               Availability Zones configured for the domain.";

/// A domain name of the modelled shape (`^[a-z][a-z0-9\-]+$`, 3..28) for one row.
fn domain_name(prefix: &str, row: usize) -> String {
    format!("{prefix}-{row}")
}

/// The clusters whose data-node count the spread rules refuse, each with the sentence the
/// requirement it breaks is stated in.
///
/// Spans both supported zone counts (named, and the smaller one left to the service's
/// default), counts below the floor and counts off the step, and a standby deployment —
/// whose own published requirement states the rule rather than the zone count.
fn refused_clusters() -> Vec<(&'static str, Value, String)> {
    let floor = |zones_word: &str| {
        format!(
            "You must choose a minimum of {zones_word} data nodes for a {zones_word} \
             Availability Zone deployment"
        )
    };
    let step = |requirement: &str, zones_word: &str| {
        format!(
            "You must choose {requirement} data nodes for a {zones_word} Availability Zone \
             deployment"
        )
    };
    let two_zones = |nodes: i64| {
        json!({
            "InstanceType": INSTANCE_TYPE,
            "InstanceCount": nodes,
            "ZoneAwarenessEnabled": true,
            "ZoneAwarenessConfig": { "AvailabilityZoneCount": 2 },
        })
    };
    let three_zones = |nodes: i64| {
        json!({
            "InstanceType": INSTANCE_TYPE,
            "InstanceCount": nodes,
            "ZoneAwarenessEnabled": true,
            "ZoneAwarenessConfig": { "AvailabilityZoneCount": 3 },
        })
    };
    vec![
        (
            "two named zones, one node — below the floor",
            two_zones(1),
            floor("two"),
        ),
        (
            "two named zones, three nodes — off the step",
            two_zones(3),
            step("an even number of", "two"),
        ),
        (
            "two named zones, nine nodes — off the step",
            two_zones(9),
            step("an even number of", "two"),
        ),
        (
            "three zones, one node — below the floor",
            three_zones(1),
            floor("three"),
        ),
        (
            "three zones, two nodes — below the floor",
            three_zones(2),
            floor("three"),
        ),
        (
            "three zones, four nodes — off the step",
            three_zones(4),
            step("a multiple of three", "three"),
        ),
        (
            "three zones, eight nodes — off the step",
            three_zones(8),
            step("a multiple of three", "three"),
        ),
        (
            "zone awareness with no zone count named, one node",
            json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 1,
                "ZoneAwarenessEnabled": true,
            }),
            floor("two"),
        ),
        (
            "zone awareness with no zone count named, seven nodes",
            json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 7,
                "ZoneAwarenessEnabled": true,
            }),
            step("an even number of", "two"),
        ),
        (
            "a standby deployment with a single data node",
            json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 1,
                "ZoneAwarenessEnabled": true,
                "ZoneAwarenessConfig": { "AvailabilityZoneCount": 3 },
                "MultiAZWithStandbyEnabled": true,
            }),
            STANDBY_MESSAGE.to_string(),
        ),
        (
            "a standby deployment with four data nodes",
            json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": 4,
                "ZoneAwarenessEnabled": true,
                "ZoneAwarenessConfig": { "AvailabilityZoneCount": 3 },
                "MultiAZWithStandbyEnabled": true,
            }),
            STANDBY_MESSAGE.to_string(),
        ),
    ]
}

/// The counterparts: the same zone counts at node counts that DO divide by them, a standby
/// deployment at such a count, and a cluster that asks for no zone awareness at a count
/// neither zone count divides — so the refusal belongs to the spread and not to zone
/// awareness or to the count alone.
fn accepted_clusters() -> Vec<(&'static str, Value)> {
    let mut rows: Vec<(&'static str, Value)> = Vec::new();
    for nodes in [2, 4, 6] {
        rows.push((
            "two named zones, a count that divides by two",
            json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": nodes,
                "ZoneAwarenessEnabled": true,
                "ZoneAwarenessConfig": { "AvailabilityZoneCount": 2 },
            }),
        ));
    }
    for nodes in [3, 6, 9] {
        rows.push((
            "three zones, a count that divides by three",
            json!({
                "InstanceType": INSTANCE_TYPE,
                "InstanceCount": nodes,
                "ZoneAwarenessEnabled": true,
                "ZoneAwarenessConfig": { "AvailabilityZoneCount": 3 },
            }),
        ));
    }
    rows.push((
        "a standby deployment at a count that divides by three",
        json!({
            "InstanceType": INSTANCE_TYPE,
            "InstanceCount": 6,
            "ZoneAwarenessEnabled": true,
            "ZoneAwarenessConfig": { "AvailabilityZoneCount": 3 },
            "MultiAZWithStandbyEnabled": true,
        }),
    ));
    rows.push((
        "zone awareness with no zone count named, an even count",
        json!({
            "InstanceType": INSTANCE_TYPE,
            "InstanceCount": 4,
            "ZoneAwarenessEnabled": true,
        }),
    ));
    rows.push((
        "no zone awareness at all, a count no zone count divides",
        json!({ "InstanceType": INSTANCE_TYPE, "InstanceCount": 5 }),
    ));
    rows
}

/// Assert one answer IS the refusal the rule prescribes: the modelled class, the modelled
/// status, and the sentence the broken requirement is stated in.
fn assert_refused(answer: &Answer, case: &str, expected: &str) {
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
        actual, expected,
        "case {case}: expected the sentence stating the broken requirement, got {actual:?}"
    );
    // No sentence of this class carries a figure from the request: the zone count is named
    // in words and the node count the caller sent is not restated at all.
    assert!(
        !actual.chars().any(|ch| ch.is_ascii_digit()),
        "case {case}: expected a sentence interpolating no count, got {actual:?}"
    );
}

#[tokio::test]
async fn a_spread_outside_the_rules_is_refused_on_create() {
    let emulator = Emulator::new();

    for (index, (label, cluster, expected)) in refused_clusters().into_iter().enumerate() {
        let name = domain_name("spread-refused", index);
        let case = format!("CreateDomain({name}, {label})");
        let answer = emulator
            .create(json!({ "DomainName": name, "ClusterConfig": cluster }))
            .await;
        assert_refused(&answer, &case, &expected);

        // A refused create mints nothing: the name is still free of any record.
        let read = emulator.describe(&name).await;
        assert_eq!(
            read.status, RECORDED_NOT_FOUND_STATUS,
            "case {case}: a refused create stores no domain, got {} {:?}",
            read.status, read.body
        );
    }
}

#[tokio::test]
async fn a_spread_that_divides_is_created_and_reads_back() {
    let emulator = Emulator::new();

    for (index, (label, cluster)) in accepted_clusters().into_iter().enumerate() {
        let name = domain_name("spread-served", index);
        let case = format!("CreateDomain({name}, {label})");
        let answer = emulator
            .create(json!({ "DomainName": name, "ClusterConfig": cluster.clone() }))
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected the topology to be provisioned, got {} {:?}",
            answer.status, answer.body
        );

        // Round trip: the node count and the placement read back from the record.
        let read = emulator.describe(&name).await;
        let stored = &read.body["DomainStatus"]["ClusterConfig"];
        for member in [
            "InstanceCount",
            "ZoneAwarenessEnabled",
            "ZoneAwarenessConfig",
            "MultiAZWithStandbyEnabled",
        ] {
            if let Some(sent) = cluster.get(member) {
                assert_eq!(
                    &stored[member], sent,
                    "case {case}: expected the stored cluster to carry {member}, got {stored:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn an_update_is_held_to_the_same_rules_and_keeps_the_stored_cluster() {
    let emulator = Emulator::new();

    for (index, (label, cluster, expected)) in refused_clusters()
        .into_iter()
        // The rows that leave `ZoneAwarenessConfig` out are create-path cases: an update is
        // judged on the MERGED record, which keeps the zone count the domain already
        // carries, so such a row would be judged against that stored count rather than
        // against the service's default. The rows that name their zone count state the
        // whole placement the update would leave behind, and each of the three sentences is
        // represented among them.
        .filter(|(_, cluster, _)| cluster.get("ZoneAwarenessConfig").is_some())
        .enumerate()
    {
        let name = domain_name("spread-update", index);
        let created = emulator
            .create(json!({
                "DomainName": name,
                "ClusterConfig": {
                    "InstanceType": INSTANCE_TYPE,
                    "InstanceCount": 6,
                    "ZoneAwarenessEnabled": true,
                    "ZoneAwarenessConfig": { "AvailabilityZoneCount": 3 },
                },
            }))
            .await;
        assert_eq!(
            created.status, 200,
            "case CreateDomain({name}): expected 200, got {} {:?}",
            created.status, created.body
        );
        // The service applies one change of a domain at a time, so the change the create
        // started has to have landed before an update can start another: the case waits
        // for THAT condition rather than for a fixed stretch of time.
        settled_domain(&emulator, &name).await;

        let case = format!("UpdateDomainConfig({name}, {label})");
        let answer = emulator
            .update_config(&name, json!({ "ClusterConfig": cluster }))
            .await;
        assert_refused(&answer, &case, &expected);

        let stored = emulator.describe_config(&name).await;
        assert_eq!(
            stored.body["DomainConfig"]["ClusterConfig"]["Options"]["InstanceCount"],
            json!(6),
            "case {case}: a refused update leaves the stored cluster alone, got {:?}",
            stored.body["DomainConfig"]["ClusterConfig"]
        );
    }
}
