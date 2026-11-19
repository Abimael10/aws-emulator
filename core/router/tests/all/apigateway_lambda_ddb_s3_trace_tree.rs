// SPDX-License-Identifier: CC0-1.0

//! The whole DISTRIBUTED TRACE a request to a tracing-enabled REST stage leaves
//! behind: API Gateway -> AWS Lambda -> Amazon DynamoDB / Amazon S3, read back
//! out of AWS X-Ray as ONE tree.
//!
//! The rule under test: **a request a deployed REST stage serves through an
//! `AWS_PROXY` integration is answered with the function's own proxy response,
//! and the chain that request belongs to is one tree of segment documents whose
//! every edge is a `parent_id` resolving to the entity that made the call — the
//! stage's own segment (`origin` `AWS::ApiGateway::Stage`) is the root of the
//! tree when `Stage$tracingEnabled` is set and no segment at all when it is not,
//! its aws-namespace call out to the backend parents the `AWS::Lambda` segment,
//! which parents the `AWS::Lambda::Function` segment, the handler's own
//! aws-namespace subsegments are carried INSIDE the function segment, and X-Ray
//! synthesises one `inferred` node per downstream resource those subsegments
//! addressed, typed by that resource (`AWS::DynamoDB::Table`,
//! `AWS::S3::Bucket`).** Six crates have to agree for that to hold (IAM holds
//! the role, CloudFormation provisions the function, API Gateway resolves the
//! stage and performs the integration, Lambda runs the handler and reports the
//! invocation's trace context, DynamoDB and S3 serve the calls the handler
//! makes, X-Ray holds the documents), so the suite lives here.
//!
//! The rows differ along what the rule generalises over: whether the stage the
//! request reached is TRACING, which decides whether the stage records the
//! request at all and so which document is the root of the tree.
//!
//! Each row asserts the whole path: the status and body the client was served
//! (which is the handler's own), the item DynamoDB holds and the object S3 holds
//! afterwards, and then the tree X-Ray reports for the chain the handler saw.
//!
//! The second rule under test is how a client that does not hold a trace id
//! FINDS that chain at all: **the trace a request leaves behind is reported by
//! `GetTraceSummaries` to any query whose window holds the instant the chain
//! began and whose filter expression selects it, and by none whose window does
//! not — one summary per trace, carrying the entry point the chain started at,
//! one service id per node of its own service graph, the HTTP request the root
//! segment served, and the id that reads the whole tree back through
//! `BatchGetTraces`.** Its rows differ along what that rule generalises over:
//! which hop of the chain the expression names and how (by name, by name and
//! the type it was reported with, by the outcome the chain had), and where the
//! window falls relative to the instant the chain began.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, opt_element, physical_ids_of};

/// The Region every request below is signed for and every ARN names.
const REGION: &str = "us-east-1";

/// The `StackStatus` the setup waits for.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The two stages the api is deployed to: one tracing, one left at the default.
const TRACED_STAGE: &str = "traced";
const UNTRACED_STAGE: &str = "untraced";

/// The resource path the api's one method is bound at.
const RESOURCE_PATH: &str = "x";

/// The table, the bucket and the hash key the handler writes under.
const TABLE: &str = "trace-tree-probe-items";
const BUCKET: &str = "trace-tree-probe-bucket";
const HASH_KEY: &str = "pk";

/// How long one invocation of this handler is given, as the function's own
/// `Timeout`.
const FUNCTION_TIMEOUT: u64 = 30;

/// How long to wait between polls of a clock-owed transition, and how many polls
/// to make. Generous: a loaded host must not decide the outcome.
const POLL: Duration = Duration::from_millis(50);
const POLLS: usize = 600;

/// The `origin` of the segment each hop of this chain records.
const STAGE_ORIGIN: &str = "AWS::ApiGateway::Stage";
const LAMBDA_ORIGIN: &str = "AWS::Lambda";
const FUNCTION_ORIGIN: &str = "AWS::Lambda::Function";
const TABLE_ORIGIN: &str = "AWS::DynamoDB::Table";
const BUCKET_ORIGIN: &str = "AWS::S3::Bucket";

/// The spans the handler reports its two downstream calls under. 16 hexadecimal
/// digits, as a segment id is, and distinct per call so each names one node.
const DDB_SPAN: &str = "aa00000000000001";
const S3_SPAN: &str = "aa00000000000002";

/// The handler the stack deploys: it reports the trace context its own
/// environment carries, performs one DynamoDB write and one S3 write, and sends
/// the X-Ray daemon one aws-namespace subsegment per call, naming the resource
/// the call addressed and the request id the call itself answered with.
const HANDLER_SOURCE: &str = r#"
import json
import os
import socket
import time

import boto3


def _sent(document):
    named = os.environ["AWS_XRAY_DAEMON_ADDRESS"]
    host, _, port = named.rpartition(":")
    datagram = json.dumps({"format": "json", "version": 1}) + "\n" + json.dumps(document)
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as writer:
        writer.sendto(datagram.encode("utf-8"), (host, int(port)))


def handler(event, context):
    raw = os.environ.get("_X_AMZN_TRACE_ID") or ""
    fields = {}
    for field in raw.split(";"):
        name, _, value = field.partition("=")
        if name:
            fields[name] = value
    parameters = event.get("queryStringParameters") or {}
    table = os.environ["PROBE_TABLE"]
    bucket = os.environ["PROBE_BUCKET"]
    key = parameters.get("k") or "probe"
    started = time.time()
    stored = boto3.client("dynamodb").put_item(
        TableName=table, Item={"pk": {"S": key}, "chain": {"S": raw}}
    )
    written = boto3.client("s3").put_object(Bucket=bucket, Key=key, Body=b"traced")
    ddb_request_id = (stored.get("ResponseMetadata") or {}).get("RequestId")
    s3_request_id = (written.get("ResponseMetadata") or {}).get("RequestId")
    calls = [
        (
            parameters.get("ddb_sub_id"),
            "DynamoDB",
            {"operation": "PutItem", "table_name": table, "request_id": ddb_request_id},
        ),
        (
            parameters.get("s3_sub_id"),
            "S3",
            {"operation": "PutObject", "bucket_name": bucket, "request_id": s3_request_id},
        ),
    ]
    # A tracer records only what the request's own sampling decision says is
    # recorded, under the chain and the span the context named.
    if fields.get("Root") and fields.get("Sampled") == "1":
        for span, name, aws in calls:
            if not span:
                continue
            document = {
                "trace_id": fields["Root"],
                "id": span,
                "name": name,
                "type": "subsegment",
                "namespace": "aws",
                "start_time": started,
                "end_time": time.time(),
                "aws": aws,
            }
            if fields.get("Parent"):
                document["parent_id"] = fields["Parent"]
            _sent(document)
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps(
            {
                "raw_trace_header": raw,
                "root": fields.get("Root"),
                "parent": fields.get("Parent"),
                "sampled": fields.get("Sampled"),
                "ddb_request_id": ddb_request_id,
                "s3_request_id": s3_request_id,
            }
        ),
    }
"#;

/// One answered request: its status and its body verbatim.
struct Answered {
    status: u16,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// The emulator as it is ASSEMBLED, on one engine: the table the handler writes,
/// the bucket it writes and the trace store its datagrams reach are the ones
/// this suite reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: a control-plane call carries a
    // credential scope so the router can name the target service, and the invoke
    // of the published stage URL carries none, as a client of one sends none.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is
/// what routes the request. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answered {
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
    Answered {
        status: parts.status.as_u16(),
        body,
    }
}

/// One call over an awsQuery wire (IAM, CloudFormation).
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> Answered {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        "POST",
        &format!("{signing_name}.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization(signing_name)),
        ],
        Bytes::from(body),
    )
    .await
}

/// One API Gateway control-plane call that has to be accepted with `expected`.
async fn apigw(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answered = dispatch(
        router,
        method,
        &format!("apigateway.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("apigateway")),
        ],
        Bytes::from(payload),
    )
    .await;
    assert_eq!(
        answered.status, expected,
        "setup {case:?}: expected HTTP {expected} from {method} {path}, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.json()
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answered {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
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
    .await
}

/// One Amazon DynamoDB call over its `awsJson1_0` wire.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: Value) -> Answered {
    dispatch(
        router,
        "POST",
        &format!("dynamodb.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("authorization", &authorization("dynamodb")),
            ("x-amz-target", &format!("DynamoDB_20120810.{operation}")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// One Amazon S3 call over its REST surface, path-style.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> Answered {
    dispatch(
        router,
        method,
        &format!("s3.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/octet-stream"),
            ("authorization", &authorization("s3")),
        ],
        Bytes::from(body.to_owned()),
    )
    .await
}

/// Every segment document X-Ray holds for `trace_id`, read through
/// `BatchGetTraces` — the operation a client reads a trace back with.
async fn documents_of(router: &MultiServiceRouter, trace_id: &str) -> Vec<Value> {
    let answered = dispatch(
        router,
        "POST",
        &format!("xray.{REGION}.amazonaws.com"),
        "/Traces",
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("xray")),
        ],
        Bytes::from(json!({ "TraceIds": [trace_id] }).to_string()),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "expected HTTP 200 from BatchGetTraces, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered
        .json()
        .get("Traces")
        .and_then(Value::as_array)
        .map(|traces| {
            traces
                .iter()
                .flat_map(|trace| {
                    trace
                        .get("Segments")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                })
                .filter_map(|segment| {
                    segment
                        .get("Document")
                        .and_then(Value::as_str)
                        .and_then(|text| serde_json::from_str::<Value>(text).ok())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Poll `BatchGetTraces` until the chain holds a document for every `origin`
/// wanted, and answer the documents it last reported. The documents arrive as
/// each hop reports them, which no fixed sleep can name; a wait that runs out
/// says so itself, so a hop that never reported is not read downstream as a
/// tree of the wrong shape.
async fn awaited_documents(
    router: &MultiServiceRouter,
    trace_id: &str,
    wanted: &[&str],
) -> Vec<Value> {
    let mut held = Vec::new();
    for _ in 0..POLLS {
        held = documents_of(router, trace_id).await;
        let origins: BTreeSet<&str> = held
            .iter()
            .filter_map(|document| document.get("origin").and_then(Value::as_str))
            .collect();
        if wanted.iter().all(|origin| origins.contains(origin)) {
            return held;
        }
        tokio::time::sleep(POLL).await;
    }
    panic!(
        "expected X-Ray to hold a document for each of {wanted:?} of the chain {trace_id}, got \
         {held:?}"
    );
}

/// A required string member of a JSON object.
fn member(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup {case:?}: no {name:?} in {body}"))
        .to_string()
}

/// Whether `text` is `width` hexadecimal digits, as a trace id's fields and a
/// span id are.
///
/// `pub(crate)` so a sibling suite of this target reading the same grammar — the
/// `apigateway_stage_trace_header_form` one — shares this reading rather than
/// keeping a second copy of X-Ray's id widths.
pub(crate) fn is_hex(text: &str, width: usize) -> bool {
    text.len() == width && text.chars().all(|digit| digit.is_ascii_hexdigit())
}

/// Whether `text` is a trace id of X-Ray's own form, `1-<8 hex>-<24 hex>`.
///
/// `pub(crate)` for the reason [`is_hex`] is.
pub(crate) fn is_trace_id(text: &str) -> bool {
    let mut fields = text.split('-');
    matches!(
        (fields.next(), fields.next(), fields.next(), fields.next()),
        (Some("1"), Some(epoch), Some(unique), None)
            if is_hex(epoch, 8) && is_hex(unique, 24)
    )
}

/// The one document of `documents` whose `origin` is `origin`, asserting there is
/// exactly one: a trace tree holds one node per hop.
fn one_with_origin<'a>(case: &str, documents: &'a [Value], origin: &str) -> &'a Value {
    let found: Vec<&Value> = documents
        .iter()
        .filter(|document| document.get("origin").and_then(Value::as_str) == Some(origin))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one {origin} document, got {found:?} of {}",
        documents.len()
    );
    found[0]
}

/// The text member `name` of `document`, or the empty string.
fn text_of(document: &Value, name: &str) -> String {
    document
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The template the suite deploys: the traced function, on the role it runs as.
fn function_template(role_arn: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Probe": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "TracingConfig": { "Mode": "Active" },
                    "Environment": {
                        "Variables": { "PROBE_TABLE": TABLE, "PROBE_BUCKET": BUCKET },
                    },
                    "Code": { "ZipFile": HANDLER_SOURCE },
                },
            },
        },
    })
    .to_string()
}

/// The trust policy a Lambda execution role needs, and the grants this handler's
/// own calls are authorized by.
fn trust_policy() -> String {
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

fn role_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": [
                "logs:CreateLogGroup",
                "logs:CreateLogStream",
                "logs:PutLogEvents",
                "xray:PutTraceSegments",
                "xray:PutTelemetryRecords",
                "dynamodb:PutItem",
                "s3:PutObject",
            ],
            "Resource": "*",
        }],
    })
    .to_string()
}

/// The role the function runs as, and the ARN IAM minted for it.
async fn given_role(router: &MultiServiceRouter) -> String {
    let created = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", "trace-tree-probe-role"),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateRole must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let arn = opt_element(&created.text(), "Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole must answer the role's ARN"));
    let embedded = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", "trace-tree-probe-role"),
            ("PolicyName", "probe-grants"),
            ("PolicyDocument", &role_document()),
        ],
    )
    .await;
    assert_eq!(
        embedded.status, 200,
        "setup: PutRolePolicy must succeed, got {} with {:?}",
        embedded.status,
        embedded.text()
    );
    arn
}

/// The table the handler writes, waited out to `ACTIVE` — "you can perform read
/// and write operations only on an ACTIVE table".
async fn given_table(router: &MultiServiceRouter) {
    let created = dynamodb(
        router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [{ "AttributeName": HASH_KEY, "KeyType": "HASH" }],
            "AttributeDefinitions": [{ "AttributeName": HASH_KEY, "AttributeType": "S" }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateTable must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    for _ in 0..POLLS {
        let described = dynamodb(router, "DescribeTable", json!({ "TableName": TABLE })).await;
        if described.json()["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
            return;
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("setup: DynamoDB never reported {TABLE} ACTIVE");
}

/// The bucket the handler writes.
async fn given_bucket(router: &MultiServiceRouter) {
    let created = s3(router, "PUT", &format!("/{BUCKET}"), "").await;
    assert!(
        created.status == 200,
        "setup: CreateBucket must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
}

/// The function every request reaches, provisioned from the template above and
/// waited out to `Active`, with the grant that admits an invocation from API
/// Gateway. Answers its name and its ARN.
async fn given_function(router: &MultiServiceRouter, role_arn: &str) -> (String, String) {
    let template = function_template(role_arn);
    let created = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", "trace-tree-probe-stack"),
            ("TemplateBody", &template),
        ],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateStack must be accepted, got {} with {:?}",
        created.status,
        created.text()
    );
    let (reached, described) =
        wait_for_stack_status(router, "trace-tree-probe-stack", CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: expected the stack to reach {CREATE_COMPLETE}, got {described}"
    );
    let resources = query(
        router,
        "cloudformation",
        "2010-05-15",
        "DescribeStackResources",
        &[("StackName", "trace-tree-probe-stack")],
    )
    .await;
    let physical: BTreeMap<String, String> = physical_ids_of(&resources.text());
    let name = physical
        .get("Probe")
        .cloned()
        .unwrap_or_else(|| panic!("setup: no physical id for Probe in {}", resources.text()));

    let mut configuration = Value::Null;
    for _ in 0..POLLS {
        let reported = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        configuration = reported.json();
        if configuration.get("State").and_then(Value::as_str) == Some("Active") {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    assert_eq!(
        configuration.get("State").and_then(Value::as_str),
        Some("Active"),
        "setup: Lambda never reported {name} Active, got {configuration}"
    );
    let arn = member("GetFunctionConfiguration", &configuration, "FunctionArn");
    let granted = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{name}/policy"),
        Some(json!({
            "StatementId": "apigw-invoke",
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
        })),
    )
    .await;
    assert_eq!(
        granted.status, 201,
        "setup: expected HTTP 201 from AddPermission, got {} with {:?}",
        granted.status,
        granted.text()
    );
    (name, arn)
}

/// The api every request reaches: one `GET /{RESOURCE_PATH}` method behind an
/// `AWS_PROXY` integration onto `function_arn`, deployed to both stages, with
/// tracing turned on for [`TRACED_STAGE`] alone.
async fn given_api(router: &MultiServiceRouter, function_arn: &str) -> String {
    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "trace-tree-api" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &created, "id");
    let root = member("CreateRestApi", &created, "rootResourceId");
    let resource = apigw(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": RESOURCE_PATH })),
        201,
    )
    .await;
    let resource_id = member("CreateResource", &resource, "id");
    let method = format!("/restapis/{api}/resources/{resource_id}/methods/GET");
    apigw(
        router,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw(
        router,
        "PutIntegration",
        "PUT",
        &format!("{method}/integration"),
        Some(json!({
            "type": "AWS_PROXY",
            // `integrationHttpMethod` travels under its `jsonName`, which is what
            // `smithy_query(query="operation",operation="PutIntegration")` reports.
            "httpMethod": "POST",
            "uri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/\
                 {function_arn}/invocations"
            ),
        })),
        201,
    )
    .await;
    for stage in [TRACED_STAGE, UNTRACED_STAGE] {
        apigw(
            router,
            "CreateDeployment",
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({ "stageName": stage })),
            201,
        )
        .await;
    }
    apigw(
        router,
        "UpdateStage",
        "PATCH",
        &format!("/restapis/{api}/stages/{TRACED_STAGE}"),
        Some(json!({
            "patchOperations": [
                { "op": "replace", "path": "/tracingEnabled", "value": "true" }
            ]
        })),
        200,
    )
    .await;
    let reported = apigw(
        router,
        "GetStage",
        "GET",
        &format!("/restapis/{api}/stages/{TRACED_STAGE}"),
        None,
        200,
    )
    .await;
    assert_eq!(
        reported.get("tracingEnabled"),
        Some(&json!(true)),
        "setup: expected the stage to report tracingEnabled, got {reported}"
    );
    api
}

/// One row: the stage the request is addressed to, and whether that stage
/// records a segment of its own for it.
struct Row {
    case: &'static str,
    stage: &'static str,
    stage_segment: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the tracing stage, which is the root of the trace it records",
        stage: TRACED_STAGE,
        stage_segment: true,
    },
    Row {
        case: "the stage left at the default, which records nothing of its own",
        stage: UNTRACED_STAGE,
        stage_segment: false,
    },
];

#[tokio::test]
async fn a_traced_proxy_stage_leaves_one_trace_tree_over_lambda_dynamodb_and_s3() {
    let router = emulator();
    let role_arn = given_role(&router).await;
    given_table(&router).await;
    given_bucket(&router).await;
    let (function, function_arn) = given_function(&router, &role_arn).await;
    let api = given_api(&router, &function_arn).await;

    for row in ROWS {
        let case = row.case;
        let key = format!("{}-probe", row.stage);

        // 1. What the client is served: the handler's own proxy response.
        let served = dispatch(
            &router,
            "GET",
            &format!("{api}.execute-api.{REGION}.amazonaws.com"),
            &format!(
                "/{}/{RESOURCE_PATH}?ddb_sub_id={DDB_SPAN}&s3_sub_id={S3_SPAN}&k={key}",
                row.stage
            ),
            &[],
            Bytes::new(),
        )
        .await;
        assert_eq!(
            served.status, 200,
            "case {case:?}: expected HTTP 200 from the stage, got {} with {:?}",
            served.status,
            served.text()
        );
        let reported = served.json();
        let raw = text_of(&reported, "raw_trace_header");
        let root = text_of(&reported, "root");
        let parent = text_of(&reported, "parent");
        assert!(
            is_trace_id(&root) && is_hex(&parent, 16) && text_of(&reported, "sampled") == "1",
            "case {case:?}: expected the invocation's trace context of X-Ray's own form, got \
             {raw:?}"
        );

        // 2. The work the answer reported is work the services really did.
        let read = dynamodb(
            &router,
            "GetItem",
            json!({
                "TableName": TABLE,
                "Key": { HASH_KEY: { "S": key } },
                "ConsistentRead": true,
            }),
        )
        .await;
        assert_eq!(
            read.json()["Item"][HASH_KEY]["S"].as_str(),
            Some(key.as_str()),
            "case {case:?}: expected the table to hold the written item, got {:?}",
            read.text()
        );
        let fetched = s3(&router, "GET", &format!("/{BUCKET}/{key}"), "").await;
        assert_eq!(
            (fetched.status, fetched.text().as_str()),
            (200, "traced"),
            "case {case:?}: expected the bucket to hold the written object, got {} with {:?}",
            fetched.status,
            fetched.text()
        );

        // 3. The tree X-Ray holds for the chain the handler saw.
        let mut wanted = vec![LAMBDA_ORIGIN, FUNCTION_ORIGIN, TABLE_ORIGIN, BUCKET_ORIGIN];
        if row.stage_segment {
            wanted.push(STAGE_ORIGIN);
        }
        let documents = awaited_documents(&router, &root, &wanted).await;
        for origin in &wanted {
            one_with_origin(case, &documents, origin);
        }
        let stage_documents = documents
            .iter()
            .filter(|document| text_of(document, "origin") == STAGE_ORIGIN)
            .count();
        assert_eq!(
            stage_documents,
            usize::from(row.stage_segment),
            "case {case:?}: a stage records a segment of its own only while it is tracing, got \
             {documents:?}"
        );

        // Exactly one root, and it is the hop that began the chain.
        let roots: Vec<&Value> = documents
            .iter()
            .filter(|document| document.get("parent_id").is_none())
            .collect();
        assert_eq!(
            roots.len(),
            1,
            "case {case:?}: a trace is one tree, so it has one root, got {roots:?}"
        );
        let expected_root = if row.stage_segment {
            STAGE_ORIGIN
        } else {
            LAMBDA_ORIGIN
        };
        assert_eq!(
            text_of(roots[0], "origin"),
            expected_root,
            "case {case:?}: expected the root of the tree to be {expected_root}, got {:?}",
            roots[0]
        );
        if row.stage_segment {
            assert_eq!(
                text_of(roots[0], "name"),
                format!("{api}/{}", row.stage),
                "case {case:?}: expected the api and stage as the stage segment's name, got {:?}",
                roots[0]
            );
        }

        // Every edge: each document's parent is an entity of this same trace,
        // and the hops hang off one another in the order the request took them.
        let ids: BTreeSet<String> = documents
            .iter()
            .flat_map(entity_ids)
            .collect();
        let edges = documents
            .iter()
            .filter_map(|document| document.get("parent_id").and_then(Value::as_str))
            .count();
        assert_eq!(
            edges,
            documents.len() - 1,
            "case {case:?}: a tree of {} documents has {} edges, got {edges}",
            documents.len(),
            documents.len() - 1
        );
        for document in &documents {
            if let Some(parent) = document.get("parent_id").and_then(Value::as_str) {
                assert!(
                    ids.contains(parent),
                    "case {case:?}: the parent {parent:?} of {document:?} names no entity of \
                     this trace, which are {ids:?}"
                );
            }
        }

        let service = one_with_origin(case, &documents, LAMBDA_ORIGIN);
        let function_segment = one_with_origin(case, &documents, FUNCTION_ORIGIN);
        assert_eq!(
            text_of(function_segment, "parent_id"),
            text_of(service, "id"),
            "case {case:?}: the function segment hangs from Lambda's own, got \
             {function_segment:?} under {service:?}"
        );
        assert_eq!(
            text_of(function_segment, "id"),
            parent,
            "case {case:?}: the span the invocation reported is the function segment's, got \
             {function_segment:?}"
        );
        assert_eq!(
            text_of(function_segment, "name"),
            function,
            "case {case:?}: the function's own segment names the function, got \
             {function_segment:?}"
        );
        if row.stage_segment {
            // The stage's call OUT is what Lambda's own segment hangs from.
            let call_out = &roots[0]["subsegments"][0];
            assert_eq!(
                (
                    text_of(call_out, "namespace"),
                    text_of(call_out, "name"),
                    text_of(&call_out["aws"], "function_name"),
                ),
                ("aws".to_string(), "Lambda".to_string(), function.clone()),
                "case {case:?}: expected the stage's aws call out onto the function, got {:?}",
                roots[0]
            );
            assert_eq!(
                text_of(service, "parent_id"),
                text_of(call_out, "id"),
                "case {case:?}: Lambda's segment hangs from the stage's call out, got \
                 {service:?}"
            );
        }

        // The handler's own calls: one aws-namespace subsegment each, carried
        // INSIDE the function segment it ran in — a trace is one tree, so an
        // entity submitted as its own document is answered for inside the entity
        // it named — and one inferred node per resource they addressed.
        let carried = function_segment
            .get("subsegments")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (span, name, resource, origin, named) in [
            (
                DDB_SPAN,
                "DynamoDB",
                "table_name",
                TABLE_ORIGIN,
                TABLE.to_string(),
            ),
            (
                S3_SPAN,
                "S3",
                "bucket_name",
                BUCKET_ORIGIN,
                BUCKET.to_string(),
            ),
        ] {
            let call = carried
                .iter()
                .find(|document| text_of(document, "id") == span)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: the handler's {name} call is carried inside the function \
                         segment {function_segment:?}, of {documents:?}"
                    )
                });
            assert_eq!(
                (
                    text_of(call, "type"),
                    text_of(call, "namespace"),
                    text_of(call, "parent_id"),
                    text_of(&call["aws"], resource),
                ),
                (
                    "subsegment".to_string(),
                    "aws".to_string(),
                    text_of(function_segment, "id"),
                    named.clone(),
                ),
                "case {case:?}: expected the handler's {name} call under the function segment, \
                 naming the resource it addressed, got {call:?}"
            );
            let request_id = text_of(&call["aws"], "request_id");
            let expected_request_id = match resource {
                "table_name" => text_of(&reported, "ddb_request_id"),
                _ => text_of(&reported, "s3_request_id"),
            };
            assert_eq!(
                request_id, expected_request_id,
                "case {case:?}: expected the request id the {name} call answered with, got \
                 {call:?}"
            );
            let inferred = one_with_origin(case, &documents, origin);
            assert_eq!(
                (
                    text_of(inferred, "parent_id"),
                    inferred.get("inferred").and_then(Value::as_bool),
                    text_of(&inferred["aws"], resource),
                ),
                (span.to_string(), Some(true), named),
                "case {case:?}: expected an inferred {origin} node hanging off the \
                 handler's {name} call, got {inferred:?}"
            );
        }
    }
}

/// Every entity id one document carries: its own, and those of the subsegments
/// nested inside it — the ids a `parent_id` of this trace may name.
fn entity_ids(document: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(id) = document.get("id").and_then(Value::as_str) {
        ids.push(id.to_string());
    }
    for nested in document
        .get("subsegments")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        ids.extend(entity_ids(nested));
    }
    ids
}

// =============================================================================
// Finding that chain without holding its id: GetTraceSummaries
// =============================================================================

/// How far either side of the instant the chain began a discovery window
/// reaches. Well inside both bounds the read publishes — the widest window it
/// serves, and how far ahead of the clock a window's end may land — so what each
/// row below asserts is the selection, never a refused window.
const WINDOW_BEFORE: f64 = 60.0;
const WINDOW_AFTER: f64 = 60.0;

/// An hour, in the epoch seconds every instant on this wire travels as: the
/// distance a window that holds none of this chain is placed at.
const HOUR: f64 = 3_600.0;

/// How many pages of one window the walk below follows before calling the query
/// unbounded. A window of two minutes is paged by hour-long slices, so one or
/// two pages is what it takes; the cap only has to terminate.
const SUMMARY_PAGES: usize = 32;

/// The `Service$Type` of the node X-Ray carries for the callers of a root
/// segment, which report no segment of their own.
const CLIENT_TYPE: &str = "client";

/// What one query over a window reported: the summaries it served over every
/// page of that window, and the count of traces the window itself holds.
struct Queried {
    summaries: Vec<Value>,
    processed: i64,
}

impl Queried {
    /// The trace ids the query reported.
    fn ids(&self) -> Vec<String> {
        self.summaries
            .iter()
            .filter_map(|summary| summary.get("Id").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    }
}

/// Query `GetTraceSummaries` over `[start, end)` for `filter`, following the
/// window's own pagination to its end — the pages of one query are slices of
/// the window, so the trace a client is looking for need not be on the first.
/// Following them is what a paginating client does, and what this suite reads
/// the selection through; whether the FIRST page of a window already reports
/// what the window holds is a rule of its own, which this walk neither asserts
/// nor stands in for.
async fn summaries_over(
    router: &MultiServiceRouter,
    case: &str,
    start: f64,
    end: f64,
    filter: Option<&str>,
) -> Queried {
    let mut summaries: Vec<Value> = Vec::new();
    let mut processed: Option<i64> = None;
    let mut token: Option<String> = None;
    for page in 0..SUMMARY_PAGES {
        let mut body = json!({ "StartTime": start, "EndTime": end });
        if let Some(expression) = filter {
            body["FilterExpression"] = json!(expression);
        }
        if let Some(resume) = &token {
            body["NextToken"] = json!(resume);
        }
        let answered = dispatch(
            router,
            "POST",
            &format!("xray.{REGION}.amazonaws.com"),
            "/TraceSummaries",
            &[
                ("content-type", "application/json"),
                ("authorization", &authorization("xray")),
            ],
            Bytes::from(body.to_string()),
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected HTTP 200 from GetTraceSummaries over [{start}, {end}) for \
             {filter:?}, got {} with {:?}",
            answered.status,
            answered.text()
        );
        let answer = answered.json();
        summaries.extend(
            answer
                .get("TraceSummaries")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        let counted = answer.get("TracesProcessedCount").and_then(Value::as_i64);
        // The count describes the window, so it is the same on every page of one
        // query — a page that reported a different one would be describing
        // itself instead.
        if let Some(first) = processed {
            assert_eq!(
                counted,
                Some(first),
                "case {case:?}: page {page} must report the count of the window, got {answer}"
            );
        }
        processed = counted.or(processed);
        token = answer
            .get("NextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if token.is_none() {
            return Queried {
                summaries,
                processed: processed.unwrap_or_default(),
            };
        }
    }
    panic!("case {case:?}: the pages of one window must end, stopped after {SUMMARY_PAGES}");
}

/// Whether `summary` reports a service id named `name` and typed `kind`.
fn summary_names_service(summary: &Value, name: &str, kind: &str) -> bool {
    summary
        .get("ServiceIds")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .any(|service| {
            service.get("Name").and_then(Value::as_str) == Some(name)
                && service.get("Type").and_then(Value::as_str) == Some(kind)
        })
}

#[tokio::test]
async fn the_chains_trace_is_found_by_a_client_that_does_not_hold_its_id() {
    let router = emulator();
    let role_arn = given_role(&router).await;
    given_table(&router).await;
    given_bucket(&router).await;
    let (_function, function_arn) = given_function(&router, &role_arn).await;
    let api = given_api(&router, &function_arn).await;

    // One request through the tracing stage, so there is a chain to find.
    let case = "the chain a request to the tracing stage left behind";
    let key = format!("{TRACED_STAGE}-discovery");
    let served = dispatch(
        &router,
        "GET",
        &format!("{api}.execute-api.{REGION}.amazonaws.com"),
        &format!(
            "/{TRACED_STAGE}/{RESOURCE_PATH}?ddb_sub_id={DDB_SPAN}&s3_sub_id={S3_SPAN}&k={key}"
        ),
        &[],
        Bytes::new(),
    )
    .await;
    assert_eq!(
        served.status, 200,
        "case {case:?}: expected HTTP 200 from the stage, got {} with {:?}",
        served.status,
        served.text()
    );
    let trace = text_of(&served.json(), "root");
    assert!(
        is_trace_id(&trace),
        "case {case:?}: expected the invocation to report the chain it belongs to, got {:?}",
        served.text()
    );

    // The hops the chain took, read back out of the stored documents: each
    // origin, and the name the node that reported it carries. The rows below are
    // built from these, so the catalog of hops is the trace's own rather than
    // one this test states twice.
    let origins = [
        STAGE_ORIGIN,
        LAMBDA_ORIGIN,
        FUNCTION_ORIGIN,
        TABLE_ORIGIN,
        BUCKET_ORIGIN,
    ];
    let documents = awaited_documents(&router, &trace, &origins).await;
    let hops: Vec<(&str, String)> = origins
        .iter()
        .map(|origin| {
            (
                *origin,
                text_of(one_with_origin(case, &documents, origin), "name"),
            )
        })
        .collect();
    let stage_name = hops[0].1.clone();
    // Each hop's own name, which every row below is built from: a node reports one,
    // so an empty name would make the rows and the service-id assertions compare a
    // default against a default.
    for (origin, name) in &hops {
        assert!(
            !name.is_empty(),
            "case {case:?}: the {origin} document names the service that reported it, got \
             {documents:?}"
        );
    }
    let began = documents
        .iter()
        .filter_map(|document| document.get("start_time").and_then(Value::as_f64))
        .fold(f64::INFINITY, f64::min);
    assert!(
        began.is_finite() && began > 0.0,
        "case {case:?}: every stored document carries the instant it began, got {documents:?}"
    );
    let (start, end) = (began - WINDOW_BEFORE, began + WINDOW_AFTER);

    // 1. WHICH EXPRESSION finds it. The rows differ along which hop of the chain
    //    the expression names and how: by the name a node carries, by that name
    //    together with the type it was reported with, by a name no hop of the
    //    chain has, and by the outcome the chain had.
    let mut rows: Vec<(String, Option<String>, bool)> = vec![(
        "no expression at all, which selects every trace of the window".to_string(),
        None,
        true,
    )];
    for (origin, name) in &hops {
        rows.push((
            format!("the {origin} hop, by its name and the type it was reported with"),
            Some(format!("service(\"{name}\", \"{origin}\")")),
            true,
        ));
    }
    rows.extend([
        (
            "the stage, by its name alone".to_string(),
            Some(format!("service(\"{stage_name}\")")),
            true,
        ),
        (
            "the stage's name paired with the type of another hop".to_string(),
            Some(format!("service(\"{stage_name}\", \"{LAMBDA_ORIGIN}\")")),
            false,
        ),
        (
            "a service no hop of this chain is".to_string(),
            Some("service(\"a-service-this-chain-never-called\")".to_string()),
            false,
        ),
        (
            "the outcome the chain had, every hop of it having answered".to_string(),
            Some("ok".to_string()),
            true,
        ),
        (
            "an outcome no hop of the chain recorded".to_string(),
            Some("fault".to_string()),
            false,
        ),
    ]);

    for (label, expression, selects) in &rows {
        let queried = summaries_over(&router, label, start, end, expression.as_deref()).await;
        let ids = queried.ids();
        assert_eq!(
            ids.contains(&trace),
            *selects,
            "case {label:?}: expected the window to {} {trace}, got {ids:?}",
            if *selects { "report" } else { "report nothing for" }
        );
        // The count is of the traces the WINDOW holds, "including traces that did
        // not match the specified filter expression", so it does not move with
        // the expression.
        assert!(
            queried.processed >= 1,
            "case {label:?}: the window holds the chain's trace whatever the expression \
             selects, got {} processed",
            queried.processed
        );
    }

    // 2. WHICH WINDOW finds it: the one holding the instant the chain began, and
    //    no window placed away from it.
    let windows: [(&str, f64, f64, bool); 3] = [
        ("the window the chain began in", start, end, true),
        (
            "a window that ended before the chain began",
            began - 3.0 * HOUR,
            began - 2.0 * HOUR,
            false,
        ),
        (
            "a window that started after the chain began",
            began + WINDOW_AFTER,
            began + 2.0 * WINDOW_AFTER,
            false,
        ),
    ];
    for (label, from, to, holds) in windows {
        let queried = summaries_over(&router, label, from, to, None).await;
        let ids = queried.ids();
        assert_eq!(
            ids.contains(&trace),
            holds,
            "case {label:?}: [{from}, {to}) {} the chain that began at {began}, got {ids:?}",
            if holds { "holds" } else { "does not hold" }
        );
    }

    // 3. WHAT the summary says about the chain: the entry point it started at,
    //    one service id per node of its own graph, and the request the root
    //    segment served.
    let queried = summaries_over(&router, case, start, end, None).await;
    let summary = queried
        .summaries
        .iter()
        .find(|summary| summary.get("Id").and_then(Value::as_str) == Some(trace.as_str()))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the window must report the chain's trace, got {:?}",
                queried.ids()
            )
        });
    assert_eq!(
        (
            text_of(&summary["EntryPoint"], "Name"),
            text_of(&summary["EntryPoint"], "Type")
        ),
        (stage_name.clone(), STAGE_ORIGIN.to_string()),
        "case {case:?}: the entry point of the chain is the stage that started it, got {summary}"
    );
    for (origin, name) in &hops {
        assert!(
            summary_names_service(summary, name, origin),
            "case {case:?}: the summary reports one service id per node of the trace, so it must \
             name {name:?} typed {origin:?}, got {summary}"
        );
    }
    // And the caller of the root segment, which reported no segment of its own.
    assert!(
        summary_names_service(summary, &stage_name, CLIENT_TYPE),
        "case {case:?}: the summary names the clients of the root service, got {summary}"
    );
    assert_eq!(
        (
            text_of(&summary["Http"], "HttpMethod"),
            summary["Http"]["HttpStatus"].as_u64(),
        ),
        ("GET".to_string(), Some(200)),
        "case {case:?}: the summary reports the request the root segment served, got {summary}"
    );
    let url = text_of(&summary["Http"], "HttpURL");
    assert!(
        url.starts_with(&format!("https://{api}.execute-api."))
            && url.contains(&format!("/{TRACED_STAGE}/{RESOURCE_PATH}")),
        "case {case:?}: expected the invoke URL the request was made to, got {url:?}"
    );
    for (flag, held) in [
        ("HasFault", false),
        ("HasError", false),
        ("HasThrottle", false),
        ("IsPartial", false),
    ] {
        assert_eq!(
            summary.get(flag).and_then(Value::as_bool),
            Some(held),
            "case {case:?}: every hop of this chain answered, so {flag} is {held}, got {summary}"
        );
    }
    assert!(
        summary["StartTime"]
            .as_f64()
            .is_some_and(|at| (at - began).abs() < 1e-3),
        "case {case:?}: a trace starts at its earliest segment's start, which is {began}, got \
         {summary}"
    );
    let duration = summary["Duration"].as_f64();
    let response_time = summary["ResponseTime"].as_f64();
    assert!(
        duration.is_some_and(|span| span >= 0.0)
            && response_time.is_some_and(|span| span >= 0.0)
            && duration
                .zip(response_time)
                .is_some_and(|(whole, root)| whole + 1e-6 >= root),
        "case {case:?}: a trace lasts at least as long as its root segment answered in, got \
         {duration:?} and {response_time:?}"
    );

    // 4. "To get the full traces, pass the trace IDs to BatchGetTraces": the id
    //    the summary reported is what reads the whole tree back.
    let discovered = text_of(summary, "Id");
    let read_back = documents_of(&router, &discovered).await;
    let reported: BTreeSet<String> = read_back
        .iter()
        .map(|document| text_of(document, "origin"))
        .collect();
    for origin in origins {
        assert!(
            reported.contains(origin),
            "case {case:?}: the id the summary reported reads the whole tree back, so it must \
             hold the {origin} hop, got {reported:?}"
        );
    }
}
