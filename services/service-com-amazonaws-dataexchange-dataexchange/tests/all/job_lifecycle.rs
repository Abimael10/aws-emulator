// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for what moves a job out of `WAITING`: `CancelJob`,
//! `StartJob` and the execution a start sets going, as `GetJob` reports them.
//!
//! Every test drives the service the way the emulator does: through
//! `MultiServiceRouter::dispatch`, with a real restJson1 request, so the route, the
//! status code, the boundary validator and the serialized body are all exercised.
//! The harness (`emulator`, `harness`, `call`, `seconds`) is the one `data_sets.rs`
//! already uses, and the job catalog is `jobs.rs`'s.
//!
//! Three rules are under test.
//!
//! 1. A cancel is admitted only on a job that has not been started: it moves the job
//!    to the terminal `State` `CANCELLED` and advances `UpdatedAt` to the
//!    cancellation instant.
//! 2. A start is admitted only on a job that has not been started: it moves the job
//!    to `IN_PROGRESS` and sets its execution going, and one execution span later the
//!    clock owes the job a terminal `State` — which it reaches whether or not anybody
//!    polls, so a client's wait-for-the-job loop terminates.
//! 3. A job operation that the job's current `State` does not admit is refused with
//!    the modelled `ConflictException` (409) and changes nothing; one naming no
//!    stored job is the modelled `ResourceNotFoundException` (404).
//!
//! The rows span what the rules generalise over: every `Type` the model admits and a
//! request can carry (so neither rule is keyed to one job's details), each `State` a
//! job can be in when a start or a cancel arrives, and both operations that act on a
//! job that has not started.

use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_dataexchange_dataexchange as dataexchange;
use smithy_server_runtime::MultiServiceRouter;

use crate::data_sets::{call, emulator, harness, seconds, Answer};
use crate::jobs::{create_job, data_set_with_revision, job_cases, ABSENT_ID, LAKE_FORMATION_TYPE};

/// The `State` values a job cannot leave on its own — what a client's wait-for-the-job
/// loop (and every SDK waiter) polls for.
///
/// Source: `smithy_query(query="shape", shape="State")` reports
/// `[WAITING, IN_PROGRESS, ERROR, COMPLETED, CANCELLED, TIMED_OUT]`, mirrored by
/// `crate::ops::types::STATE_VALUES`; `WAITING` and `IN_PROGRESS` are the two a job
/// still moves out of.
const TERMINAL_STATES: &[&str] = &["ERROR", "COMPLETED", "CANCELLED", "TIMED_OUT"];

/// How long a poll for a terminal state is given: the execution span plus room for a
/// loaded machine. A budget, so a `State` that never moves fails the test instead of
/// hanging it.
const POLL_BUDGET: Duration = Duration::from_secs(20);

/// How long a polling client waits between two reads.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// A gap left between two calls whose timestamps are compared, so the advance is
/// visible at the resolution the wire encoding carries rather than only at the
/// clock's.
const TIMESTAMP_GAP: Duration = Duration::from_millis(20);

/// How far past the execution span [`settle_past_the_execution_span`] reads the
/// clock. Any instant at or after the due one materialises the same outcome, so the
/// margin only keeps the test off the boundary.
const SETTLE_MARGIN_SECONDS: f64 = 0.5;

/// How closely a settled `UpdatedAt` must sit on the instant the execution came due.
///
/// The transition is owed at `started + the execution span`, so that — and not the
/// instant of the read that materialised it — is what `UpdatedAt` must carry.
const DUE_INSTANT_TOLERANCE_SECONDS: f64 = 1.0;

/// What a job of each `Type` reports in its `Errors` once its execution has failed —
/// which, in a router that mounts this service ALONE, is every type: there is no object
/// storage to read a source out of or to write a destination into.
///
/// This is the test's own statement of the catalog: every `Type` the model admits
/// (`smithy_query(query="shape", shape="Type")`, mirrored by
/// `crate::ops::types::TYPE_VALUES`) paired with the prerequisite its `Details` turn
/// on. A job whose content is an Amazon S3 object it reads reports that source — the
/// presigned upload, an asset source key, an asset's own snapshot; the two imports
/// whose source is neither an object nor an upload report the resource the model names
/// in its own `JobErrorLimitName` values; an export of NAMED assets reports the asset
/// its destination names, which the revision does not hold, before any bucket is
/// reached; and an export of a whole revision reports the destination bucket it cannot
/// write into.
///
/// The one unknown `Type` is a panic, so a value the model gains fails this suite
/// instead of passing it silently.
fn expected_failure(job_type: &str) -> ExpectedJobError {
    let unreadable = |source: &str| ExpectedJobError {
        message: format!("Resource not found. Unable to find source {source}."),
        resource_type: None,
        names_sources: false,
    };
    match job_type {
        "CREATE_S3_DATA_ACCESS_FROM_S3_BUCKET"
        | "EXPORT_ASSET_TO_SIGNED_URL"
        | "IMPORT_ASSET_FROM_API_GATEWAY_API"
        | "IMPORT_ASSET_FROM_SIGNED_URL" => unreadable("Amazon S3 object"),
        "IMPORT_ASSETS_FROM_LAKE_FORMATION_TAG_POLICY" => {
            unreadable("AWS Lake Formation data permission")
        }
        "IMPORT_ASSETS_FROM_REDSHIFT_DATA_SHARES" => unreadable("Amazon Redshift datashare"),
        // The source objects an import from Amazon S3 reads are named in the `Details`
        // member the model gives that job type's errors.
        "IMPORT_ASSETS_FROM_S3" => ExpectedJobError {
            names_sources: true,
            ..unreadable("Amazon S3 object")
        },
        // The asset the destination of `jobs.rs`'s catalog entry names is one no import
        // created, and the error states that identity in its own members.
        "EXPORT_ASSETS_TO_S3" => ExpectedJobError {
            message: "The requested resource was not found.".to_string(),
            resource_type: Some("ASSET"),
            names_sources: false,
        },
        // `destination-bucket`, which `jobs.rs`'s catalog names as the destination of
        // both export jobs and which no bucket of any object storage answers for.
        "EXPORT_REVISIONS_TO_S3" => ExpectedJobError {
            message: "Resource not found. Unable to find destination Amazon S3 bucket \
                      destination-bucket."
                .to_string(),
            resource_type: None,
            names_sources: false,
        },
        other => panic!("case {other}: the catalog names no failure for this Type"),
    }
}

/// The one `JobError` a failed execution of one job `Type` reports, as
/// [`expected_failure`] states it.
struct ExpectedJobError {
    /// The whole `Message`, stated independently of the crate's own constants.
    message: String,
    /// The `JobErrorResourceTypes` value the error names, where the resource it is
    /// about is one of them.
    resource_type: Option<&'static str>,
    /// Whether the error names the Amazon S3 sources it is about in `Details`, which
    /// the model gives an import from Amazon S3 the member for
    /// (`smithy_query(query="shape", shape="Details")` — `ImportAssetsFromS3JobErrorDetails`,
    /// a list of `AssetSourceEntry`; the other member carries an asset NAME, not a
    /// source).
    names_sources: bool,
}

/// Assert that `errors` is the `Errors` list of a job whose execution could not carry
/// its work out: one entry, the modelled not-found `Code`, and the message and members
/// [`expected_failure`] states for a job of `job_type`.
///
/// `LimitValue` is never absent — the model gives that member a `@default`, so it is
/// always serialized — while every member the expectation does not name describes a
/// different error class and must be absent.
fn assert_unreadable_source(job_type: &str, errors: &Value) {
    let expected = expected_failure(job_type);
    let entries = errors
        .as_array()
        .unwrap_or_else(|| panic!("case {job_type}: expected an Errors list, got {errors:?}"));
    assert_eq!(
        entries.len(),
        1,
        "case {job_type}: expected one job error, got {errors:?}"
    );
    let entry = &entries[0];
    assert_eq!(
        entry.get("Code").and_then(Value::as_str),
        Some("RESOURCE_NOT_FOUND_EXCEPTION"),
        "case {job_type}: expected the modelled not-found Code, got {entry:?}"
    );
    assert_eq!(
        entry.get("Message").and_then(Value::as_str),
        Some(expected.message.as_str()),
        "case {job_type}: expected the message of the prerequisite that failed, got {entry:?}"
    );
    if expected.names_sources {
        let named = entry
            .get("Details")
            .and_then(|details| details.get("ImportAssetsFromS3JobErrorDetails"))
            .and_then(Value::as_array)
            .unwrap_or_else(|| {
                panic!(
                    "case {job_type}: expected Details.ImportAssetsFromS3JobErrorDetails to \
                     name the source that could not be read, got {entry:?}"
                )
            });
        assert!(
            !named.is_empty()
                && named
                    .iter()
                    .all(|source| source.get("Bucket").is_some() && source.get("Key").is_some()),
            "case {job_type}: expected each named source to carry its Bucket and Key, \
             got {named:?}"
        );
    } else {
        assert!(
            entry.get("Details").is_none(),
            "case {job_type}: expected no Details on this job error, got {entry:?}"
        );
    }
    match expected.resource_type {
        Some(resource_type) => {
            assert_eq!(
                entry.get("ResourceType").and_then(Value::as_str),
                Some(resource_type),
                "case {job_type}: expected the error to name the kind of resource it is \
                 about, got {entry:?}"
            );
            assert!(
                entry
                    .get("ResourceId")
                    .and_then(Value::as_str)
                    .is_some_and(|id| !id.is_empty()),
                "case {job_type}: expected the error to name the resource it is about, \
                 got {entry:?}"
            );
        }
        None => {
            for absent in ["ResourceId", "ResourceType"] {
                assert!(
                    entry.get(absent).is_none(),
                    "case {job_type}: expected no {absent} on this job error, got {entry:?}"
                );
            }
        }
    }
    assert!(
        entry.get("LimitName").is_none(),
        "case {job_type}: expected no LimitName on a job error about no limit, got {entry:?}"
    );
}

pub(crate) async fn start_job(router: &MultiServiceRouter, job_id: &str) -> Answer {
    call(router, "PATCH", &format!("/v1/jobs/{job_id}"), None).await
}

pub(crate) async fn cancel_job(router: &MultiServiceRouter, job_id: &str) -> Answer {
    call(router, "DELETE", &format!("/v1/jobs/{job_id}"), None).await
}

pub(crate) async fn get_job(router: &MultiServiceRouter, job_id: &str) -> Answer {
    call(router, "GET", &format!("/v1/jobs/{job_id}"), None).await
}

/// Create one job of `case`'s `Type` against the data set and revision the caller
/// prepared, and hand back its `Id` and `CreatedAt`.
pub(crate) async fn job_of_type(
    router: &MultiServiceRouter,
    job_type: &str,
    member: &str,
    detail: Value,
) -> (String, f64) {
    let created = create_job(router, job_type, json!({ member: detail }), None).await;
    assert_eq!(
        created.status, 201,
        "case {job_type}: expected 201 from CreateJob, got {} body {:?}",
        created.status, created.body
    );
    assert_eq!(
        created.string("State"),
        "WAITING",
        "case {job_type}: expected a created job to be WAITING, got {:?}",
        created.member("State")
    );
    let created_at = seconds(created.member("CreatedAt"), job_type);
    (created.string("Id"), created_at)
}

/// Apply, through the store, the ONE transition the clock owes every started job —
/// read at an instant past the execution span, so a test does not spend the span in
/// wall-clock time. `polling_a_started_job_reaches_a_terminal_state` is the test that
/// does spend it, and shows a client reaches the same state with nobody writing
/// state.
pub(crate) fn settle_past_the_execution_span(engine: &Arc<engine::InMemoryEngine>) {
    let state = dataexchange::state::service_state(&**engine).expect("the dataexchange store");
    let past = dataexchange::state::later(
        dataexchange::state::now(),
        dataexchange::state::JOB_EXECUTION_SECONDS + SETTLE_MARGIN_SECONDS,
    );
    state.lock().settle_jobs(past);
}

/// RULE: a cancel is admitted on a job that has not been started, whatever its
/// `Type`: the job moves to the terminal `State` `CANCELLED` and its `UpdatedAt`
/// advances to the cancellation instant, so a following read reports a job that is
/// over. A second cancel of the same job conflicts and changes nothing.
#[tokio::test]
async fn cancelling_a_waiting_job_moves_it_to_cancelled_for_every_job_type() {
    let router = emulator();
    let (data_set_id, revision_id) = data_set_with_revision(&router, "cancel-waiting").await;

    for case in job_cases(&data_set_id, &revision_id) {
        let label = case.job_type;
        let (id, created_at) = job_of_type(&router, case.job_type, case.member, case.detail).await;

        // The cancellation instant must be distinguishable from the creation one at
        // the resolution the response carries, so the two calls are spaced.
        sleep(TIMESTAMP_GAP);
        let cancelled = cancel_job(&router, &id).await;
        assert_eq!(
            cancelled.status, 204,
            "case {label}: expected 204 from CancelJob, got {} body {:?}",
            cancelled.status, cancelled.body
        );

        let fetched = get_job(&router, &id).await;
        assert_eq!(
            fetched.status, 200,
            "case {label}: expected 200 from GetJob, got {} body {:?}",
            fetched.status, fetched.body
        );
        assert_eq!(
            fetched.string("State"),
            "CANCELLED",
            "case {label}: expected State=CANCELLED after a cancel, got {:?}",
            fetched.member("State")
        );
        assert!(
            fetched.member("Errors").is_null(),
            "case {label}: expected no Errors on a job that never ran, got {:?}",
            fetched.member("Errors")
        );
        assert_eq!(
            seconds(fetched.member("CreatedAt"), label),
            created_at,
            "case {label}: expected CreatedAt to stay at the creation instant"
        );
        let cancelled_at = seconds(fetched.member("UpdatedAt"), label);
        assert!(
            cancelled_at > created_at,
            "case {label}: expected UpdatedAt to advance past CreatedAt, got {cancelled_at} vs {created_at}"
        );

        // A job already in a terminal state is not cancellable: the second cancel is
        // the modelled conflict, and the record it refused is untouched.
        sleep(TIMESTAMP_GAP);
        let again = cancel_job(&router, &id).await;
        assert_eq!(
            again.status, 409,
            "case {label}: expected 409 from a second CancelJob, got {} body {:?}",
            again.status, again.body
        );
        assert_eq!(
            again.error.as_deref(),
            Some("ConflictException"),
            "case {label}: expected ConflictException, got {:?}",
            again.error
        );
        let after = get_job(&router, &id).await;
        assert_eq!(
            after.string("State"),
            "CANCELLED",
            "case {label}: expected the refused cancel to change nothing, got {:?}",
            after.member("State")
        );
        assert_eq!(
            seconds(after.member("UpdatedAt"), label),
            cancelled_at,
            "case {label}: expected the refused cancel to leave UpdatedAt where it was"
        );
    }
}

/// RULE: a start moves a waiting job to `IN_PROGRESS` and sets its execution going;
/// one execution span later the clock owes the job a terminal `State`, which is
/// `ERROR` with one `JobError` naming the source the execution could not find — this
/// emulator holds no asset content for any job's source. The terminal `UpdatedAt` is
/// the instant the execution came due, not the instant of the read that reported it.
/// This holds for every `Type` the model admits.
#[tokio::test]
async fn a_started_job_runs_and_settles_in_a_terminal_state_for_every_job_type() {
    let (router, engine) = harness();
    let (data_set_id, revision_id) = data_set_with_revision(&router, "start-every-type").await;

    for case in job_cases(&data_set_id, &revision_id) {
        let label = case.job_type;
        let (id, created_at) = job_of_type(&router, case.job_type, case.member, case.detail).await;

        sleep(TIMESTAMP_GAP);
        let started = start_job(&router, &id).await;
        assert_eq!(
            started.status, 202,
            "case {label}: expected 202 from StartJob, got {} body {:?}",
            started.status, started.body
        );

        // The execution span is never zero, so a read straight after the start sees
        // the job running rather than already finished.
        let running = get_job(&router, &id).await;
        assert_eq!(
            running.string("State"),
            "IN_PROGRESS",
            "case {label}: expected State=IN_PROGRESS straight after StartJob, got {:?}",
            running.member("State")
        );
        assert!(
            running.member("Errors").is_null(),
            "case {label}: expected no Errors on a job that is still running, got {:?}",
            running.member("Errors")
        );
        let started_at = seconds(running.member("UpdatedAt"), label);
        assert!(
            started_at > created_at,
            "case {label}: expected UpdatedAt to advance on the start, got {started_at} vs {created_at}"
        );

        // A job runs once: neither a start nor a cancel is admitted while it runs.
        for (operation, answer) in [
            ("StartJob", start_job(&router, &id).await),
            ("CancelJob", cancel_job(&router, &id).await),
        ] {
            assert_eq!(
                answer.status, 409,
                "case {label}: expected 409 from {operation} on a running job, got {} body {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error.as_deref(),
                Some("ConflictException"),
                "case {label}: expected ConflictException from {operation}, got {:?}",
                answer.error
            );
        }

        settle_past_the_execution_span(&engine);
        let settled = get_job(&router, &id).await;
        assert_eq!(
            settled.status, 200,
            "case {label}: expected 200 from GetJob, got {} body {:?}",
            settled.status, settled.body
        );
        assert_eq!(
            settled.string("State"),
            "ERROR",
            "case {label}: expected the execution to settle in ERROR, got {:?}",
            settled.member("State")
        );
        assert_unreadable_source(case.job_type, settled.member("Errors"));
        let settled_at = seconds(settled.member("UpdatedAt"), label);
        let span = dataexchange::state::JOB_EXECUTION_SECONDS;
        assert!(
            (settled_at - started_at - span).abs() < DUE_INSTANT_TOLERANCE_SECONDS,
            "case {label}: expected UpdatedAt at the instant the execution came due \
             ({} + {span}), got {settled_at}",
            started_at
        );
        assert_eq!(
            seconds(settled.member("CreatedAt"), label),
            created_at,
            "case {label}: expected CreatedAt to stay at the creation instant"
        );

        // The terminal state holds: a second read reports the same record, and
        // neither operation is admitted on a job that is over.
        let reread = get_job(&router, &id).await;
        assert_eq!(
            seconds(reread.member("UpdatedAt"), label),
            settled_at,
            "case {label}: expected a second read to report the same settled record"
        );
        for (operation, answer) in [
            ("StartJob", start_job(&router, &id).await),
            ("CancelJob", cancel_job(&router, &id).await),
        ] {
            assert_eq!(
                answer.status, 409,
                "case {label}: expected 409 from {operation} on a finished job, got {} body {:?}",
                answer.status, answer.body
            );
        }
    }
}

/// RULE: the terminal state a started job reaches is owed by the clock, not by a
/// reader, so a client that only polls `GetJob` reaches it — every `Type` included.
/// This is the loop that used to poll forever.
#[tokio::test]
async fn polling_a_started_job_reaches_a_terminal_state() {
    let router = emulator();
    let (data_set_id, revision_id) = data_set_with_revision(&router, "poll-to-terminal").await;

    // Every job is started before any is polled, so the whole table costs about one
    // execution span of wall clock rather than one span per row.
    let mut started: Vec<(&'static str, String)> = Vec::new();
    for case in job_cases(&data_set_id, &revision_id) {
        let (id, _) = job_of_type(&router, case.job_type, case.member, case.detail).await;
        let answer = start_job(&router, &id).await;
        assert_eq!(
            answer.status, 202,
            "case {}: expected 202 from StartJob, got {} body {:?}",
            case.job_type, answer.status, answer.body
        );
        started.push((case.job_type, id));
    }

    for (label, id) in started {
        let deadline = Instant::now() + POLL_BUDGET;
        let settled = loop {
            let answer = get_job(&router, &id).await;
            assert_eq!(
                answer.status, 200,
                "case {label}: expected 200 from GetJob while polling, got {} body {:?}",
                answer.status, answer.body
            );
            let state = answer.string("State");
            if TERMINAL_STATES.contains(&state.as_str()) {
                break answer;
            }
            assert!(
                Instant::now() < deadline,
                "case {label}: expected a terminal state within {POLL_BUDGET:?}, it is still {state:?}"
            );
            sleep(POLL_INTERVAL);
        };
        assert_eq!(
            settled.string("State"),
            "ERROR",
            "case {label}: expected the execution to settle in ERROR, got {:?}",
            settled.member("State")
        );
        assert_unreadable_source(label, settled.member("Errors"));
    }
}

/// RULE: a start or a cancel naming no stored job is the modelled
/// `ResourceNotFoundException` (404) reporting the job it could not find.
#[tokio::test]
async fn a_job_operation_naming_an_absent_job_is_not_found() {
    let router = emulator();

    for (operation, answer) in [
        ("StartJob", start_job(&router, ABSENT_ID).await),
        ("CancelJob", cancel_job(&router, ABSENT_ID).await),
    ] {
        assert_eq!(
            answer.status, 404,
            "case {operation}: expected 404 for an absent job, got {} body {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error.as_deref(),
            Some("ResourceNotFoundException"),
            "case {operation}: expected ResourceNotFoundException, got {:?}",
            answer.error
        );
        assert_eq!(
            answer.member("ResourceId").as_str(),
            Some(ABSENT_ID),
            "case {operation}: expected the refusal to name the job, got {:?}",
            answer.member("ResourceId")
        );
        assert_eq!(
            answer.member("ResourceType").as_str(),
            Some("JOB"),
            "case {operation}: expected ResourceType=JOB, got {:?}",
            answer.member("ResourceType")
        );
    }
}

/// RULE: the failure catalog these tests assert against covers every `Type` the model
/// admits, so no job type is left without a stated outcome.
#[tokio::test]
async fn the_source_catalog_covers_every_modelled_job_type() {
    // `expected_failure` panics on a `Type` it does not state an outcome for, so asking
    // it for every modelled value is what pins the coverage.
    for job_type in dataexchange::ops::types::TYPE_VALUES {
        let stated = expected_failure(job_type);
        assert!(
            !stated.message.is_empty(),
            "case {job_type}: expected a stated failure message for this Type"
        );
    }

    // Every one of them but the Lake Formation import is exercised above; that one
    // carries a `Details` member no request can satisfy (its `CatalogId` pattern
    // admits nothing), as `jobs.rs` pins, so no job of it can be created to start.
    let exercised: Vec<&str> = job_cases(ABSENT_ID, ABSENT_ID)
        .iter()
        .map(|case| case.job_type)
        .collect();
    for job_type in dataexchange::ops::types::TYPE_VALUES {
        assert!(
            exercised.contains(job_type) || *job_type == LAKE_FORMATION_TYPE,
            "case {job_type}: expected the job catalog to exercise this Type"
        );
    }
}
