// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a `Task` state whose AWS SDK integration names an operation of
//! Amazon EC2, driven over both services' own wire protocols through the production
//! `MultiServiceRouter` with Step Functions and EC2 mounted on ONE engine — the shape
//! `core/router/src/lib.rs` mounts them in.
//!
//! The rule under test: a `Task` state's service integration MAKES the call to the
//! service that owns the operation, so what the task reports is that service's own
//! answer — the operation's output members for a call it served, and, for a call it
//! REFUSED, the service-qualified name of the exception class that refusal resolves to.
//! Amazon EC2's model declares no error SHAPE at all, so no client has a class for any of
//! the codes it refuses with and each refusal resolves to the base exception of its
//! client: `Ec2.Ec2Exception`, whatever code the answer named. An interpreter-level
//! `States.Runtime` is what the execution reports only where nothing was run.
//!
//! The rows span what the rule generalises over: the operation called, the REFUSAL the
//! service answered (an unknown instance, an unknown volume, a parameter it will not
//! take), and the direction of the data flow (a launch that stores instances, a describe
//! that reads them back). The complement rows are the calls that SUCCEED — proving the
//! same binding carries a request EC2 serves — and an operation this emulator binds no
//! request for, which is still not faked.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_sfn_awsstepfunctions::EmulatedService;

use crate::state_machine_lifecycle::{string, REGION, ROLE_ARN};

/// Step Functions' wire facts, from its model's own `service_info`.
const SFN_TARGET_PREFIX: &str = "AWSStepFunctions";
const SFN_SIGNING_NAME: &str = "states";
const SFN_JSON_VERSION: &str = "1.0";

/// The error name every refusal of this service reaches a definition under: the name the
/// AWS SDK gives the service and the base exception its client raises, because that
/// model declares no error shape for any code the service answers.
const EC2_BASE_ERROR: &str = "Ec2.Ec2Exception";

/// How often an execution is looked at again, and how long it is waited for at all.
///
/// The wait is for the CONDITION — the execution leaving `RUNNING` — so the deadline is
/// generous and a host that settles an execution quickly spends none of it: nothing here
/// measures how fast the host runs.
const SETTLE_POLL: Duration = Duration::from_millis(25);
/// See [`SETTLE_POLL`].
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// Step Functions and Amazon EC2 over ONE engine, so the answer a task reads is the one
/// EC2 itself gives the same caller.
struct Chain {
    router: MultiServiceRouter,
}

impl Chain {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_ec2_amazonec2::EmulatedService::new(Arc::clone(&engine)),
        );
        Self { router }
    }

    /// One Step Functions operation, requiring a 200.
    async fn sfn(&self, operation: &str, body: &Value) -> Value {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{SFN_SIGNING_NAME}/\
             aws4_request, SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{SFN_SIGNING_NAME}.{REGION}.amazonaws.com"))
            .header(
                "content-type",
                format!("application/x-amz-json-{SFN_JSON_VERSION}"),
            )
            .header("x-amz-target", format!("{SFN_TARGET_PREFIX}.{operation}"))
            .header("authorization", authorization)
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                panic!(
                    "case {operation}: expected a JSON body, got {error} for {:?}",
                    String::from_utf8_lossy(&bytes)
                )
            })
        };
        assert_eq!(
            status, 200,
            "case {operation}: expected 200, got {status} with body {parsed:?}"
        );
        parsed
    }

    /// Create a state machine, start one execution of it, and report what it ended as —
    /// looked at again until it has left `RUNNING`.
    async fn run(&self, case: &str, name: &str, definition: &Value) -> Value {
        let created = self
            .sfn(
                "CreateStateMachine",
                &json!({
                    "name": name,
                    "definition": definition.to_string(),
                    "roleArn": ROLE_ARN,
                }),
            )
            .await;
        let machine = string(&created, "stateMachineArn");
        let started = self
            .sfn(
                "StartExecution",
                &json!({ "stateMachineArn": machine, "name": "only", "input": "{}" }),
            )
            .await;
        let execution = string(&started, "executionArn");
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let described = self
                .sfn("DescribeExecution", &json!({ "executionArn": execution }))
                .await;
            if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
                return described;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the execution to settle, got {described:?}"
            );
            tokio::time::sleep(SETTLE_POLL).await;
        }
    }
}

/// A one-state definition whose `Task` names `resource` and sends `parameters`.
fn task(resource: &str, parameters: Value) -> Value {
    json!({
        "StartAt": "Work",
        "States": {
            "Work": {
                "Type": "Task",
                "Resource": resource,
                "Parameters": parameters,
                "End": true,
            },
        },
    })
}

/// The output one execution produced, as the document it is.
fn output(case: &str, described: &Value) -> Value {
    described
        .get("output")
        .and_then(Value::as_str)
        .map(|text| serde_json::from_str(text).expect("the output is JSON"))
        .unwrap_or_else(|| panic!("case {case:?}: expected an output in {described:?}"))
}

/// A call Amazon EC2 REFUSES fails the task under the service-qualified name of the
/// exception that refusal resolves to, with the service's own sentence as the cause —
/// never under the interpreter's own `States.Runtime`, which says nothing was run.
///
/// The rows differ along the operation called and the refusal the service answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_ec2_call_fails_the_task_under_that_services_own_exception() {
    let chain = Chain::new();

    // An id of the form EC2 issues, which no instance of this account was launched for.
    let absent_instance = "i-0123456789abcdef0";
    // (case, the resource the task names, its parameters, the status EC2 answers with)
    let rows: &[(&str, &str, Value, u16)] = &[
        (
            "a describe naming an instance the account does not hold",
            "arn:aws:states:::aws-sdk:ec2:describeInstances",
            json!({ "InstanceIds": [absent_instance] }),
            400,
        ),
        (
            "a terminate naming that same instance",
            "arn:aws:states:::aws-sdk:ec2:terminateInstances",
            json!({ "InstanceIds": [absent_instance] }),
            400,
        ),
        (
            "a start naming it",
            "arn:aws:states:::aws-sdk:ec2:startInstances",
            json!({ "InstanceIds": [absent_instance] }),
            400,
        ),
        (
            "an id of a form EC2 could not have issued",
            "arn:aws:states:::aws-sdk:ec2:describeInstances",
            json!({ "InstanceIds": ["not-an-instance-id"] }),
            400,
        ),
        (
            "a launch whose AMI id is of a form EC2 could not have issued",
            "arn:aws:states:::aws-sdk:ec2:runInstances",
            json!({ "ImageId": "not-an-ami", "MinCount": 1, "MaxCount": 1 }),
            400,
        ),
        (
            "a launch asking for fewer than one instance",
            "arn:aws:states:::aws-sdk:ec2:runInstances",
            json!({ "ImageId": "ami-0123456789abcdef0", "MinCount": 0, "MaxCount": 1 }),
            400,
        ),
    ];

    for (index, (case, resource, parameters, status)) in rows.iter().enumerate() {
        let described = chain
            .run(
                case,
                &format!("ec2-refused-{index}"),
                &task(resource, parameters.clone()),
            )
            .await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("FAILED"),
            "case {case:?}: expected FAILED, got {described:?}"
        );
        assert_eq!(
            described.get("error").and_then(Value::as_str),
            Some(EC2_BASE_ERROR),
            "case {case:?}: expected {EC2_BASE_ERROR}, got {described:?}"
        );
        let cause = described
            .get("cause")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // The cause is the exception the serving client rendered: the service's OWN
        // sentence, then the metadata the answer carried — the service as that SDK names
        // it and the status it really answered.
        let (sentence, metadata) = cause
            .split_once(" (Service: ")
            .unwrap_or_else(|| panic!("case {case:?}: expected a rendered exception, got {cause:?}"));
        assert!(
            !sentence.trim().is_empty(),
            "case {case:?}: expected the sentence EC2 refused the call with, got {cause:?}"
        );
        assert!(
            metadata.starts_with("Ec2,") && metadata.contains(&format!("Status Code: {status}")),
            "case {case:?}: expected the service and the status EC2 answered ({status}), got \
             {cause:?}"
        );
        // And the identifier EC2 named the refused request by, which that protocol carries
        // in the error envelope rather than in a header.
        assert!(
            metadata.contains("Request ID: "),
            "case {case:?}: expected the request EC2 named its refusal under, got {cause:?}"
        );
    }
}

/// A call Amazon EC2 SERVES reports that operation's own output members, and what one
/// call stored is what the next one reads back: the task's parameters are the request EC2
/// really received, and its result is the answer EC2 really gave.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ec2_call_a_task_makes_is_served_out_of_that_services_own_store() {
    let chain = Chain::new();

    // A launch of two instances: the list members of the request go on the wire under the
    // singular field this service's model names them by, and the answer's own elements
    // are reported as the operation's output members.
    let case = "a launch answers the reservation it minted";
    let launched = chain
        .run(
            case,
            "ec2-launch",
            &task(
                "arn:aws:states:::aws-sdk:ec2:runInstances",
                json!({
                    "ImageId": "ami-0123456789abcdef0",
                    "MinCount": 2,
                    "MaxCount": 2,
                    "TagSpecifications": [{
                        "ResourceType": "instance",
                        "Tags": [{ "Key": "owner", "Value": "a task" }],
                    }],
                }),
            ),
        )
        .await;
    assert_eq!(
        launched.get("status").and_then(Value::as_str),
        Some("SUCCEEDED"),
        "case {case:?}: expected SUCCEEDED, got {launched:?}"
    );
    let reservation = output(case, &launched);
    assert!(
        reservation
            .get("ReservationId")
            .and_then(Value::as_str)
            .is_some_and(|id| id.starts_with("r-")),
        "case {case:?}: expected the reservation EC2 minted, got {reservation}"
    );
    let instances = reservation
        .get("Instances")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected the instances, got {reservation}"));
    assert_eq!(
        instances.len(),
        2,
        "case {case:?}: expected the two instances the launch asked for, got {reservation}"
    );
    let ids: Vec<String> = instances
        .iter()
        .map(|instance| {
            instance
                .get("instanceId")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: an instance carries its id"))
                .to_string()
        })
        .collect();

    // And the describe that names those ids reads them back: the ids reached EC2 as the
    // numbered fields of its own protocol, so the call is narrowed to them.
    let case = "a describe naming the launched instances reports them";
    let described = chain
        .run(
            case,
            "ec2-describe",
            &task(
                "arn:aws:states:::aws-sdk:ec2:describeInstances",
                json!({ "InstanceIds": ids }),
            ),
        )
        .await;
    assert_eq!(
        described.get("status").and_then(Value::as_str),
        Some("SUCCEEDED"),
        "case {case:?}: expected SUCCEEDED, got {described:?}"
    );
    let reported = output(case, &described);
    let reservations = reported
        .get("Reservations")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected the reservations, got {reported}"));
    let read_back: Vec<String> = reservations
        .iter()
        .flat_map(|held| {
            held.get("instancesSet")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .filter_map(|instance| {
            instance
                .get("instanceId")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    assert_eq!(
        read_back, ids,
        "case {case:?}: expected the instances the launch stored, got {reported}"
    );
}

/// An operation of this service whose request this emulator binds to no form field is
/// still not faked: the execution fails with ASL's own `States.Runtime` and a cause that
/// says what was not run.
#[tokio::test]
async fn an_operation_no_request_is_bound_for_is_not_faked() {
    let chain = Chain::new();

    // (case, the action the resource names)
    let rows: &[(&str, &str)] = &[
        ("an operation no binding is stated for", "describeRegions"),
        ("another of the same", "createVpc"),
    ];
    for (index, (case, action)) in rows.iter().enumerate() {
        let described = chain
            .run(
                case,
                &format!("ec2-unbound-{index}"),
                &task(
                    &format!("arn:aws:states:::aws-sdk:ec2:{action}"),
                    json!({}),
                ),
            )
            .await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("FAILED"),
            "case {case:?}: expected FAILED, got {described:?}"
        );
        assert_eq!(
            described.get("error").and_then(Value::as_str),
            Some("States.Runtime"),
            "case {case:?}: expected States.Runtime, got {described:?}"
        );
        assert!(
            described
                .get("cause")
                .and_then(Value::as_str)
                .is_some_and(|cause| cause.contains("form-field protocol")),
            "case {case:?}: the cause says what was not bound, got {described:?}"
        );
    }
}
