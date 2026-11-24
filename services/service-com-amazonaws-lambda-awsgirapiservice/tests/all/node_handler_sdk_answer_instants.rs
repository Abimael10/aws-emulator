// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING node handler reads back for a member the called service's model types as a
//! TIMESTAMP: Lambda, Amazon DynamoDB, Secrets Manager, CloudWatch Logs, IAM and the token
//! service on ONE engine, a `nodejs22.x` ES module whose first line is
//! `import { DynamoDBClient, DescribeTableCommand } from '@aws-sdk/client-dynamodb'`, and every
//! value it reports compared with what the SAME call answers on the wire.
//!
//! The RULE pinned here: the AWS SDK for JavaScript v3 deserializes every member of an answer
//! that the called service's model types as a `timestamp` into a native `Date`, because a JSON
//! protocol carries one as the seconds since the epoch — so a handler reads `d instanceof
//! Date`, `d.getTime()` and `d.toISOString()` off such a member, and a member the model types
//! as anything else (a `long` carrying an epoch, a numeric attribute value) is handed over
//! exactly as the wire carried it.
//!
//! The rows differ along what the rule generalises over: WHICH service answers and under which
//! AWS JSON version (`awsJson1_0`, `awsJson1_1`, and a service whose answer this runtime builds
//! from its own client's output), WHERE the member sits in the answer (a top-level member, a
//! member of a nested structure, a member of a structure inside a list), and WHAT the model
//! types the value as (a timestamp, against a `long` of epoch milliseconds in the same answer
//! and in another service's). No row states an instant of its own: every expected value is the
//! one the same call reports on the wire.
//!
//! The second suite is the same rule under an EXECUTION ROLE: the in-handler call is signed as
//! the function's role session, so the table it may describe is the one the role's document
//! covers and every other call is answered with DynamoDB's own refusal — which a handler reads
//! as the error class the shape names.
//!
//! Both suites RUN handlers, so they take the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoding and the XML element reader every suite of this target writes a
// role with, owned by the suite that first needed them rather than copied.
use crate::event_source_mapping_role_access::{element, form_encode};

/// The Region every request below is signed for, and the one the handler's clients are built
/// with.
const REGION: &str = "us-east-1";

/// The account every ARN below names, which is the account this emulator serves.
const ACCOUNT: &str = "000000000000";

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The `Runtime` these functions carry: a managed Node release that bundles the SDK.
const RUNTIME: &str = "nodejs22.x";

/// The `X-Amz-Target` prefixes of the AWS JSON services these suites read on the wire (each
/// service's own `OPERATIONS.md`, "Wire protocol").
const DYNAMODB_TARGET_PREFIX: &str = "DynamoDB_20120810";
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";
const SECRETS_TARGET_PREFIX: &str = "secretsmanager";

/// How long one invocation of these handlers is given, as the function's own `Timeout`: far
/// past what a run needs, because what is under test is the form a value reaches the guest in
/// and never how fast a loaded host gets an interpreter through a handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long a wait for a clock-owed transition is given before the case is a failure, and how
/// long between two looks. Each loop exits the instant its CONDITION holds, so the product is a
/// deadline and not a wait: generous room past what a transition needs even on a host running
/// every other suite beside this one.
const BUDGET: Duration = Duration::from_secs(180);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The error code DynamoDB refuses a call the policies in force do not allow with: the
/// AWS-common `AccessDeniedException`, which its own model declares no shape for.
const DENIAL_CODE: &str = "AccessDeniedException";

/// The error shape DynamoDB refuses a call on a table it does not hold with
/// (`ResourceNotFoundException`, HTTP 400).
const ABSENT_TABLE_CODE: &str = "ResourceNotFoundException";

/// Lambda and the services these handlers call, over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's SDK call is dispatched back into this
/// same router, so every value it reads back is the one this suite reads on the wire.
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
        service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
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
/// request. The signature itself is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One AWS JSON call to a service of this emulator, as its status and parsed body.
async fn aws_json(
    router: &MultiServiceRouter,
    service: &str,
    content_type: &str,
    target: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(service))
        .header("content-type", content_type)
        .header("x-amz-target", target)
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    (parts.status.as_u16(), parsed)
}

/// One DynamoDB call (`awsJson1_0`) that must answer 200.
async fn dynamodb(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = aws_json(
        router,
        "dynamodb",
        "application/x-amz-json-1.0",
        &format!("{DYNAMODB_TARGET_PREFIX}.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DynamoDB {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Secrets Manager call (`awsJson1_1`) that must answer 200.
async fn secrets(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = aws_json(
        router,
        "secretsmanager",
        "application/x-amz-json-1.1",
        &format!("{SECRETS_TARGET_PREFIX}.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Secrets Manager {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One CloudWatch Logs call (`awsJson1_1`) that must answer 200.
async fn logs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = aws_json(
        router,
        "logs",
        "application/x-amz-json-1.1",
        &format!("{LOGS_TARGET_PREFIX}.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch Logs {operation}, got {status} with \
         {parsed}"
    );
    parsed
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

/// A .zip deployment package holding `source` as `index.mjs` — one ES module, and nothing
/// else: no `node_modules`, so every `@aws-sdk/*` specifier in it is the runtime's to resolve.
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

/// Create a `nodejs22.x` function running [`HANDLER_SOURCE`] as `role`, and wait it out to
/// `Active`.
async fn given_function(router: &MultiServiceRouter, name: &str, role: &str) {
    let (status, _headers, created) = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": role,
            "Runtime": RUNTIME,
            "Handler": "index.handler",
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
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
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

/// Create the table `name` and wait it out to `ACTIVE` — "you can perform read and write
/// operations only on an ACTIVE table" — answering the `CreationDateTime` a client reads for
/// it, in the epoch seconds the wire carried.
async fn given_table(router: &MultiServiceRouter, name: &str) -> f64 {
    dynamodb(
        router,
        "setup",
        "CreateTable",
        &json!({
            "TableName": name,
            "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
            "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let deadline = Instant::now() + BUDGET;
    loop {
        let described = dynamodb(router, "setup", "DescribeTable", &json!({ "TableName": name }))
            .await;
        if described["Table"]["TableStatus"] == json!("ACTIVE") {
            return described["Table"]["CreationDateTime"]
                .as_f64()
                .unwrap_or_else(|| {
                    panic!("setup: DescribeTable must report CreationDateTime: {described}")
                });
        }
        assert!(
            Instant::now() < deadline,
            "setup: the table {name:?} never became ACTIVE: {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The ARN of one of this account's tables, as DynamoDB records it.
fn table_arn(name: &str) -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{name}")
}

/// The handler both suites run: it sends the command each probe names and reports WHAT FORM the
/// value at the probe's path reached it in, so the invocation's payload IS what the SDK gave
/// the guest.
const HANDLER_SOURCE: &str = r#"
import {
    DynamoDBClient,
    DescribeTableCommand,
    CreateBackupCommand,
    ListBackupsCommand,
} from "@aws-sdk/client-dynamodb";
import { SecretsManagerClient, DescribeSecretCommand } from "@aws-sdk/client-secrets-manager";
import { CloudWatchLogsClient, DescribeLogGroupsCommand } from "@aws-sdk/client-cloudwatch-logs";

const CLIENTS = {
    dynamodb: (region) => new DynamoDBClient({ region }),
    secretsmanager: (region) => new SecretsManagerClient({ region }),
    logs: (region) => new CloudWatchLogsClient({ region }),
};

const COMMANDS = {
    DescribeTable: DescribeTableCommand,
    CreateBackup: CreateBackupCommand,
    ListBackups: ListBackupsCommand,
    DescribeSecret: DescribeSecretCommand,
    DescribeLogGroups: DescribeLogGroupsCommand,
};

const reached = (answered, path) => path.reduce(
    (held, step) => (held === null || held === undefined ? held : held[step]),
    answered,
);

export const handler = async (event) => {
    const reports = [];
    for (const probe of event.probes) {
        const client = CLIENTS[probe.service](event.region);
        try {
            const answered = await client.send(new COMMANDS[probe.operation](probe.input));
            const value = reached(answered, probe.path);
            reports.push({
                ok: true,
                isDate: value instanceof Date,
                typeOf: typeof value,
                epochMs: value instanceof Date ? value.getTime() : null,
                iso: value instanceof Date ? value.toISOString() : null,
                plain: value instanceof Date ? null : value,
            });
        } catch (error) {
            reports.push({ ok: false, name: error.name, message: error.message });
        }
    }
    return { reports };
};
"#;

/// What one probe must find at its path.
enum Expected {
    /// The `Date` a v3 client materialises for a timestamp member, on the instant the same
    /// call reports on the wire — in the epoch SECONDS the wire carried it in.
    Instant(f64),
    /// The same, for a value whose instant this suite cannot name: a `Date`, on an instant
    /// past the epoch (a 1970 instant is what a millisecond count read as seconds produces).
    SomeInstant,
    /// A value the model does not type as a timestamp, handed over exactly as the wire
    /// carried it.
    AsCarried(Value),
    /// The service's own refusal: the error class the shape names, and its message.
    Refused(&'static str, String),
}

/// One probe: the call the handler makes, and what the value at `path` must be.
struct Probe {
    case: &'static str,
    service: &'static str,
    operation: &'static str,
    input: Value,
    path: Value,
    expected: Expected,
}

/// Make every probe's call in ONE invocation and assert the form each value reached the guest
/// in.
async fn probes_made(router: &MultiServiceRouter, name: &str, probes: &[Probe]) {
    let asked: Vec<Value> = probes
        .iter()
        .map(|probe| {
            json!({
                "service": probe.service,
                "operation": probe.operation,
                "input": probe.input,
                "path": probe.path,
            })
        })
        .collect();
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        json!({ "region": REGION, "probes": asked })
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
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        signal, None,
        "expected no FunctionError, got {signal:?} with {payload}"
    );
    let reports = payload["reports"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        reports.len(),
        probes.len(),
        "expected one report per probe, got {payload}"
    );
    for (probe, report) in probes.iter().zip(&reports) {
        let case = probe.case;
        match &probe.expected {
            Expected::Instant(seconds) => {
                let expected = (seconds * 1000.0).round();
                assert_eq!(
                    report["ok"],
                    json!(true),
                    "case {case:?}: expected the call to be served, got {report}"
                );
                assert_eq!(
                    report["isDate"],
                    json!(true),
                    "case {case:?}: expected a Date for a member the model types as a \
                     timestamp, got {report}"
                );
                assert_eq!(
                    report["epochMs"].as_f64(),
                    Some(expected),
                    "case {case:?}: expected the instant the wire carried ({expected} ms), got \
                     {report}"
                );
            }
            Expected::SomeInstant => {
                assert_eq!(
                    report["isDate"],
                    json!(true),
                    "case {case:?}: expected a Date for a member the model types as a \
                     timestamp, got {report}"
                );
                let reported = report["epochMs"].as_f64().unwrap_or_default();
                assert!(
                    reported > 0.0,
                    "case {case:?}: expected an instant past the epoch, got {report}"
                );
                let iso = report["iso"].as_str().unwrap_or_default();
                assert!(
                    !iso.starts_with("1970"),
                    "case {case:?}: expected an instant of this account's own clock, got {report}"
                );
            }
            Expected::AsCarried(carried) => {
                assert_eq!(
                    report["isDate"],
                    json!(false),
                    "case {case:?}: expected no Date for a member the model does not type as a \
                     timestamp, got {report}"
                );
                assert_eq!(
                    report["plain"].as_f64(),
                    carried.as_f64(),
                    "case {case:?}: expected the value the wire carried ({carried}), got {report}"
                );
            }
            Expected::Refused(class, message) => {
                assert_eq!(
                    report["ok"],
                    json!(false),
                    "case {case:?}: expected the call to be refused, got {report}"
                );
                assert_eq!(
                    report["name"],
                    json!(class),
                    "case {case:?}: expected the error class {class:?}, got {report}"
                );
                assert_eq!(
                    report["message"],
                    json!(message),
                    "case {case:?}: expected the service's own message, got {report}"
                );
            }
        }
    }
}

/// RULE: a member of an answer that the called service's model types as a `timestamp` reaches a
/// node handler as the `Date` a v3 client materialises from the epoch seconds the protocol
/// carried, at whatever depth it sits and whichever service answered — and a member the model
/// types as anything else reaches it exactly as the wire carried it.
#[tokio::test]
async fn an_answers_timestamp_member_reaches_a_node_handler_as_a_date() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = router();
    let case = "the instants of an answered call";
    let table = "instant-items";
    let absent = "instant-items-absent";
    let backup = "instant-items-backup";
    let secret = "instant-secret";
    let group = "/aws/lambda/instant-group";

    // Everything the probes read, created where a client creates it.
    let table_created = given_table(&router, table).await;
    let created_secret = secrets(
        &router,
        case,
        "CreateSecret",
        &json!({
            "Name": secret,
            "SecretString": "held",
            // The idempotency token every SDK client of this service fills in itself, which a
            // request made straight on the wire has to carry.
            "ClientRequestToken": "4f2a2c4e-0f1b-4f2a-9f0e-1c2d3e4f5a6b",
        }),
    )
    .await;
    assert!(
        !created_secret["ARN"].is_null(),
        "setup: expected CreateSecret to answer the secret's ARN, got {created_secret}"
    );
    let described_secret = secrets(&router, case, "DescribeSecret", &json!({ "SecretId": secret }))
        .await;
    let secret_created = described_secret["CreatedDate"]
        .as_f64()
        .unwrap_or_else(|| panic!("setup: DescribeSecret must report CreatedDate: {described_secret}"));
    logs(
        &router,
        case,
        "CreateLogGroup",
        &json!({ "logGroupName": group }),
    )
    .await;
    let described_groups = logs(
        &router,
        case,
        "DescribeLogGroups",
        &json!({ "logGroupNamePrefix": group }),
    )
    .await;
    // `logGroups[].creationTime` is a `long` of epoch MILLISECONDS, not a timestamp member:
    // the negative row below is read against the very number the wire carried.
    let group_created = described_groups["logGroups"][0]["creationTime"].clone();
    assert!(
        group_created.as_f64().is_some(),
        "setup: DescribeLogGroups must report creationTime: {described_groups}"
    );

    given_function(&router, "instant-reader", "arn:aws:iam::000000000000:role/none").await;

    let probes = vec![
        Probe {
            case: "a timestamp member of a nested structure (DescribeTable)",
            service: "dynamodb",
            operation: "DescribeTable",
            input: json!({ "TableName": table }),
            path: json!(["Table", "CreationDateTime"]),
            expected: Expected::Instant(table_created),
        },
        Probe {
            case: "a long in that same answer, which is not a timestamp (ItemCount)",
            service: "dynamodb",
            operation: "DescribeTable",
            input: json!({ "TableName": table }),
            path: json!(["Table", "ItemCount"]),
            expected: Expected::AsCarried(json!(0)),
        },
        Probe {
            case: "a timestamp member a write answers (CreateBackup)",
            service: "dynamodb",
            operation: "CreateBackup",
            input: json!({ "TableName": table, "BackupName": backup }),
            path: json!(["BackupDetails", "BackupCreationDateTime"]),
            expected: Expected::SomeInstant,
        },
        Probe {
            case: "a timestamp member of a structure inside a list (ListBackups)",
            service: "dynamodb",
            operation: "ListBackups",
            input: json!({ "TableName": table }),
            path: json!(["BackupSummaries", 0, "BackupCreationDateTime"]),
            expected: Expected::SomeInstant,
        },
        Probe {
            case: "a top-level timestamp member of another AWS JSON version (DescribeSecret)",
            service: "secretsmanager",
            operation: "DescribeSecret",
            input: json!({ "SecretId": secret }),
            path: json!(["CreatedDate"]),
            expected: Expected::Instant(secret_created),
        },
        Probe {
            case: "a long carrying an epoch in milliseconds, which is not a timestamp",
            service: "logs",
            operation: "DescribeLogGroups",
            input: json!({ "logGroupNamePrefix": group }),
            path: json!(["logGroups", 0, "creationTime"]),
            expected: Expected::AsCarried(group_created.clone()),
        },
        Probe {
            case: "a table this account does not hold",
            service: "dynamodb",
            operation: "DescribeTable",
            input: json!({ "TableName": absent }),
            path: json!(["Table", "CreationDateTime"]),
            expected: Expected::Refused(
                ABSENT_TABLE_CODE,
                format!("Requested resource not found: Table: {absent} not found"),
            ),
        },
    ];
    probes_made(&router, "instant-reader", &probes).await;

    // The other half of the two rows whose instant this suite could not name before the run:
    // the backup the handler created is the one a client lists, and the instant it reports is
    // the one the handler read as a `Date`.
    let listed = dynamodb(&router, case, "ListBackups", &json!({ "TableName": table })).await;
    let summaries = listed["BackupSummaries"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        summaries.len(),
        1,
        "case {case:?}: expected the handler's own backup to be the one on the table, got {listed}"
    );
    assert_eq!(
        summaries[0]["BackupName"],
        json!(backup),
        "case {case:?}: expected the backup the handler created, got {listed}"
    );
    assert!(
        summaries[0]["BackupCreationDateTime"].as_f64().is_some(),
        "case {case:?}: expected the listing to report the backup's creation instant, got {listed}"
    );
}

/// The execution role of the second suite, the function that runs as it, and the inline policy
/// it carries its grant in.
const ROLE_NAME: &str = "lambda-instant-reader";
const GRANTED_FUNCTION: &str = "instant-reader-with-a-role";
const INLINE_POLICY: &str = "granted-table-reads";

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

/// An identity policy granting the role `dynamodb:DescribeTable` on ONE table and nothing
/// else.
fn describe_grant(table: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "dynamodb:DescribeTable",
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
        "User: arn:aws:sts::{ACCOUNT}:assumed-role/{ROLE_NAME}/{GRANTED_FUNCTION} is not \
         authorized to perform: {action} on resource: {resource} because no identity-based \
         policy allows the {action} action"
    )
}

/// RULE: the call a running handler makes with the SDK its runtime supplies is signed as the
/// function's execution-role session, so DynamoDB decides it against the identity-based
/// policies in force on that role — answering the table's description, with its
/// `CreationDateTime` as the `Date` a v3 client materialises, for the table a statement in
/// force covers, and refusing every other with its own authorization refusal.
#[tokio::test]
async fn an_in_handler_describe_table_is_decided_against_the_execution_roles_grant() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = router();
    let granted = "role-granted-items";
    let withheld = "role-withheld-items";

    // The role the function runs as, written with IAM's own operations, so the session the
    // runtime mints speaks for the record `iam:CreateRole` wrote.
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
        format!("arn:aws:iam::{ACCOUNT}:role/{ROLE_NAME}"),
        "setup: expected IAM to record the role under its own ARN, got {recorded:?}"
    );
    iam(
        &router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", INLINE_POLICY),
            ("PolicyDocument", &describe_grant(granted)),
        ],
    )
    .await;

    let granted_created = given_table(&router, granted).await;
    given_table(&router, withheld).await;
    given_function(&router, GRANTED_FUNCTION, &recorded).await;

    let probes = vec![
        Probe {
            case: "the table the role's document covers",
            service: "dynamodb",
            operation: "DescribeTable",
            input: json!({ "TableName": granted }),
            path: json!(["Table", "CreationDateTime"]),
            expected: Expected::Instant(granted_created),
        },
        Probe {
            case: "a table of the same account the document does not cover",
            service: "dynamodb",
            operation: "DescribeTable",
            input: json!({ "TableName": withheld }),
            path: json!(["Table", "CreationDateTime"]),
            expected: Expected::Refused(
                DENIAL_CODE,
                implicit_denial("dynamodb:DescribeTable", &table_arn(withheld)),
            ),
        },
    ];
    probes_made(&router, GRANTED_FUNCTION, &probes).await;
}
