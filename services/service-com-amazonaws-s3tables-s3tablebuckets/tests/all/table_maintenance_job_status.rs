// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `GetTableMaintenanceJobStatus`: the status every
//! maintenance job of a table reports.
//!
//! The rule under test: a maintenance job is the RUN of a maintenance task, so
//! for every job type the model declares (`TableMaintenanceJobType`:
//! `icebergCompaction`, `icebergSnapshotManagement`, configured on the table,
//! and `icebergUnreferencedFileRemoval`, configured on its table bucket) the
//! read reports one entry derived from the configuration in force for that task
//! and from the clock — a task that is not enabled reports the modeled disabled
//! job status, and an enabled one reports its run as not yet run until the run
//! of its current eligible period has completed and as successful with that
//! run's instant afterwards. The read writes nothing, so two reads of a table
//! nobody reconfigured answer the same, and a call against a table (or a table
//! bucket) the store does not hold is refused with the modeled not-found.
//!
//! The table spans what the rule generalises over: which job is addressed, at
//! which level its task is configured, the status the configuration carries,
//! whether the task was ever configured, a task turned off and back on, a put
//! that changes settings without changing the status, and the region. A row's
//! expectation for a task that RUNS tolerates the scheduled run having
//! completed between the put and the read — the outcome of the host being slow,
//! not of the rule — and then holds the run to the instants it may fall
//! between; what the run must never be is the status of a task that does not
//! run. `a_jobs_scheduled_run_completes_and_the_read_reports_it` is where the
//! progression itself is pinned, by polling for the run.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use super::harness::{Emulator, Reply};

/// The fixed sentence this service states a table the store does not hold with.
const NO_TABLE: &str = "The specified table does not exist.";

/// Every value `TableMaintenanceJobType` declares, from
/// `smithy_query(query="shape",shape="TableMaintenanceJobType")`. A read reports
/// one entry per job, so this is the whole key set of the `status` map.
const EVERY_JOB: [&str; 3] = [
    "icebergCompaction",
    "icebergSnapshotManagement",
    "icebergUnreferencedFileRemoval",
];

/// The `JobStatus` values this emulator's jobs reach, from
/// `smithy_query(query="shape",shape="JobStatus")`: the job runs but has not
/// completed a run yet, its run completed, or the task does not run.
const NOT_YET_RUN: &str = "Not_Yet_Run";
const SUCCESSFUL: &str = "Successful";
const DISABLED: &str = "Disabled";

/// How long a case waits for a scheduled run to complete before giving up.
/// GENEROUS on purpose: the test polls for the run and asserts nothing about how
/// long it took, so a loaded host only makes it poll more often.
const RUN_DEADLINE: Duration = Duration::from_secs(120);

/// How long a case waits between two polls of a job's status.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One put of a maintenance configuration a case applies before it reads: which
/// level the task is configured at, the task, and the status to put on it.
#[derive(Debug, Clone)]
enum Configure {
    /// `PutTableMaintenanceConfiguration` for one of the table's own tasks.
    Table(&'static str, &'static str),
    /// `PutTableMaintenanceConfiguration` for one of the table's own tasks,
    /// carrying settings as well as the status — the case where a put changes
    /// what the task does without changing whether it runs.
    TableWithSettings(&'static str, &'static str, Value),
    /// `PutTableBucketMaintenanceConfiguration` for one of the bucket's tasks.
    Bucket(&'static str, &'static str),
}

/// Create a table bucket, a namespace and one table in it, and answer the
/// bucket's ARN with the table's ARN.
async fn create_table(
    emulator: &Emulator,
    region: &str,
    bucket: &str,
    namespace: &str,
    table: &str,
) -> (String, String) {
    let created = emulator
        .create_table_bucket(region, &json!({ "name": bucket }))
        .await;
    assert_eq!(
        created.status, 200,
        "creating the bucket {bucket:?} must succeed, got {status} {body}",
        status = created.status,
        body = created.body
    );
    let bucket_arn = created.text("arn");
    let namespace_created = emulator
        .create_namespace(region, &bucket_arn, &json!({ "namespace": [namespace] }))
        .await;
    assert_eq!(
        namespace_created.status, 200,
        "creating the namespace {namespace:?} must succeed, got {status} {body}",
        status = namespace_created.status,
        body = namespace_created.body
    );
    let table_created = emulator
        .create_table(
            region,
            &bucket_arn,
            namespace,
            &json!({ "name": table, "format": "ICEBERG" }),
        )
        .await;
    assert_eq!(
        table_created.status, 200,
        "creating the table {table:?} must succeed, got {status} {body}",
        status = table_created.status,
        body = table_created.body
    );
    (bucket_arn, table_created.text("tableARN"))
}

/// Apply one configuration put, asserting it was accepted.
async fn configure(
    emulator: &Emulator,
    region: &str,
    bucket_arn: &str,
    namespace: &str,
    table: &str,
    step: &Configure,
    case: &str,
) {
    let reply = match step {
        Configure::Table(kind, status) => {
            emulator
                .put_table_maintenance_configuration(
                    region,
                    bucket_arn,
                    namespace,
                    table,
                    kind,
                    &json!({ "status": status }),
                )
                .await
        }
        Configure::TableWithSettings(kind, status, settings) => {
            emulator
                .put_table_maintenance_configuration(
                    region,
                    bucket_arn,
                    namespace,
                    table,
                    kind,
                    &json!({ "status": status, "settings": settings }),
                )
                .await
        }
        Configure::Bucket(kind, status) => {
            emulator
                .put_table_bucket_maintenance_configuration(
                    region,
                    bucket_arn,
                    kind,
                    &json!({ "status": status }),
                )
                .await
        }
    };
    assert_eq!(
        reply.status, 204,
        "case {case:?}: the put {step:?} must succeed, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
}

/// The `status` member of one job's entry in a reply, or `None` when the reply
/// carries no entry for that job.
fn job_status(reply: &Reply, job: &str) -> Option<String> {
    reply
        .body
        .get("status")
        .and_then(|status| status.get(job))
        .and_then(|entry| entry.get("status"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// One job's entry in a reply.
fn job_entry<'reply>(reply: &'reply Reply, job: &str) -> &'reply Value {
    reply
        .body
        .get("status")
        .and_then(|status| status.get(job))
        .unwrap_or_else(|| panic!("the reply carries no entry for {job}: {body}", body = reply.body))
}

/// The jobs a reply reports, as a sorted list of their names.
fn reported_jobs(reply: &Reply) -> Vec<String> {
    let mut jobs: Vec<String> = reply
        .body
        .get("status")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("the reply carries no status map: {body}", body = reply.body))
        .keys()
        .cloned()
        .collect();
    jobs.sort();
    jobs
}

#[tokio::test]
async fn every_job_reports_the_status_of_the_task_in_force_for_it() {
    // Each row: the case name, the region, the puts applied after the table is
    // created, and the status every job must report straight afterwards — a task
    // that runs has not completed the run of its current eligible period yet,
    // whether that period began when the table was created or when a put turned
    // the task back on.
    let cases: Vec<(&str, &str, Vec<Configure>, [&str; 3])> = vec![
        (
            "no configuration at all",
            "us-east-1",
            Vec::new(),
            [NOT_YET_RUN, NOT_YET_RUN, NOT_YET_RUN],
        ),
        (
            "compaction turned off on the table",
            "us-east-1",
            vec![Configure::Table("icebergCompaction", "disabled")],
            [DISABLED, NOT_YET_RUN, NOT_YET_RUN],
        ),
        (
            "snapshot management turned off on the table",
            "us-east-1",
            vec![Configure::Table("icebergSnapshotManagement", "disabled")],
            [NOT_YET_RUN, DISABLED, NOT_YET_RUN],
        ),
        (
            "unreferenced file removal turned off on the bucket",
            "us-east-1",
            vec![Configure::Bucket(
                "icebergUnreferencedFileRemoval",
                "disabled",
            )],
            [NOT_YET_RUN, NOT_YET_RUN, DISABLED],
        ),
        (
            "every task turned off",
            "eu-west-2",
            vec![
                Configure::Table("icebergCompaction", "disabled"),
                Configure::Table("icebergSnapshotManagement", "disabled"),
                Configure::Bucket("icebergUnreferencedFileRemoval", "disabled"),
            ],
            [DISABLED, DISABLED, DISABLED],
        ),
        (
            "compaction turned off and on again",
            "ap-south-1",
            vec![
                Configure::Table("icebergCompaction", "disabled"),
                Configure::Table("icebergCompaction", "enabled"),
            ],
            [NOT_YET_RUN, NOT_YET_RUN, NOT_YET_RUN],
        ),
        (
            "unreferenced file removal turned off and on again",
            "us-west-2",
            vec![
                Configure::Bucket("icebergUnreferencedFileRemoval", "disabled"),
                Configure::Bucket("icebergUnreferencedFileRemoval", "enabled"),
            ],
            [NOT_YET_RUN, NOT_YET_RUN, NOT_YET_RUN],
        ),
        (
            "settings put on a task that stays on",
            "us-east-1",
            vec![Configure::TableWithSettings(
                "icebergSnapshotManagement",
                "enabled",
                json!({
                    "icebergSnapshotManagement": {
                        "minSnapshotsToKeep": 3,
                        "maxSnapshotAgeHours": 48
                    }
                }),
            )],
            [NOT_YET_RUN, NOT_YET_RUN, NOT_YET_RUN],
        ),
    ];

    let emulator = Emulator::new();
    for (index, (case, region, steps, expected)) in cases.iter().enumerate() {
        let bucket = format!("job-status-{index}");
        let namespace = "ns";
        let table = "t";
        // The instant the table could not have been maintained before, so a run
        // this case does see reported is one scheduled for THIS table.
        let created_at = super::harness::now_seconds();
        let (bucket_arn, table_arn) =
            create_table(&emulator, region, &bucket, namespace, table).await;
        for step in steps {
            configure(
                &emulator, region, &bucket_arn, namespace, table, step, case,
            )
            .await;
        }

        let reply = emulator
            .get_table_maintenance_job_status(region, &bucket_arn, namespace, table)
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: the read must succeed, got {status} {body}",
            status = reply.status,
            body = reply.body
        );
        assert_eq!(
            reply.text("tableARN"),
            table_arn,
            "case {case:?}: the read must report the table's own ARN, got {body}",
            body = reply.body
        );
        assert_eq!(
            reported_jobs(&reply),
            EVERY_JOB
                .iter()
                .map(|job| job.to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: the read must report every modeled job, got {body}",
            body = reply.body
        );
        for (job, expected_status) in EVERY_JOB.iter().zip(expected) {
            let entry = job_entry(&reply, job);
            let actual = job_status(&reply, job);
            if *expected_status == DISABLED {
                // A task that does not run reports exactly that, whenever the
                // read arrives, and claims no run: its eligible period was over
                // before a run of it could complete.
                assert_eq!(
                    actual.as_deref(),
                    Some(DISABLED),
                    "case {case:?}: expected {job} to report {DISABLED}, got {entry}"
                );
                assert!(
                    entry.get("lastRunTimestamp").is_none(),
                    "case {case:?}: {job} completed no run, so it must carry no \
                     lastRunTimestamp, got {entry}"
                );
            } else {
                // A task that DOES run has not completed the run of its current
                // eligible period when the read follows the put that began it —
                // and a host slow enough for the run to have completed in
                // between reports that run, with the instant it completed at,
                // never the status of a task that does not run.
                match actual.as_deref() {
                    Some(NOT_YET_RUN) => assert!(
                        entry.get("lastRunTimestamp").is_none(),
                        "case {case:?}: {job} has completed no run, so it must carry no \
                         lastRunTimestamp, got {entry}"
                    ),
                    Some(SUCCESSFUL) => {
                        let run_at =
                            super::harness::timestamp_seconds(entry, "lastRunTimestamp", case);
                        assert!(
                            run_at >= created_at,
                            "case {case:?}: expected {job}'s run instant to be at or after the \
                             table was created ({created_at}), got {run_at}"
                        );
                    }
                    other => panic!(
                        "case {case:?}: expected {job} to report {NOT_YET_RUN} (or the run it \
                         completed), got {other:?} in {entry}"
                    ),
                }
            }
            assert!(
                entry.get("failureMessage").is_none(),
                "case {case:?}: {job} must carry no failureMessage, got {entry}"
            );
        }
    }
}

#[tokio::test]
async fn a_jobs_scheduled_run_completes_and_the_read_reports_it() {
    let region = "us-east-1";
    let namespace = "ns";
    let table = "t";
    let emulator = Emulator::new();
    let before_creation = SystemTime::now();
    let (bucket_arn, _) = create_table(&emulator, region, "job-run", namespace, table).await;
    // One task is turned off while the others run, so this case covers both
    // sides of the rule at once: the job that completes its run and the job
    // that never starts one.
    configure(
        &emulator,
        region,
        &bucket_arn,
        namespace,
        table,
        &Configure::Table("icebergSnapshotManagement", "disabled"),
        "a run completes",
    )
    .await;

    let fresh = emulator
        .get_table_maintenance_job_status(region, &bucket_arn, namespace, table)
        .await;
    assert_eq!(
        job_status(&fresh, "icebergCompaction").as_deref(),
        Some(NOT_YET_RUN),
        "a table's compaction job has not run when the table has just been created, got {entry}",
        entry = job_entry(&fresh, "icebergCompaction")
    );

    // Poll for the CONDITION — the run having completed — rather than for a
    // span, and give it a deadline far longer than the run needs.
    let deadline = SystemTime::now() + RUN_DEADLINE;
    let completed = loop {
        let reply = emulator
            .get_table_maintenance_job_status(region, &bucket_arn, namespace, table)
            .await;
        if job_status(&reply, "icebergCompaction").as_deref() == Some(SUCCESSFUL) {
            break reply;
        }
        assert!(
            SystemTime::now() < deadline,
            "the table's compaction job never completed its first run: {body}",
            body = reply.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };

    // Every job whose task runs completes its run; the one that was turned off
    // keeps reporting that it does not run, and reports no run instant because
    // its period was over before a run of it could complete.
    for job in ["icebergCompaction", "icebergUnreferencedFileRemoval"] {
        assert_eq!(
            job_status(&completed, job).as_deref(),
            Some(SUCCESSFUL),
            "{job} runs on the table, so its scheduled run must complete, got {entry}",
            entry = job_entry(&completed, job)
        );
        let run_at = super::harness::timestamp_seconds(
            job_entry(&completed, job),
            "lastRunTimestamp",
            job,
        );
        let created_at = before_creation
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_secs_f64();
        assert!(
            run_at >= created_at,
            "case {job:?}: expected the run instant to be at or after the table was created \
             ({created_at}), got {run_at}"
        );
        assert!(
            run_at <= super::harness::now_seconds(),
            "case {job:?}: expected the run instant to be in the past, got {run_at}"
        );
    }
    assert_eq!(
        job_status(&completed, "icebergSnapshotManagement").as_deref(),
        Some(DISABLED),
        "a task that was turned off runs nothing, got {entry}",
        entry = job_entry(&completed, "icebergSnapshotManagement")
    );
    assert!(
        job_entry(&completed, "icebergSnapshotManagement")
            .get("lastRunTimestamp")
            .is_none(),
        "a task turned off before any run of it completed reports no run instant, got {entry}",
        entry = job_entry(&completed, "icebergSnapshotManagement")
    );

    // The read is a read: asking again answers the same run, and does not move
    // it on.
    let again = emulator
        .get_table_maintenance_job_status(region, &bucket_arn, namespace, table)
        .await;
    assert_eq!(
        job_entry(&again, "icebergCompaction"),
        job_entry(&completed, "icebergCompaction"),
        "two reads of a table nobody reconfigured must answer the same, got {first} then {second}",
        first = job_entry(&completed, "icebergCompaction"),
        second = job_entry(&again, "icebergCompaction")
    );

    // Turning a task off after its run completed leaves the run it completed to
    // report, under the status that says the task no longer runs.
    let ran_at = job_entry(&completed, "icebergCompaction")
        .get("lastRunTimestamp")
        .cloned()
        .expect("the completed run carries its instant");
    configure(
        &emulator,
        region,
        &bucket_arn,
        namespace,
        table,
        &Configure::Table("icebergCompaction", "disabled"),
        "a run completes",
    )
    .await;
    let suspended = emulator
        .get_table_maintenance_job_status(region, &bucket_arn, namespace, table)
        .await;
    assert_eq!(
        job_status(&suspended, "icebergCompaction").as_deref(),
        Some(DISABLED),
        "a task turned off after its run must report that it no longer runs, got {entry}",
        entry = job_entry(&suspended, "icebergCompaction")
    );
    assert_eq!(
        job_entry(&suspended, "icebergCompaction").get("lastRunTimestamp"),
        Some(&ran_at),
        "a task turned off after its run must still report that run, got {entry}",
        entry = job_entry(&suspended, "icebergCompaction")
    );

    // Turning it back on starts a new period, whose run has not completed yet.
    configure(
        &emulator,
        region,
        &bucket_arn,
        namespace,
        table,
        &Configure::Table("icebergCompaction", "enabled"),
        "a run completes",
    )
    .await;
    let restarted = emulator
        .get_table_maintenance_job_status(region, &bucket_arn, namespace, table)
        .await;
    assert_eq!(
        job_status(&restarted, "icebergCompaction").as_deref(),
        Some(NOT_YET_RUN),
        "a task turned back on has not completed the run of its new period, got {entry}",
        entry = job_entry(&restarted, "icebergCompaction")
    );
}

#[tokio::test]
async fn a_read_of_a_resource_the_store_does_not_hold_is_refused() {
    let region = "us-east-1";
    let namespace = "ns";
    let emulator = Emulator::new();
    let (bucket_arn, _) = create_table(&emulator, region, "job-status-404", namespace, "t").await;

    // The read addresses ONE resource, the table the bucket ARN, the namespace
    // and the name together name, so the refusal names the TABLE whichever part
    // of that address resolves to nothing.
    let cases: [(&str, String, &str, &str, &str); 3] = [
        (
            "a bucket the store does not hold",
            format!("arn:aws:s3tables:{region}:000000000000:bucket/absent-bucket"),
            namespace,
            "t",
            NO_TABLE,
        ),
        (
            "a namespace the bucket does not hold",
            bucket_arn.clone(),
            "absent",
            "t",
            NO_TABLE,
        ),
        (
            "a table the namespace does not hold",
            bucket_arn.clone(),
            namespace,
            "absent",
            NO_TABLE,
        ),
    ];

    for (case, arn, namespace, table, expected) in cases {
        let reply = emulator
            .get_table_maintenance_job_status(region, &arn, namespace, table)
            .await;
        assert_eq!(
            reply.status, 404,
            "case {case:?}: expected HTTP 404, got {status} {body}",
            status = reply.status,
            body = reply.body
        );
        assert_eq!(
            reply.error_code.as_deref(),
            Some("NotFoundException"),
            "case {case:?}: expected the modeled not-found error, got {code:?}",
            code = reply.error_code
        );
        assert_eq!(
            reply.message(),
            expected,
            "case {case:?}: expected the sentence {expected:?}, got {actual:?}",
            actual = reply.message()
        );
    }
}
