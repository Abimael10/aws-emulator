// SPDX-License-Identifier: CC0-1.0

//! WHICH Lambda target a bucket's notification configuration runs for one object write, when
//! the bucket carries SEVERAL configurations whose key-name filters differ — the prefix-scoped
//! fan-out an infrastructure-as-code client deploys on one bucket to feed several consumers.
//!
//! The rule under test: **a bucket's notification configuration is a LIST of independent
//! destination configurations, and an object event is published to every one whose own event
//! list names the event AND whose own key-name `Filter` admits the key — and to no other.** A
//! `Filter` is "A container for object key name filtering rules"
//! (`NotificationConfigurationFilter`) of ONE configuration, so one configuration's rules decide
//! nothing about another's: a key one admits and another excludes reaches the first function
//! only, and a key no configuration admits reaches none.
//!
//! Four crates have to agree for that to hold (CloudFormation installs the declared list on the
//! bucket it creates, S3 evaluates each configuration against the event it just produced and
//! makes the invocation, Lambda holds each grant and runs each handler, IAM answers for the
//! execution role the handlers' own SDK calls are signed as), so the suite lives here: no
//! crate-local suite can see more than one side of it. Its siblings reach one axis each —
//! [`crate::s3_notification_invokes_its_lambda_target`] pins what ONE configuration's filter
//! admits, and `s3_notification_event_classes_run_their_lambda_targets`'s
//! `an_event_reaches_exactly_the_destinations_that_name_it` pins the same isolation for a list a
//! client PUT through S3's own API — and what only this suite reaches is that list arriving
//! through CloudFormation's `AWS::S3::Bucket` `NotificationConfiguration`, over the key shapes a
//! prefix rule has to decide: which function ran, and which did not.
//!
//! Every row is observed the way a client observes it: each function's handler writes one
//! marker object of its own, into a bucket that carries no notification configuration at all,
//! under a key naming the function that wrote it and the object it was handed. The rows that
//! must see NOTHING come after rows whose writes demonstrably ran a function of this same
//! bucket, so a delivery path that never runs at all cannot satisfy them.
//!
//! The rows differ along what the rule generalises over: which configuration's prefix the
//! written key is under, how deep the key is, a key under neither prefix, and a key whose name
//! opens with a prefix's text as a mere substring rather than with that prefix.
//!
//! Every request below is dispatched through a BOXED future ([`Box::pin`]). The assembled
//! router's dispatch future carries the whole emulator's handler state inline, so a test
//! holding several of them across await points lays that state out on the test thread's stack
//! and overflows it as the emulator grows; the box puts each one on the heap instead. Nothing
//! about what is asserted changes.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode};
use crate::rds_docdb_neptune_signing_name_routing::{authorization, BOOT_STACK_SIZE};

/// The Region every request here is signed for, which is the Region the shared stack reader
/// ([`wait_for_stack_status`]) polls in and so the Region the stack's resources belong to.
const REGION: &str = "us-east-1";

/// The `StackStatus` the deployed stack must reach before the pipeline it declares is wired.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The service principal Amazon S3 publishes notifications under, and the action it performs on
/// a function destination.
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The event class every declared configuration names: "you can enable notifications for every
/// event of a type by using the wildcard", so this is every object-created event. The classes
/// themselves are a sibling suite's axis; here every configuration names the same one, which is
/// what leaves the key-name filter as the only thing deciding a delivery.
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";

/// The `eventName` a record of one `PutObject` carries, which each marker repeats back.
const PUT_EVENT_NAME: &str = "ObjectCreated:Put";

/// The prefix every marker key opens with, inside the bucket the handlers write to.
const MARKER_PREFIX: &str = "markers/";

/// The body every row writes, which the handler reads back off the object the event named.
const OBJECT_BODY: &str = "the object the event named";

/// How long this suite waits for work the clock owes — the stack's resources coming up and the
/// asynchronous delivery of an object event — and how often it looks while waiting.
///
/// A ceiling on PATIENCE, never a span anything here counts on: every wait looks again until
/// what it waits for is there or the deadline passes.
const WAIT: Duration = Duration::from_secs(120);
/// See [`WAIT`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// How long a row keeps looking for a marker that must NEVER appear before concluding none
/// came. It starts only once that row's own expected markers are there, so the deliveries this
/// write owed have demonstrably already been made.
const QUIET_WINDOW: Duration = Duration::from_secs(3);

/// Run `body` to completion on a thread with the shared [`BOOT_STACK_SIZE`] — the stack the
/// emulator's own boot path gives the thread that assembles the router — on the current-thread
/// runtime this suite's deliveries are made on: a notification delivery is a task of its own, so
/// it runs on this same thread and needs the same depth.
///
/// Assembling every service and then dispatching a CHAIN of in-process calls through it —
/// CloudFormation into S3, S3's delivery into Lambda, each handler's own SDK call back into S3 —
/// nests more future than the default stack of a test thread holds. Only the SIZE is shared: the
/// sibling suites that drive themselves this way each hold their own private runner.
fn driven<Body>(name: &'static str, body: Body)
where
    Body: FnOnce() -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = ()>>>
        + Send
        + 'static,
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

/// The handler every declared function runs: for each record it is handed it reads the object
/// the event named and writes one marker object naming ITSELF (the target the environment
/// states), the bucket and key the record named and the event it reported. A record's key "is
/// URL encoded" (Amazon S3 User Guide, *Event message structure*), so it is decoded the way that
/// guide's own handler decodes it before being used as a key again.
const MARKING_HANDLER: &str = r#"
import json
import os
import urllib.parse

import boto3


def handler(event, context):
    s3 = boto3.client("s3")
    target = os.environ["TARGET"]
    written = []
    for record in event["Records"]:
        bucket = record["s3"]["bucket"]["name"]
        key = urllib.parse.unquote_plus(record["s3"]["object"]["key"])
        body = s3.get_object(Bucket=bucket, Key=key)["Body"].read().decode("utf-8")
        marker = {
            "target": target,
            "bucket": bucket,
            "key": key,
            "eventName": record["eventName"],
            "body": body,
        }
        s3.put_object(
            Bucket=os.environ["MARKER_BUCKET"],
            Key="markers/" + target + "/" + key.replace("/", "_"),
            Body=json.dumps(marker).encode("utf-8"),
        )
        written.append(key)
    return {"target": target, "written": written}
"#;

/// The emulator as it is ASSEMBLED, on one engine: each handler's own SDK calls are dispatched
/// back into this same router, so the bucket a handler writes is the bucket this suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// One CloudFormation call over its awsQuery wire, answered with the HTTP status and the XML.
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

/// One response of S3's REST surface: the status and the body as text.
struct Answer {
    status: u16,
    body: String,
}

/// Dispatch one request against S3's REST surface, path-style.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> Answer {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .header("content-type", "application/octet-stream")
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// `PUT /<bucket>` — `CreateBucket`.
async fn create_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let answer = s3(router, "PUT", &format!("/{bucket}"), "").await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: CreateBucket({bucket:?}) must succeed, got {} with {}",
        answer.status, answer.body
    );
}

/// `PUT /<bucket>/<key>` — `PutObject`.
async fn put_object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str, body: &str) {
    let answer = s3(router, "PUT", &format!("/{bucket}/{key}"), body).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: PutObject({bucket:?}, {key:?}) must succeed, got {} with {}",
        answer.status, answer.body
    );
}

/// One target of the bucket's notification configuration: the function the declaration names,
/// and the key-name filter that configuration carries.
struct Target {
    /// What the function calls itself in the marker it writes, which is also the directory its
    /// markers land in.
    name: &'static str,
    /// The logical id prefix the template gives this target's resources.
    logical: &'static str,
    /// The `prefix` rule its own `Filter` carries.
    prefix: &'static str,
}

/// The two configurations one bucket carries, each scoped to a prefix of its own. Two
/// configurations of the same event class whose filters admit a common key are "ambiguously
/// defined" and refused outright, so the prefixes a bucket's list carries for one event class
/// are disjoint — which is exactly the fan-out this suite is about.
const TARGETS: &[Target] = &[
    Target {
        name: "alpha",
        logical: "Alpha",
        prefix: "alpha/",
    },
    Target {
        name: "beta",
        logical: "Beta",
        prefix: "beta/",
    },
];

/// The target of `name`, which every row names its expectations by.
fn target_of(name: &str) -> &'static Target {
    TARGETS
        .iter()
        .find(|target| target.name == name)
        .unwrap_or_else(|| panic!("the table names the target {name:?}"))
}

/// The marker key the target `target` writes for the object `key`.
fn marker_key(target: &str, key: &str) -> String {
    format!("{MARKER_PREFIX}{target}/{}", key.replace('/', "_"))
}

/// The execution role every declared function assumes: it is assumable by AWS Lambda, and it
/// grants the read and the write the handlers make with its temporary credentials.
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
            "Policies": [{
                "PolicyName": "mark-the-event",
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

/// The template the suite deploys: one function per target, the grant Amazon S3 needs on each,
/// and the bucket whose `NotificationConfiguration` names all three — each with its own
/// key-name filter.
fn template(bucket: &str, markers: &str) -> String {
    let mut resources = json!({ "HandlerRole": role_resource() });
    let mut configurations: Vec<Value> = Vec::new();
    let mut permissions: Vec<Value> = Vec::new();
    for target in TARGETS {
        let function = format!("{}Function", target.logical);
        let permission = format!("{}Permission", target.logical);
        resources[&function] = json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Role": { "Fn::GetAtt": ["HandlerRole", "Arn"] },
                "Timeout": 30,
                "Environment": { "Variables": {
                    "MARKER_BUCKET": markers,
                    "TARGET": target.name,
                } },
                "Code": { "ZipFile": MARKING_HANDLER },
            },
        });
        resources[&permission] = json!({
            "Type": "AWS::Lambda::Permission",
            "Properties": {
                "FunctionName": { "Fn::GetAtt": [function.clone(), "Arn"] },
                "Action": INVOKE_ACTION,
                "Principal": S3_PRINCIPAL,
                "SourceArn": format!("arn:aws:s3:::{bucket}"),
            },
        });
        permissions.push(json!(permission));
        // `AWS::S3::Bucket`'s `LambdaConfiguration` carries the event, the filter and the
        // function and nothing else: the configuration's own id is Amazon S3's to assign.
        configurations.push(json!({
            "Event": OBJECT_CREATED,
            "Function": { "Fn::GetAtt": [function, "Arn"] },
            "Filter": {
                "S3Key": { "Rules": [{ "Name": "prefix", "Value": target.prefix }] },
            },
        }));
    }
    // The configuration S3 records is validated against each function's own policy, so every
    // grant has to be in place before the bucket is created carrying the list.
    resources["SourceBucket"] = json!({
        "Type": "AWS::S3::Bucket",
        "DependsOn": permissions,
        "Properties": {
            "BucketName": bucket,
            "NotificationConfiguration": { "LambdaConfigurations": configurations },
        },
    });
    let mut outputs = json!({});
    for target in TARGETS {
        outputs[format!("{}Arn", target.logical)] = json!({
            "Value": { "Fn::GetAtt": [format!("{}Function", target.logical), "Arn"] },
        });
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": resources,
        "Outputs": outputs,
    })
    .to_string()
}

/// Create `stack`, wait for `CREATE_COMPLETE` and answer its outputs.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
) -> std::collections::BTreeMap<String, String> {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", template),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack({stack:?}) must be accepted, got {status} with {body}"
    );
    let (reached, described) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: the stack must reach {CREATE_COMPLETE}, got {described}"
    );
    crate::query_wire::outputs_of(&described)
}

/// The CONTROL-PLANE half: the bucket reports one Lambda configuration per declared target,
/// each naming its own function and carrying its own key-name filter.
async fn assert_installed(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    outputs: &std::collections::BTreeMap<String, String>,
) {
    let answer = s3(router, "GET", &format!("/{bucket}?notification"), "").await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: GetBucketNotificationConfiguration({bucket:?}) must succeed, got {} with \
         {}",
        answer.status, answer.body
    );
    // The ARN member of a Lambda configuration is bound as `CloudFunction`
    // (`smithy_query(query="shape",shape="NotificationConfiguration")`).
    let named: BTreeSet<String> = elements(&answer.body, "CloudFunction").into_iter().collect();
    let declared: BTreeSet<String> = TARGETS
        .iter()
        .map(|target| {
            outputs
                .get(&format!("{}Arn", target.logical))
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: the stack must output every target ARN"))
        })
        .collect();
    assert_eq!(
        named, declared,
        "case {case:?}: expected the bucket to name every declared function, got {}",
        answer.body
    );
    let scoped: BTreeSet<String> = elements(&answer.body, "Value").into_iter().collect();
    let expected: BTreeSet<String> = TARGETS
        .iter()
        .map(|target| target.prefix.to_string())
        .collect();
    assert_eq!(
        scoped, expected,
        "case {case:?}: expected the declared key-name prefixes, got {}",
        answer.body
    );
}

/// Wait until the target `target` has left its marker for `key`, and answer that marker.
async fn awaited_marker(
    router: &MultiServiceRouter,
    case: &str,
    markers: &str,
    target: &str,
    key: &str,
) -> Value {
    let path = format!("/{markers}/{}", marker_key(target, key));
    let giving_up_at = Instant::now() + WAIT;
    loop {
        let answer = s3(router, "GET", &path, "").await;
        if answer.status == 200 {
            return serde_json::from_str(&answer.body).unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: the marker {path:?} must be the document the handler wrote \
                     ({error}), got {}",
                    answer.body
                )
            });
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected the write of {key:?} to run the target {target:?} within \
             {WAIT:?}, got {} from {path:?} with {}",
            answer.status,
            answer.body
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Keep looking for the markers the row's write must leave NOWHERE, until [`QUIET_WINDOW`] is
/// spent: a target whose own filter excludes this key ran for nothing.
async fn assert_unrun(
    router: &MultiServiceRouter,
    case: &str,
    markers: &str,
    spared: &[&str],
    key: &str,
) {
    let until = Instant::now() + QUIET_WINDOW;
    while Instant::now() < until {
        for target in spared {
            let path = format!("/{markers}/{}", marker_key(target, key));
            let answer = s3(router, "GET", &path, "").await;
            assert_eq!(
                answer.status, 404,
                "case {case:?}: the filter of the target {target:?} excludes {key:?}, so it must \
                 not have run for it, got {} from {path:?} with {}",
                answer.status, answer.body
            );
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row: the key written, and the targets whose own filters admit it.
struct Row {
    case: &'static str,
    key: &'static str,
    run: &'static [&'static str],
}

/// The rows differ along which configuration's prefix the key is under, how deep the key is,
/// and whether a key that merely OPENS with a prefix's text is under that prefix. The rows that
/// must see nothing come last, after writes that demonstrably ran a function of this same
/// bucket.
const ROWS: &[Row] = &[
    Row {
        case: "a key under the first configuration's prefix",
        key: "alpha/one.txt",
        run: &["alpha"],
    },
    Row {
        case: "a key under the second configuration's prefix",
        key: "beta/two.txt",
        run: &["beta"],
    },
    Row {
        case: "a key several levels deep under the first configuration's prefix",
        key: "alpha/deep/nested/three.txt",
        run: &["alpha"],
    },
    Row {
        case: "a key under neither configuration's prefix",
        key: "shared/four.txt",
        run: &[],
    },
    Row {
        case: "a key opening with a prefix's text but not with the prefix itself",
        key: "alphabet/five.txt",
        run: &[],
    },
];

/// Every row: the write reaches each configuration whose own key-name filter admits the key,
/// and no other.
///
/// Driven on a sized thread; see [`driven`].
#[test]
fn each_prefix_scoped_configuration_runs_only_the_keys_its_own_filter_admits() {
    driven("s3-prefix-scoped-notification-targets", || {
        Box::pin(each_prefix_scoped_configuration_runs_only_the_keys_its_own_filter_admits_body())
    });
}

async fn each_prefix_scoped_configuration_runs_only_the_keys_its_own_filter_admits_body() {
    let router = build_router();
    let setup = "the bucket every row below writes into";
    // The markers land in a bucket of their own, which carries no notification configuration at
    // all, so a marker never produces an event of its own.
    let markers = "prefix-scoped-markers";
    create_bucket(&router, setup, markers).await;
    let bucket = "prefix-scoped-source";
    let outputs = deployed(
        &router,
        setup,
        "prefix-scoped-stack",
        &template(bucket, markers),
    )
    .await;
    assert_installed(&router, setup, bucket, &outputs).await;

    for row in ROWS {
        let case = row.case;
        put_object(&router, case, bucket, row.key, OBJECT_BODY).await;
        for target in row.run {
            let marker = awaited_marker(&router, case, markers, target, row.key).await;
            // (the member the marker carries, the value this row says it has to carry)
            let stated: &[(&str, Value)] = &[
                ("target", json!(target)),
                ("bucket", json!(bucket)),
                ("key", json!(row.key)),
                ("eventName", json!(PUT_EVENT_NAME)),
                ("body", json!(OBJECT_BODY)),
            ];
            for (member, expected) in stated {
                let reported = marker.get(member).cloned().unwrap_or(Value::Null);
                assert_eq!(
                    &reported, expected,
                    "case {case:?}: expected the marker of {target:?} to carry {member} \
                     {expected}, got {marker}"
                );
            }
        }
        let spared: Vec<&str> = TARGETS
            .iter()
            .map(|target| target.name)
            .filter(|name| !row.run.contains(name))
            .collect();
        assert_unrun(&router, case, markers, &spared, row.key).await;
        // The targets the row expects are exactly the ones whose own declared prefix admits the
        // key, read off the declaration rather than restated: the table and the template cannot
        // drift apart.
        for target in row.run {
            let prefix = target_of(target).prefix;
            assert!(
                row.key.starts_with(prefix),
                "case {case:?}: the table expects {target:?} to run for {:?}, but its declared \
                 prefix is {prefix:?}",
                row.key
            );
        }
        for target in &spared {
            let prefix = target_of(target).prefix;
            assert!(
                !row.key.starts_with(prefix),
                "case {case:?}: the table spares {target:?} for {:?}, but its declared prefix \
                 {prefix:?} admits that key",
                row.key
            );
        }
    }
}
