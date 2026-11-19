// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a MODIFICATION of a DB cluster answers with: the cluster as
//! the store holds it, including the members it reports from the records AROUND it.
//!
//! The rule under test: a cluster's `CustomEndpoints` and `DBClusterMembers` are not stored
//! on the cluster record — they are read, when the cluster is projected, off the custom
//! endpoint and DB instance records that NAME it ("the custom endpoints associated with the
//! DB cluster", "the list of DB instances that make up the DB cluster"). A projection
//! therefore has to be taken against a store that HOLDS the cluster: `ModifyDBCluster`
//! takes the record out to write the modification onto it, so a cluster projected while its
//! own record is out of the store resolves to nothing and reports an empty list for every
//! member read that way. The modification's own envelope must carry exactly what the
//! following read carries — anything else is a loss of state the emulator itself created.
//!
//! The rows span what the rule generalises over: the control plane that owns the engine, how
//! many custom endpoints and member instances the cluster has, and whether the modification
//! is one that takes the cluster out of service (a port change) or one that does not (a
//! retention change) — the branch that decides the status the acknowledgement reports, and
//! so the one that could leave the projection reading a record that is not there.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper
//! this test target has, as a real awsQuery form POST carrying the SigV4 credential scope
//! the router keys on. The assertions read the XML the emulator answered with.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_clusters::the_cluster;
use crate::db_instances::{call, emulator, expect_ok, optional_text, subtrees, text, texts};
use crate::lifecycle_poll_loops::poll_until;

/// The status a cluster and a custom endpoint reach once the transition their create owes
/// them has settled.
const AVAILABLE: &str = "available";

/// The instance class a cluster member is created with: one the orderable-options report
/// offers for a cluster of every engine this front door serves.
const CLUSTER_MEMBER_CLASS: &str = "db.r5.large";

/// One row of the modification table.
struct ModifyCase {
    case: &'static str,
    cluster: &'static str,
    engine: &'static str,
    /// How many custom endpoints are created against the cluster.
    endpoints: usize,
    /// How many member instances are created in it.
    members: usize,
    /// The modification itself, as the parameters it carries beyond the identifier.
    modification: &'static [(&'static str, &'static str)],
}

const MODIFY_CASES: &[ModifyCase] = &[
    ModifyCase {
        case: "a graph cluster with one custom endpoint, taken out of service by a port change",
        cluster: "emu-mod-env-graph",
        engine: "neptune",
        endpoints: 1,
        members: 1,
        modification: &[("Port", "8183")],
    },
    ModifyCase {
        case: "a graph cluster with two custom endpoints and a retention change",
        cluster: "emu-mod-env-graph-two",
        engine: "neptune",
        endpoints: 2,
        members: 0,
        modification: &[("BackupRetentionPeriod", "7")],
    },
    ModifyCase {
        case: "an Aurora cluster with two custom endpoints and two members",
        cluster: "emu-mod-env-aurora",
        engine: "aurora-postgresql",
        endpoints: 2,
        members: 2,
        modification: &[("BackupRetentionPeriod", "7")],
    },
    ModifyCase {
        case: "a cluster with no custom endpoint at all",
        cluster: "emu-mod-env-bare",
        engine: "neptune",
        endpoints: 0,
        members: 0,
        modification: &[("DeletionProtection", "true")],
    },
];

/// The `CustomEndpoints` member of one `<DBCluster>` subtree, as its list of addresses.
fn custom_endpoints(cluster: &str) -> Vec<String> {
    subtrees(cluster, "CustomEndpoints")
        .first()
        .map(|list| texts(list, "member"))
        .unwrap_or_default()
}

/// The `DBInstanceIdentifier` of every `DBClusterMembers` entry of one `<DBCluster>`,
/// sorted: WHICH instances the cluster reports is this rule's concern, not the order the
/// writer is reported first in.
fn member_identifiers(cluster: &str) -> Vec<String> {
    let mut found: Vec<String> = subtrees(cluster, "DBClusterMember")
        .iter()
        .map(|member| text(member, "DBInstanceIdentifier"))
        .collect();
    found.sort();
    found
}

/// Poll `DescribeDBClusters` until the cluster reports `available`, and hand back the
/// `<DBCluster>` it settled on.
async fn available_cluster(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    let params = vec![("DBClusterIdentifier", identifier.to_string())];
    let read = || call(router, "DescribeDBClusters", &params);
    let (body, _) = poll_until(
        case,
        &format!("{identifier} reaching {AVAILABLE}"),
        read,
        |http_status, body| {
            http_status == 200
                && subtrees(body, "DBCluster")
                    .first()
                    .is_some_and(|cluster| text(cluster, "Status") == AVAILABLE)
        },
    )
    .await;
    the_cluster(case, &body)
}

/// Poll `DescribeDBClusterEndpoints` until the named custom endpoint reports `available`,
/// which is the state a cluster's endpoint list is read in.
async fn available_endpoint(router: &MultiServiceRouter, case: &str, identifier: &str) {
    let params = vec![("DBClusterEndpointIdentifier", identifier.to_string())];
    let read = || call(router, "DescribeDBClusterEndpoints", &params);
    poll_until(
        case,
        &format!("{identifier} reaching {AVAILABLE}"),
        read,
        |http_status, body| {
            http_status == 200
                && subtrees(body, "DBClusterEndpointList")
                    .first()
                    .is_some_and(|entry| {
                        optional_text(entry, "Status").as_deref() == Some(AVAILABLE)
                    })
        },
    )
    .await;
}

/// Stand up one row's cluster, its members and its custom endpoints, and hand back the
/// address each endpoint publishes and the members created, in the order they were created.
async fn stand_up(router: &MultiServiceRouter, row: &ModifyCase) -> (Vec<String>, Vec<String>) {
    let mut params: Vec<(&str, String)> = vec![
        ("DBClusterIdentifier", row.cluster.to_string()),
        ("Engine", row.engine.to_string()),
    ];
    if row.engine.starts_with("aurora") {
        params.push(("MasterUsername", "emuadmin".to_string()));
        params.push(("MasterUserPassword", "emu-Secret-99".to_string()));
    }
    expect_ok(row.case, call(router, "CreateDBCluster", &params).await);
    available_cluster(router, row.case, row.cluster).await;

    let mut members: Vec<String> = Vec::new();
    for index in 0..row.members {
        let identifier = format!("{}-member-{index}", row.cluster);
        expect_ok(
            row.case,
            call(
                router,
                "CreateDBInstance",
                &[
                    ("DBInstanceIdentifier", identifier.as_str()),
                    ("Engine", row.engine),
                    ("DBInstanceClass", CLUSTER_MEMBER_CLASS),
                    ("DBClusterIdentifier", row.cluster),
                ],
            )
            .await,
        );
        members.push(identifier);
    }

    let mut addresses: Vec<String> = Vec::new();
    for index in 0..row.endpoints {
        let identifier = format!("{}-ce-{index}", row.cluster);
        let created = expect_ok(
            row.case,
            call(
                router,
                "CreateDBClusterEndpoint",
                &[
                    ("DBClusterIdentifier", row.cluster),
                    ("DBClusterEndpointIdentifier", identifier.as_str()),
                    ("EndpointType", "ANY"),
                ],
            )
            .await,
        );
        let address = text(&created, "Endpoint");
        assert!(
            !address.is_empty(),
            "case {:?}: the created endpoint must publish an address: {created}",
            row.case
        );
        available_endpoint(router, row.case, &identifier).await;
        addresses.push(address);
    }
    (addresses, members)
}

#[tokio::test]
async fn a_modification_answers_with_the_cluster_the_store_holds() {
    let router = emulator();

    for row in MODIFY_CASES {
        let (addresses, members) = stand_up(&router, row).await;

        // What the cluster reports through the READ path, which resolves the records around
        // it against a store that holds the cluster.
        let described = available_cluster(&router, row.case, row.cluster).await;
        let described_endpoints = custom_endpoints(&described);
        assert_eq!(
            described_endpoints.len(),
            row.endpoints,
            "case {:?} (DescribeDBClusters): expected {} custom endpoint(s), got \
             {described_endpoints:?}",
            row.case,
            row.endpoints
        );

        let mut params: Vec<(&str, String)> =
            vec![("DBClusterIdentifier", row.cluster.to_string())];
        params.extend(
            row.modification
                .iter()
                .map(|(name, value)| (*name, (*value).to_string())),
        );
        let modified = the_cluster(
            row.case,
            &expect_ok(row.case, call(&router, "ModifyDBCluster", &params).await),
        );

        // The modification's own envelope carries every custom endpoint the cluster has,
        // each as the address that endpoint publishes — not an empty list.
        let answered = custom_endpoints(&modified);
        assert_eq!(
            answered, addresses,
            "case {:?} (ModifyDBCluster): expected <CustomEndpoints> {addresses:?}, got \
             {answered:?} in {modified}",
            row.case
        );
        assert_eq!(
            answered, described_endpoints,
            "case {:?}: the modification must report what the read reports, got {answered:?} \
             against {described_endpoints:?}",
            row.case
        );

        // And the other member read off the records around the cluster: its instances.
        let answered_members = member_identifiers(&modified);
        assert_eq!(
            answered_members, members,
            "case {:?} (ModifyDBCluster): expected <DBClusterMembers> {members:?}, got \
             {answered_members:?} in {modified}",
            row.case
        );
        assert_eq!(
            answered_members,
            member_identifiers(&described),
            "case {:?}: the modification must report the same members as the read, got \
             {answered_members:?}",
            row.case
        );

        // The modification really was applied, so the envelope is the modified cluster
        // rather than a copy of the read above.
        for (member, value) in row.modification {
            let reported = optional_text(&modified, member);
            assert_eq!(
                reported.as_deref(),
                Some(*value),
                "case {:?}: expected <{member}> {value:?} in the answer, got {reported:?}",
                row.case
            );
        }
    }
}

#[tokio::test]
async fn a_modification_keeps_reporting_the_endpoints_left_after_one_is_deleted() {
    // The list is READ from the endpoint records, so a delete takes one out of every later
    // cluster report — the modification envelope included.
    let router = emulator();
    let cluster = "emu-mod-env-deleted";
    let row = ModifyCase {
        case: "two endpoints, one deleted",
        cluster,
        engine: "neptune",
        endpoints: 2,
        members: 0,
        modification: &[],
    };
    let (addresses, _) = stand_up(&router, &row).await;

    let deleted = format!("{cluster}-ce-0");
    expect_ok(
        row.case,
        call(
            &router,
            "DeleteDBClusterEndpoint",
            &[("DBClusterEndpointIdentifier", deleted.as_str())],
        )
        .await,
    );
    // Wait for the delete to have taken the record out, which is what the cluster's own
    // report is read against.
    let params = vec![("DBClusterEndpointIdentifier", deleted.clone())];
    let read = || call(&router, "DescribeDBClusterEndpoints", &params);
    poll_until(
        row.case,
        &format!("{deleted} leaving the store"),
        read,
        |http_status, body| {
            http_status == 200 && subtrees(body, "DBClusterEndpointList").is_empty()
        },
    )
    .await;

    let modified = the_cluster(
        row.case,
        &expect_ok(
            row.case,
            call(
                &router,
                "ModifyDBCluster",
                &[
                    ("DBClusterIdentifier", cluster),
                    ("BackupRetentionPeriod", "5"),
                ],
            )
            .await,
        ),
    );
    let answered = custom_endpoints(&modified);
    assert_eq!(
        answered,
        addresses[1..].to_vec(),
        "case {:?}: expected only the endpoint that is still there, got {answered:?} in \
         {modified}",
        row.case
    );
}
