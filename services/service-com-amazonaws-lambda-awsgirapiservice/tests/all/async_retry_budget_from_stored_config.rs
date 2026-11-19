// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHERE the length of a failing asynchronous event's ladder of attempts
//! comes from: the `MaximumRetryAttempts` the addressed qualifier's asynchronous-invocation
//! configuration HOLDS when the event is delivered, whichever operation wrote it.
//!
//! RULE: a failing asynchronous event enters the handler `1 + MaximumRetryAttempts` times —
//! "the maximum number of times to retry when the function returns an error"
//! (`FunctionEventInvokeConfig$MaximumRetryAttempts`, `@range(min: 0, max: 2)`), so the member
//! counts the attempts AFTER the first — with the budget read from the configuration as it
//! stands, whether `PutFunctionEventInvokeConfig` wrote it (that request replaces the
//! configuration whole) or `UpdateFunctionEventInvokeConfig` did ("to set one option without
//! affecting existing settings for other options, use UpdateFunctionEventInvokeConfig", so a
//! setting its request does not carry is left exactly as it stands). The last of those
//! attempts then earns exactly one invocation record at the destination
//! `DestinationConfig$OnFailure` names, reporting `RetriesExhausted` as
//! `requestContext.condition` and those attempts as `requestContext.approximateInvokeCount`.
//!
//! [`crate::async_retry_budget_attempts`] pins that ladder for a budget a PUT states; the rows
//! below differ along the operation the budget in force was stated by and along the direction
//! the update moved it: a budget raised to the published maximum, one lowered to the published
//! minimum, one the update alone ever stated, and one an update of ANOTHER setting left
//! standing. Each row is read back through `GetFunctionEventInvokeConfig` before its event is
//! queued, so the ladder is measured against the configuration a client can see rather than
//! against the request that wrote it.
//!
//! The wiring every row needs — the queues, the function whose package RUNS a handler that
//! counts its own entries and raises, the put of the destination, the accepting call — and the
//! one reader of what a ladder leaves behind are
//! [`crate::async_retry_budget_attempts`]'s, so there is a single harness for the two suites
//! that drive a ladder of attempts. Nothing here waits a span of its own: every wait looks
//! again until what it waits for is there or a generous deadline passes.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::async_retry_budget_attempts::{
    assert_each_ladder, given_event_invoke_config, given_function, given_queue,
    invoked_asynchronously, lambda, router_on, Ladder, EVENT_INVOKE_CONFIG_URI,
};

/// One row: how the budget in force was stated, and how many times the handler therefore
/// ENTERS for one event.
struct Row {
    case: &'static str,
    /// The function the row drives, which carries its own two queues.
    function: &'static str,
    /// The `MaximumRetryAttempts` the row's PUT states, or `None` for a put that states none.
    put_retries: Option<i64>,
    /// The settings the row's UPDATE carries, as its request body.
    update: Value,
    /// What the configuration must then REPORT: the members a client reads the budget in
    /// force through, and the value each holds.
    reported: Vec<(&'static str, Value)>,
    /// The entries the handler makes for the event: the first invocation plus the retries the
    /// configuration above holds, which is what the record reports as
    /// `approximateInvokeCount`.
    attempts: usize,
}

/// The configuration URI of a function's unpublished version, which every call below
/// addresses.
fn config_uri(function: &str) -> String {
    format!("{EVENT_INVOKE_CONFIG_URI}/{function}/event-invoke-config")
}

/// `UpdateFunctionEventInvokeConfig` on that qualifier, carrying exactly the settings the
/// row's update states.
async fn update_config(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    function: &str,
    update: &Value,
) {
    let (status, answered) = lambda(router, "POST", &config_uri(function), update, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: UpdateFunctionEventInvokeConfig answered {status} {answered}"
    );
}

/// The configuration the qualifier HOLDS, as `GetFunctionEventInvokeConfig` reports it.
async fn read_config(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    function: &str,
) -> Value {
    let (status, answered) = lambda(router, "GET", &config_uri(function), &Value::Null, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetFunctionEventInvokeConfig answered {status} {answered}"
    );
    serde_json::from_str(&answered).unwrap_or_else(|_| {
        panic!("case {case:?}: GetFunctionEventInvokeConfig answered {answered}, not a document")
    })
}

/// The rows: a budget an update raised, one it lowered, one it alone ever stated, and one an
/// update of another setting left standing.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an update raises a budget of none to the published maximum",
            function: "stored-budget-raised",
            put_retries: Some(0),
            update: json!({ "MaximumRetryAttempts": 2 }),
            reported: vec![("MaximumRetryAttempts", json!(2))],
            attempts: 3,
        },
        Row {
            case: "an update lowers a budget of the published maximum to none",
            function: "stored-budget-lowered",
            put_retries: Some(2),
            update: json!({ "MaximumRetryAttempts": 0 }),
            reported: vec![("MaximumRetryAttempts", json!(0))],
            attempts: 1,
        },
        Row {
            case: "an update states the only budget the configuration has ever held",
            function: "stored-budget-only-updated",
            put_retries: None,
            update: json!({ "MaximumRetryAttempts": 1 }),
            reported: vec![("MaximumRetryAttempts", json!(1))],
            attempts: 2,
        },
        Row {
            case: "an update of another setting leaves the budget the put stated standing",
            function: "stored-budget-untouched",
            put_retries: Some(2),
            update: json!({ "MaximumEventAgeInSeconds": 600 }),
            reported: vec![
                ("MaximumRetryAttempts", json!(2)),
                ("MaximumEventAgeInSeconds", json!(600)),
            ],
            attempts: 3,
        },
    ]
}

/// RULE: a failing asynchronous event enters the handler `1 + MaximumRetryAttempts` times with
/// the budget read from the configuration the qualifier HOLDS — whichever of the two
/// configuration operations wrote it — and the last of those attempts earns exactly one
/// `RetriesExhausted` record reporting them.
#[tokio::test]
async fn a_failing_event_spends_the_budget_its_stored_configuration_holds() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let rows = rows();

    // Every row is wired and its event queued first, so the ladders of attempts run alongside
    // one another rather than one suite-length wait per row.
    let mut wired: Vec<Ladder> = Vec::new();
    for row in &rows {
        let (attempts_url, _) =
            given_queue(&router, row.case, &format!("{}-attempts", row.function)).await;
        let (failure_url, failure_arn) =
            given_queue(&router, row.case, &format!("{}-failure", row.function)).await;
        given_function(&router, &engine, row.case, row.function).await;
        given_event_invoke_config(
            &router,
            row.case,
            row.function,
            &failure_arn,
            row.put_retries,
        )
        .await;
        update_config(&router, row.case, row.function, &row.update).await;
        // The budget the ladder is measured against is the one a client can READ back, so the
        // row is pinned to the configuration rather than to the request that wrote it.
        let held = read_config(&router, row.case, row.function).await;
        for (member, expected) in &row.reported {
            let read = held.get(member).cloned().unwrap_or(Value::Null);
            assert_eq!(
                read, *expected,
                "case {:?}: expected the held configuration to report {member} as {expected}, \
                 got {read} in {held}",
                row.case
            );
        }
        let marker = format!("{}-marker", row.function);
        invoked_asynchronously(
            &router,
            row.case,
            row.function,
            &json!({ "marker": marker, "attempts": attempts_url }),
        )
        .await;
        wired.push(Ladder {
            case: row.case,
            attempts_url,
            failure_url,
            marker,
            attempts: row.attempts,
        });
    }

    assert_each_ladder(&router, &wired).await;
}
