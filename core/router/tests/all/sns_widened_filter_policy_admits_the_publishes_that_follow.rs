// SPDX-License-Identifier: CC0-1.0

//! What a subscription whose `FilterPolicy` is WIDENED after a publish was withheld receives
//! next — asserted over the production service set (`aws_emulator_router::build_router` on one
//! engine, exactly as the binary assembles it) and read where a consumer reads it: on the
//! subscribed Amazon SQS queue.
//!
//! RULE under test: **a subscription's filter policy decides ONE message's delivery at the
//! instant that message is published — "FilterPolicy – The simple JSON object that lets your
//! subscriber receive only a subset of messages, rather than receiving every message published
//! to the topic" (`smithy_query(query="doc",shape="SubscribeInput",member="Attributes")`),
//! applied by the `Publish` that "delivers the message to each endpoint that is subscribed to
//! the topic" — so the policy IN FORCE at each publish governs that publish alone: a later
//! write of the attribute governs every publish after it and releases nothing that an earlier
//! policy withheld, redelivers nothing already delivered, and leaves the subscribed queue's
//! reported depth at zero for every message withheld from it.**
//!
//! The rows differ along what the rule generalises over: the property the policy judges, how
//! the policy is widened (another value added to the list, or the list replaced by an operator
//! condition), the whitespace the policy document is written with, whether the filtering scope
//! is named explicitly or left to its default, whether the subscription asked for raw delivery,
//! whether the `Subscribe` asked for the subscription ARN, and which published value the
//! widening admits.
//!
//! What this suite adds to each service's own cover is the CROSSING on the assembled emulator,
//! and three things only the crossing shows: the unfiltered TWIN subscription that tells a
//! withheld delivery apart from a publish nobody received, the DEPTH Amazon SQS reports for the
//! queue a policy withheld from, and the LONG POLL a client observes a delivery through
//! (`WaitTimeSeconds`). The relaxation forms themselves — cleared by an empty value, by no
//! value and by an empty policy, and every documented operator, under both filtering scopes —
//! are spanned by the SNS crate's own `tests/all/filter_policy_relaxation.rs`, which this suite
//! deliberately does not restate.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{decoded_entries, element, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The service principal Amazon SNS delivers to a subscribed queue as, which is the identity a
/// queue policy has to grant for a notification to reach the queue at all.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// The queue depth members Amazon SQS reports, each of which must stay at zero for a queue
/// whose subscription the policy withheld the message from
/// (`smithy_query(query="doc",shape="GetQueueAttributesRequest",member="AttributeNames")`).
const DEPTH_ATTRIBUTES: &[&str] = &[
    "ApproximateNumberOfMessages",
    "ApproximateNumberOfMessagesNotVisible",
    "ApproximateNumberOfMessagesDelayed",
];

/// How long a delivery is looked for before the queue is reported to hold none: far longer
/// than an in-process fan-out takes, so a loaded host changes no verdict.
const DELIVERY_BUDGET: Duration = Duration::from_secs(60);

/// How long each poll above waits before looking again.
const LOOK_INTERVAL: Duration = Duration::from_millis(20);

/// The wait one LONG-POLLING receive asks for, in seconds: "the duration (in seconds) for which
/// the call waits for a message to arrive in the queue before returning"
/// (`smithy_query(query="doc",shape="ReceiveMessageRequest",member="WaitTimeSeconds")`) — the
/// wait a client makes for a delivery it cannot otherwise be told about. One second is enough to
/// be a wait rather than a short poll.
const WAIT_TIME_SECONDS: u32 = 1;

/// The whole emulator over one engine, as the binary assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature itself is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One Amazon SNS awsQuery call (API version 2010-03-31), with whatever status it answered.
async fn sns_call(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(String, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-03-31");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sns.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("sns"))
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Amazon SNS call that must answer 200, as its XML body.
async fn sns(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(String, String)],
) -> String {
    let (status, text) = sns_call(router, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SNS {action}, got {status} with {text}"
    );
    text
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// The queue policy AWS documents for a queue subscribed to an SNS topic: the service
/// principal, allowed `sqs:SendMessage` on that queue for messages of that topic alone.
fn grant_to_sns(queue_arn: &str, topic_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowSNSDelivery",
            "Effect": "Allow",
            "Principal": {"Service": SNS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
            "Condition": {"ArnEquals": {"aws:SourceArn": topic_arn}},
        }],
    })
    .to_string()
}

/// A queue called `name` that the topic `topic_arn` may deliver to, as `(url, arn)`.
async fn given_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    topic_arn: &str,
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
            "Attributes": { "Policy": grant_to_sns(&arn, topic_arn) },
        }),
    )
    .await;
    (url, arn)
}

/// Subscribe the queue `queue_arn` to `topic`, with `attributes` as the subscription's own, and
/// answer the subscription's ARN.
///
/// `ask_for_the_arn` asks for `ReturnSubscriptionArn`, which a client may leave out: a
/// subscription whose endpoint needs no confirmation is answered with its ARN either way, and
/// that is the handle every later call about it carries.
async fn subscribe(
    router: &MultiServiceRouter,
    case: &str,
    topic: &str,
    queue_arn: &str,
    attributes: &[(&str, &str)],
    ask_for_the_arn: bool,
) -> String {
    let mut members: Vec<(String, String)> = vec![
        ("TopicArn".to_string(), topic.to_string()),
        ("Protocol".to_string(), "sqs".to_string()),
        ("Endpoint".to_string(), queue_arn.to_string()),
    ];
    if ask_for_the_arn {
        members.push(("ReturnSubscriptionArn".to_string(), "true".to_string()));
    }
    for (at, (name, value)) in attributes.iter().enumerate() {
        let entry = at + 1;
        members.push((format!("Attributes.entry.{entry}.key"), (*name).to_string()));
        members.push((
            format!("Attributes.entry.{entry}.value"),
            (*value).to_string(),
        ));
    }
    element(&sns(router, case, "Subscribe", &members).await, "SubscriptionArn")
}

/// Publish `body` to `topic` carrying the one `String` message attribute `(name, value)`.
async fn publish(
    router: &MultiServiceRouter,
    case: &str,
    topic: &str,
    body: &str,
    attribute: (&str, &str),
) {
    let members: Vec<(String, String)> = vec![
        ("TopicArn".to_string(), topic.to_string()),
        ("Message".to_string(), body.to_string()),
        (
            "MessageAttributes.entry.1.Name".to_string(),
            attribute.0.to_string(),
        ),
        (
            "MessageAttributes.entry.1.Value.DataType".to_string(),
            "String".to_string(),
        ),
        (
            "MessageAttributes.entry.1.Value.StringValue".to_string(),
            attribute.1.to_string(),
        ),
    ];
    sns(router, case, "Publish", &members).await;
}

/// Everything waiting on the queue at `url` right now, as the published messages the
/// notifications carry, taking each one off the queue.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let mut carried: Vec<String> = Vec::new();
    loop {
        let received = sqs(
            router,
            case,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        let messages = received["Messages"].as_array().cloned().unwrap_or_default();
        if messages.is_empty() {
            return carried;
        }
        for message in messages {
            let body = message["Body"].as_str().unwrap_or_default().to_string();
            carried.push(published_message(&body));
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({
                    "QueueUrl": url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            )
            .await;
        }
    }
}

/// What the queue at `url` holds once something has reached it: looked for again until it holds
/// one, or until [`DELIVERY_BUDGET`] has passed — the CONDITION is waited for, not a span.
async fn delivered(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let deadline = Instant::now() + DELIVERY_BUDGET;
    loop {
        let held = drain(router, case, url).await;
        if !held.is_empty() {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a delivery on the subscribed queue, got none"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The published message the notification `body` carries, or the body itself when the
/// subscription asked for raw delivery.
fn published_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|envelope| {
            envelope
                .get("Message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.to_string())
}

/// Assert that every depth Amazon SQS reports for the queue at `url` is zero.
async fn assert_empty_depth(router: &MultiServiceRouter, case: &str, url: &str) {
    let reported = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": DEPTH_ATTRIBUTES }),
    )
    .await;
    for name in DEPTH_ATTRIBUTES {
        // The member has to be THERE: a reader that omits it must fail here rather than read
        // as a queue holding nothing.
        let depth = reported["Attributes"][name].as_str().unwrap_or_else(|| {
            panic!("case {case:?}: expected Amazon SQS to report {name}, got {reported}")
        });
        assert_eq!(
            depth, "0",
            "case {case:?}: expected {name} to be 0 on the queue the policy withheld the \
             message from, got {depth:?} in {reported}"
        );
    }
}

/// The `FilterPolicy` the subscription reports right now, or `None` when it reports none.
async fn reported_filter_policy(
    router: &MultiServiceRouter,
    case: &str,
    subscription: &str,
) -> Option<String> {
    let body = sns(
        router,
        case,
        "GetSubscriptionAttributes",
        &[("SubscriptionArn".to_string(), subscription.to_string())],
    )
    .await;
    decoded_entries(&body).remove("FilterPolicy")
}

/// One row: the property a policy judges, the narrow policy, the widening written afterwards,
/// and the values published under each.
struct Row {
    case: &'static str,
    /// The message attribute the policy judges.
    property: &'static str,
    /// The value the narrow policy admits.
    admitted: &'static str,
    /// The value the narrow policy withholds and the widened one admits.
    withheld: &'static str,
    /// The narrow policy.
    narrow: &'static str,
    /// The widened policy, or `None` to clear the attribute outright.
    widened: Option<&'static str>,
    /// The `FilterPolicyScope` the subscription is given, or `None` to leave it to the
    /// default: "MessageAttributes (default) – The filter is applied on the message
    /// attributes" (`smithy_query(query="doc",shape="SubscribeInput",member="Attributes")`).
    scope: Option<&'static str>,
    /// Whether the filtered subscription asks for raw delivery.
    raw: bool,
    /// Whether the `Subscribe` asks for `ReturnSubscriptionArn`.
    ask_for_the_arn: bool,
}

/// The rows: each widens the policy a different way, over a property and values of its own.
const ROWS: &[Row] = &[
    Row {
        case: "another value added to the list",
        property: "kind",
        admitted: "wanted",
        withheld: "unwanted",
        narrow: r#"{"kind":["wanted"]}"#,
        widened: Some(r#"{"kind":["wanted","unwanted"]}"#),
        scope: None,
        raw: false,
        ask_for_the_arn: true,
    },
    Row {
        case: "a policy written with the spacing a JSON encoder leaves, under the scope named \
               explicitly, subscribed without asking for the ARN",
        property: "kind",
        admitted: "wanted",
        withheld: "unwanted",
        narrow: r#"{"kind": ["wanted"]}"#,
        widened: Some(r#"{"kind": ["wanted", "unwanted"]}"#),
        scope: Some("MessageAttributes"),
        raw: false,
        ask_for_the_arn: false,
    },
    Row {
        case: "the list replaced by a prefix condition, raw delivery",
        property: "region",
        admitted: "eu-west-1",
        withheld: "eu-north-1",
        narrow: r#"{"region":["eu-west-1"]}"#,
        widened: Some(r#"{"region":[{"prefix":"eu-"}]}"#),
        scope: None,
        raw: true,
        ask_for_the_arn: true,
    },
];

/// Every row: the narrow policy admits one publish and withholds another, the widening governs
/// the publishes after it, and neither the withheld nor the delivered message ever arrives
/// again.
#[tokio::test]
async fn a_widened_filter_policy_governs_the_publishes_after_it_and_releases_no_backlog() {
    let router = router();
    for (at, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let name = format!("widened-{at}");
        let topic = element(
            &sns(
                &router,
                case,
                "CreateTopic",
                &[("Name".to_string(), name.clone())],
            )
            .await,
            "TopicArn",
        );
        let (filtered_url, filtered_arn) =
            given_queue(&router, case, &format!("{name}-filtered"), &topic).await;
        let (twin_url, twin_arn) =
            given_queue(&router, case, &format!("{name}-twin"), &topic).await;

        let raw = if row.raw { "true" } else { "false" };
        let mut attributes: Vec<(&str, &str)> =
            vec![("FilterPolicy", row.narrow), ("RawMessageDelivery", raw)];
        if let Some(scope) = row.scope {
            attributes.push(("FilterPolicyScope", scope));
        }
        let filtered = subscribe(
            &router,
            case,
            &topic,
            &filtered_arn,
            &attributes,
            row.ask_for_the_arn,
        )
        .await;
        let twin = subscribe(
            &router,
            case,
            &topic,
            &twin_arn,
            &[("RawMessageDelivery", "false")],
            row.ask_for_the_arn,
        )
        .await;
        assert_eq!(
            reported_filter_policy(&router, case, &filtered).await.as_deref(),
            Some(row.narrow),
            "case {case:?}: the subscription must report the policy it was subscribed with"
        );
        assert_eq!(
            reported_filter_policy(&router, case, &twin).await,
            None,
            "case {case:?}: the twin was given no filter policy, so it reports none"
        );

        // 1. The publish the narrow policy admits reaches both the filtered subscription and
        //    the unfiltered twin.
        let warm = format!("{name}-warm");
        publish(&router, case, &topic, &warm, (row.property, row.admitted)).await;
        assert_eq!(
            delivered(&router, case, &filtered_url).await,
            vec![warm.clone()],
            "case {case:?}: the narrow policy must admit that publish"
        );
        assert_eq!(
            delivered(&router, case, &twin_url).await,
            vec![warm.clone()],
            "case {case:?}: the unfiltered twin receives every publish"
        );

        // 2. The publish the narrow policy withholds reaches the twin alone, and leaves the
        //    filtered queue at depth zero.
        let dropped = format!("{name}-dropped");
        publish(
            &router,
            case,
            &topic,
            &dropped,
            (row.property, row.withheld),
        )
        .await;
        assert_eq!(
            delivered(&router, case, &twin_url).await,
            vec![dropped.clone()],
            "case {case:?}: the unfiltered twin receives the withheld publish too"
        );
        let withheld = drain(&router, case, &filtered_url).await;
        assert!(
            withheld.is_empty(),
            "case {case:?}: the narrow policy must withhold that publish, got {withheld:?}"
        );
        assert_empty_depth(&router, case, &filtered_url).await;

        // 3. The widening is written, is reported, and releases nothing.
        let mut write: Vec<(String, String)> = vec![
            ("SubscriptionArn".to_string(), filtered.clone()),
            ("AttributeName".to_string(), "FilterPolicy".to_string()),
        ];
        if let Some(widened) = row.widened {
            write.push(("AttributeValue".to_string(), widened.to_string()));
        }
        sns(&router, case, "SetSubscriptionAttributes", &write).await;
        assert_eq!(
            reported_filter_policy(&router, case, &filtered).await.as_deref(),
            row.widened,
            "case {case:?}: GetSubscriptionAttributes must report the widened policy"
        );
        let released = drain(&router, case, &filtered_url).await;
        assert!(
            released.is_empty(),
            "case {case:?}: widening the policy must release no backlog, got {released:?}"
        );

        // 4. A fresh publish of the class the widening admits IS delivered, and is the only
        //    delivery: neither the withheld nor the already-delivered message arrives again.
        let fresh = format!("{name}-fresh");
        publish(&router, case, &topic, &fresh, (row.property, row.withheld)).await;
        assert_eq!(
            delivered(&router, case, &filtered_url).await,
            vec![fresh.clone()],
            "case {case:?}: the widened policy must admit the publishes that follow it"
        );
        let after = drain(&router, case, &filtered_url).await;
        assert!(
            after.is_empty(),
            "case {case:?}: nothing already published may be delivered again, got {after:?}"
        );
        // The twin took that publish as well, which is what the filtered subscription's
        // withheld one is told apart from: a publish nobody received at all.
        assert_eq!(
            delivered(&router, case, &twin_url).await,
            vec![fresh.clone()],
            "case {case:?}: the unfiltered twin receives the publish after the widening too"
        );
        assert_empty_depth(&router, case, &filtered_url).await;
        assert_empty_depth(&router, case, &twin_url).await;
    }
}

/// The same rule observed the way a client observes a delivery it cannot otherwise be told
/// about: through a LONG-POLLING receive, the call the recorded scenario is driven by.
///
/// A publish the policy in force admits is handed over by a long poll (its `Messages` carry the
/// notification), and a publish it withholds leaves that same long poll answering with no
/// `Messages` member at all — the empty success "The parameter is applied to the messages that
/// Amazon SQS returns in the response" leaves, with the queue reporting no depth either. Then
/// the widening is written and the next publish of the withheld class is handed over by the same
/// long poll.
#[tokio::test]
async fn a_long_poll_hands_over_what_the_policy_admits_and_answers_empty_for_what_it_withholds() {
    let case = "observed through a long-polling receive";
    let router = router();
    let topic = element(
        &sns(
            &router,
            case,
            "CreateTopic",
            &[("Name".to_string(), "widened-long-poll".to_string())],
        )
        .await,
        "TopicArn",
    );
    let (url, queue_arn) = given_queue(&router, case, "widened-long-poll-queue", &topic).await;
    let subscription = subscribe(
        &router,
        case,
        &topic,
        &queue_arn,
        &[
            ("FilterPolicy", r#"{"kind":["wanted"]}"#),
            ("RawMessageDelivery", "false"),
        ],
        true,
    )
    .await;

    // What the policy admits: the long poll hands the notification over.
    publish(&router, case, &topic, "long-poll-admitted", ("kind", "wanted")).await;
    assert_eq!(
        long_polled(&router, case, &url).await,
        vec!["long-poll-admitted".to_string()],
        "case {case:?}: a long poll must hand over the publish the policy admits"
    );

    // What it withholds: the same long poll answers with nothing, and the queue reports no
    // depth — the publish was dropped at the publish, not parked anywhere.
    publish(&router, case, &topic, "long-poll-withheld", ("kind", "unwanted")).await;
    let withheld = long_polled(&router, case, &url).await;
    assert!(
        withheld.is_empty(),
        "case {case:?}: a long poll must answer with no messages for a withheld publish, got \
         {withheld:?}"
    );
    assert_empty_depth(&router, case, &url).await;

    // And once the policy is widened, the next publish of that class is handed over by the
    // same call, while the one withheld before it never appears.
    sns(
        &router,
        case,
        "SetSubscriptionAttributes",
        &[
            ("SubscriptionArn".to_string(), subscription),
            ("AttributeName".to_string(), "FilterPolicy".to_string()),
            (
                "AttributeValue".to_string(),
                r#"{"kind":["wanted","unwanted"]}"#.to_string(),
            ),
        ],
    )
    .await;
    publish(&router, case, &topic, "long-poll-fresh", ("kind", "unwanted")).await;
    assert_eq!(
        long_polled(&router, case, &url).await,
        vec!["long-poll-fresh".to_string()],
        "case {case:?}: a long poll must hand over the publish the widened policy admits"
    );
    let again = long_polled(&router, case, &url).await;
    assert!(
        again.is_empty(),
        "case {case:?}: nothing already published may be handed over again, got {again:?}"
    );
    assert_empty_depth(&router, case, &url).await;
}

/// The same observation from the other side: a long poll that is ALREADY WAITING when the
/// publish happens.
///
/// The receive is in flight before the publish is made, so what it answers with is a delivery
/// it waited for rather than one that was already on the queue when it was called — which is
/// how a consumer that polls a queue continuously sees a fan-out. The policy in force still
/// decides it: the publish it admits is handed to the waiting receive, the publish it withholds
/// leaves that receive answering with nothing once the wait it asked for has passed, and after
/// the widening the next publish of that class is handed to a waiting receive again.
#[tokio::test]
async fn a_long_poll_already_waiting_is_handed_the_publish_that_arrives_during_it() {
    let case = "a long poll already waiting when the publish arrives";
    let router = router();
    let topic = element(
        &sns(
            &router,
            case,
            "CreateTopic",
            &[("Name".to_string(), "widened-long-poll-waiting".to_string())],
        )
        .await,
        "TopicArn",
    );
    let (url, queue_arn) =
        given_queue(&router, case, "widened-long-poll-waiting-queue", &topic).await;
    let subscription = subscribe(
        &router,
        case,
        &topic,
        &queue_arn,
        &[
            ("FilterPolicy", r#"{"kind":["wanted"]}"#),
            ("RawMessageDelivery", "false"),
        ],
        true,
    )
    .await;

    // The receive is started first; the publish is made while it is waiting.
    let admitted = "long-poll-arrived";
    let (handed, ()) = tokio::join!(
        long_polled(&router, case, &url),
        publish(&router, case, &topic, admitted, ("kind", "wanted")),
    );
    assert_eq!(
        handed,
        vec![admitted.to_string()],
        "case {case:?}: a waiting long poll must be handed the publish the policy admits"
    );

    // A publish the policy withholds reaches no waiting receive either: the wait it asked for
    // passes and the answer carries no messages.
    let (withheld, ()) = tokio::join!(
        long_polled(&router, case, &url),
        publish(
            &router,
            case,
            &topic,
            "long-poll-arrived-withheld",
            ("kind", "unwanted")
        ),
    );
    assert!(
        withheld.is_empty(),
        "case {case:?}: a waiting long poll must be handed nothing for a withheld publish, got \
         {withheld:?}"
    );
    assert_empty_depth(&router, case, &url).await;

    // And once the policy is widened, a publish of that class made during the wait is handed
    // over by the receive that was waiting for it.
    sns(
        &router,
        case,
        "SetSubscriptionAttributes",
        &[
            ("SubscriptionArn".to_string(), subscription),
            ("AttributeName".to_string(), "FilterPolicy".to_string()),
            (
                "AttributeValue".to_string(),
                r#"{"kind":["wanted","unwanted"]}"#.to_string(),
            ),
        ],
    )
    .await;
    let fresh = "long-poll-arrived-fresh";
    let (handed, ()) = tokio::join!(
        long_polled(&router, case, &url),
        publish(&router, case, &topic, fresh, ("kind", "unwanted")),
    );
    assert_eq!(
        handed,
        vec![fresh.to_string()],
        "case {case:?}: a waiting long poll must be handed the publish the widened policy \
         admits"
    );
    assert_empty_depth(&router, case, &url).await;
}

/// Whatever ONE long-polling receive hands over for the queue at `url`, as the published
/// messages the notifications carry, taking each one off the queue.
///
/// The wait is the caller's own [`WAIT_TIME_SECONDS`]: a receive that finds a message hands it
/// over, and one that finds none answers with no `Messages` member once that wait has passed.
async fn long_polled(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "WaitTimeSeconds": WAIT_TIME_SECONDS,
        }),
    )
    .await;
    let mut carried: Vec<String> = Vec::new();
    for message in received["Messages"].as_array().cloned().unwrap_or_default() {
        carried.push(published_message(
            message["Body"].as_str().unwrap_or_default(),
        ));
        sqs(
            router,
            case,
            "DeleteMessage",
            &json!({
                "QueueUrl": url,
                "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
            }),
        )
        .await;
    }
    carried
}
