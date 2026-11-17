// SPDX-License-Identifier: CC0-1.0

//! What a bucket notification's key-name `Filter` admits, read through the only thing a client
//! can see: the object the invoked handler writes back into the bucket.
//!
//! The rule under test: **an object event is published to a configured Lambda destination only
//! when the object's key passes EVERY key-name `FilterRule` the configuration carries, each rule
//! compared LITERALLY — a `prefix` rule as a literal leading substring and a `suffix` rule as a
//! literal trailing substring of the whole key, byte for byte and so case-sensitively — so a key
//! differing from a rule's value only in case, or carrying it anywhere but at the end (or the
//! start), is not published at all, however the handler itself would have judged that key.**
//!
//! That last clause is what tells S3-side filtering from handler-side filtering, so the handler
//! below accepts a case-insensitive suffix: a row whose key the filter rejects can only stay
//! unconverted because S3 never delivered it.
//!
//! Its siblings reach other configurations of the same rule, and each is the only one that
//! reaches its own: [`crate::s3_notification_converts_on_a_started_emulator`] hand-wires the
//! pipeline (IAM, `CreateFunction` with a real .zip, `AddPermission`) on the emulator a client
//! STARTS, and [`crate::s3_notification_invokes_its_lambda_target`] pins what a `Filter` admits
//! through a marker handler. What only this suite reaches is the conversion a handler writes back
//! into the bucket on the ASSEMBLED router, with the function and its execution role provisioned
//! by a CloudFormation stack — the way a client deploys one — so the filter's literal matching is
//! judged on the router every service of this emulator is mounted on.
//!
//! The rows differ along what the rule generalises over: which rule the key satisfies, and in
//! what way it fails the other — the case of the suffix, the case of the prefix, the suffix's
//! position in the key, a different prefix, and a missing prefix — with the admitted rows
//! themselves spanning a flat key and one several prefixes deep.
//!
//! Every request is dispatched through a BOXED future ([`Box::pin`]): the assembled router's
//! dispatch future carries the whole emulator's handler state inline, so a test holding several
//! of them across await points lays that state out on the test thread's stack.

use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, outputs_of};
use crate::s3_notification_invokes_its_lambda_target::driven;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The XML namespace a client's S3 payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The `StackStatus` the one stack this suite deploys has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The service principal Amazon S3 publishes notifications under, and the action it performs on
/// a function destination (`AddPermission$Principal`).
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The event class the configuration names: every object-created event of the bucket.
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";

/// The two key-name filter rules the configuration carries, and the prefix the handler writes
/// its own output under — which the filter admits no key of, so a conversion never triggers
/// another one.
const FILTER_PREFIX: &str = "raw/";
const FILTER_SUFFIX: &str = ".csv";
const OUTPUT_PREFIX: &str = "converted/";

/// How long one invocation of the handler is given, as the function's own `Timeout`.
const FUNCTION_TIMEOUT: u64 = 60;

/// How long this suite looks for work the clock owes — a stack's resources coming up, a function
/// reported `Active`, the asynchronous delivery of an object event — and how often it looks while
/// waiting. Generous: no span here is one the suite asserts on, only one it refuses to wait out
/// forever.
const WAIT: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// How long a key the filter REJECTS is watched for an output object before concluding none is
/// coming. Read only after every admitted row has already been converted, so the pipeline is
/// known live and known to have drained an event by then.
const QUIET_WINDOW: Duration = Duration::from_secs(3);

/// The CSV every row uploads: a header row and two data rows.
const SOURCE_CSV: &str = "order,total\nA-1,42\nA-2,7\n";

/// The rows the handler reads out of [`SOURCE_CSV`], as the document it writes reports them.
fn source_cells() -> Value {
    json!([["order", "total"], ["A-1", "42"], ["A-2", "7"]])
}

/// The handler the pipeline runs: for every record it is handed it reads the object the record
/// named and writes the rows it parsed back into the SAME bucket under [`OUTPUT_PREFIX`].
///
/// Its OWN admission of a key is case-INSENSITIVE, so a key whose suffix differs from the
/// configuration's only in case would be converted had S3 published it: the rows below that
/// expect no output can therefore only pass because the delivery never happened.
const CONVERTING_HANDLER: &str = r#"
import csv
import io
import json
import urllib.parse

import boto3

CLIENT = boto3.client("s3")

OUTPUT_PREFIX = "converted/"


def handler(event, context):
    written = []
    for record in event["Records"]:
        bucket = record["s3"]["bucket"]["name"]
        key = urllib.parse.unquote_plus(record["s3"]["object"]["key"])
        if not key.lower().endswith(".csv"):
            continue
        source = CLIENT.get_object(Bucket=bucket, Key=key)["Body"].read().decode("utf-8")
        rows = [row for row in csv.reader(io.StringIO(source)) if row]
        output = OUTPUT_PREFIX + key + ".json"
        CLIENT.put_object(
            Bucket=bucket,
            Key=output,
            Body=json.dumps(rows).encode("utf-8"),
        )
        written.append(output)
    return {"written": written}
"#;

/// One row: the key the client uploads, and whether every rule of the configuration's filter
/// admits it.
struct Row {
    case: &'static str,
    key: &'static str,
    admitted: bool,
}

/// The rows span which rule the key satisfies and how it fails the other.
const ROWS: &[Row] = &[
    Row {
        case: "a flat key both rules admit",
        key: "raw/orders.csv",
        admitted: true,
    },
    Row {
        case: "a key several prefixes deep under the admitted prefix",
        key: "raw/2026/03/orders.csv",
        admitted: true,
    },
    Row {
        case: "the suffix in upper case",
        key: "raw/orders.CSV",
        admitted: false,
    },
    Row {
        case: "the suffix in mixed case",
        key: "raw/orders.Csv",
        admitted: false,
    },
    Row {
        case: "the prefix in upper case",
        key: "RAW/orders.csv",
        admitted: false,
    },
    Row {
        case: "the suffix carried inside the key rather than at its end",
        key: "raw/orders.csv.bak",
        admitted: false,
    },
    Row {
        case: "the admitted suffix under another prefix",
        key: "incoming/orders.csv",
        admitted: false,
    },
    Row {
        case: "the admitted suffix under no prefix at all",
        key: "orders.csv",
        admitted: false,
    },
];

/// The key the handler owes for `key`, had the event been published to it.
fn output_key(key: &str) -> String {
    format!("{OUTPUT_PREFIX}{key}.json")
}

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

/// The emulator as it is ASSEMBLED, on one engine: S3's delivery and the handler's own SDK calls
/// are dispatched back into this same router, so the function S3 invokes and the bucket the
/// handler writes are the ones this suite reads.
fn emulator() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: each call carries a credential scope so the
    // router can name the target service, and the signature itself is never checked.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes the
/// request.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
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
    let response =
        Box::pin(router.dispatch(builder.body(body).expect("a well-formed request"))).await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        body,
    }
}

/// One CloudFormation call over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> Answered {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        "POST",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("cloudformation")),
        ],
        Bytes::from(body),
    )
    .await
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

/// One call on Amazon S3's REST surface, path-style.
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

/// The ARN of a bucket, which is how a grant on the destination function names the source of the
/// invocation. A bucket is global, so the ARN names no Region and no account.
fn bucket_arn(bucket: &str) -> String {
    format!("arn:aws:s3:::{bucket}")
}

/// `PUT /<bucket>` — `CreateBucket`.
async fn create_bucket(router: &MultiServiceRouter, bucket: &str) {
    let answered = s3(router, "PUT", &format!("/{bucket}"), "").await;
    assert_eq!(
        answered.status,
        200,
        "setup: expected HTTP 200 from CreateBucket({bucket:?}), got {} with {:?}",
        answered.status,
        answered.text()
    );
}

/// `PUT /<bucket>/<key>` — `PutObject`. The write is accepted whatever the notification
/// configuration does with it, so every row asserts it.
async fn put_object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) {
    let answered = s3(router, "PUT", &format!("/{bucket}/{key}"), SOURCE_CSV).await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from PutObject({bucket:?}, {key:?}), got {} with {:?}",
        answered.status,
        answered.text()
    );
}

/// `PUT /<bucket>?notification` with ONE Lambda configuration naming `function_arn` for
/// [`OBJECT_CREATED`], filtered by both key-name rules.
///
/// The elements are the ones S3's own model binds the Lambda list to
/// (`smithy_query(query="shape",shape="NotificationConfiguration")`: `LambdaFunctionConfigurations`
/// is bound as `CloudFunctionConfiguration`, whose ARN member is bound as `CloudFunction`).
async fn put_lambda_notification(
    router: &MultiServiceRouter,
    bucket: &str,
    function_arn: &str,
) -> Answered {
    let body = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\"><CloudFunctionConfiguration>\
         <Id>convert-the-uploaded-csv</Id><CloudFunction>{function_arn}</CloudFunction>\
         <Event>{OBJECT_CREATED}</Event><Filter><S3Key>\
         <FilterRule><Name>prefix</Name><Value>{FILTER_PREFIX}</Value></FilterRule>\
         <FilterRule><Name>suffix</Name><Value>{FILTER_SUFFIX}</Value></FilterRule>\
         </S3Key></Filter></CloudFunctionConfiguration></NotificationConfiguration>"
    );
    s3(router, "PUT", &format!("/{bucket}?notification"), &body).await
}

/// The keys `bucket` holds under [`OUTPUT_PREFIX`] right now, read through `ListObjectsV2`.
async fn output_keys(router: &MultiServiceRouter, case: &str, bucket: &str) -> Vec<String> {
    let answered = s3(
        router,
        "GET",
        &format!("/{bucket}?list-type=2&prefix={OUTPUT_PREFIX}"),
        "",
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from ListObjectsV2({bucket:?}), got {} with {:?}",
        answered.status,
        answered.text()
    );
    elements(&answered.text(), "Key")
}

/// Look for the object the handler owes at `key` until `GetObject` reports it, and answer what it
/// read. The wait is on the CONDITION; nothing asserts how long it took.
async fn awaited_object(router: &MultiServiceRouter, bucket: &str, key: &str) -> Option<Answered> {
    let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
    for _ in 0..looks {
        let read = s3(router, "GET", &format!("/{bucket}/{key}"), "").await;
        if read.status == 200 {
            return Some(read);
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    None
}

/// The trust policy a Lambda execution role needs, and the grants the handler's own calls need.
fn role_resource() -> Value {
    json!({
        "Type": "AWS::IAM::Role",
        "Properties": {
            "AssumeRolePolicyDocument": {
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "Service": "lambda.amazonaws.com" },
                    "Action": "sts:AssumeRole",
                }],
            },
            // The handler calls Amazon S3 with temporary credentials for this role, so the role
            // has to grant the read and the write it makes.
            "Policies": [{
                "PolicyName": "convert-the-uploaded-csv",
                "PolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Action": ["s3:GetObject", "s3:PutObject", "logs:*"],
                        "Resource": "*",
                    }],
                },
            }],
        },
    })
}

/// The stack this suite deploys ONCE: the execution role and the function, whose inline code
/// CloudFormation archives into its deployment package.
fn template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "HandlerRole": role_resource(),
            "Handler": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["HandlerRole", "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": CONVERTING_HANDLER },
                },
            },
        },
        "Outputs": {
            "FunctionArn": { "Value": { "Fn::GetAtt": ["Handler", "Arn"] } },
            "FunctionName": { "Value": { "Ref": "Handler" } },
        },
    })
    .to_string()
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active` — "the
/// function is ready to be invoked" (`FunctionConfiguration$State`). The span is that service's
/// own, so nothing here sleeps a fixed span on its behalf.
async fn await_active_function(router: &MultiServiceRouter, name: &str) {
    let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
    for _ in 0..looks {
        let reported = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        if reported.json().get("State").and_then(Value::as_str) == Some("Active") {
            return;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("setup: Lambda never reported {name} Active");
}

/// RULE: every key-name rule of a configuration's filter is compared literally, so only the keys
/// every rule admits are published — and the handler's own case-insensitive judgement of a key
/// never gets to see the rest.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn only_the_keys_every_literal_filter_rule_admits_are_published() {
    driven("s3-notification-literal-filter", || {
        Box::pin(only_the_keys_every_literal_filter_rule_admits_are_published_body())
    });
}

async fn only_the_keys_every_literal_filter_rule_admits_are_published_body() {
    let router = emulator();
    let bucket = "notification-literal-filter-rules";
    create_bucket(&router, bucket).await;

    let stack = "notification-literal-filter-rules-stack";
    let body = template();
    let created = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &body),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "setup: expected HTTP 200 from CreateStack, got {} with {:?}",
        created.status,
        created.text()
    );
    let (reached, described) = wait_for_stack_status(&router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: expected the stack to reach {CREATE_COMPLETE}, got {described}"
    );
    let outputs = outputs_of(&described);
    let function_arn = outputs
        .get("FunctionArn")
        .cloned()
        .unwrap_or_else(|| panic!("setup: no FunctionArn output in {described}"));
    let function_name = outputs
        .get("FunctionName")
        .cloned()
        .unwrap_or_else(|| panic!("setup: no FunctionName output in {described}"));
    await_active_function(&router, &function_name).await;

    let granted = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{function_name}/policy"),
        Some(json!({
            "StatementId": "allow-this-bucket",
            "Action": INVOKE_ACTION,
            "Principal": S3_PRINCIPAL,
            "SourceArn": bucket_arn(bucket),
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup: expected HTTP 201 from AddPermission, got {} with {:?}",
        granted.status,
        granted.text()
    );

    let armed = put_lambda_notification(&router, bucket, &function_arn).await;
    assert_eq!(
        armed.status,
        200,
        "setup: expected HTTP 200 from PutBucketNotificationConfiguration, got {} with {:?}",
        armed.status,
        armed.text()
    );

    // Every row's upload, in one pass, so the rejected keys are already written when the
    // admitted ones are read back.
    for row in ROWS {
        put_object(&router, row.case, bucket, row.key).await;
    }

    // The ADMITTED rows: the conversion the published event owes, with no further API call.
    for row in ROWS.iter().filter(|row| row.admitted) {
        let case = row.case;
        let output = output_key(row.key);
        let read = awaited_object(&router, bucket, &output).await.unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the object the invoked handler owes at {output:?} for \
                 {:?}, got none",
                row.key
            )
        });
        let recorded: Value = serde_json::from_slice(&read.body).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the converted object must carry the rows the handler read \
                 ({error}), got {:?}",
                read.text()
            )
        });
        assert_eq!(
            recorded,
            source_cells(),
            "case {case:?}: expected the rows of the uploaded CSV, got {recorded}"
        );
    }

    // The REJECTED rows: nothing, however long this keeps looking — read against a pipeline the
    // rows above have already shown live and drained.
    let expected: Vec<String> = {
        let mut keys: Vec<String> = ROWS
            .iter()
            .filter(|row| row.admitted)
            .map(|row| output_key(row.key))
            .collect();
        keys.sort();
        keys
    };
    let looks = QUIET_WINDOW.as_millis() / LOOK_INTERVAL.as_millis();
    for _ in 0..looks {
        let mut held = output_keys(&router, "a key no rule of the filter admits", bucket).await;
        held.sort();
        assert_eq!(
            held, expected,
            "case \"a key no rule of the filter admits\": expected the bucket to hold exactly \
             the conversions of the admitted keys, got {held:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}
