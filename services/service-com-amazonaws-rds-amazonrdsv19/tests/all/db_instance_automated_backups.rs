// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the AUTOMATED BACKUP of a DB instance: what
//! `DescribeDBInstanceAutomatedBackups` reports, what `DeleteDBInstance` keeps or drops,
//! and what `DeleteDBInstanceAutomatedBackup` removes.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper
//! this test target has, as a real awsQuery form POST carrying the SigV4 credential scope
//! the router keys on. The assertions read the XML the emulator answers with, so a handler
//! that computed nothing could not pass them.
//!
//! The rule under test: a DB instance has an automated backup exactly while automated
//! backups are being kept for it — which `BackupRetentionPeriod` decides, "to disable
//! automated backups, set this value to 0", and which a member of a DB cluster never has
//! one of, because the cluster holds the data. That backup is reported `active` while the
//! instance is still there ("automated backups for current instances"), it OUTLIVES the
//! instance as `retained` ("automated backups for deleted instances") when the delete asked
//! to keep it (`DeleteAutomatedBackups` false), and it goes with the instance when the
//! delete did not — the documented default. A retained backup is found by every reference
//! the report accepts (`DbiResourceId`, `DBInstanceIdentifier`, the backups' own ARN, the
//! `status` / `db-instance-id` / `dbi-resource-id` filters), `DeleteDBInstanceAutomatedBackup`
//! removes it and nothing reports it afterwards, the backup of a LIVE instance refuses that
//! delete with `InvalidDBInstanceAutomatedBackupState`, and a reference that resolves to no
//! backup is `DBInstanceAutomatedBackupNotFound` (HTTP 404) worded "Automated backups could
//! not be found for: <value>".
//!
//! The tables below span what the rule generalises over: the engine families an instance
//! runs (the two whose native wire this emulator speaks, one it does not, and MariaDB), the
//! retention periods a create may ask for including zero, each way the delete may spell its
//! `DeleteAutomatedBackups` setting, a cluster member, each reference the report accepts,
//! each filter it documents, and the page sizes a report is read in.
//!
//! Nothing here waits out a span: every state a transition this service owes leads to is
//! reached by polling a REPORTING call through `lifecycle_poll_loops::poll_until`.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, emulator, expect_error, expect_ok, optional_text, subtrees, text, texts, REGION,
};
use crate::lifecycle_poll_loops::poll_until;

/// The account id every generated emulator presents, for the ARN forms the rows below name
/// a resource by.
const ACCOUNT: &str = "000000000000";

/// The `awsQueryError` code of `DBInstanceAutomatedBackupNotFoundFault`, as the model
/// publishes it.
const NOT_FOUND: &str = "DBInstanceAutomatedBackupNotFound";

/// The `awsQueryError` code of `InvalidDBInstanceAutomatedBackupStateFault`.
const INVALID_STATE: &str = "InvalidDBInstanceAutomatedBackupState";

/// The status the automated backup of an instance this store still holds reports.
const ACTIVE: &str = "active";

/// The status the automated backup of a DELETED instance reports.
const RETAINED: &str = "retained";

/// The master user every instance below is created with, and its password: test
/// credentials, and the only place in this suite they appear.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The sentence every not-found refusal of an automated backup carries: fixed text with
/// the value the request named as its only variable part.
fn expected_not_found(requested: &str) -> String {
    format!("Automated backups could not be found for: {requested}")
}

/// `CreateDBInstance` for one row, and the `DbiResourceId` the created instance reports.
async fn available_instance(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    engine: &str,
    extra: &[(&str, String)],
) -> String {
    let mut request: Vec<(&str, String)> = params![
        "DBInstanceIdentifier" => identifier,
        "Engine" => engine,
        "DBInstanceClass" => "db.t3.micro",
        "AllocatedStorage" => 20,
        "MasterUsername" => MASTER_USER,
        "MasterUserPassword" => MASTER_PASSWORD,
    ]
    .to_vec();
    request.extend(extra.iter().cloned());
    expect_ok(case, call(router, "CreateDBInstance", &request).await);
    // The instance comes up on its own, so what this waits for is the CONDITION a client
    // waits for before it reads the instance's automated backup.
    let (described, _) = poll_until(
        case,
        &format!("{identifier} reaching available"),
        || describe_instance(router, identifier),
        |status, body| {
            status == 200 && optional_text(body, "DBInstanceStatus").as_deref() == Some("available")
        },
    )
    .await;
    text(&described, "DbiResourceId")
}

/// `DescribeDBInstances` for one instance — the REPORTING call the polls above make.
async fn describe_instance(router: &MultiServiceRouter, identifier: &str) -> (u16, String) {
    call(
        router,
        "DescribeDBInstances",
        &params!["DBInstanceIdentifier" => identifier],
    )
    .await
}

/// `DescribeDBInstanceAutomatedBackups` with the members one case names.
async fn describe_backups(
    router: &MultiServiceRouter,
    query: &[(&str, String)],
) -> (u16, String) {
    call(router, "DescribeDBInstanceAutomatedBackups", query).await
}

/// `DeleteDBInstance` for one row, with the `DeleteAutomatedBackups` setting it names.
async fn delete_instance(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    delete_automated_backups: Option<bool>,
) {
    let mut request: Vec<(&str, String)> = params![
        "DBInstanceIdentifier" => identifier,
        "SkipFinalSnapshot" => true,
    ]
    .to_vec();
    if let Some(setting) = delete_automated_backups {
        request.push(("DeleteAutomatedBackups", setting.to_string()));
    }
    expect_ok(case, call(router, "DeleteDBInstance", &request).await);
    // The delete leads to the instance's removal, which the clock owes: polled for, not
    // waited out.
    poll_until(
        case,
        &format!("{identifier} being gone"),
        || describe_instance(router, identifier),
        |status, _| status == 404,
    )
    .await;
}

/// How a row spells `DeleteDBInstance`'s `DeleteAutomatedBackups`.
#[derive(Debug, Clone, Copy)]
enum DeleteSetting {
    /// `DeleteAutomatedBackups=false` — the backups are kept.
    Keep,
    /// `DeleteAutomatedBackups=true`.
    Drop,
    /// The member is not sent at all: "the default is to remove automated backups
    /// immediately after the DB instance is deleted".
    Default,
}

impl DeleteSetting {
    fn wire(self) -> Option<bool> {
        match self {
            Self::Keep => Some(false),
            Self::Drop => Some(true),
            Self::Default => None,
        }
    }
}

/// One row of the retention table.
struct Case {
    case: &'static str,
    identifier: &'static str,
    engine: &'static str,
    /// The `BackupRetentionPeriod` the create asks for; `None` leaves the member out, which
    /// takes the documented default of one day.
    retention: Option<i32>,
    delete_setting: DeleteSetting,
    /// Whether the instance has an `active` automated backup while it is still there.
    expects_active_backup: bool,
    /// Whether a backup is still reported once the instance is gone.
    expects_retained_backup: bool,
}

#[tokio::test]
async fn an_instance_keeps_an_automated_backup_while_it_lives_and_after_a_delete_that_kept_it() {
    let table: &[Case] = &[
        Case {
            case: "a postgres instance whose delete keeps its backups",
            identifier: "emu-iab-postgres-keep",
            engine: "postgres",
            retention: Some(1),
            delete_setting: DeleteSetting::Keep,
            expects_active_backup: true,
            expects_retained_backup: true,
        },
        Case {
            case: "a mysql instance with a week of retention whose delete keeps them",
            identifier: "emu-iab-mysql-keep",
            engine: "mysql",
            retention: Some(7),
            delete_setting: DeleteSetting::Keep,
            expects_active_backup: true,
            expects_retained_backup: true,
        },
        Case {
            case: "an oracle instance, an engine this emulator serves no wire for",
            identifier: "emu-iab-oracle-keep",
            engine: "oracle-ee",
            retention: Some(35),
            delete_setting: DeleteSetting::Keep,
            expects_active_backup: true,
            expects_retained_backup: true,
        },
        Case {
            case: "a mariadb instance created with no retention member at all",
            identifier: "emu-iab-mariadb-keep",
            engine: "mariadb",
            retention: None,
            delete_setting: DeleteSetting::Keep,
            expects_active_backup: true,
            expects_retained_backup: true,
        },
        Case {
            case: "an instance with automated backups turned off",
            identifier: "emu-iab-no-backups",
            engine: "postgres",
            retention: Some(0),
            delete_setting: DeleteSetting::Keep,
            expects_active_backup: false,
            expects_retained_backup: false,
        },
        Case {
            case: "a delete that asked to remove the backups",
            identifier: "emu-iab-dropped",
            engine: "mysql",
            retention: Some(3),
            delete_setting: DeleteSetting::Drop,
            expects_active_backup: true,
            expects_retained_backup: false,
        },
        Case {
            case: "a delete that named no setting, which removes them",
            identifier: "emu-iab-default",
            engine: "mysql",
            retention: Some(2),
            delete_setting: DeleteSetting::Default,
            expects_active_backup: true,
            expects_retained_backup: false,
        },
    ];

    for row in table {
        // Each row gets its own emulator, so one row's records cannot carry into the next.
        let router = emulator();
        let case = row.case;
        let extra: Vec<(&str, String)> = row
            .retention
            .map(|period| vec![("BackupRetentionPeriod", period.to_string())])
            .unwrap_or_default();
        let resource_id =
            available_instance(&router, case, row.identifier, row.engine, &extra).await;

        // While the instance is there, its backup is reported `active` — and an instance
        // that keeps none has no backup to report, which is the not-found refusal.
        let answer = describe_backups(&router, &params!["DbiResourceId" => &resource_id]).await;
        if row.expects_active_backup {
            let reported = expect_ok(case, answer);
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
                text(&backups[0], "DBInstanceIdentifier"),
                row.identifier,
                "case {case:?}: the backup must name the instance it is of, got {reported}"
            );
            assert_eq!(
                text(&backups[0], "Engine"),
                row.engine,
                "case {case:?}: the backup must report the source instance's engine, got \
                 {reported}"
            );
        } else {
            let refused = expect_error(case, answer, 404, NOT_FOUND);
            assert_eq!(
                text(&refused, "Message"),
                expected_not_found(&resource_id),
                "case {case:?}: expected the sentence this refusal carries, got {refused}"
            );
        }

        delete_instance(&router, case, row.identifier, row.delete_setting.wire()).await;

        let answer = describe_backups(&router, &params!["DbiResourceId" => &resource_id]).await;
        if row.expects_retained_backup {
            let reported = expect_ok(case, answer);
            let backups = subtrees(&reported, "DBInstanceAutomatedBackup");
            assert_eq!(
                backups.len(),
                1,
                "case {case:?}: expected the retained backup of the deleted instance, got \
                 {reported}"
            );
            assert_eq!(
                text(&backups[0], "Status"),
                RETAINED,
                "case {case:?}: a deleted instance's backup must report {RETAINED:?}, got \
                 {reported}"
            );
            assert_eq!(
                text(&backups[0], "DBInstanceIdentifier"),
                row.identifier,
                "case {case:?}: the retained backup must name the deleted instance, got \
                 {reported}"
            );
            assert_eq!(
                text(&backups[0], "DbiResourceId"),
                resource_id,
                "case {case:?}: the retained backup must keep the source's resource id, got \
                 {reported}"
            );
            // The window it can be restored in ends when the instance was deleted and
            // reaches back no further than the instance itself.
            let window = subtrees(&backups[0], "RestoreWindow");
            assert_eq!(
                window.len(),
                1,
                "case {case:?}: the retained backup must report its restore window, got \
                 {reported}"
            );
            let earliest = text(&window[0], "EarliestTime");
            let latest = text(&window[0], "LatestTime");
            assert!(
                earliest <= latest,
                "case {case:?}: expected EarliestTime at or before LatestTime, got \
                 {earliest} and {latest}"
            );
        } else {
            let refused = expect_error(case, answer, 404, NOT_FOUND);
            assert_eq!(
                text(&refused, "Message"),
                expected_not_found(&resource_id),
                "case {case:?}: expected the sentence this refusal carries, got {refused}"
            );
        }
    }
}

#[tokio::test]
async fn a_cluster_member_has_no_automated_backup_of_its_own() {
    // "Aurora DB clusters ... the data is held in the cluster volume", so the automated
    // backups of a cluster member are the CLUSTER's and the member reports none of its own.
    let case = "an Aurora PostgreSQL cluster member";
    let router = emulator();
    expect_ok(
        case,
        call(
            &router,
            "CreateDBCluster",
            &params![
                "DBClusterIdentifier" => "emu-iab-cluster",
                "Engine" => "aurora-postgresql",
                "MasterUsername" => MASTER_USER,
                "MasterUserPassword" => MASTER_PASSWORD,
                "BackupRetentionPeriod" => 7,
            ],
        )
        .await,
    );
    let resource_id = available_instance(
        &router,
        case,
        "emu-iab-member",
        "aurora-postgresql",
        &params![
            "DBClusterIdentifier" => "emu-iab-cluster",
            "BackupRetentionPeriod" => 7,
        ],
    )
    .await;
    let refused = expect_error(
        case,
        describe_backups(&router, &params!["DbiResourceId" => &resource_id]).await,
        404,
        NOT_FOUND,
    );
    assert_eq!(
        text(&refused, "Message"),
        expected_not_found(&resource_id),
        "case {case:?}: expected the sentence this refusal carries, got {refused}"
    );
    // And the whole report holds nothing either: the only instance in the region is that
    // member.
    let reported = expect_ok(case, describe_backups(&router, &[]).await);
    assert!(
        subtrees(&reported, "DBInstanceAutomatedBackup").is_empty(),
        "case {case:?}: a cluster member must contribute no automated backup, got {reported}"
    );
}

/// How one case of the reference table names the backup it reads.
struct Reference {
    case: &'static str,
    /// Built from the deleted instance's identifier and resource id.
    query: fn(&str, &str) -> Vec<(&'static str, String)>,
}

#[tokio::test]
async fn a_retained_backup_is_found_by_every_reference_the_report_accepts() {
    let references: &[Reference] = &[
        Reference {
            case: "by the source instance's resource id",
            query: |_, resource_id| params!["DbiResourceId" => resource_id].to_vec(),
        },
        Reference {
            case: "by the source instance's resource id in another case",
            query: |_, resource_id| {
                params!["DbiResourceId" => resource_id.to_ascii_lowercase()].to_vec()
            },
        },
        Reference {
            case: "by the source instance's identifier",
            query: |identifier, _| params!["DBInstanceIdentifier" => identifier].to_vec(),
        },
        Reference {
            case: "by the source instance's identifier in another case",
            query: |identifier, _| {
                params!["DBInstanceIdentifier" => identifier.to_ascii_uppercase()].to_vec()
            },
        },
        Reference {
            case: "by the source instance's ARN",
            query: |identifier, _| {
                params![
                    "DBInstanceIdentifier" =>
                        format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:{identifier}")
                ]
                .to_vec()
            },
        },
        Reference {
            case: "by the ARN of the automated backups",
            query: |_, resource_id| {
                params!["DBInstanceAutomatedBackupsArn" => backups_arn(resource_id)].to_vec()
            },
        },
        Reference {
            case: "by the status filter",
            query: |_, _| {
                params![
                    "Filters.Filter.1.Name" => "status",
                    "Filters.Filter.1.Values.Value.1" => RETAINED,
                ]
                .to_vec()
            },
        },
        Reference {
            case: "by the db-instance-id filter",
            query: |identifier, _| {
                params![
                    "Filters.Filter.1.Name" => "db-instance-id",
                    "Filters.Filter.1.Values.Value.1" => identifier,
                ]
                .to_vec()
            },
        },
        Reference {
            case: "by the dbi-resource-id filter",
            query: |_, resource_id| {
                params![
                    "Filters.Filter.1.Name" => "dbi-resource-id",
                    "Filters.Filter.1.Values.Value.1" => resource_id,
                ]
                .to_vec()
            },
        },
    ];

    let identifier = "emu-iab-referenced";
    let router = emulator();
    let case = "a retained backup read by every reference";
    let resource_id = available_instance(
        &router,
        case,
        identifier,
        "postgres",
        &params!["BackupRetentionPeriod" => 1],
    )
    .await;
    delete_instance(&router, case, identifier, Some(false)).await;

    for reference in references {
        let case = reference.case;
        let query = (reference.query)(identifier, &resource_id);
        let reported = expect_ok(case, describe_backups(&router, &query).await);
        let backups = subtrees(&reported, "DBInstanceAutomatedBackup");
        assert_eq!(
            backups.len(),
            1,
            "case {case:?}: expected the one retained backup, got {reported}"
        );
        assert_eq!(
            text(&backups[0], "DbiResourceId"),
            resource_id,
            "case {case:?}: expected the backup of {resource_id}, got {reported}"
        );
        assert_eq!(
            text(&backups[0], "Status"),
            RETAINED,
            "case {case:?}: expected {RETAINED:?}, got {reported}"
        );
    }

    // A filter for the OTHER status matches no backup: the report is served and EMPTY,
    // because the filter excluded what the region holds rather than naming nothing.
    let reported = expect_ok(
        case,
        describe_backups(
            &router,
            &params![
                "Filters.Filter.1.Name" => "status",
                "Filters.Filter.1.Values.Value.1" => ACTIVE,
            ],
        )
        .await,
    );
    assert!(
        subtrees(&reported, "DBInstanceAutomatedBackup").is_empty(),
        "case {case:?}: a filter for {ACTIVE:?} must match no retained backup, got {reported}"
    );

    // `DeleteDBInstanceAutomatedBackup` removes it, reporting the backup it removed, and
    // nothing reports it afterwards — by any reference.
    let removed = expect_ok(
        case,
        call(
            &router,
            "DeleteDBInstanceAutomatedBackup",
            &params!["DbiResourceId" => &resource_id],
        )
        .await,
    );
    assert_eq!(
        text(&removed, "DbiResourceId"),
        resource_id,
        "case {case:?}: the delete must report the backup it removed, got {removed}"
    );
    assert_eq!(
        text(&removed, "DBInstanceIdentifier"),
        identifier,
        "case {case:?}: the removed backup must name its source instance, got {removed}"
    );
    for reference in references {
        let case = reference.case;
        let query = (reference.query)(identifier, &resource_id);
        let answer = describe_backups(&router, &query).await;
        // A reference that NARROWS the report is a not-found once the backup is gone; a
        // filter has nothing to match, so its report is served and empty.
        if query.iter().any(|(name, _)| name.starts_with("Filters.")) {
            let reported = expect_ok(case, answer);
            assert!(
                subtrees(&reported, "DBInstanceAutomatedBackup").is_empty(),
                "case {case:?}: expected no backup after the delete, got {reported}"
            );
        } else {
            expect_error(case, answer, 404, NOT_FOUND);
        }
    }
    // And deleting it again is the same not-found refusal.
    let refused = expect_error(
        case,
        call(
            &router,
            "DeleteDBInstanceAutomatedBackup",
            &params!["DbiResourceId" => &resource_id],
        )
        .await,
        404,
        NOT_FOUND,
    );
    assert_eq!(
        text(&refused, "Message"),
        expected_not_found(&resource_id),
        "case {case:?}: expected the sentence this refusal carries, got {refused}"
    );
}

#[tokio::test]
async fn one_backup_is_reported_for_an_instance_whose_delete_has_been_accepted() {
    // The delete decides what becomes of the automated backups, so from the moment it is
    // accepted the retained record is the ONE backup of that instance: the instance still
    // reports `deleting` for the span the clock owes its removal, and the report must not
    // carry its `active` backup beside the retained one.
    let router = emulator();
    let case = "an instance whose delete has been accepted";
    let identifier = "emu-iab-deleting";
    let resource_id = available_instance(
        &router,
        case,
        identifier,
        "postgres",
        &params!["BackupRetentionPeriod" => 1],
    )
    .await;
    expect_ok(
        case,
        call(
            &router,
            "DeleteDBInstance",
            &params![
                "DBInstanceIdentifier" => identifier,
                "SkipFinalSnapshot" => true,
                "DeleteAutomatedBackups" => false,
            ],
        )
        .await,
    );
    // Read the backup on every poll of the instance, right through the span the removal is
    // owed in: exactly one entry each time, whichever status it carries.
    poll_until(
        case,
        &format!("{identifier} being gone"),
        || async {
            let reported = expect_ok(
                case,
                describe_backups(&router, &params!["DbiResourceId" => &resource_id]).await,
            );
            let backups = subtrees(&reported, "DBInstanceAutomatedBackup");
            assert_eq!(
                backups.len(),
                1,
                "case {case:?}: exactly one backup must be reported for {resource_id} while \
                 the delete runs, got {reported}"
            );
            describe_instance(&router, identifier).await
        },
        |status, _| status == 404,
    )
    .await;
    let reported = expect_ok(
        case,
        describe_backups(&router, &params!["DbiResourceId" => &resource_id]).await,
    );
    assert_eq!(
        text(&reported, "Status"),
        RETAINED,
        "case {case:?}: the surviving backup must report {RETAINED:?}, got {reported}"
    );
}

#[tokio::test]
async fn a_delete_that_cannot_keep_the_backup_is_refused_and_keeps_the_instance() {
    // The region's retained-backup allowance is the DB instance quota, as the modelled
    // `DBInstanceAutomatedBackupQuotaExceededFault` words it. Filling it is what makes the
    // next delete that asks to KEEP a backup refusable — and that refused delete must
    // perform nothing, so the instance it named is still there afterwards. The engine is
    // one this emulator serves no wire for, so filling the allowance opens no listener.
    let router = emulator();
    let case = "a delete over the retained-backup allowance";
    let allowance = service_com_amazonaws_rds_amazonrdsv19::state::
        RETAINED_INSTANCE_BACKUP_QUOTA_PER_REGION;
    for index in 0..allowance {
        let identifier = format!("emu-iab-quota-{index:03}");
        available_instance(
            &router,
            case,
            &identifier,
            "oracle-ee",
            &params!["BackupRetentionPeriod" => 1],
        )
        .await;
        delete_instance(&router, case, &identifier, Some(false)).await;
    }
    let identifier = "emu-iab-quota-over";
    available_instance(
        &router,
        case,
        identifier,
        "oracle-ee",
        &params!["BackupRetentionPeriod" => 1],
    )
    .await;
    let refused = expect_error(
        case,
        call(
            &router,
            "DeleteDBInstance",
            &params![
                "DBInstanceIdentifier" => identifier,
                "SkipFinalSnapshot" => true,
                "DeleteAutomatedBackups" => false,
            ],
        )
        .await,
        400,
        "DBInstanceAutomatedBackupQuotaExceeded",
    );
    assert_eq!(
        text(&refused, "Message"),
        format!(
            "the request would exceed the allowed number of retained automated backups \
             ({allowance} per region)"
        ),
        "case {case:?}: expected the sentence this refusal carries, got {refused}"
    );
    // The refused delete performed nothing: the instance is still there, and still has its
    // own `active` backup.
    let described = expect_ok(case, describe_instance(&router, identifier).await);
    assert_eq!(
        text(&described, "DBInstanceStatus"),
        "available",
        "case {case:?}: a refused delete must leave the instance as it was, got {described}"
    );
    let resource_id = text(&described, "DbiResourceId");
    let reported = expect_ok(
        case,
        describe_backups(&router, &params!["DbiResourceId" => &resource_id]).await,
    );
    assert_eq!(
        text(&reported, "Status"),
        ACTIVE,
        "case {case:?}: the instance's own backup must still be {ACTIVE:?}, got {reported}"
    );
    // The same delete, asked to REMOVE the backups instead, is served: the allowance bounds
    // only the backups a delete keeps.
    delete_instance(&router, case, identifier, Some(true)).await;
}

/// The automated-backups ARN of the instance whose resource id is `resource_id`, as a
/// client composes it from what `DescribeDBInstanceAutomatedBackups` reported:
/// `arn:aws:rds:<region>:<account>:auto-backup:ab-<id>`.
fn backups_arn(resource_id: &str) -> String {
    format!(
        "arn:aws:rds:{REGION}:{ACCOUNT}:auto-backup:ab-{}",
        resource_id.trim_start_matches("db-")
    )
}

/// How one case of the delete table names the backup it removes: the two members
/// `DeleteDBInstanceAutomatedBackup` accepts, each on its own and both together.
struct DeleteReference {
    case: &'static str,
    /// Built from the retained backup's resource id.
    query: fn(&str) -> Vec<(&'static str, String)>,
}

#[tokio::test]
async fn a_retained_backup_is_deleted_by_every_reference_the_delete_accepts() {
    let references: &[DeleteReference] = &[
        DeleteReference {
            case: "by the source instance's resource id",
            query: |resource_id| params!["DbiResourceId" => resource_id].to_vec(),
        },
        DeleteReference {
            case: "by the ARN of the automated backups",
            query: |resource_id| {
                params!["DBInstanceAutomatedBackupsArn" => backups_arn(resource_id)].to_vec()
            },
        },
        DeleteReference {
            case: "by both members, which name the same backups",
            query: |resource_id| {
                params![
                    "DbiResourceId" => resource_id,
                    "DBInstanceAutomatedBackupsArn" => backups_arn(resource_id),
                ]
                .to_vec()
            },
        },
    ];

    // One retained backup per reference, so each is removed by a different one of them and
    // the deletes cannot stand in for each other.
    let router = emulator();
    let mut retained: Vec<String> = Vec::new();
    for (index, reference) in references.iter().enumerate() {
        let identifier = format!("emu-iab-del-{index}");
        let resource_id = available_instance(
            &router,
            reference.case,
            &identifier,
            "postgres",
            &params!["BackupRetentionPeriod" => 1],
        )
        .await;
        delete_instance(&router, reference.case, &identifier, Some(false)).await;
        retained.push(resource_id);
    }

    for (index, reference) in references.iter().enumerate() {
        let case = reference.case;
        let resource_id = &retained[index];
        let removed = expect_ok(
            case,
            call(
                &router,
                "DeleteDBInstanceAutomatedBackup",
                &(reference.query)(resource_id),
            )
            .await,
        );
        assert_eq!(
            text(&removed, "DbiResourceId"),
            *resource_id,
            "case {case:?}: the delete must report the backup it removed, got {removed}"
        );
        assert_eq!(
            text(&removed, "DBInstanceAutomatedBackupsArn"),
            backups_arn(resource_id),
            "case {case:?}: the removed backup must report its own ARN, got {removed}"
        );
        // It is gone, and the backups the other references name are untouched.
        expect_error(
            case,
            describe_backups(&router, &params!["DbiResourceId" => resource_id]).await,
            404,
            NOT_FOUND,
        );
        for other in retained.iter().skip(index + 1) {
            let reported = expect_ok(
                case,
                describe_backups(&router, &params!["DbiResourceId" => other]).await,
            );
            assert_eq!(
                text(&reported, "Status"),
                RETAINED,
                "case {case:?}: the backup of {other} must still be retained, got {reported}"
            );
        }
    }
}

#[tokio::test]
async fn the_requests_the_automated_backup_calls_refuse() {
    let router = emulator();
    let case = "the refusals";
    let identifier = "emu-iab-refusals";
    let resource_id = available_instance(
        &router,
        case,
        identifier,
        "postgres",
        &params!["BackupRetentionPeriod" => 1],
    )
    .await;

    // The backups of a LIVE instance go with that instance, so this delete refuses them —
    // by either member that names them.
    for query in [
        params!["DbiResourceId" => &resource_id].to_vec(),
        params!["DBInstanceAutomatedBackupsArn" => backups_arn(&resource_id)].to_vec(),
    ] {
        let refused = expect_error(
            case,
            call(&router, "DeleteDBInstanceAutomatedBackup", &query).await,
            400,
            INVALID_STATE,
        );
        assert_eq!(
            text(&refused, "Message"),
            format!(
                "The automated backup is associated with the active DB instance {identifier} \
                 and can't be deleted."
            ),
            "case {case:?}: expected the sentence this refusal carries, got {refused}"
        );
    }

    // Two members that name DIFFERENT backups have no one answer.
    expect_error(
        case,
        call(
            &router,
            "DeleteDBInstanceAutomatedBackup",
            &params![
                "DbiResourceId" => &resource_id,
                "DBInstanceAutomatedBackupsArn" => backups_arn("db-SOMEOTHERINSTANCESIDENTIFIER"),
            ],
        )
        .await,
        400,
        "InvalidParameterCombination",
    );

    // A delete that names NEITHER member names no backup at all.
    expect_error(
        case,
        call::<&str, &str>(&router, "DeleteDBInstanceAutomatedBackup", &[]).await,
        400,
        "InvalidParameterCombination",
    );

    // An id nothing was ever created under is the not-found refusal, from both calls, and
    // so is an automated-backups ARN naming one — each refusal naming the value it was sent.
    let unknown = "db-NOTHINGWASEVERCREATEDUNDERTHIS";
    for operation in [
        "DescribeDBInstanceAutomatedBackups",
        "DeleteDBInstanceAutomatedBackup",
    ] {
        for named in [
            params!["DbiResourceId" => unknown].to_vec(),
            params!["DBInstanceAutomatedBackupsArn" => backups_arn(unknown)].to_vec(),
        ] {
            let sent = named[0].1.clone();
            let refused = expect_error(
                operation,
                call(&router, operation, &named).await,
                404,
                NOT_FOUND,
            );
            assert_eq!(
                text(&refused, "Message"),
                expected_not_found(&sent),
                "case {operation:?}: expected the sentence this refusal carries, got {refused}"
            );
        }
    }

    // A filter the report does not document is refused rather than quietly ignored, which
    // would report backups the caller excluded.
    expect_error(
        case,
        describe_backups(
            &router,
            &params![
                "Filters.Filter.1.Name" => "db-instance-identifier",
                "Filters.Filter.1.Values.Value.1" => identifier,
            ],
        )
        .await,
        400,
        "InvalidParameterValue",
    );
}

#[tokio::test]
async fn the_report_pages_through_every_backup_a_region_holds() {
    // More backups than one page of the smallest `MaxRecords` the report accepts ("must be
    // between 20 and 100"), so the paging is real: the instances all keep automated
    // backups, and the last one is deleted with its backup kept, so the pages carry both
    // an `active` and a `retained` entry. The engine is one this emulator serves no wire
    // for, so standing this many up opens no listener.
    let router = emulator();
    let case = "paging the automated backups of a region";
    let identifiers: Vec<String> = (0..22).map(|index| format!("emu-iab-page-{index:02}")).collect();
    let mut expected: Vec<String> = Vec::new();
    for identifier in &identifiers {
        expected.push(
            available_instance(
                &router,
                case,
                identifier,
                "oracle-ee",
                &params!["BackupRetentionPeriod" => 1],
            )
            .await,
        );
    }
    let deleted = identifiers.last().expect("the table names instances");
    delete_instance(&router, case, deleted, Some(false)).await;
    expected.sort();

    let reported = expect_ok(case, describe_backups(&router, &[]).await);
    let mut listed = texts(&reported, "DbiResourceId");
    listed.sort();
    assert_eq!(
        listed, expected,
        "case {case:?}: expected every backup the region holds, got {reported}"
    );

    for page_size in [20, 21, 100] {
        let mut seen: Vec<String> = Vec::new();
        let mut marker: Option<String> = None;
        // Bounded by the number of backups the region holds, so a marker that stopped
        // advancing cannot spin here.
        for _ in 0..=expected.len() {
            let mut query = params!["MaxRecords" => page_size].to_vec();
            if let Some(marker) = &marker {
                query.push(("Marker", marker.clone()));
            }
            let page = expect_ok(case, describe_backups(&router, &query).await);
            let ids = texts(&page, "DbiResourceId");
            assert!(
                ids.len() <= page_size as usize,
                "case {case:?}: a page of {page_size} must not carry {} backups: {page}",
                ids.len()
            );
            seen.extend(ids);
            marker = optional_text(&page, "Marker");
            if marker.is_none() {
                break;
            }
        }
        seen.sort();
        assert_eq!(
            seen, expected,
            "case {case:?}: paging in {page_size}s must report every backup exactly once"
        );
    }
}
