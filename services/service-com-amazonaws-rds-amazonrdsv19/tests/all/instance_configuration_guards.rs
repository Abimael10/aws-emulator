// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the DB instance guards and flows that follow from the instance's
//! own CONFIGURATION rather than from its lifecycle state.
//!
//! The rules under test:
//!
//! * deletion protection is a SETTING of the resource a delete names, so a delete of a
//!   protected resource is refused as a parameter combination (HTTP 400,
//!   `InvalidParameterCombination`) and not as a lifecycle state fault — the refusal says
//!   what to change, because no amount of waiting clears it;
//! * every operation that takes a `MasterUserPassword` refuses one outside the engine's
//!   published policy, with the refusal naming the member and the constraint;
//! * an instance that keeps continuous backups reports a `LatestRestorableTime` that
//!   advances with the clock, and one with automated backups off reports none;
//! * `RestoreDBInstanceFromDBSnapshot` checks the target identifier BEFORE it reads the
//!   snapshot, and a restore that is accepted returns the rows the source instance held
//!   when the snapshot was taken.
//!
//! Each table spans what its rule generalises over: the three resource families that carry
//! deletion protection, the two operations that set a master password and the published
//! bounds of each engine's policy, the retention settings that do and do not keep a
//! recovery window, and the restore cases (a free identifier, a live one, a snapshot that
//! does not exist).

use crate::db_instance_engine_endpoint::{available_instance, postgres_login, Transport};
use crate::db_instances::{
    call, emulator, expect_error, expect_ok, optional_text, text, wait_for_settle,
};

/// The master user the instances here are created with, and its test password.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";
/// The `DBName` the instances here are created with.
const DATABASE: &str = "appdb";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The `awsQueryError` code RDS answers a request whose members cannot hold together.
const INVALID_PARAMETER_COMBINATION: &str = "InvalidParameterCombination";

/// The `awsQueryError` code RDS answers a member value it will not take.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

#[tokio::test]
async fn a_delete_of_a_protected_resource_names_the_setting_that_has_to_change() {
    let router = emulator();
    // One row per resource family that carries `DeletionProtection`, because the rule is
    // the setting's and not any one resource's: RDS words the refusal with the resource's
    // own kind and the identifier the request named.
    expect_ok(
        "the protected instance",
        call(
            &router,
            "CreateDBInstance",
            &params![
                "DBInstanceIdentifier" => "emu-prot-instance",
                "Engine" => "postgres",
                "DBInstanceClass" => "db.t3.micro",
                "AllocatedStorage" => 20,
                "MasterUsername" => MASTER_USER,
                "MasterUserPassword" => MASTER_PASSWORD,
                "DeletionProtection" => true,
            ],
        )
        .await,
    );
    expect_ok(
        "the protected cluster",
        call(
            &router,
            "CreateDBCluster",
            &params![
                "DBClusterIdentifier" => "emu-prot-cluster",
                "Engine" => "aurora-postgresql",
                "MasterUsername" => MASTER_USER,
                "MasterUserPassword" => MASTER_PASSWORD,
                "DeletionProtection" => true,
            ],
        )
        .await,
    );
    expect_ok(
        "the protected global cluster",
        call(
            &router,
            "CreateGlobalCluster",
            &params![
                "GlobalClusterIdentifier" => "emu-prot-global",
                "Engine" => "aurora-postgresql",
                "DeletionProtection" => true,
            ],
        )
        .await,
    );
    wait_for_settle();

    for (case, operation, member, identifier, kind, extra) in [
        (
            "a DB instance",
            "DeleteDBInstance",
            "DBInstanceIdentifier",
            "emu-prot-instance",
            // The resource KIND as Amazon RDS names it in this sentence; the recorded
            // answers carry no identifier in it.
            "DB Instance",
            Some(("SkipFinalSnapshot", "true")),
        ),
        (
            "a DB cluster",
            "DeleteDBCluster",
            "DBClusterIdentifier",
            "emu-prot-cluster",
            "Cluster",
            Some(("SkipFinalSnapshot", "true")),
        ),
        (
            "a global cluster",
            "DeleteGlobalCluster",
            "GlobalClusterIdentifier",
            "emu-prot-global",
            "Global Cluster",
            None,
        ),
    ] {
        let mut request = vec![(member, identifier.to_string())];
        if let Some((name, value)) = extra {
            request.push((name, value.to_string()));
        }
        let body = expect_error(
            case,
            call(&router, operation, &request).await,
            400,
            INVALID_PARAMETER_COMBINATION,
        );
        assert_eq!(
            text(&body, "Message"),
            format!(
                "Cannot delete protected {kind}, please disable deletion protection and \
                 try again."
            ),
            "case {case:?}: the refusal must carry the sentence this class is worded \
             with, naming the resource kind, got {:?}",
            text(&body, "Message")
        );
    }

    // And with the setting turned off the delete is accepted: the refusal was the
    // configuration's, so changing it is what clears it.
    expect_ok(
        "turn deletion protection off",
        call(
            &router,
            "ModifyDBInstance",
            &params![
                "DBInstanceIdentifier" => "emu-prot-instance",
                "DeletionProtection" => false,
            ],
        )
        .await,
    );
    wait_for_settle();
    expect_ok(
        "the delete once the setting is off",
        call(
            &router,
            "DeleteDBInstance",
            &params![
                "DBInstanceIdentifier" => "emu-prot-instance",
                "SkipFinalSnapshot" => true,
            ],
        )
        .await,
    );
}

#[tokio::test]
async fn a_master_user_password_outside_the_engines_policy_is_refused_on_every_path() {
    let router = emulator();
    // The bound is the ENGINE's: `MasterUserPassword` publishes one length line per
    // engine, and every one of them has the same lower bound of 8. The table spans the
    // engines whose published upper bound differs and the values at and just outside each
    // published bound, on both operations that set a password.
    for (case, engine, longest) in [
        ("a MySQL instance", "mysql", 41usize),
        ("a MariaDB instance", "mariadb", 41),
        ("a PostgreSQL instance", "postgres", 128),
    ] {
        let identifier = format!("emu-pw-{engine}");
        expect_ok(
            case,
            call(
                &router,
                "CreateDBInstance",
                &params![
                    "DBInstanceIdentifier" => identifier,
                    "Engine" => engine,
                    "DBInstanceClass" => "db.t3.micro",
                    "AllocatedStorage" => 20,
                    "MasterUsername" => MASTER_USER,
                    "MasterUserPassword" => MASTER_PASSWORD,
                ],
            )
            .await,
        );
        wait_for_settle();
        let rows: [(&str, String, Option<String>); 6] = [
            (
                "one character under the lower bound",
                "p".repeat(7),
                Some(
                    "The parameter MasterUserPassword is not a valid password because it \
                     is shorter than 8 characters."
                        .to_string(),
                ),
            ),
            ("the lower bound itself", "p".repeat(8), None),
            ("the upper bound itself", "p".repeat(longest), None),
            (
                "one character over the upper bound",
                "p".repeat(longest + 1),
                Some(format!(
                    "The parameter MasterUserPassword is not a valid password because it \
                     is longer than {longest} characters."
                )),
            ),
            (
                "a forbidden character",
                "pw1-Secret@".to_string(),
                Some(
                    "The parameter MasterUserPassword is not a valid password. Only \
                     printable ASCII characters besides '/', '@', '\"', ' ' may be used."
                        .to_string(),
                ),
            ),
            ("a password well inside the bounds", "pw1-Secret".to_string(), None),
        ];
        for (row, password, refusal) in rows {
            let answer = call(
                &router,
                "ModifyDBInstance",
                &params![
                    "DBInstanceIdentifier" => identifier,
                    "MasterUserPassword" => password,
                    "ApplyImmediately" => true,
                ],
            )
            .await;
            match &refusal {
                Some(message) => {
                    let body = expect_error(
                        &format!("{case}: {row}"),
                        answer,
                        400,
                        INVALID_PARAMETER_VALUE,
                    );
                    assert_eq!(
                        text(&body, "Message"),
                        *message,
                        "case {case:?} / {row:?}: the refusal must name the member and the \
                         constraint, got {:?}",
                        text(&body, "Message")
                    );
                }
                None => {
                    expect_ok(&format!("{case}: {row}"), answer);
                }
            }
            wait_for_settle();
        }
    }
}

#[tokio::test]
async fn an_instance_with_continuous_backups_reports_an_advancing_recovery_point() {
    let router = emulator();
    // `BackupRetentionPeriod`: "To disable automated backups, set this value to 0" — so
    // the table spans the retention values that do and do not keep a recovery window.
    for (case, retention, reports) in [
        ("automated backups off", 0, false),
        ("the smallest retention that keeps them", 1, true),
        ("a longer retention", 7, true),
    ] {
        let identifier = format!("emu-pitr-{retention}");
        expect_ok(
            case,
            call(
                &router,
                "CreateDBInstance",
                &params![
                    "DBInstanceIdentifier" => identifier,
                    "Engine" => "postgres",
                    "DBInstanceClass" => "db.t3.micro",
                    "AllocatedStorage" => 20,
                    "MasterUsername" => MASTER_USER,
                    "MasterUserPassword" => MASTER_PASSWORD,
                    "BackupRetentionPeriod" => retention,
                ],
            )
            .await,
        );
        wait_for_settle();
        let described = expect_ok(
            case,
            call(
                &router,
                "DescribeDBInstances",
                &params!["DBInstanceIdentifier" => identifier],
            )
            .await,
        );
        let first = optional_text(&described, "LatestRestorableTime");
        assert_eq!(
            first.is_some(),
            reports,
            "case {case:?}: a recovery point must be reported exactly while continuous \
             backups are kept, got {first:?}"
        );
        if !reports {
            continue;
        }
        // The recovery point ADVANCES: a second read a moment later is not behind the
        // first, and neither predates the instance.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let again = expect_ok(
            case,
            call(
                &router,
                "DescribeDBInstances",
                &params!["DBInstanceIdentifier" => identifier],
            )
            .await,
        );
        let second = optional_text(&again, "LatestRestorableTime").unwrap_or_default();
        let first = first.unwrap_or_default();
        assert!(
            second >= first,
            "case {case:?}: the recovery point must not go backwards, got {second:?} \
             after {first:?}"
        );
        assert!(
            first >= text(&described, "InstanceCreateTime"),
            "case {case:?}: the recovery point must not predate the instance, got \
             {first:?} against {:?}",
            text(&described, "InstanceCreateTime")
        );
    }
}

#[tokio::test]
async fn a_restore_from_a_snapshot_returns_the_rows_the_source_held() {
    let router = emulator();
    let (address, port) = available_instance(
        &router,
        "emu-restore-source",
        "postgres",
        &params!["EngineVersion" => "16.4"],
    )
    .await;
    // The rows the snapshot has to carry, written through the source instance's own
    // engine endpoint.
    let mut source = postgres_login(
        &address,
        port,
        Transport::Tls,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the master user must be admitted on the source");
    for statement in [
        "CREATE TABLE ledger (id int PRIMARY KEY, note text)",
        "INSERT INTO ledger VALUES (1, 'before the snapshot')",
    ] {
        source
            .query(statement)
            .await
            .unwrap_or_else(|refusal| panic!("{statement:?} must run, got {refusal:?}"));
    }
    expect_ok(
        "the snapshot",
        call(
            &router,
            "CreateDBSnapshot",
            &params![
                "DBInstanceIdentifier" => "emu-restore-source",
                "DBSnapshotIdentifier" => "emu-restore-snap",
            ],
        )
        .await,
    );
    wait_for_settle();
    // Written AFTER the snapshot: a restore must not return it, because a snapshot is the
    // database as it stood when it was taken.
    source
        .query("INSERT INTO ledger VALUES (2, 'after the snapshot')")
        .await
        .expect("the later write runs");

    // The identifier collision comes first, whatever the snapshot is: a restore onto a
    // live instance is refused for the name.
    let collision = expect_error(
        "a restore onto a live instance",
        call(
            &router,
            "RestoreDBInstanceFromDBSnapshot",
            &params![
                "DBInstanceIdentifier" => "emu-restore-source",
                "DBSnapshotIdentifier" => "emu-restore-snap",
            ],
        )
        .await,
        400,
        "DBInstanceAlreadyExists",
    );
    assert_eq!(
        text(&collision, "Message"),
        "DB instance already exists",
        "the refusal must be RDS's own sentence, got {:?}",
        text(&collision, "Message")
    );
    // A snapshot that does not exist is a not-found, under a free identifier.
    expect_error(
        "a snapshot that does not exist",
        call(
            &router,
            "RestoreDBInstanceFromDBSnapshot",
            &params![
                "DBInstanceIdentifier" => "emu-restore-target",
                "DBSnapshotIdentifier" => "emu-restore-absent",
            ],
        )
        .await,
        404,
        "DBSnapshotNotFound",
    );

    // The restore itself: the new instance carries the source's settings and its data.
    let restored = expect_ok(
        "the restore",
        call(
            &router,
            "RestoreDBInstanceFromDBSnapshot",
            &params![
                "DBInstanceIdentifier" => "emu-restore-target",
                "DBSnapshotIdentifier" => "emu-restore-snap",
            ],
        )
        .await,
    );
    assert_eq!(
        text(&restored, "DBInstanceStatus"),
        "creating",
        "a restored instance comes up like any other create, got {restored}"
    );
    assert_eq!(
        (
            text(&restored, "Engine").as_str(),
            text(&restored, "MasterUsername").as_str(),
            text(&restored, "DBInstanceClass").as_str(),
        ),
        ("postgres", MASTER_USER, "db.t3.micro"),
        "the restored instance must carry the source's configuration, got {restored}"
    );
    wait_for_settle();
    let described = expect_ok(
        "the restored instance",
        call(
            &router,
            "DescribeDBInstances",
            &params!["DBInstanceIdentifier" => "emu-restore-target"],
        )
        .await,
    );
    assert_eq!(
        text(&described, "DBInstanceStatus"),
        "available",
        "the restored instance must settle to available, got {described}"
    );
    let endpoint = crate::db_instances::subtrees(&described, "Endpoint")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("the restored instance must publish an endpoint: {described}"));
    let restored_port: u16 = text(&endpoint, "Port")
        .parse()
        .unwrap_or_else(|_| panic!("the endpoint must publish a port: {endpoint}"));
    assert_ne!(
        text(&endpoint, "Address").as_str(),
        "",
        "the restored instance must publish an address, got {endpoint}"
    );
    // The restored instance's master credential is not the source's: a restore installs
    // no password, so the endpoint is reachable and admits nobody until one is set.
    expect_ok(
        "set the restored instance's master password",
        call(
            &router,
            "ModifyDBInstance",
            &params![
                "DBInstanceIdentifier" => "emu-restore-target",
                "MasterUserPassword" => MASTER_PASSWORD,
                "ApplyImmediately" => true,
            ],
        )
        .await,
    );
    wait_for_settle();
    let mut target = postgres_login(
        &text(&endpoint, "Address"),
        restored_port,
        Transport::Tls,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the restored instance must admit its master user");
    let read_back = target
        .query("SELECT id, note FROM ledger ORDER BY id")
        .await
        .expect("the restored data must be readable");
    assert_eq!(
        read_back.rows,
        vec![vec![
            Some("1".to_string()),
            Some("before the snapshot".to_string())
        ]],
        "the restore must return the rows the source held when the snapshot was taken, \
         and only those, got {read_back:?}"
    );
}

/// The notes the point-in-time-restore case writes, in the order it commits them: the row
/// the restored instant must carry and the row it must not.
const PITR_ROWS: &[(i32, &str)] = &[(1, "before the instant"), (2, "after the instant")];

#[tokio::test]
async fn a_point_in_time_restore_returns_the_data_of_the_instant_it_names() {
    let router = emulator();
    let (address, port) = available_instance(
        &router,
        "emu-pitr-source",
        "postgres",
        &params!["BackupRetentionPeriod" => 1],
    )
    .await;
    let mut source = postgres_login(
        &address,
        port,
        Transport::Tls,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the master user must be admitted on the source");
    for statement in [
        "CREATE TABLE ledger (id int PRIMARY KEY, note text)",
        "INSERT INTO ledger VALUES (1, 'before the instant')",
    ] {
        source
            .query(statement)
            .await
            .unwrap_or_else(|refusal| panic!("{statement:?} must run, got {refusal:?}"));
    }
    // The instant to restore to: one the client reads off its own clock between the two
    // commits, which is how a caller names a point in time it has just passed.
    let instant = ::aws_smithy_types::DateTime::from(std::time::SystemTime::now());
    let restore_time = instant
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("an instant renders in the UTC form RestoreTime takes");
    // Committed AFTER that instant, so a restore to it must not bring this row back.
    source
        .query("INSERT INTO ledger VALUES (2, 'after the instant')")
        .await
        .expect("the later write runs");

    // An instance with automated backups OFF has no window to restore in at all, which is
    // what `PointInTimeRestoreNotEnabledFault` reports.
    available_instance(
        &router,
        "emu-pitr-no-backups",
        "postgres",
        &params!["BackupRetentionPeriod" => 0],
    )
    .await;
    // Every way the request can fail to name an instant this instance can be restored to,
    // each refused with the sentence that says WHICH way it is wrong.
    for (case, extra, status, code, message) in [
        (
            "both members naming an instant",
            vec![
                ("RestoreTime", restore_time.clone()),
                ("UseLatestRestorableTime", "true".to_string()),
            ],
            400,
            INVALID_PARAMETER_COMBINATION,
            Some("If UseLatestRestoreTime is true, the RestoreTime parameter must be omitted."),
        ),
        (
            "neither member naming one",
            Vec::new(),
            400,
            INVALID_PARAMETER_COMBINATION,
            Some("If UseLatestRestoreTime is not true, the RestoreTime parameter must be specified."),
        ),
        (
            "a source that does not exist",
            vec![
                ("SourceDBInstanceIdentifier", "emu-pitr-absent".to_string()),
                ("UseLatestRestorableTime", "true".to_string()),
            ],
            404,
            "DBInstanceNotFound",
            None,
        ),
        (
            "a source with automated backups off",
            vec![
                (
                    "SourceDBInstanceIdentifier",
                    "emu-pitr-no-backups".to_string(),
                ),
                ("UseLatestRestorableTime", "true".to_string()),
            ],
            400,
            "PointInTimeRestoreNotEnabled",
            None,
        ),
    ] {
        let mut request: Vec<(&str, String)> = vec![
            (
                "SourceDBInstanceIdentifier",
                "emu-pitr-source".to_string(),
            ),
            (
                "TargetDBInstanceIdentifier",
                format!("emu-pitr-{}", case.replace(' ', "-")),
            ),
        ];
        for (member, value) in extra {
            request.retain(|(name, _)| *name != member);
            request.push((member, value));
        }
        let refused = expect_error(
            case,
            call(&router, "RestoreDBInstanceToPointInTime", &request).await,
            status,
            code,
        );
        if let Some(expected) = message {
            let reported = text(&refused, "Message");
            assert_eq!(
                reported, expected,
                "case {case:?}: expected the refusal {expected:?}, got {reported:?}"
            );
        }
    }
    // An instant EARLIER than the window the retention period covers is refused with the
    // bound itself and the setting it follows from; an instant the instance's published
    // recovery point has not REACHED yet is refused with that point. The point advances on
    // its own, so the instant this test restores to is waited for by reading the instance
    // until the point covers it — never by sleeping for a span.
    let published = poll_until_restorable(&router, "the recovery point", instant).await;
    let too_late = ::aws_smithy_types::DateTime::from_secs(published.secs() + 60);
    let refused = expect_error(
        "an instant the recovery point has not reached",
        call(
            &router,
            "RestoreDBInstanceToPointInTime",
            &params![
                "SourceDBInstanceIdentifier" => "emu-pitr-source",
                "TargetDBInstanceIdentifier" => "emu-pitr-too-late",
                "RestoreTime" => too_late
                    .fmt(::aws_smithy_types::date_time::Format::DateTime)
                    .expect("an instant renders"),
            ],
        )
        .await,
        400,
        INVALID_PARAMETER_VALUE,
    );
    let message = text(&refused, "Message");
    assert!(
        message.starts_with("The specified instance cannot be restored to a time later than ")
            && message.ends_with('.'),
        "the refusal must state the recovery point the instance has reached, got {message:?}"
    );
    let refused = expect_error(
        "an instant before the window",
        call(
            &router,
            "RestoreDBInstanceToPointInTime",
            &params![
                "SourceDBInstanceIdentifier" => "emu-pitr-source",
                "TargetDBInstanceIdentifier" => "emu-pitr-too-early",
                // A quarter-century back, which is outside the one day of automated backups
                // the source keeps whenever this test runs.
                "RestoreTime" => "2000-01-01T00:00:00Z",
            ],
        )
        .await,
        400,
        INVALID_PARAMETER_VALUE,
    );
    let message = text(&refused, "Message");
    assert!(
        message.starts_with("The specified instance cannot be restored to a time earlier than ")
            && message.ends_with("because its backup retention period is set to 1 days."),
        "the refusal must state the window's lower bound and the retention period it \
         follows from, got {message:?}"
    );
    // An instant INSIDE that window but before the instance's own continuous backup covers
    // anything — the instance was created seconds ago — is refused too, with the instant the
    // backup does cover from: a restore must never serve the data of a later commit for an
    // earlier instant.
    let before_the_instance = ::aws_smithy_types::DateTime::from_secs(
        ::aws_smithy_types::DateTime::from(std::time::SystemTime::now()).secs() - 3_600,
    );
    let refused = expect_error(
        "an instant the backup does not cover",
        call(
            &router,
            "RestoreDBInstanceToPointInTime",
            &params![
                "SourceDBInstanceIdentifier" => "emu-pitr-source",
                "TargetDBInstanceIdentifier" => "emu-pitr-uncovered",
                "RestoreTime" => before_the_instance
                    .fmt(::aws_smithy_types::date_time::Format::DateTime)
                    .expect("an instant renders"),
            ],
        )
        .await,
        400,
        INVALID_PARAMETER_VALUE,
    );
    let message = text(&refused, "Message");
    assert!(
        message.starts_with("The specified instance cannot be restored to a time earlier than ")
            && message.ends_with("because its backup retention period is set to 1 days."),
        "the refusal must state the instant the backup covers from, got {message:?}"
    );

    // The restore itself: a new instance holding the data of the instant it names.
    let restored = expect_ok(
        "the restore",
        call(
            &router,
            "RestoreDBInstanceToPointInTime",
            &params![
                "SourceDBInstanceIdentifier" => "emu-pitr-source",
                "TargetDBInstanceIdentifier" => "emu-pitr-target",
                "RestoreTime" => &restore_time,
            ],
        )
        .await,
    );
    assert_eq!(
        text(&restored, "DBInstanceStatus"),
        "creating",
        "a restored instance comes up like any other create, got {restored}"
    );
    assert_eq!(
        (
            text(&restored, "Engine").as_str(),
            text(&restored, "MasterUsername").as_str(),
            text(&restored, "DBInstanceClass").as_str(),
        ),
        ("postgres", MASTER_USER, "db.t3.micro"),
        "the restored instance must carry the source's configuration, got {restored}"
    );
    wait_for_settle();
    let described = poll_until_available(&router, "the restored instance", "emu-pitr-target").await;
    let (restored_address, restored_port) =
        crate::db_instance_engine_endpoint::endpoint_of(&described);
    // The restored instance keeps the SOURCE's master credential, so the same login opens a
    // session on it.
    let mut target = postgres_login(
        &restored_address,
        restored_port,
        Transport::Tls,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the restored instance must admit the source's master user");
    let read_back = target
        .query("SELECT id, note FROM ledger ORDER BY id")
        .await
        .expect("the restored data must be readable");
    assert_eq!(
        read_back.rows,
        vec![vec![
            Some(PITR_ROWS[0].0.to_string()),
            Some(PITR_ROWS[0].1.to_string())
        ]],
        "the restore must return the rows committed before the instant it names, and only \
         those, got {read_back:?}"
    );

    // And `UseLatestRestorableTime` restores the LATEST instant instead, which is the data
    // the source holds now — both rows.
    expect_ok(
        "the latest restorable time",
        call(
            &router,
            "RestoreDBInstanceToPointInTime",
            &params![
                "SourceDBInstanceIdentifier" => "emu-pitr-source",
                "TargetDBInstanceIdentifier" => "emu-pitr-latest",
                "UseLatestRestorableTime" => true,
            ],
        )
        .await,
    );
    wait_for_settle();
    let described = poll_until_available(&router, "the latest restore", "emu-pitr-latest").await;
    let (latest_address, latest_port) =
        crate::db_instance_engine_endpoint::endpoint_of(&described);
    let mut latest = postgres_login(
        &latest_address,
        latest_port,
        Transport::Tls,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the latest restore must admit the source's master user");
    let read_back = latest
        .query("SELECT id, note FROM ledger ORDER BY id")
        .await
        .expect("the restored data must be readable");
    let expected: Vec<Vec<Option<String>>> = PITR_ROWS
        .iter()
        .map(|(id, note)| vec![Some(id.to_string()), Some((*note).to_string())])
        .collect();
    assert_eq!(
        read_back.rows, expected,
        "a restore to the latest restorable time must return every committed row, got \
         {read_back:?}"
    );
}

/// The one `<DBInstance>` `DescribeDBInstances` reports for `identifier`.
async fn describe_instance(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> String {
    let body = expect_ok(
        case,
        call(
            router,
            "DescribeDBInstances",
            &params!["DBInstanceIdentifier" => identifier],
        )
        .await,
    );
    crate::db_instances::the_instance(case, &body)
}

/// Read `identifier` until it reports itself `available`, and answer the instance it reported
/// then.
///
/// A restored instance comes up on its own, so what a client waits for is the CONDITION —
/// never a fixed span — under a deadline far longer than the span this service owes that
/// transition in.
async fn poll_until_available(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> String {
    let deadline = std::time::SystemTime::now() + std::time::Duration::from_secs(120);
    loop {
        let read_back = describe_instance(router, case, identifier).await;
        if optional_text(&read_back, "DBInstanceStatus").as_deref() == Some("available") {
            return read_back;
        }
        assert!(
            std::time::SystemTime::now() < deadline,
            "case {case:?}: the restored instance must settle to available, last read \
             {read_back}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Read `emu-pitr-source` until the recovery point it publishes COVERS `instant`, and answer
/// the point it published then.
///
/// `LatestRestorableTime` advances on its own, so what a caller that has just recorded an
/// instant waits for is that point reaching it — the condition, read again until it holds,
/// under a deadline far longer than the span the point advances in.
async fn poll_until_restorable(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    instant: ::aws_smithy_types::DateTime,
) -> ::aws_smithy_types::DateTime {
    let deadline = std::time::SystemTime::now() + std::time::Duration::from_secs(120);
    loop {
        let read_back = describe_instance(router, case, "emu-pitr-source").await;
        let published = optional_text(&read_back, "LatestRestorableTime").unwrap_or_default();
        if let Ok(published) = ::aws_smithy_types::DateTime::from_str(
            &published,
            ::aws_smithy_types::date_time::Format::DateTime,
        ) {
            // The published point advances in whole-second steps, so it COVERS the recorded
            // instant once it has passed the second that instant fell in.
            if published.secs() > instant.secs() {
                return published;
            }
        }
        assert!(
            std::time::SystemTime::now() < deadline,
            "case {case:?}: the published recovery point must reach the instant the client \
             recorded, last read {read_back}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// What each engine writes a statement it RAN into, and the configuration that turns that
/// logging on: `(case, engine, parameter, value, log file)`.
///
/// Source: each engine's own statement logging. PostgreSQL writes `statement:` lines into the
/// server log its `log_statement` names the classes of; the MySQL family writes every
/// statement into the GENERAL QUERY LOG, a file of its own, while `general_log` is on. The
/// file names are the ones `DescribeDBLogFiles` publishes
/// (`crate::state::engine_log::log_file_name`, `MYSQL_GENERAL_LOG_FILE`).
const STATEMENT_LOG_CASES: &[(&str, &str, &str, &str, &str)] = &[
    (
        "the PostgreSQL server log",
        "postgres",
        "log_statement",
        "all",
        "error/postgresql.log",
    ),
    (
        "the MySQL general query log",
        "mysql",
        "general_log",
        "1",
        "general/mysql-general.log",
    ),
];

#[tokio::test]
async fn the_log_file_carries_the_statements_the_engine_executed() {
    for (index, (case, engine, parameter, value, log_file)) in
        STATEMENT_LOG_CASES.iter().enumerate()
    {
        // One store per row: each engine's parameter group belongs to its own engine family,
        // which is the family of the version the instance is created on — a group of another
        // family cannot be applied to it.
        let router = emulator();
        let group = format!("emu-stmt-log-{index}");
        let identifier = format!("emu-stmt-log-{index}");
        let family = if *engine == "postgres" {
            "postgres18"
        } else {
            "mysql8.4"
        };
        expect_ok(
            case,
            call(
                &router,
                "CreateDBParameterGroup",
                &params![
                    "DBParameterGroupName" => &group,
                    "DBParameterGroupFamily" => family,
                    "Description" => "statement logging on",
                ],
            )
            .await,
        );
        expect_ok(
            case,
            call(
                &router,
                "ModifyDBParameterGroup",
                &params![
                    "DBParameterGroupName" => &group,
                    "Parameters.Parameter.1.ParameterName" => parameter,
                    "Parameters.Parameter.1.ParameterValue" => value,
                    "Parameters.Parameter.1.ApplyMethod" => "immediate",
                ],
            )
            .await,
        );
        let (address, port) = available_instance(
            &router,
            &identifier,
            engine,
            &params!["DBParameterGroupName" => &group],
        )
        .await;
        // A statement nobody else would write, so what the log carries can only be this
        // session's.
        let statement = format!("SELECT {} AS marked", 40_000 + index);
        if *engine == "postgres" {
            let mut session = postgres_login(
                &address,
                port,
                Transport::Tls,
                MASTER_USER,
                MASTER_PASSWORD,
                Some(DATABASE),
            )
            .await
            .expect("the master user must be admitted");
            session
                .query(&statement)
                .await
                .expect("the marked statement runs");
        } else {
            let mut session = crate::db_instance_engine_endpoint::mysql_login(
                &address,
                port,
                Transport::Plaintext,
                MASTER_USER,
                MASTER_PASSWORD,
                Some(DATABASE),
            )
            .await
            .expect("the master user must be admitted");
            session
                .query(&statement)
                .await
                .expect("the marked statement runs");
        }

        // The file the engine wrote it to is one the instance HAS, and the portion a client
        // downloads carries the statement.
        let listed = expect_ok(
            case,
            call(
                &router,
                "DescribeDBLogFiles",
                &params!["DBInstanceIdentifier" => &identifier],
            )
            .await,
        );
        let files = crate::db_instances::texts(&listed, "LogFileName");
        assert!(
            files.contains(&(*log_file).to_string()),
            "case {case:?}: expected {log_file:?} among the instance's log files, got \
             {files:?}"
        );
        let downloaded = expect_ok(
            case,
            call(
                &router,
                "DownloadDBLogFilePortion",
                &params![
                    "DBInstanceIdentifier" => &identifier,
                    "LogFileName" => log_file,
                ],
            )
            .await,
        );
        let data = text(&downloaded, "LogFileData");
        assert!(
            data.contains(&statement),
            "case {case:?}: expected the log to carry {statement:?}, got {data:?}"
        );
    }
}

#[tokio::test]
async fn a_statement_is_not_logged_while_the_engine_logs_none() {
    let router = emulator();
    // No parameter group of its own: the engine's published default for `log_statement` is
    // `none`, so a statement is not written to the server log at all.
    let (address, port) = available_instance(&router, "emu-stmt-quiet", "postgres", &[]).await;
    let mut session = postgres_login(
        &address,
        port,
        Transport::Tls,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the master user must be admitted");
    let statement = "SELECT 90001 AS unlogged";
    session.query(statement).await.expect("the statement runs");
    let downloaded = expect_ok(
        "the quiet log",
        call(
            &router,
            "DownloadDBLogFilePortion",
            &params![
                "DBInstanceIdentifier" => "emu-stmt-quiet",
                "LogFileName" => "error/postgresql.log",
            ],
        )
        .await,
    );
    let data = text(&downloaded, "LogFileData");
    assert!(
        !data.contains(statement),
        "a server whose statement logging is off must not write the statement to its log, \
         got {data:?}"
    );
    // What the engine DID write is still there: a statement it refused is an `ERROR` line of
    // the server log at the default `log_min_messages`.
    let refusal = session
        .query("SELECT * FROM nothing_was_created_here")
        .await
        .expect_err("the engine refuses a table that does not exist");
    let downloaded = expect_ok(
        "the refused statement",
        call(
            &router,
            "DownloadDBLogFilePortion",
            &params![
                "DBInstanceIdentifier" => "emu-stmt-quiet",
                "LogFileName" => "error/postgresql.log",
            ],
        )
        .await,
    );
    let data = text(&downloaded, "LogFileData");
    assert!(
        data.contains("ERROR") && data.contains("nothing_was_created_here"),
        "the engine's own refusal must be a line of its log, got {data:?} for {refusal:?}"
    );
}
