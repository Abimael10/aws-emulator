// SPDX-License-Identifier: CC0-1.0

//! What a REFUSAL hands a running handler, end to end over the wire: Lambda and Amazon
//! DynamoDB on ONE engine, and a `python3.x` function whose code does `import boto3`, catches
//! `botocore.exceptions.ClientError` and reports the whole of what the SDK gave it.
//!
//! The rule pinned here: an error shape is a structure like any other, so the SDK a handler
//! calls through hands the caller the MEMBERS that shape declares — each under the name the
//! shape gives it, carrying the value the service answered — beside the code, the message and
//! the response metadata. A refusal whose shape declares nothing but its message hands over
//! nothing else, and a refusal the called service's model declares no shape for hands over
//! only the code and the message every refusal carries.
//!
//! The rows of the first table differ along what the rule generalises over: WHICH member the
//! refused operation's error shape declares beside the message — an ordered list of structures
//! (`TransactionCanceledException$CancellationReasons`), a map of attribute values
//! (`ConditionalCheckFailedException$Item`), none at all (`ResourceNotFoundException`) — and a
//! refusal the service's model declares no error shape for. Every expected value is read off
//! the SAME request's answer ON THE WIRE, so what the handler got is held to what the service
//! answered rather than to anything this suite writes down.
//!
//! The second table walks the list the first row's member carries: one reason per action of
//! the transaction in request order, differing along how many actions it carries, where the
//! failing one sits and whether that action asked for the item it read
//! (`ReturnValuesOnConditionCheckFailure`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::synchronous_invoke_run::package;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The `X-Amz-Target` prefix of the DynamoDB service (its `OPERATIONS.md`, "Wire protocol").
const DYNAMODB_TARGET_PREFIX: &str = "DynamoDB_20120810";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The module and attribute the package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The runtime every function below is created with.
const RUNTIME: &str = "python3.12";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs, because what is under test is what the SDK hands the handler and not how fast an
/// interpreter gets through it.
const FUNCTION_TIMEOUT: u64 = 30;

/// How long a wait for a resource to settle is given before the row is a failure: the
/// transition is owed by the CLOCK, so the wait is for that CONDITION and the deadline is
/// generous — a loaded host may take far longer than an idle one.
const BUDGET: Duration = Duration::from_secs(60);

/// How long between two readings of a resource that is settling.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The table's partition key attribute, and the attribute every condition below reads.
const PARTITION_KEY: &str = "pk";
/// See [`PARTITION_KEY`].
const VERSION_ATTRIBUTE: &str = "v";

/// The item every row runs against: the one the conditions below are read against.
const STORED_KEY: &str = "acct";

/// Lambda and Amazon DynamoDB over ONE shared engine, as `core/router/src/lib.rs` mounts
/// them: the handler's SDK call is dispatched back into this same router, so the refusal it
/// catches is the one this suite reads off the wire.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One DynamoDB call (`awsJson1_0`, `DynamoDB_20120810.<Operation>`), as its status and parsed
/// body — whatever it answered.
async fn called(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("dynamodb.{REGION}.amazonaws.com"))
        .header("authorization", authorization("dynamodb"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{DYNAMODB_TARGET_PREFIX}.{operation}"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One DynamoDB call that must answer 200, for the setup every row needs.
async fn served(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = called(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DynamoDB {operation}, got {status} ({parsed})"
    );
    parsed
}

/// One Lambda `restJson1` call, as its status, headers and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("authorization", authorization("lambda"))
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// The table every row runs against, holding one item, waited out to `ACTIVE` — "you can
/// perform read and write operations only on an ACTIVE table".
async fn given_table(router: &MultiServiceRouter, case: &str, name: &str) {
    served(
        router,
        case,
        "CreateTable",
        &json!({
            "TableName": name,
            "KeySchema": [{ "AttributeName": PARTITION_KEY, "KeyType": "HASH" }],
            "AttributeDefinitions": [
                { "AttributeName": PARTITION_KEY, "AttributeType": "S" },
            ],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let deadline = Instant::now() + BUDGET;
    loop {
        let described = served(router, case, "DescribeTable", &json!({ "TableName": name })).await;
        if described["Table"]["TableStatus"] == json!("ACTIVE") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the table never became ACTIVE: {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    served(
        router,
        case,
        "PutItem",
        &json!({
            "TableName": name,
            "Item": {
                PARTITION_KEY: { "S": STORED_KEY },
                VERSION_ATTRIBUTE: { "N": "1" },
            },
        }),
    )
    .await;
}

/// The item the table holds at [`STORED_KEY`], read back where a client reads it: what an
/// action that asked for the item it read must be handed.
async fn stored(router: &MultiServiceRouter, case: &str, table: &str) -> Value {
    let read = served(
        router,
        case,
        "GetItem",
        &json!({
            "TableName": table,
            "Key": { PARTITION_KEY: { "S": STORED_KEY } },
            "ConsistentRead": true,
        }),
    )
    .await;
    read["Item"].clone()
}

/// Create the function running [`HANDLER_SOURCE`] and wait until it is ready to be invoked.
async fn given_function(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, _headers, created) = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(HANDLER_SOURCE)),
            },
        }))
        .expect("the create body serializes"),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from CreateFunction, got {status} ({created})"
    );
    // "Active" is when "the function is ready to be invoked", so the wait is for THAT and not
    // for a span of the clock.
    let deadline = Instant::now() + BUDGET;
    loop {
        let (_status, _headers, read) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &[],
        )
        .await;
        if read["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the function {name:?} never became Active within {BUDGET:?}, got \
             {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `Invoke` the function synchronously with `event`, asserting it ran to completion, and
/// answer the payload the run produced.
async fn invoked(router: &MultiServiceRouter, case: &str, name: &str, event: &Value) -> Value {
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        event.to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Invoke, got {status} ({payload})"
    );
    let signal = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        signal, None,
        "case {case:?}: the handler must run to completion, got the function error {signal:?} \
         with {payload}"
    );
    payload
}

/// The handler every row runs: it makes the ONE call the event names and reports the whole of
/// what the SDK handed it for the refusal — every member of `error.response` except the
/// response metadata, which carries an id of its own per call.
const HANDLER_SOURCE: &str = r##"
import boto3
import botocore.exceptions


def lambda_handler(event, context):
    client = boto3.client("dynamodb")
    method = getattr(client, event["method"])
    try:
        answered = method(**event["parameters"])
    except botocore.exceptions.ClientError as error:
        response = dict(error.response)
        metadata = response.pop("ResponseMetadata", {})
        return {
            "refused": True,
            "members": sorted(response),
            "response": response,
            "status": metadata.get("HTTPStatusCode"),
        }
    return {"refused": False, "answered": sorted(answered)}
"##;

/// The value the refusal's own answer carries for the member `member`.
///
/// The key is the member's own; a protocol that spells one in a different CASE — the message of
/// an AWS JSON refusal, which travels as `message` — carries the same member, so a key
/// differing from it only in case answers too.
fn carried(body: &Value, member: &str) -> Option<Value> {
    let document = body.as_object()?;
    document.get(member).cloned().or_else(|| {
        document
            .iter()
            .find(|(key, _value)| key.eq_ignore_ascii_case(member))
            .map(|(_key, value)| value.clone())
    })
}

/// One refusal, and the members its error shape declares.
struct Row {
    case: &'static str,
    /// The client method the handler calls.
    method: &'static str,
    /// The operation that method names, which the wire request below addresses.
    operation: &'static str,
    /// The error shape the refusal is modelled as, for the report of a failing row.
    shape: &'static str,
    /// The members the handler must be handed beside `Error`, each named as the shape names
    /// it (`smithy_query(query="shape", shape="<the error shape>")`) — which is the name the
    /// model gives the member, whatever case it spells the message member in.
    members: &'static [&'static str],
}

/// The rows, each naming the model reading its member list comes from.
fn rows() -> Vec<Row> {
    vec![
        Row {
            // `TransactionCanceledException`: members `Message` and `CancellationReasons` ->
            // CancellationReasonList, "an ordered list of errors for each item in the
            // request".
            case: "a cancelled transaction",
            method: "transact_write_items",
            operation: "TransactWriteItems",
            shape: "TransactionCanceledException",
            members: &["CancellationReasons", "Message"],
        },
        Row {
            // `ConditionalCheckFailedException`: members `message` and `Item` ->
            // AttributeMap, "item which caused the ConditionalCheckFailedException".
            case: "a conditional write whose condition was not met",
            method: "put_item",
            operation: "PutItem",
            shape: "ConditionalCheckFailedException",
            members: &["Item", "message"],
        },
        Row {
            // `ResourceNotFoundException`: the member `message` alone, so nothing else is
            // handed over.
            case: "a read of a table that is not there",
            method: "get_item",
            operation: "GetItem",
            shape: "ResourceNotFoundException",
            members: &["message"],
        },
        Row {
            // A refusal of the request's own members: this service's model declares NO
            // `ValidationException` shape, so the SDK has no modelled members to hand over
            // and the code and the message are the whole of it.
            case: "an item that gives no value for the key",
            method: "put_item",
            operation: "PutItem",
            shape: "ValidationException",
            members: &[],
        },
    ]
}

/// The parameters one row's call carries, against the table the row runs over.
fn parameters(case: &str, table: &str) -> Value {
    match case {
        "a cancelled transaction" => json!({
            "TransactItems": [
                { "Put": {
                    "TableName": table,
                    "Item": { PARTITION_KEY: { "S": "fresh" } },
                } },
                { "Update": {
                    "TableName": table,
                    "Key": { PARTITION_KEY: { "S": STORED_KEY } },
                    "UpdateExpression": format!("SET {VERSION_ATTRIBUTE} = :next"),
                    "ConditionExpression": format!("{VERSION_ATTRIBUTE} = :expected"),
                    "ExpressionAttributeValues": {
                        ":next": { "N": "3" },
                        ":expected": { "N": "2" },
                    },
                    "ReturnValuesOnConditionCheckFailure": "ALL_OLD",
                } },
            ],
        }),
        "a conditional write whose condition was not met" => json!({
            "TableName": table,
            "Item": {
                PARTITION_KEY: { "S": STORED_KEY },
                VERSION_ATTRIBUTE: { "N": "9" },
            },
            "ConditionExpression": format!("{VERSION_ATTRIBUTE} = :expected"),
            "ExpressionAttributeValues": { ":expected": { "N": "2" } },
            "ReturnValuesOnConditionCheckFailure": "ALL_OLD",
        }),
        "a read of a table that is not there" => json!({
            "TableName": format!("{table}-absent"),
            "Key": { PARTITION_KEY: { "S": STORED_KEY } },
        }),
        _ => json!({
            "TableName": table,
            "Item": { VERSION_ATTRIBUTE: { "N": "1" } },
        }),
    }
}

/// RULE: a refusal reaches a handler carrying the members its error shape declares, each under
/// the shape's own name and with the value the service answered, beside the code and the
/// message — and carrying nothing the shape does not declare.
#[tokio::test]
async fn a_refusal_reaches_a_handler_with_the_members_its_error_shape_declares() {
    let router = router();
    let table = "error-shape-members";
    let function = "error-shape-member-caller";
    given_table(&router, "the shared table", table).await;
    given_function(&router, "the shared function", function).await;

    for row in rows() {
        let case = row.case;
        let input = parameters(case, table);

        // What the SAME request answers ON THE WIRE, which is what the handler has to be
        // handed.
        let (status, body) = called(&router, row.operation, &input).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the wire answer to refuse the call, got {status} with \
             {body}"
        );

        let payload = invoked(
            &router,
            case,
            function,
            &json!({ "method": row.method, "parameters": input }),
        )
        .await;
        assert_eq!(
            payload["refused"],
            json!(true),
            "case {case:?}: expected the handler's call to be refused as {:?}, got {payload}",
            row.shape
        );
        assert_eq!(
            payload["status"].as_u64(),
            Some(u64::from(status)),
            "case {case:?}: expected the handler to read the status {status}, got {payload}"
        );
        // The members the handler was handed are `Error` and exactly the ones the shape
        // declares that the answer carried — no more, no fewer.
        let mut expected: Vec<String> = row.members.iter().map(|named| named.to_string()).collect();
        expected.push("Error".to_string());
        expected.sort();
        let handed: Vec<String> = payload["members"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|member| member.as_str().map(str::to_string))
            .collect();
        assert_eq!(
            handed, expected,
            "case {case:?}: expected the refusal to hand over {expected:?}, got {handed:?} \
             with {payload}"
        );
        // And each of them carries the value the service answered for it.
        for member in row.members {
            let answered = carried(&body, member).unwrap_or_else(|| {
                panic!("case {case:?}: the wire answer carries no {member:?}: {body}")
            });
            assert_eq!(
                payload["response"][*member], answered,
                "case {case:?}: expected the handler's {member:?} to be the one the service \
                 answered ({answered}), got {payload}"
            );
        }
        // The code and the sentence are the service's own either way.
        assert_eq!(
            payload["response"]["Error"]["Message"],
            carried(&body, "Message").unwrap_or(Value::Null),
            "case {case:?}: expected the handler to read the service's own sentence, got \
             {payload} against {body}"
        );
    }
}

/// One transaction of the second table: the actions it carries, and what each of them must
/// report.
struct Cancellation {
    case: &'static str,
    /// The reason code every action reports, in request order.
    codes: &'static [&'static str],
    /// Which action asked for the item it read (`ReturnValuesOnConditionCheckFailure=ALL_OLD`),
    /// and must therefore be handed it.
    returns_item: Option<usize>,
}

/// The rows of the second table, differing along how many actions the transaction carries,
/// where the failing one sits and whether it asked for the item it read.
fn cancellations() -> Vec<Cancellation> {
    vec![
        Cancellation {
            case: "a failing update after an applicable put",
            codes: &["None", "ConditionalCheckFailed"],
            returns_item: Some(1),
        },
        Cancellation {
            case: "a failing update before an applicable put",
            codes: &["ConditionalCheckFailed", "None"],
            returns_item: Some(0),
        },
        Cancellation {
            case: "a failing condition check on its own, asking for no item",
            codes: &["ConditionalCheckFailed"],
            returns_item: None,
        },
        Cancellation {
            case: "a failing update last of three",
            codes: &["None", "None", "ConditionalCheckFailed"],
            returns_item: Some(2),
        },
    ]
}

/// The `TransactItems` one row of the second table carries: an applicable action for every
/// `None` it expects, and the failing one — asking for the item it read where the row says —
/// where it expects `ConditionalCheckFailed`.
fn actions(row: &Cancellation, table: &str) -> Value {
    let built: Vec<Value> = row
        .codes
        .iter()
        .enumerate()
        .map(|(at, code)| {
            if *code == "None" {
                return json!({ "Put": {
                    "TableName": table,
                    "Item": { PARTITION_KEY: { "S": format!("fresh-{at}") } },
                } });
            }
            let mut failing = Map::new();
            failing.insert("TableName".to_string(), json!(table));
            failing.insert(
                "Key".to_string(),
                json!({ PARTITION_KEY: { "S": STORED_KEY } }),
            );
            failing.insert(
                "ConditionExpression".to_string(),
                json!(format!("{VERSION_ATTRIBUTE} = :expected")),
            );
            // An action asking for the item it read is an `Update` here; one asking for none is
            // a `ConditionCheck`, which writes nothing of its own — and each carries exactly the
            // substitutions its own expressions dereference.
            match row.returns_item == Some(at) {
                true => {
                    failing.insert(
                        "ExpressionAttributeValues".to_string(),
                        json!({ ":expected": { "N": "2" }, ":next": { "N": "3" } }),
                    );
                    failing.insert(
                        "UpdateExpression".to_string(),
                        json!(format!("SET {VERSION_ATTRIBUTE} = :next")),
                    );
                    failing.insert(
                        "ReturnValuesOnConditionCheckFailure".to_string(),
                        json!("ALL_OLD"),
                    );
                    json!({ "Update": Value::Object(failing) })
                }
                false => {
                    failing.insert(
                        "ExpressionAttributeValues".to_string(),
                        json!({ ":expected": { "N": "2" } }),
                    );
                    json!({ "ConditionCheck": Value::Object(failing) })
                }
            }
        })
        .collect();
    json!({ "TransactItems": built })
}

/// RULE: the cancellation reasons a handler is handed carry one reason per action of the
/// transaction in request order — the code of the action that failed where it failed, the
/// no-error code everywhere else, and the item the failing action read where that action asked
/// for it.
#[tokio::test]
async fn a_cancelled_transactions_reasons_reach_a_handler_in_request_order() {
    let router = router();
    let table = "cancellation-reason-order";
    let function = "cancellation-reason-caller";
    given_table(&router, "the shared table", table).await;
    given_function(&router, "the shared function", function).await;
    let held = stored(&router, "the shared table", table).await;

    for row in cancellations() {
        let case = row.case;
        let input = actions(&row, table);
        let payload = invoked(
            &router,
            case,
            function,
            &json!({ "method": "transact_write_items", "parameters": input }),
        )
        .await;
        assert_eq!(
            payload["refused"],
            json!(true),
            "case {case:?}: expected the transaction to be cancelled, got {payload}"
        );
        let reasons = payload["response"]["CancellationReasons"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let codes: Vec<&str> = reasons
            .iter()
            .map(|reason| reason["Code"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            codes, row.codes,
            "case {case:?}: expected one reason per action carrying {:?}, got {codes:?} with \
             {payload}",
            row.codes
        );
        for (at, reason) in reasons.iter().enumerate() {
            let expected = match row.returns_item == Some(at) {
                // "The item which caused the transaction to get cancelled", which is the item
                // the table holds and not the one the action tried to write.
                true => held.clone(),
                false => Value::Null,
            };
            assert_eq!(
                reason.get("Item").cloned().unwrap_or(Value::Null),
                expected,
                "case {case:?}: expected the reason at {at} to carry {expected}, got {reason}"
            );
        }
        // The item the cancelled transaction would have written stands untouched, so what the
        // reasons report is the stored state and not a write this call made.
        let unchanged = stored(&router, case, table).await;
        assert_eq!(
            unchanged, held,
            "case {case:?}: a cancelled transaction writes nothing, got {unchanged}"
        );
    }
}
