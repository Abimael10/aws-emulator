// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a client POLLING an execution sees when a `Task` state's
//! integration call is one the execution role's policies withhold, driven over all
//! three services' own wire protocols through the production `MultiServiceRouter` with
//! Step Functions, IAM and DynamoDB mounted on ONE engine — the shape
//! `core/router/src/lib.rs` mounts them in — so the policies the call is judged against
//! are the documents IAM's own handlers wrote and the far-end effect is read back
//! through DynamoDB's own API.
//!
//! RULE: a call the execution role's policies do not allow is reported to the
//! interpreter as that state's FAILURE, exactly as a call the far end refused would be,
//! so the execution SETTLES on its own — it leaves `RUNNING` for a terminal status that
//! `DescribeExecution` reports with a `stopDate`, its history closes on a terminal
//! event, and no reader is left with a task that never answered. Because the denial is
//! an ordinary task failure, the state's own `Retry` and `Catch` route it like any other
//! task error: a catcher that names it carries the execution down its branch to that
//! branch's own outcome, and with no catcher the execution ends `FAILED` under the
//! invoked service's refusal. Nothing is left pending either way.
//!
//! The rows differ along what the rule generalises over: the integration FAMILY the
//! `Resource` belongs to (the optimized `arn:aws:states:::dynamodb:<action>` and the AWS
//! SDK `arn:aws:states:::aws-sdk:dynamodb:<action>` form), the ACTION the role is
//! withheld (a write and a read), how the denied state is WRAPPED (on its own, behind a
//! `Retry` whose wildcard names the failure, under a `Catch` that covers it, inside one
//! branch of a `Parallel`, and after a state that already ran), and therefore which
//! terminal status the execution settles on.
//!
//! Every wait here waits for the CONDITION — a created table serving items, an execution
//! leaving `RUNNING` — and looks again until it holds or [`PATIENCE`] passes, so nothing
//! here depends on how fast the host runs. Which policies decide the call at all is the
//! question `tests/all/task_role_over_dynamodb.rs` holds; this suite is about the
//! execution SETTLING once they have decided against it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    EVENT_EXECUTION_FAILED, EVENT_EXECUTION_SUCCEEDED, EVENT_TASK_FAILED, EVENT_TASK_SUBMITTED,
    EVENT_TASK_SUCCEEDED,
};
use service_com_amazonaws_sfn_awsstepfunctions::EmulatedService;

use crate::execution_data_encryption::{form_encode, xml_element};
use crate::state_machine_lifecycle::{string, REGION};

/// How each service's requests are addressed on the wire, from each model's own
/// `service_info` (service shape, `sigv4Name` and the `awsJson` version its protocol
/// declares).
const SFN: Wire = Wire {
    signing_name: "states",
    target_prefix: "AWSStepFunctions",
};
const DYNAMODB: Wire = Wire {
    signing_name: "dynamodb",
    target_prefix: "DynamoDB_20120810",
};

/// How one service's requests are addressed on the wire. Both services below declare
/// `awsJson1_0`, so the content type is the same for each.
struct Wire {
    signing_name: &'static str,
    target_prefix: &'static str,
}

/// How long a wait for a condition may take before it is a failure, and how long one
/// look sleeps before the next.
///
/// Generous on purpose: what is under test is that the condition is reached at all, not
/// how fast this host reaches it, so a loaded host is never cut short and a quick one
/// spends none of the deadline.
const PATIENCE: Duration = Duration::from_secs(30);
/// See [`PATIENCE`].
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The table every row's `Task` addresses, which exists and is `ACTIVE` throughout: the
/// failure under test is the DENIAL, so the table the call names must be one a call the
/// role was allowed would have written.
const TABLE: &str = "settles";

/// The attribute that table is keyed by.
const KEY: &str = "pk";

/// The error a `Task` state reports when DynamoDB refuses the call it made with an error
/// that operation does not model: DynamoDB's base SDK exception, under the name the
/// OPTIMIZED integration family writes for the service.
const REFUSAL_ERROR: &str = "DynamoDB.AmazonDynamoDBException";

/// The same refusal of a call named through the AWS SDK integration family, which names
/// the service as the AWS SDK does.
const SDK_REFUSAL_ERROR: &str = "DynamoDb.AmazonDynamoDBException";

/// The sentence an access-denied answer carries, which is what makes the cause a
/// DENIAL rather than any other refusal: IAM words it over the principal, the action and
/// the resource.
const NOT_AUTHORIZED: &str = "is not authorized to perform";

/// The status DynamoDB's own front door answers an unauthorized call with — that service
/// words every client error as a BAD REQUEST — as the metadata of either family's client
/// reports it.
const REFUSAL_STATUS_FIELD: &str = "Status Code: 400";

/// The code the refused answer came back under, as the metadata of the OPTIMIZED
/// family's client reports it; the current AWS SDK's client writes no such field, so only
/// a row of that family holds it.
const REFUSAL_CODE_FIELD: &str = "Error Code: AccessDeniedException";

/// The two wildcards the Amazon States Language documents for a retrier and a catcher:
/// "States.ALL — A wildcard that matches any known error name", and "States.TaskFailed
/// […] matches any known error name except States.Timeout".
const TASK_FAILED_WILDCARD: &str = "States.TaskFailed";
/// See [`TASK_FAILED_WILDCARD`].
const ALL_ERRORS_WILDCARD: &str = "States.ALL";

/// Step Functions, IAM and DynamoDB over ONE engine.
///
/// Its own harness rather than a sibling suite's: every wait here is a poll loop, which
/// is the whole point of this suite, and the suites that drive these three services wait
/// for a span instead.
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
            service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// One `awsJson` call to one service, requiring a 200. The `Authorization` header's
    /// credential scope is what routes the request and names the Region its ARNs carry.
    async fn json(&self, wire: &Wire, operation: &str, body: &Value) -> Value {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{}/aws4_request, \
             SignedHeaders=host;x-amz-target, Signature=0123456789abcdef",
            wire.signing_name
        );
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header(
                "host",
                format!("{}.{REGION}.amazonaws.com", wire.signing_name),
            )
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("{}.{operation}", wire.target_prefix))
            .header("authorization", authorization)
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        assert_eq!(
            status, 200,
            "case {operation}: expected 200, got {status} with body {parsed:?}"
        );
        parsed
    }

    async fn sfn(&self, operation: &str, body: &Value) -> Value {
        self.json(&SFN, operation, body).await
    }

    async fn ddb(&self, operation: &str, body: &Value) -> Value {
        self.json(&DYNAMODB, operation, body).await
    }

    /// One `awsQuery` call to IAM, signed with the same credential.
    async fn iam(&self, action: &str, params: &[(&str, String)]) -> String {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", "iam.amazonaws.com")
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iam/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(Bytes::from(body))
            .expect("a well-formed awsQuery request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(&response.into_body()).to_string();
        assert_eq!(
            status, 200,
            "case {action}: expected 200 from IAM, got {status} with {text}"
        );
        text
    }

    /// An IAM role carrying `policy` as its inline policy, by its own ARN.
    async fn role(&self, name: &str, policy: &Value) -> String {
        let created = self
            .iam(
                "CreateRole",
                &[
                    ("RoleName", name.to_string()),
                    (
                        "AssumeRolePolicyDocument",
                        json!({
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Principal": { "Service": "states.amazonaws.com" },
                                "Action": "sts:AssumeRole"
                            }]
                        })
                        .to_string(),
                    ),
                ],
            )
            .await;
        let arn = xml_element(&created, "Arn")
            .unwrap_or_else(|| panic!("CreateRole answers the role Arn, got {created}"));
        self.iam(
            "PutRolePolicy",
            &[
                ("RoleName", name.to_string()),
                ("PolicyName", "inline".to_string()),
                ("PolicyDocument", policy.to_string()),
            ],
        )
        .await;
        arn
    }

    /// Create a table keyed by a single string partition key, wait until it serves items
    /// — "You can perform read and write operations only on an ACTIVE table" — and report
    /// the ARN DynamoDB minted for it.
    async fn given_table(&self, name: &str) -> String {
        let created = self
            .ddb(
                "CreateTable",
                &json!({
                    "TableName": name,
                    "KeySchema": [{ "AttributeName": KEY, "KeyType": "HASH" }],
                    "AttributeDefinitions": [{ "AttributeName": KEY, "AttributeType": "S" }],
                    "BillingMode": "PAY_PER_REQUEST"
                }),
            )
            .await;
        let arn = created
            .pointer("/TableDescription/TableArn")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("CreateTable answers a TableArn, got {created:?}"))
            .to_string();
        let deadline = Instant::now() + PATIENCE;
        loop {
            let described = self.ddb("DescribeTable", &json!({ "TableName": name })).await;
            let status = described
                .pointer("/Table/TableStatus")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if status == "ACTIVE" {
                return arn;
            }
            assert!(
                Instant::now() < deadline,
                "table {name}: expected it to become ACTIVE, got {status:?}"
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }

    /// The item stored under `pk`, or `None` when the table holds none.
    async fn item(&self, pk: &str) -> Option<Value> {
        let read = self
            .ddb(
                "GetItem",
                &json!({ "TableName": TABLE, "Key": { KEY: { "S": pk } } }),
            )
            .await;
        read.get("Item").cloned()
    }

    /// Create a state machine under `role_arn` running `definition` and start one
    /// execution of it, answering that execution's ARN.
    async fn started(&self, name: &str, definition: &Value, role_arn: &str) -> String {
        let created = self
            .sfn(
                "CreateStateMachine",
                &json!({
                    "name": name,
                    "definition": definition.to_string(),
                    "roleArn": role_arn
                }),
            )
            .await;
        let started = self
            .sfn(
                "StartExecution",
                &json!({
                    "stateMachineArn": string(&created, "stateMachineArn"),
                    "name": "only"
                }),
            )
            .await;
        string(&started, "executionArn")
    }

    /// Describe `execution` until it reports a status other than `RUNNING` and answer
    /// that record. The wait is for the STATUS and never for a span: the execution
    /// settles when it settles, up to [`PATIENCE`].
    async fn poll_until_terminal(&self, case: &str, execution: &str) -> Value {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let described = self
                .sfn("DescribeExecution", &json!({ "executionArn": execution }))
                .await;
            if string(&described, "status") != "RUNNING" {
                return described;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the execution to leave RUNNING, got {described:?}"
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }

    /// One execution's whole history.
    async fn history(&self, execution: &str) -> Vec<Value> {
        let answered = self
            .sfn(
                "GetExecutionHistory",
                &json!({ "executionArn": execution, "maxResults": 1000 }),
            )
            .await;
        answered
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    /// Whether this machine still reports a `RUNNING` execution, which is the other
    /// reader's view of an execution that never settled.
    async fn running_executions(&self, execution: &str) -> Vec<String> {
        let machine = execution
            .replace(":execution:", ":stateMachine:")
            .rsplit_once(':')
            .map(|(machine, _)| machine.to_string())
            .expect("an execution ARN closes with the execution's own name");
        let listed = self
            .sfn(
                "ListExecutions",
                &json!({ "stateMachineArn": machine, "statusFilter": "RUNNING" }),
            )
            .await;
        listed
            .get("executions")
            .and_then(Value::as_array)
            .map(|executions| {
                executions
                    .iter()
                    .filter_map(|held| held.get("executionArn").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The event types of a history, in order.
fn types(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| event.get("type").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// The names of every state a history records being entered, in order.
fn entered(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| event.pointer("/stateEnteredEventDetails/name"))
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// The `Task` state one row's denied call is made by, naming `resource`'s integration
/// over this suite's table.
///
/// The members are DynamoDB's own, from `smithy_query(query="operation",
/// operation="PutItem,GetItem", model_path="services/
/// service-com-amazonaws-dynamodb-dynamodb-20120810/smithy-model/
/// com-amazonaws-dynamodb-dynamodb-20120810.json")`: `TableName` and `Item` for the
/// write, `TableName` and `Key` for the read.
fn denied_task(resource: &str, pk: &str) -> Value {
    let parameters = match resource.ends_with("getItem") {
        true => json!({ "TableName": TABLE, "Key": { KEY: { "S": pk } } }),
        false => json!({
            "TableName": TABLE,
            "Item": { KEY: { "S": pk }, "status": { "S": "written" } }
        }),
    };
    json!({ "Type": "Task", "Resource": resource, "Parameters": parameters })
}

/// What a row expects the execution to settle on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Settles {
    /// No catcher covers the denial, so the execution ends `FAILED` under the invoked
    /// service's own refusal, reported under `error`.
    Failed(&'static str),
    /// A catcher covers it, so the execution carries on down that branch and ends
    /// `SUCCEEDED` holding the error output the catcher was handed.
    Caught(&'static str),
}

/// One row: the case, the definition it runs, the IAM action the denied call asks for,
/// and what the execution settles on.
type Row = (&'static str, Value, &'static str, Settles);

/// The IAM actions the two invoked operations publish, which is what the refusal names:
/// the operation's own name under DynamoDB's action namespace.
const PUT_ITEM_ACTION: &str = "dynamodb:PutItem";
/// See [`PUT_ITEM_ACTION`].
const GET_ITEM_ACTION: &str = "dynamodb:GetItem";

/// Every row of the table.
fn rows() -> Vec<Row> {
    let optimized = "arn:aws:states:::dynamodb:putItem";
    let sdk = "arn:aws:states:::aws-sdk:dynamodb:putItem";
    let read = "arn:aws:states:::dynamodb:getItem";
    vec![
        (
            "a denied write on its own, optimized",
            json!({
                "StartAt": "Write",
                "States": { "Write": merge(denied_task(optimized, "row-0"), json!({ "End": true })) }
            }),
            PUT_ITEM_ACTION,
            Settles::Failed(REFUSAL_ERROR),
        ),
        (
            "the same denied write through the AWS SDK integration",
            json!({
                "StartAt": "Write",
                "States": { "Write": merge(denied_task(sdk, "row-1"), json!({ "End": true })) }
            }),
            PUT_ITEM_ACTION,
            Settles::Failed(SDK_REFUSAL_ERROR),
        ),
        (
            "a denied read, which settles the same way a denied write does",
            json!({
                "StartAt": "Read",
                "States": { "Read": merge(denied_task(read, "row-2"), json!({ "End": true })) }
            }),
            GET_ITEM_ACTION,
            Settles::Failed(REFUSAL_ERROR),
        ),
        (
            "a denied write behind a Retry whose wildcard names the failure",
            json!({
                "StartAt": "Write",
                "States": {
                    "Write": merge(
                        denied_task(optimized, "row-3"),
                        json!({
                            "Retry": [{
                                "ErrorEquals": [TASK_FAILED_WILDCARD],
                                "IntervalSeconds": 1,
                                "MaxAttempts": 2,
                                "BackoffRate": 1
                            }],
                            "End": true
                        }),
                    )
                }
            }),
            PUT_ITEM_ACTION,
            Settles::Failed(REFUSAL_ERROR),
        ),
        (
            "a denied write a catch-all catcher covers",
            json!({
                "StartAt": "Write",
                "States": {
                    "Write": merge(
                        denied_task(optimized, "row-4"),
                        json!({
                            "Catch": [{
                                "ErrorEquals": [ALL_ERRORS_WILDCARD],
                                "Next": "Recovered"
                            }],
                            "End": true
                        }),
                    ),
                    "Recovered": { "Type": "Pass", "End": true }
                }
            }),
            PUT_ITEM_ACTION,
            Settles::Caught(REFUSAL_ERROR),
        ),
        (
            "a denied write in one branch of a Parallel",
            json!({
                "StartAt": "Both",
                "States": {
                    "Both": {
                        "Type": "Parallel",
                        "Branches": [
                            {
                                "StartAt": "Write",
                                "States": {
                                    "Write": merge(
                                        denied_task(optimized, "row-5"),
                                        json!({ "End": true }),
                                    )
                                }
                            },
                            {
                                "StartAt": "Idle",
                                "States": { "Idle": { "Type": "Pass", "End": true } }
                            }
                        ],
                        "End": true
                    }
                }
            }),
            PUT_ITEM_ACTION,
            Settles::Failed(REFUSAL_ERROR),
        ),
        (
            "a denied write after a state that already ran",
            json!({
                "StartAt": "Prepare",
                "States": {
                    "Prepare": { "Type": "Pass", "Next": "Write" },
                    "Write": merge(denied_task(optimized, "row-6"), json!({ "Next": "Never" })),
                    "Never": { "Type": "Pass", "End": true }
                }
            }),
            PUT_ITEM_ACTION,
            Settles::Failed(REFUSAL_ERROR),
        ),
    ]
}

/// `base` with every member of `extra` written beside its own, which is how a row adds
/// the fields that wrap its denied `Task`.
fn merge(base: Value, extra: Value) -> Value {
    let mut merged = base;
    let object = merged.as_object_mut().expect("a state is a JSON object");
    for (name, value) in extra
        .as_object()
        .expect("the fields to add are a JSON object")
    {
        object.insert(name.clone(), value.clone());
    }
    merged
}

/// A `Task` whose integration call the execution role may not make settles the
/// execution: the denial is reported as that state's failure, so a client polling
/// `DescribeExecution` reaches a terminal status and the history closes on a terminal
/// event.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_denied_task_settles_the_execution_instead_of_leaving_it_running() {
    let chain = Chain::new();
    let table_arn = chain.given_table(TABLE).await;
    // A role that may do something, and nothing at all on DynamoDB: every row's call is
    // withheld because no statement is about it.
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": "logs:PutLogEvents", "Resource": "*" }]
    });
    let role_arn = chain.role("SettlingTaskRole", &policy).await;

    for (index, (case, definition, action, settles)) in rows().into_iter().enumerate() {
        let execution = chain
            .started(&format!("settles-{index}"), &definition, &role_arn)
            .await;
        let described = chain.poll_until_terminal(case, &execution).await;
        let events = chain.history(&execution).await;
        let recorded = types(&events);

        // Whatever wraps the denied state, the execution reached a terminal status and
        // reports when it stopped — the two things a client polling it is waiting for.
        let expected_status = match settles {
            Settles::Failed(_) => "FAILED",
            Settles::Caught(_) => "SUCCEEDED",
        };
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some(expected_status),
            "case {case:?}: expected {expected_status}, got {described:?}"
        );
        assert!(
            described
                .get("stopDate")
                .is_some_and(|stopped| !stopped.is_null()),
            "case {case:?}: expected a settled execution to report a stopDate, got \
             {described:?}"
        );
        assert!(
            chain.running_executions(&execution).await.is_empty(),
            "case {case:?}: expected no RUNNING execution to be left, got {described:?}"
        );

        // The denial was reported as the state's FAILURE, not as a call that never
        // answered: the task failure is in the history and nothing was submitted or
        // succeeded.
        assert!(
            recorded.contains(&EVENT_TASK_FAILED.to_string()),
            "case {case:?}: expected a {EVENT_TASK_FAILED} for the call the role may not \
             make, got {recorded:?}"
        );
        for absent in [EVENT_TASK_SUBMITTED, EVENT_TASK_SUCCEEDED] {
            assert!(
                !recorded.contains(&absent.to_string()),
                "case {case:?}: nothing was called, so no {absent} may appear, got {recorded:?}"
            );
        }

        // The failure the task reported names the denial: the action the invoked
        // operation publishes, the ARN of the table its request named, and the code the
        // refused answer came back under.
        let failed = events
            .iter()
            .find(|event| event.get("type").and_then(Value::as_str) == Some(EVENT_TASK_FAILED))
            .and_then(|event| event.get("taskFailedEventDetails"))
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected a task failure in {events:?}"));
        let reported = match settles {
            Settles::Failed(error) | Settles::Caught(error) => error,
        };
        assert_eq!(
            failed.get("error").and_then(Value::as_str),
            Some(reported),
            "case {case:?}: expected the task failure to report {reported:?}, got {failed:?}"
        );
        let cause = failed
            .get("cause")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected a cause on {failed:?}"));
        let mut fragments = vec![
            NOT_AUTHORIZED,
            action,
            table_arn.as_str(),
            REFUSAL_STATUS_FIELD,
        ];
        if reported == REFUSAL_ERROR {
            fragments.push(REFUSAL_CODE_FIELD);
        }
        for fragment in fragments {
            assert!(
                cause.contains(fragment),
                "case {case:?}: expected the cause to carry {fragment:?}, got {cause:?}"
            );
        }

        // A denied state hands nothing on down its success path, so the state one row
        // names after it is never entered.
        assert!(
            !entered(&events).contains(&"Never".to_string()),
            "case {case:?}: expected the state after the denied one never to be entered, got \
             {events:?}"
        );

        // And the terminal event the history closes on is the one that status belongs
        // to, carrying the same failure where the execution itself failed.
        let last = recorded
            .last()
            .unwrap_or_else(|| panic!("case {case:?}: expected events, got none"));
        match settles {
            Settles::Failed(error) => {
                assert_eq!(
                    last.as_str(),
                    EVENT_EXECUTION_FAILED,
                    "case {case:?}: expected the history to close on {EVENT_EXECUTION_FAILED}, \
                     got {recorded:?}"
                );
                assert_eq!(
                    described.get("error").and_then(Value::as_str),
                    Some(error),
                    "case {case:?}: expected the execution to fail under {error:?}, got \
                     {described:?}"
                );
            }
            Settles::Caught(error) => {
                assert_eq!(
                    last.as_str(),
                    EVENT_EXECUTION_SUCCEEDED,
                    "case {case:?}: expected the history to close on \
                     {EVENT_EXECUTION_SUCCEEDED}, got {recorded:?}"
                );
                // The catcher handed its branch the `{Error, Cause}` error output, which
                // is what the branch's own `Pass` carried to the end.
                let output: Value = serde_json::from_str(&string(&described, "output"))
                    .expect("the output of a settled execution is JSON");
                assert_eq!(
                    output.get("Error").and_then(Value::as_str),
                    Some(error),
                    "case {case:?}: expected the caught error output, got {output:?}"
                );
            }
        }

        // Nothing the role was denied reached the table.
        assert_eq!(
            chain.item(&format!("row-{index}")).await,
            None,
            "case {case:?}: expected the refused write to have left the table untouched"
        );
    }
}

/// The retried row's attempts are each recorded, and the execution still settles.
///
/// The way a denied call is retried like any other task failure: a retrier that names
/// the failure makes the state try again, so the history carries one task failure per
/// attempt — the initial one plus `MaxAttempts` — and the execution ends on the last of
/// them rather than waiting for an answer that never comes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retried_denied_task_records_every_attempt_and_still_settles() {
    const MAX_ATTEMPTS: u64 = 2;

    let chain = Chain::new();
    chain.given_table(TABLE).await;
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": "logs:PutLogEvents", "Resource": "*" }]
    });
    let role_arn = chain.role("RetriedTaskRole", &policy).await;
    let definition = json!({
        "StartAt": "Write",
        "States": {
            "Write": merge(
                denied_task("arn:aws:states:::dynamodb:putItem", "retried"),
                json!({
                    "Retry": [{
                        "ErrorEquals": [TASK_FAILED_WILDCARD],
                        "IntervalSeconds": 1,
                        "MaxAttempts": MAX_ATTEMPTS,
                        "BackoffRate": 1
                    }],
                    "End": true
                }),
            )
        }
    });

    let case = "a retrier that names the denial";
    let execution = chain.started("settles-retried", &definition, &role_arn).await;
    let described = chain.poll_until_terminal(case, &execution).await;
    assert_eq!(
        described.get("status").and_then(Value::as_str),
        Some("FAILED"),
        "case {case:?}: expected FAILED once the retries are spent, got {described:?}"
    );
    let events = chain.history(&execution).await;
    let failures = types(&events)
        .iter()
        .filter(|event_type| *event_type == EVENT_TASK_FAILED)
        .count();
    let expected = usize::try_from(MAX_ATTEMPTS).expect("the count fits") + 1;
    assert_eq!(
        failures, expected,
        "case {case:?}: expected one task failure per attempt, got {failures} in \
         {:?}",
        types(&events)
    );
    // The state was entered once and never left for a success path.
    assert_eq!(
        entered(&events),
        vec!["Write".to_string()],
        "case {case:?}: expected only the denied state to be entered, got {events:?}"
    );
}
