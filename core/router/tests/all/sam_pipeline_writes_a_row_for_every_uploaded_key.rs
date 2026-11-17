// SPDX-License-Identifier: CC0-1.0

//! The whole pipeline an application deploys as ONE `AWS::Serverless-2016-10-31` template —
//! a secret, a table, an execution role, an `AWS::Serverless::Function` carrying `InlineCode`,
//! the grant Amazon S3 needs on that function, and a bucket whose `NotificationConfiguration`
//! names it — driven the way a client drives it: objects written into the bucket the stack
//! published, and the rows the handler wrote read back through Amazon DynamoDB's own `GetItem`.
//!
//! The rule under test: **a macro-carrying template is deployed as its EXPANSION, so the
//! function the transform emits is a concrete, invocable function whose `Environment.Variables`
//! carry the values the intrinsics in them resolve to against the stack's own resources, and
//! from the instant the stack reports `CREATE_COMPLETE` every accepted write into that stack's
//! bucket publishes its own event to the function the bucket's stored configuration names,
//! carrying the written key and the bucket's name — so what the handler reads out of one service
//! with the identifiers the deployment handed it lands in the other, under the key the write
//! named, and a client reads it back through the owning service's own API.** Every value on that
//! path is the request's or the stack's own: the key, the bucket name and the secret round-trip
//! unchanged, and the read of a key nothing has written is SERVED carrying no `Item` rather than
//! refused (`GetItem` output member `Item` target `AttributeMap`, with
//! `ResourceNotFoundException` HTTP 400 reserved for a table the service does not hold —
//! `smithy_query(query="operation", operation="GetItem")`).
//!
//! Five crates have to agree for that to hold — CloudFormation expands the template and makes
//! every create, Secrets Manager holds the secret and serves the handler's read of it, DynamoDB
//! holds the table and takes the handler's write, Amazon S3 holds the bucket and publishes its
//! object events, and AWS Lambda holds the grant, the resolved environment and the runtime the
//! handler runs on — so the suite lives here: a crate-local suite can only ever see one side of
//! it.
//!
//! Siblings reach one side each, and this suite is maintained with them:
//! [`crate::cloudformation_sam_stack_provisions_its_declared_resources`] pins the estate on one
//! declared bucket whose grant is scoped by source ARN,
//! [`crate::cloudformation_sam_secret_bucket_table_estate`] pins the same four-service estate
//! with the handler invoked DIRECTLY rather than by an object event,
//! [`crate::cfn_deployed_s3_notification_to_lambda_pipeline`] pins the notification-driven
//! handler of a template carrying no transform at all,
//! [`crate::s3_notification_delivers_once_per_object_write`] pins the delivery COUNT a repeated
//! write produces, and [`crate::s3_notification_event_classes_run_their_lambda_targets`] pins
//! which event classes a write is published under. What only this suite reaches is those legs
//! JOINED — the notification-driven expanded function reading the stack's own secret — spanned
//! along the forms one template states the pipeline in, with the effect of every uploaded key
//! read back out of the table the same stack published. A sixth overlapping copy is not wanted:
//! a form the rule generalises over belongs in [`ROWS`] here, or in whichever of those suites
//! owns the side it varies.
//!
//! The rows of [`ROWS`] differ along what the rule generalises over, and each is driven by a test
//! of its own: whether the template NAMES
//! the bucket or leaves the name to CloudFormation, how the grant on the function scopes the
//! service principal (by source ARN, by source account), which `Event` value the configuration
//! declares (the wildcard of the create class, and the single detail a `PutObject` publishes),
//! the shape of the `SecretString` the stack declares (a JSON document, an opaque string that is
//! no JSON at all), and the form and number of the keys written into the bucket (a prefixed key,
//! a key with no prefix at all, a deeply nested one). Every expected value is computed here from
//! what the row declared or from the owning service's own read, never taken out of the answer
//! being checked.
//!
//! Every request below is dispatched through a BOXED future ([`Box::pin`]), and the suite runs on
//! a thread sized for the assembled router ([`driven`], the one copy every suite driving this
//! chain shares): that router's dispatch future carries the whole emulator's handler state
//! inline, and this suite nests a chain of them — CloudFormation into five services, S3's
//! delivery into Lambda, and the handler's own SDK calls back into two of them — so the default
//! stack of a test thread does not hold it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{elements, form_encode, outputs_of};
use crate::rds_docdb_neptune_signing_name_routing::authorization;
use crate::s3_notification_invokes_its_lambda_target::driven;

/// The Region every request here is signed for — the Region [`stack_status`] and
/// [`authorization`] sign for — and so the Region every stack below deploys into.
const REGION: &str = "us-east-1";

/// The `StackStatus` the rule is stated about, and the other statuses a create settles into —
/// reaching one of those is an answer, so nothing waits on past it
/// (`smithy_query(query="shape", shape="StackStatus")` lists them beside the `*_IN_PROGRESS`
/// values a create passes through).
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const OTHER_TERMINAL_CREATE_STATUSES: [&str; 3] =
    ["CREATE_FAILED", "ROLLBACK_COMPLETE", "ROLLBACK_FAILED"];

/// The macro the AWS Serverless Application Model is published as, as a template's `Transform`
/// section names it, and the capabilities a client acknowledges a template declaring a macro and
/// a named IAM resource with.
const SAM_TRANSFORM: &str = "AWS::Serverless-2016-10-31";
const CAPABILITY_IAM: &str = "CAPABILITY_IAM";
const CAPABILITY_NAMED_IAM: &str = "CAPABILITY_NAMED_IAM";
const CAPABILITY_AUTO_EXPAND: &str = "CAPABILITY_AUTO_EXPAND";

/// The logical ids every template below declares.
const SECRET: &str = "MySecret";
const TABLE: &str = "MyTable";
const ROLE: &str = "ExecRole";
const FUNCTION: &str = "Fn";
const PERMISSION: &str = "FnPermission";
const BUCKET: &str = "MyBucket";

/// The output keys every template publishes its identifiers under.
const BUCKET_NAME_OUTPUT: &str = "BucketName";
const TABLE_NAME_OUTPUT: &str = "TableName";
const SECRET_ARN_OUTPUT: &str = "SecretArn";
const FUNCTION_NAME_OUTPUT: &str = "FunctionName";

/// The environment variables the declared function carries, each declared as an intrinsic over a
/// sibling resource, and so the two the expansion has to resolve.
const SECRET_ARN_VARIABLE: &str = "SECRET_ARN";
const TABLE_NAME_VARIABLE: &str = "TABLE_NAME";

/// The runtime, handler and timeout every function below declares. What a managed runtime
/// supplies is pinned by the Lambda crate's own suites; here they are held fixed. The timeout is
/// far past what one run of this handler needs on any host: nothing here is stated about how fast
/// an interpreter gets through one, so a loaded host must not turn the function's own budget into
/// the thing under test.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";
const FUNCTION_TIMEOUT: u64 = 120;

/// The service principal Amazon S3 publishes a bucket's notifications under, and the action it
/// performs on a function destination ("for Amazon Web Services services, the principal is a
/// domain-style identifier that the service defines, such as s3.amazonaws.com",
/// `AddPermission$Principal`).
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The `eventName` a `PutObject` is published under — the single detail of the create class that
/// a put produces, whichever of the two `Event` values the configuration declares.
const PUT_EVENT_NAME: &str = "ObjectCreated:Put";

/// The AWS-managed policy a function's execution role carries for its logging, named as a client
/// names it.
const BASIC_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// How long this suite keeps looking for work another service's clock owes — a stack settling, a
/// function reported `Active`, an asynchronously delivered object event — and how often it looks
/// while waiting.
///
/// Generous on purpose: no span is asserted on, only refused to be waited out forever, and a
/// host running every suite of this workspace at once takes far longer over one asynchronous
/// invocation than an idle one does. It stays short of the span that would make the wait itself
/// the thing being reported, so work that never comes is still answered as a failure, naming the
/// deadline it exhausted.
const PATIENCE: Duration = Duration::from_secs(180);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// How a row scopes the invoke grant the bucket's function destination needs.
#[derive(Debug, Clone, Copy)]
enum Grant {
    /// `SourceArn`, naming the bucket the notification comes from — which a row can only state
    /// about a bucket whose name the template itself declares.
    SourceArn,
    /// `SourceAccount`, naming the account the bucket belongs to, which needs no bucket name and
    /// so leaves the name to CloudFormation.
    SourceAccount,
}

/// One row: a SAM template in one of the forms the rule generalises over, and the keys written
/// into the bucket it publishes.
#[derive(Debug)]
struct Row {
    case: &'static str,
    /// The stack name, and the prefix of every name the template declares, so no row reads what
    /// another row left behind.
    stack: &'static str,
    /// The bucket name the template declares, or `None` where it leaves the name to
    /// CloudFormation.
    declared_bucket: Option<&'static str>,
    grant: Grant,
    /// The `Event` the single `LambdaConfigurations` entry is declared for.
    event: &'static str,
    /// The `SecretString` the stack's secret is declared with — the value the handler can only
    /// have got out of Secrets Manager.
    secret: &'static str,
    /// The keys written into the published bucket, each of which owes one row in the table.
    keys: &'static [&'static str],
}

/// The forms the pipeline is stated in. Each row is driven by a test of its own, on an emulator
/// and a stack of its own, so a row that fails names itself and neither row waits on the other.
const ROWS: [Row; 2] = [
    Row {
        case: "a bucket named by CloudFormation, the grant scoped by source account, the \
               wildcard create event, a JSON secret document",
        stack: "sam-pipeline-generated-bucket",
        declared_bucket: None,
        grant: Grant::SourceAccount,
        event: "s3:ObjectCreated:*",
        secret: r#"{"apiKey":"only-in-the-secret","rotations":3}"#,
        keys: &["incoming/file1.txt", "top-level.bin", "deep/2026/01/report.json"],
    },
    Row {
        case: "a bucket the template names, the grant scoped by source ARN, the put-only create \
               event, a secret that is no JSON document",
        stack: "sam-pipeline-declared-bucket",
        declared_bucket: Some("sam-pipeline-declared-bucket-data"),
        grant: Grant::SourceArn,
        event: "s3:ObjectCreated:Put",
        secret: "plain-value/with+punctuation=1 and a space",
        keys: &["uploads/report-2026.csv", "single.txt"],
    },
];

/// The handler every row deploys as the function's `InlineCode`: it reads the secret the stack
/// named it and, for every record it is handed, writes one item into the table the stack named
/// it, carrying the key, the bucket and the event name that record reports. Everything it writes
/// comes from the event or from another service's answer.
const SECRET_READING_WRITER: &str = r#"
import os

import boto3


def handler(event, context):
    secret = boto3.client("secretsmanager").get_secret_value(
        SecretId=os.environ["SECRET_ARN"]
    )["SecretString"]
    table = os.environ["TABLE_NAME"]
    client = boto3.client("dynamodb")
    written = []
    for record in event["Records"]:
        key = record["s3"]["object"]["key"]
        client.put_item(
            TableName=table,
            Item={
                "pk": {"S": key},
                "secret": {"S": secret},
                "bucket": {"S": record["s3"]["bucket"]["name"]},
                "event": {"S": record["eventName"]},
            },
        )
        written.append(key)
    return {"written": written}
"#;

/// The first row: a bucket CloudFormation names, notified of the create-class wildcard, whose
/// function is granted to the service by source account.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn a_generated_bucket_pipeline_writes_a_row_for_every_uploaded_key() {
    driven("sam-pipeline-generated-bucket", || {
        Box::pin(driving(&ROWS[0]))
    });
}

/// The second row: a bucket the template names, notified of the put-only create event, whose
/// function is granted to the service by source ARN, under a secret that is no JSON document.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn a_declared_bucket_pipeline_writes_a_row_for_every_uploaded_key() {
    driven("sam-pipeline-declared-bucket", || {
        Box::pin(driving(&ROWS[1]))
    });
}

/// One row, end to end: the pipeline its template declares really runs, and what it writes is
/// read back out of the table that same stack published.
async fn driving(row: &'static Row) {
    let router = emulator();
    let case = row.case;
    let outputs = deploy(&router, case, row.stack, &template(row)).await;

    // Requirement one: the identifiers a client of this stack consumes are published, each
    // carrying a value.
    let bucket = published(case, &outputs, BUCKET_NAME_OUTPUT);
    let table = published(case, &outputs, TABLE_NAME_OUTPUT);
    let secret_arn = published(case, &outputs, SECRET_ARN_OUTPUT);
    let function = published(case, &outputs, FUNCTION_NAME_OUTPUT);
    if let Some(declared) = row.declared_bucket {
        assert_eq!(
            bucket, declared,
            "case {case:?}: expected the declared bucket name {declared:?}, got {bucket:?}"
        );
    }

    // Requirement two: the expansion emitted a function Lambda holds, ready to be invoked, whose
    // environment carries what the intrinsics in it resolve to — the stack's own secret ARN and
    // table name, not the intrinsics themselves.
    let configuration = active_function(&router, case, &function).await;
    assert_eq!(
        configuration.get("State").and_then(Value::as_str),
        Some("Active"),
        "case {case:?}: expected Lambda to report {function:?} Active, got {configuration}"
    );
    let variables = configuration
        .get("Environment")
        .and_then(|environment| environment.get("Variables"))
        .cloned()
        .unwrap_or(Value::Null);
    for (variable, expected) in [
        (SECRET_ARN_VARIABLE, &secret_arn),
        (TABLE_NAME_VARIABLE, &table),
    ] {
        assert_eq!(
            variables.get(variable).and_then(Value::as_str),
            Some(expected.as_str()),
            "case {case:?}: expected {variable} to resolve to {expected:?}, got {variables}"
        );
    }

    // The secret the handler is to read, as Secrets Manager answers for the published ARN.
    let (status, held) =
        secretsmanager(&router, "GetSecretValue", &json!({"SecretId": secret_arn})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetSecretValue on the published ARN \
         {secret_arn:?}, got {status} with {held}"
    );
    assert_eq!(
        held.get("SecretString").and_then(Value::as_str),
        Some(row.secret),
        "case {case:?}: expected the declared SecretString {:?}, got {held}",
        row.secret
    );

    // The first data-plane calls a client makes with the stack's own output: a read of every key
    // the writes below will produce, each SERVED and carrying no item yet.
    for key in row.keys {
        let (status, answered, item) = read_item(&router, &table, &item_key(key)).await;
        assert_eq!(
            status, 200,
            "case {case:?}, key {key:?}: expected HTTP 200 from GetItem on the published table \
             name {table:?}, got {status} with {answered}"
        );
        assert_eq!(
            item, None,
            "case {case:?}, key {key:?}: nothing has written the key yet, so the answer must \
             carry no Item, got {answered}"
        );
    }

    // The writes, into the bucket the stack published.
    for key in row.keys {
        let (status, body) = s3(
            &router,
            "PUT",
            &format!("/{bucket}/{key}"),
            &format!("the object {key} this stack was deployed to process\n"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from PutObject({bucket:?}, {key:?}), got {status} \
             with {body}"
        );
    }

    // What the handler then wrote for each of them, read out of the published table through
    // DynamoDB's own API: the key the write named, the bucket it went into, the event it was
    // published under, and the secret value no part of the event carries.
    for key in row.keys {
        let item = awaited_item(&router, case, &table, &item_key(key)).await;
        assert_eq!(
            item,
            Some(json!({
                "pk": { "S": key },
                "secret": { "S": row.secret },
                "bucket": { "S": bucket.clone() },
                "event": { "S": PUT_EVENT_NAME },
            })),
            "case {case:?}: expected the handler's item for the record of {key:?}, got {item:?}"
        );
    }
}

/// The template a row deploys: the secret, the table, the execution role, the serverless
/// function carrying its inline code and its intrinsic environment, the grant Amazon S3 needs on
/// that function, and the bucket whose object events it is the destination of.
fn template(row: &Row) -> String {
    let mut permission = json!({
        "FunctionName": { "Ref": FUNCTION },
        "Action": INVOKE_ACTION,
        "Principal": S3_PRINCIPAL,
    });
    match row.grant {
        // A bucket is global, so its ARN names no Region and no account.
        Grant::SourceArn => {
            let declared = row
                .declared_bucket
                .expect("a source-ARN grant names the bucket the template declares");
            permission["SourceArn"] = json!(format!("arn:aws:s3:::{declared}"));
        }
        Grant::SourceAccount => {
            permission["SourceAccount"] = json!({ "Ref": "AWS::AccountId" });
        }
    }

    let mut bucket = json!({
        "NotificationConfiguration": {
            "LambdaConfigurations": [{
                "Event": row.event,
                "Function": { "Fn::GetAtt": [FUNCTION, "Arn"] },
            }],
        },
    });
    if let Some(declared) = row.declared_bucket {
        bucket["BucketName"] = json!(declared);
    }

    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Transform": SAM_TRANSFORM,
        "Resources": {
            SECRET: {
                "Type": "AWS::SecretsManager::Secret",
                "Properties": {
                    "Name": format!("{}-secret", row.stack),
                    "SecretString": row.secret,
                },
            },
            TABLE: {
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "BillingMode": "PAY_PER_REQUEST",
                    "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
                    "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
                },
            },
            ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": format!("{}-role", row.stack),
                    "AssumeRolePolicyDocument": lambda_trust_policy(),
                    "ManagedPolicyArns": [BASIC_EXECUTION_POLICY],
                    "Policies": [{
                        "PolicyName": "read-the-secret-and-write-the-table",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [
                                {
                                    "Effect": "Allow",
                                    "Action": "secretsmanager:GetSecretValue",
                                    "Resource": { "Ref": SECRET },
                                },
                                {
                                    "Effect": "Allow",
                                    "Action": "dynamodb:PutItem",
                                    "Resource": { "Fn::GetAtt": [TABLE, "Arn"] },
                                },
                            ],
                        },
                    }],
                },
            },
            FUNCTION: {
                "Type": "AWS::Serverless::Function",
                "Properties": {
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Role": { "Fn::GetAtt": [ROLE, "Arn"] },
                    "InlineCode": SECRET_READING_WRITER,
                    "Environment": {
                        "Variables": {
                            SECRET_ARN_VARIABLE: { "Ref": SECRET },
                            TABLE_NAME_VARIABLE: { "Ref": TABLE },
                        },
                    },
                },
            },
            PERMISSION: {
                "Type": "AWS::Lambda::Permission",
                "Properties": permission,
            },
            BUCKET: {
                "Type": "AWS::S3::Bucket",
                // S3 admits a function destination only while that function's own policy lets
                // the service invoke it, so the grant has to be in place before the bucket is
                // configured.
                "DependsOn": [PERMISSION],
                "Properties": bucket,
            },
        },
        "Outputs": {
            BUCKET_NAME_OUTPUT: { "Value": { "Ref": BUCKET } },
            TABLE_NAME_OUTPUT: { "Value": { "Ref": TABLE } },
            SECRET_ARN_OUTPUT: { "Value": { "Ref": SECRET } },
            FUNCTION_NAME_OUTPUT: { "Value": { "Ref": FUNCTION } },
        },
    })
    .to_string()
}

/// The emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`): every
/// in-process call CloudFormation, S3 or a handler makes is dispatched back into this same
/// router, so the resources this suite reads are the ones the stack created.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the credential
    // scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one Amazon DynamoDB operation over its `awsJson1_0` wire.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("dynamodb.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .header("authorization", authorization("dynamodb"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(http_request)).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// Call one AWS Secrets Manager operation over its `awsJson1_1` wire.
async fn secretsmanager(
    router: &MultiServiceRouter,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("secretsmanager.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("secretsmanager.{operation}"))
        .header("authorization", authorization("secretsmanager"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(http_request)).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// One AWS Lambda read over its `restJson1` wire.
async fn lambda(router: &MultiServiceRouter, path: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let (parts, body) = response.into_parts();
    let payload: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (parts.status.as_u16(), payload)
}

/// One call on Amazon S3's REST surface, path-style.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("content-type", "application/octet-stream")
        .header("authorization", authorization("s3"))
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// The trust policy a role Lambda is handed has to carry.
fn lambda_trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// Poll `stack` until it first reports a status a create SETTLES into, and answer that status
/// with the `DescribeStacks` answer it was read from. The reader is the shared one
/// ([`stack_status`]); only the deadline is this suite's, because the chain a row deploys settles
/// through four dependency levels across five services and a host running every suite of this
/// workspace at once takes far longer over them than an idle one does. The wait is on the
/// CONDITION; nothing asserts how long it took.
async fn settled(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let looks = PATIENCE.as_millis() / LOOK_INTERVAL.as_millis();
    let mut last = stack_status(router, stack).await;
    for _ in 0..looks {
        if last.0 == CREATE_COMPLETE || OTHER_TERMINAL_CREATE_STATUSES.contains(&last.0.as_str()) {
            return last;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
        last = stack_status(router, stack).await;
    }
    last
}

/// Deploy `template` as `stack` and answer the outputs it publishes, having first established
/// that it reached [`CREATE_COMPLETE`].
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
) -> BTreeMap<String, String> {
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", template),
            // The template declares a macro, a role the transform may extend, and a NAMED IAM
            // resource — which is what these three capabilities acknowledge.
            ("Capabilities.member.1", CAPABILITY_IAM),
            ("Capabilities.member.2", CAPABILITY_NAMED_IAM),
            ("Capabilities.member.3", CAPABILITY_AUTO_EXPAND),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {answered}"
    );

    let (reached, described) = settled(router, stack).await;
    let failures = if reached == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {described} with \
         resource failures {failures:?}"
    );
    outputs_of(&described)
}

/// The value one of the stack's outputs publishes.
fn published(case: &str, outputs: &BTreeMap<String, String>, key: &str) -> String {
    let value = outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {outputs:?}"));
    assert!(
        !value.is_empty(),
        "case {case:?}: expected the {key} output to carry a value, got {outputs:?}"
    );
    value
}

/// The `Key` a `GetItem` of one uploaded object's row is made with.
fn item_key(key: &str) -> Value {
    json!({ "pk": { "S": key } })
}

/// Poll `GetFunctionConfiguration` until Lambda first reports `name` `Active` — "the function is
/// ready to be invoked" (`FunctionConfiguration$State`) — and answer the configuration it
/// reported then. The span a created function spends `Pending` is that service's own, so no fixed
/// sleep names the instant it ends.
async fn active_function(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let looks = PATIENCE.as_millis() / LOOK_INTERVAL.as_millis();
    let mut last = Value::Null;
    for _ in 0..looks {
        let (status, configuration) =
            lambda(router, &format!("/2015-03-31/functions/{name}/configuration")).await;
        // A refused poll is not a function still coming up, so it is reported as the refusal it
        // is rather than as a state that never became Active.
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetFunctionConfiguration on {name:?}, got \
             {status} with {configuration}"
        );
        if configuration.get("State").and_then(Value::as_str) == Some("Active") {
            return configuration;
        }
        last = configuration;
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!(
        "case {case:?}: {name:?} was still not Active after {PATIENCE:?} of looking; last \
         configuration {last}"
    );
}

/// The item `table` holds under `key` right now, read through DynamoDB's own `GetItem`, with the
/// HTTP status that read answered. A table the service does not hold is what the rule forbids, so
/// the status is answered rather than asserted here.
async fn read_item(
    router: &MultiServiceRouter,
    table: &str,
    key: &Value,
) -> (u16, Value, Option<Value>) {
    let (status, answered) = dynamodb(
        router,
        "GetItem",
        &json!({ "TableName": table, "Key": key, "ConsistentRead": true }),
    )
    .await;
    let item = answered.get("Item").cloned();
    (status, answered, item)
}

/// Look for the item the handler owes under `key` until it is there, answering it. The wait is on
/// the CONDITION; nothing asserts how long it took, and an item that never comes is reported as
/// the exhausted deadline it is rather than as an item of the wrong shape.
async fn awaited_item(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    key: &Value,
) -> Option<Value> {
    let looks = PATIENCE.as_millis() / LOOK_INTERVAL.as_millis();
    for _ in 0..looks {
        let (status, answered, item) = read_item(router, table, key).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetItem on the published table name \
             {table:?}, got {status} with {answered}"
        );
        if item.is_some() {
            return item;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!(
        "case {case:?}: {table:?} still held no item under {key} after {PATIENCE:?} of looking, \
         so the write the notification owes never landed"
    );
}
