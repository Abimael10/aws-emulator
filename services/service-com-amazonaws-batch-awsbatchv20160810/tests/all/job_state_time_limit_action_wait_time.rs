// SPDX-License-Identifier: CC0-1.0

//! The wait time a `jobStateTimeLimitActions` entry may carry, end to end over the wire.
//!
//! The rule under test: **a `JobStateTimeLimitAction$maxTimeSeconds` outside the closed
//! range the member documents is refused with the modelled `ClientException` (HTTP 400) and
//! ONE fixed sentence that states the published range and nothing about the offending
//! value, by every operation that accepts the list; a value inside the range is accepted
//! and stored.**
//!
//! Each suite asserts that over a SET of inputs: how far outside the range the value is and
//! on which side, which position in the list carries it, how many entries the list has,
//! which `action` the entry names, and which operation (`CreateJobQueue` or
//! `UpdateJobQueue`) carries it.

use serde_json::{json, Value};

use service_com_amazonaws_batch_awsbatchv20160810::state;

use crate::job_lifecycle::{compute_environment, error_code, error_message, job_queue, Batch};

/// `maxTimeSeconds`' published bounds, from the member's own doc: "The minimum value is 600
/// (10 minutes) and the maximum value is 86,400 (24 hours)."
fn bounds() -> (i64, i64) {
    (
        i64::from(state::MIN_JOB_STATE_TIME_LIMIT_SECONDS),
        i64::from(state::MAX_JOB_STATE_TIME_LIMIT_SECONDS),
    )
}

/// One `jobStateTimeLimitActions` entry carrying `seconds`. `state` has one modelled value
/// (`RUNNABLE`); `action` is one of the two the model declares (`CANCEL`, `TERMINATE`).
fn action_entry(seconds: i64, action: &str) -> Value {
    json!({
        "reason": "MISCONFIGURATION:COMPUTE_ENVIRONMENT_MAX_RESOURCE",
        "state": "RUNNABLE",
        "maxTimeSeconds": seconds,
        "action": action,
    })
}

/// The one `JobQueueDetail` for `reference` (a queue name or ARN).
async fn describe_queue(batch: &Batch, reference: &str) -> Value {
    let body = batch
        .ok("/v1/describejobqueues", json!({"jobQueues": [reference]}))
        .await;
    let queues = body["jobQueues"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        queues.len(),
        1,
        "case describe {reference}: expected exactly one JobQueueDetail, got {body}"
    );
    queues[0].clone()
}

/// The `maxTimeSeconds` values a queue's stored `jobStateTimeLimitActions` report, in
/// order.
fn stored_wait_times(queue: &Value) -> Vec<i64> {
    queue["jobStateTimeLimitActions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry["maxTimeSeconds"].as_i64())
        .collect()
}

/// One row of the refusal table: the list a request carries, and what it varies.
struct OutOfRange {
    /// What this row varies, named in every assertion.
    name: &'static str,
    /// The `jobStateTimeLimitActions` list the request carries.
    actions: Vec<Value>,
}

/// Every list that names a wait time outside the published range, on EITHER side and at
/// any position: refused 400 `ClientException` with the one fixed sentence, which states
/// the published bounds and never the value the request gave. Nothing is written: the queue
/// `UpdateJobQueue` refused still reports the entry it was created with.
#[tokio::test]
async fn an_out_of_range_wait_time_is_refused_with_one_fixed_sentence() {
    let batch = Batch::new();
    compute_environment(&batch, "ce-wait").await;
    let (minimum, maximum) = bounds();

    let rows = [
        OutOfRange {
            name: "one below the published minimum",
            actions: vec![action_entry(minimum - 1, "CANCEL")],
        },
        OutOfRange {
            name: "one above the published maximum",
            actions: vec![action_entry(maximum + 1, "CANCEL")],
        },
        OutOfRange {
            name: "far below the minimum: zero",
            actions: vec![action_entry(0, "TERMINATE")],
        },
        OutOfRange {
            name: "negative",
            actions: vec![action_entry(-1, "CANCEL")],
        },
        OutOfRange {
            name: "far above the maximum",
            actions: vec![action_entry(maximum * 10, "TERMINATE")],
        },
        OutOfRange {
            name: "the largest value the Integer member can carry",
            actions: vec![action_entry(i64::from(i32::MAX), "CANCEL")],
        },
        OutOfRange {
            name: "the offending entry second, behind a valid one",
            actions: vec![
                action_entry(minimum, "CANCEL"),
                action_entry(minimum - 1, "TERMINATE"),
            ],
        },
        OutOfRange {
            name: "the offending entry last of three",
            actions: vec![
                action_entry(minimum, "CANCEL"),
                action_entry(maximum, "CANCEL"),
                action_entry(maximum + 1, "CANCEL"),
            ],
        },
    ];


    // The one sentence both sides of the range answer, built from the MODEL's bounds.
    let expected = state::job_action_wait_time_detail();
    assert_eq!(
        expected,
        format!("Invalid job action wait time. Value must be in range: [{minimum}..{maximum}]"),
        "case the sentence: expected the published range in [min..max] form, got {expected:?}"
    );

    // A queue that already carries an accepted entry, so a refused update can be shown to
    // have written nothing.
    let settled = json!([action_entry(minimum, "CANCEL")]);
    batch
        .ok(
            "/v1/createjobqueue",
            json!({
                "jobQueueName": "q-wait-settled",
                "priority": 1,
                "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": "ce-wait"}],
                "jobStateTimeLimitActions": settled,
            }),
        )
        .await;

    for (index, row) in rows.iter().enumerate() {
        // The same list is refused by each operation that accepts it.
        let requests: Vec<(&str, &str, Value)> = vec![
            (
                "CreateJobQueue",
                "/v1/createjobqueue",
                json!({
                    "jobQueueName": format!("q-wait-{index}"),
                    "priority": 1,
                    "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": "ce-wait"}],
                    "jobStateTimeLimitActions": row.actions,
                }),
            ),
            (
                "UpdateJobQueue",
                "/v1/updatejobqueue",
                json!({
                    "jobQueue": "q-wait-settled",
                    "jobStateTimeLimitActions": row.actions,
                }),
            ),
        ];

        for (operation, uri, body) in requests {
            let case = format!("{operation}, {}", row.name);
            let (status, answered) = batch.call(uri, body).await;
            assert_eq!(
                status, 400,
                "case {case}: expected HTTP 400, got {status} {answered}"
            );
            assert_eq!(
                error_code(&answered),
                "ClientException",
                "case {case}: expected the modelled ClientException, got {answered}"
            );
            let message = error_message(&answered);
            // One fixed sentence, identical for every row: it varies with neither the side
            // of the range the value fell outside nor the value itself, which is what "the
            // offending value is not echoed" means here — the rows vary the value while
            // this expectation stays constant.
            assert_eq!(
                message, expected,
                "case {case}: expected {expected:?}, got {message:?}"
            );
        }

        // The refused create left no queue behind, and the refused update left the queue
        // it named exactly as it was.
        let (status, missing) = batch
            .call(
                "/v1/describejobqueues",
                json!({"jobQueues": [format!("q-wait-{index}")]}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {}: expected the describe to answer, got {status} {missing}",
            row.name
        );
        assert_eq!(
            missing["jobQueues"],
            json!([]),
            "case {}: a refused create must leave no queue behind, got {missing}",
            row.name
        );
        let unchanged = describe_queue(&batch, "q-wait-settled").await;
        assert_eq!(
            stored_wait_times(&unchanged),
            vec![minimum],
            "case {}: a refused update must write nothing, got {unchanged}",
            row.name
        );
    }
}

/// The other side of the same rule: every wait time INSIDE the published range is accepted
/// and stored, on each published bound and in between, for a list of one entry and of
/// several — read back through `DescribeJobQueues`, which is the only place the stored
/// record shows.
#[tokio::test]
async fn a_wait_time_inside_the_published_bounds_is_stored() {
    let batch = Batch::new();
    compute_environment(&batch, "ce-inside").await;
    let (minimum, maximum) = bounds();
    let middle = (minimum + maximum) / 2;

    let rows: Vec<(&str, Vec<Value>, Vec<i64>)> = vec![
        (
            "the published minimum",
            vec![action_entry(minimum, "CANCEL")],
            vec![minimum],
        ),
        (
            "the published maximum",
            vec![action_entry(maximum, "TERMINATE")],
            vec![maximum],
        ),
        (
            "between the bounds",
            vec![action_entry(middle, "CANCEL")],
            vec![middle],
        ),
        (
            "both bounds in one list, in that order",
            vec![
                action_entry(maximum, "CANCEL"),
                action_entry(minimum, "TERMINATE"),
            ],
            vec![maximum, minimum],
        ),
    ];

    for (index, (case, actions, expected)) in rows.iter().enumerate() {
        let name = format!("q-inside-{index}");
        batch
            .ok(
                "/v1/createjobqueue",
                json!({
                    "jobQueueName": &name,
                    "priority": 1,
                    "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": "ce-inside"}],
                    "jobStateTimeLimitActions": actions,
                }),
            )
            .await;
        let created = describe_queue(&batch, &name).await;
        assert_eq!(
            &stored_wait_times(&created),
            expected,
            "case create {case}: expected the wait times {expected:?}, got {created}"
        );

        // And the same list through the update path, onto a queue created without one.
        let updated_name = format!("q-inside-updated-{index}");
        job_queue(&batch, &updated_name, "ce-inside").await;
        batch
            .ok(
                "/v1/updatejobqueue",
                json!({"jobQueue": &updated_name, "jobStateTimeLimitActions": actions}),
            )
            .await;
        let updated = describe_queue(&batch, &updated_name).await;
        assert_eq!(
            &stored_wait_times(&updated),
            expected,
            "case update {case}: expected the wait times {expected:?}, got {updated}"
        );
    }
}
