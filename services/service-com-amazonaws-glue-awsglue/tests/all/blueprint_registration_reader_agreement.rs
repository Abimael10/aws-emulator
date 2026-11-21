// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHO reports the status a blueprint registration came to rest in, and
//! for a start of a blueprint that nothing ever read.
//!
//! The rule under test: `CreateBlueprint` answers only `Name`, and the archive published at
//! `BlueprintLocation` is fetched afterwards, so the registration leaves `CREATING` for the
//! terminal status that fetch decides — `ACTIVE` when the object store serves the object the
//! location names, and `FAILED` carrying `Access denied on object at location:
//! <BlueprintLocation>.` when it answers that it does not. That status belongs to the RECORD,
//! not to one reader of it: every operation that reports a blueprint
//! (`GetBlueprint`, `BatchGetBlueprints`) reports the same status and the same `ErrorMessage`,
//! whichever of them is called first, and `StartBlueprintRun` consults it itself — so a
//! blueprint whose registration failed is refused with `IllegalBlueprintStateException` and
//! backs no run even when NOTHING ever read it.
//!
//! Where [`crate::blueprint_archive_registration`] tables the archive forms a registration
//! settles on and [`crate::blueprint_registration_terminal_status`] pins the sentence and the
//! terminality of what it settles on — both through `GetBlueprint` alone, and both reading the
//! record before any other call — this suite pins what neither does: that the BATCH reader
//! agrees with the single read and materialises the owed status on its own, and that a start
//! of a never-read registration is answered from the record rather than from whatever a read
//! had already materialised.
//!
//! Amazon S3 is mounted beside Glue on one engine, so the archive a row stages with
//! `PutObject` is the object the registration reads, and a location nothing stages names an
//! object the store really does not hold.
//!
//! The due instant the registration settles at is moved THROUGH THE STORE rather than slept
//! for, so every transition here is deterministic and no outcome depends on how fast the host
//! runs.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    BLUEPRINT_STATUS_ACTIVE, BLUEPRINT_STATUS_CREATING, BLUEPRINT_STATUS_FAILED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::blueprint_archive_registration::{
    make_settlement_due, read, stage_archive, PUBLISHED_ARCHIVE,
};
use crate::job_script_execution::{create_bucket, emulator_with_s3, BUCKET};
use crate::wire::{call, error_code, error_message, ok};

/// The role a run would be started as, built as the modelled `RoleArn` pattern
/// `^arn:aws[^:]*:iam::[0-9]*:role/.+$` requires.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/sim2real-glue-blueprint-reader";

/// The sentence a registration that could not fetch its published archive reports, as AWS
/// words it: fixed text, then the `BlueprintLocation` the request named, then a period — the
/// location being its one request-supplied part. Spelled out here rather than taken from the
/// service's own helper, so the wording is pinned independently of it.
fn expected_failure_message(location: &str) -> String {
    format!("Access denied on object at location: {location}.")
}

/// The refusal a start of a blueprint with no registered definition reports: fixed text plus
/// the blueprint the start named. Spelled out for the same reason as
/// [`expected_failure_message`].
fn expected_no_definition_message(name: &str) -> String {
    format!("No active definition found for Blueprint: {name}")
}

/// Register `name` at `location`, which answers only the name it stored.
async fn register(router: &MultiServiceRouter, name: &str, location: &str) {
    let answered = ok(
        router,
        "CreateBlueprint",
        &json!({ "Name": name, "BlueprintLocation": location }),
    )
    .await;
    assert_eq!(
        answered.get("Name").and_then(Value::as_str),
        Some(name),
        "case {name}: the create reports the name it registered, got {answered}"
    );
}

/// The `Blueprint` the BATCH reader reports for `name`, asked for on its own.
async fn read_batched(router: &MultiServiceRouter, name: &str) -> Value {
    let body = ok(router, "BatchGetBlueprints", &json!({ "Names": [name] })).await;
    let reported = body
        .get("Blueprints")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        reported.len(),
        1,
        "case {name}: the batch must report the one registration it was asked for, got {body}"
    );
    reported
        .into_iter()
        .next()
        .expect("the batch reported one registration")
}

/// The `Status` the reported blueprint carries.
fn status_of(case: &str, blueprint: &Value) -> String {
    blueprint
        .get("Status")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case}: every registration reports a Status, got {blueprint}")
        })
        .to_string()
}

/// The `ErrorMessage` the reported blueprint carries, and `None` when it carries none.
fn error_message_of(blueprint: &Value) -> Option<String> {
    blueprint
        .get("ErrorMessage")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// One row: where the blueprint is published, and whether the object store serves it.
struct ReaderCase {
    /// What this row varies, named in every assertion it drives.
    case: &'static str,
    /// The key to stage in the suite's bucket before the create, and `None` for a row whose
    /// location names an object the store never holds.
    staged: Option<&'static str>,
    /// The key the create names when the row stages nothing.
    unstaged_key: Option<&'static str>,
    /// Whether the location names the suite's bucket, or one nothing created.
    in_the_suites_bucket: bool,
    /// The `BlueprintStatus` the registration must come to rest in.
    settles_to: &'static str,
}

/// The rows: locations differing along how deep the key is, whether the object store holds
/// the object, and whether the bucket itself exists — two the store serves, two it does not.
fn reader_cases() -> Vec<ReaderCase> {
    vec![
        ReaderCase {
            case: "an archive staged at a single key segment",
            staged: Some("agreed-served.zip"),
            unstaged_key: None,
            in_the_suites_bucket: true,
            settles_to: BLUEPRINT_STATUS_ACTIVE,
        },
        ReaderCase {
            case: "an archive staged under several key segments",
            staged: Some("agreed/nested/deeper/served.zip"),
            unstaged_key: None,
            in_the_suites_bucket: true,
            settles_to: BLUEPRINT_STATUS_ACTIVE,
        },
        ReaderCase {
            case: "a key the suite's bucket does not hold",
            staged: None,
            unstaged_key: Some("agreed/never-published.zip"),
            in_the_suites_bucket: true,
            settles_to: BLUEPRINT_STATUS_FAILED,
        },
        ReaderCase {
            case: "a key in a bucket nothing created",
            staged: None,
            unstaged_key: Some("no-such-bucket-for-reader-agreement/agreed/archive.zip"),
            in_the_suites_bucket: false,
            settles_to: BLUEPRINT_STATUS_FAILED,
        },
    ]
}

/// The location a row registers at, staging its archive first when it has one.
async fn location_of(router: &MultiServiceRouter, row: &ReaderCase) -> String {
    match (row.staged, row.unstaged_key) {
        (Some(key), _) => stage_archive(router, key, PUBLISHED_ARCHIVE.to_vec()).await,
        (None, Some(key)) if row.in_the_suites_bucket => format!("s3://{BUCKET}/{key}"),
        (None, Some(key)) => format!("s3://{key}"),
        (None, None) => panic!("case {}: a row names a location", row.case),
    }
}

/// Every reader of a blueprint reports the same registration: the status in progress until
/// the clock owes the terminal one, then the terminal status its own archive decided and, for
/// a registration that could not read its archive, the sentence naming the location it could
/// not read.
#[tokio::test]
async fn both_readers_report_the_status_each_registration_settles_on() {
    let (router, engine) = emulator_with_s3();
    create_bucket(&router).await;

    for (index, row) in reader_cases().iter().enumerate() {
        let case = row.case;
        let name = format!("reader-agreement-{index}");
        let location = location_of(&router, row).await;
        register(&router, &name, &location).await;

        // Before the clock owes it anything, both readers report the registration being made
        // and neither reports an error.
        for (reader, registering) in [
            ("GetBlueprint", read(&router, &name).await),
            ("BatchGetBlueprints", read_batched(&router, &name).await),
        ] {
            assert_eq!(
                status_of(case, &registering),
                BLUEPRINT_STATUS_CREATING,
                "case {case} ({reader}): a registration in progress reports \
                 {BLUEPRINT_STATUS_CREATING}, got {registering}"
            );
            assert_eq!(
                error_message_of(&registering),
                None,
                "case {case} ({reader}): a registration that has not settled carries no \
                 ErrorMessage, got {registering}"
            );
        }

        make_settlement_due(&engine, &name);
        let single = read(&router, &name).await;
        let batched = read_batched(&router, &name).await;
        let expected_error = (row.settles_to == BLUEPRINT_STATUS_FAILED)
            .then(|| expected_failure_message(&location));
        for (reader, settled) in [("GetBlueprint", &single), ("BatchGetBlueprints", &batched)] {
            assert_eq!(
                status_of(case, settled),
                row.settles_to,
                "case {case} ({reader}): expected the registration to come to rest {}, got \
                 {settled}",
                row.settles_to
            );
            assert_eq!(
                error_message_of(settled),
                expected_error,
                "case {case} ({reader}): expected the ErrorMessage {expected_error:?}, got \
                 {settled}"
            );
        }
        assert_eq!(
            batched, single,
            "case {case}: the two readers must report the same registration; the batch \
             {batched} against the single read {single}"
        );
    }
}

/// The batch reader materialises the status the clock had already owed the registration on
/// its own: it reports the terminal status for a record NOTHING read before it, and the
/// single read that follows reports that same record — so the status came from the clock, not
/// from having been read.
#[tokio::test]
async fn the_batch_reader_materialises_the_status_the_clock_owed() {
    let (router, engine) = emulator_with_s3();
    create_bucket(&router).await;

    let served = stage_archive(&router, "batch-first/served.zip", PUBLISHED_ARCHIVE.to_vec()).await;
    let unserved = format!("s3://{BUCKET}/batch-first/never-served.zip");
    let rows = [
        (
            "an archive the object store serves",
            "batch-first-served",
            served,
            BLUEPRINT_STATUS_ACTIVE,
        ),
        (
            "an archive the object store does not hold",
            "batch-first-unserved",
            unserved,
            BLUEPRINT_STATUS_FAILED,
        ),
    ];
    for (_, name, location, _) in &rows {
        register(&router, name, location).await;
        make_settlement_due(&engine, name);
    }

    // ONE batch for both names, and it is the FIRST read either record has had.
    let body = ok(
        &router,
        "BatchGetBlueprints",
        &json!({ "Names": rows.iter().map(|(_, name, _, _)| *name).collect::<Vec<_>>() }),
    )
    .await;
    let reported = body
        .get("Blueprints")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        reported.len(),
        rows.len(),
        "the batch must report one registration per requested name, got {body}"
    );

    for (position, (case, name, location, settles_to)) in rows.iter().enumerate() {
        let batched = &reported[position];
        assert_eq!(
            batched.get("Name").and_then(Value::as_str),
            Some(*name),
            "case {case}: the batch reports the registrations in the request's order, got {body}"
        );
        assert_eq!(
            status_of(case, batched),
            *settles_to,
            "case {case}: the batch must report the status the clock owed the registration \
             ({settles_to}), got {batched}"
        );
        let expected_error = (*settles_to == BLUEPRINT_STATUS_FAILED)
            .then(|| expected_failure_message(location));
        assert_eq!(
            error_message_of(batched),
            expected_error,
            "case {case}: expected the ErrorMessage {expected_error:?}, got {batched}"
        );

        let single = read(&router, name).await;
        assert_eq!(
            &single, batched,
            "case {case}: the single read after the batch must report the same record; the \
             batch {batched} against {single}"
        );
    }
}

/// A start consults the registration itself: a blueprint whose archive could not be read is
/// refused and backs no run even though nothing ever read it, while one whose archive was
/// served backs exactly one run — and the refusal leaves the registration reporting what it
/// came to rest in.
#[tokio::test]
async fn a_start_consults_the_registration_nothing_ever_read() {
    let (router, engine) = emulator_with_s3();
    create_bucket(&router).await;

    let served = stage_archive(&router, "unread/served.zip", PUBLISHED_ARCHIVE.to_vec()).await;
    let unserved = format!("s3://{BUCKET}/unread/never-served.zip");

    // The registration that failed: started with NO read of the record in between.
    let failed_case = "a registration whose archive the object store does not hold";
    let failed_name = "unread-unserved";
    register(&router, failed_name, &unserved).await;
    make_settlement_due(&engine, failed_name);
    let (status, body) = call(
        &router,
        "StartBlueprintRun",
        &json!({ "BlueprintName": failed_name, "RoleArn": ROLE_ARN }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {failed_case}: a run of a registration that failed is refused, got {status} with \
         {body}"
    );
    assert!(
        error_code(&body).contains("IllegalBlueprintStateException"),
        "case {failed_case}: expected IllegalBlueprintStateException, got {body}"
    );
    assert_eq!(
        error_message(&body),
        expected_no_definition_message(failed_name),
        "case {failed_case}: expected {:?}, got {body}",
        expected_no_definition_message(failed_name)
    );
    let runs = ok(
        &router,
        "GetBlueprintRuns",
        &json!({ "BlueprintName": failed_name }),
    )
    .await;
    assert_eq!(
        runs.get("BlueprintRuns")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0),
        "case {failed_case}: a refused start creates no run, got {runs}"
    );
    // The refused start neither moved the registration nor dropped what it reports.
    let refused_record = read(&router, failed_name).await;
    assert_eq!(
        status_of(failed_case, &refused_record),
        BLUEPRINT_STATUS_FAILED,
        "case {failed_case}: the registration must still report {BLUEPRINT_STATUS_FAILED}, got \
         {refused_record}"
    );
    assert_eq!(
        error_message_of(&refused_record),
        Some(expected_failure_message(&unserved)),
        "case {failed_case}: the registration must still report the location it could not \
         read, got {refused_record}"
    );

    // The control: a registration whose archive WAS served backs a run, started the same way
    // with no read of the record in between.
    let served_case = "a registration whose archive the object store serves";
    let served_name = "unread-served";
    register(&router, served_name, &served).await;
    make_settlement_due(&engine, served_name);
    let started = ok(
        &router,
        "StartBlueprintRun",
        &json!({ "BlueprintName": served_name, "RoleArn": ROLE_ARN }),
    )
    .await;
    let run_id = started
        .get("RunId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {served_case}: a started run reports its RunId, got {started}")
        })
        .to_string();
    assert!(
        !run_id.is_empty(),
        "case {served_case}: the run id a start mints is not empty, got {started}"
    );
    let runs = ok(
        &router,
        "GetBlueprintRuns",
        &json!({ "BlueprintName": served_name }),
    )
    .await;
    let recorded: Vec<String> = runs
        .get("BlueprintRuns")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|run| {
            run.get("RunId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(
        recorded,
        vec![run_id.clone()],
        "case {served_case}: the blueprint must hold the one run the start recorded ({run_id}), \
         got {runs}"
    );
}
