// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the two MEMBERS a caller addresses an automated backup by, and for
//! what a DELETE of the source instance leaves behind.
//!
//! Every call goes over the wire through `db_instances::emulator`, as a real awsQuery form
//! POST carrying the SigV4 credential scope the router keys on, so the assertions read the
//! XML the emulator itself answered with.
//!
//! The rules under test:
//!
//! * a member whose value has to be an Amazon Resource Name carries a value with a GRAMMAR,
//!   so a value outside it names no resource of any Region or account and is refused on the
//!   INPUT — `InvalidParameterValue`, before any lookup, worded "The provided ARN (<arn>) is
//!   invalid for this parameter (<parameter>)." — while a WELL-FORMED ARN that names no
//!   stored backup is the operation's own not-found fault, which is a different answer to a
//!   different question;
//! * an instance that was keeping automated backups leaves them behind when it is deleted
//!   (`DBInstanceAutomatedBackup$Status` `retained`, "automated backups for deleted
//!   instances", addressable by the source instance's `DbiResourceId` and by the backups'
//!   own ARN) unless the delete ASKED to remove them.
//!
//! The tables below span what the rules generalise over: the kinds of value a member that
//! must be an ARN can carry (a bare word, a path, an ARN missing fields, an ARN of another
//! vendor, an ARN of this service naming another resource, and a well-formed one), both
//! operations that take the member, and the engine families and retention periods an
//! instance is deleted with.
//!
//! Nothing here waits out a span: every state a transition this service owes leads to is
//! reached by polling a REPORTING call through `lifecycle_poll_loops::poll_until`.

use crate::db_instance_automated_backups::{available_instance, describe_instance};
use crate::db_instances::{call, emulator, expect_error, expect_ok, subtrees, text};
use crate::lifecycle_poll_loops::poll_until;

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The `awsQueryError` code the Query API answers a member value it will not serve with.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The `awsQueryError` code of `DBInstanceAutomatedBackupNotFoundFault`.
const BACKUP_NOT_FOUND: &str = "DBInstanceAutomatedBackupNotFound";

/// The status the automated backup of a DELETED instance reports.
const RETAINED: &str = "retained";

/// One row of the malformed-ARN table: the value the member carries, and what makes it no
/// ARN of this service.
struct ArnCase {
    case: &'static str,
    value: &'static str,
}

/// The values a member that must be an ARN cannot be served, one per way a value fails the
/// form `arn:<partition>:<vendor>:<region>:<account>:<resource>`.
const MALFORMED: &[ArnCase] = &[
    ArnCase {
        case: "a bare word",
        value: "not-an-arn",
    },
    ArnCase {
        case: "a resource id with no ARN around it",
        value: "ab-L2IJCEXJP7XQ7HOJ4SIEXAMPLE",
    },
    ArnCase {
        case: "an ARN missing its resource field",
        value: "arn:aws:rds:us-west-2:000000000000",
    },
    ArnCase {
        case: "an ARN whose resource field names no type",
        value: "arn:aws:rds:us-west-2:000000000000:",
    },
    ArnCase {
        case: "an ARN of another vendor",
        value: "arn:aws:s3:::a-bucket/an-object",
    },
    ArnCase {
        case: "an ARN whose account is not an account id",
        value: "arn:aws:rds:us-west-2:not-an-account:auto-backup:ab-SOMETHING",
    },
    ArnCase {
        case: "a URL",
        value: "https://rds.us-west-2.amazonaws.com/auto-backup/ab-SOMETHING",
    },
    ArnCase {
        case: "an ARN of this service naming another kind of resource",
        value: "arn:aws:rds:us-west-2:000000000000:db:an-instance",
    },
    ArnCase {
        case: "an auto-backup ARN whose id is not an automated backup's",
        value: "arn:aws:rds:us-west-2:000000000000:auto-backup:db-SOMETHING",
    },
    ArnCase {
        case: "an auto-backup ARN naming no Region",
        value: "arn:aws:rds::000000000000:auto-backup:ab-SOMETHING",
    },
    ArnCase {
        case: "an auto-backup ARN naming no account",
        value: "arn:aws:rds:us-west-2::auto-backup:ab-SOMETHING",
    },
    ArnCase {
        case: "an auto-backup ARN whose account is not an account id",
        value: "arn:aws:rds:us-west-2:12345:auto-backup:ab-SOMETHING",
    },
];

/// The sentence the refusal of such a value carries: fixed text, with the value exactly as
/// the request submitted it and the member Amazon RDS names in it as its variable parts.
fn expected_invalid_arn(value: &str) -> String {
    format!(
        "The provided ARN ({value}) is invalid for this parameter \
         (DbInstanceAutomatedBackupsArn)."
    )
}

/// RULE: `DBInstanceAutomatedBackupsArn` is validated as an ARN BEFORE any lookup, by every
/// operation that takes it, and a value that is no ARN of this service is
/// `InvalidParameterValue` naming the value and the member — never a report that found
/// nothing and never a not-found fault.
#[tokio::test]
async fn a_value_that_is_no_arn_is_refused_on_the_input_by_both_operations() {
    let router = emulator();
    for operation in [
        "DescribeDBInstanceAutomatedBackups",
        "DeleteDBInstanceAutomatedBackup",
    ] {
        for row in MALFORMED {
            let case = format!("{operation}: {}", row.case);
            let refused = expect_error(
                &case,
                call(
                    &router,
                    operation,
                    &params!["DBInstanceAutomatedBackupsArn" => row.value],
                )
                .await,
                400,
                INVALID_PARAMETER_VALUE,
            );
            let message = text(&refused, "Message");
            assert_eq!(
                message,
                expected_invalid_arn(row.value),
                "case {case:?}: expected the ARN refusal for {:?}, got {message:?}",
                row.value
            );
        }
    }
}

/// RULE: a WELL-FORMED automated-backups ARN is a lookup, not a validation failure — the
/// one that names no stored backup is `DBInstanceAutomatedBackupNotFound`, and the one that
/// names a retained backup resolves it.
#[tokio::test]
async fn a_well_formed_arn_is_answered_by_the_lookup_it_names() {
    let router = emulator();
    let resource_id = available_instance(
        &router,
        "a retained backup addressed by its ARN",
        "emu-abarn-retained",
        "postgres",
        params!["BackupRetentionPeriod" => 3].as_ref(),
    )
    .await;
    // The ARN the emulator itself publishes for this instance's backup, read off the report
    // rather than written here.
    let reported = expect_ok(
        "the live instance's backup",
        call(
            &router,
            "DescribeDBInstanceAutomatedBackups",
            &params!["DbiResourceId" => &resource_id],
        )
        .await,
    );
    let backups_arn = text(&reported, "DBInstanceAutomatedBackupsArn");

    // An ARN of the same form naming an instance nothing was ever created under: a
    // well-formed value — a resource id of the form this service mints one in — so the
    // answer is the lookup's.
    let unknown = backups_arn.replace(
        resource_id.trim_start_matches("db-"),
        "NOTHINGWASEVERCREATEDUNDER",
    );
    assert_ne!(
        unknown, backups_arn,
        "the unknown ARN must differ from the published one, got {unknown}"
    );
    for operation in [
        "DescribeDBInstanceAutomatedBackups",
        "DeleteDBInstanceAutomatedBackup",
    ] {
        expect_error(
            &format!("{operation}: a well-formed ARN naming no backup"),
            call(
                &router,
                operation,
                &params!["DBInstanceAutomatedBackupsArn" => &unknown],
            )
            .await,
            404,
            BACKUP_NOT_FOUND,
        );
    }

    // And the ARN that names THIS instance's backup resolves it: the delete of a LIVE
    // instance's backup is refused for the STATE it is in, which is a lookup that found it.
    let refused = expect_error(
        "the ARN of a live instance's backup",
        call(
            &router,
            "DeleteDBInstanceAutomatedBackup",
            &params!["DBInstanceAutomatedBackupsArn" => &backups_arn],
        )
        .await,
        400,
        "InvalidDBInstanceAutomatedBackupState",
    );
    assert!(
        text(&refused, "Message").contains("emu-abarn-retained"),
        "the state refusal names the instance the backup belongs to, got {refused}"
    );
}

/// One row of the retention table: how the delete spells `DeleteAutomatedBackups`, and
/// whether a backup outlives the instance.
struct RetentionCase {
    case: &'static str,
    identifier: &'static str,
    engine: &'static str,
    /// The `BackupRetentionPeriod` the create asks for.
    retention: i32,
    /// What the delete sends for `DeleteAutomatedBackups`, or `None` to leave it out.
    delete_automated_backups: Option<bool>,
    /// Whether the deleted instance leaves a `retained` backup behind.
    expects_retained: bool,
}

/// RULE: deleting an instance that was keeping automated backups leaves a `retained` backup
/// behind, addressable until it is explicitly deleted, unless the delete asked to remove
/// them; an instance that kept none leaves none.
#[tokio::test]
async fn a_deleted_instance_leaves_its_automated_backup_unless_the_delete_removed_it() {
    // The rows span what the rule generalises over: the ways the delete may spell its
    // setting (absent, false, true), the retention periods a create may ask for including
    // the zero that keeps no backups at all, and two engine families.
    let table: &[RetentionCase] = &[
        RetentionCase {
            case: "a delete that names no setting",
            identifier: "emu-ret-default",
            engine: "postgres",
            retention: 1,
            delete_automated_backups: None,
            expects_retained: true,
        },
        RetentionCase {
            case: "a delete that asked to keep them",
            identifier: "emu-ret-keep",
            engine: "mysql",
            retention: 7,
            delete_automated_backups: Some(false),
            expects_retained: true,
        },
        RetentionCase {
            case: "a delete that asked to remove them",
            identifier: "emu-ret-drop",
            engine: "mysql",
            retention: 7,
            delete_automated_backups: Some(true),
            expects_retained: false,
        },
        RetentionCase {
            case: "an instance that kept no automated backups",
            identifier: "emu-ret-none",
            engine: "postgres",
            retention: 0,
            delete_automated_backups: None,
            expects_retained: false,
        },
    ];
    for row in table {
        // Each row gets its own emulator, so one row's records cannot carry into the next.
        let router = emulator();
        let resource_id = available_instance(
            &router,
            row.case,
            row.identifier,
            row.engine,
            params!["BackupRetentionPeriod" => row.retention].as_ref(),
        )
        .await;
        let mut delete: Vec<(&str, String)> = params![
            "DBInstanceIdentifier" => row.identifier,
            "SkipFinalSnapshot" => true,
        ]
        .to_vec();
        if let Some(setting) = row.delete_automated_backups {
            delete.push(("DeleteAutomatedBackups", setting.to_string()));
        }
        expect_ok(row.case, call(&router, "DeleteDBInstance", &delete).await);
        // The removal of the instance is owed by the clock: polled for, never waited out.
        poll_until(
            row.case,
            &format!("{} being gone", row.identifier),
            || describe_instance(&router, row.identifier),
            |status, _| status == 404,
        )
        .await;

        let answer = call(
            &router,
            "DescribeDBInstanceAutomatedBackups",
            &params!["DbiResourceId" => &resource_id],
        )
        .await;
        if !row.expects_retained {
            expect_error(row.case, answer, 404, BACKUP_NOT_FOUND);
            continue;
        }
        let reported = expect_ok(row.case, answer);
        let backups = subtrees(&reported, "DBInstanceAutomatedBackup");
        assert_eq!(
            backups.len(),
            1,
            "case {:?}: expected exactly one retained backup, got {reported}",
            row.case
        );
        let status = text(&backups[0], "Status");
        assert_eq!(
            status, RETAINED,
            "case {:?}: expected the backup of a deleted instance to report {RETAINED:?}, \
             got {status:?}",
            row.case
        );
        assert_eq!(
            text(&backups[0], "DBInstanceIdentifier"),
            row.identifier,
            "case {:?}: the retained backup names the instance it was taken of, got \
             {reported}",
            row.case
        );
        // It is the caller's to remove, by the ARN the report publishes for it, and nothing
        // reports it afterwards.
        let backups_arn = text(&backups[0], "DBInstanceAutomatedBackupsArn");
        expect_ok(
            row.case,
            call(
                &router,
                "DeleteDBInstanceAutomatedBackup",
                &params!["DBInstanceAutomatedBackupsArn" => &backups_arn],
            )
            .await,
        );
        expect_error(
            row.case,
            call(
                &router,
                "DescribeDBInstanceAutomatedBackups",
                &params!["DbiResourceId" => &resource_id],
            )
            .await,
            404,
            BACKUP_NOT_FOUND,
        );
    }
}
