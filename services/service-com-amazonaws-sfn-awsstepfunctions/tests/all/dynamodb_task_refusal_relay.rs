// SPDX-License-Identifier: CC0-1.0

//! What a `Task` state reports when DYNAMODB ITSELF refuses the call the state made,
//! driven over both services' own wire protocols through the production
//! `MultiServiceRouter` with Step Functions and DynamoDB mounted on ONE engine — the
//! shape `core/router/src/lib.rs` mounts them in — so the refusal the state relays is
//! the one DynamoDB's own handler answered and the far-end effect is read back through
//! DynamoDB's own API.
//!
//! The rule under test: the `Cause` a failed `Task` carries is the invoked service's
//! exception as the client the `Resource`'s integration FAMILY is served by renders it —
//! that service's own message, then the metadata the answer carried — so the message,
//! the status code, the error code and the id of the refused request are taken from the
//! invoked service's response and every separator and token around them is fixed text:
//! an optimized `Resource` is served by the earlier generation of client, whose
//! SEMICOLON-separated group names its own client, the status, the error code, the
//! request and the proxy the call went through (there is none, so `Proxy: null`), while
//! an `arn:aws:states:::aws-sdk:<service>:<action>` `Resource` is served by the current
//! AWS SDK, whose COMMA-separated group names the service as that SDK names it, the
//! status and the request, followed by how many attempts the call took
//! (`crate::task_execution_role::refusal_metadata` states the two renderings). A refused
//! call changes nothing, so the item it addressed is left exactly as it was.
//!
//! Every row reads the message, the error code and the status out of the SAME call made
//! DIRECTLY to DynamoDB, so what the relayed cause is held to is that service's own
//! answer rather than a sentence restated here. The id of the refused request is the
//! value of that one call, so it is masked out of the comparison
//! (`crate::task_execution_role::masked`) and held instead to the FORM the invoked
//! service names its own requests in, read off that direct answer: a client that masks
//! or parses one service's request ids recognises the relayed one only in that form.
//!
//! The rows differ along what the rule generalises over: the integration FAMILY, the
//! ACTION refused (a delete, a write, an update and a read), the refusal CLASS (a
//! condition that cannot hold, and a table the account does not hold) and therefore the
//! sentence the cause opens with, and whether a `Catch` routes the failure or the
//! execution ends with it.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_sfn_awsstepfunctions::EmulatedService;

use crate::state_machine_lifecycle::{error_code, string, REGION, ROLE_ARN};
use crate::task_execution_role::{masked, refusal_metadata};

/// How each service's requests are addressed on the wire, from each model's own
/// `service_info` (service shape, `sigv4Name` and the `awsJson` version its protocol
/// declares).
const SFN_SIGNING_NAME: &str = "states";
const SFN_TARGET_PREFIX: &str = "AWSStepFunctions";
const DYNAMODB_SIGNING_NAME: &str = "dynamodb";
const DYNAMODB_TARGET_PREFIX: &str = "DynamoDB_20120810";

/// The name the client each family is served by gives DynamoDB, which is what its
/// rendering of an exception reports the service under: the earlier client's own name,
/// and the name the current AWS SDK's `DynamoDb` client carries.
const DYNAMODB_CLIENT: &str = "AmazonDynamoDBv2";
/// See [`DYNAMODB_CLIENT`].
const DYNAMODB_SDK_NAME: &str = "DynamoDb";

/// How long a condition is waited on before it is called never: generous, because what
/// is waited for is a condition and not a span of time.
const PATIENCE: Duration = Duration::from_secs(60);

/// How long between two looks at a condition that is not there yet.
const BETWEEN_LOOKS: Duration = Duration::from_millis(25);

/// The attribute every seeded item carries beside its key, so a row can show that a
/// refused call left the item as it was.
const SEEDED: &str = "seeded";

/// Where a response names the request it answered, which is where the id the cause
/// relays is read from. Lower-cased because `http` matches a header name that way.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The field the relayed metadata names the refused request in, from which the id the
/// cause carries is read back.
const NAMES_THE_REQUEST: &str = "Request ID: ";

/// Step Functions and DynamoDB over ONE engine, so the call a task makes is served by
/// the very store this suite's tables and items were created in.
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
            service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// One `awsJson` call to one service: the status, the id the answer named the
    /// request by, and the document it answered. The `Authorization` header's
    /// credential scope is what routes the request and names the Region its ARNs carry.
    async fn call(
        &self,
        signing_name: &str,
        target_prefix: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Option<String>, Value) {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
             aws4_request, SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", &host)
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .header("authorization", authorization)
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let named = response
            .headers()
            .get(REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let parsed = match bytes.is_empty() {
            true => Value::Null,
            false => serde_json::from_slice(&bytes).unwrap_or_else(|why| {
                panic!(
                    "case {operation}: expected a JSON body, got {why} for {:?}",
                    String::from_utf8_lossy(&bytes)
                )
            }),
        };
        (status, named, parsed)
    }

    /// Call one Step Functions operation, requiring a 200.
    async fn sfn(&self, operation: &str, body: &Value) -> Value {
        let (status, _named, parsed) = self
            .call(SFN_SIGNING_NAME, SFN_TARGET_PREFIX, operation, body)
            .await;
        assert_eq!(
            status, 200,
            "case {operation}: expected 200, got {status} with body {parsed}"
        );
        parsed
    }

    /// Call one DynamoDB operation, requiring a 200.
    async fn ddb(&self, operation: &str, body: &Value) -> Value {
        let (status, _named, parsed) = self
            .call(
                DYNAMODB_SIGNING_NAME,
                DYNAMODB_TARGET_PREFIX,
                operation,
                body,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {operation}: expected 200, got {status} with body {parsed}"
        );
        parsed
    }

    /// Make the SAME call the task will make directly to DynamoDB, requiring that
    /// DynamoDB refuses it, and report what that refusal carried.
    async fn ddb_refusal(&self, case: &str, operation: &str, body: &Value) -> Refusal {
        let (status, named, parsed) = self
            .call(
                DYNAMODB_SIGNING_NAME,
                DYNAMODB_TARGET_PREFIX,
                operation,
                body,
            )
            .await;
        assert_ne!(
            status, 200,
            "case {case:?}: expected DynamoDB to refuse the {operation} call, got 200 with \
             {parsed}"
        );
        let message = ["message", "Message"]
            .iter()
            .find_map(|member| parsed.get(*member).and_then(Value::as_str))
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a message in the refusal {parsed}")
            })
            .to_string();
        let request_id = named.unwrap_or_else(|| {
            panic!("case {case:?}: expected DynamoDB to name the refused request, got {parsed}")
        });
        Refusal {
            status,
            code: error_code(&parsed),
            message,
            request_id,
        }
    }

    /// Create a table keyed by a single string partition key `pk` and wait until it
    /// serves items: "You can perform read and write operations only on an ACTIVE
    /// table", so what is waited for is that status and not a span of time.
    async fn table(&self, name: &str) {
        self.ddb(
            "CreateTable",
            &json!({
                "TableName": name,
                "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
                "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
                "BillingMode": "PAY_PER_REQUEST"
            }),
        )
        .await;
        let began = Instant::now();
        loop {
            let described = self.ddb("DescribeTable", &json!({ "TableName": name })).await;
            if described["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
                return;
            }
            assert!(
                began.elapsed() < PATIENCE,
                "case {name:?}: expected the table to serve items within {PATIENCE:?}, got \
                 {described}"
            );
            ::tokio::time::sleep(BETWEEN_LOOKS).await;
        }
    }

    /// Store one item under `pk`, so a row can show whether the refused call changed it.
    async fn seed(&self, table: &str, pk: &str) {
        self.ddb(
            "PutItem",
            &json!({
                "TableName": table,
                "Item": { "pk": { "S": pk }, "status": { "S": SEEDED } }
            }),
        )
        .await;
    }

    /// The item stored under `pk`, or `None` where the table holds none.
    async fn item(&self, table: &str, pk: &str) -> Option<Value> {
        let read = self
            .ddb(
                "GetItem",
                &json!({ "TableName": table, "Key": { "pk": { "S": pk } } }),
            )
            .await;
        read.get("Item").cloned()
    }

    /// Create the state machine `name` running `definition`, start one execution of it,
    /// and report `(the execution ARN, what it ended as)` once it has left `RUNNING`.
    async fn ran(&self, name: &str, definition: &Value, case: &str) -> (String, Value) {
        let created = self
            .sfn(
                "CreateStateMachine",
                &json!({
                    "name": name,
                    "definition": definition.to_string(),
                    "roleArn": ROLE_ARN
                }),
            )
            .await;
        let machine = string(&created, "stateMachineArn");
        let started = self
            .sfn(
                "StartExecution",
                &json!({ "stateMachineArn": machine, "name": "only" }),
            )
            .await;
        let execution_arn = string(&started, "executionArn");
        let began = Instant::now();
        loop {
            let described = self
                .sfn("DescribeExecution", &json!({ "executionArn": execution_arn }))
                .await;
            if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
                return (execution_arn, described);
            }
            assert!(
                began.elapsed() < PATIENCE,
                "case {case:?}: expected a terminal status within {PATIENCE:?}, got {described}"
            );
            ::tokio::time::sleep(BETWEEN_LOOKS).await;
        }
    }

    /// The `taskFailedEventDetails` of every `TaskFailed` event one execution wrote.
    async fn task_failures(&self, execution_arn: &str) -> Vec<Value> {
        let answered = self
            .sfn(
                "GetExecutionHistory",
                &json!({ "executionArn": execution_arn, "maxResults": 100 }),
            )
            .await;
        answered
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|event| event.get("taskFailedEventDetails").cloned())
            .collect()
    }
}

/// What DynamoDB's own answer to a refused call carried: the facts the relayed cause's
/// metadata is built from, beside the sentence it opens with.
#[derive(Debug)]
struct Refusal {
    status: u16,
    code: String,
    message: String,
    /// The id that answer named ITS request by — a value of that one call's own, so what
    /// a row holds the relayed id to is its FORM ([`same_form`]).
    request_id: String,
}

/// A one-`Task` definition naming `resource` with `parameters`, carrying a `Catch` that
/// keeps what failed when `caught` and no handler otherwise.
fn definition(resource: &str, parameters: Value, caught: bool) -> Value {
    let mut states = json!({
        "Work": {
            "Type": "Task",
            "Resource": resource,
            "Parameters": parameters,
            "End": true
        }
    });
    if caught {
        states["Work"]["Catch"] = json!([{
            "ErrorEquals": ["States.ALL"],
            "Next": "Caught",
            "ResultPath": "$.errorInfo"
        }]);
        states["Caught"] = json!({ "Type": "Pass", "End": true });
    }
    json!({ "StartAt": "Work", "States": states })
}

/// The `Error` and `Cause` the failure reached its reader as: the execution's own
/// members where nothing handled it, and the `Catch` path's output where one did.
fn reported(case: &str, described: &Value) -> (String, String) {
    if described.get("status").and_then(Value::as_str) == Some("FAILED") {
        return (string(described, "error"), string(described, "cause"));
    }
    let output: Value = described
        .get("output")
        .and_then(Value::as_str)
        .map(|text| serde_json::from_str(text).expect("the output is JSON"))
        .unwrap_or_else(|| panic!("case {case:?}: expected an output in {described}"));
    let caught = output
        .get("errorInfo")
        .unwrap_or_else(|| panic!("case {case:?}: expected the Catch output in {output}"));
    (string(caught, "Error"), string(caught, "Cause"))
}

/// The id the relayed metadata names the refused request by, read back out of `cause`:
/// the field runs up to whichever delimiter closes it.
fn relayed_request_id(case: &str, cause: &str) -> String {
    let (_, named) = cause.split_once(NAMES_THE_REQUEST).unwrap_or_else(|| {
        panic!("case {case:?}: expected the cause to name the refused request, got {cause:?}")
    });
    named
        .chars()
        .take_while(|character| !";)".contains(*character))
        .collect()
}

/// Which CLASS of character a request id is written with at one position.
///
/// An AWS request id is opaque to a client, so no particular value is part of the text:
/// what a client masking one service's ids recognises is the WIDTH and the classes it is
/// drawn from, which is what a row compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    Uppercase,
    Lowercase,
    Digit,
    Other,
}

/// The class of one character ([`Class`]).
fn class(character: char) -> Class {
    match character {
        _ if character.is_ascii_uppercase() => Class::Uppercase,
        _ if character.is_ascii_lowercase() => Class::Lowercase,
        _ if character.is_ascii_digit() => Class::Digit,
        _ => Class::Other,
    }
}

/// Whether `relayed` is spelled in the same FORM as `answered`, the id the invoked
/// service named its OWN answer's request by: the same width, and no character of a
/// class that id does not use.
fn same_form(answered: &str, relayed: &str) -> bool {
    let used: BTreeSet<Class> = answered.chars().map(class).collect();
    answered.chars().count() == relayed.chars().count()
        && relayed
            .chars()
            .all(|character| used.contains(&class(character)))
}

/// The request the row's action is refused by, as the `Parameters` the state carries and
/// the body the direct call is made with — one document, so the two calls are the same
/// call.
///
/// The members are DynamoDB's own, from
/// `smithy_query(query="operation",operation="PutItem,GetItem,UpdateItem,DeleteItem",
/// model_path="services/service-com-amazonaws-dynamodb-dynamodb-20120810/smithy-model/
/// com-amazonaws-dynamodb-dynamodb-20120810.json")`: `TableName` and `Item` for a write,
/// `TableName` and `Key` for the other three, `UpdateExpression` beside them for an
/// update, and `ConditionExpression` with its substitution maps on all but the read.
fn request(operation: &str, table: &str, pk: &str) -> Value {
    let key = json!({ "pk": { "S": pk } });
    match operation {
        // The item IS there, so a write conditioned on its absence cannot land.
        "PutItem" => json!({
            "TableName": table,
            "Item": { "pk": { "S": pk }, "status": { "S": "rewritten" } },
            "ConditionExpression": "attribute_not_exists(pk)"
        }),
        // `status` is one of DynamoDB's reserved words, so both expressions name it
        // through a substitution.
        "UpdateItem" => json!({
            "TableName": table,
            "Key": key,
            "UpdateExpression": "SET #s = :new",
            "ExpressionAttributeNames": { "#s": "status" },
            "ExpressionAttributeValues": {
                ":new": { "S": "updated" },
                ":expected": { "S": "never" }
            },
            "ConditionExpression": "#s = :expected"
        }),
        // A read, which carries no condition: it is refused for the table it names.
        "GetItem" => json!({ "TableName": table, "Key": key }),
        _ => json!({
            "TableName": table,
            "Key": key,
            "ConditionExpression": "attribute_not_exists(pk)"
        }),
    }
}

/// Which table a row addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Table {
    /// The table that was created, holding the item the call addresses.
    Held,
    /// A table nobody created, which is a refusal of another class.
    Absent,
}

/// One row: the call DynamoDB refuses, the `Resource` the task names it by, and the
/// error name the task must report it under.
struct Row {
    case: &'static str,
    /// The state machine's name for the row, which has to be unique in the account.
    machine: &'static str,
    resource: &'static str,
    /// The DynamoDB operation the `Resource`'s action names, which the direct call is
    /// made on.
    operation: &'static str,
    table: Table,
    error: &'static str,
    /// Whether the state carries a `Catch`, which is where a handled failure's `Cause`
    /// is read from.
    caught: bool,
}

/// The table every row's items live in, and the one no row created.
const HELD_TABLE: &str = "ledger";
/// See [`HELD_TABLE`].
const ABSENT_TABLE: &str = "no-such-ledger";

/// RULE: the cause a failed `Task` reports for a call the invoked service refused is
/// that service's exception as the client the `Resource`'s integration FAMILY is served
/// by renders it — the service's own message, then the metadata the answer carried, with
/// the status code, the error code and the request id taken from that answer and every
/// separator and token around them fixed — and the call having been refused, what it
/// addressed is untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_dynamodb_call_is_relayed_as_its_clients_exception_message() {
    let chain = Chain::new();
    chain.table(HELD_TABLE).await;

    let rows: &[Row] = &[
        Row {
            // The recorded case: a delete the condition refuses, caught by the state.
            case: "a delete the condition cannot satisfy, routed by a Catch",
            machine: "ddb-cause-delete-caught",
            resource: "arn:aws:states:::dynamodb:deleteItem",
            operation: "DeleteItem",
            table: Table::Held,
            error: "DynamoDB.ConditionalCheckFailedException",
            caught: true,
        },
        Row {
            // The same refusal of the same action, reached through the other family, so
            // the other client renders it.
            case: "the same delete through the AWS SDK integration, failing the execution",
            machine: "ddb-cause-delete-sdk",
            resource: "arn:aws:states:::aws-sdk:dynamodb:deleteItem",
            operation: "DeleteItem",
            table: Table::Held,
            error: "DynamoDb.ConditionalCheckFailedException",
            caught: false,
        },
        Row {
            case: "a write conditioned on the item's absence",
            machine: "ddb-cause-put",
            resource: "arn:aws:states:::dynamodb:putItem",
            operation: "PutItem",
            table: Table::Held,
            error: "DynamoDB.ConditionalCheckFailedException",
            caught: false,
        },
        Row {
            case: "an update whose condition names another value, routed by a Catch",
            machine: "ddb-cause-update",
            resource: "arn:aws:states:::dynamodb:updateItem",
            operation: "UpdateItem",
            table: Table::Held,
            error: "DynamoDB.ConditionalCheckFailedException",
            caught: true,
        },
        Row {
            // Another refusal class of the same service, whose sentence is its own and
            // not the condition's: the table the call names is not there.
            case: "a delete on a table the account does not hold",
            machine: "ddb-cause-absent",
            resource: "arn:aws:states:::dynamodb:deleteItem",
            operation: "DeleteItem",
            table: Table::Absent,
            error: "DynamoDB.ResourceNotFoundException",
            caught: true,
        },
        Row {
            case: "a read on that same table through the AWS SDK integration",
            machine: "ddb-cause-absent-sdk",
            resource: "arn:aws:states:::aws-sdk:dynamodb:getItem",
            operation: "GetItem",
            table: Table::Absent,
            error: "DynamoDb.ResourceNotFoundException",
            caught: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let Row {
            case,
            machine,
            resource,
            operation,
            table,
            error,
            caught,
        } = *row;
        let pk = format!("row-{index}");
        let named = match table {
            Table::Held => HELD_TABLE,
            Table::Absent => ABSENT_TABLE,
        };
        if table == Table::Held {
            chain.seed(HELD_TABLE, &pk).await;
        }
        let parameters = request(operation, named, &pk);

        // What DynamoDB itself answers this very call, which is what the relayed cause
        // has to be built from.
        let refusal = chain.ddb_refusal(case, operation, &parameters).await;

        let (execution_arn, described) = chain
            .ran(machine, &definition(resource, parameters, caught), case)
            .await;
        let settled = match caught {
            true => "SUCCEEDED",
            false => "FAILED",
        };
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some(settled),
            "case {case:?}: expected {settled}, got {described}"
        );
        let (reported_error, cause) = reported(case, &described);
        assert_eq!(
            reported_error, error,
            "case {case:?}: expected the error {error:?}, got {reported_error:?} with cause \
             {cause:?}"
        );

        // The cause: the service's own sentence, then the metadata its client writes for
        // the answer that carried it.
        let service = match resource.contains(":aws-sdk:") {
            true => DYNAMODB_SDK_NAME,
            false => DYNAMODB_CLIENT,
        };
        let expected = format!(
            "{}{}",
            refusal.message,
            refusal_metadata(resource, service, &refusal.code, refusal.status)
        );
        assert_eq!(
            masked(&cause),
            expected,
            "case {case:?}: expected the exception message {expected:?}, got {:?}",
            masked(&cause)
        );
        // The id in it is that one call's own value, so what is held is that the cause
        // really named a request, and spelled it the way the invoked service spells the
        // ids it names its own requests by.
        let relayed = relayed_request_id(case, &cause);
        assert!(
            same_form(&refusal.request_id, &relayed),
            "case {case:?}: expected a request id in the form DynamoDB names its own requests \
             in (like {:?}), got {relayed:?}",
            refusal.request_id
        );
        // And that form is DynamoDB's own: every sample response in the DynamoDB API
        // reference names the request by one unbroken run of uppercase letters and
        // digits, so a hyphenated or lower-cased token is one no client masking this
        // service's ids would recognise in the relayed sentence.
        assert!(
            !relayed.is_empty()
                && relayed
                    .chars()
                    .all(|character| matches!(class(character), Class::Uppercase | Class::Digit)),
            "case {case:?}: expected the relayed request id to carry uppercase letters and \
             digits alone, got {relayed:?}"
        );

        // The same error and cause reached the execution's history, which is where a
        // Catch path's reader and an operator read them.
        let failures = chain.task_failures(&execution_arn).await;
        let detail = match failures.as_slice() {
            [detail] => detail.clone(),
            _ => panic!("case {case:?}: expected one TaskFailed event, got {failures:?}"),
        };
        assert_eq!(
            (
                detail.get("error").and_then(Value::as_str),
                detail.get("cause").and_then(Value::as_str).map(masked),
            ),
            (Some(error), Some(expected.clone())),
            "case {case:?}: expected the history to carry the same failure, got {detail}"
        );

        // And the refused call changed nothing.
        if table == Table::Held {
            assert_eq!(
                chain.item(HELD_TABLE, &pk).await,
                Some(json!({ "pk": { "S": pk }, "status": { "S": SEEDED } })),
                "case {case:?}: expected the refused call to leave the item untouched"
            );
        }
    }
}
