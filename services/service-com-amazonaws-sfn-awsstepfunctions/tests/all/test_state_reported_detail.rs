// SPDX-License-Identifier: CC0-1.0

//! What `TestState` REPORTS about the one state it ran: the documents of its
//! `inspectionData`, the handlers that answered for a failure, and the two state
//! types it does not test — driven over this service's own wire protocol through the
//! production router, so what is asserted is what a signed AWS SDK request observes.
//!
//! The rules under test:
//!
//! 1. every document of `inspectionData` is reported in the BYTES this service was
//!    given that document in, exactly as the state's `output` is: a stage whose
//!    document is the caller's own `input` comes back byte for byte — its whitespace,
//!    its member order and its number literals included — and only a document the
//!    state COMPUTED is reported in the encoding the walk produced it in;
//! 2. `inspectionData.afterResultSelector` — "the effective result after Step
//!    Functions applies the ResultSelector filter" — is reported for every state that
//!    produced a raw result: the document the filter built where the state declares
//!    one, and the raw result itself where no such filter applies;
//! 3. `inspectionData.errorDetails` reports the positions of the handlers that
//!    answered for a failure: the `Retry` entry that matches the error (the first
//!    position when the state declares none that does), the `Catch` entry that took it
//!    over, and the backoff before the attempt a retrier still owes;
//! 4. a JSONata expression the engine EVALUATED and that threw fails the state with
//!    the language's own query-evaluation error, under a diagnosis naming the
//!    expression, the field of the definition it is written in and the language's own
//!    account of the failure;
//! 5. the state types this operation does not test are refused in the operation's own
//!    recorded sentences: a `mock` for a type that takes none, and a `Map` or
//!    `Parallel` with no mock standing in for its own work;
//! 6. the enum value set a validation refusal ADVERTISES is listed in the order this
//!    service states that enum's values in, which is not the order the model declares
//!    its members in.
//!
//! The harness (`Emulator`, `error_code`) is the one `state_machine_lifecycle`
//! builds; `tests/all.rs` is this binary's crate root, so it is reached through
//! `crate::<suite>`.

use serde_json::{json, Value};

use crate::state_machine_lifecycle::{error_code, Emulator, ACCOUNT, REGION, ROLE_ARN};

/// A mocked outcome every row of a table can share: the invocation the state under
/// test would otherwise make failed with an error of the caller's own naming.
const MOCKED_FAILURE: &str = "Custom.Transient";
const MOCKED_CAUSE: &str = "mocked transient failure";

/// The TEXT one `inspectionData` stage carries, or `None` when the response reports
/// no such stage.
///
/// The stages are compared as TEXT and never as parsed documents: the rule under test
/// is which BYTES the stage carries, which a parse would discard.
fn stage<'a>(body: &'a Value, member: &str) -> Option<&'a str> {
    body.get("inspectionData")?.get(member)?.as_str()
}

/// `TestState`'s request body, at the level that reports the data flow.
fn traced(definition: &str, input: &str) -> Value {
    json!({
        "definition": definition,
        "input": input,
        "roleArn": ROLE_ARN,
        "inspectionLevel": "TRACE",
    })
}

// ---------------------------------------------------------------------------
// Rule 1: the bytes a stage reports
// ---------------------------------------------------------------------------

/// One row of the reported-stage table.
struct StageCase {
    case: &'static str,
    definition: &'static str,
    /// The `input` the request carries, as the caller wrote it.
    input: &'static str,
    /// The `mock` the request carries, if any.
    mock: Option<Value>,
    /// Each stage of the answer and the exact text it must carry, `None` for a stage
    /// the state has none of.
    stages: &'static [(&'static str, Option<&'static str>)],
}

/// Every `inspectionData` document is reported in the bytes this service was given
/// that document in, and a document the state computed in the encoding the walk
/// produced it in.
///
/// Rows vary along what the caller's own text carries that a re-encoding would not
/// preserve (whitespace, member order, a number literal), and along which stage's
/// document is the caller's own rather than one the state built (an `InputPath` that
/// selects a subtree, a `Parameters` template, a JSONata `Arguments` template).
#[tokio::test]
async fn every_inspection_stage_reports_the_document_as_it_was_given() {
    let cases = [
        StageCase {
            // The shape of the recorded call this suite's issue was filed about: the
            // caller's spacing survives every stage its document passes through.
            case: "the whitespace the caller wrote survives every stage",
            definition: r#"{"Type":"Pass","End":true}"#,
            input: r#"{"n": 42}"#,
            mock: None,
            stages: &[
                ("input", Some(r#"{"n": 42}"#)),
                ("afterInputPath", Some(r#"{"n": 42}"#)),
                ("afterParameters", Some(r#"{"n": 42}"#)),
                ("result", Some(r#"{"n": 42}"#)),
                ("afterResultPath", Some(r#"{"n": 42}"#)),
            ],
        },
        StageCase {
            case: "the member order the caller wrote survives",
            definition: r#"{"Type":"Pass","End":true}"#,
            input: r#"{"b":1,"a":2}"#,
            mock: None,
            stages: &[
                ("input", Some(r#"{"b":1,"a":2}"#)),
                ("afterInputPath", Some(r#"{"b":1,"a":2}"#)),
                ("afterResultPath", Some(r#"{"b":1,"a":2}"#)),
            ],
        },
        StageCase {
            // A number is parsed into a binary double, so an exponent form and a
            // whole number written with a fraction come back from a re-encoding as
            // another literal of the same value.
            case: "the number literal the caller wrote survives",
            definition: r#"{"Type":"Pass","End":true}"#,
            input: r#"{"n": 1e2, "m": 7.50}"#,
            mock: None,
            stages: &[
                ("input", Some(r#"{"n": 1e2, "m": 7.50}"#)),
                ("afterInputPath", Some(r#"{"n": 1e2, "m": 7.50}"#)),
                ("afterResultPath", Some(r#"{"n": 1e2, "m": 7.50}"#)),
            ],
        },
        StageCase {
            case: "a subtree the state selected is reported as the walk produced it",
            definition: r#"{"Type":"Pass","InputPath":"$.detail","End":true}"#,
            input: r#"{"detail": {"n": 42}}"#,
            mock: None,
            stages: &[
                ("input", Some(r#"{"detail": {"n": 42}}"#)),
                ("afterInputPath", Some(r#"{"n":42}"#)),
                ("afterResultPath", Some(r#"{"n":42}"#)),
            ],
        },
        StageCase {
            case: "a document a Parameters template built is reported as built",
            definition: r#"{"Type":"Task","Resource":"arn:aws:states:::lambda:invoke","Parameters":{"p.$":"$.n"},"End":true}"#,
            input: r#"{"n": 42}"#,
            mock: Some(json!({ "result": r#"{"ok":true}"# })),
            stages: &[
                ("input", Some(r#"{"n": 42}"#)),
                ("afterInputPath", Some(r#"{"n": 42}"#)),
                ("afterParameters", Some(r#"{"p":42}"#)),
                ("result", Some(r#"{"ok":true}"#)),
            ],
        },
        StageCase {
            // "Not populated when the QueryLanguage is JSONata" is why a JSONata
            // state reports no `afterInputPath` and no `afterParameters`: the
            // document it hands its work is its `Arguments`.
            case: "a JSONata state reports the Arguments it built and no path stages",
            definition: r#"{"Type":"Task","QueryLanguage":"JSONata","Resource":"arn:aws:states:::lambda:invoke","Arguments":{"p":"{% $states.input.n %}"},"End":true}"#,
            input: r#"{"n": 42}"#,
            mock: Some(json!({ "result": r#"{"ok":true}"# })),
            stages: &[
                ("input", Some(r#"{"n": 42}"#)),
                ("afterInputPath", None),
                ("afterParameters", None),
                ("afterArguments", Some(r#"{"p":42}"#)),
            ],
        },
    ];

    let emulator = Emulator::new();
    for case in &cases {
        let label = case.case;
        let mut request = traced(case.definition, case.input);
        if let Some(mock) = case.mock.clone() {
            request["mock"] = mock;
        }
        let answered = emulator.ok("TestState", &request).await;
        for (member, expected) in case.stages {
            assert_eq!(
                stage(&answered, member),
                *expected,
                "case {label:?}: expected inspectionData.{member} to carry {expected:?}, got \
                 {answered:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 2: the result as the result-side data flow receives it
// ---------------------------------------------------------------------------

/// `afterResultSelector` — "the effective result after Step Functions applies the
/// ResultSelector filter" — is reported for every state that produced a raw result:
/// the result the filter built where the state declares one, and the raw result
/// itself where no such filter applies.
///
/// Rows span the three state types the Amazon States Language gives a `ResultSelector`
/// — `Task`, `Map` and `Parallel` — each with the member and without it, and a `Pass`,
/// whose result passes through no such filter at all.
#[tokio::test]
async fn after_result_selector_reports_the_result_the_filter_received() {
    // (case, the state under test, the mocked result, the stage it must carry)
    let cases: [(&str, &str, &str, &str); 7] = [
        (
            "a Task that declares one",
            r#"{"Type":"Task","Resource":"arn:aws:states:::lambda:invoke","ResultSelector":{"total.$":"$.sum"},"End":true}"#,
            r#"{"sum":7}"#,
            r#"{"total":7}"#,
        ),
        (
            "a Task that declares none",
            r#"{"Type":"Task","Resource":"arn:aws:states:::lambda:invoke","End":true}"#,
            r#"{"sum":7}"#,
            r#"{"sum":7}"#,
        ),
        (
            "a Task whose filter builds from the context object",
            r#"{"Type":"Task","Resource":"arn:aws:states:::lambda:invoke","ResultSelector":{"sum.$":"$.sum","when.$":"$$.State.EnteredTime"},"End":true}"#,
            r#"{"sum":7}"#,
            "",
        ),
        (
            "a Map that declares one",
            r#"{"Type":"Map","ItemProcessor":{"StartAt":"P","States":{"P":{"Type":"Pass","End":true}}},"ResultSelector":{"first.$":"$[0]"},"End":true}"#,
            r#"["a","b"]"#,
            r#"{"first":"a"}"#,
        ),
        (
            "a Map that declares none",
            r#"{"Type":"Map","ItemProcessor":{"StartAt":"P","States":{"P":{"Type":"Pass","End":true}}},"End":true}"#,
            r#"["a","b"]"#,
            r#"["a","b"]"#,
        ),
        (
            "a Parallel that declares one",
            r#"{"Type":"Parallel","Branches":[{"StartAt":"P","States":{"P":{"Type":"Pass","End":true}}}],"ResultSelector":{"first.$":"$[0]"},"End":true}"#,
            r#"[{"b":1}]"#,
            r#"{"first":{"b":1}}"#,
        ),
        (
            "a Parallel that declares none",
            r#"{"Type":"Parallel","Branches":[{"StartAt":"P","States":{"P":{"Type":"Pass","End":true}}}],"End":true}"#,
            r#"[{"b":1}]"#,
            r#"[{"b":1}]"#,
        ),
    ];

    let emulator = Emulator::new();
    for (case, definition, result, expected) in cases {
        let mut request = traced(definition, "{}");
        request["mock"] = json!({ "result": result });
        let answered = emulator.ok("TestState", &request).await;
        assert_eq!(
            answered.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected the mocked state to produce its output, got {answered:?}"
        );
        let reported = stage(&answered, "afterResultSelector");
        if expected.is_empty() {
            // The one row whose filter reads a value the call itself fixes: what the
            // stage must show is that the filter RAN, not the instant it read.
            let built = reported
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .unwrap_or_else(|| panic!("case {case:?}: expected the stage in {answered:?}"));
            assert_eq!(
                built.get("sum").and_then(Value::as_i64),
                Some(7),
                "case {case:?}: expected the filter's own members, got {built:?}"
            );
            assert!(
                built.get("when").and_then(Value::as_str).is_some(),
                "case {case:?}: expected the filter's context member, got {built:?}"
            );
            continue;
        }
        assert_eq!(
            reported,
            Some(expected),
            "case {case:?}: expected inspectionData.afterResultSelector {expected:?}, got \
             {answered:?}"
        );
    }
}

/// A `Pass` state's result passes through no `ResultSelector` filter — the Amazon
/// States Language gives that type a `Result` instead — so the stage reports the raw
/// result the state produced.
#[tokio::test]
async fn a_state_with_no_result_selector_filter_reports_its_raw_result() {
    let emulator = Emulator::new();
    let answered = emulator
        .ok(
            "TestState",
            &traced(
                r#"{"Type":"Pass","Result":{"result":"hello-world"},"End":true}"#,
                r#"{"value": "hello-world"}"#,
            ),
        )
        .await;
    for (member, expected) in [
        ("result", r#"{"result":"hello-world"}"#),
        ("afterResultSelector", r#"{"result":"hello-world"}"#),
        ("afterResultPath", r#"{"result":"hello-world"}"#),
    ] {
        assert_eq!(
            stage(&answered, member),
            Some(expected),
            "expected inspectionData.{member} {expected:?}, got {answered:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 3: the handlers that answered for a failure
// ---------------------------------------------------------------------------

/// One row of the handled-failure table.
struct HandledCase {
    case: &'static str,
    /// The error handling the state under test declares, as the `Retry` and `Catch`
    /// members of one `Task`.
    handlers: &'static str,
    /// `stateConfiguration.retrierRetryCount`: the attempts already taken.
    taken: i64,
    status: &'static str,
    /// `inspectionData.errorDetails.retryIndex`, `catchIndex` and
    /// `retryBackoffIntervalSeconds`, each `None` where the answer carries none.
    retry_index: Option<i64>,
    catch_index: Option<i64>,
    backoff: Option<i64>,
}

/// `inspectionData.errorDetails` reports the POSITIONS of the handlers that answered
/// for the state's failure, and a failure nothing answered for has no such object.
///
/// Rows vary along which `Retry` entry matches the error, whether that entry still
/// owes an attempt, which `Catch` entry takes the failure over, and the combinations
/// of the two a state can declare.
#[tokio::test]
async fn a_handled_failure_reports_the_handlers_that_answered_for_it() {
    const RETRY_FIRST: &str = r#""Retry":[{"ErrorEquals":["Custom.Transient"],"IntervalSeconds":4,"MaxAttempts":1}]"#;
    const RETRY_SECOND: &str = r#""Retry":[{"ErrorEquals":["Other.Error"]},{"ErrorEquals":["Custom.Transient"],"IntervalSeconds":2,"MaxAttempts":1}]"#;
    const CATCH_FIRST: &str = r#""Catch":[{"ErrorEquals":["Custom.Transient"],"Next":"Handle"}]"#;
    const CATCH_SECOND: &str = r#""Catch":[{"ErrorEquals":["Other.Error"],"Next":"Elsewhere"},{"ErrorEquals":["Custom.Transient"],"Next":"Handle"}]"#;

    let cases = [
        HandledCase {
            case: "the first retrier still owes an attempt",
            handlers: RETRY_FIRST,
            taken: 0,
            status: "RETRIABLE",
            retry_index: Some(0),
            catch_index: None,
            backoff: Some(4),
        },
        HandledCase {
            case: "the second retrier is the one that matches",
            handlers: RETRY_SECOND,
            taken: 0,
            status: "RETRIABLE",
            retry_index: Some(1),
            catch_index: None,
            backoff: Some(2),
        },
        HandledCase {
            case: "a catcher takes over a failure no retrier was declared for",
            handlers: CATCH_FIRST,
            taken: 0,
            status: "CAUGHT_ERROR",
            retry_index: Some(0),
            catch_index: Some(0),
            backoff: None,
        },
        HandledCase {
            case: "the second catcher is the one that matches",
            handlers: CATCH_SECOND,
            taken: 0,
            status: "CAUGHT_ERROR",
            retry_index: Some(0),
            catch_index: Some(1),
            backoff: None,
        },
        HandledCase {
            case: "the first retrier is out of attempts and the catcher takes over",
            handlers: r#""Retry":[{"ErrorEquals":["Custom.Transient"],"IntervalSeconds":4,"MaxAttempts":1}],"Catch":[{"ErrorEquals":["Custom.Transient"],"Next":"Handle"}]"#,
            taken: 1,
            status: "CAUGHT_ERROR",
            retry_index: Some(0),
            catch_index: Some(0),
            backoff: None,
        },
        HandledCase {
            case: "the second retrier is out of attempts and the catcher takes over",
            handlers: r#""Retry":[{"ErrorEquals":["Other.Error"]},{"ErrorEquals":["Custom.Transient"],"IntervalSeconds":2,"MaxAttempts":1}],"Catch":[{"ErrorEquals":["Custom.Transient"],"Next":"Handle"}]"#,
            taken: 1,
            status: "CAUGHT_ERROR",
            retry_index: Some(1),
            catch_index: Some(0),
            backoff: None,
        },
        HandledCase {
            case: "a retrier out of attempts with no catcher for the error",
            handlers: RETRY_FIRST,
            taken: 1,
            status: "FAILED",
            retry_index: None,
            catch_index: None,
            backoff: None,
        },
        HandledCase {
            case: "neither a retrier nor a catcher for the error",
            handlers: r#""Retry":[{"ErrorEquals":["Other.Error"]}]"#,
            taken: 0,
            status: "FAILED",
            retry_index: None,
            catch_index: None,
            backoff: None,
        },
    ];

    let emulator = Emulator::new();
    for case in &cases {
        let definition = format!(
            r#"{{"Type":"Task","Resource":"arn:aws:states:::lambda:invoke",{},"End":true}}"#,
            case.handlers
        );
        let mut request = traced(&definition, "{}");
        request["mock"] =
            json!({ "errorOutput": { "error": MOCKED_FAILURE, "cause": MOCKED_CAUSE } });
        request["stateConfiguration"] = json!({ "retrierRetryCount": case.taken });
        let answered = emulator.ok("TestState", &request).await;
        let label = case.case;

        assert_eq!(
            answered.get("status").and_then(Value::as_str),
            Some(case.status),
            "case {label:?}: expected status {:?}, got {answered:?}",
            case.status
        );
        let details = answered
            .get("inspectionData")
            .and_then(|data| data.get("errorDetails"))
            .filter(|details| !details.is_null());
        let reported = |member: &str| -> Option<i64> {
            details.and_then(|details| details.get(member)).and_then(Value::as_i64)
        };
        assert_eq!(
            reported("retryIndex"),
            case.retry_index,
            "case {label:?}: expected errorDetails.retryIndex {:?}, got {answered:?}",
            case.retry_index
        );
        assert_eq!(
            reported("catchIndex"),
            case.catch_index,
            "case {label:?}: expected errorDetails.catchIndex {:?}, got {answered:?}",
            case.catch_index
        );
        assert_eq!(
            reported("retryBackoffIntervalSeconds"),
            case.backoff,
            "case {label:?}: expected errorDetails.retryBackoffIntervalSeconds {:?}, got \
             {answered:?}",
            case.backoff
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 4: a JSONata expression that threw
// ---------------------------------------------------------------------------

/// A JSONata expression the engine evaluated and that THREW fails the state with the
/// language's own query-evaluation error, diagnosed with the expression, the field it
/// is written in and the language's own account of the failure.
///
/// Rows vary along the field of the state the expression is written in — a `Choice`
/// rule's `Condition`, the second rule's rather than the first's, a `Task`'s
/// `Arguments` member, a `Wait`'s `Seconds` — and along the failure the language
/// raises, so both the diagnosis and the account are pinned on more than one case.
#[tokio::test]
async fn a_jsonata_expression_that_threw_fails_the_state_with_the_languages_account() {
    // (case, the state under test, the input, the field, the account)
    let cases: [(&str, &str, &str, &str, &str); 4] = [
        (
            "a Choice rule comparing a string with a number",
            r#"{"Type":"Choice","QueryLanguage":"JSONata","Choices":[{"Condition":"{% $states.input.n > 10 %}","Next":"Big"}],"Default":"Small"}"#,
            r#"{"n": "abc"}"#,
            "Choices[0]/Condition",
            "T2009: The values \"abc\" and 10 either side of operator \">\" must be of the same \
             data type",
        ),
        (
            "the second rule of a Choice that declares two",
            r#"{"Type":"Choice","QueryLanguage":"JSONata","Choices":[{"Condition":"{% $states.input.m = 1 %}","Next":"One"},{"Condition":"{% $states.input.n < 5 %}","Next":"Few"}],"Default":"Small"}"#,
            r#"{"n": "abc"}"#,
            "Choices[1]/Condition",
            "T2009: The values \"abc\" and 5 either side of operator \"<\" must be of the same \
             data type",
        ),
        (
            "a member of a Task's Arguments",
            r#"{"Type":"Task","QueryLanguage":"JSONata","Resource":"arn:aws:states:::lambda:invoke","Arguments":{"total":"{% $states.input.n + 1 %}"},"End":true}"#,
            r#"{"n": "abc"}"#,
            "Arguments/total",
            "T2001: The left side of the \"+\" operator must evaluate to a number",
        ),
        (
            "a Wait state's Seconds",
            r#"{"Type":"Wait","QueryLanguage":"JSONata","Seconds":"{% $states.input.n * 2 %}","End":true}"#,
            r#"{"n": "abc"}"#,
            "Seconds",
            "T2001: The left side of the \"*\" operator must evaluate to a number",
        ),
    ];

    let emulator = Emulator::new();
    for (case, definition, input, field, account) in cases {
        let answered = emulator.ok("TestState", &traced(definition, input)).await;
        assert_eq!(
            answered.get("status").and_then(Value::as_str),
            Some("FAILED"),
            "case {case:?}: an expression that threw fails the state, got {answered:?}"
        );
        assert_eq!(
            answered.get("error").and_then(Value::as_str),
            Some("States.QueryEvaluationError"),
            "case {case:?}: expected the language's query-evaluation error, got {answered:?}"
        );
        let expression = serde_json::from_str::<Value>(definition)
            .ok()
            .and_then(|state| expression_at(&state, field))
            .unwrap_or_else(|| panic!("case {case:?}: the row names a field of its own state"));
        let expected = format!(
            "The JSONata expression '{expression}' specified for the field '{field}' threw an \
             error during evaluation. {account}"
        );
        assert_eq!(
            answered.get("cause").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {case:?}: expected the cause {expected:?}, got {answered:?}"
        );
    }
}

/// The expression the state `state` writes at the definition field `field`, without
/// the delimiters the language wraps one in — which is how a diagnosis quotes it.
///
/// The field is read out of the ROW's own definition rather than written twice, so the
/// expression a diagnosis must quote is the one the state carries.
fn expression_at(state: &Value, field: &str) -> Option<String> {
    let mut node = state;
    for segment in field.split('/') {
        node = match segment.split_once('[') {
            Some((member, index)) => {
                let index: usize = index.trim_end_matches(']').parse().ok()?;
                node.get(member)?.get(index)?
            }
            None => node.get(segment)?,
        };
    }
    let text = node.as_str()?.trim();
    Some(
        text.strip_prefix("{%")?
            .strip_suffix("%}")?
            .trim()
            .to_string(),
    )
}

// ---------------------------------------------------------------------------
// Rule 5: the state types it does not test
// ---------------------------------------------------------------------------

/// The state types this operation does not test are refused in its own sentences: a
/// `mock` for a type that takes none names that type, and a `Map` or `Parallel` with
/// no mock standing in for its own work is told which types the API does test.
///
/// Rows span every state type a `mock` may not be specified for, both types that need
/// one, and — as the counter-case — the three types a mock IS specified for, which are
/// tested rather than refused.
#[tokio::test]
async fn the_state_types_test_state_does_not_test_are_refused_in_its_own_words() {
    const SUPPORTED_WITHOUT_A_MOCK: &str =
        "TestState API does not support Map or Parallel states. Supported state types include: \
         [Task, Wait, Pass, Succeed, Fail, Choice]";
    const MAP: &str = r#"{"Type":"Map","ItemProcessor":{"StartAt":"P","States":{"P":{"Type":"Pass","End":true}}},"End":true}"#;
    const PARALLEL: &str = r#"{"Type":"Parallel","Branches":[{"StartAt":"P","States":{"P":{"Type":"Pass","End":true}}}],"End":true}"#;
    const TASK: &str = r#"{"Type":"Task","Resource":"arn:aws:states:::lambda:invoke","End":true}"#;

    // (case, the state under test, whether a mock is sent, the error, the message)
    let cases: [(&str, &str, bool, &str, &str); 9] = [
        (
            "a mock for a Choice state",
            r#"{"Type":"Choice","Choices":[{"Variable":"$.n","NumericEquals":1,"Next":"P"}],"Default":"D"}"#,
            true,
            "ValidationException",
            "State type 'Choice' is not supported when a mock is specified",
        ),
        (
            "a mock for a Pass state",
            r#"{"Type":"Pass","End":true}"#,
            true,
            "ValidationException",
            "State type 'Pass' is not supported when a mock is specified",
        ),
        (
            "a mock for a Wait state",
            r#"{"Type":"Wait","Seconds":1,"End":true}"#,
            true,
            "ValidationException",
            "State type 'Wait' is not supported when a mock is specified",
        ),
        (
            "a mock for a Succeed state",
            r#"{"Type":"Succeed"}"#,
            true,
            "ValidationException",
            "State type 'Succeed' is not supported when a mock is specified",
        ),
        (
            "a mock for a Fail state",
            r#"{"Type":"Fail","Error":"Boom"}"#,
            true,
            "ValidationException",
            "State type 'Fail' is not supported when a mock is specified",
        ),
        (
            "a Map state with no mock",
            MAP,
            false,
            "InvalidDefinition",
            SUPPORTED_WITHOUT_A_MOCK,
        ),
        (
            "a Parallel state with no mock",
            PARALLEL,
            false,
            "InvalidDefinition",
            SUPPORTED_WITHOUT_A_MOCK,
        ),
        // The two types a mock stands in for ARE tested, so the refusal above is
        // about the missing mock and not about the type.
        ("a Map state with a mock", MAP, true, "", ""),
        ("a Task state with a mock", TASK, true, "", ""),
    ];

    let emulator = Emulator::new();
    for (case, definition, mocked, expected, message) in cases {
        let mut request = traced(definition, "{}");
        if mocked {
            request["mock"] = json!({ "result": "{}" });
        }
        let (status, body) = emulator.call("TestState", &request).await;
        if expected.is_empty() {
            assert_eq!(
                status, 200,
                "case {case:?}: this state is tested rather than refused, got {status} {body:?}"
            );
            continue;
        }
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, expected),
            "case {case:?}: expected a 400 {expected}, got {status} {body:?}"
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(message),
            "case {case:?}: expected the message {message:?}, got {body:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 6: the order an advertised enum value set is listed in
// ---------------------------------------------------------------------------

/// A value outside a modelled enum is refused with the validation envelope, whose
/// value set is listed in the order THIS SERVICE states that enum's values in — not
/// the order the model declares its members in.
///
/// Rows vary along the rejected value (a name of its own, one differing from a
/// permitted value only in case, and the empty string) and along the ENUM:
/// `InspectionLevel`, `ExecutionStatus` and `StateMachineType`, whose stated orders this
/// service's refusals fix, and `ExecutionRedriveFilter`, which it states no order for
/// and which is therefore listed as the model declares it.
#[tokio::test]
async fn an_advertised_enum_value_set_is_listed_in_the_order_the_service_states_it() {
    // RECORDED for `InspectionLevel`; the model declares it `INFO | DEBUG | TRACE`
    // (`smithy_query(query="shape",shape="InspectionLevel")`).
    const INSPECTION_LEVELS: &str = "[TRACE, INFO, DEBUG]";
    // RECORDED for `ExecutionStatus`; the model declares it `RUNNING | SUCCEEDED |
    // FAILED | TIMED_OUT | ABORTED | PENDING_REDRIVE`
    // (`smithy_query(query="shape",shape="ExecutionStatus")`).
    const EXECUTION_STATUSES: &str = "[SUCCEEDED, TIMED_OUT, PENDING_REDRIVE, ABORTED, FAILED, RUNNING]";
    // RECORDED for `StateMachineType`; the model declares it the other way round
    // (`smithy_query(query="shape",shape="StateMachineType")` -> `STANDARD | EXPRESS`).
    const MACHINE_TYPES: &str = "[EXPRESS, STANDARD]";
    // `ExecutionRedriveFilter`, likewise as the model declares it
    // (`smithy_query(query="shape",shape="ExecutionRedriveFilter")` -> `REDRIVEN |
    // NOT_REDRIVEN`).
    const REDRIVE_FILTERS: &str = "[REDRIVEN, NOT_REDRIVEN]";
    const DEFINITION: &str = r#"{"Type":"Pass","End":true}"#;
    let machine = format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:listed-enum-order");

    // (case, the operation, the request, the member, the value, the set it lists)
    let cases: [(&str, &str, Value, &str, &str, &str); 7] = [
        (
            "an inspectionLevel of its own naming",
            "TestState",
            json!({ "definition": DEFINITION, "inspectionLevel": "NOT_A_REAL_LEVEL" }),
            "inspectionLevel",
            "NOT_A_REAL_LEVEL",
            INSPECTION_LEVELS,
        ),
        (
            "an inspectionLevel differing from a permitted value only in case",
            "TestState",
            json!({ "definition": DEFINITION, "inspectionLevel": "trace" }),
            "inspectionLevel",
            "trace",
            INSPECTION_LEVELS,
        ),
        (
            "an inspectionLevel that is the empty string",
            "TestState",
            json!({ "definition": DEFINITION, "inspectionLevel": "" }),
            "inspectionLevel",
            "",
            INSPECTION_LEVELS,
        ),
        (
            "a statusFilter of its own naming",
            "ListExecutions",
            json!({ "stateMachineArn": machine.clone(), "statusFilter": "NOT_A_STATUS" }),
            "statusFilter",
            "NOT_A_STATUS",
            EXECUTION_STATUSES,
        ),
        (
            "a statusFilter differing from a permitted value only in case",
            "ListExecutions",
            json!({ "stateMachineArn": machine.clone(), "statusFilter": "succeeded" }),
            "statusFilter",
            "succeeded",
            EXECUTION_STATUSES,
        ),
        (
            "another enum of the same operation, which this service states no order for",
            "ListExecutions",
            json!({ "stateMachineArn": machine.clone(), "redriveFilter": "NOT_A_FILTER" }),
            "redriveFilter",
            "NOT_A_FILTER",
            REDRIVE_FILTERS,
        ),
        (
            "an enum this service states its own order for, on another operation",
            "CreateStateMachine",
            json!({
                "name": "listed-enum-order",
                "definition": r#"{"StartAt":"P","States":{"P":{"Type":"Pass","End":true}}}"#,
                "roleArn": ROLE_ARN,
                "type": "SUPER_EXPRESS",
            }),
            "type",
            "SUPER_EXPRESS",
            MACHINE_TYPES,
        ),
    ];

    let emulator = Emulator::new();
    for (case, operation, request, member, value, listed) in cases {
        let (status, body) = emulator.call(operation, &request).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected a 400 ValidationException, got {status} {body:?}"
        );
        let expected = format!(
            "1 validation error detected: Value '{value}' at '{member}' failed to satisfy \
             constraint: Member must satisfy enum value set: {listed}"
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {case:?}: expected the message {expected:?}, got {body:?}"
        );
    }
}
