// SPDX-License-Identifier: CC0-1.0

//! WHICH NAME a failed `Task` state's error carries for the service it called, per
//! integration FAMILY.
//!
//! The rule under test: a refused service-integration call is reported as
//! `<service>.<exception>`, where the exception half is the class the answer's error
//! shape resolves to — the same class whichever `Resource` named the call — and the
//! SERVICE half is the name the SDK that serves the naming FAMILY gives that service:
//! the AWS SDK's own name for it through the AWS SDK family
//! (`arn:aws:states:::aws-sdk:<sdk name>:<action>`), and the name Step Functions' own
//! integration writes for it through an optimized `Resource`
//! (`arn:aws:states:::<namespace>:<action>`). Those two names differ for a service
//! whose SDK spells its id differently from the integration (`DynamoDb` beside
//! `DynamoDB`, `Sqs` beside `SQS`, `Sfn` beside `StepFunctions`), and a definition's
//! `Catch` matches `ErrorEquals` exactly — so the name the family writes is the name a
//! catcher has to carry.
//!
//! The names each family writes per service are stated per entry in this crate's
//! `state::integration::APIS` (`Api::error_prefix` and `Api::sdk_error_prefix`), which
//! is where the AWS SDK's own name for a service is recorded; this suite reads them off
//! the wire instead, by driving both `Resource` spellings of the SAME call to the SAME
//! service and comparing what the task reported.
//!
//! The rows differ along what the rule generalises over: which service answered, which
//! error class it answered with (a modelled shape already spelled with the `Exception`
//! suffix, a modelled shape without it, and a code no shape of that model declares, so
//! the base exception carries it), and which family named the call. The sibling suite
//! `integration_failure_error_names` pins the exception half over the optimized family;
//! this one pins the service half across both.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::integration_failure_error_names::{given_queue, Emulator};
use crate::state_machine_lifecycle::{REGION, ROLE_ARN};

/// Long enough for an execution whose one task was refused to reach a terminal status,
/// whatever else the host is running. Nothing below measures how long anything took:
/// the deadline only bounds the wait.
const PATIENCE: Duration = Duration::from_secs(60);

/// The execution `execution_arn` describes as once it has left `RUNNING`, looked at
/// again until it has or [`PATIENCE`] has passed.
async fn settled(emulator: &Emulator, case: &str, execution_arn: &str) -> Value {
    let began = Instant::now();
    loop {
        let described = emulator
            .sfn("DescribeExecution", &json!({ "executionArn": execution_arn }))
            .await;
        if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
            return described;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected a terminal status within {PATIENCE:?}, last saw {described}"
        );
        ::tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Run `definition` once under the name `name` and describe the execution it settled as.
async fn ran(emulator: &Emulator, case: &str, name: &str, definition: &Value) -> Value {
    let created = emulator
        .sfn(
            "CreateStateMachine",
            &json!({
                "name": name,
                "definition": definition.to_string(),
                "roleArn": ROLE_ARN,
                "type": "STANDARD",
            }),
        )
        .await;
    let machine = created["stateMachineArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no stateMachineArn in {created}"));
    let started = emulator
        .sfn(
            "StartExecution",
            &json!({ "stateMachineArn": machine, "name": "once", "input": "{}" }),
        )
        .await;
    let execution = started["executionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no executionArn in {started}"))
        .to_string();
    settled(emulator, case, &execution).await
}

/// A one-`Task` definition calling `resource` with `parameters`.
fn task(resource: &str, parameters: &Value) -> Value {
    json!({
        "StartAt": "Call",
        "States": {
            "Call": {
                "Type": "Task",
                "Resource": resource,
                "Parameters": parameters,
                "End": true,
            },
        },
    })
}

/// One pair of rows: the SAME refused call named through each family.
struct Pair {
    case: &'static str,
    /// The state-machine names the two rows are created under, which have to be unique
    /// in the account.
    machines: (&'static str, &'static str),
    /// The optimized `Resource` and the AWS SDK one, which name the same operation of
    /// the same service.
    resources: (&'static str, &'static str),
    parameters: Value,
    /// The service half each family writes, and the exception half both share.
    optimized_service: &'static str,
    sdk_service: &'static str,
    exception: &'static str,
    /// A fragment of the invoked service's own answer the cause carries, which is the
    /// same answer whichever family named the call.
    carries: String,
}

/// The pairs under test, built against the services this harness mounts.
async fn pairs(emulator: &Emulator) -> Vec<Pair> {
    // A FIFO queue — "A FIFO queue name must end with the .fifo suffix" — and a url
    // addressing a queue nobody created.
    let fifo = given_queue(emulator, "family-orders.fifo", json!({ "FifoQueue": "true" })).await;
    let absent_queue = fifo.replace("family-orders.fifo", "family-nowhere");
    // A state machine ARN of this account that names no state machine.
    let absent_machine = format!("arn:aws:states:{REGION}:000000000000:stateMachine:family-nowhere");
    vec![
        Pair {
            // DynamoDB models this error's shape spelled with the `Exception` suffix
            // already, so the exception half is the shape name itself.
            case: "a table the account does not hold",
            machines: ("family-table-optimized", "family-table-sdk"),
            resources: (
                "arn:aws:states:::dynamodb:putItem",
                "arn:aws:states:::aws-sdk:dynamodb:putItem",
            ),
            parameters: json!({ "TableName": "family-nowhere", "Item": { "pk": { "S": "a" } } }),
            optimized_service: "DynamoDB",
            sdk_service: "DynamoDb",
            exception: "ResourceNotFoundException",
            carries: "Error Code: ResourceNotFoundException".to_string(),
        },
        Pair {
            // Amazon SQS models `QueueDoesNotExist` WITHOUT the suffix its class
            // carries, which is the other half of the exception rule.
            case: "a queue the account does not hold",
            machines: ("family-queue-optimized", "family-queue-sdk"),
            resources: (
                "arn:aws:states:::sqs:sendMessage",
                "arn:aws:states:::aws-sdk:sqs:sendMessage",
            ),
            parameters: json!({ "QueueUrl": absent_queue, "MessageBody": "hello" }),
            optimized_service: "SQS",
            sdk_service: "Sqs",
            exception: "QueueDoesNotExistException",
            // The one sentence Amazon SQS answers for a queue that is not there.
            carries: "The specified queue does not exist.".to_string(),
        },
        Pair {
            // "If you do not provide a MessageGroupId when sending a message to a FIFO
            // queue, the action fails": Amazon SQS declares no error shape for a
            // parameter left out, so the service's BASE exception is the exception half
            // — and the service half is still the family's.
            case: "a parameter a FIFO send had to carry, which the model declares no shape for",
            machines: ("family-group-optimized", "family-group-sdk"),
            resources: (
                "arn:aws:states:::sqs:sendMessage",
                "arn:aws:states:::aws-sdk:sqs:sendMessage",
            ),
            parameters: json!({ "QueueUrl": fifo, "MessageBody": "ordered" }),
            optimized_service: "SQS",
            sdk_service: "Sqs",
            exception: "AmazonSQSException",
            carries: "MessageGroupId".to_string(),
        },
        Pair {
            // A third service — this one itself — whose two names differ the most: the
            // optimized integration writes the service's full name, the AWS SDK its id.
            case: "a child state machine the account does not hold",
            machines: ("family-child-optimized", "family-child-sdk"),
            resources: (
                "arn:aws:states:::states:startExecution",
                "arn:aws:states:::aws-sdk:sfn:startExecution",
            ),
            parameters: json!({ "StateMachineArn": absent_machine }),
            optimized_service: "StepFunctions",
            sdk_service: "Sfn",
            exception: "StateMachineDoesNotExistException",
            carries: "Error Code: StateMachineDoesNotExist".to_string(),
        },
    ]
}

/// A refused call is reported under the service as the family that named it names the
/// service, and under the same exception class either way.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_integration_family_names_the_service_as_its_own_sdk_does() {
    let emulator = Emulator::new();
    for pair in pairs(&emulator).await {
        let Pair {
            case,
            machines,
            resources,
            parameters,
            optimized_service,
            sdk_service,
            exception,
            carries,
        } = pair;
        // The two names are different strings: a pair whose two halves agreed would
        // pin nothing about which family wrote which.
        assert_ne!(
            optimized_service, sdk_service,
            "case {case:?}: the two families name this service differently"
        );
        for (family, machine, resource, service) in [
            ("the optimized integration", machines.0, resources.0, optimized_service),
            ("the AWS SDK integration", machines.1, resources.1, sdk_service),
        ] {
            let case = format!("{case} through {family}");
            let expected = format!("{service}.{exception}");
            let described = ran(&emulator, &case, machine, &task(resource, &parameters)).await;
            assert_eq!(
                described.get("status").and_then(Value::as_str),
                Some("FAILED"),
                "case {case:?}: expected the refused call to fail the execution, got {described}"
            );
            assert_eq!(
                described.get("error").and_then(Value::as_str),
                Some(expected.as_str()),
                "case {case:?}: expected the error {expected:?}, got {described}"
            );
            // The cause is the invoked service's own answer, which the family the
            // `Resource` named does not change.
            let cause = described
                .get("cause")
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert!(
                cause.contains(carries.as_str()),
                "case {case:?}: expected the cause to carry {carries:?}, got {cause:?}"
            );
        }
    }
}

/// One matching row: the `Resource` the task names, the name its catcher names, and
/// whether that catcher takes the failure over.
struct Match {
    case: &'static str,
    machine: &'static str,
    resource: &'static str,
    named: &'static str,
    /// The name the task really failed under, which a catcher matches only by naming it
    /// exactly.
    reported: &'static str,
    catches: bool,
}

/// `ErrorEquals` matches the error name EXACTLY, so a catcher recovers a failed
/// integration call only when it names the service the way the family that made the
/// call does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_catch_recovers_the_call_only_under_the_name_its_family_wrote() {
    const OPTIMIZED: &str = "arn:aws:states:::dynamodb:putItem";
    const AWS_SDK: &str = "arn:aws:states:::aws-sdk:dynamodb:putItem";
    const OPTIMIZED_ERROR: &str = "DynamoDB.ResourceNotFoundException";
    const SDK_ERROR: &str = "DynamoDb.ResourceNotFoundException";

    let emulator = Emulator::new();
    let rows = [
        Match {
            case: "the AWS SDK family, caught under the AWS SDK's name for the service",
            machine: "catch-sdk-own",
            resource: AWS_SDK,
            named: SDK_ERROR,
            reported: SDK_ERROR,
            catches: true,
        },
        Match {
            case: "the AWS SDK family, named the optimized family's way",
            machine: "catch-sdk-other",
            resource: AWS_SDK,
            named: OPTIMIZED_ERROR,
            reported: SDK_ERROR,
            catches: false,
        },
        Match {
            case: "the optimized family, caught under the name that family writes",
            machine: "catch-optimized-own",
            resource: OPTIMIZED,
            named: OPTIMIZED_ERROR,
            reported: OPTIMIZED_ERROR,
            catches: true,
        },
        Match {
            case: "the optimized family, named the AWS SDK family's way",
            machine: "catch-optimized-other",
            resource: OPTIMIZED,
            named: SDK_ERROR,
            reported: OPTIMIZED_ERROR,
            catches: false,
        },
        Match {
            // A second service, so the matching rule is not one service's property.
            case: "a second service through the AWS SDK family",
            machine: "catch-sdk-sfn",
            resource: "arn:aws:states:::aws-sdk:sfn:startExecution",
            named: "Sfn.StateMachineDoesNotExistException",
            reported: "Sfn.StateMachineDoesNotExistException",
            catches: true,
        },
    ];

    let absent_machine = format!("arn:aws:states:{REGION}:000000000000:stateMachine:catch-nowhere");
    for row in rows {
        let parameters = match row.resource.ends_with("startExecution") {
            true => json!({ "StateMachineArn": absent_machine.clone() }),
            false => json!({ "TableName": "catch-nowhere", "Item": { "pk": { "S": "a" } } }),
        };
        let case = row.case;
        let definition = json!({
            "StartAt": "Call",
            "States": {
                "Call": {
                    "Type": "Task",
                    "Resource": row.resource,
                    "Parameters": parameters,
                    "Catch": [{
                        "ErrorEquals": [row.named],
                        "Next": "Report",
                        "ResultPath": "$.failure",
                    }],
                    "End": true,
                },
                "Report": { "Type": "Pass", "OutputPath": "$.failure.Error", "End": true },
            },
        });
        let described = ran(&emulator, case, row.machine, &definition).await;
        let status = described.get("status").and_then(Value::as_str);
        if !row.catches {
            assert_eq!(
                status,
                Some("FAILED"),
                "case {case:?}: a catcher naming {:?} must not take over a failure reported as \
                 {:?}, got {described}",
                row.named,
                row.reported
            );
            assert_eq!(
                described.get("error").and_then(Value::as_str),
                Some(row.reported),
                "case {case:?}: expected the error {:?}, got {described}",
                row.reported
            );
            continue;
        }
        assert_eq!(
            status,
            Some("SUCCEEDED"),
            "case {case:?}: expected the catcher naming {:?} to recover the execution, got \
             {described}",
            row.named
        );
        // The error output the catcher placed carries the very name it matched on.
        let output = described
            .get("output")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let put_out: Value = ::serde_json::from_str(output)
            .unwrap_or_else(|why| panic!("case {case:?}: expected a JSON output, got {why} in {output:?}"));
        assert_eq!(
            put_out,
            json!(row.reported),
            "case {case:?}: expected the error output to name {:?}, got {put_out}",
            row.reported
        );
    }
}
