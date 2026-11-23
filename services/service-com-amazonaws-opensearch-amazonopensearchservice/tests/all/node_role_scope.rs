// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusal a cluster earns for naming an instance type in a
//! tier that type cannot be provisioned in, on both paths that configure a domain's
//! cluster: `CreateDomain` and `UpdateDomainConfig`.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The harness comes from [`super::domain_lifecycle`]
//! and the published roles are read through `ListInstanceTypeDetails` itself; this suite
//! adds no second copy of either.
//!
//! The rule under test: a node is provisioned on an instance type whose published
//! `InstanceTypeDetails$InstanceRole` set carries the ROLE that node serves — the data
//! nodes need the data role, the dedicated master nodes the master role, the warm tier
//! the UltraWarm role and a coordinator node option the coordinator role — so a tier
//! naming a type published for other roles only describes a node the service cannot
//! provision and is refused with the modelled `InvalidTypeException` (`errorKind`
//! client, `httpError` 409, from
//! `smithy_query(query="shape",shape="InvalidTypeException")`), whose message is the
//! fixed text `Invalid instance type: ` followed by the instance type the tier named.
//!
//! So the assertions span SETS of inputs: a table with one row per tier that names an
//! instance type and per published role class the type it names lacks, each row's premise
//! read from the catalog the service itself publishes; the counterpart rows that name a
//! type published for the tier's role, which are created and read back; a refused create
//! that stores nothing; and the update path, which refuses the same tiers and keeps the
//! stored cluster.

use serde_json::{json, Value};

use super::domain_lifecycle::{
    error_code, settled_domain, Answer, Emulator, RECORDED_NOT_FOUND_STATUS,
};
use super::instance_type_catalog::list;

/// The engine release every row is created on: one that offers every family named below,
/// so a refusal is the tier's and never the release's.
const ENGINE_VERSION: &str = "OpenSearch_2.11";

/// One row: what makes it differ from its siblings, the cluster to send, the instance
/// type the tier under test names, the role that tier provisions its nodes in, and where
/// that type sits in a reported `ClusterConfig`.
struct Row {
    case: &'static str,
    cluster: Value,
    instance_type: &'static str,
    role: &'static str,
    reported_at: &'static [&'static str],
}

/// The tiers that cannot be provisioned on the type they name: the data nodes on the
/// warm-tier family, the dedicated master nodes on a data-only family and on the
/// warm-tier family, and a coordinator node option on a family published without the
/// coordinator role — a master-eligible one and a data-only one.
///
/// The warm tier itself cannot break the rule: `ClusterConfig$WarmType` targets
/// `OpenSearchWarmPartitionInstanceType`
/// (`smithy_query(query="shape",shape="OpenSearchWarmPartitionInstanceType")`), whose
/// every value is of the family published for that role, so the request boundary admits
/// no other type there.
fn refused_rows() -> Vec<Row> {
    vec![
        Row {
            case: "the warm-tier family as the data nodes",
            cluster: json!({ "InstanceType": "ultrawarm1.medium.search", "InstanceCount": 2 }),
            instance_type: "ultrawarm1.medium.search",
            role: "data",
            reported_at: &["InstanceType"],
        },
        Row {
            case: "a data-only family as the dedicated master nodes",
            cluster: json!({
                "InstanceType": "m5.large.search",
                "InstanceCount": 2,
                "DedicatedMasterEnabled": true,
                "DedicatedMasterType": "or1.medium.search",
                "DedicatedMasterCount": 3,
            }),
            instance_type: "or1.medium.search",
            role: "master",
            reported_at: &["DedicatedMasterType"],
        },
        Row {
            case: "the warm-tier family as the dedicated master nodes",
            cluster: json!({
                "InstanceType": "m5.large.search",
                "InstanceCount": 2,
                "DedicatedMasterEnabled": true,
                "DedicatedMasterType": "ultrawarm1.large.search",
                "DedicatedMasterCount": 3,
            }),
            instance_type: "ultrawarm1.large.search",
            role: "master",
            reported_at: &["DedicatedMasterType"],
        },
        Row {
            case: "a master-eligible family as a coordinator node option",
            cluster: json!({
                "InstanceType": "m5.large.search",
                "InstanceCount": 2,
                "NodeOptions": [{
                    "NodeType": "coordinator",
                    "NodeConfig": { "Enabled": true, "Type": "t3.small.search", "Count": 2 },
                }],
            }),
            instance_type: "t3.small.search",
            role: "coordinator",
            reported_at: &["NodeOptions", "0", "NodeConfig", "Type"],
        },
        Row {
            case: "a data-only family as a coordinator node option",
            cluster: json!({
                "InstanceType": "m5.large.search",
                "InstanceCount": 2,
                "NodeOptions": [{
                    "NodeType": "coordinator",
                    "NodeConfig": { "Enabled": true, "Type": "or1.large.search", "Count": 2 },
                }],
            }),
            instance_type: "or1.large.search",
            role: "coordinator",
            reported_at: &["NodeOptions", "0", "NodeConfig", "Type"],
        },
    ]
}

/// The counterpart of each row above: the same tier naming a type the catalog publishes
/// for the role that tier provisions. The warm tier rests on dedicated master nodes, so
/// its row carries them.
fn accepted_rows() -> Vec<Row> {
    vec![
        Row {
            case: "a data-role family as the data nodes",
            cluster: json!({ "InstanceType": "or1.medium.search", "InstanceCount": 2 }),
            instance_type: "or1.medium.search",
            role: "data",
            reported_at: &["InstanceType"],
        },
        Row {
            case: "a master-eligible family as the dedicated master nodes",
            cluster: json!({
                "InstanceType": "m5.large.search",
                "InstanceCount": 2,
                "DedicatedMasterEnabled": true,
                "DedicatedMasterType": "t3.small.search",
                "DedicatedMasterCount": 3,
            }),
            instance_type: "t3.small.search",
            role: "master",
            reported_at: &["DedicatedMasterType"],
        },
        Row {
            case: "the warm-tier family as the warm tier",
            cluster: json!({
                "InstanceType": "m5.large.search",
                "InstanceCount": 2,
                "DedicatedMasterEnabled": true,
                "DedicatedMasterType": "m5.large.search",
                "DedicatedMasterCount": 3,
                "WarmEnabled": true,
                "WarmType": "ultrawarm1.medium.search",
                "WarmCount": 2,
            }),
            instance_type: "ultrawarm1.medium.search",
            role: "ultrawarm",
            reported_at: &["WarmType"],
        },
        Row {
            case: "a coordinator-capable family as a coordinator node option",
            cluster: json!({
                "InstanceType": "m5.large.search",
                "InstanceCount": 2,
                "NodeOptions": [{
                    "NodeType": "coordinator",
                    "NodeConfig": { "Enabled": true, "Type": "c5.large.search", "Count": 2 },
                }],
            }),
            instance_type: "c5.large.search",
            role: "coordinator",
            reported_at: &["NodeOptions", "0", "NodeConfig", "Type"],
        },
    ]
}

/// This suite's OWN statement of the refusal text: the fixed sentence, whose single
/// variable part is the instance type the tier named. Restated here rather than imported
/// from the service, so changing it in the service fails this suite instead of being
/// asserted against itself.
fn expected_message(instance_type: &str) -> String {
    format!("Invalid instance type: {instance_type}")
}

/// The roles the published catalog reports for one instance type on [`ENGINE_VERSION`],
/// read through `ListInstanceTypeDetails` — so each row's premise is the service's own
/// published content rather than a second copy of its table.
async fn published_roles(emulator: &Emulator, instance_type: &str) -> Vec<String> {
    let case = format!("the published roles of {instance_type}");
    let answer = list(
        emulator,
        ENGINE_VERSION,
        &format!("instanceType={instance_type}"),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected the catalog entry, got {} {:?}",
        answer.status, answer.body
    );
    answer.body["InstanceTypeDetails"][0]["InstanceRole"]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case {case}: expected an InstanceRole list, got {:?}",
                answer.body
            )
        })
        .iter()
        .map(|role| role.as_str().unwrap_or_default().to_owned())
        .collect()
}

/// A domain name of the modelled shape (`^[a-z][a-z0-9\-]+$`, 3..28) for one row.
fn domain_name(prefix: &str, row: usize) -> String {
    format!("{prefix}-{row}")
}

/// Assert one answer IS the refusal the rule prescribes for `instance_type`.
fn assert_refused(answer: &Answer, instance_type: &str, case: &str) {
    assert_eq!(
        answer.status, 409,
        "case {case}: expected HTTP 409, got {} {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        error_code(answer),
        "InvalidTypeException",
        "case {case}: expected the modelled unsupported-type refusal, got {:?}",
        answer.body
    );
    let actual = answer.body["message"].as_str().unwrap_or_default();
    assert_eq!(
        actual,
        expected_message(instance_type),
        "case {case}: expected the message to name the instance type alone, got {actual:?}"
    );
}

#[tokio::test]
async fn the_published_catalog_agrees_about_every_row() {
    let emulator = Emulator::new();

    for row in refused_rows() {
        let roles = published_roles(&emulator, row.instance_type).await;
        assert!(
            !roles.contains(&row.role.to_string()),
            "case {}: the row's premise is that {} is published for no {} role, but the \
             catalog reports {roles:?}",
            row.case,
            row.instance_type,
            row.role
        );
    }

    for row in accepted_rows() {
        let roles = published_roles(&emulator, row.instance_type).await;
        assert!(
            roles.contains(&row.role.to_string()),
            "case {}: the row's premise is that {} is published for the {} role, but the \
             catalog reports {roles:?}",
            row.case,
            row.instance_type,
            row.role
        );
    }
}

#[tokio::test]
async fn a_tier_on_a_type_published_for_other_roles_is_refused_on_create() {
    let emulator = Emulator::new();

    for (index, row) in refused_rows().into_iter().enumerate() {
        let name = domain_name("role-refused", index);
        let case = format!("CreateDomain({name}, {})", row.case);
        let answer = emulator
            .create(json!({
                "DomainName": name,
                "EngineVersion": ENGINE_VERSION,
                "ClusterConfig": row.cluster,
            }))
            .await;
        assert_refused(&answer, row.instance_type, &case);

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
async fn a_tier_on_a_type_published_for_its_role_is_created_and_reads_back() {
    let emulator = Emulator::new();

    for (index, row) in accepted_rows().into_iter().enumerate() {
        let name = domain_name("role-served", index);
        let case = format!("CreateDomain({name}, {})", row.case);
        let answer = emulator
            .create(json!({
                "DomainName": name,
                "EngineVersion": ENGINE_VERSION,
                "ClusterConfig": row.cluster.clone(),
            }))
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected a tier on a type published for its role to be created, \
             got {} {:?}",
            answer.status, answer.body
        );

        // Round trip: the tier reads back from the record on the type it was created with.
        let read = emulator.describe(&name).await;
        let stored = &read.body["DomainStatus"]["ClusterConfig"];
        let mut reported = stored;
        for step in row.reported_at {
            reported = match step.parse::<usize>() {
                Ok(position) => &reported[position],
                Err(_) => &reported[*step],
            };
        }
        assert_eq!(
            reported,
            &json!(row.instance_type),
            "case {case}: expected the stored cluster to carry {} at {:?}, got {stored:?}",
            row.instance_type,
            row.reported_at
        );
    }
}

#[tokio::test]
async fn an_update_refuses_the_same_tier_and_keeps_the_stored_cluster() {
    let emulator = Emulator::new();

    for (index, row) in refused_rows().into_iter().enumerate() {
        let name = domain_name("role-update", index);
        let created = emulator
            .create(json!({
                "DomainName": name,
                "EngineVersion": ENGINE_VERSION,
                "ClusterConfig": { "InstanceType": "m5.large.search", "InstanceCount": 2 },
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

        // An applied update and a dry run are both refused: the dry run reports what
        // applying the change would do, and this change cannot be applied.
        for dry_run in [false, true] {
            let case = format!("UpdateDomainConfig({name}, {}, dryRun={dry_run})", row.case);
            let answer = emulator
                .update_config(
                    &name,
                    json!({ "ClusterConfig": row.cluster.clone(), "DryRun": dry_run }),
                )
                .await;
            assert_refused(&answer, row.instance_type, &case);

            let stored = emulator.describe_config(&name).await;
            assert_eq!(
                stored.body["DomainConfig"]["ClusterConfig"]["Options"]["InstanceType"],
                json!("m5.large.search"),
                "case {case}: a refused update leaves the stored cluster alone, got {:?}",
                stored.body["DomainConfig"]["ClusterConfig"]
            );
        }
    }
}
