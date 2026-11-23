// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the Availability Zones a cluster turned on with a STANDBY zone
//! has to be placed in, on both paths that configure a domain's cluster: `CreateDomain`
//! and `UpdateDomainConfig`.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The harness comes from [`super::domain_lifecycle`];
//! this suite adds no second copy of it.
//!
//! The rule under test: `ClusterConfig$MultiAZWithStandbyEnabled` is "a boolean that
//! indicates whether a multi-AZ domain is turned on with a standby AZ"
//! (`smithy_query(query="shape",shape="ClusterConfig")`), so one of the domain's zones is
//! held in reserve while the others serve traffic — a cluster asking for a standby is
//! placed in the largest number of Availability Zones the service places a cluster in
//! (`ZoneAwarenessConfig$AvailabilityZoneCount` publishes "valid values are `2` and
//! `3`"), and one asking for a standby with fewer zones, or with no zone awareness at
//! all, describes a topology the service does not provision and is refused with the
//! modelled `ValidationException` (HTTP 400).
//!
//! So the assertions span SETS of inputs: a table of clusters asking for a standby
//! without the zones it takes — no zone awareness at two data-node counts, zone awareness
//! turned off explicitly, and the smaller supported zone count — each refused and storing
//! nothing; the counterpart clusters, which are created and read back, including every
//! data-node count the three-zone spread admits in the range the service provisions; a
//! cluster that asks for no standby, which is placed in any supported topology; and the
//! update path, which refuses the same change and keeps the stored cluster.

use serde_json::{json, Value};

use super::domain_lifecycle::{
    error_code, settled_domain, Answer, Emulator, RECORDED_NOT_FOUND_STATUS,
};

/// This suite's OWN statement of the refusal text: the fixed sentence naming the zone
/// count a standby takes. Restated here rather than imported from the service, so
/// changing it in the service fails this suite instead of being asserted against itself.
const EXPECTED_MESSAGE: &str =
    "You must enable zone awareness with 3 Availability Zones to use Multi-AZ with Standby.";

/// A domain name of the modelled shape (`^[a-z][a-z0-9\-]+$`, 3..28) for one row.
fn domain_name(prefix: &str, row: usize) -> String {
    format!("{prefix}-{row}")
}

/// The clusters that ask for a standby zone without the zones one takes: no zone
/// awareness at the smallest data-node count, no zone awareness at a count that would
/// spread across three zones, zone awareness turned off explicitly, and zone awareness
/// across the smaller supported count.
fn refused_clusters() -> Vec<(&'static str, Value)> {
    vec![
        (
            "a standby with a single data node and no zone awareness",
            json!({
                "InstanceType": "m5.large.search",
                "InstanceCount": 1,
                "MultiAZWithStandbyEnabled": true,
            }),
        ),
        (
            "a standby with three data nodes and no zone awareness",
            json!({
                "InstanceType": "m5.large.search",
                "InstanceCount": 3,
                "MultiAZWithStandbyEnabled": true,
            }),
        ),
        (
            "a standby with zone awareness turned off explicitly",
            json!({
                "InstanceType": "m5.large.search",
                "InstanceCount": 6,
                "ZoneAwarenessEnabled": false,
                "MultiAZWithStandbyEnabled": true,
            }),
        ),
        (
            "a standby across the smaller supported zone count",
            json!({
                "InstanceType": "m5.large.search",
                "InstanceCount": 4,
                "ZoneAwarenessEnabled": true,
                "ZoneAwarenessConfig": { "AvailabilityZoneCount": 2 },
                "MultiAZWithStandbyEnabled": true,
            }),
        ),
    ]
}

/// The counterparts: a standby across the zone count it takes, at each data-node count
/// that spread admits, and a cluster that asks for no standby in a topology the standby
/// rule would refuse — so the refusal is the standby's and not the topology's.
fn accepted_clusters() -> Vec<(&'static str, Value)> {
    let mut rows: Vec<(&'static str, Value)> = Vec::new();
    for nodes in [3, 6, 9] {
        rows.push((
            "a standby across the zone count it takes",
            json!({
                "InstanceType": "m5.large.search",
                "InstanceCount": nodes,
                "ZoneAwarenessEnabled": true,
                "ZoneAwarenessConfig": { "AvailabilityZoneCount": 3 },
                "MultiAZWithStandbyEnabled": true,
            }),
        ));
    }
    rows.push((
        "no standby asked for, in a single-zone topology",
        json!({ "InstanceType": "m5.large.search", "InstanceCount": 1 }),
    ));
    rows.push((
        "a standby asked for explicitly off, in a single-zone topology",
        json!({
            "InstanceType": "m5.large.search",
            "InstanceCount": 2,
            "MultiAZWithStandbyEnabled": false,
        }),
    ));
    rows
}

/// Assert one answer IS the refusal the rule prescribes.
fn assert_refused(answer: &Answer, case: &str) {
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
        actual, EXPECTED_MESSAGE,
        "case {case}: expected the sentence naming the zone count a standby takes, got \
         {actual:?}"
    );
}

#[tokio::test]
async fn a_standby_without_the_zones_it_takes_is_refused_on_create() {
    let emulator = Emulator::new();

    for (index, (label, cluster)) in refused_clusters().into_iter().enumerate() {
        let name = domain_name("standby-refused", index);
        let case = format!("CreateDomain({name}, {label})");
        let answer = emulator
            .create(json!({ "DomainName": name, "ClusterConfig": cluster }))
            .await;
        assert_refused(&answer, &case);

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
async fn a_standby_across_the_zones_it_takes_is_created_and_reads_back() {
    let emulator = Emulator::new();

    for (index, (label, cluster)) in accepted_clusters().into_iter().enumerate() {
        let name = domain_name("standby-served", index);
        let case = format!("CreateDomain({name}, {label})");
        let answer = emulator
            .create(json!({ "DomainName": name, "ClusterConfig": cluster.clone() }))
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected the topology to be provisioned, got {} {:?}",
            answer.status, answer.body
        );

        // Round trip: the standby setting and the node count read back from the record.
        let read = emulator.describe(&name).await;
        let stored = &read.body["DomainStatus"]["ClusterConfig"];
        for member in ["MultiAZWithStandbyEnabled", "InstanceCount"] {
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
async fn an_update_refuses_the_same_change_and_keeps_the_stored_cluster() {
    let emulator = Emulator::new();

    for (index, (label, cluster)) in refused_clusters().into_iter().enumerate() {
        let name = domain_name("standby-update", index);
        let created = emulator
            .create(json!({
                "DomainName": name,
                "ClusterConfig": {
                    "InstanceType": "m5.large.search",
                    "InstanceCount": 6,
                    "ZoneAwarenessEnabled": true,
                    "ZoneAwarenessConfig": { "AvailabilityZoneCount": 3 },
                    "MultiAZWithStandbyEnabled": true,
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
        assert_refused(&answer, &case);

        let stored = emulator.describe_config(&name).await;
        assert_eq!(
            stored.body["DomainConfig"]["ClusterConfig"]["Options"]["InstanceCount"],
            json!(6),
            "case {case}: a refused update leaves the stored cluster alone, got {:?}",
            stored.body["DomainConfig"]["ClusterConfig"]
        );
    }
}
