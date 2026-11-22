// SPDX-License-Identifier: CC0-1.0

//! What a REFUSAL hands a running NODE handler, end to end over the wire: Lambda and Amazon
//! DynamoDB on ONE engine, and a `nodejs*` function importing the bundled
//! `@aws-sdk/client-dynamodb` and catching what a refused command throws.
//!
//! The rule pinned here is the one the Python SDK of this runtime is held to as well
//! (`handler_refusal_error_shape_members`): an error shape is a structure like any other, so
//! the SDK a handler calls through hands the caller the MEMBERS that shape declares, each under
//! the name the shape gives it and carrying the value the service answered. A v3 client puts
//! them on the error it throws, beside its message and its `$metadata`.
//!
//! The probes differ along WHICH member the refused operation's error shape declares beside its
//! message — an ordered list of structures (`TransactionCanceledException$CancellationReasons`),
//! a map of attribute values (`ConditionalCheckFailedException$Item`), and none at all
//! (`ResourceNotFoundException`, where the handler must be handed neither of the other two).
//! Every expected value is read off the SAME request's answer ON THE WIRE, so what the handler
//! got is held to what the service answered.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, and the one the handler's client is built
/// with.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The `X-Amz-Target` prefix of the DynamoDB service (its `OPERATIONS.md`, "Wire protocol").
const DYNAMODB_TARGET_PREFIX: &str = "DynamoDB_20120810";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The `Runtime` the function carries: a managed Node release that bundles the SDK.
const RUNTIME: &str = "nodejs22.x";

/// How long a wait for a resource to settle is given before the test is a failure: the
/// transition is owed by the CLOCK, so the wait is for that CONDITION and the deadline is
/// generous — a loaded host may take far longer than an idle one.
const BUDGET: Duration = Duration::from_secs(60);

/// How long between two readings of a resource that is settling.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The table's partition key attribute, and the attribute every condition below reads.
const PARTITION_KEY: &str = "pk";
/// See [`PARTITION_KEY`].
const VERSION_ATTRIBUTE: &str = "v";

/// The item every probe runs against: the one the conditions below are read against.
const STORED_KEY: &str = "acct";

/// The table every probe runs over.
const TABLE: &str = "node-error-shape-members";

/// Lambda and Amazon DynamoDB over ONE shared engine, as `core/router/src/lib.rs` mounts them:
/// the handler's SDK call is dispatched back into this same router, so the refusal it catches
/// is the one this suite reads off the wire.
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
async fn called(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
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

/// One DynamoDB call that must answer 200, for the setup the probes need.
async fn served(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, parsed) = called(router, operation, body).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from DynamoDB {operation}, got {status} ({parsed})"
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

/// The table the probes run against, holding one item, waited out to `ACTIVE` — "you can
/// perform read and write operations only on an ACTIVE table".
async fn given_table(router: &MultiServiceRouter) {
    served(
        router,
        "CreateTable",
        &json!({
            "TableName": TABLE,
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
        let described = served(router, "DescribeTable", &json!({ "TableName": TABLE })).await;
        if described["Table"]["TableStatus"] == json!("ACTIVE") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the table never became ACTIVE: {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    served(
        router,
        "PutItem",
        &json!({
            "TableName": TABLE,
            "Item": {
                PARTITION_KEY: { "S": STORED_KEY },
                VERSION_ATTRIBUTE: { "N": "1" },
            },
        }),
    )
    .await;
}

/// Wait until the function `name` is ready to be invoked, which is what its `State` `Active`
/// says ("the function is ready to be invoked"): the wait is for that CONDITION and not for a
/// span of the clock.
async fn await_active(router: &MultiServiceRouter, name: &str) {
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
            "the function {name:?} never became Active within {BUDGET:?}, got {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// A .zip deployment package holding `source` as `index.mjs` — one ES module, and nothing else:
/// no `node_modules`, so the `@aws-sdk/*` specifier in it is the runtime's to resolve.
fn package(source: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("index.mjs", options)
            .expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// The handler this suite runs: it sends the commands the event lists and reports, per probe,
/// what the SDK gave it — for a refusal the class it was thrown as and the members the error
/// carries, so the report IS what a handler reads off one.
const HANDLER_SOURCE: &str = r#"
import {
    DynamoDBClient,
    GetItemCommand,
    PutItemCommand,
    TransactWriteItemsCommand,
} from "@aws-sdk/client-dynamodb";

const COMMANDS = {
    GetItem: GetItemCommand,
    PutItem: PutItemCommand,
    TransactWriteItems: TransactWriteItemsCommand,
};

export const handler = async (event) => {
    const client = new DynamoDBClient({ region: event.region });
    const probes = {};
    for (const probe of event.probes) {
        const Command = COMMANDS[probe.operation];
        try {
            const answered = await client.send(new Command(probe.input));
            probes[probe.case] = { threw: false, members: Object.keys(answered).sort() };
        } catch (error) {
            probes[probe.case] = {
                threw: true,
                name: error.name,
                message: error.message,
                // The error's own name and its `$`-prefixed metadata are a v3 client's own
                // doing on every error it throws; what the SHAPE contributes is the rest.
                members: Object.keys(error)
                    .filter((named) => named !== "name" && !named.startsWith("$"))
                    .sort(),
                CancellationReasons: error.CancellationReasons ?? null,
                Item: error.Item ?? null,
                Message: error.Message ?? null,
            };
        }
    }
    return { probes };
};
"#;

/// RULE: a refusal reaches a node handler carrying the members its error shape declares, each
/// under the shape's own name and with the value the service answered — and carrying nothing
/// the shape does not declare.
#[tokio::test]
async fn a_refusal_reaches_a_node_handler_with_the_members_its_error_shape_declares() {
    let router = router();
    given_table(&router).await;
    let held = served(
        &router,
        "GetItem",
        &json!({
            "TableName": TABLE,
            "Key": { PARTITION_KEY: { "S": STORED_KEY } },
            "ConsistentRead": true,
        }),
    )
    .await["Item"]
        .clone();

    // The three probes, each refused by the service: a cancelled transaction, whose shape
    // declares an ordered reason list; a conditional write whose condition was not met, whose
    // shape declares the item it read; and a read of a table that is not there, whose shape
    // declares nothing but its message.
    let cancelled = "a cancelled transaction";
    let unmet = "a conditional write whose condition was not met";
    let absent = "a read of a table that is not there";
    let inputs = json!({
        cancelled: {
            "TransactItems": [
                { "Put": {
                    "TableName": TABLE,
                    "Item": { PARTITION_KEY: { "S": "fresh" } },
                } },
                { "Update": {
                    "TableName": TABLE,
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
        },
        unmet: {
            "TableName": TABLE,
            "Item": {
                PARTITION_KEY: { "S": STORED_KEY },
                VERSION_ATTRIBUTE: { "N": "9" },
            },
            "ConditionExpression": format!("{VERSION_ATTRIBUTE} = :expected"),
            "ExpressionAttributeValues": { ":expected": { "N": "2" } },
            "ReturnValuesOnConditionCheckFailure": "ALL_OLD",
        },
        absent: {
            "TableName": format!("{TABLE}-absent"),
            "Key": { PARTITION_KEY: { "S": STORED_KEY } },
        },
    });

    // What each of the SAME requests answers ON THE WIRE, which is what the handler has to be
    // handed.
    let mut answers: Vec<(&str, Value)> = Vec::new();
    for (case, operation) in [
        (cancelled, "TransactWriteItems"),
        (unmet, "PutItem"),
        (absent, "GetItem"),
    ] {
        let (status, body) = called(&router, operation, &inputs[case]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the wire answer to refuse the call, got {status} with \
             {body}"
        );
        answers.push((case, body));
    }

    let name = "node-error-shape-member-caller";
    let (status, _headers, created) = lambda(
        &router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": "index.handler",
            "Timeout": 30,
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
        "expected HTTP 201 from CreateFunction, got {status} ({created})"
    );
    await_active(&router, name).await;

    let (status, headers, payload) = lambda(
        &router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        json!({
            "region": REGION,
            "probes": [
                { "case": cancelled, "operation": "TransactWriteItems",
                  "input": inputs[cancelled] },
                { "case": unmet, "operation": "PutItem", "input": inputs[unmet] },
                { "case": absent, "operation": "GetItem", "input": inputs[absent] },
            ],
        })
        .to_string()
        .as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from Invoke, got {status} ({payload})"
    );
    let signal = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok());
    assert_eq!(
        signal, None,
        "the handler must run to completion, got {signal:?} with {payload}"
    );

    // Each probe was refused as its own class, and the members the error carries are the ones
    // that class's shape declares — `TransactionCanceledException` carries Message and
    // CancellationReasons, `ConditionalCheckFailedException` message and Item, and
    // `ResourceNotFoundException` message alone
    // (`smithy_query(query="shape", shape="<the error shape>")`). A member the shape spells
    // `message` lands on the error's OWN message, which every error a v3 client throws already
    // carries, so it is read there rather than as a member added beside it.
    let expected: &[(&str, &str, &[&str])] = &[
        (cancelled, "TransactionCanceledException", &["CancellationReasons", "Message"]),
        (unmet, "ConditionalCheckFailedException", &["Item"]),
        (absent, "ResourceNotFoundException", &[]),
    ];
    for ((case, body), (named, shape, members)) in answers.iter().zip(expected) {
        assert_eq!(case, named, "the probes are reported in the order they were sent");
        let reported = &payload["probes"][*case];
        assert_eq!(
            reported["threw"],
            json!(true),
            "case {case:?}: expected the call to be refused as {shape:?}, got {reported}"
        );
        assert_eq!(
            reported["name"].as_str(),
            Some(*shape),
            "case {case:?}: expected the error to name the shape {shape:?}, got {reported}"
        );
        let handed: Vec<String> = reported["members"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|member| member.as_str().map(str::to_string))
            .collect();
        let want: Vec<String> = members.iter().map(|named| named.to_string()).collect();
        assert_eq!(
            handed, want,
            "case {case:?}: expected the error to carry {want:?}, got {handed:?} with \
             {reported}"
        );
        // A member the shape does not declare is carried by neither the error nor the answer.
        for member in ["CancellationReasons", "Item"] {
            let carried = reported[member].clone();
            let declared = members.contains(&member);
            assert_eq!(
                carried != Value::Null,
                declared,
                "case {case:?}: expected {member:?} to be carried={declared}, got {carried}"
            );
            if declared {
                assert_eq!(
                    carried, body[member],
                    "case {case:?}: expected the error's {member:?} to be the one the service \
                     answered ({}), got {carried}",
                    body[member]
                );
            }
        }
        // The sentence is the service's own on every one of them.
        let worded = body
            .get("message")
            .or_else(|| body.get("Message"))
            .and_then(Value::as_str);
        assert_eq!(
            reported["message"].as_str(),
            worded,
            "case {case:?}: expected the error to word the refusal as the service did \
             ({worded:?}), got {reported}"
        );
    }

    // The reason list is the transaction's own, in request order: the no-error code for the
    // action that could be applied and the condition code for the one that failed, which
    // carries the item it read because that action asked for it.
    let reasons = payload["probes"][cancelled]["CancellationReasons"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let codes: Vec<&str> = reasons
        .iter()
        .map(|reason| reason["Code"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        codes,
        vec!["None", "ConditionalCheckFailed"],
        "expected one reason per action in request order, got {reasons:?}"
    );
    assert_eq!(
        reasons[1]["Item"], held,
        "expected the failing action to be handed the item it read, got {reasons:?}"
    );
    assert_eq!(
        payload["probes"][unmet]["Item"], held,
        "expected the refused write to be handed the item it read, got {payload}"
    );
}
