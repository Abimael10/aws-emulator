// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the resources this service DERIVES from another one: the copy
//! `CopyDBSnapshot` makes of a DB snapshot, the copy `CopyDBClusterSnapshot` makes of a DB
//! cluster snapshot, and the DB instance `RestoreDBInstanceFromDBSnapshot` restores from a
//! snapshot.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper
//! this test target has, as a real awsQuery form POST carrying the SigV4 credential scope
//! the router keys on. The assertions read the XML the emulator answers with, so a handler
//! that computed nothing could not pass them.
//!
//! The rule under test: a derived resource is answered in a NON-TERMINAL status (a copy
//! `copying`, a restored instance `creating`) and then reaches its terminal `available`
//! status on its own, within a bounded span and with no further call from the client — so the
//! wait-until-available loop every AWS waiter and every provisioning step is written as
//! RETURNS. Each loop below reads only a REPORTING operation, so nothing but a transition
//! this service's own clock owes can end it. Once it is available the derived resource is
//! independent of the one it came from: it has its own identifier and its own ARN, it carries
//! the source's engine, and deleting the source leaves it exactly where it is.
//!
//! The cases span what the rule generalises over: the two kinds of snapshot a copy can be
//! made of (a DB instance's and a DB cluster's) and the restore that makes a DB instance out
//! of one — the three derived resources whose terminal status a client waits for.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, emulator, expect_error, expect_ok, optional_text, text, REGION,
};
use crate::lifecycle_poll_loops::poll_until;

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// Poll a report until the resource it names carries `Status`/`DBInstanceStatus`
/// `available`, answering the envelope it settled on.
async fn available_in(
    router: &MultiServiceRouter,
    case: &str,
    action: &'static str,
    member: &'static str,
    parameters: &[(&str, String)],
) -> String {
    let (body, _) = poll_until(
        case,
        "the derived resource reaching available",
        || call(router, action, parameters),
        |status, body| status == 200 && optional_text(body, member).as_deref() == Some("available"),
    )
    .await;
    body
}

/// A DB instance that is `available`, which is the state a snapshot can be taken in.
async fn available_instance(router: &MultiServiceRouter, identifier: &str, engine: &str) {
    expect_ok(
        identifier,
        call(
            router,
            "CreateDBInstance",
            &params![
                "DBInstanceIdentifier" => identifier,
                "Engine" => engine,
                "DBInstanceClass" => "db.m5.large",
                "MasterUsername" => "emuadmin",
                "MasterUserPassword" => "emu-Secret-99",
                "AllocatedStorage" => 20,
            ],
        )
        .await,
    );
    available_in(
        router,
        identifier,
        "DescribeDBInstances",
        "DBInstanceStatus",
        &params!["DBInstanceIdentifier" => identifier],
    )
    .await;
}

/// A DB snapshot of `instance` that has reached `available`, which is the state a copy can be
/// made of it in.
async fn available_snapshot(router: &MultiServiceRouter, instance: &str, snapshot: &str) -> String {
    let created = expect_ok(
        snapshot,
        call(
            router,
            "CreateDBSnapshot",
            &params![
                "DBInstanceIdentifier" => instance,
                "DBSnapshotIdentifier" => snapshot,
            ],
        )
        .await,
    );
    assert_eq!(
        text(&created, "Status"),
        "creating",
        "a snapshot that has just been asked for is still being taken, got {created}"
    );
    available_in(
        router,
        snapshot,
        "DescribeDBSnapshots",
        "Status",
        &params!["DBSnapshotIdentifier" => snapshot],
    )
    .await
}

#[tokio::test]
async fn a_db_snapshot_copy_reaches_available_and_outlives_its_source() {
    let router = emulator();
    available_instance(&router, "emu-derived-inst", "mysql").await;
    let source = available_snapshot(&router, "emu-derived-inst", "emu-derived-snap").await;

    // The copy is answered while it is still being made, which is the status
    // `CopyDBSnapshot` documents cancelling a copy against.
    let copied = expect_ok(
        "the copy",
        call(
            &router,
            "CopyDBSnapshot",
            &params![
                "SourceDBSnapshotIdentifier" => "emu-derived-snap",
                "TargetDBSnapshotIdentifier" => "emu-derived-snap-copy",
            ],
        )
        .await,
    );
    assert_eq!(
        text(&copied, "Status"),
        "copying",
        "a copy that has just been asked for is still being made, got {copied}"
    );

    // And it reaches `available` on its own: this loop makes no mutating call, so only a
    // transition this service's clock owes can end it.
    let copy = available_in(
        &router,
        "the copy",
        "DescribeDBSnapshots",
        "Status",
        &params!["DBSnapshotIdentifier" => "emu-derived-snap-copy"],
    )
    .await;
    assert_eq!(
        text(&copy, "DBSnapshotArn"),
        format!("arn:aws:rds:{REGION}:{ACCOUNT}:snapshot:emu-derived-snap-copy"),
        "the copy must carry an ARN of its own, got {copy}"
    );
    assert_ne!(
        text(&copy, "DBSnapshotArn"),
        text(&source, "DBSnapshotArn"),
        "the copy's ARN must differ from the source's"
    );
    assert_eq!(
        text(&copy, "Engine"),
        text(&source, "Engine"),
        "the copy must carry the source's engine, got {copy}"
    );

    // Deleting the source leaves the copy exactly where it was: the copy is an independent
    // snapshot, not a view of the one it was made from.
    expect_ok(
        "deleting the source",
        call(
            &router,
            "DeleteDBSnapshot",
            &params!["DBSnapshotIdentifier" => "emu-derived-snap"],
        )
        .await,
    );
    expect_error(
        "the deleted source",
        call(
            &router,
            "DescribeDBSnapshots",
            &params!["DBSnapshotIdentifier" => "emu-derived-snap"],
        )
        .await,
        404,
        "DBSnapshotNotFound",
    );
    let after = expect_ok(
        "the copy after the source was deleted",
        call(
            &router,
            "DescribeDBSnapshots",
            &params!["DBSnapshotIdentifier" => "emu-derived-snap-copy"],
        )
        .await,
    );
    assert_eq!(
        text(&after, "Status"),
        "available",
        "the copy must stay available once the source is gone, got {after}"
    );
}

#[tokio::test]
async fn a_db_cluster_snapshot_copy_reaches_available_and_outlives_its_source() {
    let router = emulator();
    expect_ok(
        "the cluster",
        call(
            &router,
            "CreateDBCluster",
            &params![
                "DBClusterIdentifier" => "emu-derived-cluster",
                "Engine" => "aurora-mysql",
                "MasterUsername" => "emuadmin",
                "MasterUserPassword" => "emu-Secret-99",
            ],
        )
        .await,
    );
    expect_ok(
        "the cluster's writer",
        call(
            &router,
            "CreateDBInstance",
            &params![
                "DBInstanceIdentifier" => "emu-derived-cluster-writer",
                "Engine" => "aurora-mysql",
                "DBInstanceClass" => "db.r6g.large",
                "DBClusterIdentifier" => "emu-derived-cluster",
            ],
        )
        .await,
    );
    available_in(
        &router,
        "the cluster",
        "DescribeDBClusters",
        "Status",
        &params!["DBClusterIdentifier" => "emu-derived-cluster"],
    )
    .await;
    available_in(
        &router,
        "the cluster's writer",
        "DescribeDBInstances",
        "DBInstanceStatus",
        &params!["DBInstanceIdentifier" => "emu-derived-cluster-writer"],
    )
    .await;

    expect_ok(
        "the cluster snapshot",
        call(
            &router,
            "CreateDBClusterSnapshot",
            &params![
                "DBClusterIdentifier" => "emu-derived-cluster",
                "DBClusterSnapshotIdentifier" => "emu-derived-cluster-snap",
            ],
        )
        .await,
    );
    let source = available_in(
        &router,
        "the cluster snapshot",
        "DescribeDBClusterSnapshots",
        "Status",
        &params!["DBClusterSnapshotIdentifier" => "emu-derived-cluster-snap"],
    )
    .await;

    let copied = expect_ok(
        "the cluster snapshot's copy",
        call(
            &router,
            "CopyDBClusterSnapshot",
            &params![
                "SourceDBClusterSnapshotIdentifier" => "emu-derived-cluster-snap",
                "TargetDBClusterSnapshotIdentifier" => "emu-derived-cluster-snap-copy",
            ],
        )
        .await,
    );
    assert_eq!(
        text(&copied, "Status"),
        "copying",
        "a copy that has just been asked for is still being made, got {copied}"
    );
    let copy = available_in(
        &router,
        "the cluster snapshot's copy",
        "DescribeDBClusterSnapshots",
        "Status",
        &params!["DBClusterSnapshotIdentifier" => "emu-derived-cluster-snap-copy"],
    )
    .await;
    assert_eq!(
        text(&copy, "DBClusterSnapshotArn"),
        format!("arn:aws:rds:{REGION}:{ACCOUNT}:cluster-snapshot:emu-derived-cluster-snap-copy"),
        "the copy must carry an ARN of its own, got {copy}"
    );
    assert_ne!(
        text(&copy, "DBClusterSnapshotArn"),
        text(&source, "DBClusterSnapshotArn"),
        "the copy's ARN must differ from the source's"
    );
    assert_eq!(
        text(&copy, "Engine"),
        text(&source, "Engine"),
        "the copy must carry the source's engine, got {copy}"
    );

    expect_ok(
        "deleting the source cluster snapshot",
        call(
            &router,
            "DeleteDBClusterSnapshot",
            &params!["DBClusterSnapshotIdentifier" => "emu-derived-cluster-snap"],
        )
        .await,
    );
    let after = expect_ok(
        "the copy after the source was deleted",
        call(
            &router,
            "DescribeDBClusterSnapshots",
            &params!["DBClusterSnapshotIdentifier" => "emu-derived-cluster-snap-copy"],
        )
        .await,
    );
    assert_eq!(
        text(&after, "Status"),
        "available",
        "the copy must stay available once the source is gone, got {after}"
    );
}

#[tokio::test]
async fn an_instance_restored_from_a_snapshot_reaches_available() {
    let router = emulator();
    available_instance(&router, "emu-derived-restore-src", "postgres").await;
    let snapshot =
        available_snapshot(&router, "emu-derived-restore-src", "emu-derived-restore-snap").await;

    let restored = expect_ok(
        "the restore",
        call(
            &router,
            "RestoreDBInstanceFromDBSnapshot",
            &params![
                "DBInstanceIdentifier" => "emu-derived-restore-dst",
                "DBSnapshotIdentifier" => "emu-derived-restore-snap",
            ],
        )
        .await,
    );
    assert_eq!(
        text(&restored, "DBInstanceStatus"),
        "creating",
        "a restored instance is still being created when the call answers, got {restored}"
    );
    let settled = available_in(
        &router,
        "the restore",
        "DescribeDBInstances",
        "DBInstanceStatus",
        &params!["DBInstanceIdentifier" => "emu-derived-restore-dst"],
    )
    .await;
    assert_eq!(
        text(&settled, "Engine"),
        text(&snapshot, "Engine"),
        "the restored instance must run the snapshot's engine, got {settled}"
    );
    assert_eq!(
        text(&settled, "DBInstanceArn"),
        format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:emu-derived-restore-dst"),
        "the restored instance must carry an ARN of its own, got {settled}"
    );
}
