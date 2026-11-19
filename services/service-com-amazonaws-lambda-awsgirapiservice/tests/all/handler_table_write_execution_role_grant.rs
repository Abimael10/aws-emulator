// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING handler's own table calls are judged against, end to end over the wire:
//! Lambda, Amazon DynamoDB, IAM and the token service on ONE engine, a `python3.x` function
//! whose code does `import boto3`, and the same calls made under three different documents in
//! force on the function's execution role.
//!
//! The RULE pinned here: a managed runtime hands the handler temporary credentials of the
//! function's EXECUTION ROLE, so every call the handler makes is signed as a session of that
//! role and the service that SERVES the call decides it against the identity-based policies in
//! force on that role at the instant of the call — serving a call some statement in force
//! allows on the resource it addresses, and refusing every other with that service's own
//! authorization answer, which does none of the work. A refusal the handler does not catch is
//! raised by the SDK, and an unhandled raise makes the invocation a FUNCTION ERROR.
//!
//! The rows differ along what the rule generalises over: WHICH call the handler makes (a
//! single-item write, a single-item read, a batched write whose action is the batch's own, a
//! table walk), and WHAT the document in force covers — nothing of the service, the write
//! action on the table the call addresses, or that same action on ANOTHER table of the same
//! account. Both halves of every row are asserted: what the SDK handed the guest, which the
//! handler returns as the invocation's payload, and what the TABLE holds afterwards, read
//! where a client reads it — so a refusal that still wrote the item, or a grant that did not,
//! fails here.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]), and makes four invocations
//! in all: one per document in force, plus the one whose refusal the handler lets raise.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoding and the XML element reader every suite of this target writes a
// role with, owned by the suite that first needed them rather than copied.
use crate::event_source_mapping_role_access::{element, form_encode};
// The one .zip writer this target packages a running handler with.
use crate::synchronous_invoke_run::package;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The account every ARN below names, which is the account this emulator serves.
const ACCOUNT: &str = "000000000000";

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The `X-Amz-Target` prefix of the DynamoDB service (its `OPERATIONS.md`, "Wire protocol").
const DYNAMODB_TARGET_PREFIX: &str = "DynamoDB_20120810";

/// The execution role the function runs as, the function's own name, and the name of the
/// inline policy the role carries each grant in.
const ROLE_NAME: &str = "lambda-table-write-boundary";
const FUNCTION: &str = "table-write-boundary";
const INLINE_POLICY: &str = "granted-table-calls";

/// The AWS-managed policy a Lambda execution role is created with: it grants the log writes a
/// function makes and nothing else, so a role carrying only it has no grant on the table
/// service at all.
const BASIC_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The table the handler addresses, and another table of the same account — the resource a
/// grant that does NOT cover the call names.
const TABLE: &str = "boundary-items";
const OTHER_TABLE: &str = "boundary-other-items";

/// The table's partition key attribute.
const PARTITION_KEY: &str = "pk";

/// The item the suite puts in the table before any grant: what a refused read must not hand
/// over and a granted read must.
const SEEDED_KEY: &str = "seeded-before-any-grant";

/// The keys the handler's own writes use, one per document in force, so what each invocation
/// left in the table is read on its own.
const WITHHELD_KEY: &str = "written-under-no-grant";
const GRANTED_KEY: &str = "written-under-the-grant";
const BATCHED_KEY: &str = "written-in-a-batch";
const OTHER_TABLE_GRANT_KEY: &str = "written-under-another-tables-grant";

/// The error code DynamoDB refuses a call the policies in force do not allow with: the
/// AWS-common `AccessDeniedException`, which its own model declares no shape for.
const DENIAL_CODE: &str = "AccessDeniedException";

/// The status that refusal carries, which is this service's client-error status for an AWS
/// JSON protocol.
const DENIAL_STATUS: u16 = 400;

/// What `X-Amz-Function-Error` reports for an invocation whose handler raised: "Unhandled".
const UNHANDLED: &str = "Unhandled";

/// How long one invocation of this handler is given, as the function's own `Timeout`: far past
/// what a run needs, because what is under test is the authorization decision and never how
/// fast a loaded host gets an interpreter through a handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long a wait for a clock-owed transition is given before the case is a failure, and how
/// long between two looks. Each loop exits the instant its CONDITION holds, so the product is a
/// deadline and not a wait: generous room past what a transition needs even on a host running
/// every other suite beside this one, so no case's outcome depends on how fast the host is.
const BUDGET: Duration = Duration::from_secs(180);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// Lambda, Amazon DynamoDB, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's SDK call is dispatched back into this
/// same router, so the table it writes is the table this suite reads and the role it runs as
/// is the record IAM holds.
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

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature itself is never checked here, and the key it names is one no store
/// holds — the account's own published credential.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One IAM call that must be served: IAM speaks `awsQuery`, so its parameters travel as a form
/// body and its answer comes back as XML.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from IAM {action}, got {status} / {text}"
    );
    text
}

/// One DynamoDB call (`awsJson1_0`, `DynamoDB_20120810.<Operation>`) made with the account's
/// own published credential, which must be served.
async fn dynamodb(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
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

/// The ARN of the execution role, as IAM records it.
fn role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{ROLE_NAME}")
}

/// The ARN of one of this account's tables, as DynamoDB records it.
fn table_arn(name: &str) -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{name}")
}

/// The session ARN AWS reports for the calls a function's code makes: the role's own name and
/// the function's, under the token service's assumed-role form
/// (`state::execution::execution_role_session`).
fn session_principal() -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{ROLE_NAME}/{FUNCTION}")
}

/// The trust policy a Lambda execution role carries, which is what makes the role one
/// `CreateFunction` accepts.
fn trust_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// An identity policy granting the role the WRITES of one table and nothing else, so a read of
/// that table and a write of any other are still outside the policies in force.
fn write_grant(table: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["dynamodb:PutItem", "dynamodb:BatchWriteItem"],
            "Resource": table_arn(table),
        }],
    })
    .to_string()
}

/// The refusal a request no readable identity-based document allows carries, as IAM words it:
/// the principal that signed, the action it asked for, the resource it addressed, and the fact
/// that no identity-based policy allows that action.
fn implicit_denial(action: &str, resource: &str) -> String {
    format!(
        "User: {} is not authorized to perform: {action} on resource: {resource} because no \
         identity-based policy allows the {action} action",
        session_principal()
    )
}

/// The handler every invocation runs: it makes each call the event names and reports what the
/// SDK gave it, so the invocation's payload IS what the guest saw.
///
/// A call the event marks `raise` is made OUTSIDE the catch, so the refusal the SDK raises for
/// it is unhandled — which is what makes the invocation a function error.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions

CLIENT = boto3.client("dynamodb")


def lambda_handler(event, context):
    results = []
    for asked in event["calls"]:
        method = getattr(CLIENT, asked["method"])
        if asked.get("raise"):
            method(**asked["parameters"])
            results.append({"ok": True})
            continue
        try:
            answered = method(**asked["parameters"])
        except botocore.exceptions.ClientError as error:
            results.append(
                {
                    "ok": False,
                    "code": error.response["Error"]["Code"],
                    "message": error.response["Error"].get("Message"),
                    "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
                }
            )
            continue
        except botocore.exceptions.BotoCoreError as error:
            results.append(
                {"ok": False, "sdk_error": type(error).__name__, "detail": str(error)}
            )
            continue
        results.append(
            {
                "ok": True,
                "status": answered["ResponseMetadata"]["HTTPStatusCode"],
                "answered": {
                    name: value
                    for name, value in answered.items()
                    if name != "ResponseMetadata"
                },
            }
        )
    return {"results": results}
"#;

/// One row: the call the handler makes, and what the policies in force mean for it.
struct Row {
    case: &'static str,
    /// The client method the handler calls, and the parameters it carries.
    method: &'static str,
    parameters: Value,
    /// For a call the policies in force do NOT allow: the action and the resource the
    /// serving service's refusal names. `None` for a call they allow.
    refused: Option<(&'static str, String)>,
    /// The output members a served call must carry, each present and not null.
    present: &'static [&'static str],
}

/// The item one write stores, keyed by `key`.
fn item(key: &str) -> Value {
    json!({ PARTITION_KEY: { "S": key }, "body": { "S": "written by the handler" } })
}

fn put_parameters(table: &str, key: &str) -> Value {
    json!({ "TableName": table, "Item": item(key) })
}

fn get_parameters(table: &str, key: &str) -> Value {
    json!({ "TableName": table, "Key": { PARTITION_KEY: { "S": key } } })
}

fn batch_parameters(table: &str, key: &str) -> Value {
    json!({
        "RequestItems": { table: [{ "PutRequest": { "Item": item(key) } }] },
    })
}

fn scan_parameters(table: &str) -> Value {
    json!({ "TableName": table })
}

/// The rows of the invocation made while the role carries only the managed policy every
/// execution role carries: no statement in force names the table service at all, so every
/// call is outside the policies in force.
fn withheld_rows() -> Vec<Row> {
    vec![
        Row {
            case: "a single-item write no identity policy allows",
            method: "put_item",
            parameters: put_parameters(TABLE, WITHHELD_KEY),
            refused: Some(("dynamodb:PutItem", table_arn(TABLE))),
            present: &[],
        },
        Row {
            case: "a single-item read no identity policy allows",
            method: "get_item",
            parameters: get_parameters(TABLE, SEEDED_KEY),
            refused: Some(("dynamodb:GetItem", table_arn(TABLE))),
            present: &[],
        },
        Row {
            case: "a batched write, decided under the batch's own action",
            method: "batch_write_item",
            parameters: batch_parameters(TABLE, BATCHED_KEY),
            refused: Some(("dynamodb:BatchWriteItem", table_arn(TABLE))),
            present: &[],
        },
        Row {
            case: "a table walk no identity policy allows",
            method: "scan",
            parameters: scan_parameters(TABLE),
            refused: Some(("dynamodb:Scan", table_arn(TABLE))),
            present: &[],
        },
    ]
}

/// The rows of the invocation made with the WRITES of the addressed table granted: the same
/// calls, of which the ones the document covers are served and the ones it does not are still
/// refused.
fn granted_rows() -> Vec<Row> {
    vec![
        Row {
            case: "a single-item write the identity policy allows",
            method: "put_item",
            parameters: put_parameters(TABLE, GRANTED_KEY),
            refused: None,
            present: &[],
        },
        Row {
            case: "a batched write the identity policy allows",
            method: "batch_write_item",
            parameters: batch_parameters(TABLE, BATCHED_KEY),
            refused: None,
            present: &["UnprocessedItems"],
        },
        Row {
            case: "a read the write grant does not cover",
            method: "get_item",
            parameters: get_parameters(TABLE, SEEDED_KEY),
            refused: Some(("dynamodb:GetItem", table_arn(TABLE))),
            present: &[],
        },
    ]
}

/// The rows of the invocation made with the same action granted on ANOTHER table: the grant is
/// about a resource this call does not address, so the write is refused exactly as it was
/// under no grant at all.
fn other_table_rows() -> Vec<Row> {
    vec![
        Row {
            case: "a write whose grant names another table",
            method: "put_item",
            parameters: put_parameters(TABLE, OTHER_TABLE_GRANT_KEY),
            refused: Some(("dynamodb:PutItem", table_arn(TABLE))),
            present: &[],
        },
        Row {
            case: "the same write on the table the grant does name",
            method: "put_item",
            parameters: put_parameters(OTHER_TABLE, OTHER_TABLE_GRANT_KEY),
            refused: None,
            present: &[],
        },
    ]
}

/// Make every row's call in ONE invocation and assert what the SDK gave the guest for each.
async fn calls_made(router: &MultiServiceRouter, label: &str, rows: &[Row]) {
    let asked: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "method": row.method,
                "parameters": row.parameters,
            })
        })
        .collect();
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
        json!({ "calls": asked }).to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {label:?}: expected HTTP 200 from Invoke, got {status} ({payload})"
    );
    let signal = function_error(&headers);
    assert_eq!(
        signal, None,
        "case {label:?}: expected no FunctionError, got {signal:?} with {payload}"
    );
    let results = payload["results"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        results.len(),
        rows.len(),
        "case {label:?}: expected one result per call, got {payload}"
    );
    for (row, result) in rows.iter().zip(&results) {
        let case = row.case;
        match &row.refused {
            Some((action, resource)) => {
                assert_eq!(
                    result["ok"],
                    json!(false),
                    "case {case:?}: expected the call to be refused, got {result}"
                );
                assert_eq!(
                    result["code"],
                    json!(DENIAL_CODE),
                    "case {case:?}: expected the error code {DENIAL_CODE:?}, got {result}"
                );
                assert_eq!(
                    result["status"],
                    json!(DENIAL_STATUS),
                    "case {case:?}: expected HTTP {DENIAL_STATUS} on the refusal, got {result}"
                );
                assert_eq!(
                    result["message"],
                    json!(implicit_denial(action, resource)),
                    "case {case:?}: expected the refusal to name the session, the action and \
                     the resource, got {result}"
                );
            }
            None => {
                assert_eq!(
                    result["ok"],
                    json!(true),
                    "case {case:?}: expected the call to be served, got {result}"
                );
                assert_eq!(
                    result["status"],
                    json!(200),
                    "case {case:?}: expected the SDK to report HTTP 200, got {result}"
                );
                for member in row.present {
                    assert!(
                        !result["answered"][*member].is_null(),
                        "case {case:?}: expected the output member {member:?} to be answered, \
                         got {result}"
                    );
                }
            }
        }
    }
}

/// What `X-Amz-Function-Error` reports for one answered invocation.
fn function_error(headers: &http::HeaderMap) -> Option<String> {
    headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// Whether the table `table` holds an item under `key`, read with the account's own published
/// credential — where a client reads it.
async fn item_held(router: &MultiServiceRouter, case: &str, table: &str, key: &str) -> bool {
    let read = dynamodb(router, case, "GetItem", &get_parameters(table, key)).await;
    !read["Item"].is_null()
}

/// Create the table `name` and wait it out to `ACTIVE` — "you can perform read and write
/// operations only on an ACTIVE table".
async fn given_table(router: &MultiServiceRouter, name: &str) {
    dynamodb(
        router,
        "setup",
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
        let described = dynamodb(
            router,
            "setup",
            "DescribeTable",
            &json!({ "TableName": name }),
        )
        .await;
        if described["Table"]["TableStatus"] == json!("ACTIVE") {
            let arn = described["Table"]["TableArn"].as_str().unwrap_or_default();
            assert_eq!(
                arn,
                table_arn(name),
                "setup: expected the table's ARN of this account and Region, got {arn:?}"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the table {name:?} never became ACTIVE: {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Create the function and wait it out to `Active`.
async fn given_function(router: &MultiServiceRouter) {
    let (status, _headers, created) = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": FUNCTION,
            "Role": role_arn(),
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
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
        "setup: expected HTTP 201 from CreateFunction, got {status} ({created})"
    );
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, _headers, read) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/configuration"),
            &[],
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: expected HTTP 200 from GetFunctionConfiguration, got {status} ({read})"
        );
        if read["State"] == json!("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the function never became Active: {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: a call a running handler makes with the SDK its runtime supplies is signed as the
/// function's execution-role session, so the service that serves it decides it against the
/// identity-based policies in force on that role — serving the calls a statement in force
/// allows on the resource they address and refusing every other with that service's own
/// authorization refusal, which does none of the work.
#[tokio::test]
async fn a_handlers_own_table_calls_are_decided_against_its_execution_roles_policies() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = router();

    // The role the function runs as, written with IAM's own operations, so the session the
    // runtime mints speaks for the record `iam:CreateRole` wrote. It carries only the managed
    // policy every execution role carries, which grants nothing on the table service.
    let created = iam(
        &router,
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME),
            ("AssumeRolePolicyDocument", &trust_document()),
        ],
    )
    .await;
    let recorded = element(&created, "Arn");
    assert_eq!(
        recorded,
        role_arn(),
        "setup: expected IAM to record the role under its own ARN, got {recorded:?}"
    );
    iam(
        &router,
        "AttachRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyArn", BASIC_EXECUTION_POLICY),
        ],
    )
    .await;

    // The two tables, created where a client creates them, and one item in the addressed one
    // before any grant: what a refused read must not hand over.
    given_table(&router, TABLE).await;
    given_table(&router, OTHER_TABLE).await;
    dynamodb(
        &router,
        "setup",
        "PutItem",
        &put_parameters(TABLE, SEEDED_KEY),
    )
    .await;

    given_function(&router).await;

    // 1. Every call outside the policies in force.
    let withheld = withheld_rows();
    calls_made(&router, "the calls under no grant", &withheld).await;

    // The other half of each refused row: the service did none of the work. Neither the single
    // write nor the batched one left an item, read where a client reads it.
    for key in [WITHHELD_KEY, BATCHED_KEY] {
        assert!(
            !item_held(&router, "the calls under no grant", TABLE, key).await,
            "case \"the calls under no grant\": expected the refused write of {key:?} to store \
             nothing"
        );
    }

    // 2. The grant on the addressed table's writes, written where a client writes one.
    iam(
        &router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", INLINE_POLICY),
            ("PolicyDocument", &write_grant(TABLE)),
        ],
    )
    .await;
    let granted = granted_rows();
    calls_made(&router, "the calls under the write grant", &granted).await;
    for key in [GRANTED_KEY, BATCHED_KEY] {
        assert!(
            item_held(&router, "the calls under the write grant", TABLE, key).await,
            "case \"the calls under the write grant\": expected the granted write of {key:?} to \
             store its item"
        );
    }

    // 3. The same action granted on ANOTHER table: the call this invocation makes on the
    // addressed table is refused again, and the one on the granted table is served.
    iam(
        &router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", INLINE_POLICY),
            ("PolicyDocument", &write_grant(OTHER_TABLE)),
        ],
    )
    .await;
    let other = other_table_rows();
    calls_made(&router, "the calls under another table's grant", &other).await;
    assert!(
        !item_held(
            &router,
            "a write whose grant names another table",
            TABLE,
            OTHER_TABLE_GRANT_KEY
        )
        .await,
        "case \"a write whose grant names another table\": expected the refused write to store \
         nothing"
    );
    assert!(
        item_held(
            &router,
            "the same write on the table the grant does name",
            OTHER_TABLE,
            OTHER_TABLE_GRANT_KEY
        )
        .await,
        "case \"the same write on the table the grant does name\": expected the granted write to \
         store its item"
    );

    // 4. The refusal the handler does NOT catch: the SDK raises it, and an unhandled raise
    // makes the invocation a function error — which is what a caller in front of the function
    // reads as a failure rather than as the handler's own answer.
    let (status, headers, payload) = lambda(
        &router,
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
        json!({
            "calls": [{
                "method": "put_item",
                "parameters": put_parameters(TABLE, WITHHELD_KEY),
                "raise": true,
            }],
        })
        .to_string()
        .as_bytes(),
    )
    .await;
    let case = "a refusal the handler lets raise";
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Invoke, got {status} ({payload})"
    );
    assert_eq!(
        function_error(&headers),
        Some(UNHANDLED.to_string()),
        "case {case:?}: expected the invocation to report the function error {UNHANDLED:?}, got \
         {payload}"
    );
    let reported = payload["errorMessage"].as_str().unwrap_or_default();
    assert!(
        reported.contains(DENIAL_CODE),
        "case {case:?}: expected the raised error to be the service's own refusal, got {payload}"
    );
    assert!(
        !item_held(&router, case, TABLE, WITHHELD_KEY).await,
        "case {case:?}: expected the refused write to store nothing"
    );
}
