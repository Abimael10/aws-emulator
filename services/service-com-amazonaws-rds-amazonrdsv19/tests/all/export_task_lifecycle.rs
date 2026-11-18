// SPDX-License-Identifier: CC0-1.0

//! The export-task LIFECYCLE a client drives with a poll loop: the record `StartExportTask`
//! writes, the progression that record makes on the CLOCK alone, and what `CancelExportTask`
//! then does to it.
//!
//! The rule this suite pins: the task `StartExportTask` records IS the one record every later
//! call to this service is served from — a client that only REPORTS (`DescribeExportTasks`,
//! making no mutating call of its own, which is what every waiter and provisioning script
//! does) sees that task leave `STARTING` and settle in a terminal status, with the
//! `PercentProgress` and the `TaskEndTime` of that status and with every member the start
//! answered reported unchanged; `CancelExportTask` ends exactly the task it names, taking it
//! through `CANCELING` to `CANCELED` and leaving the Region's other tasks to finish; and a
//! task that has stopped takes no further cancellation — that is the modelled
//! `InvalidExportTaskStateFault` (HTTP 400), naming the task and the status it is in.
//!
//! The rows span what the rule generalises over: what the request asks to be exported and
//! where (`S3Prefix` given and omitted, `ExportOnly` naming nothing, one database and two
//! tables) and, for the cancellation, the fate each task of one Region gets — so a cancel is
//! shown to act on ONE record and not on the others.
//!
//! Amazon S3, IAM and KMS are mounted beside this service on ONE engine, exactly as the
//! assembled router mounts them, because an export resolves its destination bucket, its role
//! and its key against them. Every wait here is a poll loop over a reporting call
//! ([`crate::lifecycle_poll_loops::poll_until`]), so what ends a loop is only a transition
//! this service's clock already owed — no assertion here is on how fast the host runs.

use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, expect_error, expect_ok, optional_text, subtrees, text, texts, REGION,
};
use crate::lifecycle_poll_loops::poll_until;
use crate::rds_s3_integration::{create_bucket, emulator, export_role_policy, kms, role};

/// The canonical account id every generated emulator presents, which the source ARN below is
/// read against.
const ACCOUNT: &str = "000000000000";

/// The master user the source instance is created with, and its password — test credentials,
/// and the only place in this suite they appear.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// The DB instance the exported snapshot is taken of, and the snapshot itself.
const INSTANCE: &str = "emu-lifecycle-export-instance";
const SNAPSHOT: &str = "emu-lifecycle-export-snapshot";

/// The statuses an export task has STOPPED in — what a poll loop here waits for when it
/// waits for a task to settle, taken from the store's own list
/// ([`st::TERMINAL_STATUSES`]) so the suite and the handler cannot drift on what "stopped"
/// means.
const TERMINAL: [&str; 3] = st::TERMINAL_STATUSES;

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// Everything an export needs: the emulator, the destination bucket, the role the export
/// runs under, the key its objects are encrypted with, and the ARN of the snapshot it reads.
struct Fixture {
    router: MultiServiceRouter,
    bucket: String,
    role_arn: String,
    key_id: String,
    snapshot_arn: String,
}

/// Stand up the destination and the source, driving each through its own documented
/// lifecycle: the instance is polled to the status a snapshot may be taken in, and the
/// snapshot to the status an export reads.
async fn fixture(name: &str) -> Fixture {
    let (router, _engine) = emulator();
    let bucket = format!("emu-lifecycle-{name}-bucket");
    create_bucket(&router, &bucket, REGION).await;
    let key_id = kms(&router)
        .create_key()
        .description("the export's encryption key")
        .send()
        .await
        .expect("the emulated KMS creates the export key")
        .key_metadata()
        .map(|metadata| metadata.key_id().to_string())
        .expect("the created key reports its id");
    let role_arn = role(
        &router,
        &format!("emu-lifecycle-{name}-role"),
        st::EXPORT_SERVICE_PRINCIPAL,
        Some(export_role_policy(&bucket)),
    )
    .await;

    expect_ok(
        "the source DB instance",
        call(
            &router,
            "CreateDBInstance",
            &params![
                "DBInstanceIdentifier" => INSTANCE,
                "Engine" => "mysql",
                "DBInstanceClass" => "db.t3.micro",
                "AllocatedStorage" => 20,
                "MasterUsername" => MASTER_USER,
                "MasterUserPassword" => MASTER_PASSWORD,
            ],
        )
        .await,
    );
    let instance_request = params!["DBInstanceIdentifier" => INSTANCE];
    poll_until(
        "the fixture",
        "the source DB instance reaching the status a snapshot is taken in",
        || call(&router, "DescribeDBInstances", &instance_request),
        |status, body| {
            status == 200 && text(body, "DBInstanceStatus") == st::DB_INSTANCE_STATUS_AVAILABLE
        },
    )
    .await;

    expect_ok(
        "the source DB snapshot",
        call(
            &router,
            "CreateDBSnapshot",
            &params![
                "DBInstanceIdentifier" => INSTANCE,
                "DBSnapshotIdentifier" => SNAPSHOT,
            ],
        )
        .await,
    );
    let snapshot_request = params!["DBSnapshotIdentifier" => SNAPSHOT];
    poll_until(
        "the fixture",
        "the source DB snapshot reaching the status an export reads",
        || call(&router, "DescribeDBSnapshots", &snapshot_request),
        |status, body| status == 200 && text(body, "Status") == st::SNAPSHOT_STATUS_AVAILABLE,
    )
    .await;

    Fixture {
        router,
        bucket,
        role_arn,
        key_id,
        snapshot_arn: format!("arn:aws:rds:{REGION}:{ACCOUNT}:snapshot:{SNAPSHOT}"),
    }
}

/// Start one export of the fixture's snapshot, as a client starts it.
async fn start_export(
    fixture: &Fixture,
    identifier: &str,
    prefix: Option<&str>,
    export_only: &[&str],
) -> (u16, String) {
    let mut request: Vec<(String, String)> = params![
        "ExportTaskIdentifier" => identifier,
        "SourceArn" => fixture.snapshot_arn,
        "S3BucketName" => fixture.bucket,
        "IamRoleArn" => fixture.role_arn,
        "KmsKeyId" => fixture.key_id,
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value))
    .collect();
    if let Some(prefix) = prefix {
        request.push(("S3Prefix".to_string(), prefix.to_string()));
    }
    // The awsQuery form of a list member is `ExportOnly.member.<n>`, indexed from one,
    // however many entries a request names — so this spans the whole member rather than the
    // entry counts the rows below happen to use.
    for (index, entry) in export_only.iter().enumerate() {
        request.push((
            format!("ExportOnly.member.{}", index + 1),
            (*entry).to_string(),
        ));
    }
    call(&fixture.router, "StartExportTask", &request).await
}

/// `CancelExportTask` for one identifier.
async fn cancel_export(fixture: &Fixture, identifier: &str) -> (u16, String) {
    call(
        &fixture.router,
        "CancelExportTask",
        &params!["ExportTaskIdentifier" => identifier],
    )
    .await
}

/// The one `<ExportTask>` subtree a report carries.
fn the_task(case: &str, body: &str) -> String {
    let reported = subtrees(body, "ExportTask");
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: expected exactly one reported ExportTask, got {body}"
    );
    reported.into_iter().next().expect("checked just above")
}

/// Poll `DescribeExportTasks` — a REPORTING call and nothing else — until the task reports
/// one of `wanted`, and answer the `<ExportTask>` it settled on.
async fn reported_once(
    fixture: &Fixture,
    case: &str,
    identifier: &str,
    wanted: &[&str],
) -> String {
    let request = params!["ExportTaskIdentifier" => identifier];
    let (body, _polls) = poll_until(
        case,
        &format!("the export task {identifier} reporting one of {wanted:?}"),
        || call(&fixture.router, "DescribeExportTasks", &request),
        |status, body| {
            status == 200
                && subtrees(body, "ExportTask").len() == 1
                && wanted.contains(&text(&the_task(case, body), "Status").as_str())
        },
    )
    .await;
    the_task(case, &body)
}

/// The `ExportOnly` entries one reported task carries.
fn export_only_of(task: &str) -> Vec<String> {
    subtrees(task, "ExportOnly")
        .first()
        .map(|list| texts(list, "member"))
        .unwrap_or_default()
}

/// One row of the lifecycle table: what the export is asked to write, and where.
struct Row {
    case: &'static str,
    identifier: &'static str,
    prefix: Option<&'static str>,
    export_only: &'static [&'static str],
}

const ROWS: &[Row] = &[
    Row {
        case: "the whole snapshot, at the root of the bucket",
        identifier: "emu-lifecycle-export-all",
        prefix: None,
        export_only: &[],
    },
    Row {
        case: "the whole snapshot, under a prefix",
        identifier: "emu-lifecycle-export-prefixed",
        prefix: Some("nightly/exports"),
        export_only: &[],
    },
    Row {
        case: "one database of the snapshot, under a prefix",
        identifier: "emu-lifecycle-export-one-database",
        prefix: Some("archive"),
        export_only: &["appdb"],
    },
    Row {
        case: "two tables of one database, at the root of the bucket",
        identifier: "emu-lifecycle-export-two-tables",
        prefix: None,
        export_only: &["appdb.orders", "appdb.regions"],
    },
];

/// Rule: a task `StartExportTask` recorded leaves `STARTING` on the CLOCK alone and settles
/// in a terminal status, and the report a polling client reads is served from that one
/// record — every member the start answered comes back unchanged, with the progress and the
/// end time of the status it settled in. A task that has stopped takes no cancellation.
#[tokio::test]
async fn a_started_export_task_settles_on_the_clock_and_round_trips_through_the_report() {
    let fixture = fixture("settles").await;

    for row in ROWS {
        let started = expect_ok(
            row.case,
            start_export(&fixture, row.identifier, row.prefix, row.export_only).await,
        );
        for (member, expected) in [
            ("ExportTaskIdentifier", row.identifier.to_string()),
            ("Status", st::EXPORT_TASK_STATUS_STARTING.to_string()),
            (
                "PercentProgress",
                st::INITIAL_PERCENT_PROGRESS.to_string(),
            ),
            ("SourceType", st::EXPORT_SOURCE_TYPE_SNAPSHOT.to_string()),
            ("SourceArn", fixture.snapshot_arn.clone()),
            ("S3Bucket", fixture.bucket.clone()),
            ("IamRoleArn", fixture.role_arn.clone()),
            ("KmsKeyId", fixture.key_id.clone()),
        ] {
            let reported = text(&started, member);
            assert_eq!(
                reported, expected,
                "case {:?}: expected the started task's {member} {expected:?}, got {reported:?}",
                row.case
            );
        }
        let not_ended = optional_text(&started, "TaskEndTime");
        assert!(
            not_ended.is_none(),
            "case {:?}: a task that has not stopped reports no TaskEndTime, got {not_ended:?}",
            row.case
        );

        // Only the clock moves it: this loop makes reporting calls and nothing else.
        let settled = reported_once(&fixture, row.case, row.identifier, &TERMINAL).await;
        let status = text(&settled, "Status");
        assert_eq!(
            status,
            st::EXPORT_TASK_STATUS_COMPLETE,
            "case {:?}: an export nothing cancelled finishes in {}, got {settled}",
            row.case,
            st::EXPORT_TASK_STATUS_COMPLETE
        );
        let progress = text(&settled, "PercentProgress");
        assert_eq!(
            progress,
            st::COMPLETE_PERCENT_PROGRESS.to_string(),
            "case {:?}: a finished export reports {} percent progress, got {progress:?}",
            row.case,
            st::COMPLETE_PERCENT_PROGRESS
        );
        let ended = optional_text(&settled, "TaskEndTime");
        assert!(
            ended.is_some(),
            "case {:?}: a task that has stopped reports when it stopped, got {settled}",
            row.case
        );

        // The report is served from the record the start wrote: every member the start
        // answered, other than the ones its progression changes, comes back as it was.
        for member in [
            "ExportTaskIdentifier",
            "SourceArn",
            "SourceType",
            "S3Bucket",
            "IamRoleArn",
            "KmsKeyId",
            "SnapshotTime",
            "TaskStartTime",
        ] {
            let answered = text(&started, member);
            let reported = text(&settled, member);
            assert_eq!(
                reported, answered,
                "case {:?}: expected the report's {member} to be the started task's \
                 {answered:?}, got {reported:?}",
                row.case
            );
        }
        let reported_prefix = optional_text(&settled, "S3Prefix");
        assert_eq!(
            reported_prefix.as_deref(),
            row.prefix,
            "case {:?}: expected S3Prefix {:?}, got {reported_prefix:?}",
            row.case,
            row.prefix
        );
        let reported_only = export_only_of(&settled);
        assert_eq!(
            reported_only,
            row.export_only
                .iter()
                .map(|entry| (*entry).to_string())
                .collect::<Vec<String>>(),
            "case {:?}: expected the ExportOnly entries the request named, got {settled}",
            row.case
        );

        // And a task that has stopped takes no cancellation: the modelled state fault, whose
        // sentence names the task and the status it is in.
        let refused = expect_error(
            row.case,
            cancel_export(&fixture, row.identifier).await,
            400,
            "InvalidExportTaskStateFault",
        );
        let message = text(&refused, "Message");
        assert!(
            message.contains(row.identifier) && message.contains(&status),
            "case {:?}: expected the refusal to name the task {:?} and its status \
             {status:?}, got {message:?}",
            row.case,
            row.identifier
        );
    }
}

/// Rule: `CancelExportTask` ends exactly the task it names — that task reports `CANCELING`
/// and the clock then takes it to `CANCELED`, with the end time of a task that has stopped
/// and without the progress of one that finished — and the Region's other export tasks run
/// on to their own terminal status, untouched by it.
#[tokio::test]
async fn cancelling_one_export_task_ends_that_one_and_leaves_the_others_running() {
    let fixture = fixture("cancels").await;

    // The task the cancellation names, cancelled while it is still running.
    const TARGET: &str = "emu-lifecycle-cancelled-export";
    expect_ok(
        "the cancelled task",
        start_export(&fixture, TARGET, Some("cancelled"), &[]).await,
    );
    let accepted = expect_ok("the cancellation", cancel_export(&fixture, TARGET).await);
    for (member, expected) in [
        ("ExportTaskIdentifier", TARGET.to_string()),
        ("Status", st::EXPORT_TASK_STATUS_CANCELING.to_string()),
    ] {
        let reported = text(&accepted, member);
        assert_eq!(
            reported, expected,
            "case \"the cancellation\": expected {member} {expected:?}, got {reported:?}"
        );
    }

    // The tasks the cancellation does not name, started after it was accepted.
    const OTHERS: [(&str, &str); 2] = [
        ("the task started beside the cancelled one", "emu-lifecycle-sibling-export"),
        ("a second task beside the cancelled one", "emu-lifecycle-sibling-export-two"),
    ];
    for (case, identifier) in OTHERS {
        expect_ok(case, start_export(&fixture, identifier, None, &[]).await);
    }

    // The cancelled task stops as CANCELED, with the end time of a stopped task and without
    // the progress of one that finished.
    let cancelled = reported_once(&fixture, "the cancelled task", TARGET, &TERMINAL).await;
    let status = text(&cancelled, "Status");
    assert_eq!(
        status,
        st::EXPORT_TASK_STATUS_CANCELED,
        "case \"the cancelled task\": expected {}, got {cancelled}",
        st::EXPORT_TASK_STATUS_CANCELED
    );
    let ended = optional_text(&cancelled, "TaskEndTime");
    assert!(
        ended.is_some(),
        "case \"the cancelled task\": a task that has stopped reports when it stopped, got \
         {cancelled}"
    );
    let progress = text(&cancelled, "PercentProgress");
    assert_eq!(
        progress,
        st::INITIAL_PERCENT_PROGRESS.to_string(),
        "case \"the cancelled task\": a cancelled export did not finish, so it reports {} \
         percent progress, got {progress:?}",
        st::INITIAL_PERCENT_PROGRESS
    );

    // A task that has stopped takes no second cancellation.
    let refused = expect_error(
        "a second cancellation",
        cancel_export(&fixture, TARGET).await,
        400,
        "InvalidExportTaskStateFault",
    );
    let message = text(&refused, "Message");
    assert!(
        message.contains(TARGET) && message.contains(&status),
        "case \"a second cancellation\": expected the refusal to name the task {TARGET:?} and \
         its status {status:?}, got {message:?}"
    );

    // And the tasks it did not name finish, each in its own right.
    for (case, identifier) in OTHERS {
        let other = reported_once(&fixture, case, identifier, &TERMINAL).await;
        let other_status = text(&other, "Status");
        assert_eq!(
            other_status,
            st::EXPORT_TASK_STATUS_COMPLETE,
            "case {case:?}: a task no cancellation named finishes in {}, got {other}",
            st::EXPORT_TASK_STATUS_COMPLETE
        );
        let identifier_reported = text(&other, "ExportTaskIdentifier");
        assert_eq!(
            identifier_reported, identifier,
            "case {case:?}: expected the report of {identifier:?}, got {identifier_reported:?}"
        );
    }
}
