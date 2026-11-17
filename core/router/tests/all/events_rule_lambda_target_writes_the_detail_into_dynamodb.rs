// SPDX-License-Identifier: CC0-1.0

//! The most ordinary serverless wiring there is, end to end over the real assembled
//! `MultiServiceRouter`: an event is PUBLISHED onto an event bus, the rule matching it hands
//! the entry to its Lambda function target, that function RUNS and writes what the event
//! carried into Amazon DynamoDB, and a later read of the table reports it.
//!
//! The rule pinned here: **every entry a publication ACCEPTS (`FailedEntryCount` 0 with one
//! `EventId` per entry, which is all `PutEvents` models) is matched against each enabled rule
//! on the bus it was published onto, and every target of a matching rule whose own
//! resource-based policy admits `events.amazonaws.com` is handed that entry ONCE as the event
//! envelope — `source` and `detail-type` as submitted, `detail` as the submitted JSON document,
//! and `id` the `EventId` the publication answered — so a function target's own run really
//! happens and the writes its handler makes are visible to any later reader; an entry no
//! enabled rule's pattern matches is handed to nothing.** The bus an entry naming none is
//! published onto is the default one, which is the bus a `put-events` call that names no bus
//! reaches.
//!
//! Six crates have to agree for that to hold (EventBridge matches the entry, evaluates the
//! target's grant and hands over; Lambda holds the grant and RUNS the deployment package;
//! CloudFormation archives the inline handler into that package; IAM answers for the execution
//! role the handler's own call is judged against; DynamoDB serves the table it writes; STS
//! vends the role's session), so the suite lives here rather than in any one of them. Its
//! siblings reach one hop each — [`crate::events_lambda_target_runs_the_handler`] pins that the
//! handover is queued and attempted, [`crate::events_lambda_target_resource_policy`] pins the
//! grant that decides it, and
//! [`crate::events_queue_target_nested_detail_reaches_its_lambda_consumer`] pins the same
//! payload fidelity through a QUEUE target and an event source mapping — and what only this
//! suite reaches is the DIRECT rule → function → table chain, read through DynamoDB's own
//! strongly consistent `GetItem`, which is how a client sees that the publication had an
//! effect at all.
//!
//! The rows differ along what the rule generalises over: which bus the entry is published onto
//! (the default one, a custom one with its own rule), how many entries one publication carries
//! (one, two), the JSON type of the value the detail carries (string, number, nested object,
//! array, text needing escapes), the `detail-type` published, and whether the entry's `source`
//! is one the rule's pattern names at all. The recorded case is the first row.
//!
//! Every observation is one a client makes: what `PutEvents` answered, and what the table holds
//! afterwards. Delivery is asserted ONCE per entry through a counter the handler adds to, so a
//! chain that hands the same entry over twice fails here too, and the counters are read again
//! after every row for the same reason.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for, and the Region the shared stack reader
/// ([`wait_for_stack_status`]) signs for.
const REGION: &str = "us-east-1";

/// The `StackStatus` the one stack this suite deploys has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `Source` the rules' patterns name. Every entry that must be delivered carries it.
const SOURCE: &str = "com.example.orders.pipeline";

/// A `Source` no rule here names, which is how an entry that must be delivered NOWHERE is
/// published.
const OTHER_SOURCE: &str = "com.example.orders.unmatched";

/// The names this suite's resources are created under.
const TABLE: &str = "put-events-landings";
const ROLE_NAME: &str = "put-events-writer-role";
const FUNCTION: &str = "put-events-writer";
const STACK: &str = "put-events-writer-stack";
const CUSTOM_BUS: &str = "put-events-custom-bus";
const DEFAULT_BUS_RULE: &str = "put-events-default-rule";
const CUSTOM_BUS_RULE: &str = "put-events-custom-rule";

/// The service principal EventBridge invokes a resource-policy-governed target as, and the
/// action an invocation of a function performs ("for Lambda functions, Amazon SNS topics and
/// Amazon SQS queues, EventBridge relies on resource-based policies", `PutTargets`).
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// How long the function is allowed to run, as `Timeout` carries it.
const FUNCTION_TIMEOUT: i64 = 10;

/// How long an observation of work the CLOCK owes — the table coming up, the stack settling,
/// the function going `Active`, a delivery and the write it makes — waits before giving up, and
/// how long between two looks.
const BUDGET: Duration = Duration::from_secs(45);
/// See [`BUDGET`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The handler the stack deploys: for the one event it is handed it writes the `detail`'s own
/// `value` into the table under the `detail`'s `id`, beside the envelope members the delivery
/// carried, and ADDS to a counter, so how many times this entry was handed over is readable
/// too.
const WRITER_SOURCE: &str = r##"
import json
import os

import boto3

CLIENT = boto3.client("dynamodb")


def handler(event, context):
    detail = event["detail"]
    CLIENT.update_item(
        TableName=os.environ["TABLE"],
        Key={"pk": {"S": detail["id"]}},
        UpdateExpression="SET #p = :p, #s = :s, #d = :d, #e = :e ADD #n :one",
        ExpressionAttributeNames={
            "#p": "payload",
            "#s": "src",
            "#d": "dt",
            "#e": "eid",
            "#n": "deliveries",
        },
        ExpressionAttributeValues={
            ":p": {"S": json.dumps(detail["value"])},
            ":s": {"S": event["source"]},
            ":d": {"S": event["detail-type"]},
            ":e": {"S": event["id"]},
            ":one": {"N": "1"},
        },
    )
    return {"stored": detail["id"]}
"##;

/// The emulator as it is ASSEMBLED, on one engine: the handler's own SDK call is dispatched
/// back into this same router, so the table it writes is the table this suite reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: every call carries a credential scope so the
    // router can name the service it belongs to.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire, with whatever status it answered.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> (u16, Bytes) {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    (parts.status.as_u16(), body)
}

/// One `X-Amz-Target` RPC call, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let (status, answered) = dispatch(
        router,
        "POST",
        &format!("{signing_name}.{REGION}.amazonaws.com"),
        "/",
        &[
            (
                "content-type",
                &format!("application/x-amz-json-{json_version}"),
            ),
            ("x-amz-target", &format!("{target_prefix}.{operation}")),
            ("authorization", &authorization(signing_name)),
        ],
        Bytes::from(body.to_string()),
    )
    .await;
    let parsed: Value = if answered.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&answered)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&answered).to_string()))
    };
    (status, parsed)
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One Amazon DynamoDB call (`awsJson1_0`, `DynamoDB_20120810.<Operation>`) that must answer
/// 200.
async fn dynamodb(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(
        router,
        "dynamodb",
        "DynamoDB_20120810",
        "1.0",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DynamoDB {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One AWS Lambda call over its `restJson1` wire, with whatever status it answered.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let (status, answered) = dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::from(payload),
    )
    .await;
    let parsed: Value = if answered.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&answered)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&answered).to_string()))
    };
    (status, parsed)
}

/// One awsQuery call of `signing_name` at `host`, as its status and XML body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    host: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, answered) = dispatch(
        router,
        "POST",
        host,
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization(signing_name)),
        ],
        Bytes::from(body),
    )
    .await;
    (status, String::from_utf8_lossy(&answered).to_string())
}

/// One IAM call that must answer 200.
async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(
        router,
        "iam",
        "iam.amazonaws.com",
        "2010-05-08",
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from IAM {action}, got {status} with {body}"
    );
    body
}

/// One CloudFormation call that must answer 200.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(
        router,
        "cloudformation",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "2010-05-15",
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from CloudFormation {action}, got {status} with {body}"
    );
    body
}

/// The table the handler writes, with one string hash key; answers the ARN DynamoDB reports for
/// it.
async fn given_table(router: &MultiServiceRouter) -> String {
    let created = dynamodb(
        router,
        "setup",
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
            "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    created["TableDescription"]["TableArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateTable reports no TableArn, got {created}"))
        .to_string()
}

/// Wait until DynamoDB first reports the table `ACTIVE`: "you can perform read and write
/// operations only on an ACTIVE table".
async fn await_table_active(router: &MultiServiceRouter) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let described =
            dynamodb(router, "setup", "DescribeTable", &json!({"TableName": TABLE})).await;
        if described["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: DynamoDB never reported {TABLE} ACTIVE, last read {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The execution role the function runs as: trusted by Lambda, granting the one write its
/// handler makes. Answers its ARN as IAM reports it.
async fn given_role(router: &MultiServiceRouter, table_arn: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    let arn = element(&created, "Arn");
    let permissions = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "dynamodb:UpdateItem",
            "Resource": table_arn,
        }],
    })
    .to_string();
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", "write-the-landings"),
            ("PolicyDocument", &permissions),
        ],
    )
    .await;
    arn
}

/// Deploy the writer: a stack holding the one function, whose inline code CloudFormation
/// archives into the deployment package Lambda runs ("the cfnresponse module is available only
/// when you use the `ZipFile` property to write your function's source code" — inline code is
/// the form a template declares a runnable package in). Answers the function's ARN as Lambda
/// reports it.
async fn given_function(router: &MultiServiceRouter, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Writer": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": { "Variables": { "TABLE": TABLE } },
                    "Code": { "ZipFile": WRITER_SOURCE },
                },
            },
        },
    })
    .to_string();
    cloudformation(
        router,
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    let (reached, described) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: expected the writer's stack to reach {CREATE_COMPLETE}, got {described}"
    );
    await_function_active(router).await
}

/// Wait until Lambda first reports the function `Active` — the state an invocation needs — and
/// answer the `FunctionArn` it reports.
async fn await_function_active(router: &MultiServiceRouter) -> String {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            None,
        )
        .await;
        if status == 200 && configuration["State"].as_str() == Some("Active") {
            return configuration["FunctionArn"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("setup: the function reports no FunctionArn, got {configuration}")
                })
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "setup: Lambda never reported {FUNCTION} Active, last read {status} {configuration}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// An enabled rule matching [`SOURCE`] on the bus `bus` names (`None` is the default bus, the
/// one an entry naming no bus is published onto), with the function as its one target; answers
/// the rule's ARN.
async fn given_rule(
    router: &MultiServiceRouter,
    name: &str,
    bus: Option<&str>,
    function_arn: &str,
) -> String {
    let mut body = json!({
        "Name": name,
        "EventPattern": json!({ "source": [SOURCE] }).to_string(),
        "State": "ENABLED",
    });
    if let Some(bus) = bus {
        body["EventBusName"] = json!(bus);
    }
    let rule = events(router, "setup", "PutRule", &body).await;
    let rule_arn = rule["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: PutRule {name:?} reports no RuleArn, got {rule}"))
        .to_string();
    let mut targets = json!({
        "Rule": name,
        "Targets": [{ "Id": "writer", "Arn": function_arn }],
    });
    if let Some(bus) = bus {
        targets["EventBusName"] = json!(bus);
    }
    let wired = events(router, "setup", "PutTargets", &targets).await;
    assert_eq!(
        wired["FailedEntryCount"].as_i64(),
        Some(0),
        "setup: the function target of {name:?} must be accepted, got {wired}"
    );
    rule_arn
}

/// The statement on the FUNCTION that admits the invocation this rule makes: the service
/// principal, the invoking action, scoped by `aws:SourceArn` to the rule itself.
async fn allow_the_rule_to_invoke(
    router: &MultiServiceRouter,
    statement_id: &str,
    rule_arn: &str,
) {
    let (status, granted) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(&json!({
            "StatementId": statement_id,
            "Action": INVOKE_ACTION,
            "Principal": EVENTS_PRINCIPAL,
            "SourceArn": rule_arn,
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: expected HTTP 201 from AddPermission for {rule_arn}, got {status} with {granted}"
    );
}

/// The item the table holds under `key` right now, read through DynamoDB's own strongly
/// consistent read — `Value::Null` where no item is held.
async fn item(router: &MultiServiceRouter, case: &str, key: &str) -> Value {
    let read = dynamodb(
        router,
        case,
        "GetItem",
        &json!({
            "TableName": TABLE,
            "Key": { "pk": { "S": key } },
            "ConsistentRead": true,
        }),
    )
    .await;
    read.get("Item").cloned().unwrap_or(Value::Null)
}

/// The item the handler wrote under `key`, polled for as long as [`BUDGET`].
async fn await_item(router: &MultiServiceRouter, case: &str, key: &str) -> Value {
    let deadline = Instant::now() + BUDGET;
    loop {
        let held = item(router, case, key).await;
        if held.is_object() {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the rule's Lambda target never wrote the item {key:?} the published \
             detail names"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One entry of one publication: the `Source` it carries, the `DetailType` it is published
/// under, the `id` and `value` its `detail` carries, and whether a rule here matches it at all.
struct Entry {
    source: &'static str,
    detail_type: &'static str,
    id: &'static str,
    value: Value,
    delivered: bool,
}

/// One row: the bus the publication names (`None` is the default bus) and the entries it
/// carries.
struct Row {
    case: &'static str,
    bus: Option<&'static str>,
    entries: Vec<Entry>,
}

/// The rows: one per way a publication the rule has to carry through to the table differs — the
/// bus, the batch size, the JSON type of the value, the `detail-type`, and whether the entry's
/// source is matched at all. Every row that publishes an unmatched entry publishes a matched one
/// beside it, so the quiet half is read only once a delivery of the SAME publication has landed.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one entry on the default bus, as a publication naming no bus reaches it",
            bus: None,
            entries: vec![Entry {
                source: SOURCE,
                detail_type: "Order Placed",
                id: "order-default-bus",
                value: json!("ready-for-pickup"),
                delivered: true,
            }],
        },
        Row {
            case: "two entries in one publication, a number and a nested object",
            bus: None,
            entries: vec![
                Entry {
                    source: SOURCE,
                    detail_type: "Order Placed",
                    id: "order-batch-first",
                    value: json!(1_299.95),
                    delivered: true,
                },
                Entry {
                    source: SOURCE,
                    detail_type: "Order Amended",
                    id: "order-batch-second",
                    value: json!({ "customer": { "tier": "gold" }, "items": 3 }),
                    delivered: true,
                },
            ],
        },
        Row {
            case: "one entry on a custom bus whose own rule targets the same function",
            bus: Some(CUSTOM_BUS),
            entries: vec![Entry {
                source: SOURCE,
                detail_type: "Order Placed",
                id: "order-custom-bus",
                value: json!(["sku-alpha", "sku-beta"]),
                delivered: true,
            }],
        },
        Row {
            case: "an entry whose source no rule's pattern names, beside one every rule matches",
            bus: None,
            entries: vec![
                Entry {
                    source: OTHER_SOURCE,
                    detail_type: "Order Placed",
                    id: "order-unmatched",
                    value: json!("never-written"),
                    delivered: false,
                },
                Entry {
                    source: SOURCE,
                    detail_type: "Order Placed",
                    id: "order-matched-beside-it",
                    value: json!(true),
                    delivered: true,
                },
            ],
        },
        Row {
            case: "a value carrying quotes, backslashes, control characters and non-ASCII text",
            bus: None,
            entries: vec![Entry {
                source: SOURCE,
                detail_type: "Order Annotated",
                id: "order-escapes",
                value: json!("she said \"ship it\"\u{0009}a\\b café ☕ 日本語"),
                delivered: true,
            }],
        },
    ]
}

/// RULE: every accepted entry matching an enabled rule is handed to that rule's granted Lambda
/// target exactly once with the published envelope, so the handler's own write lands in the
/// table and a later read reports it; an unmatched entry is handed to nothing.
#[tokio::test]
async fn a_published_entry_reaches_its_rules_lambda_target_whose_write_lands_in_dynamodb() {
    let router = emulator();

    // The table the handler writes, the role it runs as, the function itself, the custom bus,
    // and one granted rule on each bus.
    let table_arn = given_table(&router).await;
    await_table_active(&router).await;
    let role_arn = given_role(&router, &table_arn).await;
    let function_arn = given_function(&router, &role_arn).await;
    events(&router, "setup", "CreateEventBus", &json!({ "Name": CUSTOM_BUS })).await;
    let default_rule_arn = given_rule(&router, DEFAULT_BUS_RULE, None, &function_arn).await;
    let custom_rule_arn =
        given_rule(&router, CUSTOM_BUS_RULE, Some(CUSTOM_BUS), &function_arn).await;
    allow_the_rule_to_invoke(&router, "events-default-bus", &default_rule_arn).await;
    allow_the_rule_to_invoke(&router, "events-custom-bus", &custom_rule_arn).await;

    // Every key a row claims a delivery for, so the counters can be read again at the end.
    let mut written: Vec<(&'static str, &'static str, Value)> = Vec::new();
    let mut unwritten: Vec<(&'static str, &'static str)> = Vec::new();

    for row in rows() {
        let case = row.case;
        let entries: Vec<Value> = row
            .entries
            .iter()
            .map(|entry| {
                let mut published = json!({
                    "Source": entry.source,
                    "DetailType": entry.detail_type,
                    "Detail": json!({ "id": entry.id, "value": entry.value }).to_string(),
                });
                if let Some(bus) = row.bus {
                    published["EventBusName"] = json!(bus);
                }
                published
            })
            .collect();
        let answered = events(&router, case, "PutEvents", &json!({ "Entries": entries })).await;
        assert_eq!(
            answered["FailedEntryCount"].as_i64(),
            Some(0),
            "case {case:?}: every entry must be accepted, got {answered}"
        );
        let answered_entries = answered["Entries"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            answered_entries.len(),
            row.entries.len(),
            "case {case:?}: expected one result entry per submitted entry, got {answered}"
        );

        // The deliveries this publication owes, in the order the entries were submitted.
        for (entry, result) in row.entries.iter().zip(answered_entries.iter()) {
            let event_id = result["EventId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: an accepted entry must carry an EventId, got {answered}")
                })
                .to_string();
            if !entry.delivered {
                unwritten.push((case, entry.id));
                continue;
            }
            let held = await_item(&router, case, entry.id).await;
            let payload = held["payload"]["S"].as_str().unwrap_or_default();
            let parsed: Value = serde_json::from_str(payload).unwrap_or_else(|error| {
                panic!("case {case:?}: the handler writes JSON text, got {payload:?} ({error})")
            });
            assert_eq!(
                parsed, entry.value,
                "case {case:?}: expected the target to have been handed the published value {}, \
                 got {parsed}",
                entry.value
            );
            assert_eq!(
                held["src"]["S"].as_str(),
                Some(entry.source),
                "case {case:?}: expected the envelope to carry the published source, got {held}"
            );
            assert_eq!(
                held["dt"]["S"].as_str(),
                Some(entry.detail_type),
                "case {case:?}: expected the envelope to carry the published detail-type, got \
                 {held}"
            );
            assert_eq!(
                held["eid"]["S"].as_str(),
                Some(event_id.as_str()),
                "case {case:?}: expected the envelope's id to be the answered EventId \
                 {event_id:?}, got {held}"
            );
            assert_eq!(
                held["deliveries"]["N"].as_str(),
                Some("1"),
                "case {case:?}: expected the entry to have been handed over exactly once, got \
                 {held}"
            );
            written.push((case, entry.id, entry.value.clone()));
        }

        // The quiet half of the same publication: an entry no pattern matched is handed to
        // nothing, so the key its detail names holds no item even though the matched entry of
        // the SAME publication has already landed.
        for (unmatched_case, key) in &unwritten {
            let held = item(&router, unmatched_case, key).await;
            assert!(
                held.is_null(),
                "case {unmatched_case:?}: an entry no rule's pattern matches must be delivered \
                 nowhere, yet {key:?} holds {held}"
            );
        }
    }

    // Every delivery stays a single delivery: a second handover of any entry would show up as a
    // higher counter, and the value each write landed is still the published one.
    for (case, key, value) in &written {
        let held = item(&router, case, key).await;
        assert_eq!(
            held["deliveries"]["N"].as_str(),
            Some("1"),
            "case {case:?}: expected {key:?} to have been written by exactly one delivery, got \
             {held}"
        );
        let payload = held["payload"]["S"].as_str().unwrap_or_default();
        let parsed: Value = serde_json::from_str(payload).unwrap_or_else(|error| {
            panic!("case {case:?}: the handler writes JSON text, got {payload:?} ({error})")
        });
        assert_eq!(
            &parsed, value,
            "case {case:?}: expected {key:?} to still hold the published value, got {parsed}"
        );
    }
    for (case, key) in &unwritten {
        let held = item(&router, case, key).await;
        assert!(
            held.is_null(),
            "case {case:?}: an unmatched entry must never be delivered, yet {key:?} holds {held}"
        );
    }
}
