// SPDX-License-Identifier: CC0-1.0

//! Deletion protection outlives the clock.
//!
//! RULE: `DeletionProtection` documents "If DeletionProtection is enabled, the cluster
//! cannot be deleted unless it is modified and DeletionProtection is disabled", so the gate
//! is on the stored RECORD and not on any status the clock has moved that record to: a
//! delete naming a protected cluster is refused the same way however much of the cluster's
//! provisioning window has passed, and only clearing the protection lets a delete through.
//!
//! This is the half of `DeleteDBCluster`'s refusals that a case reading only the error CODE
//! cannot see: the modelled `InvalidDBClusterStateFault` is also what a delete of a cluster
//! still `creating` is refused with, so a case that deletes inside the provisioning window
//! is answered the same whether the protection was honoured or merely unobserved. Every row
//! below therefore reads the refusal's own sentence, and one row deletes only once the
//! cluster has SETTLED to `available` — where the status gate is out of the way and the
//! protection is the only thing left that can refuse it.
//!
//! The rows span what the rule generalises over: which of the two final-snapshot members the
//! delete carries (and in which of the forms this wire carries a boolean), and how much of
//! the clock has passed since the create. The counterpart is asserted too, so the refusal is
//! the protection's doing and not a gate refusing every delete: once `ModifyDBCluster`
//! disables the protection the very same delete succeeds and the cluster is gone from the
//! store.
//!
//! Every call goes over the wire through the harness in `cluster_lifecycle.rs`.

use std::time::{Duration, Instant};

use crate::cluster_lifecycle::{error_code, text, Docdb};

/// How long a case waits for the CLOCK to settle a cluster before it gives up.
///
/// Generous: a deadline for a machine under load, not a span anything is expected to take.
/// The service never sleeps for a due instant, so a case that needs the settled status looks
/// again until the service reports it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long one poll pauses before it reads the cluster again. Only how often the condition
/// is checked, never what a row asserts.
const POLL_PAUSE: Duration = Duration::from_millis(10);

/// One row: the case, the cluster it creates, whether it lets the clock settle that cluster
/// before the delete, and the members the delete carries beside the cluster it names.
type Row = (&'static str, &'static str, bool, Vec<(&'static str, &'static str)>);

/// The members a `CreateDBCluster` for this engine needs, with deletion protection enabled.
fn protected_cluster(identifier: &str) -> Vec<(&str, &str)> {
    vec![
        ("DBClusterIdentifier", identifier),
        ("Engine", "docdb"),
        ("MasterUsername", "emuadmin"),
        ("MasterUserPassword", "emu-password-1"),
        ("DeletionProtection", "true"),
    ]
}

/// Read the cluster until the service reports it `available`, and answer that response.
///
/// `std::thread::sleep` rather than `tokio::time::sleep`, as this crate's harness does for
/// the same reason (`cluster_lifecycle::let_the_clock_catch_up`): the workspace's tokio does
/// not enable the `time` feature here. Each test runs on its own thread, so pausing it
/// between two reads of the condition is harmless.
async fn settled(docdb: &Docdb, case: &str, identifier: &str) -> String {
    let started = Instant::now();
    loop {
        let described = docdb
            .ok(case, "DescribeDBClusters", &[("DBClusterIdentifier", identifier)])
            .await;
        if text(&described, "Status") == "available" {
            return described;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the cluster never settled to available, got {described}"
        );
        std::thread::sleep(POLL_PAUSE);
    }
}

#[tokio::test]
async fn a_protected_cluster_is_refused_whatever_the_clock_has_done() {
    // Each row names one of the two final-snapshot members and how much of the clock has
    // passed since the create.
    let rows: [Row; 6] = [
        (
            "SkipFinalSnapshot, deleted straight after the create",
            "emu-protection-skip-fresh",
            false,
            vec![("SkipFinalSnapshot", "true")],
        ),
        (
            "SkipFinalSnapshot, deleted once the cluster is available",
            "emu-protection-skip-settled",
            true,
            vec![("SkipFinalSnapshot", "true")],
        ),
        (
            // The Query wire carries a boolean as `true`, `false`, `1` or `0`.
            "SkipFinalSnapshot as the numeral, deleted straight after the create",
            "emu-protection-numeral-fresh",
            false,
            vec![("SkipFinalSnapshot", "1")],
        ),
        (
            "SkipFinalSnapshot as the numeral, deleted once the cluster is available",
            "emu-protection-numeral-settled",
            true,
            vec![("SkipFinalSnapshot", "1")],
        ),
        (
            "a final snapshot identifier, deleted straight after the create",
            "emu-protection-snapshot-fresh",
            false,
            vec![("FinalDBSnapshotIdentifier", "emu-protection-snap-a")],
        ),
        (
            "a final snapshot identifier, deleted once the cluster is available",
            "emu-protection-snapshot-settled",
            true,
            vec![("FinalDBSnapshotIdentifier", "emu-protection-snap-b")],
        ),
    ];

    for (case, cluster, settle_first, members) in rows {
        // Each row gets its own store, so what it deletes is the cluster it created.
        let docdb = Docdb::new();
        let created = docdb
            .ok(case, "CreateDBCluster", &protected_cluster(cluster))
            .await;
        assert_eq!(
            text(&created, "DeletionProtection"),
            "true",
            "case {case:?}: the create must report the protection it was given, got {created}"
        );
        if settle_first {
            let described = settled(&docdb, case, cluster).await;
            assert_eq!(
                text(&described, "DeletionProtection"),
                "true",
                "case {case:?}: a cluster the clock has settled still carries its \
                 protection, got {described}"
            );
        }

        let mut params = vec![("DBClusterIdentifier", cluster)];
        params.extend(members.iter().copied());
        let reply = docdb.call("DeleteDBCluster", &params).await;
        assert_eq!(
            (reply.status, error_code(&reply.xml).as_str()),
            (400, "InvalidDBClusterStateFault"),
            "case {case:?}: a protected cluster cannot be deleted, got {} with {}",
            reply.status,
            reply.xml
        );
        let message = text(&reply.xml, "Message");
        assert!(
            message.contains("DeletionProtection"),
            "case {case:?}: the refusal must say which gate answered it — the protection on \
             the record, not the status the clock has reached — got {message:?}"
        );
        assert!(
            message.contains(cluster),
            "case {case:?}: the refusal names the cluster the delete named, got {message:?}"
        );

        // The cluster is still there: a refused delete deletes nothing.
        let described = docdb
            .ok(case, "DescribeDBClusters", &[("DBClusterIdentifier", cluster)])
            .await;
        assert_eq!(
            text(&described, "DBClusterIdentifier"),
            cluster,
            "case {case:?}: the refused delete must leave the cluster in state, got \
             {described}"
        );

        // The counterpart: the protection is the ONLY thing holding this delete back, so the
        // same delete succeeds once `ModifyDBCluster` has disabled it on a cluster the clock
        // has settled.
        settled(&docdb, case, cluster).await;
        let modified = docdb
            .ok(
                case,
                "ModifyDBCluster",
                &[
                    ("DBClusterIdentifier", cluster),
                    ("DeletionProtection", "false"),
                    ("ApplyImmediately", "true"),
                ],
            )
            .await;
        assert_eq!(
            text(&modified, "DeletionProtection"),
            "false",
            "case {case:?}: the modify must report the protection it cleared, got {modified}"
        );
        let deleted = docdb.ok(case, "DeleteDBCluster", &params).await;
        assert_eq!(
            text(&deleted, "Status"),
            "deleting",
            "case {case:?}: a cluster AWS is deleting reports that status, got {deleted}"
        );
        let gone = docdb.call("DescribeDBClusters", &[("DBClusterIdentifier", cluster)]).await;
        assert_eq!(
            (gone.status, error_code(&gone.xml).as_str()),
            (404, "DBClusterNotFoundFault"),
            "case {case:?}: the deleted cluster must be gone from the store, got {} with {}",
            gone.status,
            gone.xml
        );
    }
}
