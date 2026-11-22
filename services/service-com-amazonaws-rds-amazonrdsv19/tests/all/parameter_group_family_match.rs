// SPDX-License-Identifier: CC0-1.0

//! WHICH DB parameter group a DB instance may be given.
//!
//! THE RULE: a DB parameter group "can be associated with one and only one DB parameter group
//! family, and can be applied only to a DB instance running a database engine and engine version
//! compatible with that DB parameter group family"
//! (`CreateDBParameterGroupMessage$DBParameterGroupFamily`), so every operation that puts a group
//! on an instance refuses a group whose own `DBParameterGroupFamily` is not the family of the
//! engine version that instance runs — HTTP 400 `InvalidParameterCombination`, worded as "The
//! parameter group `<group>` with DBParameterGroupFamily `<its family>` can't be used for this
//! instance. Use a parameter group with DBParameterGroupFamily `<the instance's family>`." The
//! parameters of another family's group are another engine release's settings, which could never
//! apply to the instance, so an accepted create would come up bound to a group reported in sync
//! whose parameters cannot take effect.
//!
//! The rows span what the rule generalises over: a group of ANOTHER ENGINE's family, a group of
//! another MAJOR VERSION of the same engine, a group of the instance's own family (which is
//! admitted), and the two kinds of group a name can resolve to — one a caller created and one of
//! the `default.<family>` groups RDS provides itself. It is asserted over the operations that
//! associate a group with an instance: the create, the modify and the restore.

use crate::db_instances::{call, emulator, expect_error, expect_ok, text};
use crate::lifecycle_poll_loops::poll_until;
use smithy_server_runtime::MultiServiceRouter;

/// Wait until the instance reports `available`, which is the state a modification and a snapshot
/// of it are accepted in.
///
/// The CONDITION is waited for rather than a span: the loop is the one this test target shares,
/// whose deadline is generous beside the span a transition this service owes becomes due after.
async fn available_instance(router: &MultiServiceRouter, case: &str, identifier: &str) {
    let read = [("DBInstanceIdentifier", identifier)];
    poll_until(
        case,
        "the instance",
        || call(router, "DescribeDBInstances", &read),
        |status, body| status == 200 && text(body, "DBInstanceStatus") == "available",
    )
    .await;
}

/// The engine every instance of this suite runs, and the family of the version it resolves to.
const ENGINE: &str = "postgres";

/// The class every instance is ordered in.
const CLASS: &str = "db.t4g.micro";

/// The master user every create carries.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "a-strong-password";

/// One group this suite creates: the name and the `DBParameterGroupFamily` it is created in.
type Group = (&'static str, &'static str);

const GROUPS: &[Group] = &[
    ("emu-pgf-mysql", "mysql8.0"),
    ("emu-pgf-postgres15", "postgres15"),
    ("emu-pgf-postgres16", "postgres16"),
    ("emu-pgf-aurora", "aurora-postgresql16"),
];

/// Create every group of [`GROUPS`].
async fn create_groups(router: &MultiServiceRouter) {
    for (name, family) in GROUPS {
        expect_ok(
            name,
            call(
                router,
                "CreateDBParameterGroup",
                &[
                    ("DBParameterGroupName", *name),
                    ("DBParameterGroupFamily", *family),
                    ("Description", "a group of this suite"),
                ],
            )
            .await,
        );
    }
}

/// One row: the group a request names, the family that group carries, and whether the instance's
/// own family (`postgres16`) can run it.
struct Row {
    case: &'static str,
    group: &'static str,
    family: &'static str,
    admitted: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a group of another engine's family",
        group: "emu-pgf-mysql",
        family: "mysql8.0",
        admitted: false,
    },
    Row {
        case: "a group of another major version of the same engine",
        group: "emu-pgf-postgres15",
        family: "postgres15",
        admitted: false,
    },
    Row {
        case: "a group of the cluster engine that shares the instance engine's name prefix",
        group: "emu-pgf-aurora",
        family: "aurora-postgresql16",
        admitted: false,
    },
    Row {
        case: "one of RDS's own default groups of another engine's family",
        group: "default.mysql8.0",
        family: "mysql8.0",
        admitted: false,
    },
    Row {
        case: "a group of the instance's own family",
        group: "emu-pgf-postgres16",
        family: "postgres16",
        admitted: true,
    },
    Row {
        case: "RDS's own default group of the instance's own family",
        group: "default.postgres16",
        family: "postgres16",
        admitted: true,
    },
];

/// The family the instance's engine version belongs to, which is what a group of this suite is
/// compared against.
const INSTANCE_FAMILY: &str = "postgres16";

/// The engine version every instance is created with: a build of [`INSTANCE_FAMILY`], named
/// exactly so the family the rule compares against is the row's own subject rather than whichever
/// build the engine's default happens to be.
const ENGINE_VERSION: &str = "16.8";

/// The refusal AWS words for a group of another family.
fn mismatch_message(group: &str, carried: &str) -> String {
    format!(
        "The parameter group {group} with DBParameterGroupFamily {carried} can't be used for \
         this instance. Use a parameter group with DBParameterGroupFamily {INSTANCE_FAMILY}."
    )
}

/// `CreateDBInstance` for `identifier` naming `group`.
async fn create_with_group(
    router: &MultiServiceRouter,
    identifier: &str,
    group: &str,
) -> (u16, String) {
    call(
        router,
        "CreateDBInstance",
        &[
            ("DBInstanceIdentifier", identifier),
            ("Engine", ENGINE),
            ("EngineVersion", ENGINE_VERSION),
            ("DBInstanceClass", CLASS),
            ("MasterUsername", MASTER_USER),
            ("MasterUserPassword", MASTER_PASSWORD),
            ("DBParameterGroupName", group),
        ],
    )
    .await
}

#[tokio::test]
async fn a_create_takes_only_a_group_of_the_engine_versions_own_family() {
    let router = emulator();
    create_groups(&router).await;
    for (index, row) in ROWS.iter().enumerate() {
        let identifier = format!("emu-pgf-create-{index}");
        let answer = create_with_group(&router, &identifier, row.group).await;
        if row.admitted {
            let body = expect_ok(row.case, answer);
            let reported = text(&body, "DBParameterGroupName");
            assert_eq!(
                reported, row.group,
                "case {:?}: expected the instance to report the group it was created with, \
                 got {reported:?}",
                row.case
            );
            continue;
        }
        let body = expect_error(row.case, answer, 400, "InvalidParameterCombination");
        let message = text(&body, "Message");
        assert_eq!(
            message,
            mismatch_message(row.group, row.family),
            "case {:?}: expected the cross-family refusal, got {message:?}",
            row.case
        );
        // And nothing was created for it: a refused create leaves no instance behind for a
        // later call to find.
        let (status, found) = call(
            &router,
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", identifier.as_str())],
        )
        .await;
        assert_eq!(
            status, 404,
            "case {:?}: a refused create must leave no DB instance behind, got {status} \
             {found}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_modification_takes_only_a_group_of_the_engine_versions_own_family() {
    let router = emulator();
    create_groups(&router).await;
    let identifier = "emu-pgf-modify";
    expect_ok(
        "the instance under modification",
        create_with_group(&router, identifier, "emu-pgf-postgres16").await,
    );
    available_instance(&router, "the instance under modification", identifier).await;
    for row in ROWS {
        let answer = call(
            &router,
            "ModifyDBInstance",
            &[
                ("DBInstanceIdentifier", identifier),
                ("DBParameterGroupName", row.group),
                ("ApplyImmediately", "true"),
            ],
        )
        .await;
        if row.admitted {
            expect_ok(row.case, answer);
            continue;
        }
        let body = expect_error(row.case, answer, 400, "InvalidParameterCombination");
        let message = text(&body, "Message");
        assert_eq!(
            message,
            mismatch_message(row.group, row.family),
            "case {:?}: expected the cross-family refusal, got {message:?}",
            row.case
        );
    }
    // The refusals changed nothing: the instance still reports a group of its own family.
    let body = expect_ok(
        "the instance after the refusals",
        call(
            &router,
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", identifier)],
        )
        .await,
    );
    let reported = text(&body, "DBParameterGroupName");
    assert!(
        reported.starts_with("emu-pgf-postgres16") || reported.starts_with("default.postgres16"),
        "case the instance after the refusals: expected a group of {INSTANCE_FAMILY}, got \
         {reported:?}"
    );
}

#[tokio::test]
async fn a_restore_takes_only_a_group_of_the_engine_versions_own_family() {
    let router = emulator();
    create_groups(&router).await;
    let source = "emu-pgf-restore-source";
    expect_ok(
        "the instance the snapshot is taken of",
        create_with_group(&router, source, "emu-pgf-postgres16").await,
    );
    available_instance(&router, "the instance the snapshot is taken of", source).await;
    let snapshot = "emu-pgf-restore-snapshot";
    expect_ok(
        "the snapshot",
        call(
            &router,
            "CreateDBSnapshot",
            &[
                ("DBSnapshotIdentifier", snapshot),
                ("DBInstanceIdentifier", source),
            ],
        )
        .await,
    );
    // A restore reads a snapshot that is already there, so the snapshot's own span is waited
    // out — as the CONDITION, not as a sleep.
    let read = [("DBSnapshotIdentifier", snapshot)];
    poll_until(
        "the snapshot",
        "the snapshot",
        || call(&router, "DescribeDBSnapshots", &read),
        |status, body| status == 200 && text(body, "Status") == "available",
    )
    .await;
    for (index, row) in ROWS.iter().enumerate() {
        let identifier = format!("emu-pgf-restored-{index}");
        let answer = call(
            &router,
            "RestoreDBInstanceFromDBSnapshot",
            &[
                ("DBInstanceIdentifier", identifier.as_str()),
                ("DBSnapshotIdentifier", snapshot),
                ("DBParameterGroupName", row.group),
            ],
        )
        .await;
        if row.admitted {
            expect_ok(row.case, answer);
            continue;
        }
        let body = expect_error(row.case, answer, 400, "InvalidParameterCombination");
        let message = text(&body, "Message");
        assert_eq!(
            message,
            mismatch_message(row.group, row.family),
            "case {:?}: expected the cross-family refusal, got {message:?}",
            row.case
        );
    }
}
