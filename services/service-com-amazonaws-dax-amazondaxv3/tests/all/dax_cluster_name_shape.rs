// SPDX-License-Identifier: CC0-1.0

//! What a `ClusterName` is read for, and what it is NOT read for.
//!
//! `CreateCluster$ClusterName` targets the model's unconstrained `String`
//! (`smithy_query(query="operation",operation="CreateCluster")` reports it as `String`,
//! and `smithy_query(query="shape",shape="String")` reports that shape carrying no
//! `@length`), so the only thing a name is judged by is its SHAPE: letters, digits and
//! hyphens, a leading letter, no trailing hyphen and no two consecutive hyphens. Its
//! LENGTH is not a rule — DAX serves a name of any length — which is what lets a client
//! address a cluster by a prefixed unique identifier, reach `ClusterAlreadyExistsFault`
//! on a duplicate of it, and run the rest of the cluster surface against it.
//!
//! Every case drives `POST /` with `X-Amz-Target: AmazonDAXV3.<Operation>` through the
//! same `MultiServiceRouter` the production router builds, so what is asserted is the
//! HTTP behaviour a DAX SDK client sees. The tables below span the CLASS each rule
//! covers; a name of any one length is a single row.

use serde_json::{json, Value};
use service_com_amazonaws_dax_amazondaxv3::state;

use crate::dax_account_network::{
    call, cluster_in_status, emulator, error_code, error_message, REGION,
};

/// The canonical emulator account id every ARN carries.
const ACCOUNT: &str = "000000000000";

/// A role ARN to pass as the required `IamRoleArn`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/DAXRole";

/// A node type to pass as the required `NodeType`.
const NODE_TYPE: &str = "dax.t3.small";

/// The status a client fault answers with on this service's JSON protocol.
const CLIENT_FAULT_STATUS: u16 = 400;

/// A `CreateCluster` body with the required members filled in.
fn create_cluster_body(name: &str, replication_factor: i64) -> Value {
    json!({
        "ClusterName": name,
        "NodeType": NODE_TYPE,
        "ReplicationFactor": replication_factor,
        "IamRoleArn": ROLE_ARN,
    })
}

/// A name of `length` characters that satisfies every clause of the shape rule: a leading
/// letter, then letters with a single hyphen every fourth position and never one last.
fn well_shaped_name_of_length(length: usize) -> String {
    (0..length)
        .map(|index| {
            if index > 0 && index % 4 == 0 && index + 1 < length {
                '-'
            } else if index == 0 {
                'c'
            } else {
                'd'
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Rule: `ClusterName` carries no length constraint, so a well-shaped name of any length
// up to what this emulator stores is accepted: the cluster is stored under its
// lower-cased name, and its ARN, discovery endpoint and nodes are derived from that name.
// Source: `CreateCluster$ClusterName` targets `String` with no `@length`
// (`smithy_query(query="operation",operation="CreateCluster")`,
// `smithy_query(query="shape",shape="String")`); the member's own documentation prose
// mentions "from 1 to 20" characters, which the wire does not enforce. The only length
// that is refused is past `state::MAX_STORED_CLUSTER_NAME_LEN`, the emulator's own bound
// on what one request may make it store.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_well_shaped_cluster_name_of_any_length_is_created_and_reads_back() {
    // Rows differ along the one dimension the rule generalises over — LENGTH — spanning
    // the shortest name, the length the member's prose happens to mention, one past it,
    // the prefixed unique names a client generates, a far longer one and the emulator's
    // stored bound itself. The last two rows vary the form instead: upper case (stored
    // lower-cased) and digits.
    let names = [
        well_shaped_name_of_length(1),
        well_shaped_name_of_length(20),
        well_shaped_name_of_length(21),
        "sim2real-dax-cl-0123456789ab".to_string(),
        "sim2real-dax-race-0123456789ab".to_string(),
        well_shaped_name_of_length(128),
        well_shaped_name_of_length(state::MAX_STORED_CLUSTER_NAME_LEN),
        "SIM2REAL-DAX-UP-0123456789AB".to_string(),
        format!("c{}", "9".repeat(40)),
    ];

    let router = emulator();
    for name in &names {
        let (status, created) = call(&router, "CreateCluster", create_cluster_body(name, 1)).await;
        assert_eq!(
            status, 200,
            "case {name:?} ({} characters): a name the model constrains in no way must be \
             accepted, got {status} {created:?}",
            name.chars().count()
        );
        let cluster = &created["Cluster"];
        let stored = name.to_lowercase();
        assert_eq!(
            cluster["ClusterName"],
            json!(stored),
            "case {name:?}: the name is stored lower-cased, got {cluster:?}"
        );
        assert_eq!(
            cluster["ClusterArn"],
            json!(format!("arn:aws:dax:{REGION}:{ACCOUNT}:cache/{stored}")),
            "case {name:?}: the ARN must be derived from the stored name, got {cluster:?}"
        );
        assert_eq!(
            cluster["ClusterDiscoveryEndpoint"]["Address"],
            json!(format!("{stored}.dax-clusters.{REGION}.amazonaws.com")),
            "case {name:?}: the discovery endpoint must be derived from the stored name, \
             got {cluster:?}"
        );
        assert_eq!(
            cluster["Status"],
            json!(state::CLUSTER_CREATING),
            "case {name:?}: a freshly created cluster is not yet available, got {cluster:?}"
        );

        // Round trip: the clock settles it, and the stored cluster reads back under the
        // same name with its node.
        let read = cluster_in_status(&router, &stored, state::CLUSTER_AVAILABLE).await;
        assert_eq!(
            read["ClusterArn"], cluster["ClusterArn"],
            "case {name:?}: the describe must report the stored ARN, got {read:?}"
        );
        assert_eq!(
            read["TotalNodes"],
            json!(1),
            "case {name:?}: one node per replication factor, got {read:?}"
        );
        assert_eq!(
            read["Nodes"][0]["NodeId"],
            json!(format!("{stored}-a")),
            "case {name:?}: a node is identified by its cluster and a letter, got {read:?}"
        );
        assert_eq!(
            read["Nodes"][0]["Endpoint"]["Address"],
            json!(format!(
                "{}.{stored}.dax-clusters.{REGION}.amazonaws.com",
                read["Nodes"][0]["NodeId"].as_str().unwrap_or_default()
            )),
            "case {name:?}: a node endpoint must be derived from the stored name, got {read:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule: a `ClusterName` breaking a clause of the shape rule — a character outside letters,
// digits and hyphens, a first character that is not a letter, a trailing hyphen, or two
// consecutive hyphens — is refused with `InvalidParameterValueException` (HTTP 400),
// whatever its length, and stores nothing; so is a name longer than the emulator stores.
// Source: `CreateCluster$ClusterName` ("The first character must be a letter. A name
// cannot end with a hyphen or contain two consecutive hyphens.") and
// `state::MAX_STORED_CLUSTER_NAME_LEN` for the one length that is refused.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_cluster_name_breaking_a_shape_clause_is_refused_at_every_length() {
    // Rows: each clause of the rule, broken once in a SHORT name and once in a long one,
    // so what the refusal rests on is the shape rather than the size. The empty name the
    // model's `@required` admits begins with no letter, so it breaks the first-character
    // clause too; the last row is well shaped but one character past what the emulator
    // stores.
    let long = well_shaped_name_of_length(32);
    let cases = [
        ("a leading digit", "1abc".to_string()),
        ("a leading digit, long", format!("1{long}")),
        ("a leading hyphen", "-abc".to_string()),
        ("a leading hyphen, long", format!("-{long}")),
        ("a trailing hyphen", "abc-".to_string()),
        ("a trailing hyphen, long", format!("{long}-")),
        ("two consecutive hyphens", "a--b".to_string()),
        ("two consecutive hyphens, long", format!("{long}--x")),
        ("an underscore", "ab_c".to_string()),
        ("an underscore, long", format!("{long}_x")),
        ("a dot", "ab.c".to_string()),
        ("a dot, long", format!("{long}.x")),
        ("the empty name", String::new()),
        (
            "one character past what is stored",
            well_shaped_name_of_length(state::MAX_STORED_CLUSTER_NAME_LEN + 1),
        ),
    ];

    let router = emulator();
    for (case, name) in &cases {
        let (status, answered) = call(&router, "CreateCluster", create_cluster_body(name, 1)).await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {case:?} ({name:?}): expected a refusal, got {status} {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "InvalidParameterValueException",
            "case {case:?} ({name:?}): expected InvalidParameterValueException, \
             got {answered:?}"
        );
        // The message names the member and the rule that was broken; the stale "from 1 to
        // 20" wording is gone, since that prose is not a rule the service enforces.
        let message = error_message(&answered);
        assert!(
            message.starts_with("ClusterName") && !message.contains("1 to"),
            "case {case:?} ({name:?}): the refusal must state the ClusterName rule that was \
             broken, got {message:?}"
        );
        // Nothing was stored under it.
        let (status, read) = call(
            &router,
            "DescribeClusters",
            json!({ "ClusterNames": [name.to_lowercase()] }),
        )
        .await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {case:?} ({name:?}): a refused create leaves nothing behind, \
             got {status} {read:?}"
        );
        assert_eq!(
            error_code(&read),
            "ClusterNotFoundFault",
            "case {case:?} ({name:?}): expected ClusterNotFoundFault, got {read:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule: a name is taken by the cluster stored under it, so a second create of a name the
// account already holds is refused with `ClusterAlreadyExistsFault` (HTTP 400) — at any
// length, and whatever casing either request used — and the stored cluster is untouched.
// Source: the modelled `ClusterAlreadyExistsFault` ("You already have a DAX cluster with
// the given identifier") and `CreateCluster$ClusterName` ("This parameter is stored as a
// lowercase string").
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_duplicate_cluster_name_is_refused_as_already_existing() {
    // Rows: the name created, then the name the duplicate create asks for — the same
    // string, the same string in another casing, and a long name in both forms.
    let long = "sim2real-dax-cl-0123456789ab";
    let cases = [
        ("short, verbatim", "demo".to_string(), "demo".to_string()),
        ("short, re-cased", "other".to_string(), "OTHER".to_string()),
        ("long, verbatim", long.to_string(), long.to_string()),
        ("long, re-cased", format!("x{long}"), format!("X{}", long.to_uppercase())),
    ];

    let router = emulator();
    for (case, created_name, duplicate) in &cases {
        let (status, created) =
            call(&router, "CreateCluster", create_cluster_body(created_name, 1)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateCluster {created_name:?} got {status} {created:?}"
        );

        let (status, answered) =
            call(&router, "CreateCluster", create_cluster_body(duplicate, 3)).await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {case:?}: a duplicate of {created_name:?} must be refused, \
             got {status} {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "ClusterAlreadyExistsFault",
            "case {case:?}: expected ClusterAlreadyExistsFault, got {answered:?}"
        );
        assert_eq!(
            error_message(&answered),
            "Cluster with specified name already exists.",
            "case {case:?}: the fault's recorded wording names no identifier, got {answered:?}"
        );

        // The refused duplicate changed nothing: the stored cluster keeps the node count
        // the create that won asked for.
        let read = cluster_in_status(&router, &created_name.to_lowercase(), state::CLUSTER_AVAILABLE)
            .await;
        assert_eq!(
            read["TotalNodes"],
            json!(1),
            "case {case:?}: the refused duplicate must not resize the stored cluster, \
             got {read:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule: the name is claimed under the store's lock, so concurrent creates of ONE name
// have exactly one winner and every other racer is answered
// `ClusterAlreadyExistsFault`; one cluster is stored.
// Source: the same modelled fault, and `DescribeClusters` as the account's own record of
// what exists.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_creates_of_one_cluster_name_have_exactly_one_winner() {
    // Rows differ in the name the racers contend for: a short one, and the prefixed
    // unique name of the length a client generates.
    for name in ["racer", "sim2real-dax-race-0123456789ab"] {
        let router = emulator();
        let body = || create_cluster_body(name, 1);
        let answers = ::tokio::join!(
            call(&router, "CreateCluster", body()),
            call(&router, "CreateCluster", body()),
            call(&router, "CreateCluster", body()),
            call(&router, "CreateCluster", body()),
            call(&router, "CreateCluster", body()),
        );
        let answers = [
            answers.0, answers.1, answers.2, answers.3, answers.4,
        ];

        let winners: Vec<&(u16, Value)> = answers
            .iter()
            .filter(|(status, _)| *status == 200)
            .collect();
        assert_eq!(
            winners.len(),
            1,
            "case {name:?}: expected exactly one winner, got {answers:?}"
        );
        for (status, body) in answers.iter().filter(|(status, _)| *status != 200) {
            assert_eq!(
                *status, CLIENT_FAULT_STATUS,
                "case {name:?}: a losing racer is answered a client fault, \
                 got {status} {body:?}"
            );
            assert_eq!(
                error_code(body),
                "ClusterAlreadyExistsFault",
                "case {name:?}: a losing racer must see the duplicate-name fault, got {body:?}"
            );
        }

        // One cluster is stored, under the contended name.
        let (status, described) = call(&router, "DescribeClusters", json!({})).await;
        assert_eq!(status, 200, "case {name:?}: DescribeClusters got {described:?}");
        assert_eq!(
            described["Clusters"].as_array().map(Vec::len),
            Some(1),
            "case {name:?}: exactly one cluster must exist, got {described:?}"
        );
        assert_eq!(
            described["Clusters"][0]["ClusterName"],
            json!(name),
            "case {name:?}: the stored cluster carries the contended name, got {described:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule: every operation that addresses a cluster BY NAME addresses it by the name it was
// created under, so the whole cluster surface is reachable for a name of any length.
// Source: `UpdateCluster$ClusterName`, `IncreaseReplicationFactor$ClusterName` and
// `DeleteCluster$ClusterName`, each the identifier of the stored cluster.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_cluster_surface_is_reachable_for_a_name_of_any_length() {
    // Rows: a short name, and the prefixed unique name whose length the member's prose
    // would have refused.
    for name in ["demo", "sim2real-dax-cl-0123456789ab"] {
        let router = emulator();
        let (status, created) = call(&router, "CreateCluster", create_cluster_body(name, 1)).await;
        assert_eq!(
            status, 200,
            "case {name:?}: CreateCluster got {status} {created:?}"
        );
        cluster_in_status(&router, name, state::CLUSTER_AVAILABLE).await;

        let (status, updated) = call(
            &router,
            "UpdateCluster",
            json!({ "ClusterName": name, "Description": "updated" }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: UpdateCluster got {status} {updated:?}"
        );
        let read = cluster_in_status(&router, name, state::CLUSTER_AVAILABLE).await;
        assert_eq!(
            read["Description"],
            json!("updated"),
            "case {name:?}: the new description must be persisted, got {read:?}"
        );

        let (status, grown) = call(
            &router,
            "IncreaseReplicationFactor",
            json!({ "ClusterName": name, "NewReplicationFactor": 3 }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: IncreaseReplicationFactor got {status} {grown:?}"
        );
        let read = cluster_in_status(&router, name, state::CLUSTER_AVAILABLE).await;
        assert_eq!(
            read["TotalNodes"],
            json!(3),
            "case {name:?}: the grown cluster must report its new node count, got {read:?}"
        );
        let node_ids: Vec<String> = read["Nodes"]
            .as_array()
            .map(|nodes| {
                nodes
                    .iter()
                    .map(|node| node["NodeId"].as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            node_ids,
            vec![format!("{name}-a"), format!("{name}-b"), format!("{name}-c")],
            "case {name:?}: every node is identified by the cluster and a letter, got {read:?}"
        );

        let (status, deleted) =
            call(&router, "DeleteCluster", json!({ "ClusterName": name })).await;
        assert_eq!(
            status, 200,
            "case {name:?}: DeleteCluster got {status} {deleted:?}"
        );
        assert_eq!(
            deleted["Cluster"]["Status"],
            json!(state::CLUSTER_DELETING),
            "case {name:?}: a deleted cluster is reported deleting, got {deleted:?}"
        );
    }
}
