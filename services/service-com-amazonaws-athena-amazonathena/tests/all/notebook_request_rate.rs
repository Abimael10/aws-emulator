// SPDX-License-Identifier: CC0-1.0

//! The RATE Athena admits a metered API's calls at, and what it answers the calls past it
//! with.
//!
//! THE RULE UNDER TEST: Athena serves a metered API's calls, per account and Region, from a
//! token bucket whose capacity is the published rate times the published burst multiplier —
//! so a sustained burst is served up to that capacity and every call past it is REFUSED with
//! the modelled `TooManyRequestsException` (HTTP 400) stating AWS's throttling sentence,
//! performing nothing at all. The budget belongs to the ONE operation it is published for:
//! an operation no rate is metered for is served while the metered one is throttled.
//!
//! The rules are asserted over SETS rather than one call: the first test walks a whole burst
//! and classifies every outcome in it (inside the capacity, past it), and the second walks a
//! table of the service's other notebook operations — two reads, a listing and a mutation
//! that writes a notebook — each driven while the metered operation is being refused.
//!
//! Nothing here waits out a span: a burst looks for the CONDITION — the refusal — call by
//! call, because a budget the rate refills may serve a call at any point and that call
//! charges the capacity again.
//!
//! Driven end to end over the same router the emulator boots (awsJson1_1,
//! `x-amz-target: AmazonAthena.<Operation>`).

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::request_quota::{
    metered_limits, THROTTLE_MESSAGE,
};
use service_com_amazonaws_athena_amazonathena::state::{MAX_NOTEBOOKS, NOTEBOOK_TYPE_IPYNB};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, error_code, message, router};

/// The Spark enabled workgroup the notebooks are created in.
const WORK_GROUP: &str = "spark-rate";

/// The PySpark engine version a Spark enabled workgroup selects.
const PYSPARK_ENGINE_VERSION: &str = "PySpark engine version 3";

/// The operation whose call rate this suite drives.
const METERED_OPERATION: &str = "CreateNotebook";

/// The error the model declares for a call past a rate, which is what an SDK's throttling
/// classification keys off.
const THROTTLE_CODE: &str = "TooManyRequestsException";

/// How many calls a burst makes before giving up on seeing the refusal.
///
/// Far above the capacity the published pair admits at once, so the burst reaches the
/// refusal; and far below the notebooks this emulator holds ([`MAX_NOTEBOOKS`]), so no call
/// in it can be refused for capacity instead.
const BURST_CALLS: usize = 60;

/// How many calls a PROBE makes before giving up: a budget already spent refuses the next
/// call, unless the rate refilled one in between — which that call then charges again.
const PROBE_CALLS: usize = 8;

/// What one call of a burst answered.
struct Outcome {
    /// The notebook name it carried, which is what the registry holds if it was served.
    name: String,
    /// The wire error code it was refused with, or `None` when it was served.
    code: Option<String>,
    /// Its HTTP status.
    status: u16,
    /// Its whole answer, so the refusal's own sentence is read off the call that got it.
    body: Value,
}

/// Create the Spark enabled workgroup the notebooks live in.
async fn given_spark_work_group(router: &MultiServiceRouter) {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": WORK_GROUP,
            "Configuration": {
                "EngineVersion": { "SelectedEngineVersion": PYSPARK_ENGINE_VERSION },
                "ExecutionRole": "arn:aws:iam::000000000000:role/AthenaSparkExecution"
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the Spark workgroup {WORK_GROUP} must be created, got {status} with {created:?}"
    );
}

/// One create of the notebook `name`, as a client sends it.
async fn create(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    call(
        router,
        METERED_OPERATION,
        &json!({ "WorkGroup": WORK_GROUP, "Name": name }),
    )
    .await
}

/// Call the metered operation, names prefixed with `label`, until the rate refuses one —
/// answering with every outcome in the order the calls were made, the refusal last.
///
/// Panics when `cap` calls bring no refusal: the capacity is finite, so a burst that never
/// reaches it is a budget that is not being spent.
async fn burst(router: &MultiServiceRouter, label: &str, cap: usize) -> Vec<Outcome> {
    let mut outcomes: Vec<Outcome> = Vec::new();
    for index in 0..cap {
        let name = format!("{label}-{index}");
        let (status, body) = create(router, &name).await;
        let code = (status != 200).then(|| error_code(&body));
        let throttled = code.as_deref() == Some(THROTTLE_CODE);
        outcomes.push(Outcome {
            name,
            code,
            status,
            body,
        });
        if throttled {
            return outcomes;
        }
    }
    panic!(
        "case {label:?}: {METERED_OPERATION} must be refused within {cap} calls, got \
         {:?}",
        outcomes
            .iter()
            .map(|outcome| (outcome.status, outcome.code.clone()))
            .collect::<Vec<_>>()
    );
}

/// The names of the notebooks `WORK_GROUP` holds, as its own listing reports them, sorted.
async fn held_names(router: &MultiServiceRouter) -> Vec<String> {
    let (status, listed) = call(
        router,
        "ListNotebookMetadata",
        &json!({ "WorkGroup": WORK_GROUP, "MaxResults": 50 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the workgroup's notebooks must be listable, got {status} with {listed:?}"
    );
    let mut names: Vec<String> = listed["NotebookMetadataList"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|entry| entry["Name"].as_str().map(str::to_string))
        .collect();
    names.sort();
    names
}

/// A sustained burst of calls to the metered operation is served up to the capacity the
/// published pair admits and then refused with the modelled throttling error — and the
/// refused calls performed nothing.
#[tokio::test]
async fn a_burst_past_the_metered_rate_is_refused_with_the_throttling_error() {
    let router = router();
    given_spark_work_group(&router).await;

    let limits = metered_limits(METERED_OPERATION)
        .unwrap_or_else(|| panic!("{METERED_OPERATION} must be metered"));
    assert!(
        limits.burst > 0.0 && limits.burst < BURST_CALLS as f64,
        "case \"the metered pair\": the capacity {} must be inside the burst this suite \
         drives, and the emulator's notebook bound is {MAX_NOTEBOOKS}",
        limits.burst
    );

    let outcomes = burst(&router, "burst", BURST_CALLS).await;
    let served: Vec<String> = outcomes
        .iter()
        .filter(|outcome| outcome.code.is_none() && outcome.status == 200)
        .map(|outcome| outcome.name.clone())
        .collect();
    let refused: Vec<&Outcome> = outcomes
        .iter()
        .filter(|outcome| outcome.code.is_some())
        .collect();

    // Every call inside the capacity is served: the bucket starts full, so a burst of that
    // many cannot be refused.
    assert!(
        served.len() as f64 >= limits.burst,
        "case \"the calls inside the capacity\": expected at least {} served, got {}",
        limits.burst,
        served.len()
    );
    // Every refusal in the burst is the THROTTLE and nothing else: no other code appears.
    for outcome in &refused {
        assert_eq!(
            (outcome.status, outcome.code.as_deref()),
            (400, Some(THROTTLE_CODE)),
            "case {:?}: expected the throttling refusal, got {} with {:?}",
            outcome.name,
            outcome.status,
            outcome.body
        );
    }

    // The refusal states AWS's throttling sentence and mints no notebook.
    let throttled = refused
        .last()
        .unwrap_or_else(|| panic!("the burst ends on the refusal, got {served:?}"));
    assert_eq!(
        message(&throttled.body),
        THROTTLE_MESSAGE,
        "case \"the throttling sentence\": expected {THROTTLE_MESSAGE:?}, got {:?}",
        throttled.body
    );
    assert_eq!(
        throttled.body["NotebookId"].as_str(),
        None,
        "case \"the throttled call\": a refused call mints no notebook, got {:?}",
        throttled.body
    );

    // What the account holds is exactly what was SERVED: every throttled call performed
    // nothing, so neither the name it carried nor anything else entered the registry.
    let mut expected = served.clone();
    expected.sort();
    let held = held_names(&router).await;
    assert_eq!(
        held, expected,
        "case \"what the burst left behind\": expected only the served notebooks"
    );
}

/// The budget belongs to the operation it is published for: each of the service's other
/// notebook operations — a read of a notebook's content, a read of its metadata, a listing
/// and an import that writes one — is served while the metered operation is being refused.
#[tokio::test]
async fn an_operation_no_rate_is_metered_for_is_served_while_the_metered_one_is_throttled() {
    let router = router();
    given_spark_work_group(&router).await;

    // One notebook, created before the budget is spent, for the read and the delete below to
    // address.
    let (status, created) = create(&router, "read-me").await;
    assert_eq!(
        status, 200,
        "the first create must be served, got {status} with {created:?}"
    );
    let notebook_id = created["NotebookId"]
        .as_str()
        .unwrap_or_else(|| panic!("a create answers with an id, got {created:?}"))
        .to_string();

    // Spend the budget, so the metered operation is being refused.
    burst(&router, "spend", BURST_CALLS).await;

    // Rows differ along the operation: a read of one notebook's content, a read of its
    // metadata, a listing of the workgroup's notebooks and a mutation that writes one. No
    // rate is metered for any of them, so each is served — and each row re-establishes that
    // the metered one is refused at that moment rather than assuming it from the burst.
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "a read of one notebook's content",
            "ExportNotebook",
            json!({ "NotebookId": notebook_id }),
        ),
        (
            "a read of one notebook's metadata",
            "GetNotebookMetadata",
            json!({ "NotebookId": notebook_id }),
        ),
        (
            "a listing of the workgroup's notebooks",
            "ListNotebookMetadata",
            json!({ "WorkGroup": WORK_GROUP, "MaxResults": 50 }),
        ),
        (
            "a mutation that writes a notebook",
            "ImportNotebook",
            json!({
                "WorkGroup": WORK_GROUP,
                "Name": "imported",
                "Type": NOTEBOOK_TYPE_IPYNB,
                "Payload": "{}",
            }),
        ),
    ];

    for (case, operation, request) in rows {
        let probe = burst(&router, case, PROBE_CALLS).await;
        let refusal = probe
            .last()
            .unwrap_or_else(|| panic!("case {case:?}: a probe ends on the refusal"));
        assert_eq!(
            (refusal.status, refusal.code.as_deref()),
            (400, Some(THROTTLE_CODE)),
            "case {case:?}: the metered operation must be refused here, got {} with {:?}",
            refusal.status,
            refusal.body
        );
        let (status, answer) = call(&router, operation, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} is metered by no rate, so it must be served while \
             {METERED_OPERATION} is throttled, got {status} with {answer:?}"
        );
    }
}
