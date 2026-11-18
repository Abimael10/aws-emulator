// SPDX-License-Identifier: CC0-1.0

//! One bucket's notification configuration FANNING OUT across destination KINDS: a queue
//! configuration and a Lambda function configuration recorded together, each scoped by its
//! own key-name filter.
//!
//! The rule under test: **a bucket's notification configuration is evaluated as ONE set of
//! destination configurations — every entry of every destination list is judged on its own
//! `Events` and its own key-name `Filter`, independently of the other entries and of which
//! list it sits in — so an object write notifies each destination whose filter admits the
//! key, whatever kind that destination is, and no destination whose filter does not.**
//!
//! Two crates have to agree for that to hold (S3 evaluates the configuration and makes the
//! publications, Amazon SQS holds the queue the queue leg lands on and Lambda runs the
//! function the function leg invokes), so the suite lives here: a crate-local suite can only
//! ever see one leg. Its siblings pin the legs one at a time —
//! [`crate::s3_notification_invokes_its_lambda_target`] pins what ONE function
//! configuration's filter admits, [`crate::s3_prefix_scoped_notification_rules_run_their_own_function`]
//! pins several prefix-scoped configurations of the SAME kind, and
//! [`crate::s3_notification_delivery_follows_the_destination_grant`] pins the destination
//! grant each publication is decided by. What only this suite reaches is the MIXED container:
//! that recording a queue leg does not cost the function leg, that neither leg receives the
//! other's keys, and that the delivery does not depend on the order the container lists the
//! two kinds in.
//!
//! The rows differ along what the rule generalises over: which destination kind the matching
//! configuration names (a queue, a function), which rule KIND picks it (a `prefix` rule, and a
//! `prefix` shared with another configuration plus a `suffix` rule that separates them), and
//! whether any configuration matches at all. A final phase writes the same container with the
//! two destination lists in the opposite order, because list order is not part of the rule.
//!
//! Every request is dispatched through a BOXED future on a thread sized like the emulator's
//! own boot path: the chain this drives — S3's delivery into Amazon SQS and into Lambda, and
//! the handler Lambda then runs — nests more future than a default test thread's stack holds.
//!
//! The wire helpers (the SQS, Lambda and CloudWatch Logs calls, the queue and its policy, the
//! stack) are this suite's own, which is this target's convention for a suite's own fixtures;
//! what IS shared is reached from where it already lives — the S3 request harness
//! ([`crate::s3_notification_invokes_its_lambda_target`]), the one stack waiter
//! ([`wait_for_stack_status`]) and the query-wire readers ([`crate::query_wire`]).

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::outputs_of;
use crate::s3_notification_invokes_its_lambda_target::{
    authorization, build_router, create_bucket, driven, query, s3,
};

/// The Region every request here is signed for, and so the Region every destination of these
/// configurations lives in. The shared S3 harness signs for this same Region.
const REGION: &str = "us-east-1";

/// The XML namespace a client puts on an Amazon S3 request payload.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The event class every configuration below is notified of: "you can enable notifications
/// for every event of a type by using the wildcard" (`Event`).
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";

/// The service principal Amazon S3 publishes a bucket's notifications under — "for Amazon
/// Web Services services, the principal is a domain-style identifier that the service
/// defines, such as s3.amazonaws.com" (`AddPermission$Principal`).
const S3_PRINCIPAL: &str = "s3.amazonaws.com";

/// The action Amazon S3 performs to publish a notification to each destination kind.
const SEND_MESSAGE_ACTION: &str = "sqs:SendMessage";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The condition key a destination grant narrowed to ONE publishing bucket names it by.
const SOURCE_ARN_KEY: &str = "aws:SourceArn";

/// The `Event` of the test notification a validated queue destination receives when the
/// configuration is installed, which is drained before any object is written.
const TEST_EVENT: &str = "s3:TestEvent";

/// The `StackStatus` the one stack this suite deploys has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `State` a function serves invocations in ("the function is ready to be invoked",
/// `FunctionConfiguration$State`).
const FUNCTION_ACTIVE: &str = "Active";

/// What the handler prefixes every key it was handed with, so a log line that reports an
/// invocation is told apart from anything else the runtime logs.
const MARKER: &str = "S3_EVT::";

/// How long a phase waits for work the clock owes — the stack's resources coming up, the
/// function reaching `Active`, and the asynchronous delivery of an object event — and how
/// often it looks while waiting. Deliberately generous: this suite shares its host with the
/// rest of the test target, and nothing here tests how fast that host is.
const DELIVERY_BUDGET: Duration = Duration::from_secs(240);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// How long the reads that must see NOTHING MORE keep looking after every expected delivery
/// has arrived. By then each destination has been seen to take the keys its own filter
/// admits, so the window is read against a fan-out known to be live.
const QUIET_WINDOW: Duration = Duration::from_secs(5);

/// The key-name prefix each destination of the MIXED container is scoped to.
const QUEUE_PREFIX: &str = "sqs/";
const FUNCTION_PREFIX: &str = "lambda/";
const SHARED_PREFIX: &str = "shared/";

/// The `suffix` rules that separate the two configurations that share [`SHARED_PREFIX`]:
/// "overlapping prefixes and suffixes are not supported" (`FilterRule$Name`), and two
/// configurations of one event type whose prefixes overlap are kept apart by suffixes that do
/// not.
const QUEUE_SUFFIX: &str = ".queue";
const FUNCTION_SUFFIX: &str = ".fn";

/// The `Id` each configuration of the mixed container carries, which is the value a delivered
/// record reports as its `configurationId` ("the ID found in the bucket notification
/// configuration", Amazon S3 User Guide, *Event message structure*).
const QUEUE_PREFIX_ID: &str = "queue-by-prefix";
const FUNCTION_PREFIX_ID: &str = "function-by-prefix";
const QUEUE_SUFFIX_ID: &str = "queue-by-suffix";
const FUNCTION_SUFFIX_ID: &str = "function-by-suffix";

/// Which destination list a configuration of the container belongs to, and the ARN it names
/// its destination by.
#[derive(Debug, Clone, Copy)]
enum Destination<'a> {
    Queue(&'a str),
    Function(&'a str),
}

/// One destination configuration of the container, as the request's XML carries it.
#[derive(Debug, Clone, Copy)]
struct Configured<'a> {
    id: &'a str,
    destination: Destination<'a>,
    prefix: &'a str,
    suffix: Option<&'a str>,
}

impl Configured<'_> {
    /// The element this configuration is written as. The elements are the ones S3's own model
    /// binds each destination list to
    /// (`smithy_query(query="shape",shape="NotificationConfiguration")`:
    /// `QueueConfigurations` -> `QueueConfiguration`/`Queue`, `LambdaFunctionConfigurations`
    /// -> `CloudFunctionConfiguration`/`CloudFunction`).
    fn xml(&self) -> String {
        let (container, member, arn) = match self.destination {
            Destination::Queue(arn) => ("QueueConfiguration", "Queue", arn),
            Destination::Function(arn) => ("CloudFunctionConfiguration", "CloudFunction", arn),
        };
        let mut rules = format!(
            "<FilterRule><Name>prefix</Name><Value>{}</Value></FilterRule>",
            self.prefix
        );
        if let Some(suffix) = self.suffix {
            rules.push_str(&format!(
                "<FilterRule><Name>suffix</Name><Value>{suffix}</Value></FilterRule>"
            ));
        }
        format!(
            "<{container}><Id>{}</Id><{member}>{arn}</{member}><Event>{OBJECT_CREATED}</Event>\
             <Filter><S3Key>{rules}</S3Key></Filter></{container}>",
            self.id
        )
    }
}

/// `PUT /<bucket>?notification` with `configured` as the whole container, in the order given —
/// which is how a phase writes the two destination lists in either order.
async fn put_notification(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    configured: &[Configured<'_>],
) {
    let entries: String = configured.iter().map(Configured::xml).collect();
    let body = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\">{entries}\
         </NotificationConfiguration>"
    );
    let answer = s3(router, "PUT", &format!("/{bucket}?notification"), &body).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: PutBucketNotificationConfiguration({bucket:?}) must record a container \
         naming both a queue and a function, got {} with {}",
        answer.status, answer.body
    );
}

/// `PUT /<bucket>/<key>` — the object write whose event the configuration publishes.
async fn put_object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) {
    let answer = s3(
        router,
        "PUT",
        &format!("/{bucket}/{key}"),
        "the object the notification is published for",
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: PutObject({bucket:?}, {key:?}) must succeed, got {} with {}",
        answer.status, answer.body
    );
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must be answered.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// A queue called `name`, its policy already admitting the notifications of `bucket`: the url
/// and the ARN Amazon SQS reports for it.
async fn given_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    bucket: &str,
) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
        .to_string();
    let reported = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = reported["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn, got {reported}"))
        .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({
            "QueueUrl": url,
            "Attributes": { "Policy": grant(SEND_MESSAGE_ACTION, &arn, bucket) },
        }),
    )
    .await;
    (url, arn)
}

/// One statement allowing the Amazon S3 service principal `action` on `resource` for the
/// notifications of `bucket`, narrowed by `aws:SourceArn` — the key such a grant names ONE
/// publishing bucket by. An S3 bucket's ARN carries no Region and no account, because a
/// bucket name is globally unique.
fn grant(action: &str, resource: &str, bucket: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowS3Notifications",
            "Effect": "Allow",
            "Principal": { "Service": S3_PRINCIPAL },
            "Action": action,
            "Resource": resource,
            "Condition": { "ArnEquals": { SOURCE_ARN_KEY: format!("arn:aws:s3:::{bucket}") } },
        }],
    })
    .to_string()
}

/// Take every message waiting on the queue at `url` off it, as the text each body carries.
async fn drained(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let mut found = Vec::new();
    loop {
        let answered = sqs(
            router,
            case,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        let messages = answered["Messages"].as_array().cloned().unwrap_or_default();
        if messages.is_empty() {
            return found;
        }
        for message in messages {
            found.push(message["Body"].as_str().unwrap_or_default().to_string());
            if let Some(receipt) = message["ReceiptHandle"].as_str() {
                sqs(
                    router,
                    case,
                    "DeleteMessage",
                    &json!({ "QueueUrl": url, "ReceiptHandle": receipt }),
                )
                .await;
            }
        }
    }
}

/// One record a destination received, read off the `Records` envelope S3 published: which
/// configuration admitted it, which bucket it happened in, which event it was and which key
/// it named.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Record {
    configuration_id: String,
    bucket: String,
    event_name: String,
    key: String,
}

/// Every record the texts in `bodies` carry. A body that is not a `Records` envelope — the
/// `s3:TestEvent` probe a validated queue receives when the configuration is installed — has
/// none.
fn records_of(bodies: &[String]) -> Vec<Record> {
    bodies
        .iter()
        .filter_map(|text| serde_json::from_str::<Value>(text).ok())
        .flat_map(|parsed| parsed["Records"].as_array().cloned().unwrap_or_default())
        .map(|record| Record {
            configuration_id: record["s3"]["configurationId"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            bucket: record["s3"]["bucket"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            event_name: record["eventName"].as_str().unwrap_or_default().to_string(),
            key: record["s3"]["object"]["key"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        })
        .collect()
}

/// Drain the queue at `url` until the `s3:TestEvent` probe a validated queue destination
/// receives on install has been taken off it, and answer every body seen while waiting.
///
/// Bounded by [`DELIVERY_BUDGET`], which is the CONDITION's deadline and not a measurement of
/// it: the probe is what says the container was installed against a queue this suite reads,
/// and draining it is what lets each object write be observed on its own.
async fn awaited_test_event(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let deadline = Instant::now() + DELIVERY_BUDGET;
    let mut seen = Vec::new();
    loop {
        seen.extend(drained(router, case, url).await);
        if seen.iter().any(|text| text.contains(TEST_EVENT)) {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: installing the container must send the validated queue the \
             {TEST_EVENT} probe, saw {seen:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// One Lambda call over its REST surface, as its status and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(
            body.map(|body| body.to_string()).unwrap_or_default(),
        ))
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

/// The handler the function destination runs: for each record it is handed it prints the key
/// the event named, prefixed by [`MARKER`]. That line, read back out of the function's own log
/// group, is the evidence the invocation really happened. It calls no SDK and imports nothing,
/// so a phase observes the INVOCATION rather than a handler's own further call.
const MARKING_HANDLER: &str = r#"
def handler(event, context):
    seen = []
    for record in event["Records"]:
        key = record["s3"]["object"]["key"]
        print("S3_EVT::" + key)
        seen.append(key)
    return {"seen": seen}
"#;

/// The stack this suite deploys: the execution role and the function. The grants for Amazon
/// S3 are added through Lambda's own `AddPermission`, one per publishing bucket.
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
                        "PolicyName": "write-its-own-log-group",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": "logs:*",
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
                    "Code": { "ZipFile": MARKING_HANDLER },
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

/// Create the stack and wait for `CREATE_COMPLETE`, answering its outputs.
///
/// The wait is the test crate's one shared stack waiter ([`wait_for_stack_status`]), which
/// looks again until the status holds rather than sleeping a span on CloudFormation's behalf.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
) -> std::collections::BTreeMap<String, String> {
    let (status, body) = query(
        router,
        "cloudformation",
        "2010-05-15",
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
    let (reported, body) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        reported, CREATE_COMPLETE,
        "case {case:?}: the stack must reach {CREATE_COMPLETE}, last reported {reported:?} in \
         {body}"
    );
    outputs_of(&body)
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active`, so a
/// missing invocation later is the configuration's verdict and not the lifecycle's.
async fn awaited_active(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + DELIVERY_BUDGET;
    loop {
        let (_, reported) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        if reported["State"].as_str() == Some(FUNCTION_ACTIVE) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Lambda must report {name:?} {FUNCTION_ACTIVE}, got {reported}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Grant the Amazon S3 service principal the invocation of `function` for the notifications of
/// `bucket`, through `AddPermission`, which answers 201 with the created statement.
async fn add_permission(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    statement_id: &str,
    bucket: &str,
) {
    let (status, answered) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        Some(json!({
            "StatementId": statement_id,
            "Action": INVOKE_ACTION,
            "Principal": S3_PRINCIPAL,
            "SourceArn": format!("arn:aws:s3:::{bucket}"),
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from Lambda AddPermission({statement_id:?}), got \
         {status} with {answered}"
    );
}

/// Every key the function has reported handling, as `FilterLogEvents` reports its log group —
/// the probe a client polling for an invocation's evidence makes. A group nothing has created
/// yet is a refusal a caller polls through, so the status is not asserted.
async fn invoked_keys(router: &MultiServiceRouter, function: &str) -> Vec<String> {
    let group = format!("/aws/lambda/{function}");
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", "Logs_20140328.FilterLogEvents")
        .header("authorization", authorization("logs"))
        .body(Bytes::from(json!({ "logGroupName": group }).to_string()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    parsed["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|event| event["message"].as_str().map(str::to_string))
        .flat_map(|message| {
            message
                .split(MARKER)
                .skip(1)
                .map(|tail| tail.trim().to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// One row of the table: a key written into the mixed-container bucket, and which destination
/// of that container the rule says is notified of it.
struct Row {
    case: &'static str,
    key: &'static str,
    /// The `Id` of the configuration that admits the key on the queue leg, when one does.
    on_queue: Option<&'static str>,
    /// The `Id` of the configuration that admits the key on the function leg, when one does.
    at_function: Option<&'static str>,
}

/// The rows, differing along which destination kind the matching configuration names, which
/// rule kind picks it, and whether any configuration matches at all.
const ROWS: &[Row] = &[
    Row {
        case: "a key under the queue configuration's own prefix",
        key: "sqs/routed-to-queue.txt",
        on_queue: Some(QUEUE_PREFIX_ID),
        at_function: None,
    },
    Row {
        case: "a key under the function configuration's own prefix",
        key: "lambda/routed-to-function.txt",
        on_queue: None,
        at_function: Some(FUNCTION_PREFIX_ID),
    },
    Row {
        case: "a shared prefix separated by the queue configuration's suffix",
        key: "shared/nightly-report.queue",
        on_queue: Some(QUEUE_SUFFIX_ID),
        at_function: None,
    },
    Row {
        case: "a shared prefix separated by the function configuration's suffix",
        key: "shared/nightly-report.fn",
        on_queue: None,
        at_function: Some(FUNCTION_SUFFIX_ID),
    },
    Row {
        case: "a key no configuration of the container admits",
        key: "other/unmatched.txt",
        on_queue: None,
        at_function: None,
    },
];

/// The keys the rows expect on each leg.
fn expected(of: impl Fn(&Row) -> Option<&'static str>) -> BTreeSet<String> {
    ROWS.iter()
        .filter(|row| of(row).is_some())
        .map(|row| row.key.to_string())
        .collect()
}

/// What both destinations were seen to take, and whether the wait ran out first.
struct Observed {
    /// Every record the queue took, including one no configuration admits — which is what the
    /// absence half of the rule is read from.
    records: Vec<Record>,
    /// Every key the function reported handling, read the same way.
    handled: Vec<String>,
    /// Whether [`DELIVERY_BUDGET`] passed before every REQUIRED delivery had arrived, so a
    /// failing assertion can say the fan-out never settled rather than only naming the sets.
    timed_out: bool,
}

impl Observed {
    /// The keys the queue leg took, and the keys the function leg was invoked for.
    fn legs(&self) -> (BTreeSet<String>, BTreeSet<String>) {
        (
            self.records
                .iter()
                .map(|record| record.key.clone())
                .collect(),
            self.handled.iter().cloned().collect(),
        )
    }

    /// What to add to a failing assertion when the wait ran out, which tells a genuine
    /// timeout apart from a delivery that went to the wrong leg.
    fn note(&self) -> &'static str {
        if self.timed_out {
            " (the delivery budget passed before every required delivery arrived)"
        } else {
            ""
        }
    }
}

/// Look at both destinations until each holds every key the rows require of it, or
/// [`DELIVERY_BUDGET`] passes — the CONDITION's deadline, not a measurement of it.
async fn awaited_fan_out(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    function: &str,
    want_on_queue: &BTreeSet<String>,
    want_at_function: &BTreeSet<String>,
) -> Observed {
    let deadline = Instant::now() + DELIVERY_BUDGET;
    let mut records: Vec<Record> = Vec::new();
    loop {
        records.extend(records_of(&drained(router, case, url).await));
        let handled = invoked_keys(router, function).await;
        let queued: BTreeSet<String> = records.iter().map(|record| record.key.clone()).collect();
        let ran: BTreeSet<String> = handled.iter().cloned().collect();
        if want_on_queue.is_subset(&queued) && want_at_function.is_subset(&ran) {
            // The fan-out the rows require has arrived; keep looking for the whole quiet
            // window, so a delivery the rule FORBIDS has the same chance to show up as the
            // ones it requires already had.
            let quiet_until = Instant::now() + QUIET_WINDOW;
            while Instant::now() < quiet_until {
                tokio::time::sleep(LOOK_INTERVAL).await;
                records.extend(records_of(&drained(router, case, url).await));
            }
            return Observed {
                records,
                handled: invoked_keys(router, function).await,
                timed_out: false,
            };
        }
        if Instant::now() >= deadline {
            return Observed {
                records,
                handled,
                timed_out: true,
            };
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

#[test]
fn a_mixed_container_notifies_every_destination_its_own_filter_admits() {
    driven("s3-notification-mixed-fan-out", || {
        Box::pin(async {
            // ONE emulator for the whole table: the publication S3 makes is dispatched back
            // into this same router, so the destinations this reads are the ones it reached.
            let router = build_router();
            let case = "the mixed container";
            let bucket = "s3fan-mixed-bucket";
            create_bucket(&router, case, bucket).await;
            let (url, queue_arn) = given_queue(&router, case, "s3fan-mixed-queue", bucket).await;

            let outputs = deployed(&router, case, "s3fan-handler", &handler_stack()).await;
            let function_arn = outputs
                .get("FunctionArn")
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: the stack must output the ARN"));
            let function_name = outputs
                .get("FunctionName")
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: the stack must output the name"));
            awaited_active(&router, case, &function_name).await;
            add_permission(&router, case, &function_name, "s3-mixed", bucket).await;

            // ONE container carrying BOTH destination lists, each list scoped by its own
            // key-name filter.
            let container = [
                Configured {
                    id: QUEUE_PREFIX_ID,
                    destination: Destination::Queue(&queue_arn),
                    prefix: QUEUE_PREFIX,
                    suffix: None,
                },
                Configured {
                    id: QUEUE_SUFFIX_ID,
                    destination: Destination::Queue(&queue_arn),
                    prefix: SHARED_PREFIX,
                    suffix: Some(QUEUE_SUFFIX),
                },
                Configured {
                    id: FUNCTION_PREFIX_ID,
                    destination: Destination::Function(&function_arn),
                    prefix: FUNCTION_PREFIX,
                    suffix: None,
                },
                Configured {
                    id: FUNCTION_SUFFIX_ID,
                    destination: Destination::Function(&function_arn),
                    prefix: SHARED_PREFIX,
                    suffix: Some(FUNCTION_SUFFIX),
                },
            ];
            put_notification(&router, case, bucket, &container).await;

            // The test notification a validated queue destination receives is drained first,
            // so what the writes below produce is read on its own.
            awaited_test_event(&router, case, &url).await;

            // Every row's key is written before anything is read, so the two legs are observed
            // against the same set of writes.
            for row in ROWS {
                put_object(&router, row.case, bucket, row.key).await;
            }
            let want_on_queue = expected(|row| row.on_queue);
            let want_at_function = expected(|row| row.at_function);
            let observed = awaited_fan_out(
                &router,
                case,
                &url,
                &function_name,
                &want_on_queue,
                &want_at_function,
            )
            .await;

            // Each leg saw EXACTLY the keys its own filters admit: the required deliveries
            // arrived, and the forbidden ones — the other leg's keys, and the key no
            // configuration names — did not.
            let (queued, ran) = observed.legs();
            let note = observed.note();
            let records = &observed.records;
            assert_eq!(
                queued, want_on_queue,
                "case {case:?}: the queue leg must see exactly the keys its own \
                 configurations' filters admit{note}, got {queued:?} from {records:?}"
            );
            assert_eq!(
                ran,
                want_at_function,
                "case {case:?}: the function leg must be invoked for exactly the keys its own \
                 configurations' filters admit{note}, got {ran:?} from {:?}",
                observed.handled
            );

            // And every record the queue leg took names the configuration that admitted it,
            // the bucket the change happened in and the event it was.
            for row in ROWS {
                let Some(id) = row.on_queue else { continue };
                let taken: Vec<&Record> =
                    records.iter().filter(|record| record.key == row.key).collect();
                assert!(
                    !taken.is_empty(),
                    "case {:?}: expected a record for {:?}, got {records:?}",
                    row.case,
                    row.key
                );
                for record in taken {
                    assert_eq!(
                        record.configuration_id, id,
                        "case {:?}: the record must name the configuration that admitted it, \
                         got {record:?}",
                        row.case
                    );
                    assert_eq!(
                        record.bucket, bucket,
                        "case {:?}: the record must name the bucket the change happened in, \
                         got {record:?}",
                        row.case
                    );
                    assert_eq!(
                        record.event_name, "ObjectCreated:Put",
                        "case {:?}: the record must name the event the write was, got \
                         {record:?}",
                        row.case
                    );
                }
            }

            // ORDER: the same two destination kinds, written with the function list FIRST,
            // fan out the same way — list order is not part of the rule. A bucket and a queue
            // of its own, so neither leg can be read off the table's observations.
            let reversed = "the same container with the function list written first";
            let other_bucket = "s3fan-reversed-bucket";
            create_bucket(&router, reversed, other_bucket).await;
            let (other_url, other_queue_arn) =
                given_queue(&router, reversed, "s3fan-reversed-queue", other_bucket).await;
            add_permission(
                &router,
                reversed,
                &function_name,
                "s3-reversed",
                other_bucket,
            )
            .await;
            put_notification(
                &router,
                reversed,
                other_bucket,
                &[
                    Configured {
                        id: FUNCTION_PREFIX_ID,
                        destination: Destination::Function(&function_arn),
                        prefix: FUNCTION_PREFIX,
                        suffix: None,
                    },
                    Configured {
                        id: QUEUE_PREFIX_ID,
                        destination: Destination::Queue(&other_queue_arn),
                        prefix: QUEUE_PREFIX,
                        suffix: None,
                    },
                ],
            )
            .await;
            awaited_test_event(&router, reversed, &other_url).await;

            let queue_key = "sqs/reversed-to-queue.txt";
            let function_key = "lambda/reversed-to-function.txt";
            put_object(&router, reversed, other_bucket, queue_key).await;
            put_object(&router, reversed, other_bucket, function_key).await;
            let want_on_queue = BTreeSet::from([queue_key.to_string()]);
            let want_at_function = BTreeSet::from([function_key.to_string()]);
            let observed = awaited_fan_out(
                &router,
                reversed,
                &other_url,
                &function_name,
                &want_on_queue,
                &want_at_function,
            )
            .await;
            let (queued, _) = observed.legs();
            let note = observed.note();
            let records = &observed.records;
            assert_eq!(
                queued, want_on_queue,
                "case {reversed:?}: the queue leg must see exactly the key its own filter \
                 admits{note}, got {queued:?} from {records:?}"
            );
            // The function is the SAME one the table drove, so its log group still holds the
            // keys of that phase: this phase's own writes are the ones named after it.
            let from_other_bucket: BTreeSet<String> = observed
                .handled
                .iter()
                .filter(|key| key.contains("reversed"))
                .cloned()
                .collect();
            assert_eq!(
                from_other_bucket,
                want_at_function,
                "case {reversed:?}: the function leg must be invoked for exactly the key its \
                 own filter admits{note}, got {from_other_bucket:?} from {:?}",
                observed.handled
            );
        })
    });
}
