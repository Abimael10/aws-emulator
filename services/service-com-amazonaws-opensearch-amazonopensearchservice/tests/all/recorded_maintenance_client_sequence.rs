// SPDX-License-Identifier: CC0-1.0

//! The SEQUENCE a client that maintains a node of a domain runs, end to end, with nothing
//! but the calls it makes in between.
//!
//! The rule under test, one sentence: **a client that creates a domain, waits for its node
//! census to become readable, starts a maintenance action on a node THAT census published,
//! and then polls that action, is served at every step — the start is accepted (never
//! refused as an unknown node), and the action reaches the terminal status this service
//! reports a finished action with on the polls alone.**
//!
//! That is the whole contract such a client depends on: it never invents a node id, it only
//! ever names one the service itself published, and it drives nothing but
//! `GetDomainMaintenanceStatus` after the start. `crate::domain_maintenance` owns the
//! eligibility table and `crate::maintenance_walk_observability` the walk's intermediate
//! statuses; this suite adds the one SEQUENCE across the operations, over the cluster shapes
//! such a client comes with.
//!
//! The rows differ along what the create asks for — the engine release, whether the domain
//! is zone aware, whether it runs dedicated master nodes and warm nodes, how many data nodes
//! it has, and whether EBS volumes are attached — because none of that bears on the rule:
//! the node the census publishes first is a data node of the domain, and that is the node
//! maintenance accepts.

use serde_json::{json, Value};
use service_com_amazonaws_opensearch_amazonopensearchservice::state::{
    MAINTENANCE_STATUS_SUCCESS, NODE_TYPE_DATA,
};

use super::domain_lifecycle::{poll_until, string_member, Emulator, PREFIX};
use super::domain_maintenance::{list, start, status, MAINTENANCE_TYPES};

/// One client's domain, and the action it maintains a node of it with.
struct Row {
    /// What the row is about.
    case: &'static str,
    /// The `CreateDomain` body, beside the domain name.
    create: fn() -> Value,
    /// The `MaintenanceType` the client asks for.
    action: &'static str,
}

/// The create a client makes for a small single-zone domain with EBS volumes attached — the
/// shape a client that is only trying a maintenance action comes with.
fn plain_create() -> Value {
    json!({
        "EngineVersion": "OpenSearch_2.13",
        "ClusterConfig": {
            "InstanceType": "t3.small.search",
            "InstanceCount": 3,
            "DedicatedMasterEnabled": false,
            "ZoneAwarenessEnabled": false,
        },
        "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
    })
}

/// The same client, on a zone-aware domain whose data nodes are spread over two zones.
fn zone_aware_create() -> Value {
    json!({
        "ClusterConfig": {
            "InstanceType": "r6g.large.search",
            "InstanceCount": 4,
            "DedicatedMasterEnabled": false,
            "ZoneAwarenessEnabled": true,
            "ZoneAwarenessConfig": { "AvailabilityZoneCount": 2 },
        },
        "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 20 },
    })
}

/// The same client, on a domain that also runs dedicated master nodes and warm nodes — so
/// the census publishes nodes of other roles beside the data nodes.
fn every_role_create() -> Value {
    json!({
        "EngineVersion": "OpenSearch_2.19",
        "ClusterConfig": {
            "InstanceType": "r6g.large.search",
            "InstanceCount": 3,
            "DedicatedMasterEnabled": true,
            "DedicatedMasterType": "m5.large.search",
            "DedicatedMasterCount": 3,
            "WarmEnabled": true,
            "WarmType": "ultrawarm1.medium.search",
            "WarmCount": 3,
        },
    })
}

const ROWS: &[Row] = &[
    Row {
        case: "a small single-zone domain on EBS",
        create: plain_create,
        action: MAINTENANCE_TYPES[1],
    },
    Row {
        case: "a zone-aware domain over two zones",
        create: zone_aware_create,
        action: MAINTENANCE_TYPES[0],
    },
    Row {
        case: "a domain running every role",
        create: every_role_create,
        action: MAINTENANCE_TYPES[2],
    },
];

/// The census of `name`, as a client reads it: the whole `DomainNodesStatusList`, polled
/// until the domain publishes one — a domain with work in flight publishes none, which is
/// the condition such a client waits out.
async fn published_census(emulator: &Emulator, name: &str) -> Vec<Value> {
    let case = format!("DescribeDomainNodes({name:?})");
    let uri = format!("{PREFIX}/opensearch/domain/{name}/nodes");
    poll_until(&case, || async {
        let answer = emulator.call("GET", &uri, None).await;
        answer.status == 200
            && answer.body["DomainNodesStatusList"]
                .as_array()
                .is_some_and(|nodes| !nodes.is_empty())
    })
    .await;
    let answer = emulator.call("GET", &uri, None).await;
    answer.body["DomainNodesStatusList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn a_client_that_polls_a_started_maintenance_action_reads_it_finish() {
    let emulator = Emulator::new();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let name = format!("client-sequence-{index}");

        let mut body = (row.create)();
        body["DomainName"] = json!(name.clone());
        let created = emulator.create(body).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateDomain must be accepted, got {} with {:?}",
            created.status, created.body
        );

        // The client waits for the census, then names the FIRST node it published — it has
        // no other source of node ids.
        let census = published_census(&emulator, &name).await;
        let first = census
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: the census must publish a node"));
        let node_id = string_member(first, "NodeId", case);
        assert_eq!(
            first["NodeType"].as_str(),
            Some(NODE_TYPE_DATA),
            "case {case:?}: the first node the census publishes is a data node, got {first:?}"
        );

        let started = start(&emulator, &name, row.action, Some(&node_id)).await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: StartDomainMaintenance on the node the census published must be \
             accepted, got {} with {:?}",
            started.status, started.body
        );
        let maintenance_id = string_member(&started.body, "MaintenanceId", case);

        // And the polls ALONE get the action to its terminal status: the client makes no
        // other call in between.
        let poll_case = format!("GetDomainMaintenanceStatus({case:?})");
        poll_until(&poll_case, || async {
            status(&emulator, &name, &maintenance_id).await.body["Status"].as_str()
                == Some(MAINTENANCE_STATUS_SUCCESS)
        })
        .await;

        let finished = status(&emulator, &name, &maintenance_id).await;
        assert_eq!(
            finished.body["Status"].as_str(),
            Some(MAINTENANCE_STATUS_SUCCESS),
            "case {case:?}: the action must report the terminal status, got {:?}",
            finished.body
        );
        assert_eq!(
            finished.body["Action"].as_str(),
            Some(row.action),
            "case {case:?}: the action must echo what the request asked for, got {:?}",
            finished.body
        );
        assert_eq!(
            finished.body["NodeId"].as_str(),
            Some(node_id.as_str()),
            "case {case:?}: the action must echo the node the request named, got {:?}",
            finished.body
        );

        // The listing carries the same action under the same status, which is the last read
        // such a client takes.
        let listed = list(&emulator, &name, "").await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: ListDomainMaintenances must answer 200, got {} with {:?}",
            listed.status, listed.body
        );
        let row_of_the_action = listed.body["DomainMaintenances"]
            .as_array()
            .and_then(|rows| {
                rows.iter()
                    .find(|row| row["MaintenanceId"].as_str() == Some(maintenance_id.as_str()))
            })
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the listing must carry the started action, got {:?}",
                    listed.body
                )
            });
        assert_eq!(
            row_of_the_action["Status"].as_str(),
            Some(MAINTENANCE_STATUS_SUCCESS),
            "case {case:?}: the listing must carry the terminal status, got \
             {row_of_the_action:?}"
        );
    }
}
