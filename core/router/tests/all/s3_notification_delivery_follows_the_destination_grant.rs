// SPDX-License-Identifier: CC0-1.0

//! The DESTINATION's own resource policy as the gate on every notification Amazon S3
//! publishes — asked when the publication is made, not only when the configuration was
//! recorded.
//!
//! The rule under test: **a notification is a call Amazon S3 makes as its own service
//! principal acting for the publishing bucket (`sqs:SendMessage` on a queue, `sns:Publish`
//! on a topic, `lambda:InvokeFunction` on a function), so it happens only while the
//! destination's OWN resource policy allows that call for that bucket. The grant is
//! therefore decided at every publication: a statement withdrawn, pointed at another
//! action of the same service, or narrowed to another bucket after the configuration was
//! accepted stops the deliveries from then on — while the recorded configuration is
//! untouched and the object writes themselves still succeed — and putting the grant back
//! resumes them with no further call on the bucket.**
//!
//! Four crates have to agree for that to hold (S3 evaluates the configuration and makes the
//! publication, Amazon SQS and Amazon SNS hold the destination documents the decision is
//! made from, and Lambda holds the function's own), so the suite lives here: a crate-local
//! suite can only ever see one side of it. Its siblings reach the INSTALL-time half of the
//! same question — [`crate::cloudformation_sns_topic_policy_admits_s3_notifications`] and
//! [`crate::s3_notification_lambda_invoke_grant_gates_delivery`] pin the refusal of a
//! container whose destination grants S3 nothing — and
//! [`crate::s3_sns_lambda_chain_invoke_permission`] pins the same decision on the hop AFTER
//! this one (SNS into Lambda). What only this suite reaches is the grant MOVING under a
//! configuration that is already installed, which is what tells a destination that is not
//! permitted apart from a chain that delivers nothing at all.
//!
//! The rows differ along what the rule generalises over: which destination kind the
//! configuration names (a queue, a topic, a function), and which part of the grant stops
//! admitting the publication (the statement is withdrawn entirely, it names another action
//! of the destination's own service, or its `aws:SourceArn` names another bucket). Every row
//! asserts the delivery BEFORE the grant moves and again after it is put back, so each
//! absence is read against the same chain known to deliver.
//!
//! Every request is dispatched through a BOXED future, and the suite runs on a thread sized
//! like the emulator's own boot path: the chain this drives — S3's delivery into SQS, SNS and
//! Lambda, and a handler's own SDK call back into S3 — nests more future than a default test
//! thread's stack holds.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, elements, form_encode, outputs_of};

/// The Region every request here is signed for, and so the Region every destination of these
/// configurations lives in.
const REGION: &str = "us-east-1";

/// The XML namespace a client puts on an Amazon S3 request payload.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The service principal Amazon S3 publishes a bucket's notifications under — "for Amazon
/// Web Services services, the principal is a domain-style identifier that the service
/// defines, such as s3.amazonaws.com" (`AddPermission$Principal`).
const S3_PRINCIPAL: &str = "s3.amazonaws.com";

/// The action Amazon S3 performs on each destination kind to publish a notification to it,
/// and — beside each — ANOTHER action of that same service, which is the row where the grant
/// exists and does not name the publication.
const SEND_MESSAGE_ACTION: &str = "sqs:SendMessage";
const OTHER_QUEUE_ACTION: &str = "sqs:ReceiveMessage";
const PUBLISH_ACTION: &str = "sns:Publish";
const OTHER_TOPIC_ACTION: &str = "sns:Subscribe";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The condition key a grant narrowed to ONE publishing bucket names it by.
const SOURCE_ARN_KEY: &str = "aws:SourceArn";

/// The event class every configuration below is notified of, and the prefix its key-name
/// filter admits. The handler writes its own marker into a bucket of its own, so a
/// conversion never triggers another one.
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";
const WATCHED_PREFIX: &str = "incoming/";

/// The `Event` of the test notification a validated destination receives when the
/// configuration is installed, which is drained before a row writes anything.
const TEST_EVENT: &str = "s3:TestEvent";

/// The `StackStatus` the one stack this suite deploys has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `State` a function serves invocations in ("the function is ready to be invoked",
/// `FunctionConfiguration$State`).
const FUNCTION_ACTIVE: &str = "Active";

/// How long a row waits for work the clock owes — a stack's resources coming up, a function
/// reaching `Active`, and the asynchronous delivery of an object event — and how often it
/// looks while waiting. Deliberately generous: this suite shares its host with the rest of
/// the test target, and nothing here tests how fast that host is.
const DELIVERY_BUDGET: Duration = Duration::from_secs(240);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// How long a read that must see NOTHING keeps looking before concluding nothing came. Each
/// row has already seen the same chain deliver, so the window is read against a pipeline
/// known to be live.
const QUIET_WINDOW: Duration = Duration::from_secs(5);

/// The stack the emulator's own boot path gives the thread that assembles the router
/// (`core/router/src/main.rs`).
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// Run `body` to completion on a thread with [`BOOT_STACK_SIZE`], on the current-thread
/// runtime the deliveries are made on: a notification delivery is a task of its own, so it
/// runs on this same thread and needs the same depth.
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

/// The emulator as it is ASSEMBLED, on one engine: the publication S3 makes is dispatched
/// back into this same router, so the destination it reaches is the one this suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Amazon SNS call (API version 2010-03-31) that must be answered.
async fn sns(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, "sns", "2010-03-31", action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SNS {action}, got {status} with {body}"
    );
    body
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
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with \
         {parsed}"
    );
    parsed
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

/// `PUT /<bucket>` — `CreateBucket` in the Region every call here is signed for. The payload
/// states no location constraint: "buckets in Region us-east-1 have a LocationConstraint of
/// null" (`GetBucketLocationOutput$LocationConstraint`), so naming it is what S3 refuses.
async fn given_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let answer = s3(router, "PUT", &format!("/{bucket}"), "").await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: CreateBucket({bucket:?}) must succeed, got {} with {}",
        answer.status, answer.body
    );
}

/// A queue called `name`: its url and the ARN Amazon SQS reports for it.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
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
    (url, arn)
}

/// Write `policy` as the `Policy` attribute of the queue at `url`. An empty value is how
/// Amazon SQS clears an attribute, which is what withdrawing a queue policy writes.
async fn set_queue_policy(router: &MultiServiceRouter, case: &str, url: &str, policy: &str) {
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
}

/// Write `policy` as the topic's `Policy` attribute, which is "the JSON serialization of the
/// topic's access control policy" (`GetTopicAttributes`).
async fn set_topic_policy(router: &MultiServiceRouter, case: &str, arn: &str, policy: &str) {
    sns(
        router,
        case,
        "SetTopicAttributes",
        &[
            ("TopicArn", arn),
            ("AttributeName", "Policy"),
            ("AttributeValue", policy),
        ],
    )
    .await;
}

/// Write `document` as the policy of the destination `address` names, which is an attribute
/// of the destination itself in both services.
async fn apply_grant(
    router: &MultiServiceRouter,
    case: &str,
    kind: Kind,
    address: &str,
    document: &str,
) {
    match kind {
        Kind::Queue => set_queue_policy(router, case, address, document).await,
        Kind::Topic => set_topic_policy(router, case, address, document).await,
    }
}

/// One statement allowing the Amazon S3 service principal `action` on `resource` for the
/// notifications of `bucket`, which is the grant a notification destination's own policy has
/// to state for Amazon S3 to publish to it. `scoped` narrows it by `aws:SourceArn`, the key
/// such a grant names ONE publishing bucket by; an S3 bucket's ARN carries no Region and no
/// account, because a bucket name is globally unique.
fn grant(action: &str, resource: &str, bucket: &str, scoped: bool) -> String {
    let mut statement = json!({
        "Sid": "AllowS3Notifications",
        "Effect": "Allow",
        "Principal": { "Service": S3_PRINCIPAL },
        "Action": action,
        "Resource": resource,
    });
    if scoped {
        statement["Condition"] =
            json!({ "ArnLike": { SOURCE_ARN_KEY: format!("arn:aws:s3:::{bucket}") } });
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] }).to_string()
}

/// The policy the SINK queue of a topic row carries: the queue is where the topic's own
/// delivery is observed, so it admits that service rather than Amazon S3 — nothing in these
/// rows moves this grant.
fn sns_delivery_grant(resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowSnsDeliveries",
            "Effect": "Allow",
            "Principal": { "Service": "sns.amazonaws.com" },
            "Action": SEND_MESSAGE_ACTION,
            "Resource": resource,
        }],
    })
    .to_string()
}

/// `PUT /<bucket>?notification` with one configuration naming `destination` for
/// `s3:ObjectCreated:*`, filtered to [`WATCHED_PREFIX`]. The container's elements are the
/// ones S3's own model binds each destination list to
/// (`smithy_query(query="shape",shape="NotificationConfiguration")`:
/// `QueueConfigurations` -> `QueueConfiguration`/`Queue`, `TopicConfigurations` ->
/// `TopicConfiguration`/`Topic`, `LambdaFunctionConfigurations` ->
/// `CloudFunctionConfiguration`/`CloudFunction`).
async fn put_notification(
    router: &MultiServiceRouter,
    bucket: &str,
    destination: Destination<'_>,
) -> Answer {
    let (container, member, arn) = match destination {
        Destination::Queue(arn) => ("QueueConfiguration", "Queue", arn),
        Destination::Topic(arn) => ("TopicConfiguration", "Topic", arn),
        Destination::Function(arn) => ("CloudFunctionConfiguration", "CloudFunction", arn),
    };
    let body = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\"><{container}>\
         <Id>gated</Id><{member}>{arn}</{member}><Event>{OBJECT_CREATED}</Event>\
         <Filter><S3Key><FilterRule><Name>prefix</Name>\
         <Value>{WATCHED_PREFIX}</Value></FilterRule></S3Key></Filter>\
         </{container}></NotificationConfiguration>"
    );
    s3(router, "PUT", &format!("/{bucket}?notification"), &body).await
}

/// Which destination a configuration names, and the ARN it names it by.
#[derive(Debug, Clone, Copy)]
enum Destination<'a> {
    Queue(&'a str),
    Topic(&'a str),
    Function(&'a str),
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
        "case {case:?}: PutObject({bucket:?}, {key:?}) must succeed whatever the destination's \
         policy says, got {} with {}",
        answer.status, answer.body
    );
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

/// Look at the queue at `url` until one of its messages mentions `needle`, and answer every
/// message text seen while waiting. Bounded by [`DELIVERY_BUDGET`], which is the CONDITION's
/// deadline and not a measurement of it.
async fn awaited_mention(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    needle: &str,
) -> (bool, Vec<String>) {
    let deadline = Instant::now() + DELIVERY_BUDGET;
    let mut seen = Vec::new();
    loop {
        seen.extend(drained(router, case, url).await);
        if seen.iter().any(|text| text.contains(needle)) {
            return (true, seen);
        }
        if Instant::now() >= deadline {
            return (false, seen);
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Look at the queue at `url` for the whole of [`QUIET_WINDOW`] and answer every message text
/// that mentions `needle` — which must be none where the destination's policy does not admit
/// the publication.
async fn quietly_absent(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    needle: &str,
) -> Vec<String> {
    let deadline = Instant::now() + QUIET_WINDOW;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        seen.extend(drained(router, case, url).await);
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    seen.into_iter().filter(|text| text.contains(needle)).collect()
}

/// The destination ARN the bucket's stored configuration reports, whichever kind it names —
/// read so each row can assert that moving a GRANT changed nothing about the configuration.
async fn reported_destination(router: &MultiServiceRouter, case: &str, bucket: &str) -> String {
    let answer = s3(router, "GET", &format!("/{bucket}?notification"), "").await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: GetBucketNotificationConfiguration({bucket:?}) must answer 200, got \
         {} with {}",
        answer.status, answer.body
    );
    for tag in ["Queue", "Topic", "CloudFunction"] {
        if let Some(arn) = elements(&answer.body, tag).into_iter().next() {
            return arn;
        }
    }
    String::new()
}

/// WHICH destination kind a row's configuration names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// An Amazon SQS queue, where the publication is `sqs:SendMessage`.
    Queue,
    /// An Amazon SNS topic, where it is `sns:Publish` — observed on a queue subscribed to
    /// the topic.
    Topic,
}

/// HOW a row's destination stops admitting the publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Withdrawn {
    /// The statement is gone: the destination carries no policy at all.
    Cleared,
    /// The statement is there and names ANOTHER action of the destination's own service.
    OtherAction,
    /// It names the publishing action, narrowed by `aws:SourceArn` to another bucket.
    OtherBucket,
}

/// One row of the table.
struct Row {
    case: &'static str,
    kind: Kind,
    withdrawn: Withdrawn,
    /// What this row's own resources are named after, so no row inherits another's.
    slug: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a queue destination whose policy is cleared",
        kind: Kind::Queue,
        withdrawn: Withdrawn::Cleared,
        slug: "queue-cleared",
    },
    Row {
        case: "a queue destination whose grant is narrowed to another bucket",
        kind: Kind::Queue,
        withdrawn: Withdrawn::OtherBucket,
        slug: "queue-other-bucket",
    },
    Row {
        case: "a topic destination whose policy names another action of Amazon SNS",
        kind: Kind::Topic,
        withdrawn: Withdrawn::OtherAction,
        slug: "topic-other-action",
    },
];

#[test]
fn a_destinations_own_grant_decides_every_notification_published_to_it() {
    driven("s3-notification-destination-grant", || {
        Box::pin(async {
            // ONE emulator for the whole table: every row names its own bucket, queue and
            // topic after its own slug, so no row can observe another's, and assembling the
            // router once is what keeps the suite's cost to the work under test.
            let router = build_router();
            for row in ROWS {
                let case = row.case;
                let bucket = format!("s3ev-{}-bucket", row.slug);
                let elsewhere = format!("s3ev-{}-elsewhere", row.slug);
                given_bucket(&router, case, &bucket).await;

                // The destination, and the grant that admits this bucket's notifications on
                // it: a queue row publishes to the queue it reads, a topic row publishes to a
                // topic whose deliveries that queue is subscribed to.
                let (url, queue_arn) =
                    given_queue(&router, case, &format!("s3ev-{}-queue", row.slug)).await;
                let (admitting, destination_arn, action) = match row.kind {
                    Kind::Queue => (
                        grant(SEND_MESSAGE_ACTION, &queue_arn, &bucket, true),
                        queue_arn.clone(),
                        SEND_MESSAGE_ACTION,
                    ),
                    Kind::Topic => {
                        set_queue_policy(&router, case, &url, &sns_delivery_grant(&queue_arn))
                            .await;
                        let created = sns(
                            &router,
                            case,
                            "CreateTopic",
                            &[("Name", &format!("s3ev-{}-topic", row.slug))],
                        )
                        .await;
                        let topic_arn = element(&created, "TopicArn");
                        sns(
                            &router,
                            case,
                            "Subscribe",
                            &[
                                ("TopicArn", &topic_arn),
                                ("Protocol", "sqs"),
                                ("Endpoint", &queue_arn),
                                ("ReturnSubscriptionArn", "true"),
                            ],
                        )
                        .await;
                        (
                            grant(PUBLISH_ACTION, &topic_arn, &bucket, true),
                            topic_arn,
                            PUBLISH_ACTION,
                        )
                    }
                };
                let write_to = match row.kind {
                    // A queue's policy is an ATTRIBUTE of the queue, addressed by its url;
                    // a topic's is an attribute of the topic, addressed by its ARN.
                    Kind::Queue => url.clone(),
                    Kind::Topic => destination_arn.clone(),
                };
                apply_grant(&router, case, row.kind, &write_to, &admitting).await;

                // The configuration is accepted while the grant is in place, which is the
                // install-time half of the same question.
                let named = match row.kind {
                    Kind::Queue => Destination::Queue(&destination_arn),
                    Kind::Topic => Destination::Topic(&destination_arn),
                };
                let accepted = put_notification(&router, &bucket, named).await;
                assert_eq!(
                    accepted.status, 200,
                    "case {case:?}: the configuration must be accepted while the destination \
                     grants Amazon S3 the publication, got {} with {}",
                    accepted.status, accepted.body
                );

                // The test notification a validated destination receives is drained first, so
                // what each write below produces is read on its own.
                let (probed, seen) = awaited_mention(&router, case, &url, TEST_EVENT).await;
                assert!(
                    probed,
                    "case {case:?}: expected the validated destination to receive the \
                     {TEST_EVENT} probe, saw {seen:?}"
                );

                // GRANTED: the write the filter admits reaches the destination.
                let first = format!("{WATCHED_PREFIX}first.txt");
                put_object(&router, case, &bucket, &first).await;
                let (delivered, seen) = awaited_mention(&router, case, &url, "first.txt").await;
                assert!(
                    delivered,
                    "case {case:?}: expected the object event of {first:?} to be published to \
                     the destination its configuration names, saw {seen:?}"
                );

                // The grant MOVES, and nothing about the configuration does.
                let withheld = match row.withdrawn {
                    Withdrawn::Cleared => String::new(),
                    Withdrawn::OtherAction => grant(
                        match row.kind {
                            Kind::Queue => OTHER_QUEUE_ACTION,
                            Kind::Topic => OTHER_TOPIC_ACTION,
                        },
                        &destination_arn,
                        &bucket,
                        true,
                    ),
                    Withdrawn::OtherBucket => {
                        grant(action, &destination_arn, &elsewhere, true)
                    }
                };
                apply_grant(&router, case, row.kind, &write_to, &withheld).await;
                let second = format!("{WATCHED_PREFIX}second.txt");
                put_object(&router, case, &bucket, &second).await;
                let arrived = quietly_absent(&router, case, &url, "second.txt").await;
                assert!(
                    arrived.is_empty(),
                    "case {case:?}: the destination's own policy no longer admits Amazon S3 \
                     publishing this bucket's notifications, so the event of {second:?} must \
                     not be published to it, got {arrived:?}"
                );
                assert_eq!(
                    reported_destination(&router, case, &bucket).await,
                    destination_arn,
                    "case {case:?}: moving a destination's grant must leave the recorded \
                     configuration exactly as it was"
                );

                // And putting it back resumes the deliveries, with no further call on the
                // bucket.
                apply_grant(&router, case, row.kind, &write_to, &admitting).await;
                let third = format!("{WATCHED_PREFIX}third.txt");
                put_object(&router, case, &bucket, &third).await;
                let (resumed, seen) = awaited_mention(&router, case, &url, "third.txt").await;
                assert!(
                    resumed,
                    "case {case:?}: the restored grant must admit the publication again, so \
                     the event of {third:?} must reach the destination, saw {seen:?}"
                );
            }
        })
    });
}

/// The handler the function row deploys: for each record it is handed it prints the key the
/// event named, which is the evidence that the invocation really happened — read back out of
/// the function's own log group. It calls no SDK and imports nothing, so the row observes the
/// INVOCATION rather than a handler's own further call.
const PRINTING_HANDLER: &str = r#"
def handler(event, context):
    seen = []
    for record in event["Records"]:
        key = record["s3"]["object"]["key"]
        print("handled " + key)
        seen.append(key)
    return {"seen": seen}
"#;

/// The stack the function row deploys: the execution role and the function, with NO grant for
/// Amazon S3 — the row adds and withdraws that itself, through Lambda's own calls.
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
                    "Code": { "ZipFile": PRINTING_HANDLER },
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

/// Add one statement to the function's resource-based policy through `AddPermission`, which
/// answers 201 with the created statement.
async fn add_permission(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    statement_id: &str,
    source_bucket: &str,
) {
    let (status, answered) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        Some(json!({
            "StatementId": statement_id,
            "Action": INVOKE_ACTION,
            "Principal": S3_PRINCIPAL,
            "SourceArn": format!("arn:aws:s3:::{source_bucket}"),
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from Lambda AddPermission({statement_id:?}), got \
         {status} with {answered}"
    );
}

/// Withdraw that statement again through `RemovePermission`, whose answer is 204.
async fn remove_permission(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    statement_id: &str,
) {
    let (status, answered) = lambda(
        router,
        "DELETE",
        &format!("/2015-03-31/functions/{function}/policy/{statement_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 204,
        "case {case:?}: expected HTTP 204 from Lambda RemovePermission({statement_id:?}), got \
         {status} with {answered}"
    );
}

/// Everything the function has logged, as `FilterLogEvents` reports it for the function's own
/// log group — the probe a client polling for an invocation's evidence makes. A group nothing
/// has created yet is a refusal a caller polls through, so the status is not asserted.
async fn logged(router: &MultiServiceRouter, function: &str) -> Vec<String> {
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
        .map(|event| event["message"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Look at the function's log group until it holds a line mentioning `needle`; answer every
/// line seen.
async fn awaited_log_line(
    router: &MultiServiceRouter,
    function: &str,
    needle: &str,
) -> (bool, Vec<String>) {
    let deadline = Instant::now() + DELIVERY_BUDGET;
    loop {
        let lines = logged(router, function).await;
        if lines.iter().any(|line| line.contains(needle)) {
            return (true, lines);
        }
        if Instant::now() >= deadline {
            return (false, lines);
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Create the stack and wait for `CREATE_COMPLETE`, answering its outputs.
///
/// The wait is the test crate's one shared stack waiter
/// ([`wait_for_stack_status`]), which looks again until the status holds rather than sleeping a
/// span on CloudFormation's behalf; the outputs come from the answer it read the status from.
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
/// handover withheld later is the policy's refusal and not the lifecycle's.
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

#[test]
fn a_functions_own_invoke_grant_decides_every_notification_published_to_it() {
    driven("s3-notification-function-grant", || {
        Box::pin(async {
            let case = "a function destination whose invoke permission is withdrawn";
            let router = build_router();
            let bucket = "s3ev-function-grant-bucket";
            let statement = "s3-notifications";
            given_bucket(&router, case, bucket).await;

            let outputs = deployed(&router, case, "s3ev-function-grant", &handler_stack()).await;
            let function_arn = outputs
                .get("FunctionArn")
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: the stack must output the ARN"));
            let function_name = outputs
                .get("FunctionName")
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: the stack must output the name"));
            awaited_active(&router, case, &function_name).await;

            // GRANTED: the configuration is accepted and the write it admits runs the handler.
            add_permission(&router, case, &function_name, statement, bucket).await;
            let accepted =
                put_notification(&router, bucket, Destination::Function(&function_arn)).await;
            assert_eq!(
                accepted.status, 200,
                "case {case:?}: the configuration must be accepted while the function admits \
                 the invocation, got {} with {}",
                accepted.status, accepted.body
            );
            let first = format!("{WATCHED_PREFIX}first.txt");
            put_object(&router, case, bucket, &first).await;
            let (ran, lines) = awaited_log_line(&router, &function_name, "first.txt").await;
            assert!(
                ran,
                "case {case:?}: expected the granted function to be invoked for {first:?} and \
                 log the key it handled, got {lines:?}"
            );

            // WITHDRAWN: the invocation is the call that policy decides, so it does not happen.
            remove_permission(&router, case, &function_name, statement).await;
            let second = format!("{WATCHED_PREFIX}second.txt");
            put_object(&router, case, bucket, &second).await;
            let deadline = Instant::now() + QUIET_WINDOW;
            while Instant::now() < deadline {
                tokio::time::sleep(LOOK_INTERVAL).await;
            }
            let lines = logged(&router, &function_name).await;
            assert!(
                !lines.iter().any(|line| line.contains("second.txt")),
                "case {case:?}: the function's own policy no longer lets Amazon S3 invoke it \
                 for this bucket, so no invocation may be made for {second:?}, got {lines:?}"
            );
            assert_eq!(
                reported_destination(&router, case, bucket).await,
                function_arn,
                "case {case:?}: withdrawing the grant must leave the recorded configuration \
                 exactly as it was"
            );

            // And re-granted, the next write is published again.
            add_permission(&router, case, &function_name, statement, bucket).await;
            let third = format!("{WATCHED_PREFIX}third.txt");
            put_object(&router, case, bucket, &third).await;
            let (resumed, lines) = awaited_log_line(&router, &function_name, "third.txt").await;
            assert!(
                resumed,
                "case {case:?}: the restored grant must admit the invocation again, so the \
                 event of {third:?} must run the handler, got {lines:?}"
            );
        })
    });
}
