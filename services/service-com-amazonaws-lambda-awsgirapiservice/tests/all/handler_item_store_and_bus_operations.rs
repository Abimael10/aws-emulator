// SPDX-License-Identifier: CC0-1.0

//! The ITEM-STORE and EVENT-BUS operations a running `python3.x` handler calls with the AWS SDK
//! its managed runtime supplies, end to end over the wire: Lambda, Amazon DynamoDB, Amazon
//! EventBridge, Amazon SQS, IAM and the token service on ONE engine, a function whose
//! module-scope code does `import boto3`, and every item it wrote, read, changed and deleted
//! and every event it published read back where a client reads them.
//!
//! RULE: a managed `python3.x` runtime puts the AWS SDK on the handler's import path, so a
//! client the handler builds reaches the service that client names with temporary credentials of
//! the FUNCTION's execution role — the call is carried out by that service itself, whatever
//! operation it is: its effect is the one a later read of that service reports, and its answer
//! (the operation's own output members, or the modelled error it raised) is what the SDK gives
//! the guest. A call no document in force on the role permits is refused by the addressed
//! service with its own `AccessDeniedException`, naming the session as the principal, and
//! changes nothing. None of this is a failure of the RUNTIME, so every such invocation answers
//! HTTP 200 with no `X-Amz-Function-Error` header, carrying the handler's own result.
//!
//! The steps span the class rather than one operation: they differ along WHICH service the
//! client is built for (an item store and an event bus), WHICH operation of it is called (a
//! single-item write, a batched write, a key-condition read, a filtered table read, a
//! conditional update, a delete, a publication, a rule read), WHETHER the call carries a
//! condition the stored item satisfies, and WHICH bus a publication names (the account's default
//! one and a custom one). The same steps run twice, under two execution roles that differ in the
//! one thing the handler cannot change: whether the role's policies grant the calls at all.
//!
//! Each group's steps run in ONE invocation, because an invocation is the dearest thing this
//! target does, and the closing assertions read everything back through DynamoDB's own `Scan`
//! and off the SQS queue the matching rules target — so a step that reported work it did not do
//! fails there, and so does a refusal that changed something anyway.
//!
//! The suite RUNS handlers, so it takes this target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The Amazon SQS wire call is the one the failure-destination suite publishes: reached rather
// than written again here.
use crate::event_source_mapping_failure_destination::sqs;
use crate::event_source_mapping_role_access::{allow, given_role, logs_only};
use crate::event_source_mappings::{await_function_active, call, router_on};
// The one .zip writer this crate's suites package a handler with, exported by the suite that
// first needed it rather than copied here.
use crate::node_invoke_run::package;
// The DynamoDB call that must be SERVED, exported by the bundled-SDK suite for the same reason.
use crate::python_runtime_bundled_sdk::dynamodb_ok;

/// Where `CreateFunction` is bound, and the prefix every function request below is built on.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The runtime every function below runs, and the module and attribute its package carries its
/// handler as.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The table every item below lives in, and the two attributes its key is made of: a partition
/// key and a sort key, so a key-condition read of one partition is a read of several items.
const TABLE: &str = "handler-order-lines";
/// See [`TABLE`].
const PARTITION_KEY: &str = "pk";
/// See [`TABLE`].
const SORT_KEY: &str = "sk";

/// The attribute a conditional update turns over, and the one carried beside it. Neither is read
/// by this emulator: both are the handler's own, and what is asserted is what they round-trip
/// to.
const STATUS_ATTRIBUTE: &str = "status";
/// See [`STATUS_ATTRIBUTE`].
const QUANTITY_ATTRIBUTE: &str = "qty";

/// The three states an item's [`STATUS_ATTRIBUTE`] takes here: the one a write stores, the one a
/// conditional update moves it to, and the one the filtered read selects on.
const NEW: &str = "new";
/// See [`NEW`].
const SHIPPED: &str = "shipped";
/// See [`NEW`].
const DONE: &str = "done";

/// The two buses the publications below name: the account's default one, and a custom one this
/// suite creates.
const DEFAULT_BUS: &str = "default";
/// See [`DEFAULT_BUS`].
const CUSTOM_BUS: &str = "handler-markers-bus";

/// The rule each bus carries, the target id the queue is attached under, and the queue both
/// rules deliver to.
const DEFAULT_BUS_RULE: &str = "receives-default-bus-markers";
/// See [`DEFAULT_BUS_RULE`].
const CUSTOM_BUS_RULE: &str = "receives-custom-bus-markers";
/// See [`DEFAULT_BUS_RULE`].
const TARGET_ID: &str = "t1";
/// See [`DEFAULT_BUS_RULE`].
const QUEUE: &str = "handler-markers";

/// The source and detail type every publication carries, which is what both rules match on.
const SOURCE: &str = "com.demo.handler-sdk";
/// See [`SOURCE`].
const DETAIL_TYPE: &str = "Handler Marker";

/// The member of a published event's detail that carries which step published it.
const MARKER: &str = "marker";

/// The service principal EventBridge delivers to a queue as, which the queue's own policy has to
/// grant: "for Lambda functions, Amazon SNS topics and Amazon SQS queues, EventBridge relies on
/// resource-based policies" (`PutTargets`).
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The code and status an addressed service refuses an unauthorized call with: DynamoDB's
/// `AccessDeniedException` and EventBridge's, both at the client-error default.
const ACCESS_DENIED: &str = "AccessDeniedException";
/// See [`ACCESS_DENIED`].
const ACCESS_DENIED_STATUS: u16 = 400;

/// The code and status DynamoDB answers a write whose own condition the stored item does not
/// satisfy with — a modelled failure of the CALL, reached only once the call was authorized.
const CONDITIONAL_CHECK_FAILED: &str = "ConditionalCheckFailedException";
/// See [`CONDITIONAL_CHECK_FAILED`].
const CONDITION_FAILED_STATUS: u16 = 400;
/// See [`CONDITIONAL_CHECK_FAILED`].
const CONDITION_FAILED_MESSAGE: &str = "The conditional request failed";

/// The IAM actions of the operations the steps call, each in the namespace the owning service
/// publishes its actions under (its `arnNamespace`, `smithy_query(query="service_info")`).
const PUT_ITEM: &str = "dynamodb:PutItem";
/// See [`PUT_ITEM`].
const BATCH_WRITE_ITEM: &str = "dynamodb:BatchWriteItem";
/// See [`PUT_ITEM`].
const QUERY: &str = "dynamodb:Query";
/// See [`PUT_ITEM`].
const SCAN: &str = "dynamodb:Scan";
/// See [`PUT_ITEM`].
const UPDATE_ITEM: &str = "dynamodb:UpdateItem";
/// See [`PUT_ITEM`].
const DELETE_ITEM: &str = "dynamodb:DeleteItem";
/// See [`PUT_ITEM`].
const PUT_EVENTS: &str = "events:PutEvents";
/// See [`PUT_ITEM`].
const DESCRIBE_RULE: &str = "events:DescribeRule";

/// How long the closing read waits for what the publications owe the queue, and how often it
/// looks again: the wait is for the CONDITION, with room to spare on a loaded host.
const BUDGET: Duration = Duration::from_secs(45);
/// See [`BUDGET`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// Lambda, Amazon DynamoDB, Amazon EventBridge, Amazon SQS, IAM and the token service over ONE
/// shared engine, as `core/router/src/lib.rs` mounts them: the handler's own SDK call is
/// dispatched back into this same router, so the table and the buses it reaches are the ones
/// this suite reads, and the role its credentials are minted of is the record IAM holds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// One EventBridge call that must be served, as the account's own credential. The signature is
/// never checked: the harness turns request authentication off, and the credential scope is what
/// routes the request.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/events/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let answered: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&payload).to_string()))
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with \
         {answered}"
    );
    answered
}

/// One Amazon SQS call that must be served, over the shared wire helper. The credential this
/// harness signs with is the account's own, so these are the OWNER's reads rather than a role's,
/// as they are for the DynamoDB reads beside them ([`dynamodb_ok`]).
async fn sqs_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answered) = sqs(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Amazon SQS {operation} must succeed, got {status} with {answered}"
    );
    answered
}

/// The table every step addresses, keyed on a partition key AND a sort key, waited out to
/// `ACTIVE`; answers the ARN DynamoDB persisted, which is the resource a grant for it names.
async fn given_table(router: &MultiServiceRouter, case: &str) -> String {
    let created = dynamodb_ok(
        router,
        case,
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "AttributeDefinitions": [
                { "AttributeName": PARTITION_KEY, "AttributeType": "S" },
                { "AttributeName": SORT_KEY, "AttributeType": "S" },
            ],
            "KeySchema": [
                { "AttributeName": PARTITION_KEY, "KeyType": "HASH" },
                { "AttributeName": SORT_KEY, "KeyType": "RANGE" },
            ],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let arn = created["TableDescription"]["TableArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no TableArn in {created}"))
        .to_string();
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = dynamodb_ok(router, case, "DescribeTable", &json!({ "TableName": TABLE })).await;
        if read["Table"]["TableStatus"] == json!("ACTIVE") {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the table never became ACTIVE: {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The ARN of the bus `name`, as EventBridge holds it: the default one is read, a custom one is
/// created.
async fn given_bus(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let answered = if name == DEFAULT_BUS {
        events(router, case, "DescribeEventBus", &json!({ "Name": name })).await
    } else {
        events(router, case, "CreateEventBus", &json!({ "Name": name })).await
    };
    answered
        .get("Arn")
        .or_else(|| answered.get("EventBusArn"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no bus ARN in {answered}"))
        .to_string()
}

/// The queue both rules deliver to, carrying the policy EventBridge's delivery needs; answers
/// its url.
async fn given_queue(router: &MultiServiceRouter, case: &str) -> String {
    let created = sqs_ok(router, case, "CreateQueue", &json!({ "QueueName": QUEUE })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    let attributes = sqs_ok(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowEventBridge",
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": arn,
        }],
    })
    .to_string();
    sqs_ok(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
    url
}

/// The pattern both rules watch for: exactly the events this suite publishes, so a message on
/// the queue is one of them and nothing else.
fn rule_pattern() -> String {
    json!({ "source": [SOURCE], "detail-type": [DETAIL_TYPE] }).to_string()
}

/// The rule `name` on the bus `bus`, carrying the queue at `queue_url` as its one target;
/// answers the ARN EventBridge persisted for the rule.
async fn given_rule(
    router: &MultiServiceRouter,
    case: &str,
    bus: &str,
    name: &str,
    queue_url: &str,
) -> String {
    let attributes = sqs_ok(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": queue_url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let queue_arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
        .to_string();
    let written = events(
        router,
        case,
        "PutRule",
        &json!({
            "Name": name,
            "EventBusName": bus,
            "EventPattern": rule_pattern(),
            "State": "ENABLED",
        }),
    )
    .await;
    let rule_arn = written["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no RuleArn in {written}"))
        .to_string();
    let targets = events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": name,
            "EventBusName": bus,
            "Targets": [{ "Id": TARGET_ID, "Arn": queue_arn }],
        }),
    )
    .await;
    assert_eq!(
        targets["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutTargets on {name:?} reported a failed entry: {targets}"
    );
    rule_arn
}

/// The handler every function runs: it imports the SDK and builds BOTH its clients at MODULE
/// scope, so a runtime shipping no SDK fails the initialization phase and no step runs at all;
/// then for each step the event names it calls that operation on the named client and reports
/// what came back — the output members the SDK gave it, or the error the addressed service
/// raised as the SDK's own class carries it.
///
/// What it reports of a publication is whether every entry was MINTED an event id rather than
/// the ids themselves: an id is the bus's own per-call value, so the shape is what a caller can
/// assert on.
const HANDLER_SOURCE: &str = r#"
import boto3
from botocore.exceptions import ClientError

CLIENTS = {"dynamodb": boto3.client("dynamodb"), "events": boto3.client("events")}

REPORTED = ("Items", "Count", "Attributes", "Name", "State", "EventBusName", "EventPattern")


def lambda_handler(event, context):
    results = []
    for step in event["steps"]:
        client = CLIENTS[step["client"]]
        try:
            answered = getattr(client, step["operation"])(**step["arguments"])
        except ClientError as error:
            results.append({
                "ok": False,
                "code": error.response["Error"]["Code"],
                "message": error.response["Error"]["Message"],
                "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
            })
            continue
        read = {"ok": True}
        for member in REPORTED:
            if member in answered:
                read[member] = answered[member]
        if "UnprocessedItems" in answered:
            read["unprocessed"] = sum(
                [len(requests) for requests in answered["UnprocessedItems"].values()]
            )
        if "Entries" in answered:
            read["FailedEntryCount"] = answered["FailedEntryCount"]
            read["minted"] = all(
                [bool(entry.get("EventId")) for entry in answered["Entries"]]
            )
        results.append(read)
    return {"results": results}
"#;

/// The role one group's function runs as.
fn role_of(slug: &str) -> String {
    format!("handler-sdk-{slug}")
}

/// The function one group invokes.
fn function_of(slug: &str) -> String {
    format!("handler-sdk-{slug}-caller")
}

/// The assumed-role session an SDK call made by an invocation of that group's function speaks
/// for: the session is named for the FUNCTION, and it is the principal a refusal names.
fn session_of(slug: &str) -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{}/{}",
        engine::emulator_account_id(),
        role_of(slug),
        function_of(slug)
    )
}

/// The partition one group's items live under, so one group's items are its own.
fn partition(slug: &str) -> String {
    format!("{slug}-order")
}

/// The partition a group's batched write puts its second item under.
fn other_partition(slug: &str) -> String {
    format!("{slug}-order-2")
}

/// One item of a group, as the wire carries attribute values.
fn item(partition: &str, sort: &str, status: &str, quantity: &str) -> Value {
    json!({
        PARTITION_KEY: { "S": partition },
        SORT_KEY: { "S": sort },
        STATUS_ATTRIBUTE: { "S": status },
        QUANTITY_ATTRIBUTE: { "N": quantity },
    })
}

/// The key of one item, as a request addresses it.
fn key(partition: &str, sort: &str) -> Value {
    json!({ PARTITION_KEY: { "S": partition }, SORT_KEY: { "S": sort } })
}

/// The marker a group's publication onto `bus` carries, which is what the queue is drained for.
fn marker(slug: &str, bus: &str) -> String {
    format!("{slug}-{bus}")
}

/// One step of a group's invocation: the client it is made on, the call the handler makes, and
/// what the SDK must have given the guest for it.
struct Step {
    case: &'static str,
    /// The service token the client was built with, which names the service addressed.
    client: &'static str,
    /// The client method, which names the operation.
    operation: &'static str,
    /// The keyword arguments it is called with: the operation's own input members.
    arguments: Value,
    /// What the handler must have read back.
    expected: Value,
}

/// One group: the statements of its function's execution role, and the steps one invocation of
/// that function makes.
struct Group {
    case: &'static str,
    /// What this group's role, function and partitions are named after.
    slug: &'static str,
    /// The statements of the role's one inline identity policy.
    statements: Value,
    steps: Vec<Step>,
    /// Whether this group's writes were granted, so DynamoDB must hold what they left behind.
    writes: bool,
    /// Whether this group's publications were granted, so the queue must hold their markers.
    publishes: bool,
}

/// What a served call reports when the operation carries no output member this suite reads.
fn served() -> Value {
    json!({ "ok": true })
}

/// What a served batched write reports: nothing left unprocessed.
fn wrote_batch() -> Value {
    json!({ "ok": true, "unprocessed": 0 })
}

/// What a served publication reports: no failed entry, and an event id minted for every entry.
fn published() -> Value {
    json!({ "ok": true, "FailedEntryCount": 0, "minted": true })
}

/// What a served read of the rule `name` on the bus `bus` reports.
fn described_rule(bus: &str, name: &str) -> Value {
    json!({
        "ok": true,
        "Name": name,
        "State": "ENABLED",
        "EventBusName": bus,
        "EventPattern": rule_pattern(),
    })
}

/// The refusal the addressed service words when nothing in force permits the calling session the
/// action: its own error code and status, over the sentence that names who was refused, which
/// action on which resource, and the reason the refusal rests on.
fn refused(slug: &str, action: &str, resource: &str) -> Value {
    json!({
        "ok": false,
        "code": ACCESS_DENIED,
        "status": ACCESS_DENIED_STATUS,
        "message": format!(
            "User: {} is not authorized to perform: {action} on resource: {resource} \
             because no identity-based policy allows the {action} action",
            session_of(slug)
        ),
    })
}

/// The arguments of each step, in the order one invocation makes them: a single-item write, a
/// batched write of two more, a key-condition read of the one partition that then holds two
/// items, a filtered read of the whole table, a conditional update the stored item satisfies,
/// the SAME conditional update once the item no longer satisfies it, a conditional delete, a
/// publication onto each bus, and a read of the custom bus's rule.
fn put_item_arguments(slug: &str) -> Value {
    json!({
        "TableName": TABLE,
        "Item": item(&partition(slug), "line#1", NEW, "2"),
    })
}
/// See [`put_item_arguments`].
fn batch_write_arguments(slug: &str) -> Value {
    json!({
        "RequestItems": {
            TABLE: [
                { "PutRequest": { "Item": item(&partition(slug), "line#2", NEW, "5") } },
                { "PutRequest": { "Item": item(&other_partition(slug), "line#1", DONE, "1") } },
            ],
        },
    })
}
/// See [`put_item_arguments`].
fn query_arguments(slug: &str) -> Value {
    json!({
        "TableName": TABLE,
        "KeyConditionExpression": "#p = :partition AND begins_with(#s, :prefix)",
        "ExpressionAttributeNames": { "#p": PARTITION_KEY, "#s": SORT_KEY },
        "ExpressionAttributeValues": {
            ":partition": { "S": partition(slug) },
            ":prefix": { "S": "line#" },
        },
    })
}
/// See [`put_item_arguments`].
fn scan_arguments(slug: &str) -> Value {
    json!({
        "TableName": TABLE,
        "FilterExpression": "#status = :done AND #p = :partition",
        "ExpressionAttributeNames": { "#status": STATUS_ATTRIBUTE, "#p": PARTITION_KEY },
        "ExpressionAttributeValues": {
            ":done": { "S": DONE },
            ":partition": { "S": other_partition(slug) },
        },
    })
}
/// See [`put_item_arguments`].
fn update_arguments(slug: &str) -> Value {
    json!({
        "TableName": TABLE,
        "Key": key(&partition(slug), "line#1"),
        "UpdateExpression": "SET #status = :shipped",
        "ConditionExpression": "#status = :new",
        "ExpressionAttributeNames": { "#status": STATUS_ATTRIBUTE },
        "ExpressionAttributeValues": { ":shipped": { "S": SHIPPED }, ":new": { "S": NEW } },
        "ReturnValues": "ALL_NEW",
    })
}
/// See [`put_item_arguments`].
fn delete_arguments(slug: &str) -> Value {
    json!({
        "TableName": TABLE,
        "Key": key(&other_partition(slug), "line#1"),
        "ConditionExpression": "attribute_exists(#p)",
        "ExpressionAttributeNames": { "#p": PARTITION_KEY },
    })
}
/// See [`put_item_arguments`].
fn put_events_arguments(slug: &str, bus: &str) -> Value {
    json!({
        "Entries": [{
            "EventBusName": bus,
            "Source": SOURCE,
            "DetailType": DETAIL_TYPE,
            "Detail": json!({ MARKER: marker(slug, bus) }).to_string(),
        }],
    })
}
/// See [`put_item_arguments`].
fn describe_rule_arguments() -> Value {
    json!({ "Name": CUSTOM_BUS_RULE, "EventBusName": CUSTOM_BUS })
}

/// The steps of the group whose role grants every call, with what each must have answered.
fn granted_steps(slug: &'static str) -> Vec<Step> {
    vec![
        Step {
            case: "the single-item write the role allows",
            client: "dynamodb",
            operation: "put_item",
            arguments: put_item_arguments(slug),
            expected: served(),
        },
        Step {
            case: "the batched write of two more items",
            client: "dynamodb",
            operation: "batch_write_item",
            arguments: batch_write_arguments(slug),
            expected: wrote_batch(),
        },
        Step {
            case: "the key-condition read of the partition holding two of them",
            client: "dynamodb",
            operation: "query",
            arguments: query_arguments(slug),
            expected: json!({
                "ok": true,
                "Count": 2,
                "Items": [
                    item(&partition(slug), "line#1", NEW, "2"),
                    item(&partition(slug), "line#2", NEW, "5"),
                ],
            }),
        },
        Step {
            case: "the filtered read of the whole table",
            client: "dynamodb",
            operation: "scan",
            arguments: scan_arguments(slug),
            expected: json!({
                "ok": true,
                "Count": 1,
                "Items": [item(&other_partition(slug), "line#1", DONE, "1")],
            }),
        },
        Step {
            case: "the conditional update the stored item satisfies",
            client: "dynamodb",
            operation: "update_item",
            arguments: update_arguments(slug),
            expected: json!({
                "ok": true,
                "Attributes": item(&partition(slug), "line#1", SHIPPED, "2"),
            }),
        },
        Step {
            case: "the same conditional update once the item no longer satisfies it",
            client: "dynamodb",
            operation: "update_item",
            arguments: update_arguments(slug),
            expected: json!({
                "ok": false,
                "code": CONDITIONAL_CHECK_FAILED,
                "status": CONDITION_FAILED_STATUS,
                "message": CONDITION_FAILED_MESSAGE,
            }),
        },
        Step {
            case: "the conditional delete of the item the batch wrote",
            client: "dynamodb",
            operation: "delete_item",
            arguments: delete_arguments(slug),
            expected: served(),
        },
        Step {
            case: "the publication onto the account's default bus",
            client: "events",
            operation: "put_events",
            arguments: put_events_arguments(slug, DEFAULT_BUS),
            expected: published(),
        },
        Step {
            case: "the publication onto a custom bus",
            client: "events",
            operation: "put_events",
            arguments: put_events_arguments(slug, CUSTOM_BUS),
            expected: published(),
        },
        Step {
            case: "the read of the custom bus's own rule",
            client: "events",
            operation: "describe_rule",
            arguments: describe_rule_arguments(),
            expected: described_rule(CUSTOM_BUS, CUSTOM_BUS_RULE),
        },
    ]
}

/// The steps of the group whose role grants none of them: the same calls, each refused by the
/// service that owns it, naming that call's own action and the resource it addressed.
fn ungranted_steps(
    slug: &'static str,
    table_arn: &str,
    default_bus_arn: &str,
    custom_bus_arn: &str,
    custom_rule_arn: &str,
) -> Vec<Step> {
    vec![
        Step {
            case: "a single-item write nothing in force allows",
            client: "dynamodb",
            operation: "put_item",
            arguments: put_item_arguments(slug),
            expected: refused(slug, PUT_ITEM, table_arn),
        },
        Step {
            case: "a batched write nothing in force allows",
            client: "dynamodb",
            operation: "batch_write_item",
            arguments: batch_write_arguments(slug),
            expected: refused(slug, BATCH_WRITE_ITEM, table_arn),
        },
        Step {
            case: "a key-condition read nothing in force allows",
            client: "dynamodb",
            operation: "query",
            arguments: query_arguments(slug),
            expected: refused(slug, QUERY, table_arn),
        },
        Step {
            case: "a filtered table read nothing in force allows",
            client: "dynamodb",
            operation: "scan",
            arguments: scan_arguments(slug),
            expected: refused(slug, SCAN, table_arn),
        },
        Step {
            case: "a conditional update nothing in force allows",
            client: "dynamodb",
            operation: "update_item",
            arguments: update_arguments(slug),
            expected: refused(slug, UPDATE_ITEM, table_arn),
        },
        Step {
            case: "a conditional delete nothing in force allows",
            client: "dynamodb",
            operation: "delete_item",
            arguments: delete_arguments(slug),
            expected: refused(slug, DELETE_ITEM, table_arn),
        },
        Step {
            case: "a publication onto the default bus nothing in force allows",
            client: "events",
            operation: "put_events",
            arguments: put_events_arguments(slug, DEFAULT_BUS),
            expected: refused(slug, PUT_EVENTS, default_bus_arn),
        },
        Step {
            case: "a publication onto a custom bus nothing in force allows",
            client: "events",
            operation: "put_events",
            arguments: put_events_arguments(slug, CUSTOM_BUS),
            expected: refused(slug, PUT_EVENTS, custom_bus_arn),
        },
        Step {
            case: "a rule read nothing in force allows",
            client: "events",
            operation: "describe_rule",
            arguments: describe_rule_arguments(),
            expected: refused(slug, DESCRIBE_RULE, custom_rule_arn),
        },
    ]
}

/// The two groups: a role granted every action each call asks for on the very resource it
/// addresses, and a role whose policy says nothing about either service.
fn groups(
    table_arn: &str,
    default_bus_arn: &str,
    custom_bus_arn: &str,
    custom_rule_arn: &str,
) -> Vec<Group> {
    vec![
        Group {
            case: "a role granted every item-store and event-bus call its handler makes",
            slug: "granted",
            statements: json!([
                allow(
                    "dynamodb",
                    &[
                        "PutItem",
                        "BatchWriteItem",
                        "Query",
                        "Scan",
                        "UpdateItem",
                        "DeleteItem",
                    ],
                    table_arn,
                ),
                allow("events", &["PutEvents"], default_bus_arn),
                allow("events", &["PutEvents"], custom_bus_arn),
                allow("events", &["DescribeRule"], custom_rule_arn),
                logs_only(),
            ]),
            steps: granted_steps("granted"),
            writes: true,
            publishes: true,
        },
        Group {
            case: "a role whose policy says nothing about either service",
            slug: "ungranted",
            statements: json!([logs_only()]),
            steps: ungranted_steps(
                "ungranted",
                table_arn,
                default_bus_arn,
                custom_bus_arn,
                custom_rule_arn,
            ),
            writes: false,
            publishes: false,
        },
    ]
}

/// Create the function of one group, running the one handler as `role_arn`.
async fn given_function(router: &MultiServiceRouter, case: &str, name: &str, role_arn: &str) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": 30,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("lambda_function.py", HANDLER_SOURCE)])),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction {name:?} to answer 201, got {} {}",
        created.status,
        created.text()
    );
}

/// Every item the table holds, keyed by its partition and sort key, read through DynamoDB's own
/// `Scan` the way a client reads it.
async fn stored_items(
    router: &MultiServiceRouter,
    case: &str,
) -> BTreeMap<(String, String), Value> {
    let scanned = dynamodb_ok(router, case, "Scan", &json!({ "TableName": TABLE })).await;
    scanned["Items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|held| {
            let partition = held[PARTITION_KEY]["S"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let sort = held[SORT_KEY]["S"].as_str().unwrap_or_default().to_string();
            ((partition, sort), held)
        })
        .collect()
}

/// The markers of the messages waiting on the queue at `url`, taken off it until `expected` of
/// them have arrived or the budget runs out. Every message is one of this suite's own
/// publications, so its envelope is asserted as it is drained.
async fn drained_markers(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    expected: usize,
) -> BTreeSet<String> {
    let deadline = Instant::now() + BUDGET;
    let mut markers: BTreeSet<String> = BTreeSet::new();
    loop {
        let received = sqs_ok(
            router,
            case,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            let text = message["Body"].as_str().unwrap_or_default();
            let body: Value = serde_json::from_str(text).unwrap_or(Value::Null);
            assert_eq!(
                (body["source"].as_str(), body["detail-type"].as_str()),
                (Some(SOURCE), Some(DETAIL_TYPE)),
                "case {case:?}: expected the rule's own event on the queue, got {text}"
            );
            markers.insert(
                body["detail"][MARKER]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: no {MARKER} in {text}"))
                    .to_string(),
            );
        }
        if markers.len() >= expected || Instant::now() >= deadline {
            return markers;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Every item-store and event-bus operation a handler calls with the SDK its runtime supplies is
/// carried out by the service that owns it, decided per action and per resource by the policies
/// in force on the function's execution role — and what that service answered, served or
/// refused, is what the handler read back.
#[tokio::test]
async fn a_handlers_sdk_calls_reach_the_item_store_and_the_event_bus_that_own_them() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a python handler's own item-store and event-bus calls";
    let router = emulator();

    let table_arn = given_table(&router, case).await;
    let default_bus_arn = given_bus(&router, case, DEFAULT_BUS).await;
    let custom_bus_arn = given_bus(&router, case, CUSTOM_BUS).await;
    let queue_url = given_queue(&router, case).await;
    given_rule(&router, case, DEFAULT_BUS, DEFAULT_BUS_RULE, &queue_url).await;
    let custom_rule_arn = given_rule(&router, case, CUSTOM_BUS, CUSTOM_BUS_RULE, &queue_url).await;

    let groups = groups(
        &table_arn,
        &default_bus_arn,
        &custom_bus_arn,
        &custom_rule_arn,
    );

    let mut role_arns: Vec<String> = Vec::with_capacity(groups.len());
    for group in &groups {
        role_arns.push(
            given_role(
                &router,
                group.case,
                &role_of(group.slug),
                Some(group.statements.clone()),
            )
            .await,
        );
    }

    // Every function first, so the provisioning spans the clock owes run down together rather
    // than one after another.
    for (group, role_arn) in groups.iter().zip(&role_arns) {
        given_function(&router, group.case, &function_of(group.slug), role_arn).await;
    }
    for group in &groups {
        await_function_active(&router, group.case, &function_of(group.slug)).await;
    }

    for group in &groups {
        let steps: Vec<Value> = group
            .steps
            .iter()
            .map(|step| {
                json!({
                    "client": step.client,
                    "operation": step.operation,
                    "arguments": step.arguments,
                })
            })
            .collect();
        let answered = call(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{}/invocations", function_of(group.slug)),
            &json!({ "steps": steps }),
        )
        .await;
        assert_eq!(
            answered.status,
            200,
            "case {:?}: expected HTTP 200 from Invoke, got {} {}",
            group.case,
            answered.status,
            answered.text()
        );
        let payload = answered.json();
        // The handler RAN: a runtime that could not import the SDK fails the initialization
        // phase, which Invoke reports as a function error instead of a result.
        assert_eq!(
            answered.header("x-amz-function-error"),
            None,
            "case {:?}: expected the handler's own result, got a function error with {payload}",
            group.case
        );
        assert_eq!(
            payload["results"].as_array().map(Vec::len),
            Some(group.steps.len()),
            "case {:?}: expected one result per step, got {payload}",
            group.case
        );
        for (index, step) in group.steps.iter().enumerate() {
            let actual = payload["results"][index].clone();
            assert_eq!(
                actual, step.expected,
                "case {:?} / {:?}: expected {}, got {actual} (the whole run: {payload})",
                group.case, step.case, step.expected
            );
        }
    }

    // What DYNAMODB itself holds, read where a client reads it: the item the served write wrote
    // with the status the served conditional update moved it to, the one the served batch wrote
    // beside it, nothing under the partition the served delete emptied, and nothing at all from
    // the group whose calls were refused.
    let held = stored_items(&router, case).await;
    let granted = groups
        .iter()
        .find(|group| group.writes)
        .expect("one group's writes are granted");
    let refused_group = groups
        .iter()
        .find(|group| !group.writes)
        .expect("one group's writes are refused");
    let expected: BTreeMap<(String, String), Value> = [
        (
            (partition(granted.slug), "line#1".to_string()),
            item(&partition(granted.slug), "line#1", SHIPPED, "2"),
        ),
        (
            (partition(granted.slug), "line#2".to_string()),
            item(&partition(granted.slug), "line#2", NEW, "5"),
        ),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        held, expected,
        "case {case:?}: expected DynamoDB to hold exactly what the served calls left behind, got \
         {held:?}"
    );
    assert!(
        !held
            .keys()
            .any(|(partition_key, _)| partition_key.starts_with(refused_group.slug)),
        "case {case:?}: expected the refused writes to have stored nothing, got {held:?}"
    );

    // What the BUSES did with the publications, read off the queue both rules target: one
    // message per served publication, carrying that publication's own marker, and nothing from
    // the publications that were refused.
    let expected_markers: BTreeSet<String> = groups
        .iter()
        .filter(|group| group.publishes)
        .flat_map(|group| {
            [DEFAULT_BUS, CUSTOM_BUS]
                .into_iter()
                .map(|bus| marker(group.slug, bus))
        })
        .collect();
    let delivered = drained_markers(&router, case, &queue_url, expected_markers.len()).await;
    assert_eq!(
        delivered, expected_markers,
        "case {case:?}: expected the queue to hold the markers of the served publications, got \
         {delivered:?}"
    );
}
