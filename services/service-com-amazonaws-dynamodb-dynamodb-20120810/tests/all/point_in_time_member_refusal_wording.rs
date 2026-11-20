// SPDX-License-Identifier: CC0-1.0

//! What the point-in-time surface's refusals of a request's own MEMBERS say.
//!
//! The rule pinned here: a refusal carries the sentence the service states for that error
//! class — AWS's fixed text, with only the values the request and the emulator's state
//! supply filled in — rather than a paraphrase of it or the request validator's generic
//! clause about a null member. A client that reads the message is told which pair of
//! members contradicted each other, or which member the call cannot be made without; a
//! generic `Member must not be null` clause names neither.
//!
//! The refusals pinned, each over a SET of inputs rather than one example:
//!
//! * `RestoreTableToPointInTime` naming BOTH a point in time and the newest restorable one
//!   — two selectors that name different instants, so the request names no single point;
//! * `ExportTableToPointInTime` asking for an INCREMENTAL export with no period at all;
//! * one whose period names no start;
//! * and a FULL export that carries a period, which is the incremental export's own member.
//!
//! The rows differ along what each rule generalises over: which instant the contradicting
//! restore carries, which members of the period the request does state, and whether the
//! full export states its kind outright or leaves it to the documented default. Each row
//! also asserts the refusal RECORDED nothing, so the wording is of a call that did not
//! happen.
//!
//! Every call is driven over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same `MultiServiceRouter`
//! the emulator's front doors use, with Amazon S3 mounted beside this service on ONE engine
//! so an export that IS served has a destination that really exists. Every wait is a poll
//! for the condition it waits on.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_dynamodb_dynamodb_20120810::state;

use crate::emulator::{
    call, create_bucket, describe_when_active, emulator, emulator_with_s3, error_code, message, ok,
    restorable_window, POLL_INTERVAL, REGION,
};

/// The bucket every export of this suite names as its destination.
const BUCKET: &str = "point-in-time-refusal-wording";

/// How long a poll for the restorable window to reach the span a row needs may take before
/// this suite calls it a hang. Generous slack over the emulator's own transient span: a
/// loaded machine is not a defect, and the point of the bound is that the wait ENDS.
const WINDOW_DEADLINE: Duration = Duration::from_secs(60);

/// The refusal of a restore that names a point in time AND the newest restorable one.
const BOTH_RESTORE_SELECTORS: &str =
    "Invalid Request: Both RestoreDateTime and UseLatestRestorableTime cannot be set for \
     point-in-time restore request. Please specify either RestoreDateTime time or \
     UseLatestRestorableTime";

/// The refusal of an incremental export that states no period.
const SPECIFICATION_EXPECTED: &str = "Invalid Request: Incremental Export Specification expected \
                                      when export type is set as Incremental Export";

/// The refusal of an incremental period that names no start.
const EXPORT_FROM_TIME_EXPECTED: &str =
    "Invalid Request: ExportFromTime must be provided in IncrementalExportSpecification";

/// The refusal of a FULL export that carries an incremental period.
const SPECIFICATION_UNEXPECTED: &str = "Invalid Request: When Export Type is Full Export , \
                                        Incremental Export Specification should not be provided";

/// One active table whose point-in-time recovery is on.
async fn table_with_recovery(router: &MultiServiceRouter, name: &str) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(router, name, name).await;
    ok(
        router,
        "PutItem",
        json!({"TableName": name, "Item": {"pk": {"S": "only"}}}),
    )
    .await;
    ok(
        router,
        "UpdateContinuousBackups",
        json!({
            "TableName": name,
            "PointInTimeRecoverySpecification": {"PointInTimeRecoveryEnabled": true},
        }),
    )
    .await;
}

/// Poll `DescribeContinuousBackups` until the restorable window spans `span`, and answer
/// the window it then reports: the window widens on its own as the recovery catches up, so
/// the condition becomes true without anything being written.
async fn window_spanning(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    span: Duration,
) -> (f64, f64) {
    let started = std::time::Instant::now();
    loop {
        let window = restorable_window(router, name).await;
        if window.1 - window.0 >= span.as_secs_f64() {
            return window;
        }
        assert!(
            started.elapsed() < WINDOW_DEADLINE,
            "case {case:?}: the window of {name:?} never spanned {span:?}, last was {window:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One token no other call of this suite uses, so each export is a NEW one rather than a
/// replay of the row before it.
fn fresh_token() -> String {
    static NEXT: ::std::sync::atomic::AtomicUsize = ::std::sync::atomic::AtomicUsize::new(0);
    format!(
        "refusal-wording-{}",
        NEXT.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed)
    )
}

/// An export request for `table` carrying the two members every export states, plus `extra`.
fn export_body(table: &str, extra: Value) -> Value {
    let mut body = json!({
        "TableArn": state::table_arn(REGION, table),
        "S3Bucket": BUCKET,
        "ClientToken": fresh_token(),
    });
    for (member, value) in extra.as_object().cloned().unwrap_or_default() {
        body[member] = value;
    }
    body
}

/// How many exports the catalogue holds for `table`.
async fn recorded_exports(router: &MultiServiceRouter, table: &str) -> usize {
    ok(
        router,
        "ListExports",
        json!({"TableArn": state::table_arn(REGION, table)}),
    )
    .await["ExportSummaries"]
        .as_array()
        .map_or(0, Vec::len)
}

#[tokio::test]
async fn a_restore_naming_two_points_in_time_is_refused_with_the_services_own_sentence() {
    let router = emulator();
    let table = "SelectorsNamed";
    table_with_recovery(&router, table).await;
    let (earliest, latest) = restorable_window(&router, table).await;

    // Rows differ in the instant the contradicting request carries: inside the window, far
    // before it and far after it. The contradiction is between the two SELECTORS, so it is
    // settled whatever instant one of them names.
    let instants: [(&str, f64); 3] = [
        ("an instant inside the window", latest),
        ("an instant years before the window", earliest - 365.0 * 24.0 * 60.0 * 60.0),
        ("an instant in the future", latest + 3_600.0),
    ];

    for (case, instant) in instants {
        let (status, answered) = call(
            &router,
            "RestoreTableToPointInTime",
            json!({
                "SourceTableName": table,
                "TargetTableName": "SelectorsRestored",
                "RestoreDateTime": instant,
                "UseLatestRestorableTime": true,
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "ValidationException"),
            "case {case:?}: a request naming two points in time is refused, got {status} with \
             {answered}"
        );
        assert_eq!(
            message(&answered),
            BOTH_RESTORE_SELECTORS,
            "case {case:?}: expected the sentence naming both selectors, got {answered}"
        );
    }

    // The control: each selector ON ITS OWN is served, so the refusals above are about the
    // pair rather than about either member.
    for (case, extra) in [
        ("the newest restorable point alone", json!({"UseLatestRestorableTime": true})),
        ("an instant alone", json!({"RestoreDateTime": latest})),
    ] {
        let mut body = json!({
            "SourceTableName": table,
            "TargetTableName": format!("SelectorServed{}", case.len()),
        });
        for (member, value) in extra.as_object().cloned().unwrap_or_default() {
            body[member] = value;
        }
        let served = ok(&router, "RestoreTableToPointInTime", body).await;
        assert_eq!(
            served["TableDescription"]["RestoreSummary"]["RestoreInProgress"],
            json!(true),
            "case {case:?}: expected a restore in flight, got {served}"
        );
    }
}

#[tokio::test]
async fn an_export_period_a_request_cannot_be_read_from_is_refused_with_the_services_own_sentence()
{
    let router = emulator_with_s3();
    create_bucket(&router, BUCKET).await;
    let table = "PeriodRefused";
    table_with_recovery(&router, table).await;
    // A window wide enough for a period the surface accepts, which the control below takes.
    let (_, latest) = window_spanning(
        &router,
        "the export period rows",
        table,
        2 * state::MIN_INCREMENTAL_EXPORT_PERIOD,
    )
    .await;
    let period = state::MIN_INCREMENTAL_EXPORT_PERIOD.as_secs_f64();

    // Rows differ in WHICH members of the period the request states and in whether it
    // states the kind of export outright or leaves it to the documented default — and each
    // names the sentence its own fault earns.
    let rows: [(&str, Value, &str); 6] = [
        (
            "an incremental export with no period at all",
            json!({"ExportType": "INCREMENTAL_EXPORT"}),
            SPECIFICATION_EXPECTED,
        ),
        (
            "an incremental period naming only its end",
            json!({"ExportType": "INCREMENTAL_EXPORT",
                   "IncrementalExportSpecification": {"ExportToTime": latest}}),
            EXPORT_FROM_TIME_EXPECTED,
        ),
        (
            "an incremental period naming only its view",
            json!({"ExportType": "INCREMENTAL_EXPORT",
                   "IncrementalExportSpecification": {"ExportViewType": "NEW_IMAGE"}}),
            EXPORT_FROM_TIME_EXPECTED,
        ),
        (
            "a full export by default carrying a period",
            json!({"IncrementalExportSpecification": {
                       "ExportFromTime": latest - period, "ExportToTime": latest}}),
            SPECIFICATION_UNEXPECTED,
        ),
        (
            "a full export stated outright carrying a period",
            json!({"ExportType": "FULL_EXPORT",
                   "IncrementalExportSpecification": {
                       "ExportFromTime": latest - period, "ExportToTime": latest}}),
            SPECIFICATION_UNEXPECTED,
        ),
        (
            "a full export carrying a period that names no end",
            json!({"ExportType": "FULL_EXPORT",
                   "IncrementalExportSpecification": {"ExportFromTime": latest - period}}),
            SPECIFICATION_UNEXPECTED,
        ),
    ];

    for (case, extra, expected) in rows {
        let (status, answered) = call(
            &router,
            "ExportTableToPointInTime",
            export_body(table, extra),
        )
        .await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "ValidationException"),
            "case {case:?}: a period this request cannot be read from is refused, got {status} \
             with {answered}"
        );
        assert_eq!(
            message(&answered),
            expected,
            "case {case:?}: expected the sentence that names what the request got wrong, got \
             {answered}"
        );
        assert_eq!(
            recorded_exports(&router, table).await,
            0,
            "case {case:?}: a refused export records nothing"
        );
    }

    // The control: a period the surface DOES accept is served and recorded, so each refusal
    // above is about the members it names.
    let served = ok(
        &router,
        "ExportTableToPointInTime",
        export_body(
            table,
            json!({"ExportType": "INCREMENTAL_EXPORT",
                   "IncrementalExportSpecification": {
                       "ExportFromTime": latest - 2.0 * period, "ExportToTime": latest}}),
        ),
    )
    .await;
    assert_eq!(
        served["ExportDescription"]["ExportType"],
        json!("INCREMENTAL_EXPORT"),
        "case a period the surface accepts: expected the incremental export, got {served}"
    );
    assert_eq!(
        recorded_exports(&router, table).await,
        1,
        "case a period the surface accepts: the export it answered is enumerable"
    );
}
