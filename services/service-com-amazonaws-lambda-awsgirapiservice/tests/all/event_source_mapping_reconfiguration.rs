// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for RECONFIGURING an event source mapping that is already standing:
//! `UpdateEventSourceMapping` "updates an event source mapping. You can change the function
//! that Lambda invokes, or pause invocation and resume later from the same location."
//!
//! RULE: a mapping is unavailable to an update only while it is still being STOOD UP — the
//! `Creating` window, the model's own example of `ResourceInUseException` ("you tried to
//! update an event source mapping in the CREATING state") — so once that window has closed
//! every further update is accepted, including one that arrives while a change this mapping
//! already acknowledged is still being applied; and a member of the request is decided
//! against the VALUE it carries, which no state of the mapping shadows.
//!
//! Each row drives real HTTP requests through the router — over the ONE mapping harness this
//! binary has, [`crate::event_source_mappings`] — and reads the mapping back through
//! `GetEventSourceMapping`, so a change that was answered but never persisted fails here.
//! The rows differ along what the rule generalises over: WHICH member a step states (the
//! Amazon SQS scaling configuration, the batch size, the batching window, a pause), how many
//! steps follow one another without a settle in between, and whether the value a step states
//! is inside the range its own member publishes.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    await_mapping_state, call, create_function, create_mapping, create_request, get_mapping,
    router_on, Answer, MAPPINGS_URI, QUEUE_SOURCE, STATE_DISABLED, STATE_ENABLED,
};

/// The `State` a mapping an update was accepted for carries, from
/// `EventSourceMappingConfiguration$State`: "It can be one of the following: Creating,
/// Enabling, Enabled, Disabling, Disabled, Updating, or Deleting."
const STATE_UPDATING: &str = "Updating";

/// The error code the boundary answers for a member whose value is outside the range the
/// model declares for it, which is the refusal a value is held to whatever state the resource
/// it is stated for is in.
const VALUE_REFUSAL_CODE: &str = "ValidationException";

/// The ends of the range `ScalingConfig$MaximumConcurrency` publishes
/// (`smithy_query(query="shape", shape="MaximumConcurrency")`: `@range(min: 2, max: 1000)`),
/// which is what a value stated for it is decided against.
const CONCURRENCY_FLOOR: i64 = 2;
/// See [`CONCURRENCY_FLOOR`].
const CONCURRENCY_CEILING: i64 = 1000;

/// `UpdateEventSourceMapping` on the mapping `uuid` names.
async fn update_mapping(router: &MultiServiceRouter, uuid: &str, body: &Value) -> Answer {
    call(router, "PUT", &format!("{MAPPINGS_URI}/{uuid}"), body).await
}

/// A queue mapping of `function`, created and waited out of its `Creating` window, so every
/// row below starts from a mapping that is settled.
async fn given_settled_queue_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
) -> String {
    create_function(router, function).await;
    let created = create_mapping(
        router,
        &create_request(function, Some(QUEUE_SOURCE), &json!({ "BatchSize": 4 })),
    )
    .await;
    assert_eq!(
        created.status,
        202,
        "case {case:?}: the mapping under test must be created, got {} {}",
        created.status,
        created.text()
    );
    let uuid = created
        .string("UUID")
        .unwrap_or_else(|| panic!("case {case:?}: the create answers a UUID, got {}", created.text()));
    // The settle out of `Creating` is owed by the CLOCK, so this waits for the mapping to
    // REPORT it rather than for a span of its own.
    await_mapping_state(router, case, &uuid, STATE_ENABLED).await;
    uuid
}

/// One row of [`a_settled_mapping_takes_every_update_in_a_run`]: a run of update bodies sent
/// one after another with no settle in between, what the mapping must report at the end of
/// the run, and the state it settles into.
struct RunCase {
    name: &'static str,
    function: &'static str,
    /// The bodies of the run, in order. Each must be accepted.
    steps: Vec<Value>,
    /// What the mapping reports once the whole run has been accepted, each as
    /// `(member, value)`; a `Null` expectation means the member must be ABSENT.
    expected: Vec<(&'static str, Value)>,
    settles_into: &'static str,
}

/// The rows: runs that differ in which members they state, how long the run is, and whether
/// the last step sets or removes a setting.
fn run_cases() -> Vec<RunCase> {
    vec![
        RunCase {
            // The scenario the scaling configuration is reconfigured through: set to the
            // floor of its own published range, then raised, then removed.
            name: "the scaling configuration set, raised and then removed",
            function: "esm-recfg-scaling",
            steps: vec![
                json!({ "ScalingConfig": { "MaximumConcurrency": CONCURRENCY_FLOOR } }),
                json!({ "ScalingConfig": { "MaximumConcurrency": 50 } }),
                json!({ "ScalingConfig": {} }),
            ],
            // "To remove the configuration, pass an empty value" (`ScalingConfig`): the
            // mapping then has no scaling configuration at all, exactly as one created
            // without one.
            expected: vec![("ScalingConfig", Value::Null), ("BatchSize", json!(4))],
            settles_into: STATE_ENABLED,
        },
        RunCase {
            name: "the scaling configuration set at the ceiling of its range",
            function: "esm-recfg-ceiling",
            steps: vec![
                json!({ "ScalingConfig": { "MaximumConcurrency": CONCURRENCY_FLOOR } }),
                json!({ "ScalingConfig": { "MaximumConcurrency": CONCURRENCY_CEILING } }),
            ],
            expected: vec![(
                "ScalingConfig",
                json!({ "MaximumConcurrency": CONCURRENCY_CEILING }),
            )],
            settles_into: STATE_ENABLED,
        },
        RunCase {
            name: "two batch sizes and a batching window in one run",
            function: "esm-recfg-batching",
            steps: vec![
                json!({ "BatchSize": 7 }),
                json!({ "BatchSize": 20, "MaximumBatchingWindowInSeconds": 5 }),
            ],
            expected: vec![
                ("BatchSize", json!(20)),
                ("MaximumBatchingWindowInSeconds", json!(5)),
            ],
            settles_into: STATE_ENABLED,
        },
        RunCase {
            name: "a re-batch followed by a pause, which the mapping settles into",
            function: "esm-recfg-pause",
            steps: vec![
                json!({ "BatchSize": 9 }),
                json!({ "Enabled": false }),
            ],
            expected: vec![("BatchSize", json!(9))],
            settles_into: STATE_DISABLED,
        },
        RunCase {
            name: "one step alone, which is a run of length one",
            function: "esm-recfg-single",
            steps: vec![json!({ "ScalingConfig": { "MaximumConcurrency": 10 } })],
            expected: vec![("ScalingConfig", json!({ "MaximumConcurrency": 10 }))],
            settles_into: STATE_ENABLED,
        },
    ]
}

#[tokio::test]
async fn a_settled_mapping_takes_every_update_in_a_run() {
    for case in run_cases() {
        let label = case.name;
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = router_on(&engine);
        let uuid = given_settled_queue_mapping(&router, label, case.function).await;

        for (step, body) in case.steps.iter().enumerate() {
            let answer = update_mapping(&router, &uuid, body).await;
            assert_eq!(
                answer.status,
                202,
                "case {label:?}: step {step} ({body}) must be accepted, got {} {}",
                answer.status,
                answer.text()
            );
            assert_eq!(
                answer.string("State").as_deref(),
                Some(STATE_UPDATING),
                "case {label:?}: step {step} must answer the mapping as {STATE_UPDATING}, got {}",
                answer.text()
            );
        }

        // Read back: the run's LAST value of each member it stated is the stored one.
        let reading = get_mapping(&router, &uuid).await;
        assert_eq!(
            reading.status,
            200,
            "case {label:?}: the mapping must read back, got {} {}",
            reading.status,
            reading.text()
        );
        for (member, value) in &case.expected {
            let got = reading.json().get(*member).cloned().unwrap_or(Value::Null);
            assert_eq!(
                &got, value,
                "case {label:?}: expected {member} to report {value}, got {got} in {}",
                reading.text()
            );
        }

        // And the mapping leaves the `Updating` window the run left it in, for the state its
        // `Enabled` setting asks for.
        await_mapping_state(&router, label, &uuid, case.settles_into).await;
    }
}

#[tokio::test]
async fn a_value_outside_a_members_range_is_refused_whatever_the_mapping_is_doing() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let uuid = given_settled_queue_mapping(&router, "value refusals", "esm-recfg-values").await;

    // Each row states one value outside the range its own member publishes. The rows run one
    // after another, so every row but the first reaches a mapping that is still applying the
    // change the row before it made: a value refusal is decided on the VALUE, not on what the
    // mapping is doing.
    for (label, body, member) in [
        (
            "a concurrency below the floor of its range",
            json!({ "ScalingConfig": { "MaximumConcurrency": CONCURRENCY_FLOOR - 1 } }),
            "MaximumConcurrency",
        ),
        (
            "a concurrency above the ceiling of its range",
            json!({ "ScalingConfig": { "MaximumConcurrency": CONCURRENCY_CEILING + 1 } }),
            "MaximumConcurrency",
        ),
        (
            "a batch size below the floor of its range",
            json!({ "BatchSize": 0 }),
            "BatchSize",
        ),
        (
            "a batching window above the ceiling of its range",
            json!({ "MaximumBatchingWindowInSeconds": 301 }),
            "MaximumBatchingWindowInSeconds",
        ),
    ] {
        let refused = update_mapping(&router, &uuid, &body).await;
        assert_eq!(
            refused.status,
            400,
            "case {label:?}: expected the value to be refused with 400, got {} {}",
            refused.status,
            refused.text()
        );
        // The refusal is about the VALUE: the constraint refusal the boundary answers for a
        // member outside the range the model declares for it, naming that member — not the
        // mapping's availability.
        assert!(
            refused.error_code().contains(VALUE_REFUSAL_CODE),
            "case {label:?}: expected {VALUE_REFUSAL_CODE}, got {} {}",
            refused.error_code(),
            refused.text()
        );
        let message = refused
            .string("message")
            .or_else(|| refused.string("Message"))
            .unwrap_or_default();
        assert!(
            message.to_lowercase().contains(&member.to_lowercase()),
            "case {label:?}: the refusal must name the member {member}, got {message:?}"
        );
        // Nothing of a refused update is stored: the mapping still carries no scaling
        // configuration and the batch size its create stated.
        let reading = get_mapping(&router, &uuid).await;
        assert_eq!(
            (
                reading.json().get("ScalingConfig").cloned(),
                reading.json().get("BatchSize").cloned()
            ),
            (None, Some(json!(4))),
            "case {label:?}: a refused update must change nothing, got {}",
            reading.text()
        );
        // A value INSIDE the range is accepted at the same instant, and then removed again so
        // the next row starts from the same mapping.
        for accepted in [
            json!({ "ScalingConfig": { "MaximumConcurrency": CONCURRENCY_FLOOR } }),
            json!({ "ScalingConfig": {} }),
        ] {
            let answer = update_mapping(&router, &uuid, &accepted).await;
            assert_eq!(
                answer.status,
                202,
                "case {label:?}: the value inside the range ({accepted}) must be accepted, got \
                 {} {}",
                answer.status,
                answer.text()
            );
        }
    }
}
