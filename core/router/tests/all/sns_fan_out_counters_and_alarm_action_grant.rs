// SPDX-License-Identifier: CC0-1.0

//! What a topic's fan-out COUNTS, and what an alarm action is ALLOWED to publish — both read
//! on the WHOLE assembled emulator, with request authentication ON, which is the configuration
//! the router serves traffic in.
//!
//! Two rules, each cross-service, so each is asserted where a client sees it: the counters
//! through CloudWatch's own `GetMetricStatistics`, and the alarm action through the alarm's own
//! `DescribeAlarmHistory` beside the queue subscribed to the action's topic.
//!
//! 1. **A topic reports what its publish path did as four INDEPENDENT `AWS/SNS` series
//!    dimensioned by `TopicName`**: one count per publish the topic accepted
//!    (`NumberOfMessagesPublished`), one per subscription the message was offered to whose
//!    filter policy did not admit it (`NumberOfNotificationsFilteredOut`), one per notification
//!    a subscribed endpoint took (`NumberOfNotificationsDelivered`) and one per notification it
//!    did not (`NumberOfNotificationsFailed`). No series absorbs another — a message a filter
//!    withheld is counted as filtered out and nowhere else, a notification an endpoint refused
//!    is counted as failed and not as delivered — and a counter nothing contributed to carries
//!    NO datapoint rather than a zero, so a `Sum` over a window is the events that window
//!    carried. That is what lets a client tell "the filter withheld it" from "the endpoint
//!    refused it" instead of seeing an idle topic. The metric names are documented rather than
//!    modelled; the namespace is one `PutMetricData` reserves ("you should not specify a
//!    namespace that begins with `AWS/`", `PutMetricData$Namespace`), so these series can only
//!    come from the publish path itself.
//!
//!    The rows differ along what the rule generalises over: how many subscriptions the topic
//!    has (none, one, two), whether each subscription's filter policy admits the message, WHY a
//!    notification fails (a queue that has granted the send to nobody, a queue whose policy
//!    denies it) and how many publishes the window carried.
//!
//! 2. **Executing an alarm action is a cross-service request CloudWatch makes as the service
//!    principal `cloudwatch.amazonaws.com`, so the destination topic's own access policy
//!    decides it**: the publish is made only where that policy allows `SNS:Publish` for that
//!    principal on that topic, and a policy that withholds the grant — by stating none for it,
//!    or by denying it outright — suppresses the publish. Nothing then reaches the topic's
//!    subscribers and the alarm's history records the `Action` item with `actionState`
//!    `Failed`, an `error` naming the authorization failure and the topic, and no
//!    `publishedMessage`; where the grant is there the action is `Succeeded` and the notification
//!    arrives. `PutMetricAlarm$OKActions` and `DescribeAlarmHistory` are the modelled contract;
//!    the `Action` item's `HistoryData` document is not a Smithy shape.
//!
//!    The rows differ along which action list the transition fires (`AlarmActions`,
//!    `OKActions`, `InsufficientDataActions`) and along the FORM the topic's policy takes (a
//!    grant to another service principal, a grant of this principal, a grant of another action
//!    only, an allow of everyone with a deny of this principal beside it).
//!
//! The crate-local suites of `service-com-amazonaws-sns-amazonsimplenotificationservice` and
//! `service-com-amazonaws-cloudwatch-graniteserviceversion20100801` pin both rules over a few
//! services mounted alone with authentication OFF. What only this suite adds is the EMULATOR AS
//! SERVED: every service the router assembles, and signatures verified on every request —
//! including the ones these paths make through the emulator's own callback, which such a router
//! has to accept before a counter is written or a notification published.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so each test builds it — and
//! drives its requests — on an explicitly sized thread. Every wait below is on the CONDITION
//! rather than on a span, so a slow host delays a row instead of breaking it.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// Keep this at or above the size `boot_serve.rs` uses: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call is signed for and addressed in.
const REGION: &str = "us-west-2";

/// Amazon SNS's signing name and the API version its form wire carries.
const SNS_SIGNING_NAME: &str = "sns";
const SNS_API_VERSION: &str = "2010-03-31";

/// CloudWatch's signing name and the `X-Amz-Target` prefix its model declares.
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// Amazon SQS's signing name and target prefix.
const SQS_SIGNING_NAME: &str = "sqs";
const SQS_TARGET_PREFIX: &str = "AmazonSQS";

/// The content types the two RPC wires and the form wire are sent with.
const JSON_1_0: &str = "application/x-amz-json-1.0";
const FORM: &str = "application/x-www-form-urlencoded";

/// The namespace, dimension and metric names under test, spelled out here rather than imported
/// so a wrong constant in the publisher cannot make this suite pass.
const SNS_NAMESPACE: &str = "AWS/SNS";
const TOPIC_NAME_DIMENSION: &str = "TopicName";
const PUBLISHED_METRIC: &str = "NumberOfMessagesPublished";
const FILTERED_OUT_METRIC: &str = "NumberOfNotificationsFilteredOut";
const DELIVERED_METRIC: &str = "NumberOfNotificationsDelivered";
const FAILED_METRIC: &str = "NumberOfNotificationsFailed";

/// The four series of rule 1, in the order a row states its expectations in.
const SERIES: [&str; 4] = [
    PUBLISHED_METRIC,
    FILTERED_OUT_METRIC,
    DELIVERED_METRIC,
    FAILED_METRIC,
];

/// The unit a count of things is published in: `Count`, the `StandardUnit` value for a number
/// of things.
const COUNT_UNIT: &str = "Count";

/// How many seconds either side of the calls a metric read spans, and the period it folds that
/// window into — the resolution these counters are published at.
const WINDOW_SECS: f64 = 300.0;
const PERIOD_SECS: i64 = 60;

/// The message attribute every publish carries, and the one a filter policy selects on.
const PROBE_ATTRIBUTE: &str = "probe";

/// The attribute value every publish below carries.
const PROBE_VALUE: &str = "emit";

/// The service principal this emulator executes an alarm action as, which a topic policy names
/// under `Principal` as `{"Service": …}`.
const CLOUDWATCH_PRINCIPAL: &str = "cloudwatch.amazonaws.com";

/// The service principal Amazon SNS hands a notification to a subscribed queue as, which that
/// queue's own policy has to allow the send for.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// The IAM action publishing to a topic performs, as a topic policy names it.
const PUBLISH_ACTION: &str = "sns:Publish";

/// How a refused publish names the action it was refused, as the recorded `error` carries it.
const REFUSED_ACTION: &str = "not authorized to perform: SNS:Publish";

/// The two readings that withhold the grant, as the recorded `error` words each.
const EXPLICIT_DENY: &str = "with an explicit deny in a resource-based policy";
const NO_ALLOW: &str = "because no resource-based policy allows the SNS:Publish action";

/// The alarm states a row drives its alarm through.
const ALARM: &str = "ALARM";
const OK: &str = "OK";
const INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";

/// How long a row looks for what it waits on before giving up, and how often it looks
/// meanwhile. Generous: what is asserted is what the emulator reports, never how fast the host
/// runs.
const WAIT: Duration = Duration::from_secs(90);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The assembled emulator a test drives, and the keys every request to it is signed with.
struct Harness {
    /// Every service `core/router` mounts, on one engine, verifying request signatures.
    router: MultiServiceRouter,
    /// The access key id this process publishes once authentication is on.
    key_id: String,
    /// Its secret.
    secret: String,
}

/// One assembled emulator with request authentication ON.
fn harness() -> Harness {
    let mut router = aws_emulator_router::build_router(aws_emulator_router::new_engine());
    router.set_request_authentication(true);
    let published = ::emulator_server::SigV4Authenticator::minted_credentials()
        .expect("turning authentication on publishes this process's keys");
    Harness {
        router,
        key_id: published.access_key_id.clone(),
        secret: published.secret_access_key.clone(),
    }
}

impl Harness {
    /// Sign `request` for `signing_name` in [`REGION`], in the `Authorization` header an SDK
    /// request carries.
    fn sign(
        &self,
        mut request: http::Request<Vec<u8>>,
        signing_name: &str,
    ) -> http::Request<Vec<u8>> {
        let identity = Credentials::new(&self.key_id, &self.secret, None, None, "test").into();
        let params = SigningParams::builder()
            .identity(&identity)
            .region(REGION)
            .name(signing_name)
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .expect("the signing parameters are complete")
            .into();
        let (method, uri) = (request.method().to_string(), request.uri().to_string());
        let headers: Vec<(String, String)> = request
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().expect("a text header").to_string(),
                )
            })
            .collect();
        let signable = SignableRequest::new(
            &method,
            &uri,
            headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            SignableBody::Bytes(request.body()),
        )
        .expect("the request is signable");
        let (instructions, _) = sign(signable, &params)
            .expect("the request signs")
            .into_parts();
        instructions.apply_to_request_http1x(&mut request);
        request
    }

    /// One SIGNED `POST /` on the regional endpoint of `signing_name`, answering the HTTP
    /// status and the body bytes as answered.
    async fn call(
        &self,
        signing_name: &str,
        content_type: &str,
        target: Option<&str>,
        body: Vec<u8>,
    ) -> (u16, Vec<u8>) {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let mut builder = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}/"))
            .header("host", &host)
            .header("content-type", content_type);
        if let Some(target) = target {
            builder = builder.header("x-amz-target", target);
        }
        let built = builder.body(body).expect("the test request is well formed");
        let signed = self.sign(built, signing_name);
        let (parts, payload) = signed.into_parts();
        let request = http::Request::from_parts(parts, bytes::Bytes::from(payload));
        let response = self.router.dispatch(request).await;
        (response.status().as_u16(), response.body().to_vec())
    }
}

/// One `X-Amz-Target` RPC call, requiring a 200 and answering the parsed body.
async fn rpc(
    harness: &Harness,
    case: &str,
    signing_name: &str,
    target: &str,
    body: &Value,
) -> Value {
    let (status, payload) = harness
        .call(
            signing_name,
            JSON_1_0,
            Some(target),
            body.to_string().into_bytes(),
        )
        .await;
    let parsed: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {target}, got {status} with {parsed}"
    );
    parsed
}

/// One CloudWatch call over its `awsJson1_0` wire.
async fn cloudwatch(harness: &Harness, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        harness,
        case,
        CLOUDWATCH_SIGNING_NAME,
        &format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
        body,
    )
    .await
}

/// One Amazon SQS call over its `awsJson1_0` wire.
async fn sqs(harness: &Harness, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        harness,
        case,
        SQS_SIGNING_NAME,
        &format!("{SQS_TARGET_PREFIX}.{operation}"),
        body,
    )
    .await
}

/// One Amazon SNS call over its `awsQuery` FORM wire, requiring a 200 and answering the XML
/// body. The form body is built with the ONE encoder this test crate holds.
async fn sns(harness: &Harness, case: &str, action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={SNS_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, payload) = harness
        .call(SNS_SIGNING_NAME, FORM, None, body.into_bytes())
        .await;
    let text = String::from_utf8_lossy(&payload).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SNS {action}, got {status} with {text}"
    );
    text
}

/// A whole policy document carrying `statements`, as a resource's owner writes it.
fn policy(statements: Vec<Value>) -> String {
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// One policy statement: an effect, a principal, an action and the resource it is about.
fn statement(effect: &str, principal: Value, action: &str, resource: &str) -> Value {
    json!({
        "Sid": format!("{effect}{}", action.replace([':', '*'], "")),
        "Effect": effect,
        "Principal": principal,
        "Action": action,
        "Resource": resource,
    })
}

/// What the queue subscribed to a topic has granted Amazon SNS about the send a notification
/// to it performs — which is what decides whether that notification is TAKEN.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueGrant {
    /// The queue policy AWS documents for a queue subscribed to a topic.
    Granted,
    /// A queue carrying no policy of its own, which has granted the service nothing.
    Silent,
    /// A queue whose policy denies the send outright.
    Denied,
}

/// A queue called `name`: its url and its ARN, carrying the policy `grant` describes.
async fn given_queue(
    harness: &Harness,
    case: &str,
    name: &str,
    grant: QueueGrant,
) -> (String, String) {
    let created = sqs(harness, case, "CreateQueue", &json!({"QueueName": name})).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        harness,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": &url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn in {attributes}"))
        .to_string();
    let document = match grant {
        QueueGrant::Silent => None,
        QueueGrant::Granted => Some(policy(vec![statement(
            "Allow",
            json!({"Service": SNS_PRINCIPAL}),
            "sqs:SendMessage",
            &arn,
        )])),
        QueueGrant::Denied => Some(policy(vec![statement(
            "Deny",
            json!({"Service": SNS_PRINCIPAL}),
            "sqs:SendMessage",
            &arn,
        )])),
    };
    if let Some(document) = document {
        sqs(
            harness,
            case,
            "SetQueueAttributes",
            &json!({"QueueUrl": &url, "Attributes": {"Policy": document}}),
        )
        .await;
    }
    (url, arn)
}

/// Receive everything waiting on the queue at `url` and delete it; answers how many messages
/// were taken off it.
async fn drain(harness: &Harness, case: &str, url: &str) -> usize {
    let received = sqs(
        harness,
        case,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    for message in &messages {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                harness,
                case,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            )
            .await;
        }
    }
    messages.len()
}

/// A topic called `name`: its ARN, as `CreateTopic` reports it.
async fn given_topic(harness: &Harness, case: &str, name: &str) -> String {
    let created = sns(harness, case, "CreateTopic", &[("Name", name)]).await;
    element(&created, "TopicArn")
}

/// Wall-clock now as epoch seconds, the clock a metric window is measured against.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// The datapoints `GetMetricStatistics` holds for one `AWS/SNS` series of `topic_name` across
/// the window around this row's calls.
async fn datapoints(harness: &Harness, case: &str, topic_name: &str, metric: &str) -> Vec<Value> {
    let now = now_secs();
    let answered = cloudwatch(
        harness,
        case,
        "GetMetricStatistics",
        &json!({
            "Namespace": SNS_NAMESPACE,
            "MetricName": metric,
            "Dimensions": [{"Name": TOPIC_NAME_DIMENSION, "Value": topic_name}],
            "StartTime": now - WINDOW_SECS,
            "EndTime": now + WINDOW_SECS,
            "Period": PERIOD_SECS,
            "Statistics": ["Sum"],
        }),
    )
    .await;
    answered["Datapoints"].as_array().cloned().unwrap_or_default()
}

/// What `metric` totals for `topic_name` across the window — the `Sum` of every datapoint it
/// holds — or `None` when the series holds none, which is what a counter nothing contributed
/// to reports.
async fn total(harness: &Harness, case: &str, topic_name: &str, metric: &str) -> Option<f64> {
    let points = datapoints(harness, case, topic_name, metric).await;
    if points.is_empty() {
        return None;
    }
    Some(
        points
            .iter()
            .filter_map(|point| point.get("Sum").and_then(Value::as_f64))
            .sum(),
    )
}

/// What all four series of [`SERIES`] total for `topic_name`, in that order.
async fn totals(harness: &Harness, case: &str, topic_name: &str) -> [Option<f64>; 4] {
    let mut read = [None; 4];
    for (index, metric) in SERIES.iter().enumerate() {
        read[index] = total(harness, case, topic_name, metric).await;
    }
    read
}

/// One subscription of a row's topic: what its queue granted the send, and the filter policy
/// written on the subscription.
struct Subscriber {
    /// What the subscribed queue says about the `SendMessage` a notification performs.
    grant: QueueGrant,
    /// The value this subscription's `FilterPolicy` admits, or `None` for a subscription
    /// carrying no filter policy at all.
    admits: Option<&'static str>,
}

/// One row of rule 1: the topic's subscriptions, how many publishes the window carried, and
/// what each of the four series must then total.
struct CounterRow {
    case: &'static str,
    /// The topic and queue names this row works on, so each row's totals are its own.
    slug: &'static str,
    subscribers: &'static [Subscriber],
    publishes: usize,
    /// `NumberOfMessagesPublished`, `…FilteredOut`, `…Delivered`, `…Failed`.
    expected: [Option<f64>; 4],
}

/// RULE 1, on the assembled emulator: a topic's publish path is reported as four independent
/// `AWS/SNS` counters dimensioned by `TopicName`, each counting its own events, and a counter
/// nothing contributed to holds no datapoint.
#[test]
fn a_topic_s_fan_out_is_counted_into_its_own_aws_sns_series() {
    on_a_sized_thread("sns-fan-out-counters", drive_counters);
}

/// Every row of rule 1, against ONE assembled emulator: each row has its own topic, so the
/// totals it asserts are its own.
async fn drive_counters() {
    let rows = [
        CounterRow {
            case: "a filter withholds one subscription while the other's queue granted nothing",
            slug: "filtered-and-ungranted",
            subscribers: &[
                Subscriber {
                    grant: QueueGrant::Granted,
                    admits: Some("other"),
                },
                Subscriber {
                    grant: QueueGrant::Silent,
                    admits: None,
                },
            ],
            publishes: 1,
            expected: [Some(1.0), Some(1.0), None, Some(1.0)],
        },
        CounterRow {
            case: "a queue that denies the send fails beside a filter that admits the message",
            slug: "denied-and-admitted",
            subscribers: &[
                Subscriber {
                    grant: QueueGrant::Denied,
                    admits: None,
                },
                Subscriber {
                    grant: QueueGrant::Granted,
                    admits: Some(PROBE_VALUE),
                },
            ],
            publishes: 1,
            expected: [Some(1.0), None, Some(1.0), Some(1.0)],
        },
        CounterRow {
            case: "two subscribers take both publishes",
            slug: "two-subscribers-twice",
            subscribers: &[
                Subscriber {
                    grant: QueueGrant::Granted,
                    admits: None,
                },
                Subscriber {
                    grant: QueueGrant::Granted,
                    admits: None,
                },
            ],
            publishes: 2,
            expected: [Some(2.0), None, Some(4.0), None],
        },
        CounterRow {
            case: "a topic nothing is subscribed to counts only the publishes it accepted",
            slug: "no-subscriber",
            subscribers: &[],
            publishes: 3,
            expected: [Some(3.0), None, None, None],
        },
    ];

    let harness = harness();
    for row in &rows {
        let case = row.case;
        let topic_name = format!("counters-{}", row.slug);
        let topic = given_topic(&harness, case, &topic_name).await;
        for (index, subscriber) in row.subscribers.iter().enumerate() {
            let (_, queue_arn) = given_queue(
                &harness,
                case,
                &format!("counters-{}-{index}", row.slug),
                subscriber.grant,
            )
            .await;
            let mut params = vec![
                ("TopicArn", topic.as_str()),
                ("Protocol", "sqs"),
                ("Endpoint", queue_arn.as_str()),
                ("ReturnSubscriptionArn", "true"),
            ];
            let admitted = subscriber
                .admits
                .map(|value| json!({PROBE_ATTRIBUTE: [value]}).to_string());
            if let Some(admitted) = admitted.as_deref() {
                params.push(("Attributes.entry.1.key", "FilterPolicy"));
                params.push(("Attributes.entry.1.value", admitted));
            }
            let subscribed = sns(&harness, case, "Subscribe", &params).await;
            let arn = element(&subscribed, "SubscriptionArn");
            assert!(
                arn.starts_with(topic.as_str()),
                "case {case:?}: a queue subscription is confirmed at once, so its ARN extends \
                 the topic's, got {arn:?}"
            );
        }
        for sequence in 0..row.publishes {
            sns(
                &harness,
                case,
                "Publish",
                &[
                    ("TopicArn", topic.as_str()),
                    ("Message", &format!("{} message {sequence}", row.slug)),
                    ("MessageAttributes.entry.1.Name", PROBE_ATTRIBUTE),
                    ("MessageAttributes.entry.1.Value.DataType", "String"),
                    ("MessageAttributes.entry.1.Value.StringValue", PROBE_VALUE),
                ],
            )
            .await;
        }

        // The counters are written as the publish path resolves each event, so the read that
        // follows the publishes sees them; the loop waits for that CONDITION rather than for a
        // span, and a row whose counters never reach what the rule says fails on the totals
        // below.
        let deadline = Instant::now() + WAIT;
        let mut actual = totals(&harness, case, &topic_name).await;
        while actual != row.expected && Instant::now() < deadline {
            tokio::time::sleep(LOOK_INTERVAL).await;
            actual = totals(&harness, case, &topic_name).await;
        }
        for (index, metric) in SERIES.iter().enumerate() {
            let (reported, expected) = (actual[index], row.expected[index]);
            assert_eq!(
                reported, expected,
                "case {case:?}: {metric} for {TOPIC_NAME_DIMENSION}={topic_name} must total \
                 {expected:?}, got {reported:?}"
            );
        }
        // Each count is one observation of one event, so every datapoint of a series that has
        // one is published in the unit a count of things carries.
        for metric in SERIES {
            for point in datapoints(&harness, case, &topic_name, metric).await {
                let unit = point.get("Unit").and_then(Value::as_str);
                assert_eq!(
                    unit,
                    Some(COUNT_UNIT),
                    "case {case:?}: {metric} counts events, so its datapoints carry \
                     {COUNT_UNIT}, got {point}"
                );
            }
        }
    }
}

/// What the topic's own policy said about the publish an alarm action makes.
#[derive(Debug, Clone, Copy)]
enum Grant {
    /// The publish was made: the action is `Succeeded` and the subscriber receives the
    /// notification.
    Allowed,
    /// The publish was withheld, and the recorded `error` carries this reading of the policy.
    Withheld(&'static str),
}

/// One row of rule 2: which action list the transition fires, the policy the topic carries, and
/// what that policy says about the publish.
struct ActionRow {
    case: &'static str,
    /// The topic, queue and alarm names this row works on.
    slug: &'static str,
    /// The action-list member of `PutMetricAlarm` the topic is configured on.
    list: &'static str,
    /// The state the alarm is driven into, which is the one that fires that list.
    fires_in: &'static str,
    /// The statements the topic's owner wrote on it, over the topic's own ARN.
    statements: fn(&str) -> Vec<Value>,
    grant: Grant,
}

/// RULE 2, on the assembled emulator: an alarm action's publish is made only where the target
/// topic's own policy allows this service principal `SNS:Publish` on that topic, and a refused
/// one is recorded as the authorization failure it was with nothing published.
#[test]
fn an_alarm_action_is_published_only_where_its_topic_grants_this_service_the_publish() {
    on_a_sized_thread("sns-alarm-action-grant", drive_action_grants);
}

/// Every row of rule 2, against ONE assembled emulator: each row has its own topic, queue and
/// alarm.
async fn drive_action_grants() {
    let rows = [
        ActionRow {
            case: "an OK action on a topic that grants the publish to another service only",
            slug: "ok-other-principal",
            list: "OKActions",
            fires_in: OK,
            statements: |topic| {
                vec![statement(
                    "Allow",
                    json!({"Service": "s3.amazonaws.com"}),
                    PUBLISH_ACTION,
                    topic,
                )]
            },
            grant: Grant::Withheld(NO_ALLOW),
        },
        ActionRow {
            case: "an OK action on a topic that grants this service the publish",
            slug: "ok-granted",
            list: "OKActions",
            fires_in: OK,
            statements: |topic| {
                vec![statement(
                    "Allow",
                    json!({"Service": CLOUDWATCH_PRINCIPAL}),
                    PUBLISH_ACTION,
                    topic,
                )]
            },
            grant: Grant::Allowed,
        },
        ActionRow {
            case: "an ALARM action on a topic that allows everyone but denies this service",
            slug: "alarm-denied",
            list: "AlarmActions",
            fires_in: ALARM,
            statements: |topic| {
                vec![
                    statement("Allow", json!("*"), PUBLISH_ACTION, topic),
                    statement(
                        "Deny",
                        json!({"Service": CLOUDWATCH_PRINCIPAL}),
                        PUBLISH_ACTION,
                        topic,
                    ),
                ]
            },
            grant: Grant::Withheld(EXPLICIT_DENY),
        },
        ActionRow {
            case: "an INSUFFICIENT_DATA action on a topic that grants this service another \
                   action only",
            slug: "insufficient-other-action",
            list: "InsufficientDataActions",
            fires_in: INSUFFICIENT_DATA,
            statements: |topic| {
                vec![statement(
                    "Allow",
                    json!({"Service": CLOUDWATCH_PRINCIPAL}),
                    "sns:GetTopicAttributes",
                    topic,
                )]
            },
            grant: Grant::Withheld(NO_ALLOW),
        },
    ];

    let harness = harness();
    for row in &rows {
        let case = row.case;
        let topic = given_topic(&harness, case, &format!("action-{}", row.slug)).await;
        let (url, queue_arn) = given_queue(
            &harness,
            case,
            &format!("action-{}", row.slug),
            QueueGrant::Granted,
        )
        .await;
        sns(
            &harness,
            case,
            "Subscribe",
            &[
                ("TopicArn", topic.as_str()),
                ("Protocol", "sqs"),
                ("Endpoint", queue_arn.as_str()),
                ("ReturnSubscriptionArn", "true"),
            ],
        )
        .await;
        sns(
            &harness,
            case,
            "SetTopicAttributes",
            &[
                ("TopicArn", topic.as_str()),
                ("AttributeName", "Policy"),
                ("AttributeValue", &policy((row.statements)(&topic))),
            ],
        )
        .await;

        let alarm = format!("action-{}", row.slug);
        cloudwatch(
            &harness,
            case,
            "PutMetricAlarm",
            &json!({
                "AlarmName": &alarm,
                "Namespace": "Custom/Orders",
                "MetricName": "Backlog",
                "Dimensions": [{"Name": "Row", "Value": &alarm}],
                "Statistic": "Average",
                "Period": PERIOD_SECS,
                "EvaluationPeriods": 1,
                "Threshold": 1.0,
                "ComparisonOperator": "GreaterThanOrEqualToThreshold",
                row.list: [&topic],
            }),
        )
        .await;

        // A transition is what fires an action list, so the alarm is driven through a state
        // that is NOT the row's and then into the row's — whose list is the only one this
        // alarm carries, so the first move fires nothing.
        let elsewhere = if row.fires_in == ALARM { OK } else { ALARM };
        for state in [elsewhere, row.fires_in] {
            cloudwatch(
                &harness,
                case,
                "SetAlarmState",
                &json!({
                    "AlarmName": &alarm,
                    "StateValue": state,
                    "StateReason": "driven by this suite",
                }),
            )
            .await;
        }

        // Delivery is asynchronous, so the record of it is looked for until it is there or the
        // deadline passes — the wait is on the CONDITION.
        let deadline = Instant::now() + WAIT;
        let mut items = action_items(&harness, case, &alarm, &topic).await;
        while items.is_empty() && Instant::now() < deadline {
            tokio::time::sleep(LOOK_INTERVAL).await;
            items = action_items(&harness, case, &alarm, &topic).await;
        }
        assert!(
            !items.is_empty(),
            "case {case:?}: expected an Action item naming {topic} in the history of {alarm:?}, \
             got none"
        );
        for item in &items {
            assert_action_item(case, item, &topic, row.grant);
        }

        match row.grant {
            Grant::Allowed => {
                let mut arrived = drain(&harness, case, &url).await;
                let deadline = Instant::now() + WAIT;
                while arrived == 0 && Instant::now() < deadline {
                    tokio::time::sleep(LOOK_INTERVAL).await;
                    arrived += drain(&harness, case, &url).await;
                }
                assert!(
                    arrived > 0,
                    "case {case:?}: a published action reaches the queue subscribed to {topic}, \
                     got nothing on {url}"
                );
            }
            Grant::Withheld(_) => {
                // The record is written as the action resolves, so by the time it is there the
                // publish decision has been taken: a notification on the queue would be one
                // the policy withheld.
                let arrived = drain(&harness, case, &url).await;
                assert_eq!(
                    arrived, 0,
                    "case {case:?}: a withheld action publishes nothing, so the queue \
                     subscribed to {topic} must hold no notification, got {arrived} on {url}"
                );
            }
        }
    }
}

/// Every `Action` item of the alarm `alarm`'s history whose record names `topic`, as the parsed
/// `HistoryData` document of each.
async fn action_items(harness: &Harness, case: &str, alarm: &str, topic: &str) -> Vec<Value> {
    let history = cloudwatch(
        harness,
        case,
        "DescribeAlarmHistory",
        &json!({
            "AlarmName": alarm,
            "HistoryItemType": "Action",
            "AlarmTypes": ["MetricAlarm"],
        }),
    )
    .await;
    history["AlarmHistoryItems"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|item| {
            let data = item["HistoryData"].as_str().unwrap_or_default();
            serde_json::from_str::<Value>(data).ok()
        })
        .filter(|parsed| parsed["notificationResource"].as_str() == Some(topic))
        .collect()
}

/// Assert ONE `Action` item against what the topic's policy said about the publish.
fn assert_action_item(case: &str, item: &Value, topic: &str, grant: Grant) {
    let expected = match grant {
        Grant::Allowed => "Succeeded",
        Grant::Withheld(_) => "Failed",
    };
    let state = item["actionState"].as_str().unwrap_or_default();
    assert_eq!(
        state, expected,
        "case {case:?}: expected actionState {expected:?}, got {item}"
    );
    let error = item["error"].as_str();
    let message = item["publishedMessage"].as_str();
    let Grant::Withheld(reason) = grant else {
        assert_eq!(
            error, None,
            "case {case:?}: a published action records no error, got {item}"
        );
        assert!(
            message.is_some_and(|published| published.contains("\"NewStateValue\"")),
            "case {case:?}: a published action records the notification it handed over, got \
             {item}"
        );
        return;
    };
    assert_eq!(
        message, None,
        "case {case:?}: a refused action hands nothing over, so it records no published \
         message, got {item}"
    );
    let error = error.unwrap_or_default();
    assert!(
        error.contains(REFUSED_ACTION) && error.contains(topic) && error.contains(reason),
        "case {case:?}: the error must name the {REFUSED_ACTION:?} failure, the topic {topic:?} \
         and {reason:?}, got {error:?}"
    );
}

/// Build the future `driving` makes and run it to completion on a thread sized for assembling
/// the whole router, named `name`.
///
/// The future is MADE on that thread rather than handed to it, so everything the drive holds —
/// the assembled router among it — belongs to the thread that polls it.
fn on_a_sized_thread<Make, Driving>(name: &str, driving: Make)
where
    Make: FnOnce() -> Driving + Send + 'static,
    Driving: std::future::Future<Output = ()>,
{
    std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(driving());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}
