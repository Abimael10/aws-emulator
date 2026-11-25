// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the SENTENCE `DeleteDBCluster` refuses a request that names NEITHER
//! of its two final-snapshot members with.
//!
//! RULE: `SkipFinalSnapshot` documents "If false is specified, a cluster snapshot is created
//! before the DB cluster is deleted … If SkipFinalSnapshot is false, you must specify a
//! FinalDBSnapshotIdentifier parameter. Default: false", so exactly ONE of the two members
//! has to be given and a request giving neither is refused with the fixed sentence this
//! front door words that parameter combination with
//! ([`state::FINAL_SNAPSHOT_MEMBER_REQUIRED`]) — an imperative naming the two members, with
//! no terminal period and nothing of the request or of the store in it.
//!
//! The rows span what the rule generalises over: how the member is left out (absent
//! altogether, or present as the documented default `false`, in either of the two lexical
//! forms this wire carries a boolean in), and whether the cluster the delete names exists at
//! all — the combination is a statement about the REQUEST, so it is answered before any
//! record is read and the sentence cannot vary with what the store holds.
//!
//! The counterpart is asserted too, so the sentence above is the combination's doing and not
//! a gate refusing every delete: a delete that names one of the two members is answered by a
//! statement about the STORE instead — the cluster it names is not there, or it is there and
//! another gate holds it back — never by the parameter sentence.
//!
//! Every call goes over the wire through the harness in `cluster_lifecycle.rs`, so each
//! assertion reads the XML this emulator itself answered with.

use service_com_amazonaws_docdb_amazonrdsv19::state;

use crate::cluster_lifecycle::{error_code, text, Docdb};

/// The cluster the rows below delete: created here, so the rule is asserted on a cluster
/// this store really holds as well as on one it does not.
const CLUSTER: &str = "emu-final-snapshot-cl";

/// A cluster identifier nothing was created under, for the row that shows the parameter
/// combination is answered before the cluster is looked up.
const ABSENT_CLUSTER: &str = "emu-final-snapshot-absent";

/// A cluster created with `DeletionProtection` enabled, for the rows that reach a gate on the
/// STORE: that gate holds whatever status the cluster is in, so the answer is the same
/// however much of the clock has passed.
const PROTECTED_CLUSTER: &str = "emu-final-snapshot-protected";

/// The members a `CreateDBCluster` for this engine needs.
fn create_members(identifier: &str) -> Vec<(&str, &str)> {
    vec![
        ("DBClusterIdentifier", identifier),
        ("Engine", "docdb"),
        ("MasterUsername", "emuadmin"),
        ("MasterUserPassword", "emu-password-1"),
    ]
}

#[tokio::test]
async fn a_delete_naming_neither_final_snapshot_member_is_refused_with_the_fixed_sentence() {
    let docdb = Docdb::new();
    docdb
        .ok("the cluster to delete", "CreateDBCluster", &create_members(CLUSTER))
        .await;

    // (case, the members the delete carries beside the cluster it names)
    let rows: [(&str, Vec<(&str, &str)>); 3] = [
        ("neither member", Vec::new()),
        (
            "SkipFinalSnapshot as the documented default",
            vec![("SkipFinalSnapshot", "false")],
        ),
        (
            // The Query wire carries a boolean as `true`, `false`, `1` or `0`.
            "SkipFinalSnapshot as the numeral for false",
            vec![("SkipFinalSnapshot", "0")],
        ),
    ];

    for (case, members) in rows {
        for cluster in [CLUSTER, ABSENT_CLUSTER] {
            let case = format!("{case}, cluster {cluster:?}");
            let mut params = vec![("DBClusterIdentifier", cluster)];
            params.extend(members.iter().copied());
            let reply = docdb.call("DeleteDBCluster", &params).await;
            assert_eq!(
                reply.status, 400,
                "case {case:?}: expected HTTP 400, got {} with {}",
                reply.status, reply.xml
            );
            let message = text(&reply.xml, "Message");
            assert_eq!(
                message,
                state::FINAL_SNAPSHOT_MEMBER_REQUIRED,
                "case {case:?}: expected {:?}, got {message:?}",
                state::FINAL_SNAPSHOT_MEMBER_REQUIRED
            );
            // Entirely fixed text: neither the cluster the request named nor anything else
            // it carried appears in it.
            assert!(
                !message.contains(cluster),
                "case {case:?}: the sentence names no value of the request, got {message:?}"
            );
        }
    }

    // The cluster is still there: a refused delete deletes nothing.
    let described = docdb
        .ok(
            "the cluster survives the refused deletes",
            "DescribeDBClusters",
            &[("DBClusterIdentifier", CLUSTER)],
        )
        .await;
    assert_eq!(
        text(&described, "DBClusterIdentifier"),
        CLUSTER,
        "the refused deletes must leave the cluster in state, got {described}"
    );
}

#[tokio::test]
async fn a_delete_that_names_one_of_the_two_members_is_past_that_combination() {
    let docdb = Docdb::new();
    let mut protected = create_members(PROTECTED_CLUSTER);
    // `DeletionProtection`: "If DeletionProtection is enabled, the cluster cannot be deleted
    // unless it is modified and DeletionProtection is disabled" — a gate on the RECORD, so a
    // delete that reaches it is answered the same whatever the clock has done to the
    // cluster's status since it was created.
    protected.push(("DeletionProtection", "true"));
    docdb
        .ok("the protected cluster", "CreateDBCluster", &protected)
        .await;

    // Each row names ONE of the pair, so the combination above is satisfied and the delete
    // reaches the store: the cluster it names is not there at all, or it is there and its own
    // deletion protection holds the delete back. Both answers are statements about the store
    // rather than about the request's parameters, and neither depends on how much of the
    // clock has passed.
    let members: [(&str, Vec<(&str, &str)>); 3] = [
        (
            "SkipFinalSnapshot set",
            vec![("SkipFinalSnapshot", "true")],
        ),
        (
            "SkipFinalSnapshot set as the numeral for true",
            vec![("SkipFinalSnapshot", "1")],
        ),
        (
            "a final snapshot identifier and no SkipFinalSnapshot",
            vec![("FinalDBSnapshotIdentifier", "emu-final-snapshot-snap")],
        ),
    ];
    // (the cluster the delete names, the status and error code the store answers it with)
    let clusters: [(&str, u16, &str); 2] = [
        (ABSENT_CLUSTER, 404, "DBClusterNotFoundFault"),
        (PROTECTED_CLUSTER, 400, "InvalidDBClusterStateFault"),
    ];

    for (case, members) in members {
        for (cluster, status, code) in clusters {
            let case = format!("{case}, cluster {cluster:?}");
            let mut params = vec![("DBClusterIdentifier", cluster)];
            params.extend(members.iter().copied());
            let reply = docdb.call("DeleteDBCluster", &params).await;
            assert_eq!(
                (reply.status, error_code(&reply.xml).as_str()),
                (status, code),
                "case {case:?}: expected the store's own answer, got {} with {}",
                reply.status,
                reply.xml
            );
            let message = text(&reply.xml, "Message");
            assert_ne!(
                message,
                state::FINAL_SNAPSHOT_MEMBER_REQUIRED,
                "case {case:?}: a request naming one of the pair is past that combination, got \
                 {message:?}"
            );
        }
    }

    // The protected cluster is still there: a refused delete deletes nothing.
    let described = docdb
        .ok(
            "the protected cluster survives the refused deletes",
            "DescribeDBClusters",
            &[("DBClusterIdentifier", PROTECTED_CLUSTER)],
        )
        .await;
    assert_eq!(
        text(&described, "DBClusterIdentifier"),
        PROTECTED_CLUSTER,
        "the refused deletes must leave the cluster in state, got {described}"
    );
}
