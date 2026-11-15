// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the SENTENCE this service answers an unsupported `Filter$Name`
//! with.
//!
//! The rule under test: a report refuses a `Filters` entry whose `Name` it does not support
//! with ONE sentence — the fixed text, then the name the request carried, and nothing after
//! it. In particular it does NOT append the filter names this emulator happens to implement:
//! that list is this emulator's own support rather than the API's, and a caller reading it
//! back would build its next request against the wrong set.
//!
//! The table spans what the rule generalises over: which report refused the filter, and what
//! kind of name it was — a name no report of this API publishes, a case variant of one this
//! report does, a truncation of one, and a name that belongs to another API's report. The
//! recorded request is ONE row.
//!
//! Every call goes over the awsQuery wire through the harness this test target shares
//! (`db_cluster_lifecycle::Neptune`), so each assertion reads the XML the emulator itself
//! answered with.

use crate::db_cluster_lifecycle::Neptune;

/// One row: the report, and the `Filter$Name` it does not support.
struct FilterCase {
    case: &'static str,
    action: &'static str,
    name: &'static str,
}

#[tokio::test]
async fn an_unsupported_filter_name_is_refused_with_fixed_text_and_the_name() {
    let neptune = Neptune::new();
    let rows = [
        FilterCase {
            case: "a name no report of this API publishes",
            action: "DescribeDBClusters",
            name: "not-a-filter-at-all",
        },
        FilterCase {
            case: "a case variant of a name this report does publish",
            action: "DescribeDBClusters",
            name: "DB-Cluster-Id",
        },
        FilterCase {
            case: "a truncation of a name this report publishes",
            action: "DescribeDBClusters",
            name: "db-cluster",
        },
        FilterCase {
            case: "a name another API's report publishes",
            action: "DescribeDBClusters",
            name: "clone-group-id",
        },
        FilterCase {
            case: "an instance-level name on the cluster report",
            action: "DescribeDBClusters",
            name: "db-instance-id",
        },
        FilterCase {
            case: "a cluster-level name on the instance report",
            action: "DescribeDBInstances",
            name: "db-cluster-resource-id",
        },
        FilterCase {
            case: "a case variant on the instance report",
            action: "DescribeDBInstances",
            name: "Engine",
        },
    ];

    for row in &rows {
        let reply = neptune
            .call(
                row.action,
                &[
                    ("Filters.Filter.1.Name", row.name),
                    ("Filters.Filter.1.Values.Value.1", "any-value"),
                ],
            )
            .await;
        assert_eq!(
            reply.status, 400,
            "case {:?}: expected HTTP 400, got {} with body {}",
            row.case, reply.status, reply.body
        );
        let message = reply.error_message();
        let expected = format!("Unrecognized filter name: {}", row.name);
        assert_eq!(
            message, expected,
            "case {:?}: expected {expected:?}, got {message:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_supported_filter_name_is_still_served() {
    let neptune = Neptune::new();
    let identifier = "emu-filter-graph";
    let created = neptune
        .call(
            "CreateDBCluster",
            &[("DBClusterIdentifier", identifier), ("Engine", "neptune")],
        )
        .await;
    assert_eq!(
        created.status, 200,
        "expected the create to be served, got {} with body {}",
        created.status, created.body
    );

    // The refusal is about names the report does not support: the ones it does are served,
    // so the fixed sentence above cannot be reached by narrowing it to this cluster.
    for (case, name, value) in [
        ("the cluster-identifier filter", "db-cluster-id", identifier),
        ("the engine filter", "engine", "neptune"),
    ] {
        let reply = neptune
            .call(
                "DescribeDBClusters",
                &[
                    ("Filters.Filter.1.Name", name),
                    ("Filters.Filter.1.Values.Value.1", value),
                ],
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: expected HTTP 200, got {} with body {}",
            reply.status, reply.body
        );
        let reported = reply.field(&[
            "DescribeDBClustersResult",
            "DBClusters",
            "DBCluster",
            "DBClusterIdentifier",
        ]);
        assert_eq!(
            reported, identifier,
            "case {case:?}: expected the stored cluster reported, got {reported:?} in {}",
            reply.body
        );
    }
}
