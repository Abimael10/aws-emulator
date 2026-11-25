// SPDX-License-Identifier: CC0-1.0

//! What the CASE of a key-name `suffix` rule decides, in BOTH directions, read through the only
//! thing a client can see: the object the invoked handler writes back into the bucket.
//!
//! The rule under test: **a key-name `FilterRule` is compared with an object's key BYTE FOR BYTE
//! — a `suffix` rule as a literal trailing substring of the whole key — so a configuration is
//! published an object event only while every rule it carries admits that key literally; a key
//! differing from a rule's value only in the case of its characters is published to nobody,
//! whichever of the two spells the lower case, and however the destination's own handler would
//! have judged that key.**
//!
//! Its siblings reach the same rule with a LOWER-CASE rule value and a prefix rule beside it —
//! [`crate::s3_notification_key_filter_is_literal`] spans the ways a key fails one of two rules,
//! [`crate::s3_notification_invokes_its_lambda_target`] spans which rule kinds a configuration
//! carries, and [`crate::s3_notification_converts_on_a_started_emulator`] drives the pair on the
//! emulator a client starts. What only this suite reaches is the SYMMETRY of the comparison: a
//! configuration whose rule value is itself spelled in upper case admits the upper-case key and
//! suppresses the lower-case one, which is what tells a byte comparison from a matcher that
//! folds one side's case before comparing — the two are indistinguishable while every rule value
//! a suite states is lower case.
//!
//! The rows differ along what the rule generalises over: the case the RULE's value is spelled in
//! (lower, upper) and the case the KEY's own suffix is spelled in (the rule's own, the opposite,
//! mixed). Each bucket carries one configuration, so a row's outcome is its own configuration's
//! verdict; the handler deployed here accepts a key case-INSENSITIVELY, so a row that stays
//! unconverted can only have stayed unconverted because Amazon S3 never published it. The ways a
//! key can fail a LOWER-CASE rule (its position in the key, a second rule beside it) are the
//! sibling's table and are not restated here.
//!
//! Every request below is dispatched through a BOXED future ([`Box::pin`]). The assembled
//! router's dispatch future carries the whole emulator's handler state inline, so a test holding
//! several of them across await points lays that state out on the test thread's stack and
//! overflows it as the emulator grows; the box puts each one on the heap instead.

use std::collections::BTreeMap;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, outputs_of};
use crate::s3_notification_invokes_its_lambda_target::driven;

/// The Region every request here is signed for. It is the Region the shared stack reader
/// ([`wait_for_stack_status`]) signs for too, so the stack this suite creates is the one it reads.
const REGION: &str = "us-east-1";

/// The XML namespace a client's Amazon S3 payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The `StackStatus` the one stack this suite deploys has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The service principal Amazon S3 publishes notifications under, and the action it performs on a
/// function destination ("for Amazon Web Services services, the principal is a domain-style
/// identifier that the service defines, such as s3.amazonaws.com", `AddPermission$Principal`).
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The event class each configuration names: "you can enable notifications for every event of a
/// type by using the wildcard", so this is every object-created event of the bucket.
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";

/// The prefix the handler writes its own output under. No configuration here filters on a prefix,
/// so what keeps a conversion from triggering another one is its suffix: the handler writes
/// `.json`, which no rule value below admits.
const OUTPUT_PREFIX: &str = "converted/";

/// How long one invocation of the handler is given, as the function's own `Timeout`. Nothing here
/// tests a function's own timeout, so it is given generous room: a run shares this host with the
/// rest of this test target.
const FUNCTION_TIMEOUT: u64 = 60;

/// How long this suite looks for work the clock owes — the stack's resources coming up, the
/// function reported `Active`, the asynchronous delivery of an object event — and how often it
/// looks while waiting. Generous: no span here is one this suite asserts on, only one it refuses
/// to wait out forever.
const WAIT: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// How long each bucket's output listing is watched once every admitted row has been converted,
/// before the absence of every other conversion is read as an absence. Opened only after the
/// admitted rows have arrived, so the pipeline is known live and known to have drained an event
/// by then.
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
/// A record's key "is URL encoded" (Amazon S3 User Guide, *Event message structure*), so it is
/// decoded before being used as a key again. The handler's OWN admission of a key is
/// case-INSENSITIVE, so every key uploaded below would be converted had Amazon S3 published it:
/// the rows that expect no conversion can only pass because the delivery never happened.
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
        if ".csv" not in key.lower():
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

/// One armed bucket: the value its one `suffix` rule carries, and the bucket it is armed on.
struct Armed {
    case: &'static str,
    bucket: &'static str,
    suffix: &'static str,
}

/// The two configurations, differing only in the CASE of the one rule value they carry. One
/// bucket each, so each row's outcome is the verdict of the configuration it was written into.
const ARMED: &[Armed] = &[
    Armed {
        case: "a suffix rule spelled in lower case",
        bucket: "notification-suffix-case-lower",
        suffix: ".csv",
    },
    Armed {
        case: "a suffix rule spelled in upper case",
        bucket: "notification-suffix-case-upper",
        suffix: ".CSV",
    },
];

/// One row: the key a client writes into `bucket`, and whether that bucket's one `suffix` rule
/// admits it.
struct Row {
    case: &'static str,
    bucket: &'static str,
    key: &'static str,
    admitted: bool,
}

/// The rows span the case the rule's value is spelled in and the case the key's own suffix is
/// spelled in, with the two readings of a case mismatch — the key in the rule's own case, and in
/// the other one — stated for EACH rule value, which is what makes the comparison's symmetry
/// visible. The ways a key can fail a LOWER-CASE rule are the sibling's table
/// ([`crate::s3_notification_key_filter_is_literal`]) and are not restated here; this suite
/// carries one lower-case pair as the live positive control the absences below are read against.
const ROWS: &[Row] = &[
    Row {
        case: "the key's suffix spelled as the lower-case rule spells it",
        bucket: "notification-suffix-case-lower",
        key: "orders.csv",
        admitted: true,
    },
    Row {
        case: "the key's suffix in upper case against the lower-case rule",
        bucket: "notification-suffix-case-lower",
        key: "upper.CSV",
        admitted: false,
    },
    Row {
        case: "the key's suffix spelled as the upper-case rule spells it",
        bucket: "notification-suffix-case-upper",
        key: "reports.CSV",
        admitted: true,
    },
    Row {
        case: "the key's suffix in lower case against the upper-case rule",
        bucket: "notification-suffix-case-upper",
        key: "lower.csv",
        admitted: false,
    },
    Row {
        case: "the key's suffix in mixed case against the upper-case rule",
        bucket: "notification-suffix-case-upper",
        key: "mixed.cSV",
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
/// are dispatched back into this same router, so the function S3 invokes and the buckets the
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
async fn create_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let answered = s3(router, "PUT", &format!("/{bucket}"), "").await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from CreateBucket({bucket:?}), got {} with {:?}",
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
/// [`OBJECT_CREATED`], filtered by one `suffix` rule and nothing else.
///
/// The elements are the ones S3's own model binds the Lambda list to
/// (`smithy_query(query="shape",shape="NotificationConfiguration")`:
/// `LambdaFunctionConfigurations` is bound as `CloudFunctionConfiguration`, whose ARN member is
/// bound as `CloudFunction`), and `suffix` is a value of `FilterRuleName`
/// (`smithy_query(query="shape",shape="FilterRuleName")` reports `[prefix, suffix]`).
async fn arm(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    suffix: &str,
    function_arn: &str,
) {
    let body = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\"><CloudFunctionConfiguration>\
         <Id>convert-the-uploaded-csv</Id><CloudFunction>{function_arn}</CloudFunction>\
         <Event>{OBJECT_CREATED}</Event><Filter><S3Key>\
         <FilterRule><Name>suffix</Name><Value>{suffix}</Value></FilterRule>\
         </S3Key></Filter></CloudFunctionConfiguration></NotificationConfiguration>"
    );
    let answered = s3(router, "PUT", &format!("/{bucket}?notification"), &body).await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from PutBucketNotificationConfiguration({bucket:?}), \
         got {} with {:?}",
        answered.status,
        answered.text()
    );
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
    let mut keys = elements(&answered.text(), "Key");
    keys.sort();
    keys
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

/// The stack this suite deploys ONCE: the execution role the handler's own Amazon S3 calls are
/// made as, and the function, whose inline code CloudFormation archives into its deployment
/// package. The grant Amazon S3 needs and each bucket's configuration are NOT declared here —
/// they are what the suite states through each service's own API, the way a client wires a
/// function it has already deployed.
fn template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "HandlerRole": {
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
                    // The handler calls Amazon S3 with temporary credentials for this role, so
                    // the role has to grant the read and the write it makes.
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
            },
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

/// RULE: a key-name `suffix` rule is compared with an object's key byte for byte, so each armed
/// bucket converts exactly the keys whose own suffix is spelled as its rule spells it — whichever
/// of the two is the lower-case one — and converts no key that differs from it only in case.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn a_suffix_rule_admits_the_key_that_spells_it_byte_for_byte() {
    driven("s3-notification-suffix-rule-case", || {
        Box::pin(a_suffix_rule_admits_the_key_that_spells_it_byte_for_byte_body())
    });
}

async fn a_suffix_rule_admits_the_key_that_spells_it_byte_for_byte_body() {
    let router = emulator();

    // Every row is written into one of the buckets ARMED names: a row naming any other would be
    // written into a bucket carrying no configuration at all, where an absence would hold for a
    // reason this suite is not about.
    for row in ROWS {
        assert!(
            ARMED.iter().any(|armed| armed.bucket == row.bucket),
            "case {:?}: {:?} is no bucket this suite arms, so the row would prove nothing; the \
             armed buckets are {:?}",
            row.case,
            row.bucket,
            ARMED.iter().map(|armed| armed.bucket).collect::<Vec<_>>()
        );
    }

    for armed in ARMED {
        create_bucket(&router, armed.case, armed.bucket).await;
    }

    let stack = "notification-suffix-rule-case-stack";
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

    // The grant Amazon S3 reads before it records a configuration: "in the case of Lambda
    // destinations, Amazon S3 verifies that the Lambda function permissions grant Amazon S3
    // permission to invoke the function from the Amazon S3 bucket"
    // (`PutBucketNotificationConfiguration`'s own documentation) — one statement per publishing
    // bucket, since each grant is scoped to the bucket that invokes.
    for armed in ARMED {
        let granted = lambda(
            &router,
            "POST",
            &format!("/2015-03-31/functions/{function_name}/policy"),
            Some(json!({
                "StatementId": format!("allow-{}", armed.bucket),
                "Action": INVOKE_ACTION,
                "Principal": S3_PRINCIPAL,
                "SourceArn": bucket_arn(armed.bucket),
            })),
        )
        .await;
        assert_eq!(
            granted.status,
            201,
            "case {:?}: expected HTTP 201 from AddPermission, got {} with {:?}",
            armed.case,
            granted.status,
            granted.text()
        );
        arm(
            &router,
            armed.case,
            armed.bucket,
            armed.suffix,
            &function_arn,
        )
        .await;
    }

    // Every row's upload, in one pass, so the keys no configuration admits are already written
    // when the admitted ones are read back.
    for row in ROWS {
        put_object(&router, row.case, row.bucket, row.key).await;
    }

    // The ADMITTED rows: the conversion the published event owes, with no further API call than
    // the write itself.
    for row in ROWS.iter().filter(|row| row.admitted) {
        let case = row.case;
        let output = output_key(row.key);
        let read = awaited_object(&router, row.bucket, &output)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the object the invoked handler owes at {output:?} \
                     for {:?} in {:?}, got none",
                    row.key, row.bucket
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

    // And every other key reached the function not at all, however long this keeps looking — read
    // against a pipeline the rows above have already shown live and shown to have drained an
    // event. Each bucket is read on its own, so one bucket's conversion cannot stand in for the
    // other's.
    let expected: BTreeMap<&str, Vec<String>> = ARMED
        .iter()
        .map(|armed| {
            let mut keys: Vec<String> = ROWS
                .iter()
                .filter(|row| row.admitted && row.bucket == armed.bucket)
                .map(|row| output_key(row.key))
                .collect();
            keys.sort();
            (armed.bucket, keys)
        })
        .collect();
    let looks = QUIET_WINDOW.as_millis() / LOOK_INTERVAL.as_millis();
    for _ in 0..looks {
        for armed in ARMED {
            let held = output_keys(&router, armed.case, armed.bucket).await;
            assert_eq!(
                Some(&held),
                expected.get(armed.bucket),
                "case {:?}: expected {:?} to hold exactly the conversions of the keys its rule \
                 {:?} admits, got {held:?}",
                armed.case,
                armed.bucket,
                armed.suffix
            );
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}
