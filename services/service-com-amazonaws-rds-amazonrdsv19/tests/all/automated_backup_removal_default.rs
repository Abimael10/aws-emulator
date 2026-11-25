// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the LIFECYCLE of a DB instance's automated backup across the delete
//! that ends the instance: what `DescribeDBInstanceAutomatedBackups` reports while the
//! instance lives, and what it reports once the instance is gone.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper
//! this test target has, as a real awsQuery form POST carrying the SigV4 credential scope
//! the router keys on. The assertions read the XML the emulator answers with, so a handler
//! that computed nothing could not pass them.
//!
//! The rule under test: an instance that keeps automated backups ("to disable automated
//! backups, set this value to 0") has exactly one automated backup while it lives, reported
//! `active` — "automated backups for current instances" — and that backup survives the
//! instance's delete, as `retained`, ONLY where the delete asked to keep it by sending
//! `DeleteAutomatedBackups` false, because the member's documented default is the opposite:
//! "the default is to remove automated backups immediately after the DB instance is
//! deleted" (`smithy_query(query="doc",shape="DeleteDBInstanceMessage",
//! member="DeleteAutomatedBackups")`). So a delete that names no setting leaves nothing for
//! the report to find, whichever member a later request narrows the report by, and the
//! refusal is the modelled `DBInstanceAutomatedBackupNotFoundFault`.
//!
//! The rows span what the rule generalises over: the boolean spellings this Query wire
//! accepts for the member (`true`, `false`, `1`, `0`, and the member left out), the engine
//! families an instance runs, the retention periods a create may ask for including the zero
//! that keeps no backup at all, and both ways the delete may dispose of a final snapshot —
//! skipping it and taking one — since the member's default is a setting of the backups
//! rather than of the snapshot.
//!
//! Nothing here waits out a span: the removal of the instance is a transition this service
//! owes, reached by polling a REPORTING call through `lifecycle_poll_loops::poll_until`.

use service_com_amazonaws_rds_amazonrdsv19::state::describe_automated_backup_not_found_message;

use crate::db_instance_automated_backups::{available_instance, describe_backups, describe_instance};
use crate::db_instances::{call, emulator, expect_error, expect_ok, subtrees, text};
use crate::lifecycle_poll_loops::poll_until;

/// The `awsQueryError` code of `DBInstanceAutomatedBackupNotFoundFault`, as the model
/// publishes it.
const NOT_FOUND: &str = "DBInstanceAutomatedBackupNotFound";

/// The status the automated backup of an instance this store still holds reports.
const ACTIVE: &str = "active";

/// The status the automated backup of a DELETED instance the delete kept reports.
const RETAINED: &str = "retained";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// One row: an instance, how its delete spells `DeleteAutomatedBackups`, and whether a
/// backup outlives it.
struct Row {
    case: &'static str,
    identifier: &'static str,
    engine: &'static str,
    /// The `BackupRetentionPeriod` the create asks for.
    retention: i32,
    /// What the delete sends for `DeleteAutomatedBackups`, or `None` to leave the member
    /// out. The spellings are the lexical space of a boolean on this wire — `true`, `false`,
    /// `1` and `0`.
    setting: Option<&'static str>,
    /// Whether the delete takes a final snapshot instead of skipping it.
    final_snapshot: Option<&'static str>,
    /// Whether a `retained` backup is reported once the instance is gone.
    expects_retained: bool,
}

#[tokio::test]
async fn a_delete_removes_the_automated_backup_unless_it_asked_to_keep_it() {
    let table: &[Row] = &[
        Row {
            case: "a delete that names no setting, which takes the documented default",
            identifier: "emu-abrd-default",
            engine: "mysql",
            retention: 1,
            setting: None,
            final_snapshot: None,
            expects_retained: false,
        },
        Row {
            case: "a delete that asked to keep the backups",
            identifier: "emu-abrd-keep",
            engine: "postgres",
            retention: 7,
            setting: Some("false"),
            final_snapshot: None,
            expects_retained: true,
        },
        Row {
            case: "a delete that asked to keep them with the numeral spelling of false",
            identifier: "emu-abrd-keep-numeral",
            engine: "mariadb",
            retention: 35,
            setting: Some("0"),
            final_snapshot: None,
            expects_retained: true,
        },
        Row {
            case: "a delete that asked to remove them",
            identifier: "emu-abrd-drop",
            engine: "mysql",
            retention: 2,
            setting: Some("true"),
            final_snapshot: None,
            expects_retained: false,
        },
        Row {
            case: "a delete that asked to remove them with the numeral spelling of true",
            identifier: "emu-abrd-drop-numeral",
            engine: "postgres",
            retention: 1,
            setting: Some("1"),
            final_snapshot: None,
            expects_retained: false,
        },
        Row {
            case: "a default delete that takes a final snapshot instead of skipping it",
            identifier: "emu-abrd-final-snapshot",
            engine: "mysql",
            retention: 3,
            setting: None,
            final_snapshot: Some("emu-abrd-final"),
            expects_retained: false,
        },
        Row {
            case: "a delete that asked to keep them while taking a final snapshot",
            identifier: "emu-abrd-keep-final-snapshot",
            engine: "postgres",
            retention: 3,
            setting: Some("false"),
            final_snapshot: Some("emu-abrd-keep-final"),
            expects_retained: true,
        },
        Row {
            case: "an instance that kept no automated backups at all",
            identifier: "emu-abrd-no-backups",
            engine: "postgres",
            retention: 0,
            setting: Some("false"),
            final_snapshot: None,
            expects_retained: false,
        },
    ];

    for row in table {
        // Each row gets its own emulator, so one row's records cannot carry into the next.
        let router = emulator();
        let case = row.case;
        let resource_id = available_instance(
            &router,
            case,
            row.identifier,
            row.engine,
            params!["BackupRetentionPeriod" => row.retention].as_ref(),
        )
        .await;

        // While the instance lives, an instance that keeps backups has exactly one, reported
        // `active`; one that keeps none has none to report, which is the refusal.
        let live =
            describe_backups(&router, &params!["DBInstanceIdentifier" => row.identifier]).await;
        if row.retention > 0 {
            let reported = expect_ok(case, live);
            let backups = subtrees(&reported, "DBInstanceAutomatedBackup");
            assert_eq!(
                backups.len(),
                1,
                "case {case:?}: expected exactly one automated backup for the live instance, \
                 got {reported}"
            );
            assert_eq!(
                text(&backups[0], "Status"),
                ACTIVE,
                "case {case:?}: a live instance's backup must report {ACTIVE:?}, got {reported}"
            );
            assert_eq!(
                text(&backups[0], "DbiResourceId"),
                resource_id,
                "case {case:?}: the backup must name the source instance's resource id, got \
                 {reported}"
            );
        } else {
            let refused = expect_error(case, live, 404, NOT_FOUND);
            assert_eq!(
                text(&refused, "Message"),
                describe_automated_backup_not_found_message(row.identifier),
                "case {case:?}: expected the sentence this refusal carries, got {refused}"
            );
        }

        let mut delete: Vec<(&str, String)> =
            params!["DBInstanceIdentifier" => row.identifier].to_vec();
        match row.final_snapshot {
            Some(snapshot) => delete.push(("FinalDBSnapshotIdentifier", snapshot.to_string())),
            None => delete.push(("SkipFinalSnapshot", true.to_string())),
        }
        if let Some(setting) = row.setting {
            delete.push(("DeleteAutomatedBackups", setting.to_string()));
        }
        expect_ok(case, call(&router, "DeleteDBInstance", &delete).await);
        // The removal of the instance is owed by the clock: polled for, never waited out.
        poll_until(
            case,
            &format!("{} being gone", row.identifier),
            || describe_instance(&router, row.identifier),
            |status, _| status == 404,
        )
        .await;

        // Once the instance is gone, the backup is there only where the delete asked for it.
        // Read by BOTH members that narrow the report to one backup, so a backup that was
        // removed cannot be a backup one member fails to resolve.
        let by_identifier =
            describe_backups(&router, &params!["DBInstanceIdentifier" => row.identifier]).await;
        let by_resource_id =
            describe_backups(&router, &params!["DbiResourceId" => &resource_id]).await;
        if row.expects_retained {
            for (named, answer) in [
                ("DBInstanceIdentifier", by_identifier),
                ("DbiResourceId", by_resource_id),
            ] {
                let reported = expect_ok(case, answer);
                let backups = subtrees(&reported, "DBInstanceAutomatedBackup");
                assert_eq!(
                    backups.len(),
                    1,
                    "case {case:?}: expected the retained backup read by {named}, got {reported}"
                );
                assert_eq!(
                    text(&backups[0], "Status"),
                    RETAINED,
                    "case {case:?}: a kept backup of a deleted instance must report \
                     {RETAINED:?} read by {named}, got {reported}"
                );
                assert_eq!(
                    text(&backups[0], "DbiResourceId"),
                    resource_id,
                    "case {case:?}: the retained backup must keep the source's resource id \
                     read by {named}, got {reported}"
                );
            }
            continue;
        }
        let refused = expect_error(case, by_identifier, 404, NOT_FOUND);
        assert_eq!(
            text(&refused, "Message"),
            describe_automated_backup_not_found_message(row.identifier),
            "case {case:?}: expected the sentence this refusal carries, got {refused}"
        );
        let refused = expect_error(case, by_resource_id, 404, NOT_FOUND);
        assert_eq!(
            text(&refused, "Message"),
            describe_automated_backup_not_found_message(&resource_id),
            "case {case:?}: expected the sentence this refusal carries, got {refused}"
        );
        // And the region holds no automated backup at all: the removal is of the RECORD, not
        // of one member's ability to resolve it.
        let reported = expect_ok(case, describe_backups(&router, &[]).await);
        assert!(
            subtrees(&reported, "DBInstanceAutomatedBackup").is_empty(),
            "case {case:?}: expected the region to report no automated backup, got {reported}"
        );
    }
}
