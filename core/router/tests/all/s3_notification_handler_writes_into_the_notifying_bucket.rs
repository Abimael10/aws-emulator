// SPDX-License-Identifier: CC0-1.0

//! A bucket's Lambda notification destination whose handler writes BACK INTO THE SAME BUCKET:
//! the delivery of the object event really runs the function, so the object the handler itself
//! writes is in the bucket a client reads — and the write the handler makes is itself an object
//! change the same configuration decides, so it reaches the function again only while every
//! rule of the configuration's key-name filter admits its key.
//!
//! The rule under test: **once a function destination's own resource policy lets the Amazon S3
//! service principal invoke it, every object change the bucket's `LambdaFunctionConfiguration`
//! admits — each one whose key satisfies every rule of its key-name `Filter` — is published to
//! that function as one invocation carrying the `Records` envelope (`eventSource` `aws:s3`,
//! `eventName` the class of the change, `s3.bucket.name` and `s3.object.key` naming what
//! changed), so whatever the handler performs on that envelope lands and is readable through
//! the ordinary object API; an object change the configuration does not admit reaches no
//! function at all, whichever call wrote it.**
//!
//! Four crates have to agree for that to hold (CloudFormation provisions the function, its
//! execution role and the invoke grant; Lambda holds the grant, answers for it and runs the
//! handler; IAM answers for the role the handler's own SDK call is signed as; S3 evaluates the
//! configuration, makes the invocation and serves both the written object and the handler's),
//! so the suite lives here. What only this suite reaches is the destination bucket being the
//! SOURCE bucket: the handler's own `PutObject` is an object change of the very bucket that
//! notified it, so the chain carries on into the function a second time, and each later hop is
//! admitted by the same configuration as the first. Its siblings reach the other sides —
//! [`crate::s3_notification_invokes_its_lambda_target`] pins a delivery whose handler writes
//! into a bucket of its own, and
//! [`crate::s3_notification_lambda_invoke_grant_gates_delivery`] pins what the grant on the
//! destination decides.
//!
//! The rows differ along what the rule generalises over: which rule kinds the configuration's
//! filter carries (none, prefix, suffix), the shape of the written key (at the root of the
//! bucket, under a path), how many objects one row writes, and — the part only a
//! self-addressed handler reaches — whether the key the HANDLER writes is itself one the
//! filter admits. Each row names every marker its wiring must leave, so a delivery that never
//! happened and one the filter excluded are told apart, and a hop the filter should have
//! stopped is a marker the row did not name.
//!
//! Every request below is dispatched through a BOXED future ([`Box::pin`]). The assembled
//! router's dispatch future carries the whole emulator's handler state inline, so a test
//! holding several of them across await points lays that state out on the test thread's stack
//! and overflows it as the emulator grows; the box puts each one on the heap instead.

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{elements, outputs_of};
// One harness for every suite that drives this chain over the assembled router, so none of
// them can drift from the way a client reaches this emulator.
use crate::s3_notification_invokes_its_lambda_target::{
    build_router, create_bucket, driven, query, s3,
};

/// The XML namespace a client's S3 payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The `StackStatus` the one stack this suite deploys has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The service principal Amazon S3 publishes a bucket's notifications under, and the action it
/// performs on a function destination ("for Amazon Web Services services, the principal is a
/// domain-style identifier that the service defines, such as s3.amazonaws.com",
/// `AddPermission$Principal`).
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The event class every configuration below is recorded for: "you can enable notifications
/// for [...] `s3:ObjectCreated:*`" — every way an object is created.
const CONFIGURED_EVENT: &str = "s3:ObjectCreated:*";

/// The `eventName` one `PutObject` of an object is published under, and the `eventSource` every
/// record Amazon S3 publishes carries.
const PUT_EVENT_NAME: &str = "ObjectCreated:Put";
const EVENT_SOURCE: &str = "aws:s3";

/// Where the handler keys what it writes for a record naming an ordinary object, and where it
/// keys what it writes for a record naming one of its OWN earlier writes. Two prefixes rather
/// than one, so the hop a configuration admits a second time is told from the first by the key
/// alone, and a run-away chain would leave a key under neither.
const MARKER_PREFIX: &str = "triggered/";
const ECHO_PREFIX: &str = "echoed/";

/// The body every row writes into its bucket. Immaterial to the rule: what a row observes is
/// the record the handler was handed, not the bytes.
const OBJECT_BODY: &str = "the object the notification is published for";

/// How long this suite waits for work the clock owes — a stack's resources coming up, and the
/// asynchronous delivery of an object event — and how often it looks while waiting. Generous:
/// no row is timing the emulator, every row is waiting for a condition.
const WAIT: Duration = Duration::from_secs(90);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// How long a row KEEPS LOOKING after its named markers have all arrived, to conclude that
/// NOTHING else is coming: the chain of self-addressed deliveries has stopped where the
/// configuration stops admitting its keys. Well past the span one delivery takes to leave a
/// marker.
const QUIET_WINDOW: Duration = Duration::from_secs(5);

/// The handler every row deploys: for each record it is handed it writes ONE object back into
/// the bucket the record names — the bucket that notified it — carrying what the event said.
/// A record naming a key the handler itself wrote under [`MARKER_PREFIX`] is written under
/// [`ECHO_PREFIX`] instead, and a record naming an [`ECHO_PREFIX`] key is the end of the chain,
/// so what a row observes is bounded by the handler rather than by how long it waits.
const SELF_ADDRESSED_HANDLER: &str = r#"
import json
import os

import boto3

MARKER_PREFIX = os.environ["MARKER_PREFIX"]
ECHO_PREFIX = os.environ["ECHO_PREFIX"]


def handler(event, context):
    s3 = boto3.client("s3")
    written = []
    for record in event["Records"]:
        bucket = record["s3"]["bucket"]["name"]
        key = record["s3"]["object"]["key"]
        if key.startswith(ECHO_PREFIX):
            continue
        prefix = ECHO_PREFIX if key.startswith(MARKER_PREFIX) else MARKER_PREFIX
        document = {
            "eventSource": record["eventSource"],
            "eventName": record["eventName"],
            "bucket": bucket,
            "key": key,
        }
        s3.put_object(
            Bucket=bucket,
            Key=prefix + key.replace("/", "_"),
            Body=json.dumps(document).encode("utf-8"),
        )
        written.append(key)
    return {"written": written}
"#;

/// `PUT /<bucket>/<key>` — `PutObject`.
async fn put_object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) {
    let answer = s3(router, "PUT", &format!("/{bucket}/{key}"), OBJECT_BODY).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: PutObject({bucket:?}, {key:?}) must succeed, got {} with {}",
        answer.status, answer.body
    );
}

/// One key-name `FilterRule`, as the request's XML carries it.
struct Rule {
    name: &'static str,
    value: &'static str,
}

/// `PUT /<bucket>?notification` with ONE Lambda configuration naming `function_arn` for
/// [`CONFIGURED_EVENT`], filtered by `rules`.
///
/// The elements are the ones S3's own model binds the Lambda list to
/// (`smithy_query(query="shape",shape="NotificationConfiguration")`:
/// `LambdaFunctionConfigurations` is bound as `CloudFunctionConfiguration`, whose ARN member is
/// bound as `CloudFunction`).
async fn put_lambda_notification(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    function_arn: &str,
    rules: &[Rule],
) {
    let filter = if rules.is_empty() {
        String::new()
    } else {
        let written: String = rules
            .iter()
            .map(|rule| {
                format!(
                    "<FilterRule><Name>{}</Name><Value>{}</Value></FilterRule>",
                    rule.name, rule.value
                )
            })
            .collect();
        format!("<Filter><S3Key>{written}</S3Key></Filter>")
    };
    let body = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\"><CloudFunctionConfiguration>\
         <Id>self-addressed</Id><CloudFunction>{function_arn}</CloudFunction>\
         <Event>{CONFIGURED_EVENT}</Event>{filter}\
         </CloudFunctionConfiguration></NotificationConfiguration>"
    );
    let answer = s3(router, "PUT", &format!("/{bucket}?notification"), &body).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: PutBucketNotificationConfiguration({bucket:?}) must succeed while the \
         function grants the invocation, got {} with {}",
        answer.status, answer.body
    );
    // The configuration is readable back, so a row that sees no delivery cannot be a row whose
    // configuration was never recorded.
    let read = s3(router, "GET", &format!("/{bucket}?notification"), "").await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: GetBucketNotificationConfiguration({bucket:?}) must succeed, got {} \
         with {}",
        read.status, read.body
    );
    assert!(
        read.body.contains(function_arn),
        "case {case:?}: the recorded configuration must report {function_arn:?}, got {}",
        read.body
    );
}

/// Every key `bucket` holds that the handler wrote — the two prefixes it keys its own writes
/// under — read the way a client observes an invocation it cannot see: `ListObjectsV2` with the
/// prefix.
async fn written_keys(router: &MultiServiceRouter, case: &str, bucket: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for prefix in [MARKER_PREFIX, ECHO_PREFIX] {
        let answer = s3(
            router,
            "GET",
            &format!("/{bucket}?list-type=2&prefix={prefix}"),
            "",
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: ListObjectsV2({bucket:?}, {prefix:?}) must succeed, got {} with {}",
            answer.status, answer.body
        );
        keys.extend(elements(&answer.body, "Key"));
    }
    keys
}

/// Look again until `bucket` holds exactly the keys `wanted`, or [`WAIT`] passes, and answer
/// what it held on the last look.
async fn awaited_keys(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    wanted: &BTreeSet<String>,
) -> BTreeSet<String> {
    let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
    let mut held = written_keys(router, case, bucket).await;
    for _ in 0..looks {
        if &held == wanted {
            break;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
        held = written_keys(router, case, bucket).await;
    }
    held
}

/// Keep looking for [`QUIET_WINDOW`], asserting at every look that `bucket` still holds exactly
/// `wanted`: the chain of self-addressed deliveries has stopped where the configuration stops
/// admitting the handler's own keys, rather than the row having looked at a moment that happened
/// to match.
async fn assert_settled(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    wanted: &BTreeSet<String>,
) {
    let looks = QUIET_WINDOW.as_millis() / LOOK_INTERVAL.as_millis();
    for _ in 0..looks {
        tokio::time::sleep(LOOK_INTERVAL).await;
        let held = written_keys(router, case, bucket).await;
        assert_eq!(
            &held, wanted,
            "case {case:?}: no object event the configuration does not admit may reach the \
             function, so the bucket must hold exactly {wanted:?} from then on, got {held:?}"
        );
    }
}

/// The document the handler left at `key`, with the record it was handed asserted: the event
/// Amazon S3 publishes for a put of `named` in `bucket`.
async fn assert_marker(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    key: &str,
    named: &str,
) {
    let answer = s3(router, "GET", &format!("/{bucket}/{key}"), "").await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: GetObject({bucket:?}, {key:?}) must succeed, got {} with {}",
        answer.status, answer.body
    );
    let marker: Value = serde_json::from_str(&answer.body).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: {key:?} must be the document the handler wrote ({error}), got {}",
            answer.body
        )
    });
    assert_eq!(
        marker["eventSource"].as_str(),
        Some(EVENT_SOURCE),
        "case {case:?}: expected the record's eventSource at {key:?}, got {marker}"
    );
    assert_eq!(
        marker["eventName"].as_str(),
        Some(PUT_EVENT_NAME),
        "case {case:?}: expected the eventName of a put at {key:?}, got {marker}"
    );
    assert_eq!(
        marker["bucket"].as_str(),
        Some(bucket),
        "case {case:?}: expected the record at {key:?} to name the notifying bucket, got {marker}"
    );
    assert_eq!(
        marker["key"].as_str(),
        Some(named),
        "case {case:?}: expected the record at {key:?} to name the changed object, got {marker}"
    );
}

/// The stack this suite deploys once: the execution role the handler's own calls are signed as,
/// the function, and the grant that lets the Amazon S3 service principal invoke it.
fn handler_stack() -> Value {
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
                    // The handler writes with temporary credentials for this role, so the role
                    // has to grant the write it makes.
                    "Policies": [{
                        "PolicyName": "write-the-marker",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": ["s3:PutObject", "logs:*"],
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
                    "Timeout": 30,
                    "Environment": { "Variables": {
                        "MARKER_PREFIX": MARKER_PREFIX,
                        "ECHO_PREFIX": ECHO_PREFIX,
                    } },
                    "Code": { "ZipFile": SELF_ADDRESSED_HANDLER },
                },
            },
            "InvokePermission": {
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "FunctionName": { "Fn::GetAtt": ["Handler", "Arn"] },
                    "Action": INVOKE_ACTION,
                    "Principal": S3_PRINCIPAL,
                },
            },
        },
        "Outputs": {
            "FunctionArn": { "Value": { "Fn::GetAtt": ["Handler", "Arn"] } },
        },
    })
}

/// Create `stack`, wait for [`CREATE_COMPLETE`] and answer the function ARN it outputs.
async fn deployed_function(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let (status, body) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &handler_stack().to_string()),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack({stack:?}) must be accepted, got {status} with {body}"
    );
    let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
    let mut reported = stack_status(router, stack).await;
    for _ in 0..looks {
        if reported.0 == CREATE_COMPLETE {
            break;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
        reported = stack_status(router, stack).await;
    }
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "case {case:?}: the stack must reach {CREATE_COMPLETE}, got {:?} with {}",
        reported.0, reported.1
    );
    outputs_of(&reported.1)
        .get("FunctionArn")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack must output the function's ARN"))
}

/// One row: the filter the bucket's configuration carries, the objects a client writes into it,
/// and every object the HANDLER must then have written — each named beside the key the record it
/// was handed named, so a hop that should not have happened is a key no pair names.
struct Row {
    case: &'static str,
    rules: &'static [Rule],
    writes: &'static [&'static str],
    markers: &'static [(&'static str, &'static str)],
}

const ROWS: &[Row] = &[
    Row {
        // No filter: the handler's own write is a change the configuration admits too, so the
        // chain carries on into the function once more and stops where the handler stops.
        case: "an unfiltered configuration whose handler writes back into the notifying bucket",
        rules: &[],
        writes: &["trigger.txt"],
        markers: &[
            ("triggered/trigger.txt", "trigger.txt"),
            ("echoed/triggered_trigger.txt", "triggered/trigger.txt"),
        ],
    },
    Row {
        // Two objects, one of them under a path: every admitted change is published, one
        // invocation per changed object.
        case: "two writes, one of them keyed under a path",
        rules: &[],
        writes: &["uploads/2026/report.json", "second.txt"],
        markers: &[
            (
                "triggered/uploads_2026_report.json",
                "uploads/2026/report.json",
            ),
            (
                "echoed/triggered_uploads_2026_report.json",
                "triggered/uploads_2026_report.json",
            ),
            ("triggered/second.txt", "second.txt"),
            ("echoed/triggered_second.txt", "triggered/second.txt"),
        ],
    },
    Row {
        // A prefix rule the handler's OWN key falls outside of: the first change is published
        // and the handler's write is not, so the chain stops at the filter.
        case: "a prefix filter the handler's own write falls outside of",
        rules: &[Rule {
            name: "prefix",
            value: "uploads/",
        }],
        writes: &["uploads/one.txt", "outside.txt"],
        markers: &[("triggered/uploads_one.txt", "uploads/one.txt")],
    },
    Row {
        // A suffix rule the handler's own key satisfies, so the same filter admits the second
        // hop as well — and excludes the client's other write entirely.
        case: "a suffix filter the handler's own write satisfies",
        rules: &[Rule {
            name: "suffix",
            value: ".json",
        }],
        writes: &["data.json", "data.csv"],
        markers: &[
            ("triggered/data.json", "data.json"),
            ("echoed/triggered_data.json", "triggered/data.json"),
        ],
    },
];

/// Every row: the configured bucket's own writes run the function, whose marker lands in that
/// same bucket, and nothing the configuration does not admit reaches the function at all.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn a_notification_whose_handler_writes_into_the_notifying_bucket_delivers() {
    driven("s3-notification-handler-writes-into-its-bucket", || {
        Box::pin(a_notification_whose_handler_writes_into_the_notifying_bucket_delivers_body())
    });
}

async fn a_notification_whose_handler_writes_into_the_notifying_bucket_delivers_body() {
    let router = build_router();
    let function_arn = deployed_function(
        &router,
        "the shared wiring",
        "self-addressed-handler-stack",
    )
    .await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let bucket = format!("self-addressed-{index}");
        create_bucket(&router, case, &bucket).await;
        put_lambda_notification(&router, case, &bucket, &function_arn, row.rules).await;

        for key in row.writes {
            put_object(&router, case, &bucket, key).await;
        }

        let wanted: BTreeSet<String> = row
            .markers
            .iter()
            .map(|(written, _)| (*written).to_string())
            .collect();
        let held = awaited_keys(&router, case, &bucket, &wanted).await;
        assert_eq!(
            held, wanted,
            "case {case:?}: the writes {:?} must leave exactly the handler's objects \
             {wanted:?}, got {held:?}",
            row.writes
        );

        // Nothing else is coming: the self-addressed chain has stopped.
        assert_settled(&router, case, &bucket, &wanted).await;

        for (written, named) in row.markers {
            assert_marker(&router, case, &bucket, written, named).await;
        }
    }
}
