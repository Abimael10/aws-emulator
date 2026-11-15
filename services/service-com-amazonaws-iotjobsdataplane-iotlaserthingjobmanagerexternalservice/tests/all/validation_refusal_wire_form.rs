// SPDX-License-Identifier: CC0-1.0

//! How this plane WORDS and NAMES a request it refuses before anything is stored.
//!
//! Two rules are under test, each over a table of requests rather than over one:
//!
//! 1. A refusal the request-validation layer raises in FRONT of the service — a value
//!    outside a modelled constraint, a required member the request never sent — is answered
//!    400 in the counted envelope that layer words (`N validation error(s) detected: …`),
//!    under the client-input error shape the OPERATION declares rather than the generic one
//!    the framework's boundary renderer names, in the `x-amzn-errortype` header a restJson1
//!    client resolves the shape from AND in the body's own discriminator. A value outside an
//!    ENUMERATED type additionally lists that type's admissible values, in the order the
//!    layer's own table of them is walked (ascending bucket of each value's string hash) and
//!    NOT in the order the model declares them in — so the listing belongs to the enumerated
//!    type and is the same wherever that type is carried.
//!
//! 2. A refusal one of this plane's HANDLERS decides keeps the shape that handler returned.
//!    A `stepTimeoutInMinutes` the member does not admit is one such refusal, and the
//!    sentence it states names the member and the rejected value and no bound — the same
//!    sentence from both operations that accept the member, whatever each of them admits.
//!
//! Every request goes over the wire (restJson1 through the multi-service router), so what is
//! asserted is the status, the body and the header a device agent or an SDK actually reads.

use serde_json::{json, Value};
use service_common::validation_message::hashed_enum_order;
use service_com_amazonaws_iotjobsdataplane_iotlaserthingjobmanagerexternalservice::ops::types::JOB_EXECUTION_STATUS_VALUES;

use crate::support::{Answered, Emulator, COMMAND_ARN};

/// The client-input error the four device-facing operations declare
/// (`smithy_query(query="digest")`: `UpdateJobExecution`, `DescribeJobExecution`,
/// `GetPendingJobExecutions` and `StartNextPendingJobExecution` each declare
/// `InvalidRequestException`, `@httpError 400`), and therefore the shape a refusal of one of
/// them is resolved by — whether the boundary raised it or the handler decided it.
const INVALID_REQUEST: &str = "InvalidRequestException";

/// The shape `StartCommandExecution` declares for the same class
/// (`smithy_query(query="digest")`: `ValidationException`, `@httpError 400`), which is also
/// the shape the framework's boundary renderer names — so that operation's boundary refusals
/// are already resolved by the shape it models.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The opening of the counted envelope the validation layer words one finding in.
const ONE_FINDING: &str = "1 validation error detected: ";

/// The order a refusal lists `JobExecutionStatus`'s admissible values in.
///
/// The values are the model's (`smithy_query(query="shape", shape="JobExecutionStatus")`:
/// QUEUED, IN_PROGRESS, SUCCEEDED, FAILED, TIMED_OUT, REJECTED, REMOVED, CANCELED) and the
/// ORDER is the one the validation layer's own table of them is walked in, which is neither
/// that declaration order nor a sort of the values. Written out here so the wire text is
/// pinned independently of the code that derives it, and cross-checked against that
/// derivation by [`the_listed_order_is_the_derivation_of_the_declared_values`].
const STATED_STATUS_VALUE_SET: &str =
    "REMOVED, SUCCEEDED, TIMED_OUT, IN_PROGRESS, QUEUED, CANCELED, FAILED, REJECTED";

/// The whole message a refusal of `value` at the `status` member states.
fn enum_refusal_message(value: &str) -> String {
    format!(
        "{ONE_FINDING}Value '{value}' at 'status' failed to satisfy constraint: \
         Member must satisfy enum value set: [{STATED_STATUS_VALUE_SET}]"
    )
}

/// The `__type` a refusal's body names, or `""` when it names none.
fn shape_of(answered: &Answered) -> &str {
    answered.body["__type"].as_str().unwrap_or("")
}

/// The `message` a refusal's body states, or `""` when it states none.
fn message_of(answered: &Answered) -> &str {
    answered.body["message"].as_str().unwrap_or("")
}

/// RULE: the order a refusal lists an enumerated type's values in is derived from the values
/// themselves — the ascending bucket of each value's own string hash — so the text pinned in
/// [`STATED_STATUS_VALUE_SET`] is not a list someone chose and a model revision that
/// reorders the type's declaration does not move it.
#[test]
fn the_listed_order_is_the_derivation_of_the_declared_values() {
    let derived = hashed_enum_order(JOB_EXECUTION_STATUS_VALUES).join(", ");
    assert_eq!(
        derived, STATED_STATUS_VALUE_SET,
        "the listed order must be the derivation over {JOB_EXECUTION_STATUS_VALUES:?}, got {derived:?}"
    );
}

/// RULE: a `status` outside `JobExecutionStatus` is refused 400 by the validation layer, in
/// its counted envelope, quoting the value the request carried and listing the type's
/// admissible values in the order that layer lists them — under the shape
/// `UpdateJobExecution` declares for a refused request, in the body and the header alike.
///
/// The rows span what the rule generalises over: the FORM of the rejected value (an unknown
/// token, a legal value in the wrong case, one with the model's own separator, a single
/// character, the empty string) and whether the named execution exists at all, since the
/// layer answers before any handler looks one up.
#[tokio::test]
async fn a_status_outside_the_enum_is_refused_with_the_listed_value_set() {
    let emulator = Emulator::new();
    let thing = "laser-enum-refusal";
    let job_id = emulator.start_command_on(thing, None).await;

    for (case, rejected) in [
        ("an unknown token", "BOGUS_STATUS"),
        ("a legal value lower-cased", "in_progress"),
        ("a legal value mis-spelled", "SUCCEDED"),
        ("a value carrying the separator", "_"),
        ("a single character", "Q"),
        ("the empty string", ""),
    ] {
        for (where_case, named) in [
            ("an execution that exists", job_id.as_str()),
            ("an execution that does not", "no-such-job"),
        ] {
            let answered = emulator
                .answer(
                    "POST",
                    &format!("/things/{thing}/jobs/{named}"),
                    Some(json!({"status": rejected})),
                )
                .await;
            assert_eq!(
                answered.status, 400,
                "case {case:?} / {where_case:?}: expected 400, got {} {}",
                answered.status, answered.body
            );
            assert_eq!(
                shape_of(&answered), INVALID_REQUEST,
                "case {case:?} / {where_case:?}: expected the body to name {INVALID_REQUEST:?}, got {}",
                answered.body
            );
            assert_eq!(
                message_of(&answered),
                enum_refusal_message(rejected),
                "case {case:?} / {where_case:?}: unexpected refusal text, got {}",
                answered.body
            );
            assert_eq!(
                answered.error_type.as_deref(),
                Some(INVALID_REQUEST),
                "case {case:?} / {where_case:?}: expected the header {INVALID_REQUEST:?}, got {:?}",
                answered.error_type
            );
        }
    }

    // Nothing was written: the execution the rows named is still queued at the version its
    // creation gave it.
    let stored = emulator.stored_execution(thing, &job_id);
    assert_eq!(
        (stored.status.as_str(), stored.version_number),
        ("QUEUED", 1),
        "a refused update must leave the execution exactly as it was, got {stored:?}"
    );
}

/// One request a constraint of this model refuses: the member path the finding names, and
/// the client-input error shape the operation it addresses declares.
struct Refused {
    case: &'static str,
    method: &'static str,
    path: String,
    body: Option<Value>,
    member: &'static str,
    shape: &'static str,
}

/// RULE: EVERY refusal the validation layer raises — whatever constraint class, whatever
/// operation, and whether the offending value arrived in the body or in the URI — is
/// answered 400 in that layer's counted envelope, under the client-input error shape the
/// operation it addresses declares.
///
/// The rows span the constraint classes this model declares (a required member the request
/// never sent, a string past its `@length` maximum, a map below its `@length` minimum, a
/// value outside its `@pattern`, a number below its `@range` minimum) across four of the
/// five operations, both bindings a value arrives in, and BOTH shapes this model declares
/// for the class — so the rule is the operation's own shape and not one name written once.
#[tokio::test]
async fn every_constraint_class_names_the_shape_its_operation_declares() {
    let emulator = Emulator::new();
    let thing = "laser-constraint-classes";
    let job_id = emulator.start_command_on(thing, None).await;
    // `JobId` is `length 1..64` and `ThingName` `length 1..128`
    // (`smithy_query(query="operation", operation="UpdateJobExecution")`), so one character
    // past each maximum is outside the member.
    let long_job_id = "j".repeat(65);
    let long_thing = "t".repeat(129);

    for refused in [
        Refused {
            case: "a required member the request never sent",
            method: "POST",
            path: format!("/things/{thing}/jobs/{job_id}"),
            body: Some(json!({})),
            member: "status",
            shape: INVALID_REQUEST,
        },
        Refused {
            case: "a URI label past its length maximum",
            method: "POST",
            path: format!("/things/{thing}/jobs/{long_job_id}"),
            body: Some(json!({"status": "IN_PROGRESS"})),
            member: "jobId",
            shape: INVALID_REQUEST,
        },
        Refused {
            case: "a URI label past its length maximum on a read",
            method: "GET",
            path: format!("/things/{long_thing}/jobs"),
            body: None,
            member: "thingName",
            shape: INVALID_REQUEST,
        },
        Refused {
            case: "a URI label past its length maximum on a start",
            method: "PUT",
            path: format!("/things/{long_thing}/jobs/$next"),
            body: Some(json!({})),
            member: "thingName",
            shape: INVALID_REQUEST,
        },
        Refused {
            case: "a map below its length minimum",
            method: "POST",
            path: "/command-executions".to_string(),
            body: Some(json!({
                "targetArn": format!("arn:aws:iot:us-east-1:111122223333:thing/{thing}"),
                "commandArn": COMMAND_ARN,
                "parameters": {},
            })),
            member: "parameters",
            shape: VALIDATION_EXCEPTION,
        },
        Refused {
            case: "a value outside its pattern",
            method: "POST",
            path: "/command-executions".to_string(),
            body: Some(json!({
                "targetArn": format!("arn:aws:iot:us-east-1:111122223333:thing/{thing}"),
                "commandArn": COMMAND_ARN,
                "clientToken": "a token with spaces",
            })),
            member: "clientToken",
            shape: VALIDATION_EXCEPTION,
        },
        Refused {
            case: "a number below its range minimum",
            method: "POST",
            path: "/command-executions".to_string(),
            body: Some(json!({
                "targetArn": format!("arn:aws:iot:us-east-1:111122223333:thing/{thing}"),
                "commandArn": COMMAND_ARN,
                "executionTimeoutSeconds": 0,
            })),
            member: "executionTimeoutSeconds",
            shape: VALIDATION_EXCEPTION,
        },
    ] {
        let case = refused.case;
        let answered = emulator
            .answer(refused.method, &refused.path, refused.body)
            .await;
        assert_eq!(
            answered.status, 400,
            "case {case:?}: expected 400, got {} {}",
            answered.status, answered.body
        );
        assert_eq!(
            shape_of(&answered), refused.shape,
            "case {case:?}: expected the body to name {:?}, got {}",
            refused.shape, answered.body
        );
        let message = message_of(&answered);
        assert!(
            message.starts_with(ONE_FINDING),
            "case {case:?}: expected the counted envelope, got {message:?}"
        );
        assert!(
            message.contains(&format!("at '{}'", refused.member)),
            "case {case:?}: expected the finding to name the member {:?}, got {message:?}",
            refused.member
        );
        assert_eq!(
            answered.error_type.as_deref(),
            Some(refused.shape),
            "case {case:?}: expected the header {:?}, got {:?}",
            refused.shape, answered.error_type
        );
    }
}

/// The sentence both operations state for a `stepTimeoutInMinutes` their member does not
/// admit, written out so the wire text is pinned independently of the code that builds it.
fn step_timeout_refusal(minutes: i64) -> String {
    format!("Provide valid timeout value, stepTimeoutInMinutes cannot be {minutes}.")
}

/// RULE: a refusal one of this plane's HANDLERS decides is answered under the shape that
/// handler returned — the same `InvalidRequestException` a boundary refusal of the operation
/// is resolved by, so a client sees one shape for a refused request however the refusal was
/// reached. For a `stepTimeoutInMinutes` outside what the member admits, the sentence names
/// the member and the rejected value and no bound, so both operations that accept the member
/// state a refused value alike.
///
/// The rows span the magnitudes the rule generalises over — below the range, past it, the
/// extremes a `long` admits — and the cancelling value, which only `UpdateJobExecution`'s
/// member documents and which `StartNextPendingJobExecution` therefore refuses with the same
/// sentence.
#[tokio::test]
async fn a_handler_refusal_states_the_rejected_value_under_the_modelled_shape() {
    let emulator = Emulator::new();

    for (index, (case, minutes, refused_by_the_update)) in [
        ("below the range", 0_i64, true),
        ("two below the range", -2, true),
        ("one past the range", 10_081, true),
        ("the smallest long", i64::MIN, true),
        ("the largest long", i64::MAX, true),
        ("the cancelling value", -1, false),
    ]
    .into_iter()
    .enumerate()
    {
        let thing = format!("laser-timeout-wire-{index}");
        let job_id = emulator.start_command_on(&thing, None).await;

        // `StartNextPendingJobExecution`, whose member documents the range alone.
        let answered = emulator
            .answer(
                "PUT",
                &format!("/things/{thing}/jobs/$next"),
                Some(json!({"stepTimeoutInMinutes": minutes})),
            )
            .await;
        assert_eq!(
            answered.status, 400,
            "case {case:?} ({minutes}): expected 400 from $next, got {} {}",
            answered.status, answered.body
        );
        assert_eq!(
            shape_of(&answered), INVALID_REQUEST,
            "case {case:?} ({minutes}): expected the body to name {INVALID_REQUEST:?}, got {}",
            answered.body
        );
        assert_eq!(
            message_of(&answered),
            step_timeout_refusal(minutes),
            "case {case:?} ({minutes}): unexpected refusal text from $next, got {}",
            answered.body
        );
        assert_eq!(
            answered.error_type.as_deref(),
            Some(INVALID_REQUEST),
            "case {case:?} ({minutes}): expected {INVALID_REQUEST:?} in the header, got {:?}",
            answered.error_type
        );

        // `UpdateJobExecution`, whose member additionally documents the cancelling value.
        let answered = emulator
            .answer(
                "POST",
                &format!("/things/{thing}/jobs/{job_id}"),
                Some(json!({"status": "IN_PROGRESS", "stepTimeoutInMinutes": minutes})),
            )
            .await;
        if !refused_by_the_update {
            assert_eq!(
                answered.status, 200,
                "case {case:?} ({minutes}): the update's member admits this value, got {} {}",
                answered.status, answered.body
            );
            continue;
        }
        assert_eq!(
            answered.status, 400,
            "case {case:?} ({minutes}): expected 400 from the update, got {} {}",
            answered.status, answered.body
        );
        assert_eq!(
            message_of(&answered),
            step_timeout_refusal(minutes),
            "case {case:?} ({minutes}): unexpected refusal text from the update, got {}",
            answered.body
        );
        assert_eq!(
            answered.error_type.as_deref(),
            Some(INVALID_REQUEST),
            "case {case:?} ({minutes}): expected {INVALID_REQUEST:?} in the header, got {:?}",
            answered.error_type
        );
    }
}

/// RULE (the other side of the one above): a failure a handler decides from the STORE keeps
/// the shape that handler returned, whatever status it is answered under — so the
/// restatement above touches the boundary's generic shape and nothing else.
///
/// The rows span the refusals a handler decides from the store rather than from the request:
/// an execution that does not exist (404) and one a device already finished (409).
#[tokio::test]
async fn a_modelled_shape_this_plane_answers_from_the_store_is_left_alone() {
    let emulator = Emulator::new();
    let thing = "laser-bare-shapes";
    let job_id = emulator.start_command_on(thing, None).await;
    emulator.with_stored_execution(thing, &job_id, |record| {
        record.status = "SUCCEEDED".to_string();
    });

    for (case, path, expected_status, expected_shape) in [
        (
            "an execution that does not exist",
            format!("/things/{thing}/jobs/no-such-job"),
            404,
            "ResourceNotFoundException",
        ),
        (
            "an execution a device already finished",
            format!("/things/{thing}/jobs/{job_id}"),
            409,
            "InvalidStateTransitionException",
        ),
    ] {
        let answered = emulator
            .answer("POST", &path, Some(json!({"status": "IN_PROGRESS"})))
            .await;
        assert_eq!(
            answered.status, expected_status,
            "case {case:?}: expected {expected_status}, got {} {}",
            answered.status, answered.body
        );
        assert_eq!(
            shape_of(&answered), expected_shape,
            "case {case:?}: expected the body to name {expected_shape:?}, got {}",
            answered.body
        );
        assert_eq!(
            answered.error_type.as_deref(),
            Some(expected_shape),
            "case {case:?}: expected {expected_shape:?} in the header, got {:?}",
            answered.error_type
        );
    }
}
