// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the SENTENCE this service's `DBClusterAlreadyExistsFault` carries.
//!
//! The rule under test: the fault a create that names an identifier the region already holds
//! is refused with (HTTP 400, `smithy_query(query="operation",
//! operation="CreateDBCluster")`) carries the FIXED sentence the service words that fault
//! class with — the whole message is fixed text, and neither the identifier the request
//! carried nor the one the store holds appears in it.
//!
//! The table spans what the rule generalises over: the shape of the identifier the first
//! create stored (plain, digit-bearing, a single letter, one at the member's published
//! length bound), and the spelling the second create sends — the same one, and the same one
//! in another letter case, since the identifier "isn't case-sensitive". One sentence answers
//! every row, which is the point: nothing of the request reaches it.
//!
//! Every call goes over the awsQuery wire through the harness this test target shares
//! (`db_cluster_lifecycle::Neptune`), so each assertion reads the XML the emulator itself
//! answered with.

use crate::db_cluster_lifecycle::Neptune;

/// The FIXED sentence the fault carries, written out here rather than read from the crate so
/// a drift in it fails this test.
const DB_CLUSTER_ALREADY_EXISTS: &str = "DB Cluster already exists";

/// The longest `DBClusterIdentifier` this service admits: "must contain from 1 to 63
/// letters, numbers, or hyphens" — the bound the member's own documentation states, which
/// this crate's state carries as `IDENTIFIER_MAX_LEN`.
const IDENTIFIER_MAX_LEN: usize = 63;

/// `CreateDBCluster` for a Neptune cluster under `identifier`.
async fn create_cluster(neptune: &Neptune, identifier: &str) -> crate::db_cluster_lifecycle::Reply {
    neptune
        .call(
            "CreateDBCluster",
            &[("DBClusterIdentifier", identifier), ("Engine", "neptune")],
        )
        .await
}

#[tokio::test]
async fn a_duplicate_cluster_identifier_is_refused_with_the_one_fixed_sentence() {
    let neptune = Neptune::new();
    let longest = format!("c{}", "a".repeat(IDENTIFIER_MAX_LEN - 1));

    // (case, the identifier the first create stores, the spelling the second create sends)
    let rows: [(&str, &str, String); 5] = [
        (
            "a plain identifier",
            "emu-dup-graph",
            "emu-dup-graph".to_string(),
        ),
        (
            "an identifier carrying digits and hyphens",
            "emu-dup-graph-2024-01",
            "emu-dup-graph-2024-01".to_string(),
        ),
        ("a single-letter identifier", "c", "c".to_string()),
        (
            "an identifier at the published length bound",
            &longest,
            longest.clone(),
        ),
        (
            "a held identifier named in another letter case",
            "emu-dup-graph-mixed",
            "EMU-Dup-Graph-Mixed".to_string(),
        ),
    ];

    for (case, stored_as, requested_as) in rows {
        let created = create_cluster(&neptune, stored_as).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected the first create to be served, got HTTP {} with body {}",
            created.status, created.body
        );

        let refused = create_cluster(&neptune, &requested_as).await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (400, "DBClusterAlreadyExistsFault".to_string()),
            "case {case:?}: expected 400 DBClusterAlreadyExistsFault, got body {}",
            refused.body
        );
        let message = refused.error_message();
        assert_eq!(
            message, DB_CLUSTER_ALREADY_EXISTS,
            "case {case:?}: expected the fixed sentence {DB_CLUSTER_ALREADY_EXISTS:?}, got \
             {message:?}"
        );
        // Nothing of the request reaches the sentence: neither spelling of the identifier is
        // in it.
        for spelling in [stored_as, requested_as.as_str()] {
            assert!(
                !message.contains(spelling),
                "case {case:?}: the sentence names no identifier, got {message:?}"
            );
        }

        // The refused create left the stored cluster alone.
        let described = neptune
            .call("DescribeDBClusters", &[("DBClusterIdentifier", stored_as)])
            .await;
        assert_eq!(
            (
                described.status,
                described.field(&[
                    "DescribeDBClustersResult",
                    "DBClusters",
                    "DBCluster",
                    "DBClusterIdentifier"
                ])
            ),
            (200, stored_as.to_string()),
            "case {case:?}: expected the refused create to leave the cluster alone, got body {}",
            described.body
        );
    }
}
