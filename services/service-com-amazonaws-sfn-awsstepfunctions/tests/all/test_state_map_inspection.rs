// SPDX-License-Identifier: CC0-1.0

//! What `TestState` reports about the ITERATION side of the one state it ran, and what
//! it makes of the mocked outcome the request stands in for that iteration with —
//! driven over this service's own wire protocol through the production router, so what
//! is asserted is what a signed AWS SDK request observes.
//!
//! The rules under test:
//!
//! 1. a `Map` state's `inspectionData` carries the stages its items pass through —
//!    `afterItemsPath` (the effective input after the `ItemsPath` filter),
//!    `afterItemSelector` (the input each iteration is handed), `afterItemBatcher` (the
//!    batches a declared `ItemBatcher` groups them into) — and the bounds the state
//!    DECLARES (`maxConcurrency`, `toleratedFailureCount`, `toleratedFailurePercentage`);
//!    a state of any other type, and a bound the definition leaves out, carry none;
//! 2. a `Map` state whose failed-iteration count goes PAST the tolerated-failure
//!    threshold it declared fails with `States.ExceedToleratedFailureThreshold` and the
//!    service's fixed sentence for the breach, superseding whatever the iterations
//!    themselves failed with; a count the threshold still tolerates leaves the
//!    iteration's own error standing; the breach ENDS the state, so what the state's own
//!    `Catch` declares for that error does not save it — and an EXECUTION's `Map` state,
//!    whose items really run, ends with the same error and the same sentence;
//! 3. a failed-iteration count describing more iterations than the state HAS is a
//!    defect of the request, refused 400 in the service's own sentence;
//! 4. a failure a `Catch` of the state took over reports the document that catcher's
//!    `ResultPath` placed as its `afterResultPath` stage: the error output is the
//!    result such a state has;
//! 5. a `Task` state whose integration call this operation is to MAKE — the one type
//!    whose own work is a call — is tested only with the `roleArn` that call is made
//!    as, and a request naming none is refused 400 in the service's own sentence;
//! 6. a `stateConfiguration.errorCausedByState` naming a state the submitted definition
//!    declares none of is refused 400 in the service's own sentence;
//! 7. a `mock` naming both a result and an error output is refused 400 in the service's
//!    fixed sentence, whichever members the error output carries and whichever
//!    validation mode the mock asks for; a request asking for the SECRETS of an
//!    invocation a mock stood in for is refused 400 too, and a mode outside the
//!    modelled enum is refused with the value set in the order this service states it;
//! 8. the document an unresolved-JSONPath cause QUOTES is the one the caller supplied,
//!    in the bytes the caller supplied it in — the request's own `input` and the
//!    request's own mocked `result` alike.
//!
//! The harness (`Emulator`, `error_code`) is the one `state_machine_lifecycle` builds;
//! `tests/all/main.rs` is this binary's crate root, so it is reached through
//! `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::state_machine_lifecycle::{
    create_machine, describe_execution, error_code, start_execution, Emulator, ROLE_ARN,
};

/// The `ItemProcessor` every `Map` state of this suite iterates with: one state that
/// carries its item through, since what is under test is the state around it.
const PROCESSOR: &str =
    r#""ItemProcessor":{"StartAt":"P","States":{"P":{"Type":"Pass","End":true}}}"#;

/// The mocked failure of a `Map` iteration: the error one iteration raised, which is
/// what the tolerated-failure threshold decides the fate of.
const ITERATION_ERROR: &str = "States.TaskFailed";
const ITERATION_CAUSE: &str = "the mocked iteration raised";

/// `States.ExceedToleratedFailureThreshold`, and the service's fixed sentence for it.
const THRESHOLD_ERROR: &str = "States.ExceedToleratedFailureThreshold";
const THRESHOLD_CAUSE: &str = "The specified tolerated failure threshold was exceeded";

/// How often a wait looks at an execution again, and how long it waits at all.
///
/// The wait is for the CONDITION — the execution leaving `RUNNING` — so the deadline is
/// generous and a host that settles an execution quickly spends none of it: nothing here
/// measures how fast the host runs.
const SETTLE_POLL: Duration = Duration::from_millis(25);
/// See [`SETTLE_POLL`].
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// One `Map` state's definition, from the members a row varies.
fn map_state(members: &str) -> String {
    format!(r#"{{"Type":"Map",{members},{PROCESSOR},"End":true}}"#)
}

/// `TestState`'s request body, at the level that reports the data flow.
fn traced(definition: &str, input: &str) -> Value {
    json!({
        "definition": definition,
        "input": input,
        "roleArn": ROLE_ARN,
        "inspectionLevel": "DEBUG",
    })
}

/// The TEXT one `inspectionData` member carries, or `None` when the answer reports no
/// such member.
///
/// The stages are compared as TEXT and never as parsed documents: what is under test is
/// which BYTES each stage carries, which a parse would discard.
fn stage<'a>(body: &'a Value, member: &str) -> Option<&'a str> {
    body.get("inspectionData")?.get(member)?.as_str()
}

/// One numeric `inspectionData` member, as the JSON number it is reported as.
fn bound(body: &Value, member: &str) -> Option<f64> {
    body.get("inspectionData")?.get(member)?.as_f64()
}

// ---------------------------------------------------------------------------
// Rule 1: the stages and bounds only a Map state has
// ---------------------------------------------------------------------------

/// One row of the Map-inspection table.
struct MapStageCase {
    case: &'static str,
    /// The state under test, as the whole `definition` the request carries.
    definition: String,
    /// The `input` the state is tested on, as the caller wrote it.
    input: &'static str,
    /// The `mock` the request carries.
    mock: Value,
    /// Each stage of the answer and the exact text it must carry, `None` for a stage
    /// this state has none of.
    stages: Vec<(&'static str, Option<&'static str>)>,
    /// Each numeric bound of the answer and the figure it must carry, `None` for a
    /// bound this state declares none of.
    bounds: Vec<(&'static str, Option<f64>)>,
}

/// A `Map` state's `inspectionData` reports the stages its items pass through and the
/// bounds it declared; every other state type reports none of them.
///
/// Rows vary along where the items come from (an `ItemsPath`, the whole input, an
/// `Items` member, an `ItemReader`'s dataset), whether the state transforms each item
/// (`ItemSelector`, the legacy `Parameters`), whether it groups them (`ItemBatcher`),
/// which bounds it declares and in which form (the member, its reference-path form),
/// how many items it iterates, and the type of the state itself.
#[tokio::test]
async fn a_map_state_reports_its_iteration_stages_and_the_bounds_it_declares() {
    let selector = r#""ItemSelector":{"ref.$":"$$.Map.Item.Value.ref","pos.$":"$$.Map.Item.Index"}"#;
    let cases = vec![
        MapStageCase {
            // The shape of the recorded call this suite's issue was filed about.
            case: "an ItemsPath, an ItemSelector and both bounds",
            definition: map_state(&format!(
                r#""ItemsPath":"$.refs",{selector},"MaxConcurrency":2,"ToleratedFailureCount":1"#
            )),
            input: r#"{"refs": [{"ref":"r1"}, {"ref":"r2"}, {"ref":"r3"}]}"#,
            mock: json!({ "result": r#"[{"ok":1},{"ok":2},{"ok":3}]"# }),
            stages: vec![
                (
                    "afterItemsPath",
                    Some(r#"[{"ref":"r1"}, {"ref":"r2"}, {"ref":"r3"}]"#),
                ),
                (
                    "afterItemSelector",
                    Some(r#"[{"ref":"r1","pos":0}, {"ref":"r2","pos":1}, {"ref":"r3","pos":2}]"#),
                ),
                ("afterItemBatcher", None),
                ("afterItemsPointer", None),
            ],
            bounds: vec![
                ("maxConcurrency", Some(2.0)),
                ("toleratedFailureCount", Some(1.0)),
                ("toleratedFailurePercentage", None),
            ],
        },
        MapStageCase {
            case: "no ItemsPath, so the whole effective input is the item array",
            definition: map_state(r#""Comment":"every member defaulted""#),
            input: r#"[1, 2]"#,
            mock: json!({ "result": "[1,2]" }),
            stages: vec![
                ("afterItemsPath", Some("[1, 2]")),
                ("afterItemSelector", Some("[1, 2]")),
                ("afterItemBatcher", None),
            ],
            bounds: vec![
                ("maxConcurrency", None),
                ("toleratedFailureCount", None),
                ("toleratedFailurePercentage", None),
            ],
        },
        MapStageCase {
            case: "an Items member passes its items through no ItemsPath filter",
            definition: map_state(r#""Items":[{"n":1},{"n":2}]"#),
            input: "{}",
            mock: json!({ "result": "[1,2]" }),
            stages: vec![
                ("afterItemsPath", None),
                ("afterItemSelector", Some(r#"[{"n":1}, {"n":2}]"#)),
            ],
            bounds: vec![("maxConcurrency", None)],
        },
        MapStageCase {
            case: "an ItemBatcher groups the selected items into the batches iterated",
            definition: map_state(
                r#""ItemsPath":"$.refs","ItemBatcher":{"MaxItemsPerBatch":2},"MaxConcurrency":1"#,
            ),
            input: r#"{"refs":[1,2,3]}"#,
            mock: json!({ "result": "[1,2]" }),
            stages: vec![
                ("afterItemsPath", Some("[1, 2, 3]")),
                ("afterItemSelector", Some("[1, 2, 3]")),
                (
                    "afterItemBatcher",
                    Some(r#"[{"Items":[1,2]}, {"Items":[3]}]"#),
                ),
            ],
            bounds: vec![("maxConcurrency", Some(1.0))],
        },
        MapStageCase {
            case: "the percentage form of the threshold, and the path form of the concurrency",
            definition: map_state(
                r#""ItemsPath":"$.refs","MaxConcurrencyPath":"$.lanes","ToleratedFailurePercentage":50"#,
            ),
            input: r#"{"refs":["a"],"lanes":4}"#,
            mock: json!({ "result": "[1]" }),
            stages: vec![("afterItemsPath", Some(r#"["a"]"#))],
            bounds: vec![
                ("maxConcurrency", Some(4.0)),
                ("toleratedFailureCount", None),
                ("toleratedFailurePercentage", Some(50.0)),
            ],
        },
        MapStageCase {
            case: "an empty item array iterates nothing and still reports its stages",
            definition: map_state(r#""ItemsPath":"$.refs","ToleratedFailureCount":0"#),
            input: r#"{"refs":[]}"#,
            mock: json!({ "result": "[]" }),
            stages: vec![
                ("afterItemsPath", Some("[]")),
                ("afterItemSelector", Some("[]")),
            ],
            bounds: vec![("toleratedFailureCount", Some(0.0))],
        },
        MapStageCase {
            case: "an ItemsPath that points at no array has no stage to report",
            definition: map_state(r#""ItemsPath":"$.refs","MaxConcurrency":3"#),
            input: r#"{"refs":{"not":"an array"}}"#,
            mock: json!({ "result": "[]" }),
            stages: vec![("afterItemsPath", None), ("afterItemSelector", None)],
            // A bound the state declares is reported whether or not its items could
            // be derived: the figure is the definition's own.
            bounds: vec![("maxConcurrency", Some(3.0))],
        },
        MapStageCase {
            case: "a Task state has no iteration stage and no Map bound at all",
            definition: r#"{"Type":"Task","Resource":"arn:aws:states:::lambda:invoke","End":true}"#
                .to_string(),
            input: r#"{"refs":[1,2]}"#,
            mock: json!({ "result": r#"{"ok":true}"#, "fieldValidationMode": "NONE" }),
            stages: vec![
                ("afterItemsPath", None),
                ("afterItemSelector", None),
                ("afterItemBatcher", None),
            ],
            bounds: vec![
                ("maxConcurrency", None),
                ("toleratedFailureCount", None),
                ("toleratedFailurePercentage", None),
            ],
        },
    ];

    let emulator = Emulator::new();
    for case in &cases {
        let label = case.case;
        let mut request = traced(&case.definition, case.input);
        request["mock"] = case.mock.clone();
        let answered = emulator.ok("TestState", &request).await;
        assert_eq!(
            answered.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {label:?}: expected the mocked state to produce its output, got {answered:?}"
        );
        for (member, expected) in &case.stages {
            assert_eq!(
                stage(&answered, member),
                *expected,
                "case {label:?}: expected inspectionData.{member} to carry {expected:?}, got \
                 {answered:?}"
            );
        }
        for (member, expected) in &case.bounds {
            assert_eq!(
                bound(&answered, member),
                *expected,
                "case {label:?}: expected inspectionData.{member} to carry {expected:?}, got \
                 {answered:?}"
            );
        }
    }
}

/// A `Map` state that produced a result reports the result-side stages beside its
/// per-iteration ones: the iteration stages are added to the state's data flow rather
/// than replacing it.
#[tokio::test]
async fn a_map_state_reports_its_result_side_stages_beside_its_iteration_stages() {
    let emulator = Emulator::new();
    let definition = map_state(r#""ItemsPath":"$.refs","ResultPath":"$.done","MaxConcurrency":2"#);
    let mut request = traced(&definition, r#"{"refs":[1,2]}"#);
    request["mock"] = json!({ "result": r#"["a","b"]"# });
    let answered = emulator.ok("TestState", &request).await;
    for (member, expected) in [
        ("afterItemsPath", r#"[1, 2]"#),
        ("afterItemSelector", r#"[1, 2]"#),
        ("result", r#"["a","b"]"#),
        ("afterResultSelector", r#"["a","b"]"#),
        ("afterResultPath", r#"{"refs":[1,2],"done":["a","b"]}"#),
    ] {
        assert_eq!(
            stage(&answered, member),
            Some(expected),
            "expected inspectionData.{member} {expected:?}, got {answered:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 2: the tolerated-failure threshold decides the state's outcome
// ---------------------------------------------------------------------------

/// One row of the threshold table.
struct ThresholdCase {
    case: &'static str,
    /// The bounds and error handling the `Map` state declares.
    members: String,
    /// How many items the state iterates.
    items: &'static str,
    /// `stateConfiguration.mapIterationFailureCount`.
    failures: Option<i64>,
    status: &'static str,
    error: &'static str,
    cause: &'static str,
}

/// A `Map` state whose failed-iteration count goes past the threshold it declared fails
/// with the BREACH of that threshold; a count the threshold still tolerates leaves the
/// iteration's own error standing.
///
/// Rows vary along which form of the threshold the state declares (count, percentage,
/// both, neither), where the count falls against it (under, at, over), how many items
/// there are for a percentage to be a share of, and whether the state declares a
/// catcher for the error the breach raises — which does not save the state from it,
/// since the breach is what ended the state.
#[tokio::test]
async fn a_map_state_past_its_tolerated_failure_threshold_fails_with_the_breach() {
    const CATCH_THRESHOLD: &str =
        r#""Catch":[{"ErrorEquals":["States.ExceedToleratedFailureThreshold"],"Next":"Handle"}]"#;
    let cases = vec![
        ThresholdCase {
            case: "one failure, one tolerated: the iteration's own error stands",
            members: r#""ItemsPath":"$.refs","ToleratedFailureCount":1"#.to_string(),
            items: "[1,2,3]",
            failures: Some(1),
            status: "FAILED",
            error: ITERATION_ERROR,
            cause: ITERATION_CAUSE,
        },
        ThresholdCase {
            case: "two failures, one tolerated: the threshold is what terminates it",
            members: r#""ItemsPath":"$.refs","ToleratedFailureCount":1"#.to_string(),
            items: "[1,2,3]",
            failures: Some(2),
            status: "FAILED",
            error: THRESHOLD_ERROR,
            cause: THRESHOLD_CAUSE,
        },
        ThresholdCase {
            case: "a declared threshold of none is exceeded by the first failure",
            members: r#""ItemsPath":"$.refs","ToleratedFailureCount":0"#.to_string(),
            items: "[1,2,3]",
            failures: Some(1),
            status: "FAILED",
            error: THRESHOLD_ERROR,
            cause: THRESHOLD_CAUSE,
        },
        ThresholdCase {
            case: "no threshold declared at all: there is none to exceed",
            members: r#""ItemsPath":"$.refs""#.to_string(),
            items: "[1,2,3]",
            failures: Some(3),
            status: "FAILED",
            error: ITERATION_ERROR,
            cause: ITERATION_CAUSE,
        },
        ThresholdCase {
            case: "one failure of three is inside the tolerated share",
            members: r#""ItemsPath":"$.refs","ToleratedFailurePercentage":50"#.to_string(),
            items: "[1,2,3]",
            failures: Some(1),
            status: "FAILED",
            error: ITERATION_ERROR,
            cause: ITERATION_CAUSE,
        },
        ThresholdCase {
            case: "two failures of three are past the tolerated share",
            members: r#""ItemsPath":"$.refs","ToleratedFailurePercentage":50"#.to_string(),
            items: "[1,2,3]",
            failures: Some(2),
            status: "FAILED",
            error: THRESHOLD_ERROR,
            cause: THRESHOLD_CAUSE,
        },
        ThresholdCase {
            case: "either threshold on its own tolerates the failures",
            members:
                r#""ItemsPath":"$.refs","ToleratedFailureCount":0,"ToleratedFailurePercentage":75"#
                    .to_string(),
            items: "[1,2,3,4]",
            failures: Some(3),
            status: "FAILED",
            error: ITERATION_ERROR,
            cause: ITERATION_CAUSE,
        },
        ThresholdCase {
            case: "no failed iteration is reported at all",
            members: r#""ItemsPath":"$.refs","ToleratedFailureCount":0"#.to_string(),
            items: "[1,2,3]",
            failures: None,
            status: "FAILED",
            error: ITERATION_ERROR,
            cause: ITERATION_CAUSE,
        },
        ThresholdCase {
            case: "a count of zero failures breaches nothing",
            members: r#""ItemsPath":"$.refs","ToleratedFailureCount":0"#.to_string(),
            items: "[1,2,3]",
            failures: Some(0),
            status: "FAILED",
            error: ITERATION_ERROR,
            cause: ITERATION_CAUSE,
        },
        ThresholdCase {
            case: "the state's own handler does not save it from the breach",
            members: format!(r#""ItemsPath":"$.refs","ToleratedFailureCount":1,{CATCH_THRESHOLD}"#),
            items: "[1,2,3]",
            failures: Some(3),
            status: "FAILED",
            error: THRESHOLD_ERROR,
            cause: THRESHOLD_CAUSE,
        },
    ];

    let emulator = Emulator::new();
    for case in &cases {
        let label = case.case;
        let definition = map_state(&case.members);
        let input = format!(r#"{{"refs":{}}}"#, case.items);
        let mut request = traced(&definition, &input);
        request["mock"] = json!({
            "errorOutput": { "error": ITERATION_ERROR, "cause": ITERATION_CAUSE },
        });
        if let Some(failures) = case.failures {
            request["stateConfiguration"] = json!({ "mapIterationFailureCount": failures });
        }
        let answered = emulator.ok("TestState", &request).await;
        for (member, expected) in [
            ("status", case.status),
            ("error", case.error),
            ("cause", case.cause),
        ] {
            assert_eq!(
                answered.get(member).and_then(Value::as_str),
                Some(expected),
                "case {label:?}: expected {member} {expected:?}, got {answered:?}"
            );
        }
        // "If you haven't defined a next state in your definition or if the execution
        // of the state FAILS, this field doesn't contain a value" — and a state that
        // failed produced no output either, whichever handler the state declares for
        // the error it failed with.
        for member in ["nextState", "output"] {
            assert_eq!(
                answered.get(member),
                None,
                "case {label:?}: expected a failed state to report no {member}, got {answered:?}"
            );
        }
    }
}

/// The breach of the threshold supersedes the iteration's error on the response thatstill reports the state's per-iteration stages: both are the answer to one call.
#[tokio::test]
async fn the_breach_is_reported_beside_the_states_iteration_stages() {
    let emulator = Emulator::new();
    let definition = map_state(
        r#""ItemsPath":"$.refs","ItemSelector":{"ref.$":"$$.Map.Item.Value"},"MaxConcurrency":2,"ToleratedFailureCount":1"#,
    );
    let mut request = traced(&definition, r#"{"refs":["a", "b", "c"]}"#);
    request["mock"] = json!({
        "errorOutput": { "error": ITERATION_ERROR, "cause": ITERATION_CAUSE },
    });
    request["stateConfiguration"] = json!({ "mapIterationFailureCount": 2 });
    let answered = emulator.ok("TestState", &request).await;
    assert_eq!(
        answered.get("error").and_then(Value::as_str),
        Some(THRESHOLD_ERROR),
        "expected the threshold error, got {answered:?}"
    );
    for (member, expected) in [
        ("afterItemsPath", r#"["a", "b", "c"]"#),
        (
            "afterItemSelector",
            r#"[{"ref":"a"}, {"ref":"b"}, {"ref":"c"}]"#,
        ),
    ] {
        assert_eq!(
            stage(&answered, member),
            Some(expected),
            "expected inspectionData.{member} {expected:?}, got {answered:?}"
        );
    }
    assert_eq!(
        bound(&answered, "toleratedFailureCount"),
        Some(1.0),
        "expected the declared threshold, got {answered:?}"
    );
}

/// The breach is one rule with one sentence, whichever walk reaches it: an EXECUTION's
/// own `Map` state, whose items really run and really fail, ends with the same error and
/// the same fixed cause the single-state trial reports.
#[tokio::test]
async fn an_executions_map_state_reports_the_same_breach() {
    let emulator = Emulator::new();
    let definition = json!({
        "StartAt": "Fan",
        "States": {
            "Fan": {
                "Type": "Map",
                "ItemsPath": "$.refs",
                // Declared, so the state HAS a threshold for its failed items to
                // exceed; the items themselves fail of their own accord.
                "ToleratedFailureCount": 1,
                "ItemProcessor": {
                    "StartAt": "Boom",
                    "States": {
                        "Boom": {
                            "Type": "Fail",
                            "Error": "Custom.Item",
                            "Cause": "the item failed",
                        },
                    },
                },
                "End": true,
            },
        },
    })
    .to_string();
    let machine = create_machine(&emulator, "map-threshold-breach", &definition).await;
    let execution = start_execution(&emulator, &machine, "only", r#"{"refs":[1,2,3]}"#).await;
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let described = loop {
        let described = describe_execution(&emulator, &execution).await;
        if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
            break described;
        }
        assert!(
            Instant::now() < deadline,
            "expected the execution to settle, got {described:?}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    };
    for (member, expected) in [
        ("status", "FAILED"),
        ("error", THRESHOLD_ERROR),
        ("cause", THRESHOLD_CAUSE),
    ] {
        assert_eq!(
            described.get(member).and_then(Value::as_str),
            Some(expected),
            "expected the execution's {member} {expected:?}, got {described:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 3: a failed-iteration count past the state's iterations
// ---------------------------------------------------------------------------
/// A `stateConfiguration.mapIterationFailureCount` counting more failed iterations than
/// the state under test HAS describes no invocation of that state, so the request is
/// refused.
///
/// Rows vary along how many iterations the state has (several, one, none, and batches
/// rather than items) and by how far the count goes past them.
#[tokio::test]
async fn a_failed_iteration_count_past_the_states_iterations_is_refused() {
    const REFUSAL: &str =
        "Map iteration failure count must be less than or equal to the number of Map iterations";
    // (case, the Map members, the item array, the count the request carries)
    let cases: [(&str, &str, &str, i64); 4] = [
        (
            "one more failure than the state has items",
            r#""ItemsPath":"$.refs","ToleratedFailureCount":1"#,
            "[1,2,3]",
            4,
        ),
        (
            "a failure counted for a state that iterates nothing",
            r#""ItemsPath":"$.refs""#,
            "[]",
            1,
        ),
        (
            "far past the one item the state iterates",
            r#""ItemsPath":"$.refs","ToleratedFailurePercentage":100"#,
            "[1]",
            7,
        ),
        (
            "more failures than the batches a batched state iterates",
            r#""ItemsPath":"$.refs","ItemBatcher":{"MaxItemsPerBatch":2}"#,
            "[1,2,3]",
            3,
        ),
    ];

    let emulator = Emulator::new();
    for (case, members, items, failures) in cases {
        let definition = map_state(members);
        let input = format!(r#"{{"refs":{items}}}"#);
        let mut request = traced(&definition, &input);
        request["mock"] = json!({
            "errorOutput": { "error": ITERATION_ERROR, "cause": ITERATION_CAUSE },
        });
        request["stateConfiguration"] = json!({ "mapIterationFailureCount": failures });
        let (status, body) = emulator.call("TestState", &request).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected a 400 ValidationException, got {status} with {body:?}"
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(REFUSAL),
            "case {case:?}: expected the refusal {REFUSAL:?}, got {body:?}"
        );
    }
}

/// A failed-iteration count the state's own iterations carry is not refused: the count
/// at the state's iteration count is as many failures as it can have.
#[tokio::test]
async fn a_failed_iteration_count_within_the_states_iterations_is_served() {
    let emulator = Emulator::new();
    let definition = map_state(r#""ItemsPath":"$.refs","ToleratedFailurePercentage":100"#);
    let mut request = traced(&definition, r#"{"refs":[1,2,3]}"#);
    request["mock"] = json!({
        "errorOutput": { "error": ITERATION_ERROR, "cause": ITERATION_CAUSE },
    });
    request["stateConfiguration"] = json!({ "mapIterationFailureCount": 3 });
    let answered = emulator.ok("TestState", &request).await;
    assert_eq!(
        answered.get("error").and_then(Value::as_str),
        Some(ITERATION_ERROR),
        "expected every iteration's failure to stay inside the whole-share threshold, got \
         {answered:?}"
    );
}

// ---------------------------------------------------------------------------
// Rule 4: the result-side stage a caught failure reports
// ---------------------------------------------------------------------------

/// A caught failure reports the document the CATCHER's `ResultPath` placed as its
/// `afterResultPath` stage: the error output is the result such a state has.
///
/// Rows vary along where the catcher places the error output (a member of the raw
/// input, the whole output, a discarded one) and along the state type that failed.
#[tokio::test]
async fn a_caught_failure_reports_the_document_its_catcher_placed() {
    // (case, the catcher's own members, the state's input, the stage it must carry)
    let cases: [(&str, &str, &str, &str); 3] = [
        (
            "the catcher places the error output under a member of the raw input",
            r#""ResultPath":"$.failure""#,
            r#"{"refs":[1]}"#,
            r#"{"refs":[1],"failure":{"Error":"States.TaskFailed","Cause":"the mocked iteration raised"}}"#,
        ),
        (
            "the catcher declares no ResultPath, so the error output is the whole result",
            "",
            r#"{"refs":[1]}"#,
            r#"{"Error":"States.TaskFailed","Cause":"the mocked iteration raised"}"#,
        ),
        (
            "a null ResultPath keeps the state's own raw input",
            r#""ResultPath":null"#,
            r#"{"refs":[1]}"#,
            r#"{"refs":[1]}"#,
        ),
    ];

    let emulator = Emulator::new();
    for (case, members, input, expected) in cases {
        let separator = if members.is_empty() { "" } else { "," };
        let catcher = format!(
            r#""Catch":[{{"ErrorEquals":["{ITERATION_ERROR}"],"Next":"Handle"{separator}{members}}}]"#
        );
        let definition = map_state(&format!(r#""ItemsPath":"$.refs",{catcher}"#));
        let mut request = traced(&definition, input);
        request["mock"] = json!({
            "errorOutput": { "error": ITERATION_ERROR, "cause": ITERATION_CAUSE },
        });
        let answered = emulator.ok("TestState", &request).await;
        assert_eq!(
            answered.get("status").and_then(Value::as_str),
            Some("CAUGHT_ERROR"),
            "case {case:?}: expected the catcher to take the failure over, got {answered:?}"
        );
        assert_eq!(
            stage(&answered, "afterResultPath"),
            Some(expected),
            "case {case:?}: expected inspectionData.afterResultPath {expected:?}, got \
             {answered:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 5: the role a tested Task state's work is made as
// ---------------------------------------------------------------------------

/// A `Task` state whose integration call this operation is to MAKE is tested only with
/// the `roleArn` that call is made as; a mocked one, and every other state type, needs
/// none.
///
/// Rows span the state types this operation tests — the one whose work is a call, with
/// and without a mock standing in for it, and the data-flow, iteration and terminal
/// types whose work reaches no resource — each without a role, and the `Task` type with
/// one.
#[tokio::test]
async fn a_task_state_whose_call_is_made_is_tested_only_as_a_role() {
    const REFUSAL: &str = "RoleArn must be specified when testing a Task state";
    let task = r#"{"Type":"Task","Resource":"arn:aws:states:::lambda:invoke","End":true}"#;
    let iterating = map_state(r#""ItemsPath":"$.refs""#);
    // (case, the state under test, whether the request names a role, whether a mock
    // stands in for the state's work, whether the request is refused)
    let cases: [(&str, &str, bool, bool, bool); 7] = [
        ("a Task state whose call is to be made", task, false, false, true),
        (
            "the same Task state with the role its call is made as",
            task,
            true,
            false,
            false,
        ),
        (
            "a Task state whose call a mock stands in for",
            task,
            false,
            true,
            false,
        ),
        (
            "a Pass state needs none",
            r#"{"Type":"Pass","End":true}"#,
            false,
            false,
            false,
        ),
        (
            "a Wait state needs none",
            r#"{"Type":"Wait","Seconds":1,"End":true}"#,
            false,
            false,
            false,
        ),
        ("a Succeed state needs none", r#"{"Type":"Succeed"}"#, false, false, false),
        ("a mocked Map state needs none", &iterating, false, true, false),
    ];

    let emulator = Emulator::new();
    for (case, definition, with_role, mocked, refused) in cases {
        let mut request = traced(definition, r#"{"refs":[1]}"#);
        if !with_role {
            request
                .as_object_mut()
                .expect("the request body is an object")
                .remove("roleArn");
        }
        if mocked {
            request["mock"] = json!({ "result": "[1]", "fieldValidationMode": "NONE" });
        }
        let (status, body) = emulator.call("TestState", &request).await;
        if !refused {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the state to be tested, got {status} with {body:?}"
            );
            continue;
        }
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected a 400 ValidationException, got {status} with {body:?}"
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(REFUSAL),
            "case {case:?}: expected the refusal {REFUSAL:?}, got {body:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 6: the state a mocked error is said to come from
// ---------------------------------------------------------------------------
/// A `stateConfiguration.errorCausedByState` naming a state the submitted definition
/// declares none of is refused; a name the definition does declare is served.
///
/// Rows vary along where the state the name has to match is declared (inside the
/// `ItemProcessor` of the `Map` under test, inside a `Parallel` branch, at the top
/// level of a state machine definition the request names a `stateName` of) and along
/// whether the name is one of them.
#[tokio::test]
async fn an_error_caused_by_state_must_name_a_state_the_definition_declares() {
    const REFUSAL: &str = "The ErrorCausedByState specified was not found in the state definition";
    let machine = json!({
        "StartAt": "Fan",
        "States": {
            "Fan": {
                "Type": "Map",
                "ItemsPath": "$.refs",
                "ItemProcessor": {
                    "StartAt": "Inner",
                    "States": { "Inner": { "Type": "Pass", "End": true } },
                },
                "End": true,
            },
        },
    })
    .to_string();
    let parallel = json!({
        "Type": "Parallel",
        "Branches": [{
            "StartAt": "Leg",
            "States": { "Leg": { "Type": "Pass", "End": true } },
        }],
        "End": true,
    })
    .to_string();
    let iterating = map_state(r#""ItemsPath":"$.refs""#);
    // (case, the definition, the stateName the request names, the state the error is
    // said to come from, whether it is refused)
    let cases: [(&str, &str, Option<&str>, &str, bool); 5] = [
        (
            "a state of the ItemProcessor the Map under test iterates",
            &iterating,
            None,
            "P",
            false,
        ),
        (
            "a state no list of the definition declares",
            &iterating,
            None,
            "Nowhere",
            true,
        ),
        (
            "a state of one of the Parallel's branches",
            &parallel,
            None,
            "Leg",
            false,
        ),
        (
            "a state of the state machine definition the request names a state of",
            &machine,
            Some("Fan"),
            "Inner",
            false,
        ),
        (
            "the enclosing state's own name is no state of its processor",
            &machine,
            Some("Fan"),
            "Elsewhere",
            true,
        ),
    ];

    let emulator = Emulator::new();
    for (case, definition, state_name, caused_by, refused) in cases {
        let mut request = traced(definition, r#"{"refs":[1]}"#);
        request["mock"] = json!({
            "errorOutput": { "error": ITERATION_ERROR, "cause": ITERATION_CAUSE },
        });
        request["stateConfiguration"] = json!({ "errorCausedByState": caused_by });
        if let Some(state_name) = state_name {
            request["stateName"] = json!(state_name);
        }
        let (status, body) = emulator.call("TestState", &request).await;
        if !refused {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the named state to be accepted, got {status} with \
                 {body:?}"
            );
            continue;
        }
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected a 400 ValidationException, got {status} with {body:?}"
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(REFUSAL),
            "case {case:?}: expected the refusal {REFUSAL:?}, got {body:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 7: a mock is an exclusive choice between a result and an error output
// ---------------------------------------------------------------------------

/// A `mock` naming BOTH a mocked result and a mocked error output is refused in the
/// service's own fixed sentence, which lists the two members the model declares.
///
/// Rows vary along which members the error output carries, which validation mode the
/// mock asks for, and the type of the state the mock was supplied for.
#[tokio::test]
async fn a_mock_naming_both_a_result_and_an_error_output_is_refused() {
    const REFUSAL: &str = "A test mock should have only one of the following fields: \
                           [result, errorOutput].";
    let task = r#"{"Type":"Task","Resource":"arn:aws:states:::lambda:invoke","End":true}"#;
    let map = map_state(r#""ItemsPath":"$.refs""#);
    // (case, the state under test, the mock the request carries)
    let cases: [(&str, &str, Value); 5] = [
        (
            "an error output naming both an error and a cause",
            task,
            json!({
                "result": r#"{"ok":true}"#,
                "errorOutput": { "error": ITERATION_ERROR, "cause": ITERATION_CAUSE },
            }),
        ),
        (
            "an error output naming only an error",
            task,
            json!({ "result": r#"{"ok":true}"#, "errorOutput": { "error": ITERATION_ERROR } }),
        ),
        (
            "an error output naming only a cause",
            task,
            json!({ "result": r#"{"ok":true}"#, "errorOutput": { "cause": ITERATION_CAUSE } }),
        ),
        (
            "an empty error output beside a result",
            task,
            json!({ "result": r#"{"ok":true}"#, "errorOutput": {} }),
        ),
        (
            "validating nothing does not make the two members compatible",
            &map,
            json!({
                "result": "[1]",
                "errorOutput": { "error": ITERATION_ERROR },
                "fieldValidationMode": "NONE",
            }),
        ),
    ];

    let emulator = Emulator::new();
    for (case, definition, mock) in cases {
        let mut request = traced(definition, r#"{"refs":[1]}"#);
        request["mock"] = mock;
        let (status, body) = emulator.call("TestState", &request).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected a 400 ValidationException, got {status} with {body:?}"
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(REFUSAL),
            "case {case:?}: expected the refusal {REFUSAL:?}, got {body:?}"
        );
    }
}

/// A request asking for the secrets of an invocation a `mock` stood in for is refused:
/// there is no invocation of the state's own for one to be revealed of.
///
/// Rows vary along which outcome the mock names and along whether the request asks for
/// secrets at all.
#[tokio::test]
async fn revealing_secrets_of_a_mocked_invocation_is_refused() {
    const REFUSAL: &str = "TestState does not support RevealSecrets when a mock is specified.";
    let task = r#"{"Type":"Task","Resource":"arn:aws:states:::lambda:invoke","End":true}"#;
    // (case, the mock the request carries, whether it asks for secrets, refused)
    let cases: [(&str, Value, bool, bool); 4] = [
        (
            "secrets asked for beside a mocked result",
            json!({ "result": r#"{"ok":true}"#, "fieldValidationMode": "NONE" }),
            true,
            true,
        ),
        (
            "secrets asked for beside a mocked error output",
            json!({ "errorOutput": { "error": ITERATION_ERROR } }),
            true,
            true,
        ),
        (
            "secrets switched off beside a mock",
            json!({ "result": r#"{"ok":true}"#, "fieldValidationMode": "NONE" }),
            false,
            false,
        ),
        (
            "a mock with no secrets asked for at all",
            json!({ "result": r#"{"ok":true}"#, "fieldValidationMode": "NONE" }),
            false,
            false,
        ),
    ];

    let emulator = Emulator::new();
    for (case, mock, reveals, refused) in cases {
        let mut request = traced(task, "{}");
        request["mock"] = mock;
        if reveals {
            request["revealSecrets"] = json!(true);
        }
        let (status, body) = emulator.call("TestState", &request).await;
        if !refused {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the mocked state to be tested, got {status} with \
                 {body:?}"
            );
            continue;
        }
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected a 400 ValidationException, got {status} with {body:?}"
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(REFUSAL),
            "case {case:?}: expected the refusal {REFUSAL:?}, got {body:?}"
        );
    }
}

/// A `mock.fieldValidationMode` outside the modelled enum is refused with the
/// validation envelope, whose value set is listed in the order THIS SERVICE states that
/// enum's values in — not the order the model declares its members in.
///
/// Rows vary along the rejected value (a name of its own, one differing from a
/// permitted value only in case, the empty string).
#[tokio::test]
async fn an_advertised_field_validation_mode_set_is_listed_in_the_order_the_service_states_it() {
    // RECORDED for `MockResponseValidationMode`; the model declares it `STRICT |
    // PRESENT | NONE` (`smithy_query(query="shape",shape="MockResponseValidationMode")`).
    const MODES: &str = "[STRICT, NONE, PRESENT]";
    let task = r#"{"Type":"Task","Resource":"arn:aws:states:::lambda:invoke","End":true}"#;
    let emulator = Emulator::new();
    for value in ["LENIENT", "strict", ""] {
        let mut request = traced(task, "{}");
        request["mock"] = json!({ "result": "{}", "fieldValidationMode": value });
        let (status, body) = emulator.call("TestState", &request).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {value:?}: expected a 400 ValidationException, got {status} with {body:?}"
        );
        let expected = format!(
            "1 validation error detected: Value '{value}' at 'mock.fieldValidationMode' failed \
             to satisfy constraint: Member must satisfy enum value set: {MODES}"
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {value:?}: expected the message {expected:?}, got {body:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 8: the bytes an unresolved-path cause quotes the document in
// ---------------------------------------------------------------------------
/// The document an unresolved-JSONPath cause QUOTES is the one the caller supplied, in
/// the bytes the caller supplied it in.
///
/// Rows vary along which caller-supplied document the path was evaluated against (the
/// request's `input`, the request's mocked `result`) and along what a re-encoding of a
/// parsed copy would lose (insignificant whitespace, a number literal).
#[tokio::test]
async fn an_unresolved_path_quotes_the_document_as_the_caller_supplied_it() {
    // (case, the state under test, the input, the mocked result, the quoted document)
    let cases: [(&str, &str, &str, &str, &str); 4] = [
        (
            "the whitespace of the request's own input",
            r#"{"Type":"Task","Resource":"arn:aws:states:::lambda:invoke","Parameters":{"ref.$":"$.Item.id.S"},"End":true}"#,
            r#"{"Item": "a7"}"#,
            r#"{"ok":true}"#,
            r#"{"Item": "a7"}"#,
        ),
        (
            "the number literal of the request's own input",
            r#"{"Type":"Task","Resource":"arn:aws:states:::lambda:invoke","Parameters":{"ref.$":"$.Item.id.S"},"End":true}"#,
            r#"{"n": 1e2, "Item": 7.50}"#,
            r#"{"ok":true}"#,
            r#"{"n": 1e2, "Item": 7.50}"#,
        ),
        (
            "the whitespace of the request's own mocked result",
            r#"{"Type":"Task","Resource":"arn:aws:states:::lambda:invoke","ResultSelector":{"ref.$":"$.Item.id.S"},"End":true}"#,
            "{}",
            r#"{"Item": "a7"}"#,
            r#"{"Item": "a7"}"#,
        ),
        (
            "the number literal of the request's own mocked result",
            r#"{"Type":"Task","Resource":"arn:aws:states:::lambda:invoke","ResultSelector":{"ref.$":"$.Item.id.S"},"End":true}"#,
            "{}",
            r#"{"Item": 1e2}"#,
            r#"{"Item": 1e2}"#,
        ),
    ];

    let emulator = Emulator::new();
    for (case, definition, input, result, quoted) in cases {
        let mut request = traced(definition, input);
        request["mock"] = json!({ "result": result, "fieldValidationMode": "NONE" });
        let answered = emulator.ok("TestState", &request).await;
        let expected = format!(
            "The JSONPath '$.Item.id.S' specified for the field 'ref.$' could not be found in the \
             input '{quoted}'"
        );
        assert_eq!(
            answered.get("cause").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {case:?}: expected the cause {expected:?}, got {answered:?}"
        );
    }
}
