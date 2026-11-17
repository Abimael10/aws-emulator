// SPDX-License-Identifier: CC0-1.0

//! The key-name FILTER leg of the armed S3-to-Lambda pipeline, driven against the emulator a
//! client STARTS — the built binary, on its own port, with request authentication on, reached
//! over TLS with SigV4-signed calls.
//!
//! The rule under test: **once a bucket's notification configuration names a Lambda function for
//! an event class and that function's resource policy lets Amazon S3 invoke it for the bucket,
//! every object event whose key passes EVERY key-name `FilterRule` of the configuration is
//! published to that function asynchronously — with no further API call from the client — and no
//! other event is; each rule is compared LITERALLY, a `prefix` as a literal leading substring and
//! a `suffix` as a literal trailing substring of the whole key, byte for byte and so
//! case-sensitively, so a key differing from a rule's value only in case is published to nobody,
//! however the handler itself would have judged it.**
//!
//! The handler deployed here accepts a key case-INSENSITIVELY (`key.lower().endswith(".csv")`),
//! which is what makes each suppressed row mean something: a key the configuration's filter
//! rejects can only stay unconverted because Amazon S3 never delivered it, and not because the
//! handler declined it. The admitted row is written LAST and read first, so each absence is read
//! against a pipeline already known to be live and known to have drained an event.
//!
//! This module carries NO test of its own: it is one LEG of
//! [`crate::cfn_deployed_pipeline_on_a_started_emulator`], which calls it on the emulator that
//! suite has already started. Starting the binary costs what starting the emulator costs, so the
//! two legs share one start rather than paying it twice, and this one spans only what the other
//! does not reach — a key-name filter, and the case of its rules' characters. What the rule
//! generalises over is spanned in process by [`crate::s3_notification_key_filter_is_literal`];
//! what only the started binary reaches is the chain behind VERIFIED credentials, over TLS, with
//! the delivery and the handler's own SDK calls made on the binary's own multi-thread runtime.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::{deployed, output, Started};

/// Where Lambda binds the function collection.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The XML namespace a client puts on an Amazon S3 request payload.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The service principal Amazon S3 publishes notifications under, and the action it performs on a
/// function destination ("for Amazon Web Services services, the principal is a domain-style
/// identifier that the service defines, such as s3.amazonaws.com", `AddPermission$Principal`).
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The event class the configuration names: "you can enable notifications for every event of a
/// type by using the wildcard", so this is every object-created event of the bucket.
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";

/// The bucket this leg writes into, and the stack that brings up the function it is wired to.
const BUCKET: &str = "started-emulator-armed-notification";
const STACK: &str = "started-emulator-armed-notification-handler";

/// The two key-name rules the configuration's filter carries — the rule kinds `FilterRuleName`
/// publishes (`smithy_query(query="shape",shape="FilterRuleName")` reports `[prefix, suffix]`) —
/// and the prefix the handler writes under, which no rule of the filter admits, so a conversion
/// never triggers another one.
const FILTER_PREFIX: &str = "raw/";
const FILTER_SUFFIX: &str = ".csv";
const CONVERTED_PREFIX: &str = "converted/";

/// The `State` a function serves invocations in ("the function is ready to be invoked",
/// `FunctionConfiguration$State`).
const FUNCTION_ACTIVE: &str = "Active";

/// The `Timeout` the function is created with. Nothing here tests a function's own timeout, so it
/// is given generous room: a run shares this host with the rest of this test target.
const FUNCTION_TIMEOUT: u64 = 60;

/// How long this leg waits for work the CLOCK owes — the function's provisioning window and the
/// asynchronous delivery of an object event — and how often it looks while waiting. Every wait is
/// on a CONDITION; nothing here asserts how long one took.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// How long a suppressed row is watched before its absence is read as an absence. Read only once
/// the admitted row has already been converted, so the pipeline is known live by then.
const QUIET_WINDOW: Duration = Duration::from_secs(3);

/// The CSV every object this leg writes holds, so a delivery that happened always converts: an
/// absence is then the delivery Amazon S3 did not make and never a handler that failed.
const SOURCE_CSV: &str = "order,total\nA-1,42\nA-2,7\n";

/// What that CSV converts to, which is what the handler must have written.
fn converted_document() -> Value {
    json!([
        {"order": "A-1", "total": "42"},
        {"order": "A-2", "total": "7"},
    ])
}

/// The handler the function runs: for each record it is handed it reads the object the event
/// named, parses the CSV it holds and writes the rows back as JSON under [`CONVERTED_PREFIX`] —
/// through the AWS SDK its runtime supplies, so the write is a real Amazon S3 call made with the
/// function's own execution-role credentials.
///
/// A record's key "is URL encoded" (Amazon S3 User Guide, *Event message structure*), so it is
/// decoded the way that guide's own handler decodes it before being used as a key again. The
/// handler accepts a `.CSV` key as readily as a `.csv` one, which is what the suppressed rows
/// below rest on.
const CONVERTING_HANDLER: &str = r#"
import csv
import io
import json
import urllib.parse

import boto3

CONVERTED_PREFIX = "converted/"


def handler(event, context):
    client = boto3.client("s3")
    converted = []
    for record in event["Records"]:
        bucket = record["s3"]["bucket"]["name"]
        key = urllib.parse.unquote_plus(record["s3"]["object"]["key"])
        if not key.lower().endswith(".csv"):
            continue
        body = client.get_object(Bucket=bucket, Key=key)["Body"].read().decode("utf-8")
        rows = list(csv.DictReader(io.StringIO(body)))
        target = CONVERTED_PREFIX + key + ".json"
        client.put_object(
            Bucket=bucket,
            Key=target,
            Body=json.dumps(rows).encode("utf-8"),
        )
        converted.append(target)
    return {"converted": converted}
"#;

/// One row: the key written, and whether every rule of the configuration's filter admits it.
struct Row {
    case: &'static str,
    key: &'static str,
    admitted: bool,
}

/// The rows the started binary carries: the admitted key, and one per way of failing a rule only
/// by the CASE of the characters that rule's value spells — the dimension the whole table in
/// process ([`crate::s3_notification_key_filter_is_literal`]) spans, kept here to the two rows
/// that need a running emulator to mean anything.
const ROWS: &[Row] = &[
    Row {
        case: "a key both rules admit",
        key: "raw/orders.csv",
        admitted: true,
    },
    Row {
        case: "the suffix spelled in upper case",
        key: "raw/upper.CSV",
        admitted: false,
    },
    Row {
        case: "the prefix spelled in upper case",
        key: "RAW/cased.csv",
        admitted: false,
    },
];

/// Where the handler writes the conversion of `key`: one target per key, so no row's absence can
/// be satisfied by another row's delivery.
fn converted_key(key: &str) -> String {
    format!("{CONVERTED_PREFIX}{key}.json")
}

/// One AWS Lambda call over its `restJson1` wire.
fn lambda(emulator: &Started, method: &str, path: &str, body: Option<&Value>) -> (u16, String) {
    let payload = body.map(Value::to_string).unwrap_or_default();
    emulator.call(
        "lambda",
        method,
        path,
        &[("content-type", "application/json")],
        payload.into_bytes(),
    )
}

/// The stack that brings up the handler: the execution role the handler's own Amazon S3 calls are
/// made as, and the function itself, whose inline source CloudFormation archives into its
/// deployment package. The grant Amazon S3 needs and the bucket's configuration are NOT declared
/// here — they are what the leg below states through each service's own API, the way a client
/// wires a function it has already deployed.
fn handler_stack() -> String {
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
            "Converter": {
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
            "FunctionArn": { "Value": { "Fn::GetAtt": ["Converter", "Arn"] } },
            "FunctionName": { "Value": { "Ref": "Converter" } },
        },
    })
    .to_string()
}

/// RULE: an armed Lambda notification runs its handler for the key its key-name filter admits,
/// and for no key that differs from a rule's value only in case — on `emulator`, the binary a
/// client starts.
///
/// Called by [`crate::cfn_deployed_pipeline_on_a_started_emulator`] on the emulator it started,
/// so this leg pays no second start.
pub(crate) fn an_armed_notification_converts_only_what_its_literal_filter_admits(
    emulator: &Started,
) {
    let case = "the armed notification's key-name filter on a started emulator";

    let template = handler_stack();
    let outputs = deployed(emulator, case, STACK, &template);
    let function_arn = output(&outputs, case, "FunctionArn");
    let function_name = output(&outputs, case, "FunctionName");

    // An invocation needs a function in `Active`, which the clock owes: waited for by LOOKING
    // rather than by sleeping a span of this leg's own choosing.
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    let mut state = String::new();
    while Instant::now() < giving_up_at {
        let (status, body) = lambda(
            emulator,
            "GET",
            &format!("{FUNCTIONS_URI}/{function_name}/configuration"),
            None,
        );
        assert_eq!(
            status, 200,
            "case {case:?}: GetFunctionConfiguration must succeed, got {status} with {body}"
        );
        state = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|reported| reported["State"].as_str().map(str::to_string))
            .unwrap_or_default();
        if state == FUNCTION_ACTIVE {
            break;
        }
        std::thread::sleep(LOOK_INTERVAL);
    }
    assert_eq!(
        state, FUNCTION_ACTIVE,
        "case {case:?}: the function never reached {FUNCTION_ACTIVE}, last reported {state:?}; \
         the log:\n{}",
        emulator.log()
    );

    // The grant Amazon S3 reads before it records the configuration: "in the case of Lambda
    // destinations, Amazon S3 verifies that the Lambda function permissions grant Amazon S3
    // permission to invoke the function from the Amazon S3 bucket"
    // (`PutBucketNotificationConfiguration`'s own documentation).
    let (status, body) = lambda(
        emulator,
        "POST",
        &format!("{FUNCTIONS_URI}/{function_name}/policy"),
        Some(&json!({
            "StatementId": "allow-this-bucket",
            "Action": INVOKE_ACTION,
            "Principal": S3_PRINCIPAL,
            "SourceArn": format!("arn:aws:s3:::{BUCKET}"),
        })),
    );
    assert_eq!(
        status, 201,
        "case {case:?}: AddPermission must succeed, got {status} with {body}"
    );

    let (status, body) = emulator.s3("PUT", &format!("/{BUCKET}"), "");
    assert_eq!(
        status, 200,
        "case {case:?}: CreateBucket must succeed, got {status} with {body}"
    );

    // The configuration: the Lambda list is bound as `CloudFunctionConfiguration`, whose ARN
    // member is bound as `CloudFunction`
    // (`smithy_query(query="shape",shape="NotificationConfiguration")`).
    let configuration = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\"><CloudFunctionConfiguration>\
         <Id>convert-the-uploaded-csv</Id><CloudFunction>{function_arn}</CloudFunction>\
         <Event>{OBJECT_CREATED}</Event><Filter><S3Key>\
         <FilterRule><Name>prefix</Name><Value>{FILTER_PREFIX}</Value></FilterRule>\
         <FilterRule><Name>suffix</Name><Value>{FILTER_SUFFIX}</Value></FilterRule>\
         </S3Key></Filter></CloudFunctionConfiguration></NotificationConfiguration>"
    );
    let (status, body) = emulator.s3("PUT", &format!("/{BUCKET}?notification"), &configuration);
    assert_eq!(
        status, 200,
        "case {case:?}: PutBucketNotificationConfiguration must succeed, got {status} with \
         {body}; the log:\n{}",
        emulator.log()
    );

    // The suppressed keys FIRST, then the admitted one: the conversion of the admitted key is
    // the barrier that proves the pipeline ran, so each absence read after it is an absence
    // rather than lateness.
    for row in ROWS
        .iter()
        .filter(|row| !row.admitted)
        .chain(ROWS.iter().filter(|row| row.admitted))
    {
        let (status, body) = emulator.s3("PUT", &format!("/{BUCKET}/{}", row.key), SOURCE_CSV);
        assert_eq!(
            status, 200,
            "case {:?}: PutObject({:?}) must succeed, got {status} with {body}",
            row.case, row.key
        );
    }

    // The barrier: the handler ran on the admitted key's event and wrote what it converted, with
    // no further API call from this leg than the write itself.
    for row in ROWS.iter().filter(|row| row.admitted) {
        let target = converted_key(row.key);
        let giving_up_at = Instant::now() + SETTLE_DEADLINE;
        let served = loop {
            let (status, body) = emulator.s3("GET", &format!("/{BUCKET}/{target}"), "");
            if status == 200 {
                break body;
            }
            assert!(
                Instant::now() < giving_up_at,
                "case {:?}: the armed notification never converted {:?}: nothing was written to \
                 {target:?} (the last read answered {status} with {body}); {} ; the emulator's \
                 log:\n{}",
                row.case,
                row.key,
                emulator.logged(&function_name),
                emulator.log()
            );
            std::thread::sleep(LOOK_INTERVAL);
        };
        let document: Value = serde_json::from_str(&served).unwrap_or_else(|error| {
            panic!(
                "case {:?}: expected the handler's converted document, got {error} for \
                 {served:?}",
                row.case
            )
        });
        assert_eq!(
            document,
            converted_document(),
            "case {:?}: expected the CSV converted row for row, got {document}",
            row.case
        );
    }

    // And every key the filter excludes reached the function not at all, however long this keeps
    // looking — read against a pipeline the row above has shown live and shown to have drained.
    let giving_up_at = Instant::now() + QUIET_WINDOW;
    while Instant::now() < giving_up_at {
        for row in ROWS.iter().filter(|row| !row.admitted) {
            let target = converted_key(row.key);
            let (status, body) = emulator.s3("GET", &format!("/{BUCKET}/{target}"), "");
            assert_eq!(
                status, 404,
                "case {:?}: {:?} passes no rule of the filter, so nothing may be written to \
                 {target:?}; the read answered {status} with {body}",
                row.case, row.key
            );
        }
        std::thread::sleep(LOOK_INTERVAL);
    }
}
