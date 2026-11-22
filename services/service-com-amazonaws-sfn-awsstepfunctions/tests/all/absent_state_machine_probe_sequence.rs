// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a client PROBING a state machine the store no longer
//! holds observes, driven over this service's own wire protocol (`awsJson1_0`,
//! `POST /` with `X-Amz-Target: AWSStepFunctions.<Operation>`) through the
//! production `MultiServiceRouter`, so what is asserted is what a signed AWS SDK
//! request observes.
//!
//! RULE: an operation addressed to a state machine the store holds no record of —
//! or to a version, an alias or an execution that went with it — answers from that
//! absence: the operation's OWN modelled client error where its model declares one,
//! and the documented empty or already-satisfied success where it does not. The
//! answer is a property of the store, so it does not depend on HOW the record came
//! to be absent, it does not change when the same probe is repeated (a probe is a
//! read of an absent record and writes nothing), and it leaves every other state
//! machine answering exactly as before.
//!
//! The rows differ along what the rule generalises over: the operation probed (a
//! read, a mutation, a start, a listing, a tag write, a delete), which resource of
//! the gone state machine the request addresses (the state machine, its version, its
//! alias, its execution), whether the operation models an absent-resource error at
//! all, and how the record came to be absent — deleted after it had carried a
//! version, an alias, a tag and a run, or never created in the first place.
//!
//! The harness (`Emulator`, `create_machine`, `error_code`, `string`) is the one
//! `state_machine_lifecycle` builds and the wait for the deletion to go through is
//! `error_messages`' own; `tests/all/main.rs` is this binary's crate root, so both
//! are reached through `crate::<suite>`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::service::wire::CLIENT_ERROR_STATUS;
use service_com_amazonaws_sfn_awsstepfunctions::state::{
    resource_error_message, resource_not_found_message, EXECUTION_DOES_NOT_EXIST,
    ROUTING_CONFIGURATION_VERSION_NOT_FOUND, STATE_MACHINE_DOES_NOT_EXIST,
    STATE_MACHINE_VERSION_DOES_NOT_EXIST,
};

use crate::error_messages::poll_until_absent;
use crate::state_machine_lifecycle::{
    create_machine, error_code, string, Emulator, ACCOUNT, HELLO_DEFINITION, REGION,
};

/// The four ARNs one state machine's probes are addressed to: the state machine
/// itself and the three resources that belong to it.
struct Absent {
    machine: String,
    version: String,
    alias: String,
    execution: String,
}

/// One probe: the case it stands for, the call it makes, and the refusal it must be
/// answered with.
///
/// The refusal carries no status of its own: every modelled CLIENT error of this
/// service is answered with [`CLIENT_ERROR_STATUS`] whatever its shape's
/// `@httpError` declares, which `client_refusal_status` owns.
struct Refused {
    case: &'static str,
    operation: &'static str,
    request: Value,
    code: &'static str,
    /// Asserted when the shape's message reports a value of the request; `None`
    /// where the wording belongs to another suite.
    message: Option<String>,
}

/// One probe whose operation models no absent-resource error, and so is SERVED.
struct Served {
    case: &'static str,
    operation: &'static str,
    request: Value,
    /// The list member that must come back EMPTY, where the operation reports one.
    empty_member: Option<&'static str>,
}

/// Create a state machine that carries a published version, an alias over it, a tag
/// and a finished execution, delete it, and wait for the RECORD to go.
async fn deleted_state_machine(emulator: &Emulator, name: &str) -> Absent {
    let machine = create_machine(emulator, name, HELLO_DEFINITION).await;
    let version = string(
        &emulator
            .ok(
                "PublishStateMachineVersion",
                &json!({ "stateMachineArn": &machine }),
            )
            .await,
        "stateMachineVersionArn",
    );
    let alias = string(
        &emulator
            .ok(
                "CreateStateMachineAlias",
                &json!({
                    "name": "live",
                    "routingConfiguration": [
                        { "stateMachineVersionArn": &version, "weight": 100 },
                    ],
                }),
            )
            .await,
        "stateMachineAliasArn",
    );
    emulator
        .ok(
            "TagResource",
            &json!({
                "resourceArn": &machine,
                "tags": [{ "key": "team", "value": "platform" }],
            }),
        )
        .await;
    let execution = string(
        &emulator
            .ok(
                "StartExecution",
                &json!({
                    "stateMachineArn": &machine,
                    "name": format!("{name}-run"),
                    "input": "{}",
                }),
            )
            .await,
        "executionArn",
    );
    emulator
        .ok("DeleteStateMachine", &json!({ "stateMachineArn": &machine }))
        .await;
    // The deletion is owed to the clock, so the probes below wait for the CONDITION
    // — this service answering for nothing under the ARN — and never for a span.
    poll_until_absent(emulator, &machine).await;
    Absent {
        machine,
        version,
        alias,
        execution,
    }
}

/// The same four ARNs for a state machine `name` that was never created: the absence
/// the probes are about is the same absence, reached without a delete.
fn never_created(name: &str) -> Absent {
    let machine = format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:{name}");
    Absent {
        version: format!("{machine}:1"),
        alias: format!("{machine}:live"),
        execution: format!("arn:aws:states:{REGION}:{ACCOUNT}:execution:{name}:{name}-run"),
        machine,
    }
}

/// Every probe whose operation models an error for the resource it could not find,
/// over the four ARNs of one absent state machine.
fn refused_probes(absent: &Absent) -> Vec<Refused> {
    let Absent {
        machine,
        version,
        alias,
        execution,
    } = absent;
    let gone_machine = resource_error_message(STATE_MACHINE_DOES_NOT_EXIST, machine);
    let gone_version = resource_error_message(STATE_MACHINE_VERSION_DOES_NOT_EXIST, version);
    let gone_execution = resource_error_message(EXECUTION_DOES_NOT_EXIST, execution);
    vec![
        Refused {
            case: "a read of the state machine",
            operation: "DescribeStateMachine",
            request: json!({ "stateMachineArn": machine }),
            code: "StateMachineDoesNotExist",
            message: Some(gone_machine.clone()),
        },
        Refused {
            // A metadata-only read reports no definition, so it decrypts nothing —
            // and still has no record to report.
            case: "a metadata-only read of the state machine",
            operation: "DescribeStateMachine",
            request: json!({ "stateMachineArn": machine, "includedData": "METADATA_ONLY" }),
            code: "StateMachineDoesNotExist",
            message: Some(gone_machine.clone()),
        },
        Refused {
            // A version-qualified ARN is about the VERSION it names, so the
            // condition stated is the version's.
            case: "a read of one of its versions",
            operation: "DescribeStateMachine",
            request: json!({ "stateMachineArn": version }),
            code: "StateMachineDoesNotExist",
            message: Some(gone_version),
        },
        Refused {
            case: "an update of its configuration",
            operation: "UpdateStateMachine",
            request: json!({ "stateMachineArn": machine, "definition": HELLO_DEFINITION }),
            code: "StateMachineDoesNotExist",
            message: Some(gone_machine.clone()),
        },
        Refused {
            case: "an asynchronous start",
            operation: "StartExecution",
            request: json!({ "stateMachineArn": machine, "input": "{}" }),
            code: "StateMachineDoesNotExist",
            message: Some(gone_machine.clone()),
        },
        Refused {
            case: "a synchronous start",
            operation: "StartSyncExecution",
            request: json!({ "stateMachineArn": machine, "input": "{}" }),
            code: "StateMachineDoesNotExist",
            message: Some(gone_machine.clone()),
        },
        Refused {
            case: "a listing of its executions",
            operation: "ListExecutions",
            request: json!({ "stateMachineArn": machine }),
            code: "StateMachineDoesNotExist",
            message: Some(gone_machine.clone()),
        },
        Refused {
            case: "a publish of another version",
            operation: "PublishStateMachineVersion",
            request: json!({ "stateMachineArn": machine }),
            code: "StateMachineDoesNotExist",
            message: Some(gone_machine.clone()),
        },
        Refused {
            case: "a listing of its aliases",
            operation: "ListStateMachineAliases",
            request: json!({ "stateMachineArn": machine }),
            code: "StateMachineDoesNotExist",
            message: Some(gone_machine),
        },
        Refused {
            // An alias is a referenced resource of its own, which the tagging
            // family's `ResourceNotFound` reports.
            case: "a read of one of its aliases",
            operation: "DescribeStateMachineAlias",
            request: json!({ "stateMachineAliasArn": alias }),
            code: "ResourceNotFound",
            message: None,
        },
        Refused {
            case: "a repoint of one of its aliases",
            operation: "UpdateStateMachineAlias",
            request: json!({ "stateMachineAliasArn": alias, "description": "probe" }),
            code: "ResourceNotFound",
            message: None,
        },
        Refused {
            // The rule enforced is the routing configuration's own — every entry
            // must reference a version that already exists — so the refusal states
            // that rule.
            case: "a create of an alias over one of its versions",
            operation: "CreateStateMachineAlias",
            request: json!({
                "name": "probe",
                "routingConfiguration": [
                    { "stateMachineVersionArn": version, "weight": 100 },
                ],
            }),
            code: "ResourceNotFound",
            message: Some(ROUTING_CONFIGURATION_VERSION_NOT_FOUND.to_string()),
        },
        Refused {
            case: "a tag write on the state machine",
            operation: "TagResource",
            request: json!({
                "resourceArn": machine,
                "tags": [{ "key": "probe", "value": "1" }],
            }),
            code: "ResourceNotFound",
            message: Some(resource_not_found_message(machine)),
        },
        Refused {
            case: "a tag removal on the state machine",
            operation: "UntagResource",
            request: json!({ "resourceArn": machine, "tagKeys": ["team"] }),
            code: "ResourceNotFound",
            message: Some(resource_not_found_message(machine)),
        },
        Refused {
            case: "a read of one of its executions",
            operation: "DescribeExecution",
            request: json!({ "executionArn": execution }),
            code: "ExecutionDoesNotExist",
            message: Some(gone_execution.clone()),
        },
        Refused {
            case: "a read of the configuration an execution ran",
            operation: "DescribeStateMachineForExecution",
            request: json!({ "executionArn": execution }),
            code: "ExecutionDoesNotExist",
            message: Some(gone_execution.clone()),
        },
        Refused {
            case: "a read of an execution's history",
            operation: "GetExecutionHistory",
            request: json!({ "executionArn": execution }),
            code: "ExecutionDoesNotExist",
            message: Some(gone_execution.clone()),
        },
        Refused {
            case: "a listing of an execution's map runs",
            operation: "ListMapRuns",
            request: json!({ "executionArn": execution }),
            code: "ExecutionDoesNotExist",
            message: Some(gone_execution.clone()),
        },
        Refused {
            case: "a stop of one of its executions",
            operation: "StopExecution",
            request: json!({ "executionArn": execution }),
            code: "ExecutionDoesNotExist",
            message: Some(gone_execution.clone()),
        },
        Refused {
            case: "a redrive of one of its executions",
            operation: "RedriveExecution",
            request: json!({ "executionArn": execution }),
            code: "ExecutionDoesNotExist",
            message: Some(gone_execution),
        },
    ]
}

/// Every probe whose operation models NO absent-resource error, and so answers the
/// documented empty listing or already-satisfied delete instead.
fn served_probes(absent: &Absent) -> Vec<Served> {
    let Absent {
        machine,
        version,
        alias,
        ..
    } = absent;
    vec![
        Served {
            // "An array of tags associated with the resource": the array of an ARN
            // nothing is tagged under is empty, which is what the tagging READ
            // answers where the two tag WRITES above report the resource absent.
            case: "a read of the tags recorded under the state machine's ARN",
            operation: "ListTagsForResource",
            request: json!({ "resourceArn": machine }),
            empty_member: Some("tags"),
        },
        Served {
            case: "a listing of its versions",
            operation: "ListStateMachineVersions",
            request: json!({ "stateMachineArn": machine }),
            empty_member: Some("stateMachineVersions"),
        },
        Served {
            // Each delete's post-condition already holds, and none of the three
            // models an absent-resource error.
            case: "a delete of the state machine",
            operation: "DeleteStateMachine",
            request: json!({ "stateMachineArn": machine }),
            empty_member: None,
        },
        Served {
            case: "a delete of one of its versions",
            operation: "DeleteStateMachineVersion",
            request: json!({ "stateMachineVersionArn": version }),
            empty_member: None,
        },
        Served {
            case: "a delete of one of its aliases",
            operation: "DeleteStateMachineAlias",
            request: json!({ "stateMachineAliasArn": alias }),
            empty_member: None,
        },
    ]
}

/// How many times each probe is made: it is the REPETITION that is asserted — the
/// same answer every time, because a probe of an absent record writes nothing — and
/// never a span of the clock.
const PROBES_PER_CASE: usize = 3;

/// Every operation addressed to an absent state machine answers its own modelled
/// error, the same way however often it is asked and however the record came to be
/// absent — and the service keeps serving every other state machine throughout.
#[tokio::test]
async fn an_absent_state_machine_is_refused_the_same_way_every_time() {
    // The status a caller reads for every refusal below; that ONE status answers the
    // whole modelled client-error class is `client_refusal_status`'s own rule.
    assert_eq!(
        CLIENT_ERROR_STATUS, 400,
        "expected a refused request to read HTTP 400, got {CLIENT_ERROR_STATUS}"
    );

    let emulator = Emulator::new();
    // A state machine that is NOT probed, created before the probes and read after
    // them: whatever the probes answer, they leave this one exactly as it was.
    let live = create_machine(&emulator, "probe-bystander", HELLO_DEFINITION).await;
    let before = emulator
        .ok("DescribeStateMachine", &json!({ "stateMachineArn": &live }))
        .await;

    // Two ways to be absent: deleted after the record had carried a version, an
    // alias, a tag and a run, and never created at all.
    let deleted = deleted_state_machine(&emulator, "probe-deleted").await;
    let missing = never_created("probe-never-created");

    for (absence, absent) in [("deleted", &deleted), ("never created", &missing)] {
        for probe in refused_probes(absent) {
            for round in 0..PROBES_PER_CASE {
                let (status, body) = emulator.call(probe.operation, &probe.request).await;
                let code = error_code(&body);
                assert_eq!(
                    (status, code.as_str()),
                    (CLIENT_ERROR_STATUS, probe.code),
                    "case {:?} on a {absence} state machine, probe {round}: expected \
                     {CLIENT_ERROR_STATUS} {}, got {status} {code} with body {body:?}",
                    probe.case,
                    probe.code
                );
                if let Some(expected) = &probe.message {
                    assert_eq!(
                        body.get("message").and_then(Value::as_str),
                        Some(expected.as_str()),
                        "case {:?} on a {absence} state machine, probe {round}: expected message \
                         {expected:?}, got {body:?}",
                        probe.case
                    );
                }
            }
        }
    }

    let after = emulator
        .ok("DescribeStateMachine", &json!({ "stateMachineArn": &live }))
        .await;
    assert_eq!(
        after, before,
        "case the bystander state machine: the probes must leave it as it was, got {after:?} \
         after {before:?}"
    );
}

/// An operation that models no absent-resource error answers the documented empty
/// listing or already-satisfied delete instead — and answering it leaves the record
/// absent rather than bringing anything back.
#[tokio::test]
async fn an_absent_state_machine_serves_the_calls_that_model_no_absence() {
    let emulator = Emulator::new();
    let deleted = deleted_state_machine(&emulator, "served-deleted").await;
    let missing = never_created("served-never-created");

    for (absence, absent) in [("deleted", &deleted), ("never created", &missing)] {
        for probe in served_probes(absent) {
            for round in 0..PROBES_PER_CASE {
                let (status, body) = emulator.call(probe.operation, &probe.request).await;
                assert_eq!(
                    status,
                    200,
                    "case {:?} on a {absence} state machine, probe {round}: expected 200, got \
                     {status} with body {body:?}",
                    probe.case
                );
                if let Some(member) = probe.empty_member {
                    let reported = body
                        .get(member)
                        .and_then(Value::as_array)
                        .map(Vec::as_slice)
                        .unwrap_or_else(|| {
                            panic!(
                                "case {:?} on a {absence} state machine, probe {round}: expected \
                                 a '{member}' list, got {body:?}",
                                probe.case
                            )
                        });
                    assert!(
                        reported.is_empty(),
                        "case {:?} on a {absence} state machine, probe {round}: expected an empty \
                         '{member}', got {reported:?}",
                        probe.case
                    );
                }
            }
        }

        // And none of those successes put anything back: the record is still absent.
        let (status, body) = emulator
            .call(
                "DescribeStateMachine",
                &json!({ "stateMachineArn": &absent.machine }),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (CLIENT_ERROR_STATUS, "StateMachineDoesNotExist"),
            "case a {absence} state machine: the served probes must leave it absent, got {status} \
             with body {body:?}"
        );
    }
}
