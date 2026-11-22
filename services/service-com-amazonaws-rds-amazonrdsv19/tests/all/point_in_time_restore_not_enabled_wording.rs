// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for HOW a point-in-time restore refuses a source DB instance that has no
//! instant to restore to.
//!
//! The rule under test: `RestoreDBInstanceToPointInTime` answers
//! `PointInTimeRestoreNotEnabled` (HTTP 400) for a source with no restorable window, and the
//! sentence it answers with states WHY there is none — a `BackupRetentionPeriod` of zero keeps
//! no continuous backup at all, so the refusal names the operation that turns backups on,
//! while a non-zero period whose backups cover nothing yet is refused with the period in
//! effect and the fact that no backup is available yet. The two reasons ask the caller for
//! different things (change a setting, or wait), so one sentence cannot serve both.
//!
//! The table spans what the rule generalises over: the retention periods that keep backups,
//! at the smallest and the largest value the member accepts and one in between, a restore
//! whose own request carries a DIFFERENT retention period for the instance it would mint (the
//! sentence states the SOURCE's, which is the state the window follows from), and the period
//! of zero that keeps none. The zero case is then read again once the instance has come up, to
//! show the sentence follows from the setting and not from the status.

use crate::db_instances::{call, emulator, expect_error, expect_ok, optional_text, text};

/// The master user the instances here are created with, and its test password.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The query parameters of one call, owned so a table row can hold them.
type Params = Vec<(&'static str, String)>;

/// The status a DB instance reports while it is still coming up, which is the state in which
/// its automated backups cover nothing yet.
const CREATING: &str = "creating";

/// The sentence Amazon RDS refuses a request that needs a DB instance's automated backups and
/// finds them turned off with: one fixed text, naming the operation that turns them on.
const BACKUPS_OFF: &str = "Automated backups are not enabled for this database instance. \
                           To enable automated backups, use ModifyDBInstance to set the \
                           backup retention period to a non-zero value.";

/// The sentence it refuses a restore of an instance whose backups ARE on and have covered
/// nothing yet with: the retention period in effect is its only variable part.
fn no_backups_yet(backup_retention_period: i32) -> String {
    format!(
        "The backup retention period is set to {backup_retention_period}, but no backups are \
         available yet. Most likely this is because backup retention has only recently been \
         changed from zero to a non-zero value."
    )
}

#[tokio::test]
async fn a_point_in_time_restore_states_why_the_source_has_no_restorable_instant() {
    let router = emulator();
    // The retention periods are the bounds `BackupRetentionPeriod` publishes ("must be a value
    // from 0 to 35") and a value inside them, so the table carries the smallest period that
    // keeps backups, the largest, one in between and the one that keeps none.
    let rows: Vec<(&str, &str, i32, Params, String)> = vec![
        (
            "the smallest period that keeps backups",
            "emu-pitrw-one",
            1,
            Vec::new(),
            no_backups_yet(1),
        ),
        (
            "a week of backups",
            "emu-pitrw-week",
            7,
            Vec::new(),
            no_backups_yet(7),
        ),
        (
            "the largest period the member accepts",
            "emu-pitrw-max",
            35,
            Vec::new(),
            no_backups_yet(35),
        ),
        (
            // The restore asks for another retention period for the instance it would mint:
            // the window it reads is still the SOURCE's, so the sentence states the source's
            // period and never the one the request carried.
            "a restore that asks for a different period on the target",
            "emu-pitrw-source-period",
            3,
            params!["BackupRetentionPeriod" => 20].to_vec(),
            no_backups_yet(3),
        ),
        (
            "automated backups off",
            "emu-pitrw-off",
            0,
            Vec::new(),
            BACKUPS_OFF.to_string(),
        ),
    ];
    for (case, source, retention, restore_extra, expected) in &rows {
        // The source is read straight back from its own create answer: an instance that has
        // just been created is still coming up, which is the state in which a non-zero
        // retention period covers no instant yet.
        let created = expect_ok(
            case,
            call(
                &router,
                "CreateDBInstance",
                &params![
                    "DBInstanceIdentifier" => source,
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
        let status = text(&created, "DBInstanceStatus");
        assert_eq!(
            status, CREATING,
            "case {case:?}: the source must still be coming up when the restore is called, \
             got {status:?}"
        );
        let mut request: Params = params![
            "SourceDBInstanceIdentifier" => source,
            "TargetDBInstanceIdentifier" => format!("{source}-restored"),
            "UseLatestRestorableTime" => true,
        ]
        .to_vec();
        for (member, value) in restore_extra {
            request.retain(|(name, _)| name != member);
            request.push((member, value.clone()));
        }
        let refused = expect_error(
            case,
            call(&router, "RestoreDBInstanceToPointInTime", &request).await,
            400,
            "PointInTimeRestoreNotEnabled",
        );
        let reported = text(&refused, "Message");
        assert_eq!(
            &reported, expected,
            "case {case:?}: expected the refusal {expected:?}, got {reported:?}"
        );
    }

    // The instance with backups OFF has no restorable instant however long the caller waits,
    // so the same sentence answers a restore of it once it is up: the condition the test waits
    // for is the instance reporting itself available, read again until it does.
    poll_until_available(&router, "the instance with backups off", "emu-pitrw-off").await;
    let refused = expect_error(
        "automated backups off, once the instance is up",
        call(
            &router,
            "RestoreDBInstanceToPointInTime",
            &params![
                "SourceDBInstanceIdentifier" => "emu-pitrw-off",
                "TargetDBInstanceIdentifier" => "emu-pitrw-off-restored-later",
                "UseLatestRestorableTime" => true,
            ],
        )
        .await,
        400,
        "PointInTimeRestoreNotEnabled",
    );
    let reported = text(&refused, "Message");
    assert_eq!(
        reported, BACKUPS_OFF,
        "case \"automated backups off, once the instance is up\": the refusal must follow from \
         the retention setting and not from the status, got {reported:?}"
    );
    // And nothing was minted by any of the refusals: a refused restore leaves no instance
    // behind for the target identifier it named.
    for (case, source, ..) in &rows {
        let (status, body) = call(
            &router,
            "DescribeDBInstances",
            &params!["DBInstanceIdentifier" => format!("{source}-restored")],
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: a refused restore must mint no instance, got {body}"
        );
    }
}

/// Read `identifier` until it reports itself `available`.
///
/// An instance comes up on its own, so what the test waits for is the CONDITION — never a
/// fixed span — under a deadline far longer than the span this service owes that transition
/// in.
async fn poll_until_available(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    identifier: &str,
) {
    let deadline = std::time::SystemTime::now() + std::time::Duration::from_secs(120);
    loop {
        let body = expect_ok(
            case,
            call(
                router,
                "DescribeDBInstances",
                &params!["DBInstanceIdentifier" => identifier],
            )
            .await,
        );
        let read_back = crate::db_instances::the_instance(case, &body);
        if optional_text(&read_back, "DBInstanceStatus").as_deref() == Some("available") {
            return;
        }
        assert!(
            std::time::SystemTime::now() < deadline,
            "case {case:?}: the instance must settle to available, last read {read_back}"
        );
        ::tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}
