// SPDX-License-Identifier: CC0-1.0

//! A SAM stack's VERBATIM resources — the concrete CloudFormation types a transform-carrying
//! template spells out beside its `AWS::Serverless::*` declarations — are real resources in the
//! services that own them, under the very identifiers the completed stack publishes.
//!
//! The rule under test: **a template declaring a macro is PROCESSED and then provisioned
//! resource by resource, so by the time `DescribeStacks` reports `CREATE_COMPLETE` every
//! resource of the processed template exists in the service that owns it, addressable under the
//! physical id CloudFormation reports for it — which is the value `Ref` yields and therefore the
//! value a stack `Output` publishes.** For an `AWS::DynamoDB::Table` that id is the table name,
//! so a data-plane call made with the published name is SERVED: `GetItem` answers HTTP 200,
//! carrying no `Item` member until something writes the key, and never the refusal DynamoDB
//! bounds that read with for a table it does not hold (`ResourceNotFoundException`, HTTP 400 —
//! `smithy_query(query="operation", operation="GetItem")`). The transform is not an exception to
//! any of that: an expansion replaces the macro's own input types and leaves every other
//! declaration as the template wrote it, so those declarations are created exactly as they are
//! in a template carrying no transform at all.
//!
//! Five crates have to agree for that to hold — CloudFormation expands the template, decides
//! each physical id and makes the creates; DynamoDB holds the table, decides its status and
//! serves its data plane; Secrets Manager holds the secret; Lambda holds the function the
//! expansion emitted; Amazon S3 holds the bucket and publishes its object events — so the suite
//! lives here rather than in any one of them: a CloudFormation-only test can see the stack
//! status and the published id but not whether anything exists behind them.
//!
//! Why it matters: `CREATE_COMPLETE` plus an `Output` is the whole contract between a deployment
//! and the code that consumes it. A stack that reports success while one of its declared
//! resources was never created hands its clients a name that resolves nowhere, and every call
//! made with the stack's own output is refused.
//!
//! The rows of [`ROWS`] differ along what the rule generalises over — the declaration whose
//! resource has to be there: the read/write mode the table declares (both values of
//! `BillingMode`, `smithy_query(query="shape", shape="BillingMode")`), its key schema (a
//! partition key alone, a partition key with a sort key), the `AttributeType` of the key
//! attributes (every value of `ScalarAttributeType`: `S`, `N`, `B` —
//! `smithy_query(query="shape", shape="ScalarAttributeType")`), whether the declaration NAMES
//! the table or leaves the name to CloudFormation, and the shape of the `SecretString` the
//! stack's secret carries. Each row reads its table through DynamoDB's own API only, by the
//! name the stack published, and every expected value is computed here from what the row
//! declared.
//!
//! [`the_notification_driven_function_writes_the_declared_table`] carries the same rule through
//! the whole estate such a stack is deployed for: the bucket the stack publishes takes a write,
//! the object event reaches the function the transform expanded, and what that handler reads out
//! of Secrets Manager lands in the declared table — observed, as everywhere here, through each
//! service's own API. Two siblings reach one side of that each, and are maintained with it:
//! [`crate::s3_notification_lambda_invoke_grant_gates_delivery`] pins what the grant on a
//! function destination decides about a bucket's notification and its delivery, and
//! [`crate::cloudformation_sam_function_writes_its_table`] pins what a SAM-expanded handler can
//! do with the SDK its runtime supplies. What only this suite reaches is the two JOINED on a
//! table and a secret the template spells VERBATIM: the write lands in the resource the stack
//! declared and published, carrying a value no part of the event holds.
//!
//! Both tests are driven on a thread sized like the emulator's own boot path ([`driven`]): the
//! assembled router's dispatch future carries every service's handler state inline, and the
//! chains here nest several of them (CloudFormation into DynamoDB, Secrets Manager, IAM, Lambda
//! and S3; S3's delivery into Lambda; the handler's own SDK calls back into the router).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, outputs_of, physical_ids_of};
use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// The Region every request here is signed for — the Region [`wait_for_stack_status`] polls in
/// — and so the Region every stack below deploys into.
const REGION: &str = "us-east-1";

/// The `StackStatus` the rule is stated about.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `TableStatus` a table ready for use reports: "`ACTIVE` - The table is ready for use"
/// (`TableDescription$TableStatus`).
const ACTIVE: &str = "ACTIVE";

/// The macro the AWS Serverless Application Model is published as, as a template's `Transform`
/// section names it, and the capability a client acknowledges a template declaring a macro with.
const SAM_TRANSFORM: &str = "AWS::Serverless-2016-10-31";
const CAPABILITY_AUTO_EXPAND: &str = "CAPABILITY_AUTO_EXPAND";
const CAPABILITY_IAM: &str = "CAPABILITY_IAM";
const CAPABILITY_NAMED_IAM: &str = "CAPABILITY_NAMED_IAM";

/// The two values of DynamoDB's `BillingMode` enum
/// (`smithy_query(query="shape", shape="BillingMode")`).
const PAY_PER_REQUEST: &str = "PAY_PER_REQUEST";
const PROVISIONED: &str = "PROVISIONED";

/// The logical ids the templates below declare.
const TABLE: &str = "MyTable";
const SECRET: &str = "MySecret";
const FUNCTION: &str = "Writer";
const ROLE: &str = "FunctionRole";
const PERMISSION: &str = "InvokePermission";
const BUCKET: &str = "AssetBucket";

/// The output keys the templates publish each identifier under.
const TABLE_NAME_OUTPUT: &str = "TableName";
const SECRET_ARN_OUTPUT: &str = "SecretArn";
const FUNCTION_NAME_OUTPUT: &str = "FunctionName";
const BUCKET_NAME_OUTPUT: &str = "BucketName";

/// The runtime and handler every function below declares. What a managed runtime supplies, and
/// how an inline package is archived, is pinned by
/// [`crate::cloudformation_sam_function_writes_its_table`]; here they are held fixed.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// How long one invocation of these handlers is given, as the function's own `Timeout`: well
/// past what a run needs, because nothing here is stated about how fast an interpreter gets
/// through one.
const FUNCTION_TIMEOUT: u64 = 30;

/// How long this suite keeps looking for work another service's clock owes — a function reported
/// `Active`, an asynchronously delivered object event — and how often it looks while waiting.
///
/// Generous on purpose: no span is asserted on, only refused to be waited out forever, and a
/// host running every suite of this workspace at once takes far longer over one asynchronous
/// invocation than an idle one does. It stays short of the span that makes a test itself the
/// thing being reported, so a delivery that never comes is still answered as a failure.
const PATIENCE: Duration = Duration::from_secs(240);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The stack the emulator's own boot path gives the thread that assembles the router
/// (`core/router/src/main.rs`), which is what the chains here need; see the module header.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The service principal Amazon S3 publishes notifications under, and the action it performs on
/// a function destination ("for Amazon Web Services services, the principal is a domain-style
/// identifier that the service defines, such as s3.amazonaws.com", `AddPermission$Principal`).
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The object-change class the bucket's configuration is notified about.
const CONFIGURED_EVENT: &str = "s3:ObjectCreated:*";

/// Run `body` to completion on a thread with [`BOOT_STACK_SIZE`], on a current-thread runtime:
/// an asynchronous notification delivery is a task of its own, so it runs on this same thread
/// and needs the same depth.
fn driven<Body>(name: &'static str, body: Body)
where
    Body: FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> + Send + 'static,
{
    std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(body());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
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

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(router: &MultiServiceRouter, method: &str, path: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
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

/// One DynamoDB attribute value, as the wire writes it: the type the attribute is of, carrying
/// the value.
fn attribute(kind: &str, value: &str) -> Value {
    let mut typed = Map::new();
    typed.insert(kind.to_string(), Value::String(value.to_string()));
    Value::Object(typed)
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

/// Deploy `template` as `stack` and answer `(the outputs it publishes, its logical -> physical id
/// map)`, having first established that it reached [`CREATE_COMPLETE`].
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", template),
            // The template declares a macro, a role the transform may extend, and a NAMED
            // IAM resource — which is what these three capabilities acknowledge.
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

    let (reached, described) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
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
    (outputs_of(&described), physical_ids_of(&resources))
}

/// The value one of the stack's outputs publishes.
fn published(case: &str, outputs: &BTreeMap<String, String>, key: &str) -> String {
    outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {outputs:?}"))
}

/// Poll `GetFunctionConfiguration` until Lambda first reports `name` `Active` — "the function is
/// ready to be invoked" (`FunctionConfiguration$State`) — and answer the configuration it
/// reported then. The span a created function spends `Pending` is that service's own, so no
/// fixed sleep names the instant it ends.
async fn active_function(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let looks = PATIENCE.as_millis() / LOOK_INTERVAL.as_millis();
    let mut last = Value::Null;
    for _ in 0..looks {
        let (status, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
        )
        .await;
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
    last
}

/// The item `table` holds under `key` right now, read through DynamoDB's own `GetItem`, with the
/// HTTP status that read answered. A table the service does not hold is what the rule forbids,
/// so the status is answered rather than asserted here.
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

/// Look for the item the handler owes under `key` until it is there, answering it, or `None`
/// once [`PATIENCE`] has run out. The wait is on the CONDITION; nothing asserts how long it
/// took.
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
    None
}

/// One row of the table: the `AWS::DynamoDB::Table` and the `AWS::SecretsManager::Secret` the
/// SAM template spells VERBATIM beside its serverless function.
struct Row {
    case: &'static str,
    /// Whether the declaration names the table itself, so the published id is that name, rather
    /// than leaving the name to CloudFormation.
    named: bool,
    /// The provisioned capacity the declaration asks for (`PROVISIONED`), or `None` for a table
    /// declared on-demand (`PAY_PER_REQUEST`).
    provisioned: Option<(i64, i64)>,
    /// The partition key: its attribute name, its `AttributeType`, and the value every read and
    /// write below addresses the item by.
    hash: (&'static str, &'static str, &'static str),
    /// The sort key, for a row declaring one, in the same three parts.
    range: Option<(&'static str, &'static str, &'static str)>,
    /// The `SecretString` the stack's secret is deployed with.
    secret: &'static str,
}

/// The rows span the declaration: both read/write modes, every `ScalarAttributeType`, a key
/// schema of one member and of two, and a table named by the template as well as one named by
/// CloudFormation.
const ROWS: &[Row] = &[
    Row {
        case: "on-demand, one string partition key, the table name left to CloudFormation",
        named: false,
        provisioned: None,
        hash: ("pk", "S", "incoming/file1.txt"),
        range: None,
        secret: "a-plain-secret-string",
    },
    Row {
        case: "provisioned capacity, a string partition key with a numeric sort key, under a \
               name the template declares",
        named: true,
        provisioned: Some((3, 2)),
        hash: ("pk", "S", "tenant#42"),
        range: Some(("version", "N", "7")),
        secret: r#"{"endpoint":"db.internal","port":5432}"#,
    },
    Row {
        case: "on-demand, a numeric partition key, the table name left to CloudFormation",
        named: false,
        provisioned: None,
        hash: ("id", "N", "4711"),
        range: None,
        secret: "line one\nline two\n",
    },
    Row {
        case: "provisioned capacity, a binary partition key, under a name the template declares",
        named: true,
        provisioned: Some((1, 1)),
        // A `B` attribute's value travels as base64 on the awsJson wire.
        hash: ("digest", "B", "YWJjZA=="),
        range: None,
        secret: "a-secret-with-unicode-£-and-a-space",
    },
];

impl Row {
    /// The read/write mode this row's declaration asks for.
    fn billing_mode(&self) -> &'static str {
        if self.provisioned.is_some() {
            PROVISIONED
        } else {
            PAY_PER_REQUEST
        }
    }

    /// The key members in declared order: the partition key, then the sort key where there is
    /// one.
    fn key_members(&self) -> Vec<(&'static str, &'static str, &'static str)> {
        let mut members = vec![self.hash];
        if let Some(range) = self.range {
            members.push(range);
        }
        members
    }

    /// The `KeySchema` the declaration carries, which is also the one `DescribeTable` reports.
    fn key_schema(&self) -> Value {
        let mut schema = Vec::new();
        for (index, (name, _, _)) in self.key_members().into_iter().enumerate() {
            let key_type = if index == 0 { "HASH" } else { "RANGE" };
            schema.push(json!({ "AttributeName": name, "KeyType": key_type }));
        }
        Value::Array(schema)
    }

    /// The `AttributeDefinitions` the declaration carries, in declared order.
    fn attribute_definitions(&self) -> Value {
        Value::Array(
            self.key_members()
                .into_iter()
                .map(|(name, kind, _)| json!({ "AttributeName": name, "AttributeType": kind }))
                .collect(),
        )
    }

    /// The `AttributeDefinitions` the declaration carries, as a SET: one entry per key
    /// attribute, ordered by attribute name here and in what is compared against it, because no
    /// member of the model fixes the order a description reports them in.
    fn definition_set(&self) -> Vec<Value> {
        let mut members = self.key_members();
        members.sort_by_key(|(name, _, _)| *name);
        members
            .into_iter()
            .map(|(name, kind, _)| json!({ "AttributeName": name, "AttributeType": kind }))
            .collect()
    }

    /// The key every read and write of this row's table addresses.
    fn key(&self) -> Value {
        let mut key = Map::new();
        for (name, kind, value) in self.key_members() {
            key.insert(name.to_string(), attribute(kind, value));
        }
        Value::Object(key)
    }

    /// The item this row writes once it has established the table serves a read: its key, plus
    /// one attribute that is no part of it.
    fn item(&self) -> Value {
        let mut item = self.key().as_object().cloned().expect("the key is an object");
        item.insert("note".to_string(), attribute("S", self.case));
        Value::Object(item)
    }

    /// The table name the declaration states, for a row that states one.
    fn declared_name(&self, stack: &str) -> Option<String> {
        self.named.then(|| format!("{stack}-table"))
    }

    /// The template this row deploys: the transform, the table and the secret it spells
    /// verbatim, and one `AWS::Serverless::Function` so the macro really has something to
    /// expand — the function is named the table and the secret through its environment, exactly
    /// as the declarations a SAM template hands its function.
    fn template(&self, stack: &str) -> String {
        let mut table = json!({
            "KeySchema": self.key_schema(),
            "AttributeDefinitions": self.attribute_definitions(),
            "BillingMode": self.billing_mode(),
        });
        if let Some((read, write)) = self.provisioned {
            table["ProvisionedThroughput"] =
                json!({ "ReadCapacityUnits": read, "WriteCapacityUnits": write });
        }
        if let Some(name) = self.declared_name(stack) {
            table["TableName"] = json!(name);
        }
        json!({
            "Transform": SAM_TRANSFORM,
            "Resources": {
                TABLE: { "Type": "AWS::DynamoDB::Table", "Properties": table },
                SECRET: {
                    "Type": "AWS::SecretsManager::Secret",
                    "Properties": {
                        "Name": format!("{stack}-secret"),
                        "SecretString": self.secret,
                    },
                },
                FUNCTION: {
                    "Type": "AWS::Serverless::Function",
                    "Properties": {
                        "Runtime": RUNTIME,
                        "Handler": HANDLER,
                        "Timeout": FUNCTION_TIMEOUT,
                        "InlineCode": IDLE_HANDLER,
                        "Environment": {
                            "Variables": {
                                "TABLE_NAME": { "Ref": TABLE },
                                "SECRET_ARN": { "Ref": SECRET },
                            },
                        },
                        "Policies": [
                            { "DynamoDBCrudPolicy": { "TableName": { "Ref": TABLE } } },
                        ],
                    },
                },
            },
            "Outputs": {
                TABLE_NAME_OUTPUT: { "Value": { "Ref": TABLE } },
                SECRET_ARN_OUTPUT: { "Value": { "Ref": SECRET } },
                FUNCTION_NAME_OUTPUT: { "Value": { "Ref": FUNCTION } },
            },
        })
        .to_string()
    }
}

/// The function every row of [`ROWS`] declares. It is never invoked by those rows — what they
/// assert of it is that the expansion emitted a function Lambda really holds — so it carries the
/// smallest handler there is.
const IDLE_HANDLER: &str = r#"
def handler(event, context):
    return {}
"#;

/// The handler [`the_notification_driven_function_writes_the_declared_table`] deploys: for every
/// record it is handed it reads the secret the stack named it and writes one item into the table
/// the stack named it, carrying the key and the bucket the record reports. Everything it writes
/// comes from the event or from another service's answer; nothing is computed by the deployment.
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
            },
        )
        written.append(key)
    return {"written": written}
"#;

/// Every row: a SAM stack reported `CREATE_COMPLETE` publishes a table name DynamoDB serves, a
/// secret ARN Secrets Manager answers for, and a function Lambda holds.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn a_sam_stacks_declared_table_is_served_under_the_name_it_publishes() {
    driven("sam-verbatim-resources", || {
        Box::pin(a_sam_stacks_declared_table_is_served_under_the_name_it_publishes_body())
    });
}

async fn a_sam_stacks_declared_table_is_served_under_the_name_it_publishes_body() {
    // One assembled emulator for ALL the rows: each row deploys a stack of its own, under names
    // of its own, so nothing a row leaves behind is what another row reads — and assembling the
    // whole service set once per row is load this suite does not need.
    let router = emulator();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let stack = format!("sam-verbatim-{index}");
        let (outputs, physical) = deploy(&router, case, &stack, &row.template(&stack)).await;

        // The published name is the resource's own physical id — the value `Ref` yields — and,
        // where the template named the table, the name it declared.
        let table = published(case, &outputs, TABLE_NAME_OUTPUT);
        assert_eq!(
            Some(&table),
            physical.get(TABLE),
            "case {case:?}: expected the {TABLE_NAME_OUTPUT} output to carry the table's own \
             physical id {:?}, got {table:?}",
            physical.get(TABLE)
        );
        if let Some(declared) = row.declared_name(&stack) {
            assert_eq!(
                table, declared,
                "case {case:?}: expected the declared table name {declared:?}, got {table:?}"
            );
        }

        // What DynamoDB holds under that name, read through its own API: a table ready for use,
        // configured as the template declared it. No poll: the stack reporting the create
        // complete is what the rule says the table's readiness precedes.
        let (status, described) = dynamodb(&router, "DescribeTable", &json!({"TableName": table}))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeTable on the published name \
             {table:?}, got {status} with {described}"
        );
        let held = described
            .get("Table")
            .unwrap_or_else(|| panic!("case {case:?}: no Table in {described}"));
        for (member, expected) in [
            ("TableStatus", json!(ACTIVE)),
            ("TableName", json!(table.clone())),
            ("KeySchema", row.key_schema()),
        ] {
            assert_eq!(
                held.get(member),
                Some(&expected),
                "case {case:?}: expected {member} to be {expected}, got {held}"
            );
        }
        // The key attributes the table was created with, compared as a SET: which order a
        // description lists them in is no part of the rule.
        let mut reported: Vec<Value> = held
            .get("AttributeDefinitions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        reported.sort_by_key(|definition| {
            definition
                .get("AttributeName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        });
        assert_eq!(
            reported,
            row.definition_set(),
            "case {case:?}: expected the declared attribute definitions {:?}, got {held}",
            row.definition_set()
        );
        // The read/write mode EVERY table reports, whichever mode it is in
        // (`TableDescription$TableThroughputModeSummary`).
        assert_eq!(
            held.get("TableThroughputModeSummary")
                .and_then(|summary| summary.get("TableThroughputMode")),
            Some(&json!(row.billing_mode())),
            "case {case:?}: expected the declared read/write mode {:?}, got {held}",
            row.billing_mode()
        );
        if let Some((read, write)) = row.provisioned {
            assert_eq!(
                held.get("ProvisionedThroughput")
                    .and_then(|throughput| throughput.get("ReadCapacityUnits")),
                Some(&json!(read)),
                "case {case:?}: expected the declared read capacity {read}, got {held}"
            );
            assert_eq!(
                held.get("ProvisionedThroughput")
                    .and_then(|throughput| throughput.get("WriteCapacityUnits")),
                Some(&json!(write)),
                "case {case:?}: expected the declared write capacity {write}, got {held}"
            );
        }

        // The call the rule is stated about: a read of a key nothing has written is SERVED,
        // carrying no item — never the refusal a table that does not exist is read with.
        let key = row.key();
        let (status, answered, item) = read_item(&router, &table, &key).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetItem on the published table name \
             {table:?}, got {status} with {answered}"
        );
        assert_eq!(
            item, None,
            "case {case:?}: nothing has written {key}, so the answer must carry no Item, got \
             {answered}"
        );

        // And the data plane really is this table's: an item written through it is read back.
        let written = row.item();
        let (status, put) =
            dynamodb(&router, "PutItem", &json!({"TableName": table, "Item": written})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from PutItem on {table:?}, got {status} with {put}"
        );
        let (status, answered, item) = read_item(&router, &table, &key).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetItem after the write, got {status} with \
             {answered}"
        );
        assert_eq!(
            item.as_ref(),
            Some(&written),
            "case {case:?}: expected the item just written {written}, got {answered}"
        );

        // The same rule over the other two services this stack declares into: the secret the
        // stack published an ARN for is one Secrets Manager holds, and the function the
        // expansion emitted is one Lambda holds.
        let secret_arn = published(case, &outputs, SECRET_ARN_OUTPUT);
        let (status, secret) =
            secretsmanager(&router, "GetSecretValue", &json!({"SecretId": secret_arn})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetSecretValue on the published ARN, got \
             {status} with {secret}"
        );
        assert_eq!(
            secret.get("SecretString").and_then(Value::as_str),
            Some(row.secret),
            "case {case:?}: expected the deployed SecretString {:?}, got {secret}",
            row.secret
        );

        let function = published(case, &outputs, FUNCTION_NAME_OUTPUT);
        let configuration = active_function(&router, case, &function).await;
        assert_eq!(
            configuration.get("State").and_then(Value::as_str),
            Some("Active"),
            "case {case:?}: expected Lambda to report {function} Active, got {configuration}"
        );
    }
}

/// The whole estate such a stack is deployed for, on the one case the rule above is about: a
/// write into the bucket the stack published reaches the function the transform expanded, and
/// what that handler reads out of Secrets Manager lands in the table the stack published.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn the_notification_driven_function_writes_the_declared_table() {
    driven("sam-notification-estate", || {
        Box::pin(the_notification_driven_function_writes_the_declared_table_body())
    });
}

/// The template the estate case deploys: the secret, the table, the execution role, the
/// serverless function, the grant Amazon S3 needs on it, and the bucket whose object events it
/// is the destination of.
fn estate_template(stack: &str, secret: &str) -> String {
    let bucket = format!("{stack}-bucket");
    json!({
        "Transform": SAM_TRANSFORM,
        "Resources": {
            SECRET: {
                "Type": "AWS::SecretsManager::Secret",
                "Properties": {
                    "Name": format!("{stack}-secret"),
                    "SecretString": secret,
                },
            },
            TABLE: {
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "BillingMode": PAY_PER_REQUEST,
                    "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
                    "AttributeDefinitions": [
                        { "AttributeName": "pk", "AttributeType": "S" },
                    ],
                },
            },
            ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": format!("{stack}-role"),
                    "AssumeRolePolicyDocument": lambda_trust_policy(),
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
                            "SECRET_ARN": { "Ref": SECRET },
                            "TABLE_NAME": { "Ref": TABLE },
                        },
                    },
                },
            },
            PERMISSION: {
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "FunctionName": { "Ref": FUNCTION },
                    "Action": INVOKE_ACTION,
                    "Principal": S3_PRINCIPAL,
                    // The bucket the grant is scoped to is the one declared below, named by the
                    // template so the grant can name it without the two resources depending on
                    // each other. A bucket is global, so its ARN names no Region and no account.
                    "SourceArn": format!("arn:aws:s3:::{bucket}"),
                },
            },
            BUCKET: {
                "Type": "AWS::S3::Bucket",
                // S3 admits a function destination only while that function's own policy lets
                // the service invoke it, so the grant has to be in place before the bucket is
                // configured.
                "DependsOn": [PERMISSION],
                "Properties": {
                    "BucketName": bucket,
                    "NotificationConfiguration": {
                        "LambdaConfigurations": [{
                            "Event": CONFIGURED_EVENT,
                            "Function": { "Fn::GetAtt": [FUNCTION, "Arn"] },
                        }],
                    },
                },
            },
        },
        "Outputs": {
            BUCKET_NAME_OUTPUT: { "Value": { "Ref": BUCKET } },
            TABLE_NAME_OUTPUT: { "Value": { "Ref": TABLE } },
            SECRET_ARN_OUTPUT: { "Value": { "Ref": SECRET } },
        },
    })
    .to_string()
}

async fn the_notification_driven_function_writes_the_declared_table_body() {
    let case = "a write into the published bucket reaches the expanded function, which writes \
                the published table";
    let router = emulator();
    let stack = "sam-full-estate";
    // The value the handler can only have got from Secrets Manager: it is no part of the event,
    // of the key, or of anything the template passes the function but the secret's identifier.
    let secret = r#"{"endpoint":"db.internal","value":"only-in-the-secret"}"#;
    let (outputs, physical) =
        deploy(&router, case, stack, &estate_template(stack, secret)).await;

    for logical in [SECRET, TABLE, ROLE, FUNCTION, PERMISSION, BUCKET] {
        assert!(
            physical.contains_key(logical),
            "case {case:?}: expected a physical id for {logical}, got {physical:?}"
        );
    }

    let table = published(case, &outputs, TABLE_NAME_OUTPUT);
    let bucket = published(case, &outputs, BUCKET_NAME_OUTPUT);
    let secret_arn = published(case, &outputs, SECRET_ARN_OUTPUT);

    // The first data-plane call a client makes with the stack's own output: a read of the key
    // the write below will produce, which the table serves and nothing has written yet.
    let key = json!({ "pk": { "S": "incoming/file1.txt" } });
    let (status, answered, item) = read_item(&router, &table, &key).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetItem on the published table name {table:?}, \
         got {status} with {answered}"
    );
    assert_eq!(
        item, None,
        "case {case:?}: nothing has written the key yet, so the answer must carry no Item, got \
         {answered}"
    );

    // The secret the handler is to read, as Secrets Manager answers for the published ARN.
    let (status, held) =
        secretsmanager(&router, "GetSecretValue", &json!({"SecretId": secret_arn})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetSecretValue on the published ARN, got {status} \
         with {held}"
    );
    assert_eq!(
        held.get("SecretString").and_then(Value::as_str),
        Some(secret),
        "case {case:?}: expected the deployed SecretString {secret:?}, got {held}"
    );

    // The write, into the bucket the stack published.
    let object_key = "incoming/file1.txt";
    let (status, body) = s3(
        &router,
        "PUT",
        &format!("/{bucket}/{object_key}"),
        "the object this stack was deployed to process\n",
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from PutObject({bucket:?}, {object_key:?}), got \
         {status} with {body}"
    );

    // What the handler then wrote, read out of the declared table through DynamoDB's own API.
    let item = awaited_item(&router, case, &table, &key).await;
    assert_eq!(
        item,
        Some(json!({
            "pk": { "S": object_key },
            "secret": { "S": secret },
            "bucket": { "S": bucket },
        })),
        "case {case:?}: expected the handler's item for the record of {object_key:?}, got \
         {item:?}"
    );
}
