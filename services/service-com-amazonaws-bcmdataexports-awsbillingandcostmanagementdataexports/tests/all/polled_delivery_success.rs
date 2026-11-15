// SPDX-License-Identifier: CC0-1.0

//! What a POLLING client observes for an export this service accepted, over the awsJson1_1
//! wire with Amazon S3 beside it on one engine: `CreateExport` -> `ListExecutions` /
//! `GetExecution` -> `GetExport`, with nothing but the service's own clock moving the
//! delivery along.
//!
//! The rule under test: a delivery of a definition this service ACCEPTED — its statement
//! reads the published catalog and its destination takes the write — SUCCEEDS, so the
//! execution recording it climbs the modelled `ExecutionStatusCode` values in order and ends
//! in the modelled terminal success `DELIVERY_SUCCESS`, with `CompletedAt` set and no
//! `StatusReason` (never in a terminal failure), the export then reports
//! `ExportStatus.LastRefreshedAt` ("The timestamp of when the export was last generated") at
//! that instant and stays `HEALTHY`, and the success is backed by the file the delivery left
//! in the destination bucket.
//!
//! Why a client cares: a client polls `GetExecution` (or `ListExecutions`) to learn whether
//! its data has been delivered, and reads `LastRefreshedAt` to decide whether to read it. An
//! execution that can only ever end in a failure sends every such client down the failure
//! branch for good, with no error to act on and nothing to retry.
//!
//! It is asserted over the TABLE of accepted definitions `export_file_delivery::rows`
//! publishes — every output encoding this service writes, both modelled `Overwrite`
//! dispositions, five shapes of select list over two catalog tables, and four forms of
//! `S3Prefix` — so the success is asserted over the class of definitions the service accepts
//! rather than over one of them.
//!
//! What this suite adds to `tests/all/export_file_delivery.rs`, which asserts what one
//! delivery WRITES: nothing here moves a stored due instant. The delivery is waited for
//! exactly as a client waits for it — by looking again until the execution reports a terminal
//! status — so the ladder a client polls has to reach the success state on the service's own
//! clock. The wait is bounded by patience, not by a deadline the assertions depend on.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_bcmdataexports_awsbillingandcostmanagementdataexports::ops::types::EXECUTION_STATUS_CODE_VALUES;
use service_com_amazonaws_bcmdataexports_awsbillingandcostmanagementdataexports::state::{
    DELIVERY_PIPELINE, EXECUTION_STATUS_DELIVERY_SUCCESS, EXPORT_STATUS_HEALTHY,
};

use crate::delivery_grant::{published_grant, Deliveries};
use crate::export_file_delivery::{create_request, listed_keys, rows, Row};
use crate::export_lifecycle::timestamp;

/// How long one row waits for the delivery the service's own clock owes it.
///
/// PATIENCE, not a weakened expectation: the assertions below are unchanged by how long the
/// wait took, and a whole-workspace test run shares its cores with every other suite, so the
/// task the clock owes can be scheduled far later than it is owed.
const POLL_BUDGET: Duration = Duration::from_secs(60);

/// How often the execution is looked at again while waiting. Short enough that the
/// non-terminal steps of a delivery are observed on an unloaded machine, though nothing here
/// asserts that any particular one of them was seen — which status is in force at an instant
/// is asserted from the stored instants in `tests/all/export_file_delivery.rs`.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The modelled `ExecutionStatusCode` values a SUCCESSFUL delivery passes through, in the
/// order it passes through them: the non-terminal steps the service works through
/// ([`DELIVERY_PIPELINE`]) and then the terminal success.
fn ladder() -> Vec<&'static str> {
    DELIVERY_PIPELINE
        .iter()
        .copied()
        .chain([EXECUTION_STATUS_DELIVERY_SUCCESS])
        .collect()
}

/// The execution `export_arn` has accrued as `(id, ExecutionStatus)`, read the way a client
/// reads it — `ListExecutions` for the id, `GetExecution` for that id — or `None` while the
/// service owes a delivery whose execution does not exist yet.
async fn polled_execution(
    case: &str,
    emulator: &Deliveries,
    export_arn: &str,
) -> Option<(String, Value)> {
    let listed = emulator
        .ok("ListExecutions", &json!({ "ExportArn": export_arn }))
        .await;
    let references = match listed.get("Executions") {
        Some(Value::Array(references)) => references.clone(),
        None => Vec::new(),
        other => panic!("case {case:?}: Executions must be a list, got {other:?}"),
    };
    assert!(
        references.len() <= 1,
        "case {case:?}: one persisted definition is owed one delivery, got {references:?}"
    );
    let reference = references.into_iter().next()?;
    let id = reference["ExecutionId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a reference carries its ExecutionId, got {reference}")
        })
        .to_string();
    let described = emulator
        .ok(
            "GetExecution",
            &json!({ "ExportArn": export_arn, "ExecutionId": &id }),
        )
        .await;
    assert_eq!(
        described["ExecutionId"].as_str(),
        Some(id.as_str()),
        "case {case:?}: GetExecution reports the execution it was asked for, got {described}"
    );
    Some((id, described["ExecutionStatus"].clone()))
}

/// The `StatusCode` of a reported `ExecutionStatus`, required to be one of the values the
/// model publishes.
fn status_code(case: &str, status: &Value) -> String {
    let code = status["StatusCode"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: an execution reports a StatusCode, got {status}"));
    assert!(
        EXECUTION_STATUS_CODE_VALUES.contains(&code),
        "case {case:?}: expected a published ExecutionStatusCode, got {code:?} (published: \
         {EXECUTION_STATUS_CODE_VALUES:?})"
    );
    code.to_string()
}

/// Look at the execution of `export_arn` until it reports a TERMINAL status, answering its
/// id, the status it ended in, and every status code observed on the way, in order.
///
/// Nothing is slept for outside this loop and no assertion depends on how long it took: the
/// loop looks again until the condition holds or its patience runs out.
async fn await_terminal(
    case: &str,
    emulator: &Deliveries,
    export_arn: &str,
) -> (String, Value, Vec<String>) {
    let pipeline = DELIVERY_PIPELINE;
    let deadline = Instant::now() + POLL_BUDGET;
    let mut observed: Vec<String> = Vec::new();
    let mut last: Option<(String, Value)> = None;
    loop {
        if let Some((id, status)) = polled_execution(case, emulator, export_arn).await {
            let code = status_code(case, &status);
            if observed.last() != Some(&code) {
                observed.push(code.clone());
            }
            if !pipeline.contains(&code.as_str()) {
                return (id, status, observed);
            }
            last = Some((id, status));
        }
        if Instant::now() >= deadline {
            panic!(
                "case {case:?}: expected the delivery the service owes to reach a terminal \
                 status, got {last:?} after observing {observed:?}"
            );
        }
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Assert that `observed` is a prefix-respecting walk of the ladder a successful delivery
/// climbs: every code it reports is on that ladder, and no code comes after one that is
/// later on it.
fn assert_climbs_the_ladder(case: &str, observed: &[String]) {
    let ladder = ladder();
    let mut reached = 0usize;
    for code in observed {
        let step = ladder
            .iter()
            .position(|rung| *rung == code.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: a successful delivery only ever reports a status on \
                     {ladder:?}, got {code:?} in {observed:?}"
                )
            });
        assert!(
            step >= reached,
            "case {case:?}: expected the statuses to progress along {ladder:?}, got {code:?} \
             after {:?} in {observed:?}",
            ladder[reached]
        );
        reached = step;
    }
    assert_eq!(
        ladder.get(reached).copied(),
        Some(EXECUTION_STATUS_DELIVERY_SUCCESS),
        "case {case:?}: expected the walk to end on the terminal success, got {observed:?}"
    );
}

/// One reported instant as the WHOLE instant it is, so two of them are compared to the
/// precision the service stores them at rather than to the second alone.
fn instant(case: &str, status: &Value, member: &str) -> (i64, u32) {
    let reported = timestamp(case, status, member);
    (reported.secs(), reported.subsec_nanos())
}

/// Assert the export's own status after a delivery that succeeded: it was generated at the
/// instant the delivery completed, and the export is healthy.
async fn assert_generated_at(
    case: &str,
    emulator: &Deliveries,
    arn: &str,
    completed_at: (i64, u32),
) {
    let status = emulator.ok("GetExport", &json!({ "ExportArn": arn })).await["ExportStatus"]
        .clone();
    assert_eq!(
        instant(case, &status, "LastRefreshedAt"),
        completed_at,
        "case {case:?}: expected LastRefreshedAt at the instant the export was generated, got \
         {status}"
    );
    assert_eq!(
        status["StatusCode"].as_str(),
        Some(EXPORT_STATUS_HEALTHY),
        "case {case:?}: an export whose delivery succeeded is healthy, got {status}"
    );
}

/// Assert the delivered file the success is backed by: the destination bucket holds exactly
/// the one object this delivery wrote, named after the export, read through Amazon S3's own
/// listing.
async fn assert_backed_by_a_file(case: &str, emulator: &Deliveries, row: &Row, name: &str) {
    let keys = listed_keys(emulator, &bucket_of(name), row.prefix).await;
    assert_eq!(
        keys.len(),
        1,
        "case {case:?}: a delivery that reported success wrote its one file, got {keys:?}"
    );
    assert!(
        keys[0].contains(name),
        "case {case:?}: the delivered file is the one this export's delivery wrote, got {keys:?}"
    );
}

/// The destination bucket of the export called `name`: one bucket per row, named after it.
fn bucket_of(name: &str) -> String {
    name.replace('_', "-")
}

/// RULE: the delivery of a definition this service accepted succeeds on the service's own
/// clock — the execution a client polls ends in `DELIVERY_SUCCESS` with `CompletedAt` and no
/// `StatusReason`, and the export reports it was generated at that instant and stays healthy.
#[tokio::test]
async fn a_polled_execution_of_an_accepted_export_ends_in_delivery_success() {
    for (index, row) in rows().into_iter().enumerate() {
        let case = format!("{} (row {index})", row.label);
        let emulator = Deliveries::new();
        let name = format!("polled_delivery_{index}");
        let bucket = bucket_of(&name);
        emulator
            .bucket_with(&bucket, Some(&published_grant(&bucket)))
            .await;

        let created = emulator
            .ok("CreateExport", &create_request(&row, &name, &bucket))
            .await;
        let arn = created["ExportArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateExport returns an ExportArn"))
            .to_string();

        // The client does nothing but look again: the delivery is the service's own work.
        let (execution_id, status, observed) = await_terminal(&case, &emulator, &arn).await;
        assert_eq!(
            status["StatusCode"].as_str(),
            Some(EXECUTION_STATUS_DELIVERY_SUCCESS),
            "case {case:?}: an accepted definition delivers successfully, got {status} after \
             observing {observed:?}"
        );
        assert_eq!(
            status.get("StatusReason"),
            None,
            "case {case:?}: a delivery that succeeded has no failure to report, got {status}"
        );
        assert_climbs_the_ladder(&case, &observed);

        // It reports WHEN it finished, at or after the instant it was created at.
        let completed_at = instant(&case, &status, "CompletedAt");
        let created_at = instant(&case, &status, "CreatedAt");
        assert!(
            completed_at >= created_at,
            "case {case:?}: expected CompletedAt at or after CreatedAt, got {status}"
        );
        assert_eq!(
            instant(&case, &status, "LastUpdatedAt"),
            completed_at,
            "case {case:?}: the last update of a finished delivery is its completion, got \
             {status}"
        );

        // The terminal status is what BOTH readers report, and it does not move again.
        let (listed_id, listed_status) = polled_execution(&case, &emulator, &arn)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the execution stays reported"));
        assert_eq!(
            (listed_id.as_str(), listed_status["StatusCode"].as_str()),
            (execution_id.as_str(), Some(EXECUTION_STATUS_DELIVERY_SUCCESS)),
            "case {case:?}: a terminal success is what a later poll reports too, got \
             {listed_status}"
        );

        assert_generated_at(&case, &emulator, &arn, completed_at).await;
        assert_backed_by_a_file(&case, &emulator, &row, &name).await;
    }
}
