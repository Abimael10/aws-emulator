// SPDX-License-Identifier: CC0-1.0

//! The Aurora DB cluster snapshot-and-restore lifecycle a client POLLS, and the data a
//! restored cluster serves, end to end on the ASSEMBLED emulator.
//!
//! RULE: every status an Amazon RDS resource is answered in while the service is still
//! working on it is a TRANSIENT status the clock settles, so each reaches its terminal value
//! in finite time with no further call — a DB cluster snapshot reaches `available`, a
//! restored DB cluster reaches `available`, a cluster's managed master user secret reaches
//! `active`, a DB instance reaches `available`. And a DB cluster snapshot is a copy of the
//! DATA the cluster held when it was taken — every database the cluster serves, whichever
//! surface wrote it — so a cluster restored from that snapshot serves exactly that data:
//! what was committed before the snapshot is on it, what was committed after is not, and
//! restoring one snapshot twice gives two clusters whose later writes are their own.
//!
//! The suite lives beside the router because the lifecycle spans three services: Amazon RDS
//! holds the cluster, the snapshot and the instance, Secrets Manager holds the managed
//! master user credential a Data API call authenticates with, and the RDS Data API owns the
//! databases the rows are written to and read back from. A crate-local suite can only ever
//! see one of the three.
//!
//! The rows differ along what the rule generalises over: the KIND of resource polled (a
//! snapshot, a cluster, a managed secret, a DB instance), whether the resource was created
//! outright or restored from a snapshot, WHEN the snapshot was taken relative to each write
//! (before anything, after two rows, after a third row and a second database), how many
//! databases the cluster holds, and how many clusters one snapshot is restored into.
//!
//! Sources: `smithy_query(query="operation", operation="CreateDBClusterSnapshot")` and
//! `smithy_query(query="operation", operation="RestoreDBClusterFromSnapshot")` on the rds
//! model for the two lifecycles, `smithy_query(query="shape", shape="MasterUserSecret")` for
//! `SecretStatus`, and `smithy_query(query="operation", operation="ExecuteStatement")` on the
//! rdsdata model for the call that reads the rows back.
//!
//! The calls and the polls this lifecycle is driven by live in
//! [`crate::rds_cluster_lifecycle_calls`], beside the other suite that drives it
//! (`rds_restored_cluster_vpc_scaling_and_managed_secret.rs`): what each suite is ABOUT stays
//! here, how either one reaches the service does not.

use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{blocks_of, element, opt_element};
use crate::rds_cluster_lifecycle_calls::{
    add_writer, await_active_secret, await_available_cluster, build_router, create_source_cluster,
    served, take_snapshot, Addressed, DATABASE, ENGINE, STATUS_AVAILABLE, TABLE,
};

/// A SECOND database, created through the Data API itself: the rule is about every database
/// a cluster holds, so one of the rows below writes into one the create never named.
const SECOND_DATABASE: &str = "ledgerdb";

/// Restore `snapshot` into a NEW cluster called `identifier`, wait for every status the
/// lifecycle owes, give the restored cluster a managed master user password of its own and
/// turn its Data API endpoint on — the sequence a client runs after a restore — and answer
/// the restored cluster as a Data API caller addresses it.
async fn restore_cluster(
    router: &MultiServiceRouter,
    case: &str,
    snapshot: &str,
    identifier: &str,
) -> Addressed {
    let restored = served(
        router,
        case,
        "RestoreDBClusterFromSnapshot",
        &[
            ("DBClusterIdentifier", identifier),
            ("SnapshotIdentifier", snapshot),
            ("Engine", ENGINE),
        ],
    )
    .await;
    let arn = element(&restored, "DBClusterArn");
    await_available_cluster(router, case, identifier).await;
    served(
        router,
        case,
        "ModifyDBCluster",
        &[
            ("DBClusterIdentifier", identifier),
            ("ManageMasterUserPassword", "true"),
            ("ApplyImmediately", "true"),
        ],
    )
    .await;
    let secret_arn = await_active_secret(router, case, identifier).await;
    await_available_cluster(router, case, identifier).await;
    served(
        router,
        case,
        "EnableHttpEndpoint",
        &[("ResourceArn", arn.as_str())],
    )
    .await;
    let addressed = Addressed { arn, secret_arn };
    addressed.await_data_api(router, case).await;
    addressed
}

/// The whole lifecycle a client drives, in order, with every status it polls for: the
/// snapshot, the restored cluster, the restored cluster's managed secret and its writer all
/// reach their terminal value, and the restored cluster serves the rows the snapshot held
/// and not the one committed after it.
#[tokio::test]
async fn an_aurora_cluster_snapshot_and_restore_converge_and_carry_the_snapshots_rows() {
    let router = build_router();
    let source_cluster = "emu-snaprestore-src";
    let restored_cluster = "emu-snaprestore-dst";
    let snapshot = "emu-snaprestore-snap";
    let case = "the snapshot-and-restore lifecycle";

    let source = create_source_cluster(&router, case, source_cluster, &[]).await;
    add_writer(&router, case, source_cluster, "emu-snaprestore-src-1").await;

    // The rows the snapshot must carry, committed through the Data API before it is taken.
    for sql in [
        &format!("CREATE TABLE {TABLE} (id int PRIMARY KEY, label text)"),
        &format!("INSERT INTO {TABLE} (id, label) VALUES (1, 'before')"),
        &format!("INSERT INTO {TABLE} (id, label) VALUES (2, 'before')"),
    ] {
        source.run(&router, case, sql).await;
    }

    take_snapshot(&router, case, source_cluster, snapshot).await;

    // A row committed AFTER the snapshot, which the restored cluster must therefore not
    // carry.
    source
        .run(
            &router,
            case,
            &format!("INSERT INTO {TABLE} (id, label) VALUES (3, 'after')"),
        )
        .await;

    let restored = restore_cluster(&router, case, snapshot, restored_cluster).await;
    assert_ne!(
        restored.arn, source.arn,
        "case {case:?}: a restore creates a resource of its own, got {} for both",
        restored.arn
    );
    add_writer(&router, case, restored_cluster, "emu-snaprestore-dst-1").await;

    let reported = restored.ids_in(&router, case, DATABASE).await;
    assert_eq!(
        reported,
        vec![1, 2],
        "case {case:?}: expected the rows committed before the snapshot on the restored \
         cluster, got {reported:?}"
    );
    // And the source cluster still carries all three: the restore is a cluster of its own,
    // so writing to one is not writing to the other.
    let reported = source.ids_in(&router, case, DATABASE).await;
    assert_eq!(
        reported,
        vec![1, 2, 3],
        "case {case:?}: expected every committed row on the source cluster, got {reported:?}"
    );
    restored
        .run(
            &router,
            case,
            &format!("INSERT INTO {TABLE} (id, label) VALUES (4, 'restored')"),
        )
        .await;
    let reported = source.ids_in(&router, case, DATABASE).await;
    assert_eq!(
        reported,
        vec![1, 2, 3],
        "case {case:?}: a write to the restored cluster is the restored cluster's alone, got \
         {reported:?} on the source"
    );

    // Both clusters are reported in the account's listing in their terminal status: the polls
    // above waited for a status the service really settled on, not for one read.
    let listed = served(&router, case, "DescribeDBClusters", &[]).await;
    for block in blocks_of(&listed, "DBCluster") {
        let identifier = opt_element(block, "DBClusterIdentifier").unwrap_or_default();
        if identifier != source_cluster && identifier != restored_cluster {
            continue;
        }
        let reported = opt_element(block, "Status").unwrap_or_default();
        assert_eq!(
            reported, STATUS_AVAILABLE,
            "case {identifier:?}: expected {STATUS_AVAILABLE:?} in the listing, got {reported:?}"
        );
    }
}

/// One checkpoint of the source cluster: what is committed before the snapshot is taken, and
/// what a cluster restored from that snapshot must then serve.
struct Checkpoint {
    case: &'static str,
    /// What runs on the source cluster BEFORE the snapshot, as `(database, statement)`.
    writes: &'static [(&'static str, &'static str)],
    /// The identifiers of the snapshot and of the cluster it is restored into.
    snapshot: &'static str,
    restored: &'static str,
    /// The ids the restored cluster's table must answer with in [`DATABASE`], or `None` when
    /// it must hold no such table at all.
    expected: Option<&'static [i64]>,
    /// The same for [`SECOND_DATABASE`].
    expected_second: Option<&'static [i64]>,
}

/// What a restored cluster serves is what its snapshot HELD, over the instants a snapshot
/// can be taken at: before anything was written, after two rows, and after a third row and a
/// second database the cluster was not created with.
#[tokio::test]
async fn what_a_restored_cluster_serves_is_what_its_snapshot_held() {
    let router = build_router();
    let source_cluster = "emu-snapheld-src";
    let source = create_source_cluster(&router, "the source cluster", source_cluster, &[]).await;

    let checkpoints: &[Checkpoint] = &[
        Checkpoint {
            case: "a snapshot taken before anything was written",
            writes: &[],
            snapshot: "emu-snapheld-none",
            restored: "emu-snapheld-dst-none",
            expected: None,
            expected_second: None,
        },
        Checkpoint {
            case: "a snapshot taken after two rows",
            writes: &[
                (
                    DATABASE,
                    "CREATE TABLE sim2real_snap_restore (id int PRIMARY KEY, label text)",
                ),
                (
                    DATABASE,
                    "INSERT INTO sim2real_snap_restore (id, label) VALUES (1, 'first')",
                ),
                (
                    DATABASE,
                    "INSERT INTO sim2real_snap_restore (id, label) VALUES (2, 'second')",
                ),
            ],
            snapshot: "emu-snapheld-two",
            restored: "emu-snapheld-dst-two",
            expected: Some(&[1, 2]),
            expected_second: None,
        },
        Checkpoint {
            case: "a snapshot taken after a third row and a second database",
            writes: &[
                (
                    DATABASE,
                    "INSERT INTO sim2real_snap_restore (id, label) VALUES (3, 'third')",
                ),
                (DATABASE, "CREATE DATABASE ledgerdb"),
                (
                    SECOND_DATABASE,
                    "CREATE TABLE sim2real_snap_restore (id int PRIMARY KEY, label text)",
                ),
                (
                    SECOND_DATABASE,
                    "INSERT INTO sim2real_snap_restore (id, label) VALUES (7, 'ledger')",
                ),
            ],
            snapshot: "emu-snapheld-three",
            restored: "emu-snapheld-dst-three",
            expected: Some(&[1, 2, 3]),
            expected_second: Some(&[7]),
        },
    ];

    for checkpoint in checkpoints {
        let case = checkpoint.case;
        for (database, sql) in checkpoint.writes {
            source.run_in(&router, case, database, sql).await;
        }
        take_snapshot(&router, case, source_cluster, checkpoint.snapshot).await;
        let restored =
            restore_cluster(&router, case, checkpoint.snapshot, checkpoint.restored).await;

        for (database, expected) in [
            (DATABASE, checkpoint.expected),
            (SECOND_DATABASE, checkpoint.expected_second),
        ] {
            match expected {
                Some(expected) => {
                    let reported = restored.ids_in(&router, case, database).await;
                    assert_eq!(
                        reported, expected,
                        "case {case:?}: expected {expected:?} in {database:?} on the restored \
                         cluster, got {reported:?}"
                    );
                }
                None => {
                    restored.assert_no_table_in(&router, case, database).await;
                }
            }
        }
    }

    // One snapshot restores MORE THAN ONCE, and each restored cluster's later writes are its
    // own: the copy a snapshot holds is not consumed by being restored.
    let case = "a second cluster restored from the same snapshot";
    let again = restore_cluster(&router, case, "emu-snapheld-two", "emu-snapheld-dst-again").await;
    let reported = again.ids_in(&router, case, DATABASE).await;
    assert_eq!(
        reported,
        vec![1, 2],
        "case {case:?}: expected the same snapshot to restore again, got {reported:?}"
    );
    again
        .run(
            &router,
            case,
            "INSERT INTO sim2real_snap_restore (id, label) VALUES (9, 'only here')",
        )
        .await;
    let reported = again.ids_in(&router, case, DATABASE).await;
    assert_eq!(
        reported,
        vec![1, 2, 9],
        "case {case:?}: expected the write on this cluster, got {reported:?}"
    );
    let first = Addressed {
        arn: element(
            &served(
                &router,
                case,
                "DescribeDBClusters",
                &[("DBClusterIdentifier", "emu-snapheld-dst-two")],
            )
            .await,
            "DBClusterArn",
        ),
        secret_arn: await_active_secret(&router, case, "emu-snapheld-dst-two").await,
    };
    let reported = first.ids_in(&router, case, DATABASE).await;
    assert_eq!(
        reported,
        vec![1, 2],
        "case {case:?}: a write to one restored cluster is not a write to the other, got \
         {reported:?}"
    );
    // And the source cluster is untouched by every restore above.
    let reported = source.ids_in(&router, case, DATABASE).await;
    assert_eq!(
        reported,
        vec![1, 2, 3],
        "case {case:?}: expected the source cluster's own rows, got {reported:?}"
    );
}
