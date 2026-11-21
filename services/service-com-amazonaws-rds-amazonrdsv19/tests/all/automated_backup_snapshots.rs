// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the AUTOMATED DB snapshots a DB instance's own backups take, which
//! `DescribeDBSnapshots` reports under the `automated` snapshot type.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper
//! this test target has, as a real awsQuery form POST carrying the SigV4 credential scope
//! the router keys on. The assertions read the XML the emulator answers with, so a handler
//! that computed nothing could not pass them.
//!
//! The rule under test: a DB instance that keeps automated backups — `BackupRetentionPeriod`
//! above zero, "to disable automated backups, set this value to 0" — HAS an automated
//! snapshot of its own, taken by the service rather than by a caller, and it has it from the
//! moment its automated backup stops "waiting for the first automated snapshot to be
//! available" (`DBInstanceAutomatedBackup$Status`: `creating` until then, `active` for a
//! current instance). That snapshot is reported as `SnapshotType` `automated`, is
//! `available`, carries the instance's own settings, and is restorable like any other
//! snapshot; it is not one a caller asked for, so it is absent from the `manual` report, and
//! it goes away when the instance stops keeping automated backups at all.
//!
//! The table spans what the rule generalises over: the engine the instance runs, the
//! retention period it keeps (the documented default when the request names none, a middling
//! value, the published maximum of 35 days, and the zero that disables backups), and whether
//! the instance is a cluster member — whose "data is held in the cluster volume", so its
//! backups are the cluster's and it keeps none of its own.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, emulator, expect_ok, optional_text, subtrees, text, texts, REGION,
};
use crate::lifecycle_poll_loops::poll_until;

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// The instance class every instance below is created with; which class an instance runs on
/// is nothing an automated snapshot reads.
const INSTANCE_CLASS: &str = "db.m5.large";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// One row of the retention table: an instance, the engine it runs, the
/// `BackupRetentionPeriod` the create asks for, and whether it then keeps automated backups.
struct RetentionCase {
    case: &'static str,
    instance: &'static str,
    engine: &'static str,
    /// `None` for a create that names none at all, which takes the documented default
    /// ("Default: 1").
    retention: Option<i32>,
    /// What `BackupRetentionPeriod` reads back as, which decides whether the instance keeps
    /// automated backups.
    expected_retention: &'static str,
}

/// Source for the bounds: `BackupRetentionPeriod` documents "Default: 1", "must be a value
/// from 0 to 35" and "can't be set to 0 if the DB instance is a source to read replicas".
const RETENTION_CASES: &[RetentionCase] = &[
    RetentionCase {
        case: "postgres, no retention named at all",
        instance: "emu-auto-pg-default",
        engine: "postgres",
        retention: None,
        expected_retention: "1",
    },
    RetentionCase {
        case: "mysql, a week of backups",
        instance: "emu-auto-my-week",
        engine: "mysql",
        retention: Some(7),
        expected_retention: "7",
    },
    RetentionCase {
        case: "mariadb at the published maximum retention",
        instance: "emu-auto-maria-max",
        engine: "mariadb",
        retention: Some(35),
        expected_retention: "35",
    },
    RetentionCase {
        case: "oracle with automated backups disabled",
        instance: "emu-auto-ora-off",
        engine: "oracle-se2",
        retention: Some(0),
        expected_retention: "0",
    },
];

/// `CreateDBInstance` followed by a poll loop until the instance reports `available`, which
/// is the status its automated backup reports as `active` from. Answers the instance's own
/// `DescribeDBInstances` envelope.
async fn available_instance(
    router: &MultiServiceRouter,
    identifier: &str,
    parameters: &[(&str, String)],
) -> String {
    let mut request = params!["DBInstanceIdentifier" => identifier].to_vec();
    request.extend(parameters.iter().cloned());
    expect_ok(identifier, call(router, "CreateDBInstance", &request).await);
    let read = params!["DBInstanceIdentifier" => identifier];
    let (body, _) = poll_until(
        identifier,
        "the DB instance reaching available",
        || call(router, "DescribeDBInstances", &read),
        |status, body| {
            status == 200
                && optional_text(body, "DBInstanceStatus").as_deref() == Some("available")
        },
    )
    .await;
    body
}

/// The create parameters of a STANDALONE DB instance of `engine`: everything but the
/// retention period a row sets for itself.
fn standalone(engine: &str) -> Vec<(&str, String)> {
    params![
        "Engine" => engine,
        "DBInstanceClass" => INSTANCE_CLASS,
        "MasterUsername" => "emuadmin",
        "MasterUserPassword" => "emu-Secret-99",
        "AllocatedStorage" => 20,
    ]
    .to_vec()
}

/// The `<DBSnapshot>` subtrees `DescribeDBSnapshots` reports for `parameters`.
async fn snapshots(
    router: &MultiServiceRouter,
    case: &str,
    parameters: &[(&str, String)],
) -> Vec<String> {
    let body = expect_ok(case, call(router, "DescribeDBSnapshots", parameters).await);
    subtrees(&body, "DBSnapshot")
}

/// The automated snapshots of `instance`, polled for until there is one: the snapshot is a
/// change the clock owes the instance, so the loop waits for the CONDITION rather than for
/// any span of time.
async fn automated_snapshots_of(
    router: &MultiServiceRouter,
    case: &str,
    instance: &str,
) -> Vec<String> {
    let read = params![
        "DBInstanceIdentifier" => instance,
        "SnapshotType" => "automated",
    ];
    let (body, _) = poll_until(
        case,
        "the instance's automated snapshot being reported",
        || call(router, "DescribeDBSnapshots", &read),
        |status, body| status == 200 && !subtrees(body, "DBSnapshot").is_empty(),
    )
    .await;
    subtrees(&body, "DBSnapshot")
}

#[tokio::test]
async fn an_instance_that_keeps_automated_backups_has_one_automated_snapshot_of_its_own() {
    for row in RETENTION_CASES {
        // Each row gets its own emulator, so one row's snapshots cannot be read as another's.
        let router = emulator();
        let mut parameters = standalone(row.engine);
        if let Some(days) = row.retention {
            parameters.push(("BackupRetentionPeriod", days.to_string()));
        }
        let described = available_instance(&router, row.instance, &parameters).await;
        assert_eq!(
            text(&described, "BackupRetentionPeriod"),
            row.expected_retention,
            "case {:?}: expected the instance to keep {} days of backups, got {described}",
            row.case,
            row.expected_retention
        );

        if row.expected_retention == "0" {
            // An instance that keeps no automated backups takes no snapshot of them: the
            // report stays empty however often it is read, which is what the second read
            // after the status settled shows.
            for attempt in ["the first read", "and the second"] {
                let reported = snapshots(
                    &router,
                    row.case,
                    &params![
                        "DBInstanceIdentifier" => row.instance,
                        "SnapshotType" => "automated",
                    ],
                )
                .await;
                assert!(
                    reported.is_empty(),
                    "case {:?}: {attempt} of an instance with automated backups disabled \
                     must report no automated snapshot, got {reported:?}",
                    row.case
                );
            }
            continue;
        }

        let taken = automated_snapshots_of(&router, row.case, row.instance).await;
        assert_eq!(
            taken.len(),
            1,
            "case {:?}: an instance has ONE automated snapshot, got {taken:?}",
            row.case
        );
        let snapshot = &taken[0];
        let identifier = text(snapshot, "DBSnapshotIdentifier");
        // Amazon RDS names the snapshots it takes itself under the `rds:` prefix, which is
        // the form `CopyDBSnapshot$SourceDBSnapshotIdentifier` gives as an example
        // ("rds:mysql-instance1-snapshot-20130805") and which no caller can spell, since no
        // identifier may carry a colon.
        assert!(
            identifier.starts_with(&format!("rds:{}-", row.instance)),
            "case {:?}: expected an automated snapshot named rds:{}-…, got {identifier:?}",
            row.case,
            row.instance
        );
        for (member, expected) in [
            ("SnapshotType", "automated".to_string()),
            ("Status", "available".to_string()),
            ("PercentProgress", "100".to_string()),
            ("DBInstanceIdentifier", row.instance.to_string()),
            ("Engine", row.engine.to_string()),
            ("BackupRetentionPeriod", row.expected_retention.to_string()),
            (
                "DBSnapshotArn",
                format!("arn:aws:rds:{REGION}:{ACCOUNT}:snapshot:{identifier}"),
            ),
        ] {
            assert_eq!(
                text(snapshot, member),
                expected,
                "case {:?}: expected the automated snapshot's {member} to be {expected:?}, \
                 got {snapshot}",
                row.case
            );
        }
        // Every setting it reports is the instance's own, read off the instance it was taken
        // of rather than invented.
        for member in [
            "DbiResourceId",
            "AllocatedStorage",
            "MasterUsername",
            "EngineVersion",
            "AvailabilityZone",
            "StorageType",
            "InstanceCreateTime",
        ] {
            assert_eq!(
                optional_text(snapshot, member),
                optional_text(&described, member),
                "case {:?}: the automated snapshot reported {member} as {:?}, but the \
                 instance holds {:?}",
                row.case,
                optional_text(snapshot, member),
                optional_text(&described, member)
            );
        }

        // It is not a snapshot a caller asked for, so the `manual` report leaves it out —
        // and the unfiltered report, which "returns both automated and manual snapshots",
        // carries it.
        let manual = snapshots(
            &router,
            row.case,
            &params!["DBInstanceIdentifier" => row.instance, "SnapshotType" => "manual"],
        )
        .await;
        assert!(
            manual.is_empty(),
            "case {:?}: an automated snapshot is not a manual one, got {manual:?}",
            row.case
        );
        let every = snapshots(
            &router,
            row.case,
            &params!["DBInstanceIdentifier" => row.instance],
        )
        .await;
        assert_eq!(
            every
                .iter()
                .map(|snapshot| text(snapshot, "DBSnapshotIdentifier"))
                .collect::<Vec<String>>(),
            vec![identifier.clone()],
            "case {:?}: the unfiltered report must carry the automated snapshot, got {every:?}",
            row.case
        );
        // And it is addressable by its own identifier, like any other snapshot.
        let named = snapshots(
            &router,
            row.case,
            &params!["DBSnapshotIdentifier" => identifier.clone()],
        )
        .await;
        assert_eq!(
            named.len(),
            1,
            "case {:?}: the automated snapshot must be readable by its identifier, got \
             {named:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_cluster_member_keeps_no_automated_backups_of_its_own() {
    let router = emulator();
    expect_ok(
        "the cluster the member belongs to",
        call(
            &router,
            "CreateDBCluster",
            &params![
                "DBClusterIdentifier" => "emu-auto-cluster",
                "Engine" => "aurora-postgresql",
                "MasterUsername" => "emuadmin",
                "MasterUserPassword" => "emu-Secret-99",
                "BackupRetentionPeriod" => 7,
            ],
        )
        .await,
    );
    available_instance(
        &router,
        "emu-auto-cluster-writer",
        &params![
            "Engine" => "aurora-postgresql",
            "DBInstanceClass" => "db.r6g.large",
            "DBClusterIdentifier" => "emu-auto-cluster",
        ],
    )
    .await;

    // "The data is held in the cluster volume", so the automated backups of a cluster member
    // are the CLUSTER's: the member takes no automated snapshot of its own, read twice so a
    // transition that had not settled yet cannot be read as an absence.
    for attempt in ["the first read", "and the second"] {
        let reported = snapshots(
            &router,
            "a cluster member",
            &params![
                "DBInstanceIdentifier" => "emu-auto-cluster-writer",
                "SnapshotType" => "automated",
            ],
        )
        .await;
        assert!(
            reported.is_empty(),
            "{attempt} of a cluster member must report no automated snapshot of its own, \
             got {reported:?}"
        );
    }
}

#[tokio::test]
async fn the_automated_snapshot_follows_the_retention_period_and_the_instance() {
    let router = emulator();
    available_instance(&router, "emu-auto-switch", &standalone("postgres")).await;
    let first = text(
        &automated_snapshots_of(&router, "the first automated snapshot", "emu-auto-switch").await
            [0],
        "DBSnapshotIdentifier",
    );

    // "To disable automated backups, set this value to 0": an instance that keeps none keeps
    // no snapshot of them either, so the one it had taken goes.
    expect_ok(
        "turning automated backups off",
        call(
            &router,
            "ModifyDBInstance",
            &params![
                "DBInstanceIdentifier" => "emu-auto-switch",
                "BackupRetentionPeriod" => 0,
                "ApplyImmediately" => true,
            ],
        )
        .await,
    );
    let read = params![
        "DBInstanceIdentifier" => "emu-auto-switch",
        "SnapshotType" => "automated",
    ];
    poll_until(
        "turning automated backups off",
        "the automated snapshot going with the backups",
        || call(&router, "DescribeDBSnapshots", &read),
        |status, body| status == 200 && subtrees(body, "DBSnapshot").is_empty(),
    )
    .await;

    // Turning them back on takes a new one: the instance keeps automated backups again, so
    // it has the first snapshot of them again.
    expect_ok(
        "turning automated backups back on",
        call(
            &router,
            "ModifyDBInstance",
            &params![
                "DBInstanceIdentifier" => "emu-auto-switch",
                "BackupRetentionPeriod" => 3,
                "ApplyImmediately" => true,
            ],
        )
        .await,
    );
    let retaken = automated_snapshots_of(&router, "after turning them back on", "emu-auto-switch")
        .await;
    assert_eq!(
        text(&retaken[0], "BackupRetentionPeriod"),
        "3",
        "the new automated snapshot must carry the retention the instance keeps now, got {:?}",
        retaken[0]
    );

    // And the snapshot of an instance's automated backups goes with the instance: an
    // instance this service no longer holds keeps no automated backups at all.
    expect_ok(
        "deleting the instance",
        call(
            &router,
            "DeleteDBInstance",
            &params![
                "DBInstanceIdentifier" => "emu-auto-switch",
                "SkipFinalSnapshot" => true,
            ],
        )
        .await,
    );
    let all = params!["SnapshotType" => "automated"];
    let (gone, _) = poll_until(
        "deleting the instance",
        "the deleted instance's automated snapshot going with it",
        || call(&router, "DescribeDBSnapshots", &all),
        |status, body| status == 200 && subtrees(body, "DBSnapshot").is_empty(),
    )
    .await;
    assert!(
        !texts(&gone, "DBSnapshotIdentifier").contains(&first),
        "the first automated snapshot must not come back, got {gone}"
    );
}

#[tokio::test]
async fn an_automated_snapshot_restores_into_a_db_instance_of_its_own() {
    let router = emulator();
    let source =
        available_instance(&router, "emu-auto-restore-src", &standalone("postgres")).await;
    let snapshot = text(
        &automated_snapshots_of(&router, "the snapshot to restore", "emu-auto-restore-src").await
            [0],
        "DBSnapshotIdentifier",
    );

    // An automated snapshot is restorable exactly as a manual one is: it holds a copy of the
    // instance's data, taken by the engine, not a reference to the instance.
    expect_ok(
        "restoring the automated snapshot",
        call(
            &router,
            "RestoreDBInstanceFromDBSnapshot",
            &params![
                "DBInstanceIdentifier" => "emu-auto-restore-dst",
                "DBSnapshotIdentifier" => snapshot.clone(),
            ],
        )
        .await,
    );
    let read = params!["DBInstanceIdentifier" => "emu-auto-restore-dst"];
    let (restored, _) = poll_until(
        "restoring the automated snapshot",
        "the restored instance reaching available",
        || call(&router, "DescribeDBInstances", &read),
        |status, body| {
            status == 200
                && optional_text(body, "DBInstanceStatus").as_deref() == Some("available")
        },
    )
    .await;
    for member in ["Engine", "AllocatedStorage", "MasterUsername"] {
        assert_eq!(
            optional_text(&restored, member),
            optional_text(&source, member),
            "the restored instance must carry the source's {member}, got {restored}"
        );
    }
    assert_eq!(
        text(&restored, "DBInstanceIdentifier"),
        "emu-auto-restore-dst",
        "the restored instance is a DB instance of its own, got {restored}"
    );
}
