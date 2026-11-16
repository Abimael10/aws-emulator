// SPDX-License-Identifier: CC0-1.0

//! A SAM template that declares a function BESIDE a secret, a bucket and a table: every one
//! of those resources is created in the service that owns it, the stack completes rather than
//! rolling back, and the function the stack deployed really reaches all three.
//!
//! The rules under test:
//!
//! * **A transformed template's declarations are created where they live.** A stack whose
//!   template declares the `AWS::Serverless-2016-10-31` transform is deployed as its EXPANSION,
//!   so each resource it declares is created in the service that owns that resource — the
//!   expanded function in AWS Lambda, and every concrete type beside it in its own service —
//!   and the stack reaches `CREATE_COMPLETE` only once each of them is readable there under the
//!   physical id CloudFormation published for it. A declaration the deployment cannot create in
//!   its owning service is a failed resource, which rolls the whole stack back, so a completed
//!   stack is a claim about all four services at once. The function such a stack deploys runs
//!   as the role the transform synthesized from its `Policies`, so the calls its handler makes
//!   into those same services are served and the work they do lands where a client reads it.
//! * **A rotation of that stack's secret is admitted only through a function this service may
//!   invoke.** A rotation is driven by INVOKING the rotation function — "you also need
//!   `lambda:InvokeFunction` permissions on the rotation function" (`RotateSecret`, Secrets
//!   Manager's Smithy model), granted by that FUNCTION's own resource-based policy to the
//!   service principal the invocation is made as — so a `RotationLambdaARN` naming a function
//!   Lambda does not hold, or one whose policy admits no such invocation, is refused with
//!   nothing about the secret changed, and the identical call once that policy admits this
//!   service is served and stakes `AWSPENDING` on the version the rotation will create.
//!
//! Five crates have to agree for that to hold — CloudFormation expands the macro and makes
//! every create, Secrets Manager holds the secret and serves the handler's read of it, Amazon
//! S3 holds the bucket and takes the handler's object, DynamoDB holds the table and takes the
//! handler's item, and IAM answers for the role the transform synthesized — so the suite lives
//! here: a crate-local suite can only ever see one side of it. Its siblings reach one side
//! each: [`crate::cloudformation_sam_function_writes_its_table`] pins the expansion of a
//! function and the table it writes, and [`crate::cloudformation_provisions_secrets_manager_secrets`]
//! pins a secret declared in a plain template. What only this suite reaches is the four of them
//! declared TOGETHER in one transformed template, which is the estate an application deploys.
//!
//! The second rule has siblings of its own, and the rows here are the ones they do not reach:
//! the Secrets Manager crate's `secret_rotation_grant_added_after_refusal` drives the
//! refusal-then-grant sequence on a secret and a function a CLIENT created, its
//! `secret_refusal_message_member` pins where each refusal states its sentence, and
//! [`crate::cloudformation_rotation_grant_forms_admit_the_rotation`] spans the FORMS a grant on
//! the function takes. What is only here is the `RotationLambdaARN` itself as the axis — the
//! stack's own function, a function ARN Lambda holds nothing under, one naming a function of
//! another account — on a secret and a rotation function one SAM stack deployed, which is how an
//! application's rotation is configured. A `RotationLambdaARN` that is no function ARN at all is
//! refused too, under a sentence of its own, but that sentence is the subject of the open issue
//! which owns the wording of a malformed rotation ARN, so nothing here pins it.
//!
//! The rows of the first table differ along what its rule generalises over: how the secret's
//! value is stated (a value the template spells, and one the owning service generates), whether
//! the template NAMES the bucket or leaves the name to CloudFormation, which form the table is
//! declared in (the concrete `AWS::DynamoDB::Table`, and the `AWS::Serverless::SimpleTable` the
//! transform expands into one), and how the function's grants are stated (the published policy
//! templates, and inline statement documents). The rows of the second differ along the form the
//! `RotationLambdaARN` takes — the stack's own function before anything grants the invocation, a
//! well-formed function ARN no function is held under, one naming a function of another account
//! — and the table ends with the accepted call, so a refusal and an admission are told apart.
//! Every expected value is computed here from what the row declares or from the owning service's
//! own read, never taken out of the answer being checked.
//!
//! Every request below is dispatched through a BOXED future ([`Box::pin`]), and the whole
//! suite runs on a thread sized for the assembled router: that router's dispatch future
//! carries the whole emulator's handler state inline, and this suite nests a chain of them —
//! CloudFormation into four services, and the handler's own SDK calls back into three of
//! them — so the default stack of a test thread does not hold it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{elements, form_encode, physical_ids_of};

/// The Region every request here is signed for, and so the Region the stack's resources
/// belong to. It is the Region the shared stack reader ([`stack_status`]) signs for too, so
/// the stack this suite deploys is the stack it reports.
const REGION: &str = "us-east-1";

/// The `StackStatus` a deployed stack must reach — the rule's own claim, and the observation
/// a rolled-back deployment fails.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The logical ids every row declares its four resources under.
const SECRET_ID: &str = "Config";
const BUCKET_ID: &str = "Uploads";
const TABLE_ID: &str = "Audit";
const FUNCTION_ID: &str = "Processor";

/// The partition key the audit table is declared with, which is the attribute the handler
/// keys each item by.
const TABLE_KEY: &str = "pk";

/// The prefix the handler keys the object it writes under.
const OBJECT_PREFIX: &str = "processed/";

/// How long one invocation of the handler is given, as the function's own `Timeout`: well past
/// what a run needs on any host, because what is under test is the estate the stack deployed
/// and not how fast an interpreter gets through a handler on a machine running every other
/// suite of this workspace beside it.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long this suite looks for work the clock owes — a stack's resources coming up, a
/// function reported `Active`, a table reported `ACTIVE` — and how often it looks while
/// waiting. Generous, and deliberately its own rather than the shared stack reader's: no span
/// here is one the suite asserts on, only one it refuses to wait out forever, and this estate's
/// five resources are created on a host that may be running every other suite of the workspace
/// at the same time.
const WAIT: Duration = Duration::from_secs(180);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// The stack the emulator's own boot path gives the thread that assembles the router
/// (`core/router/src/main.rs`), kept at the size the sibling cross-service suites use.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The handler every row deploys: it reads the secret the same stack declared, writes one
/// object into that stack's bucket and one item into that stack's table, and returns what it
/// recorded — so the answer and the two stores can be read against each other.
///
/// Nothing about the secret's material travels into either store: what is recorded is the
/// secret's NAME and the LENGTH of the value read, both of which the suite computes itself from
/// the owning service's own answer.
const PROCESSING_HANDLER: &str = r#"
import json
import os

import boto3


def handler(event, context):
    key = event["key"]
    answered = boto3.client("secretsmanager").get_secret_value(
        SecretId=os.environ["SECRET_ARN"]
    )
    record = {
        "key": key,
        "secret_name": answered["Name"],
        "value_length": len(answered["SecretString"]),
    }
    boto3.client("s3").put_object(
        Bucket=os.environ["UPLOAD_BUCKET"],
        Key="processed/" + key,
        Body=json.dumps(record, sort_keys=True).encode("utf-8"),
    )
    boto3.client("dynamodb").put_item(
        TableName=os.environ["AUDIT_TABLE"],
        Item={
            "pk": {"S": key},
            "SecretName": {"S": record["secret_name"]},
            "ValueLength": {"N": str(record["value_length"])},
        },
    )
    return record
"#;

/// Run `body` to completion on a thread with [`BOOT_STACK_SIZE`], on the current-thread
/// runtime the handler's own invocation runs on: an invocation is a task of its own, so it runs
/// on this same thread and needs the same depth.
fn driven<Body>(name: &'static str, body: Body)
where
    Body: FnOnce() -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = ()>>> + Send + 'static,
{
    ::std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(move || {
            let runtime = ::tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(body());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}

/// The emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`): the
/// handler's SDK calls are dispatched back into this same router, so the stores it writes are
/// the stores this suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
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

/// Call one AWS Secrets Manager operation over its `awsJson1_1` wire.
async fn secretsmanager(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("secretsmanager.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("secretsmanager.{operation}"))
        .header("authorization", authorization("secretsmanager"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Call one DynamoDB operation over its `awsJson1_0` wire.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("dynamodb.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .header("authorization", authorization("dynamodb"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Amazon S3 request, path style, as its status and the body it answered with.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> (u16, String) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .body(Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let (parts, body) = response.into_parts();
    (
        parts.status.as_u16(),
        String::from_utf8_lossy(&body).to_string(),
    )
}

/// One Lambda `restJson1` call, as its status, its headers and its parsed body.
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
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(body.to_vec()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let (parts, body) = response.into_parts();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// What the row expects the secret's value to be: the string the template spells, or a
/// generated password of the declared length, whose SHAPE is all a template that asked the
/// owning service to generate it can state.
enum ExpectedValue {
    Verbatim(&'static str),
    GeneratedPassword { length: usize },
}

/// One row of the table: the declarations its template carries, and what the rule says the
/// four services must then hold.
struct Row {
    case: &'static str,
    /// The secret's `Properties`, built from the stack name the row runs under.
    secret: fn(&str) -> Value,
    /// The bucket's `Properties`, built from the same stack name.
    bucket: fn(&str) -> Value,
    /// The table's declaration — its type and properties, so a row may declare either the
    /// concrete table type or the SAM one the transform expands into it.
    table: fn() -> Value,
    /// The function's `Policies`, which is what the role the transform synthesizes grants.
    policies: fn() -> Value,
    /// The NAME the secret must end up carrying, `None` where the declaration states none and
    /// CloudFormation generates it.
    expected_secret_name: fn(&str) -> Option<String>,
    /// The value the secret must hold, which is what the handler reads.
    expected_value: ExpectedValue,
}

/// The published policy templates for the three services the handler reaches, each scoped to
/// the resource this stack declared ("AWS SAM policy templates", AWS SAM Developer Guide).
fn policy_templates() -> Value {
    json!([
        { "AWSSecretsManagerGetSecretValuePolicy": { "SecretArn": { "Ref": SECRET_ID } } },
        { "DynamoDBCrudPolicy": { "TableName": { "Ref": TABLE_ID } } },
        { "S3CrudPolicy": { "BucketName": { "Ref": BUCKET_ID } } }
    ])
}

/// The same three grants written as an inline statement document, the other form a `Policies`
/// entry takes: "every entry that is not an AWS managed policy becomes one inline policy on
/// that role".
fn inline_statements() -> Value {
    json!([{
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": "secretsmanager:GetSecretValue",
                "Resource": { "Ref": SECRET_ID }
            },
            {
                "Effect": "Allow",
                "Action": ["dynamodb:PutItem", "dynamodb:GetItem"],
                "Resource": { "Fn::GetAtt": [TABLE_ID, "Arn"] }
            },
            {
                "Effect": "Allow",
                "Action": "s3:PutObject",
                "Resource": { "Fn::Sub": format!("arn:${{AWS::Partition}}:s3:::${{{BUCKET_ID}}}/*") }
            }
        ]
    }])
}

/// The concrete table type, with the key schema spelled out.
fn concrete_table() -> Value {
    json!({
        "Type": "AWS::DynamoDB::Table",
        "Properties": {
            "AttributeDefinitions": [{ "AttributeName": TABLE_KEY, "AttributeType": "S" }],
            "KeySchema": [{ "AttributeName": TABLE_KEY, "KeyType": "HASH" }],
            "BillingMode": "PAY_PER_REQUEST"
        }
    })
}

/// The SAM table type, which the transform expands into the same concrete one.
fn simple_table() -> Value {
    json!({
        "Type": "AWS::Serverless::SimpleTable",
        "Properties": { "PrimaryKey": { "Name": TABLE_KEY, "Type": "String" } }
    })
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a named secret whose value the template spells, a named bucket, the \
                   concrete table type, and the published policy templates for the grants",
            secret: |stack| {
                json!({
                    "Name": format!("{stack}-config"),
                    "Description": "the application's configuration",
                    "SecretString": "{\"username\":\"app\",\"password\":\"declared-value\"}"
                })
            },
            bucket: |stack| json!({ "BucketName": format!("{stack}-uploads") }),
            table: concrete_table,
            policies: policy_templates,
            expected_secret_name: |stack| Some(format!("{stack}-config")),
            expected_value: ExpectedValue::Verbatim(
                "{\"username\":\"app\",\"password\":\"declared-value\"}",
            ),
        },
        Row {
            case: "a secret whose value the owning service generates, a bucket and a table \
                   whose names the stack generates, the SAM table type, and the grants stated \
                   as inline statements",
            secret: |_| {
                json!({
                    "GenerateSecretString": { "PasswordLength": 20, "ExcludePunctuation": true }
                })
            },
            bucket: |_| json!({}),
            table: simple_table,
            policies: inline_statements,
            expected_secret_name: |_| None,
            expected_value: ExpectedValue::GeneratedPassword { length: 20 },
        },
    ]
}

/// The template one row deploys: the four declarations, and the function wired to the other
/// three through its environment.
fn template(row: &Row, stack: &str) -> String {
    let mut resources = json!({});
    resources[SECRET_ID] = json!({
        "Type": "AWS::SecretsManager::Secret",
        "Properties": (row.secret)(stack)
    });
    resources[BUCKET_ID] = json!({
        "Type": "AWS::S3::Bucket",
        "Properties": (row.bucket)(stack)
    });
    resources[TABLE_ID] = (row.table)();
    resources[FUNCTION_ID] = json!({
        "Type": "AWS::Serverless::Function",
        "Properties": {
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Timeout": FUNCTION_TIMEOUT,
            "InlineCode": PROCESSING_HANDLER,
            "Environment": { "Variables": {
                "SECRET_ARN": { "Ref": SECRET_ID },
                "UPLOAD_BUCKET": { "Ref": BUCKET_ID },
                "AUDIT_TABLE": { "Ref": TABLE_ID }
            }},
            "Policies": (row.policies)()
        }
    });
    json!({
        "Transform": "AWS::Serverless-2016-10-31",
        "Resources": resources
    })
    .to_string()
}

/// Deploy one row's stack, require it to COMPLETE rather than roll back, and answer the
/// physical ids its resources were created under.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    row: &Row,
) -> BTreeMap<String, String> {
    let body = template(row, stack);
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &body),
            // A template declaring a macro is acknowledged with CAPABILITY_AUTO_EXPAND, and
            // the role the transform synthesizes for the function with CAPABILITY_IAM.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {answered}"
    );

    let (reached, described) = wait_for_the_stack(router, stack).await;
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

    let (status, resources) =
        cloudformation(router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with body \
         {resources}"
    );
    let physical = physical_ids_of(&resources);
    for logical in [SECRET_ID, BUCKET_ID, TABLE_ID, FUNCTION_ID] {
        assert!(
            physical.contains_key(logical),
            "case {case:?}: expected a physical id for {logical}, got {resources}"
        );
    }
    physical
}

/// The `StackStatus` the stack `stack` reports once it first reports [`CREATE_COMPLETE`], with
/// the answer that status was read from — or the last status it reported before [`WAIT`] passed.
///
/// The stack's own transition is owed from the instant its five resources' owning services
/// report them ready, which no fixed sleep can name, so the wait is on that CONDITION. The
/// shared reader of one status is used; only the ceiling is this suite's, because this estate is
/// larger than the one that reader's own suite deploys.
async fn wait_for_the_stack(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let deadline = Instant::now() + WAIT;
    loop {
        let reported = stack_status(router, stack).await;
        if reported.0 == CREATE_COMPLETE || Instant::now() >= deadline {
            return reported;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The `TableStatus` DynamoDB reports for `table` once it first reports it usable — "you can
/// perform read and write operations only on an ACTIVE table" — or the last status it reported
/// before [`WAIT`] passed. The span a created table spends `CREATING` is that service's own, so
/// nothing here sleeps a fixed span on its behalf.
async fn wait_until_table_active(router: &MultiServiceRouter, table: &str) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        let (_, described) =
            dynamodb(router, "DescribeTable", &json!({ "TableName": table })).await;
        let reported = described["Table"]["TableStatus"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if reported == "ACTIVE" || Instant::now() >= deadline {
            return reported;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The `State` Lambda reports for `function` once it first reports it invocable — "the State
/// field in the response from GetFunctionConfiguration indicate[s] when the function is ready
/// to invoke" — or the last state it reported before [`WAIT`] passed.
async fn wait_until_function_active(router: &MultiServiceRouter, function: &str) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            &[],
        )
        .await;
        let reported = configuration["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if reported == "Active" || Instant::now() >= deadline {
            return reported;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// RULE: a transformed template's four declarations become four resources in four services,
/// the stack completes, and the function it deployed reaches each of them.
#[test]
fn a_transformed_estate_is_created_in_every_service_that_owns_a_part_of_it() {
    driven("sam-secret-bucket-table-estate", || {
        Box::pin(async {
            for (index, row) in rows().iter().enumerate() {
                one_row(index, row).await;
            }
        })
    });
}

/// The service principal Secrets Manager invokes a rotation function as ("ensure that the
/// function policy grants access to the principal `secretsmanager.amazonaws.com`", Secrets
/// Manager User Guide, *Permissions for rotation*).
const ROTATION_PRINCIPAL: &str = "secretsmanager.amazonaws.com";

/// The IAM action an invocation of a rotation function performs: "you also need
/// lambda:InvokeFunction permissions on the rotation function" (`RotateSecret`, Secrets
/// Manager's Smithy model).
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// What Secrets Manager answers when the rotation function's own policy does not let it invoke
/// the function. Fixed text, whose only variable part is the principal it names.
const INVOKE_NOT_PERMITTED: &str = "Secrets Manager cannot invoke the specified Lambda function. \
     Ensure that the function policy grants access to the principal secretsmanager.amazonaws.com.";

/// The account of a function ARN that names no function of this emulator's own account, which
/// Lambda holds nothing under either.
const ANOTHER_ACCOUNT: &str = "111122223333";

/// One row of the rotation-admission table: the `RotationLambdaARN` the call carries — built
/// from the function the stack deployed — and the refusal it has to be answered with.
struct RotationRow {
    case: &'static str,
    /// The `RotationLambdaARN`, from the physical id of the stack's own function.
    lambda_arn: fn(&str) -> String,
    code: &'static str,
    message: &'static str,
}

/// The rows: the forms a `RotationLambdaARN` takes that name no function this service may
/// invoke — the stack's own function, which grants this service nothing; a well-formed function
/// ARN no function is held under; and one naming a function of another account, which this
/// emulator holds nothing under either.
///
/// A value that is not a Lambda function's ARN at all is refused too, and under a sentence of
/// its own, but WHICH sentence that is belongs to the issue that owns the wording of a
/// malformed `RotationLambdaARN`, so no row of this table pins it.
const ROTATION_ROWS: &[RotationRow] = &[
    RotationRow {
        case: "the stack's own function, whose policy grants this service nothing",
        lambda_arn: |function| {
            format!(
                "arn:aws:lambda:{REGION}:{}:function:{function}",
                ::engine::EMULATOR_ACCOUNT_ID
            )
        },
        code: "AccessDeniedException",
        message: INVOKE_NOT_PERMITTED,
    },
    RotationRow {
        case: "a well-formed function ARN Lambda holds no function under",
        lambda_arn: |_| {
            format!(
                "arn:aws:lambda:{REGION}:{}:function:no-such-rotator",
                ::engine::EMULATOR_ACCOUNT_ID
            )
        },
        code: "AccessDeniedException",
        message: INVOKE_NOT_PERMITTED,
    },
    RotationRow {
        case: "a function ARN of another account, which this emulator holds nothing under",
        lambda_arn: |function| format!("arn:aws:lambda:{REGION}:{ANOTHER_ACCOUNT}:function:{function}"),
        code: "AccessDeniedException",
        message: INVOKE_NOT_PERMITTED,
    },
];

/// RULE: a rotation is driven by INVOKING the rotation function, so `RotateSecret` is admitted
/// only for a `RotationLambdaARN` that names a function this service may invoke — one Lambda
/// holds and whose own resource-based policy grants [`ROTATION_PRINCIPAL`] the invoking action.
/// A call naming any other value is refused, with nothing about the secret changed: rotation
/// stays off and the value the stack stored is still the one served. The same call, once that
/// function's policy admits this service, is accepted and stakes `AWSPENDING` on the version
/// the rotation will create.
///
/// Driven over the ASSEMBLED router on a secret and a function one stack deployed, which is
/// where a client configures a rotation: Secrets Manager decides the admission, and the answer
/// is Lambda's own about the function's policy.
#[test]
fn a_stacks_secret_rotates_only_through_a_function_this_service_may_invoke() {
    driven("sam-estate-rotation-admission", || {
        Box::pin(async { rotation_admission().await })
    });
}

async fn rotation_admission() {
    let rows = rows();
    let row = &rows[0];
    let router = build_router();
    let stack = "samestaterotation";
    let physical = deploy(&router, "the stack the rotation rows run on", stack, row).await;
    let secret_arn = physical[SECRET_ID].clone();
    let function = physical[FUNCTION_ID].clone();
    let stored = match row.expected_value {
        ExpectedValue::Verbatim(value) => value,
        ExpectedValue::GeneratedPassword { .. } => {
            panic!("the row the rotation rows run on states its value verbatim")
        }
    };

    for rotation in ROTATION_ROWS {
        let case = rotation.case;
        let (status, refused) = secretsmanager(
            &router,
            "RotateSecret",
            &json!({
                "SecretId": &secret_arn,
                "RotationLambdaARN": (rotation.lambda_arn)(&function),
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from RotateSecret, got {status} with {refused}"
        );
        let code = refused["__type"]
            .as_str()
            .and_then(|raw| raw.rsplit(['#', '/']).next())
            .unwrap_or_default();
        assert_eq!(
            code, rotation.code,
            "case {case:?}: expected {}, got {code} in {refused}",
            rotation.code
        );
        assert_eq!(
            refused["Message"].as_str(),
            Some(rotation.message),
            "case {case:?}: expected the sentence for that refusal, got {refused}"
        );

        // Nothing about the secret changed: a refused rotation is refused before the
        // configuration it carries is applied.
        let (_, described) =
            secretsmanager(&router, "DescribeSecret", &json!({ "SecretId": &secret_arn })).await;
        assert_ne!(
            described["RotationEnabled"].as_bool(),
            Some(true),
            "case {case:?}: a refused rotation enabled rotation, got {described}"
        );
        assert!(
            described["RotationLambdaARN"].is_null(),
            "case {case:?}: a refused rotation configured a rotation function, got {described}"
        );
        let (_, read) =
            secretsmanager(&router, "GetSecretValue", &json!({ "SecretId": &secret_arn })).await;
        assert_eq!(
            read["SecretString"].as_str(),
            Some(stored),
            "case {case:?}: a refused rotation changed the value the stack stored, got {read}"
        );
    }

    // The ACCEPTED half of the same rule: the identical call, once the function's own policy
    // admits this service, is served and stakes AWSPENDING on the version the rotation creates.
    let case = "the same call once the function's policy admits this service";
    let (status, _, granted) = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        json!({
            "StatementId": "rotation-invoke",
            "Action": INVOKE_ACTION,
            "Principal": ROTATION_PRINCIPAL,
        })
        .to_string()
        .as_bytes(),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from AddPermission, got {status} with {granted}"
    );
    let function_arn = (ROTATION_ROWS[0].lambda_arn)(&function);
    let (status, started) = secretsmanager(
        &router,
        "RotateSecret",
        &json!({ "SecretId": &secret_arn, "RotationLambdaARN": &function_arn }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from RotateSecret, got {status} with {started}"
    );
    let staked = started["VersionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a VersionId, got {started}"))
        .to_string();
    let (_, described) =
        secretsmanager(&router, "DescribeSecret", &json!({ "SecretId": &secret_arn })).await;
    assert_eq!(
        described["RotationEnabled"].as_bool(),
        Some(true),
        "case {case:?}: expected the accepted call to enable rotation, got {described}"
    );
    assert_eq!(
        described["RotationLambdaARN"].as_str(),
        Some(function_arn.as_str()),
        "case {case:?}: expected the configured rotation function, got {described}"
    );
    let staged: Vec<&str> = described["VersionIdsToStages"][&staked]
        .as_array()
        .map(|stages| stages.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    assert!(
        staged.contains(&"AWSPENDING"),
        "case {case:?}: expected AWSPENDING staked on the version {staked:?} the rotation \
         creates, got {described}"
    );
}

/// One row: deploy, read every resource back where it lives, then invoke the function and read
/// what it wrote.
async fn one_row(index: usize, row: &Row) {
    let case = row.case;
    let router = build_router();
    let stack = format!("samestate{index}");
    let physical = deploy(&router, case, &stack, row).await;
    let secret_arn = physical[SECRET_ID].clone();
    let bucket = physical[BUCKET_ID].clone();
    let table = physical[TABLE_ID].clone();
    let function = physical[FUNCTION_ID].clone();

    // SECRETS MANAGER holds the secret, under the ARN the stack published for it.
    let (status, described) =
        secretsmanager(&router, "DescribeSecret", &json!({ "SecretId": &secret_arn })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeSecret for {secret_arn:?}, got {status} \
         with {described}"
    );
    assert_eq!(
        described["ARN"].as_str(),
        Some(secret_arn.as_str()),
        "case {case:?}: expected the secret to answer for the published ARN, got {described}"
    );
    let secret_name = described["Name"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a Name, got {described}"))
        .to_string();
    if let Some(wanted) = (row.expected_secret_name)(&stack) {
        assert_eq!(
            secret_name, wanted,
            "case {case:?}: expected the declared name {wanted:?}, got {secret_name:?}"
        );
    }

    // The VALUE the secret holds, which is what the handler reads: the one the template spelled,
    // or a generated password of the declared length.
    let (status, read) =
        secretsmanager(&router, "GetSecretValue", &json!({ "SecretId": &secret_arn })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetSecretValue, got {status} with {read}"
    );
    let value = read["SecretString"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a SecretString, got {read}"))
        .to_string();
    match row.expected_value {
        ExpectedValue::Verbatim(wanted) => assert_eq!(
            value, wanted,
            "case {case:?}: expected the value the template spelled, got a value of {} \
             characters",
            value.chars().count()
        ),
        ExpectedValue::GeneratedPassword { length } => assert_eq!(
            value.chars().count(),
            length,
            "case {case:?}: expected a generated password of {length} characters, got {}",
            value.chars().count()
        ),
    }
    let value_length = value.chars().count();

    // AMAZON S3 holds the bucket, under the name the stack published for it.
    let (status, listed) = s3(&router, "GET", &format!("/{bucket}?list-type=2"), &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 listing the bucket {bucket:?}, got {status} with \
         {listed}"
    );

    // DYNAMODB holds the table, and reports it usable: "you can perform read and write
    // operations only on an ACTIVE table".
    let table_status = wait_until_table_active(&router, &table).await;
    assert_eq!(
        table_status, "ACTIVE",
        "case {case:?}: expected DynamoDB to report {table} ACTIVE, got {table_status:?}"
    );

    // AWS LAMBDA holds the function, and reports it invocable: "the State field in the response
    // from GetFunctionConfiguration indicate[s] when the function is ready to invoke".
    let state = wait_until_function_active(&router, &function).await;
    assert_eq!(
        state, "Active",
        "case {case:?}: expected Lambda to report {function} Active, got {state:?}"
    );

    // And the function the stack deployed reaches all three: it reads the secret, writes the
    // bucket and writes the table, as the role the transform synthesized from its `Policies`.
    let key = format!("upload-{index}.json");
    let expected = json!({
        "key": &key,
        "secret_name": &secret_name,
        "value_length": value_length,
    });
    let (status, headers, payload) = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{function}/invocations"),
        json!({ "key": &key }).to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Invoke, got {status} with payload {payload}"
    );
    let signal = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        signal, None,
        "case {case:?}: expected no function error, got {signal:?} with payload {payload}"
    );
    assert_eq!(
        payload, expected,
        "case {case:?}: expected the handler's own record {expected}, got {payload}"
    );

    // What the stores hold afterwards: the work the handler reported is work those services
    // really did.
    let (status, object) = s3(
        &router,
        "GET",
        &format!("/{bucket}/{OBJECT_PREFIX}{key}"),
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the handler's object in {bucket:?}, got {status} with {object}"
    );
    let stored: Value = serde_json::from_str(&object).unwrap_or(Value::Null);
    assert_eq!(
        stored, expected,
        "case {case:?}: expected the object to carry {expected}, got {object}"
    );

    let (status, item) = dynamodb(
        &router,
        "GetItem",
        &json!({
            "TableName": &table,
            "Key": { TABLE_KEY: { "S": &key } },
            "ConsistentRead": true,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetItem for {key:?}, got {status} with {item}"
    );
    let wanted_item = json!({
        TABLE_KEY: { "S": &key },
        "SecretName": { "S": &secret_name },
        "ValueLength": { "N": value_length.to_string() },
    });
    assert_eq!(
        item["Item"], wanted_item,
        "case {case:?}: expected the table to hold {wanted_item}, got {item}"
    );
}
