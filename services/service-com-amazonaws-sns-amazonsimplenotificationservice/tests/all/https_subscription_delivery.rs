// SPDX-License-Identifier: CC0-1.0

//! `Subscribe` and `Publish` POST to an `https` subscription's ENDPOINT, end to end over the
//! wire through the real `MultiServiceRouter`, with the endpoint served by another service of
//! this same emulator — a Lambda function URL whose handler records every request it was sent
//! into an Amazon SQS queue and answers the status the request's own path asks for.
//!
//! The RULES asserted here, all about one subscription to a URL:
//!
//! * "https – delivery of JSON-encoded message via HTTPS POST" (`SubscribeInput$Protocol`):
//!   `Subscribe` POSTs a `SubscriptionConfirmation` to the endpoint — carrying the `Token`
//!   `ConfirmSubscription` is called with and the `SubscribeURL` that presents it — and, once
//!   the subscription is confirmed, every `Publish` is POSTed as a `Notification` whose
//!   `x-amz-sns-message-id` is the `MessageId` the publish answered with.
//! * "DeliveryPolicy – The policy that defines how Amazon SNS retries failed deliveries to
//!   HTTP/S endpoints" (`SetSubscriptionAttributesInput$AttributeName`): the attempts one
//!   notification costs are one plus the `healthyRetryPolicy`'s `numRetries` of the policy in
//!   force on that subscription — the one `EffectiveDeliveryPolicy` reports — so a
//!   subscription written no retry budget is POSTed to exactly once and one written a larger
//!   budget is attempted again for the SAME publish.
//! * "RedrivePolicy – When specified, sends undeliverable messages to the specified Amazon SQS
//!   dead-letter queue" (same member): the notification an endpoint never took is held in that
//!   queue, once per publish, in the form the subscription renders the message.
//!
//! The rows differ along what the rules generalise over: whether the endpoint takes the
//! notification, the retry budget the subscription was written (none at all, zero, one, two),
//! whether it asks for a redrive, and whether it asks for raw delivery. The attempts are
//! counted on the REQUESTS the endpoint recorded, so no row can pass without the POSTs really
//! being made — and those records are CONSUMED as they are read ([`consumed`]), so one POST is
//! counted exactly once however long a case runs. Nothing here is decided by a span of host
//! seconds: a case that waits for an absence waits for a whole schedule the emulator itself
//! runs ([`recorded_while_a_whole_schedule_runs`]), and every other wait looks again until the
//! condition it is about holds.
//!
//! The `http` protocol's own half of the rule is the same path with the other scheme; what
//! differs is only the scheme an endpoint is read under, which the delivery module's own unit
//! test pins over both. This suite drives `https`, because a URL served inside this emulator
//! is published as an `https` one.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState;

use crate::lambda_subscription_dead_letter::{given_queue, held};
use crate::lambda_subscription_delivery::{
    emulator, fetch, given_function_running, lambda, publish, sns, sns_signed, sns_unsigned, sqs,
    subscribe, Emulator,
};
use crate::sqs_subscription_delivery::element;

/// The subscription protocol under test: the endpoint is a URL, POSTed to over HTTPS.
const HTTPS_PROTOCOL: &str = "https";

/// The `Content-Type` a notification is POSTed with: the request policy of the subscription's
/// effective delivery policy, which for a subscription nobody gave one is the account system
/// default (`GetSubscriptionAttributesResponse$Attributes`, `EffectiveDeliveryPolicy`).
const DELIVERY_CONTENT_TYPE: &str = "text/plain; charset=UTF-8";

/// The header a RAW delivery carries, which is how an endpoint tells the bare published body
/// it was handed from the notification envelope it would otherwise receive.
const RAW_DELIVERY_HEADER: &str = "x-amz-sns-rawdelivery";

/// How long a wait for a recorded request or a dead letter lasts before a case fails: far
/// longer than any delivery here takes, so a loaded machine does not fail a case the emulator
/// would have served.
///
/// The longest schedule a case waits out — the published opening phases plus the two retries
/// the case's own policy states ([`SCHEDULED_ATTEMPTS`]) — is some fifteen seconds of
/// emulator time, so this is an order of magnitude of room: a whole-workspace test run has
/// been seen to stretch that schedule past a minute, which is the host's speed and not the
/// service's.
const DEADLINE: Duration = Duration::from_secs(180);

/// How often such a wait looks again.
const LOOK_INTERVAL: Duration = Duration::from_millis(25);

/// The handler behind the function URL every row's endpoint addresses.
///
/// It records every request it was sent — the path, the headers and the body — on the capture
/// queue whose url is formatted into it, which is how the POSTs SNS made are observed from
/// outside the endpoint, and then answers from the request's own path: a confirmation request
/// is always taken, a notification to a `/fail` path is refused with a server error, and every
/// other notification is taken.
///
/// `pub(crate)`: the suite that pins the ONE authority a delivered document's URL members name
/// observes the POSTs through this same endpoint rather than a second copy of it
/// (`signing_certificate_address.rs`).
pub(crate) const SINK_HANDLER: &str = r#"
import base64
import json

import boto3


def lambda_handler(event, context):
    headers = event.get("headers") or {}
    path = event.get("rawPath") or "/"
    body = event.get("body") or ""
    if event.get("isBase64Encoded"):
        body = base64.b64decode(body).decode("utf-8")
    boto3.client("sqs").send_message(
        QueueUrl="{queue_url}",
        MessageBody=json.dumps({"path": path, "headers": headers, "body": body}),
    )
    if headers.get("x-amz-sns-message-type") == "SubscriptionConfirmation":
        return {"statusCode": 200, "body": "confirmed"}
    if path.startswith("/fail"):
        return {"statusCode": 500, "body": "the sink could not take this notification"}
    return {"statusCode": 200, "body": "taken"}
"#;

/// One request the endpoint recorded.
///
/// `pub(crate)` for the reason [`SINK_HANDLER`] is.
#[derive(Debug, Clone)]
pub(crate) struct Recorded {
    /// The path the request addressed, which is the subscription it belongs to.
    pub(crate) path: String,
    /// The request's headers, as HTTP compares them: lowercased.
    pub(crate) headers: Value,
    /// The document POSTed.
    pub(crate) body: String,
}

impl Recorded {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(Value::as_str)
    }

    /// The POSTed document as JSON, or a failing case when it is not JSON at all.
    pub(crate) fn document(&self, case: &str) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the POSTed body must be a JSON document, got {error} for {:?}",
                self.body
            )
        })
    }
}

/// Every message waiting on the queue at `url`, CONSUMED: each one is received and then
/// DELETED, so one message is observed exactly once however long a case runs.
///
/// A message that is only received is still ON the queue: it is hidden for the queue's
/// visibility timeout and handed out again once that lapses. A reader that counts what it saw
/// would then count the same record a second time as soon as the case outlasted that timeout —
/// an outcome decided by how fast the host served the requests rather than by what the
/// emulator did — so every read of a queue this suite observes through deletes what it read.
async fn consumed(emulator: &Emulator, url: &str) -> Vec<Value> {
    let mut taken = Vec::new();
    loop {
        let received = sqs(
            emulator,
            "ReceiveMessage",
            &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
        )
        .await;
        let messages = received["Messages"].as_array().cloned().unwrap_or_default();
        if messages.is_empty() {
            return taken;
        }
        for message in messages {
            sqs(
                emulator,
                "DeleteMessage",
                &json!({
                    "QueueUrl": url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            )
            .await;
            taken.push(message);
        }
    }
}

/// Every request the endpoint has recorded on the capture queue so far, drained: the queue is
/// consumed until a read reports nothing, so one call collects the whole batch and no request
/// is ever collected twice ([`consumed`]).
async fn drain_recorded(emulator: &Emulator, capture_url: &str) -> Vec<Recorded> {
    consumed(emulator, capture_url)
        .await
        .into_iter()
        .map(|message| {
            let body = message["Body"].as_str().unwrap_or_default();
            let captured: Value = serde_json::from_str(body).unwrap_or_else(|error| {
                panic!("the capture queue must hold JSON records, got {error} for {body:?}")
            });
            Recorded {
                path: captured["path"].as_str().unwrap_or_default().to_string(),
                headers: captured["headers"].clone(),
                body: captured["body"].as_str().unwrap_or_default().to_string(),
            }
        })
        .collect()
}

/// The requests recorded for `path`, waiting until there are at least `expected` of them — or
/// until the deadline, which is how a case that expects a COUNT fails with what it did see.
///
/// It LOOKS until the count holds rather than waiting any span out, so it answers the moment the
/// endpoint has been POSTed to that many times — whether the POSTs were made inside the call
/// that owed them or on the schedule that outlived it — and the deadline only bounds the case
/// where they never come.
///
/// `pub(crate)` for the reason [`SINK_HANDLER`] is.
pub(crate) async fn recorded_for(
    emulator: &Emulator,
    capture_url: &str,
    path: &str,
    expected: usize,
) -> Vec<Recorded> {
    let deadline = Instant::now() + DEADLINE;
    let mut collected: Vec<Recorded> = Vec::new();
    loop {
        collected.extend(
            drain_recorded(emulator, capture_url)
                .await
                .into_iter()
                .filter(|recorded| recorded.path == path),
        );
        if collected.len() >= expected || Instant::now() >= deadline {
            return collected;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The function URL the endpoint of every row addresses: an `AuthType=NONE` URL of `function`,
/// granted to every caller so an anonymous delivery reaches the handler.
///
/// `pub(crate)` for the reason [`SINK_HANDLER`] is.
pub(crate) async fn given_function_url(emulator: &Emulator, function: &str) -> String {
    let (status, url) = lambda(
        emulator,
        "POST",
        &format!("/2021-10-31/functions/{function}/url"),
        json!({"AuthType": "NONE"}),
    )
    .await;
    assert_eq!(
        status, 201,
        "CreateFunctionUrlConfig {function}: expected HTTP 201, got {status} with {url}"
    );
    let (status, granted) = lambda(
        emulator,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        json!({
            "StatementId": "public-url",
            "Action": "lambda:InvokeFunctionUrl",
            "Principal": "*",
            "FunctionUrlAuthType": "NONE",
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "AddPermission {function}: expected HTTP 201, got {status} with {granted}"
    );
    url["FunctionUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateFunctionUrlConfig {function}: no FunctionUrl in {url}"))
        .trim_end_matches('/')
        .to_string()
}

/// Write one subscription attribute, through the call a client configures it with.
async fn set_attribute(emulator: &Emulator, subscription: &str, name: &str, value: &str) {
    sns(
        emulator,
        "SetSubscriptionAttributes",
        &[
            ("SubscriptionArn", subscription),
            ("AttributeName", name),
            ("AttributeValue", value),
        ],
    )
    .await;
}

/// The delivery policy that asks for `retries` retries of a failed delivery, with the shortest
/// delay a policy may name between them (`minDelayTarget` and `maxDelayTarget` are "delays in
/// seconds of at least 1", SNS Developer Guide, "Amazon SNS message delivery retries").
fn retry_policy(retries: i64) -> String {
    json!({
        "healthyRetryPolicy": {
            "numRetries": retries,
            "minDelayTarget": 1,
            "maxDelayTarget": 1,
            "numNoDelayRetries": 0,
            "numMinDelayRetries": 0,
            "numMaxDelayRetries": 0,
            "backoffFunction": "linear",
        },
    })
    .to_string()
}

/// One row: how the endpoint answers this subscription's notifications, the retry budget and
/// redrive the subscription is written, and what one publish must then cost.
struct Row {
    case: &'static str,
    /// The path on the sink this subscription's endpoint addresses, which is what tells the
    /// handler whether to take the notification — and what tells the recorded requests of one
    /// subscription from another's.
    path: &'static str,
    /// The `numRetries` written on the subscription, or `None` for one left on the account
    /// system defaults.
    retries: Option<i64>,
    /// Whether the subscription asks for a redrive.
    redrive: bool,
    /// Whether the subscription asks for raw delivery.
    raw: bool,
    /// The message published to this row's topic.
    message: &'static str,
    /// How many POSTs of that notification the endpoint must have recorded.
    posts: usize,
    /// How many dead letters the publish must leave in this subscription's queue.
    dead_letters: usize,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an endpoint that takes the notification",
            path: "/take/plain",
            retries: None,
            redrive: true,
            raw: false,
            message: "the plain body",
            posts: 1,
            dead_letters: 0,
        },
        Row {
            case: "a refusing endpoint and no retry budget at all",
            path: "/fail/none",
            retries: Some(0),
            redrive: true,
            raw: false,
            message: "the body nobody takes",
            posts: 1,
            dead_letters: 1,
        },
        Row {
            case: "a refusing endpoint and one retry",
            path: "/fail/one",
            retries: Some(1),
            redrive: true,
            raw: false,
            message: "the body attempted twice",
            posts: 2,
            dead_letters: 1,
        },
        Row {
            case: "a refusing endpoint and two retries",
            path: "/fail/two",
            retries: Some(2),
            redrive: true,
            raw: false,
            message: "the body attempted three times",
            posts: 3,
            dead_letters: 1,
        },
        Row {
            case: "a refusing endpoint and no redrive",
            path: "/fail/unheld",
            retries: Some(0),
            redrive: false,
            raw: false,
            message: "the body held nowhere",
            posts: 1,
            dead_letters: 0,
        },
        Row {
            case: "an endpoint taking a raw delivery",
            path: "/take/raw",
            retries: None,
            redrive: true,
            raw: true,
            message: "the raw body itself",
            posts: 1,
            dead_letters: 0,
        },
    ]
}

/// Every message waiting on a redrive's queue is read through the suite-shared reader of what
/// a redrive held ([`crate::lambda_subscription_dead_letter::held`]), which waits out its own
/// quiet budget for a row that expects none rather than a second copy of that rule here.

#[tokio::test]
async fn an_https_subscription_is_posted_to_under_its_own_delivery_policy() {
    let emulator = emulator();
    // The endpoint of every row: ONE function URL, whose handler records what it was sent and
    // answers from the path, so the rows differ in the subscription rather than in the sink.
    let (capture_url, _capture_arn) = given_queue(&emulator, "https-capture").await;
    let function = "https-subscription-sink";
    given_function_running(
        &emulator,
        function,
        &SINK_HANDLER.replace("{queue_url}", &capture_url),
    )
    .await;
    let sink = given_function_url(&emulator, function).await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let topic = element(
            &sns(
                &emulator,
                "CreateTopic",
                &[("Name", &format!("https-delivery-{index}"))],
            )
            .await,
            "TopicArn",
        );
        let endpoint = format!("{sink}{}", row.path);
        let subscription = subscribe(&emulator, &topic, HTTPS_PROTOCOL, &endpoint).await;

        // The confirmation request: it is POSTed by `Subscribe` itself, and it is what carries
        // the token no response of this service does.
        let confirmations = recorded_for(&emulator, &capture_url, row.path, 1).await;
        assert_eq!(
            confirmations.len(),
            1,
            "case {case:?}: expected exactly one SubscriptionConfirmation POST, got {}",
            confirmations.len()
        );
        let confirmation = &confirmations[0];
        assert_eq!(
            confirmation.header("x-amz-sns-message-type"),
            Some("SubscriptionConfirmation"),
            "case {case:?}: expected the confirmation's message-type header, got {:?}",
            confirmation.headers
        );
        assert_eq!(
            confirmation.header("x-amz-sns-topic-arn"),
            Some(topic.as_str()),
            "case {case:?}: expected the topic {topic:?} in the confirmation's headers, got {:?}",
            confirmation.headers
        );
        let posted_message_id = confirmation
            .header("x-amz-sns-message-id")
            .unwrap_or_default()
            .to_string();
        assert!(
            !posted_message_id.is_empty(),
            "case {case:?}: expected a message-id header on the confirmation, got {:?}",
            confirmation.headers
        );
        let document = confirmation.document(case);
        assert_eq!(
            (
                document["Type"].as_str(),
                document["TopicArn"].as_str(),
                document["MessageId"].as_str(),
            ),
            (
                Some("SubscriptionConfirmation"),
                Some(topic.as_str()),
                Some(posted_message_id.as_str()),
            ),
            "case {case:?}: expected the confirmation document of {topic:?}, got {document}"
        );
        let token = document["Token"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no Token in {document}"))
            .to_string();
        let subscribe_url = document["SubscribeURL"].as_str().unwrap_or_default();
        assert!(
            subscribe_url.contains("Action=ConfirmSubscription") && subscribe_url.contains(&token),
            "case {case:?}: expected a SubscribeURL presenting the token, got {subscribe_url:?}"
        );
        assert!(
            document["Message"]
                .as_str()
                .unwrap_or_default()
                .contains(&topic),
            "case {case:?}: expected the confirmation's Message to name the topic, got {document}"
        );
        assert!(
            document["Signature"].as_str().is_some()
                && document["SigningCertURL"].as_str().is_some(),
            "case {case:?}: expected the confirmation to be signed, got {document}"
        );

        // The token the endpoint was POSTed is the one that confirms the subscription.
        let confirmed = sns(
            &emulator,
            "ConfirmSubscription",
            &[("TopicArn", &topic), ("Token", &token)],
        )
        .await;
        assert_eq!(
            element(&confirmed, "SubscriptionArn"),
            subscription,
            "case {case:?}: expected the POSTed token to confirm {subscription:?}, got {confirmed}"
        );

        if let Some(retries) = row.retries {
            set_attribute(
                &emulator,
                &subscription,
                "DeliveryPolicy",
                &retry_policy(retries),
            )
            .await;
        }
        let dead_letter_url = if row.redrive {
            let (url, arn) = given_queue(&emulator, &format!("https-dlq-{index}")).await;
            set_attribute(
                &emulator,
                &subscription,
                "RedrivePolicy",
                &json!({"deadLetterTargetArn": arn}).to_string(),
            )
            .await;
            Some(url)
        } else {
            None
        };
        if row.raw {
            set_attribute(&emulator, &subscription, "RawMessageDelivery", "true").await;
        }

        let message_id = publish(&emulator, &topic, &[("Message", row.message)]).await;
        let notifications = recorded_for(&emulator, &capture_url, row.path, row.posts).await;
        assert_eq!(
            notifications.len(),
            row.posts,
            "case {case:?}: expected {} POST(s) of one publish, got {}",
            row.posts,
            notifications.len()
        );
        for notification in &notifications {
            assert_eq!(
                (
                    notification.header("x-amz-sns-message-type"),
                    notification.header("x-amz-sns-message-id"),
                    notification.header("x-amz-sns-topic-arn"),
                    notification.header("x-amz-sns-subscription-arn"),
                    notification.header("content-type"),
                ),
                (
                    Some("Notification"),
                    Some(message_id.as_str()),
                    Some(topic.as_str()),
                    Some(subscription.as_str()),
                    Some(DELIVERY_CONTENT_TYPE),
                ),
                "case {case:?}: expected the notification's headers of {message_id:?}, got {:?}",
                notification.headers
            );
            if row.raw {
                // A raw delivery POSTs the published body ITSELF, with nothing added, and
                // says so in the request so an endpoint can tell it from an envelope.
                assert_eq!(
                    notification.header(RAW_DELIVERY_HEADER),
                    Some("true"),
                    "case {case:?}: expected the raw-delivery header, got {:?}",
                    notification.headers
                );
                assert_eq!(
                    notification.body, row.message,
                    "case {case:?}: expected the raw body {:?}, got {:?}",
                    row.message, notification.body
                );
                continue;
            }
            assert_eq!(
                notification.header(RAW_DELIVERY_HEADER),
                None,
                "case {case:?}: an enveloped delivery carries no raw-delivery header, got {:?}",
                notification.headers
            );
            let document = notification.document(case);
            assert_eq!(
                (
                    document["Type"].as_str(),
                    document["MessageId"].as_str(),
                    document["TopicArn"].as_str(),
                    document["Message"].as_str(),
                ),
                (
                    Some("Notification"),
                    Some(message_id.as_str()),
                    Some(topic.as_str()),
                    Some(row.message),
                ),
                "case {case:?}: expected the notification of {message_id:?}, got {document}"
            );
            assert!(
                document["UnsubscribeURL"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(&subscription),
                "case {case:?}: expected the notification's own unsubscribe link, got {document}"
            );
        }

        let Some(dead_letter_url) = dead_letter_url else {
            continue;
        };
        let letters = held(&emulator, &dead_letter_url, row.dead_letters).await;
        assert_eq!(
            letters.len(),
            row.dead_letters,
            "case {case:?}: expected {} dead letter(s), got {letters:?}",
            row.dead_letters
        );
        for letter in &letters {
            let document: Value = serde_json::from_str(letter).unwrap_or_else(|error| {
                panic!("case {case:?}: the dead letter must be the notification: {error}")
            });
            assert_eq!(
                (
                    document["MessageId"].as_str(),
                    document["Message"].as_str(),
                ),
                (Some(message_id.as_str()), Some(row.message)),
                "case {case:?}: expected the undeliverable notification of {message_id:?}, got \
                 {document}"
            );
        }
    }
}

/// A subscription still awaiting confirmation is sent the request again by a repeated
/// `Subscribe`, so a subscriber that did not receive the first one can ask for another — and
/// the token it carries is still the one that confirms that same subscription.
#[tokio::test]
async fn a_repeated_subscribe_posts_the_confirmation_again() {
    let emulator = emulator();
    let (capture_url, _capture_arn) = given_queue(&emulator, "https-recapture").await;
    let function = "https-reconfirmation-sink";
    given_function_running(
        &emulator,
        function,
        &SINK_HANDLER.replace("{queue_url}", &capture_url),
    )
    .await;
    let sink = given_function_url(&emulator, function).await;
    let topic = element(
        &sns(&emulator, "CreateTopic", &[("Name", "https-reconfirm")]).await,
        "TopicArn",
    );
    let path = "/take/again";
    let endpoint = format!("{sink}{path}");

    let subscription = subscribe(&emulator, &topic, HTTPS_PROTOCOL, &endpoint).await;
    let again = subscribe(&emulator, &topic, HTTPS_PROTOCOL, &endpoint).await;
    assert_eq!(
        again, subscription,
        "a repeated Subscribe must answer the same subscription, got {again:?}"
    );

    let requests = recorded_for(&emulator, &capture_url, path, 2).await;
    assert_eq!(
        requests.len(),
        2,
        "expected one SubscriptionConfirmation POST per Subscribe, got {}",
        requests.len()
    );
    let tokens: Vec<String> = requests
        .iter()
        .map(|request| {
            request.document("a repeated Subscribe")["Token"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(
        tokens[0], tokens[1],
        "expected the same token in both confirmation requests, got {tokens:?}"
    );
    let ids: Vec<Option<&str>> = requests
        .iter()
        .map(|request| request.header("x-amz-sns-message-id"))
        .collect();
    assert_ne!(
        ids[0], ids[1],
        "expected each confirmation request to be its own message, got {ids:?}"
    );

    let confirmed = sns(
        &emulator,
        "ConfirmSubscription",
        &[("TopicArn", &topic), ("Token", &tokens[1])],
    )
    .await;
    assert_eq!(
        element(&confirmed, "SubscriptionArn"),
        subscription,
        "expected the re-sent token to confirm {subscription:?}, got {confirmed}"
    );

    // And a CONFIRMED subscription is sent nothing further: there is nothing left to confirm.
    let third = subscribe(&emulator, &topic, HTTPS_PROTOCOL, &endpoint).await;
    assert_eq!(
        third, subscription,
        "a Subscribe on a confirmed subscription must answer it, got {third:?}"
    );
    let after = drain_recorded(&emulator, &capture_url).await;
    assert!(
        after.is_empty(),
        "expected no further confirmation request, got {after:?}"
    );
}

/// The endpoint a subscription names may be the address the emulator announces for a data
/// plane it serves ITSELF — its own authority followed by the reserved path that names the
/// receiver behind it — rather than the public host of that receiver: a subscription is POSTed
/// to the URL it was given, whichever of the two forms that URL is in.
#[tokio::test]
async fn an_endpoint_behind_the_emulators_own_authority_is_posted_to() {
    let emulator = emulator();
    let (capture_url, _capture_arn) = given_queue(&emulator, "https-local-capture").await;
    let function = "https-local-sink";
    given_function_running(
        &emulator,
        function,
        &SINK_HANDLER.replace("{queue_url}", &capture_url),
    )
    .await;
    let sink = given_function_url(&emulator, function).await;
    // The same receiver, addressed the other way: the url id the ANNOUNCED address names —
    // in the path of the address a function URL is published at
    // (`https://<host:port>/_aws/lambda-url/<url-id>/`), and in the first host label of the
    // documented form — behind the reserved data-plane path of a loopback authority.
    let url_id = sink
        .split('/')
        .skip_while(|segment| *segment != "lambda-url")
        .nth(1)
        .filter(|segment| !segment.is_empty())
        .or_else(|| {
            sink.strip_prefix("https://")
                .and_then(|rest| rest.split('.').next())
        })
        .unwrap_or_else(|| panic!("the announced function URL must name a url id, got {sink:?}"));
    let path = format!("/_aws/lambda-url/{url_id}/take/local");
    let endpoint = format!("https://127.0.0.1:9297{path}");

    let topic = element(
        &sns(&emulator, "CreateTopic", &[("Name", "https-local")]).await,
        "TopicArn",
    );
    let subscription = subscribe(&emulator, &topic, HTTPS_PROTOCOL, &endpoint).await;
    let requests = recorded_for(&emulator, &capture_url, "/take/local", 1).await;
    assert_eq!(
        requests.len(),
        1,
        "expected the confirmation request at the emulator's own authority, got {}",
        requests.len()
    );
    let document = requests[0].document("an endpoint behind the emulator's own authority");
    let token = document["Token"]
        .as_str()
        .unwrap_or_else(|| panic!("no Token in {document}"))
        .to_string();
    let confirmed = sns(
        &emulator,
        "ConfirmSubscription",
        &[("TopicArn", &topic), ("Token", &token)],
    )
    .await;
    assert_eq!(
        element(&confirmed, "SubscriptionArn"),
        subscription,
        "expected that request's token to confirm {subscription:?}, got {confirmed}"
    );

    let message = "the body for a locally addressed endpoint";
    let message_id = publish(&emulator, &topic, &[("Message", message)]).await;
    let notifications = recorded_for(&emulator, &capture_url, "/take/local", 1).await;
    assert_eq!(
        notifications.len(),
        1,
        "expected one notification POST, got {}",
        notifications.len()
    );
    let document = notifications[0].document("an endpoint behind the emulator's own authority");
    assert_eq!(
        (
            notifications[0].header("x-amz-sns-message-id"),
            document["Message"].as_str(),
        ),
        (Some(message_id.as_str()), Some(message)),
        "expected the notification of {message_id:?}, got {document}"
    );
}
/// An endpoint that cannot take the request YET still receives it: the confirmation request is
/// attempted again along the phases the published delivery policy opens with, exactly as every
/// other delivery of this crate retries that answer.
///
/// The endpoint here is a function still inside the activation window its own `State` reports
/// — "During this time, you can't invoke or modify the function" (`CreateFunction`) — which is
/// the one condition a fresh receiver really meets: a client stands its webhook up and
/// subscribes it in the same breath.
#[tokio::test]
async fn an_endpoint_that_is_not_up_yet_still_receives_the_confirmation() {
    let emulator = emulator();
    let (capture_url, _capture_arn) = given_queue(&emulator, "https-pending-capture").await;
    let function = "https-pending-sink";
    given_function_running(
        &emulator,
        function,
        &SINK_HANDLER.replace("{queue_url}", &capture_url),
    )
    .await;
    let sink = given_function_url(&emulator, function).await;
    let topic = element(
        &sns(&emulator, "CreateTopic", &[("Name", "https-pending")]).await,
        "TopicArn",
    );
    let path = "/take/pending";

    // The function is put back inside its activation window, which is where a receiver
    // created moments before the subscription really is. The window is far shorter than the
    // retry schedule, so what the case turns on is that the request is attempted again at all.
    let window = Duration::from_millis(400);
    {
        let state = emulator
            .engine
            .state::<ComAmazonawsLambdaAwsgirapiserviceState>();
        let mut store = state.lock();
        for record in store.functions.values_mut() {
            record.state = "Pending".to_string();
            record.active_due = SystemTime::now() + window;
        }
    }

    let subscription = subscribe(&emulator, &topic, HTTPS_PROTOCOL, &format!("{sink}{path}")).await;
    let requests = recorded_for(&emulator, &capture_url, path, 1).await;
    assert_eq!(
        requests.len(),
        1,
        "expected the confirmation request to reach an endpoint that was not up yet, got {}",
        requests.len()
    );
    let document = requests[0].document("an endpoint that is not up yet");
    let token = document["Token"]
        .as_str()
        .unwrap_or_else(|| panic!("no Token in {document}"))
        .to_string();
    let confirmed = sns(
        &emulator,
        "ConfirmSubscription",
        &[("TopicArn", &topic), ("Token", &token)],
    )
    .await;
    assert_eq!(
        element(&confirmed, "SubscriptionArn"),
        subscription,
        "expected the retried request's token to confirm {subscription:?}, got {confirmed}"
    );
}

/// The `UnsubscribeConfirmation` POST, and what its token does.
///
/// RULE: an `Unsubscribe` whose requester is NOT the subscription's owner — "if the
/// Unsubscribe call does not require authentication and the requester is not the subscription
/// owner, a final cancellation message is delivered to the endpoint, so that the endpoint
/// owner can easily resubscribe to the topic if the Unsubscribe request was unintended"
/// (`Unsubscribe`) — has the cancelled subscription's endpoint POSTed that message, whose
/// `Token` `ConfirmSubscription` restores the subscription with ("if the token is valid, the
/// action creates a new subscription"); an `Unsubscribe` the owner SIGNED delivers none. And a
/// subscription that REQUIRES authentication for deletion — "disallows unauthenticated
/// unsubscribes of the subscription" (`ConfirmSubscriptionInput$AuthenticateOnUnsubscribe`) —
/// is not deleted by such a call at all: it is refused, and nothing is delivered.
///
/// The rows differ along what the rule generalises over: whether the call presented a
/// credential, whether the subscription requires authentication for deletion, and whether the
/// subscription's endpoint is a URL at all — a subscriber reached another way is sent no such
/// message, because there is no request to make.
struct Ending {
    case: &'static str,
    /// The subscription's protocol, which decides what kind of endpoint it has.
    protocol: &'static str,
    /// The path on the sink this row's endpoint addresses, for the rows whose endpoint is a
    /// URL — what tells one row's recorded requests from another's.
    path: &'static str,
    /// Whether the `Unsubscribe` presented a credential.
    signed: bool,
    /// Whether the subscription was confirmed asking that its unsubscribes be
    /// authenticated.
    authenticated_unsubscribes: bool,
    /// The status the `Unsubscribe` must be answered with.
    status: u16,
    /// How many final cancellations the endpoint must then have recorded.
    cancellations: usize,
}

const ENDINGS: &[Ending] = &[
    Ending {
        case: "an unsubscribe that presented no credential",
        protocol: HTTPS_PROTOCOL,
        path: "/take/cancelled",
        signed: false,
        authenticated_unsubscribes: false,
        status: 200,
        cancellations: 1,
    },
    Ending {
        case: "an unsubscribe the owner signed",
        protocol: HTTPS_PROTOCOL,
        path: "/take/owned",
        signed: true,
        authenticated_unsubscribes: false,
        status: 200,
        cancellations: 0,
    },
    Ending {
        // The same unauthenticated call on a subscription whose owner asked that its
        // unsubscribes be authenticated: the call deletes nothing and is refused.
        case: "an unsubscribe that presented no credential where the subscription requires one",
        protocol: HTTPS_PROTOCOL,
        path: "/take/protected",
        signed: false,
        authenticated_unsubscribes: true,
        status: 403,
        cancellations: 0,
    },
    Ending {
        // The OWNER's own call on the same kind of subscription still deletes it.
        case: "an unsubscribe the owner signed where the subscription requires one",
        protocol: HTTPS_PROTOCOL,
        path: "/take/protected-owned",
        signed: true,
        authenticated_unsubscribes: true,
        status: 200,
        cancellations: 0,
    },
    Ending {
        // The same unauthenticated call on a subscription whose endpoint is a QUEUE: there is
        // no URL to POST to, so nothing is sent anywhere.
        case: "an unsubscribe that presented no credential on a queue subscription",
        protocol: "sqs",
        path: "/take/queued",
        signed: false,
        authenticated_unsubscribes: false,
        status: 200,
        cancellations: 0,
    },
];

#[tokio::test]
async fn only_an_unauthenticated_unsubscribe_posts_the_final_cancellation() {
    let emulator = emulator();
    let (capture_url, _capture_arn) = given_queue(&emulator, "https-cancel-capture").await;
    let function = "https-cancellation-sink";
    given_function_running(
        &emulator,
        function,
        &SINK_HANDLER.replace("{queue_url}", &capture_url),
    )
    .await;
    let sink = given_function_url(&emulator, function).await;

    for (index, row) in ENDINGS.iter().enumerate() {
        let case = row.case;
        let topic = element(
            &sns(
                &emulator,
                "CreateTopic",
                &[("Name", &format!("https-cancellation-{index}"))],
            )
            .await,
            "TopicArn",
        );
        // A URL subscription confirms through the token its endpoint is POSTed; a queue
        // subscription is confirmed by SNS itself, so it has no token of its own.
        let (subscription, endpoint) = if row.protocol == HTTPS_PROTOCOL {
            let endpoint = format!("{sink}{}", row.path);
            let subscription = subscribe(&emulator, &topic, row.protocol, &endpoint).await;
            let requests = recorded_for(&emulator, &capture_url, row.path, 1).await;
            assert_eq!(
                requests.len(),
                1,
                "case {case:?}: expected the confirmation request, got {requests:?}"
            );
            let token = requests[0].document(case)["Token"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no Token in {:?}", requests[0].body))
                .to_string();
            let mut confirming = vec![("TopicArn", topic.as_str()), ("Token", token.as_str())];
            if row.authenticated_unsubscribes {
                confirming.push(("AuthenticateOnUnsubscribe", "true"));
            }
            sns(&emulator, "ConfirmSubscription", &confirming).await;
            (subscription, endpoint)
        } else {
            let (_url, arn) = given_queue(&emulator, &format!("https-cancel-queue-{index}")).await;
            (
                subscribe(&emulator, &topic, row.protocol, &arn).await,
                arn,
            )
        };

        let (status, answered) = if row.signed {
            sns_signed(
                &emulator,
                "Unsubscribe",
                &[("SubscriptionArn", &subscription)],
            )
            .await
        } else {
            sns_unsigned(
                &emulator,
                "Unsubscribe",
                &[("SubscriptionArn", &subscription)],
            )
            .await
        };
        assert_eq!(
            status, row.status,
            "case {case:?}: expected the unsubscribe of {endpoint:?} to be answered {}, got \
             {status} / {answered}",
            row.status
        );
        if row.status != 200 {
            // The refusal says what it is: the code and the sentence Amazon SNS answers an
            // unsubscribe of a subscription that requires authentication for deletion with.
            assert_eq!(
                (
                    element(&answered, "Code").as_str(),
                    element(&answered, "Message").as_str(),
                ),
                (
                    "AuthorizationError",
                    "This subscription may only be deleted by an authorized user.",
                ),
                "case {case:?}: expected the refusal of an unauthenticated unsubscribe, got \
                 {answered}"
            );
            // A refused unsubscribe deletes nothing: the subscription still receives what is
            // published to its topic.
            let message = "the body after a refused unsubscribe";
            let message_id = publish(&emulator, &topic, &[("Message", message)]).await;
            let delivered = recorded_for(&emulator, &capture_url, row.path, 1).await;
            assert_eq!(
                delivered.len(),
                1,
                "case {case:?}: a refused unsubscribe leaves the subscription in place, so it \
                 is POSTed to, got {delivered:?}"
            );
            assert_eq!(
                delivered[0].header("x-amz-sns-message-id"),
                Some(message_id.as_str()),
                "case {case:?}: expected the notification of {message_id:?}, got {:?}",
                delivered[0].headers
            );
            continue;
        }

        // The POST an unsubscribe owes is made before it answers, so what the endpoint has
        // recorded by now is the whole of what this call sent it.
        let cancellations: Vec<Recorded> = drain_recorded(&emulator, &capture_url)
            .await
            .into_iter()
            .filter(|recorded| recorded.path == row.path)
            .collect();
        assert_eq!(
            cancellations.len(),
            row.cancellations,
            "case {case:?}: expected {} final cancellation(s), got {cancellations:?}",
            row.cancellations
        );
        let Some(cancellation) = cancellations.first() else {
            continue;
        };
        let posted_message_id = cancellation
            .header("x-amz-sns-message-id")
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            (
                cancellation.header("x-amz-sns-message-type"),
                cancellation.header("x-amz-sns-topic-arn"),
                cancellation.header("x-amz-sns-subscription-arn"),
                cancellation.header("content-type"),
            ),
            (
                Some("UnsubscribeConfirmation"),
                Some(topic.as_str()),
                Some(subscription.as_str()),
                Some(DELIVERY_CONTENT_TYPE),
            ),
            "case {case:?}: expected the cancellation's headers, got {:?}",
            cancellation.headers
        );
        assert!(
            !posted_message_id.is_empty(),
            "case {case:?}: expected a message-id header, got {:?}",
            cancellation.headers
        );
        let document = cancellation.document(case);
        assert_eq!(
            (
                document["Type"].as_str(),
                document["TopicArn"].as_str(),
                document["MessageId"].as_str(),
            ),
            (
                Some("UnsubscribeConfirmation"),
                Some(topic.as_str()),
                Some(posted_message_id.as_str()),
            ),
            "case {case:?}: expected the cancellation document of {topic:?}, got {document}"
        );
        assert!(
            document["Message"]
                .as_str()
                .unwrap_or_default()
                .contains(&subscription),
            "case {case:?}: expected the cancellation's Message to name the subscription it \
             deactivated, got {document}"
        );
        assert!(
            document["Signature"].as_str().is_some()
                && document["SigningCertURL"].as_str().is_some(),
            "case {case:?}: expected the cancellation to be signed, got {document}"
        );
        let token = document["Token"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no Token in {document}"))
            .to_string();
        let subscribe_url = document["SubscribeURL"].as_str().unwrap_or_default();
        assert!(
            subscribe_url.contains("Action=ConfirmSubscription") && subscribe_url.contains(&token),
            "case {case:?}: expected a SubscribeURL presenting the token, got {subscribe_url:?}"
        );

        // The subscription is really gone: a publish made now is delivered nowhere.
        publish(&emulator, &topic, &[("Message", "after the cancellation")]).await;
        let withheld = drain_recorded(&emulator, &capture_url).await;
        assert!(
            withheld.is_empty(),
            "case {case:?}: a cancelled subscription receives nothing, got {withheld:?}"
        );

        // And the token restores it: the subscription the cancellation named is confirmed
        // again, and the next publish reaches its endpoint.
        let restored = sns(
            &emulator,
            "ConfirmSubscription",
            &[("TopicArn", &topic), ("Token", &token)],
        )
        .await;
        assert_eq!(
            element(&restored, "SubscriptionArn"),
            subscription,
            "case {case:?}: expected the cancellation's token to restore {subscription:?}, got \
             {restored}"
        );
        let message = "after the restoration";
        let message_id = publish(&emulator, &topic, &[("Message", message)]).await;
        let delivered = recorded_for(&emulator, &capture_url, row.path, 1).await;
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: expected the restored subscription to be POSTed to, got {delivered:?}"
        );
        let document = delivered[0].document(case);
        assert_eq!(
            (
                delivered[0].header("x-amz-sns-message-id"),
                document["Message"].as_str(),
            ),
            (Some(message_id.as_str()), Some(message)),
            "case {case:?}: expected the notification of {message_id:?}, got {document}"
        );
    }
}

/// The LINKS a subscriber is handed lead to the calls they write.
///
/// RULE: the URL members SNS delivers to an endpoint are addresses its owner FOLLOWS — "to
/// confirm the subscription, visit the SubscribeURL included in this message", and the
/// `UnsubscribeURL` a notification carries — so following one performs the call it writes,
/// exactly as posting that call does: the `SubscribeURL` of a confirmation request confirms
/// the subscription, and the `UnsubscribeURL` of a notification ends it, which is the
/// unauthenticated unsubscribe a final cancellation is delivered for.
///
/// The rows this spans are the three links one subscription is handed, each followed in turn:
/// its confirmation's `SubscribeURL`, its notification's `UnsubscribeURL`, and the
/// cancellation's own `SubscribeURL`, which restores what the second ended.
#[tokio::test]
async fn the_links_a_subscriber_is_handed_perform_the_calls_they_write() {
    let emulator = emulator();
    let (capture_url, _capture_arn) = given_queue(&emulator, "https-link-capture").await;
    let function = "https-link-sink";
    given_function_running(
        &emulator,
        function,
        &SINK_HANDLER.replace("{queue_url}", &capture_url),
    )
    .await;
    let sink = given_function_url(&emulator, function).await;
    let topic = element(
        &sns(&emulator, "CreateTopic", &[("Name", "https-links")]).await,
        "TopicArn",
    );
    let path = "/take/links";
    let subscription = subscribe(
        &emulator,
        &topic,
        HTTPS_PROTOCOL,
        &format!("{sink}{path}"),
    )
    .await;

    // The confirmation's SubscribeURL: following it confirms the subscription, which is what
    // the sentence that request states tells the endpoint's owner to do.
    let confirmations = recorded_for(&emulator, &capture_url, path, 1).await;
    assert_eq!(
        confirmations.len(),
        1,
        "expected the confirmation request, got {confirmations:?}"
    );
    let subscribe_url = confirmations[0].document("a visited SubscribeURL")["SubscribeURL"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !subscribe_url.is_empty(),
        "expected a SubscribeURL in the confirmation request, got {:?}",
        confirmations[0].body
    );
    let (status, answered) = fetch(&emulator, &subscribe_url).await;
    assert_eq!(
        status, 200,
        "expected following the SubscribeURL to confirm the subscription, got {status} / \
         {answered}"
    );
    assert_eq!(
        element(&answered, "SubscriptionArn"),
        subscription,
        "expected the visited SubscribeURL to answer {subscription:?}, got {answered}"
    );

    // A confirmed subscription receives notifications, each carrying the link that ends it.
    let message = "the body of a link-confirmed subscription";
    let message_id = publish(&emulator, &topic, &[("Message", message)]).await;
    let notifications = recorded_for(&emulator, &capture_url, path, 1).await;
    assert_eq!(
        notifications.len(),
        1,
        "expected one notification POST, got {notifications:?}"
    );
    let document = notifications[0].document("a visited UnsubscribeURL");
    assert_eq!(
        (
            notifications[0].header("x-amz-sns-message-id"),
            document["Message"].as_str(),
        ),
        (Some(message_id.as_str()), Some(message)),
        "expected the notification of {message_id:?}, got {document}"
    );
    let unsubscribe_url = document["UnsubscribeURL"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        unsubscribe_url.contains(&subscription),
        "expected the notification's own unsubscribe link, got {unsubscribe_url:?}"
    );

    // Following it ends the subscription — and, because whoever followed it authenticated
    // nobody, the endpoint is sent the final cancellation that can restore it.
    let (status, answered) = fetch(&emulator, &unsubscribe_url).await;
    assert_eq!(
        status, 200,
        "expected following the UnsubscribeURL to end the subscription, got {status} / \
         {answered}"
    );
    let cancellations = recorded_for(&emulator, &capture_url, path, 1).await;
    assert_eq!(
        cancellations.len(),
        1,
        "expected the final cancellation of an unauthenticated unsubscribe, got \
         {cancellations:?}"
    );
    let cancellation = cancellations[0].document("a visited UnsubscribeURL");
    assert_eq!(
        (
            cancellations[0].header("x-amz-sns-message-type"),
            cancellations[0].header("x-amz-sns-subscription-arn"),
            cancellation["Type"].as_str(),
        ),
        (
            Some("UnsubscribeConfirmation"),
            Some(subscription.as_str()),
            Some("UnsubscribeConfirmation"),
        ),
        "expected the cancellation of {subscription:?}, got {cancellation}"
    );

    // And the cancellation's own SubscribeURL restores it: the subscription receives the next
    // publish again.
    let restore_url = cancellation["SubscribeURL"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let (status, answered) = fetch(&emulator, &restore_url).await;
    assert_eq!(
        status, 200,
        "expected following the cancellation's SubscribeURL to restore the subscription, got \
         {status} / {answered}"
    );
    assert_eq!(
        element(&answered, "SubscriptionArn"),
        subscription,
        "expected the restored subscription {subscription:?}, got {answered}"
    );
    let message = "the body after a restored subscription";
    let message_id = publish(&emulator, &topic, &[("Message", message)]).await;
    let delivered = recorded_for(&emulator, &capture_url, path, 1).await;
    assert_eq!(
        delivered.len(),
        1,
        "expected the restored subscription to be POSTed to, got {delivered:?}"
    );
    let document = delivered[0].document("a restored subscription");
    assert_eq!(
        (
            delivered[0].header("x-amz-sns-message-id"),
            document["Message"].as_str(),
        ),
        (Some(message_id.as_str()), Some(message)),
        "expected the notification of {message_id:?}, got {document}"
    );
}

/// The handler behind the function URL the retry-schedule case subscribes: it records every
/// request it was sent on the capture queue, exactly as [`SINK_HANDLER`] does, and then answers
/// from a queue of REFUSALS the case pre-loads — one message per answer of "cannot take this
/// yet" it owes, each consumed by the request it refuses, so the n+1st request it is sent is
/// the first it takes. Nothing here reads a clock: how many requests it refuses is state the
/// case wrote.
const UNREADY_SINK_HANDLER: &str = r#"
import base64
import json

import boto3


def lambda_handler(event, context):
    headers = event.get("headers") or {}
    path = event.get("rawPath") or "/"
    body = event.get("body") or ""
    if event.get("isBase64Encoded"):
        body = base64.b64decode(body).decode("utf-8")
    sqs = boto3.client("sqs")
    sqs.send_message(
        QueueUrl="{queue_url}",
        MessageBody=json.dumps({"path": path, "headers": headers, "body": body}),
    )
    owed = sqs.receive_message(QueueUrl="{refusals_url}", MaxNumberOfMessages=1)
    for message in owed.get("Messages") or []:
        sqs.delete_message(
            QueueUrl="{refusals_url}", ReceiptHandle=message["ReceiptHandle"]
        )
        return {"statusCode": 503, "body": "the sink cannot take this yet"}
    return {"statusCode": 200, "body": "taken"}
"#;

/// The retry policy the TOPIC states for its `http` subscriptions in the case below, and so the
/// `healthyRetryPolicy` of the effective delivery policy every subscription of it runs under:
/// two retries, each waiting `minDelayTarget`.
///
/// The delay is longer than what ONE call has left to spend waiting
/// (`ops::publish::MAX_RETRY_WAIT_PER_REQUEST`) once the opening phases have been waited out,
/// so these two retries are exactly the attempts that can only be made after the subscribing
/// call has answered — which is what the case is about — while staying short enough that
/// waiting for them costs a few seconds.
const UNREADY_RETRY_POLICY_DELAY_SECONDS: u64 = 4;
const UNREADY_RETRY_POLICY_RETRIES: usize = 2;

/// How many attempts the opening phases of the published policy make: the first POST, three
/// immediate retries and two a second apart (`ops::publish::retry_waits`), all of them inside
/// the call that owes the request.
const OPENING_ATTEMPTS: usize = 6;

/// How many POSTs one confirmation request may cost in total: the opening phases plus the
/// retries the subscription's own policy states.
const SCHEDULED_ATTEMPTS: usize = OPENING_ATTEMPTS + UNREADY_RETRY_POLICY_RETRIES;

/// Everything `path` records over the span of a WHOLE schedule of the same policy, run from
/// now: how a case concludes that a schedule is EXHAUSTED without waiting out a span of host
/// seconds.
///
/// An absence has no condition of its own to look for, so this waits for one the EMULATOR
/// states: a WITNESS subscription over the same topic policy, to an endpoint refused until the
/// last retry that policy allows, so its confirmation costs the whole schedule
/// ([`SCHEDULED_ATTEMPTS`] POSTs) — and the wait looks until the witness has been POSTed to that
/// many times. The witness is subscribed after the row's last POST, so the whole span its own
/// schedule spends lies after that POST, and a further attempt of the row — due one retry delay
/// after its last, which is the shortest step the policy states — falls inside that span and is
/// collected here rather than missed. Under load the witness's attempts stretch exactly as the
/// row's would, which no number of seconds written here does.
///
/// `index` tells this witness's topic and path from another row's. The witness is owed exactly
/// the refusals its own schedule's last retry is reached by, on a queue emptied first, so what it
/// is handed does not depend on what the calling row left there; the deadline bounds only the
/// case where the witness itself was never POSTed to as the policy states, which this reports as
/// the failure it is rather than as an absence.
async fn recorded_while_a_whole_schedule_runs(
    emulator: &Emulator,
    capture_url: &str,
    refusals_url: &str,
    sink: &str,
    index: usize,
    path: &str,
) -> Vec<Recorded> {
    let witness_path = format!("/unready/witness-{index}");
    consumed(emulator, refusals_url).await;
    for owed in 0..SCHEDULED_ATTEMPTS - 1 {
        sqs(
            emulator,
            "SendMessage",
            &json!({
                "QueueUrl": refusals_url,
                "MessageBody": format!("witness-{index}:{owed}"),
            }),
        )
        .await;
    }
    let topic = element(
        &sns(
            emulator,
            "CreateTopic",
            &[("Name", &format!("https-unready-witness-{index}"))],
        )
        .await,
        "TopicArn",
    );
    sns(
        emulator,
        "SetTopicAttributes",
        &[
            ("TopicArn", &topic),
            ("AttributeName", "DeliveryPolicy"),
            ("AttributeValue", &unready_topic_policy()),
        ],
    )
    .await;
    subscribe(
        emulator,
        &topic,
        HTTPS_PROTOCOL,
        &format!("{sink}{witness_path}"),
    )
    .await;

    let deadline = Instant::now() + DEADLINE;
    let mut collected: Vec<Recorded> = Vec::new();
    let mut witnessed = 0;
    loop {
        for recorded in drain_recorded(emulator, capture_url).await {
            if recorded.path == witness_path {
                witnessed += 1;
            } else if recorded.path == path {
                collected.push(recorded);
            }
        }
        if witnessed >= SCHEDULED_ATTEMPTS {
            return collected;
        }
        assert!(
            Instant::now() < deadline,
            "case {witness_path:?}: expected the witness subscription to be POSTed to \
             {SCHEDULED_ATTEMPTS} times, so that a whole schedule is known to have run, it was \
             POSTed to {witnessed} times while {path:?} recorded {} further POST(s)",
            collected.len()
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The topic-level delivery policy the case writes.
fn unready_topic_policy() -> String {
    json!({
        "http": {
            "defaultHealthyRetryPolicy": {
                "numRetries": UNREADY_RETRY_POLICY_RETRIES,
                "minDelayTarget": UNREADY_RETRY_POLICY_DELAY_SECONDS,
                "maxDelayTarget": UNREADY_RETRY_POLICY_DELAY_SECONDS,
                "numNoDelayRetries": 0,
                "numMinDelayRetries": 0,
                "numMaxDelayRetries": 0,
                "backoffFunction": "linear",
            },
        },
    })
    .to_string()
}

/// One row: how many times the endpoint answers that it cannot take the confirmation yet, and
/// how many POSTs the request then costs.
struct Unready {
    case: &'static str,
    /// The path on the sink this row's subscription addresses, which tells its recorded
    /// requests from another row's.
    path: &'static str,
    /// How many requests the endpoint refuses with a server error before taking one.
    refusals: usize,
    /// How many POSTs the endpoint must have recorded for this row.
    posts: usize,
    /// Whether the endpoint ever TOOK the request, which is whether its subscriber holds a
    /// token it can confirm the subscription with.
    taken: bool,
}

fn unready_rows() -> Vec<Unready> {
    vec![
        Unready {
            case: "an endpoint that takes the confirmation at once",
            path: "/unready/none",
            refusals: 0,
            posts: 1,
            taken: true,
        },
        Unready {
            case: "an endpoint ready by the immediate phase's last retry",
            path: "/unready/immediate",
            refusals: 3,
            posts: 4,
            taken: true,
        },
        Unready {
            case: "an endpoint ready by the last attempt the subscribing call makes",
            path: "/unready/in-call",
            refusals: OPENING_ATTEMPTS - 1,
            posts: OPENING_ATTEMPTS,
            taken: true,
        },
        Unready {
            // The row the schedule has to outlive the call for: every attempt the call itself
            // can make has been refused, and the endpoint takes the NEXT one.
            case: "an endpoint ready only after the subscribing call has answered",
            path: "/unready/after-the-call",
            refusals: OPENING_ATTEMPTS,
            posts: OPENING_ATTEMPTS + 1,
            taken: true,
        },
        Unready {
            case: "an endpoint ready on the schedule's last retry",
            path: "/unready/last-retry",
            refusals: SCHEDULED_ATTEMPTS - 1,
            posts: SCHEDULED_ATTEMPTS,
            taken: true,
        },
        Unready {
            // A policy's schedule is a budget, not a promise: an endpoint that refuses every
            // attempt it states is POSTed to that many times and no more.
            case: "an endpoint that never takes it",
            path: "/unready/never",
            refusals: SCHEDULED_ATTEMPTS + 1,
            posts: SCHEDULED_ATTEMPTS,
            taken: false,
        },
    ]
}

/// RULE: the confirmation request a subscription to a URL starts with is retried along the
/// schedule its delivery policy states — the published opening phases and then the
/// `healthyRetryPolicy` the subscription's `EffectiveDeliveryPolicy` reports — ON THE CLOCK, so
/// an endpoint that answers that it cannot take the request YET is POSTed to again after the
/// subscribing call has answered, until it takes one or that schedule is exhausted.
///
/// The rows differ along what the rule generalises over: how long the endpoint goes on refusing
/// — none at all, within the attempts the call itself makes, exactly one attempt past them, the
/// schedule's last retry, and longer than the whole schedule.
#[tokio::test]
async fn a_confirmation_is_retried_on_the_clock_until_the_endpoint_takes_it() {
    let emulator = emulator();
    let (capture_url, _capture_arn) = given_queue(&emulator, "https-unready-capture").await;
    let (refusals_url, _refusals_arn) = given_queue(&emulator, "https-unready-refusals").await;
    let function = "https-unready-sink";
    given_function_running(
        &emulator,
        function,
        &UNREADY_SINK_HANDLER
            .replace("{queue_url}", &capture_url)
            .replace("{refusals_url}", &refusals_url),
    )
    .await;
    let sink = given_function_url(&emulator, function).await;

    for (index, row) in unready_rows().into_iter().enumerate() {
        let case = row.case;
        // What the endpoint owes in refusals, written before anything is POSTed to it — on a
        // queue emptied first, so a row is handed exactly what it asked for.
        consumed(&emulator, &refusals_url).await;
        for owed in 0..row.refusals {
            sqs(
                &emulator,
                "SendMessage",
                &json!({"QueueUrl": refusals_url, "MessageBody": format!("{case}:{owed}")}),
            )
            .await;
        }
        let topic = element(
            &sns(
                &emulator,
                "CreateTopic",
                &[("Name", &format!("https-unready-{index}"))],
            )
            .await,
            "TopicArn",
        );
        sns(
            &emulator,
            "SetTopicAttributes",
            &[
                ("TopicArn", &topic),
                ("AttributeName", "DeliveryPolicy"),
                ("AttributeValue", &unready_topic_policy()),
            ],
        )
        .await;

        let endpoint = format!("{sink}{}", row.path);
        let subscription = subscribe(&emulator, &topic, HTTPS_PROTOCOL, &endpoint).await;
        let posted = recorded_for(&emulator, &capture_url, row.path, row.posts).await;
        assert_eq!(
            posted.len(),
            row.posts,
            "case {case:?}: expected the confirmation to cost {} POST(s), the endpoint recorded \
             {}",
            row.posts,
            posted.len()
        );
        // Every attempt of ONE request carries that request's own token: what a retry re-sends
        // is the same confirmation rather than a new one.
        let documents: Vec<Value> = posted.iter().map(|request| request.document(case)).collect();
        let tokens: Vec<&str> = documents
            .iter()
            .map(|document| document["Token"].as_str().unwrap_or_default())
            .collect();
        assert!(
            tokens
                .iter()
                .all(|token| Some(*token) == tokens.first().copied())
                && !tokens[0].is_empty(),
            "case {case:?}: expected every attempt to carry the one token, got {tokens:?}"
        );

        // The subscriber can confirm with the token of a request its endpoint TOOK; a row whose
        // endpoint took none is left awaiting confirmation, and nothing further is POSTed to it.
        if row.taken {
            let confirmed = sns(
                &emulator,
                "ConfirmSubscription",
                &[("TopicArn", &topic), ("Token", tokens[0])],
            )
            .await;
            assert_eq!(
                element(&confirmed, "SubscriptionArn"),
                subscription,
                "case {case:?}: expected the POSTed token to confirm {subscription:?}, got \
                 {confirmed}"
            );
        } else {
            // Nothing further may be POSTed, and an absence is concluded from the EMULATOR's
            // clock rather than from this host's: a witness subscription runs a whole schedule
            // of the same policy from here, and everything this row's endpoint recorded while
            // it did is what a schedule that was not exhausted would have POSTed.
            let further = recorded_while_a_whole_schedule_runs(
                &emulator,
                &capture_url,
                &refusals_url,
                &sink,
                index,
                row.path,
            )
            .await;
            assert!(
                further.is_empty(),
                "case {case:?}: expected the schedule to be exhausted after {} POSTs, the \
                 endpoint recorded {} more",
                row.posts,
                further.len()
            );
        }
    }
}

/// RULE: a read of the capture queue CONSUMES what it collected ([`consumed`]), so one recorded
/// request is handed to a case exactly once — however long the case runs and however often it
/// looks again. A read that only RECEIVED would leave the record on the queue, in flight, to be
/// handed out again once its visibility timeout lapsed; a case counting its POSTs would then
/// count one of them twice because the host was slow rather than because the emulator POSTed
/// again.
///
/// The rows differ along what the rule generalises over: how many records are waiting — none,
/// one, a few, and more than one read of the queue carries (`MaxNumberOfMessages` admits at most
/// ten, `ReceiveMessageRequest$MaxNumberOfMessages`).
///
/// What the rule is asserted on is the QUEUE's own account of what is left — the messages waiting
/// and the messages in flight it reports (`GetQueueAttributesResult$Attributes`) — because that
/// is what tells a record that was consumed from one that is merely invisible for now, and it
/// says so without waiting any visibility timeout out.
#[tokio::test]
async fn a_read_of_the_capture_queue_consumes_what_it_collected() {
    let emulator = emulator();
    // (case, how many records are waiting)
    let rows: &[(&str, usize)] = &[
        ("no record at all", 0),
        ("one record", 1),
        ("a few records", 3),
        ("more records than one read of the queue carries", 12),
    ];
    for (index, (case, waiting)) in rows.iter().enumerate() {
        let (url, _arn) = given_queue(&emulator, &format!("https-consumed-{index}")).await;
        for record in 0..*waiting {
            sqs(
                &emulator,
                "SendMessage",
                &json!({
                    "QueueUrl": url,
                    "MessageBody": json!({
                        "path": format!("/consumed/{record}"),
                        "headers": {},
                        "body": "",
                    })
                    .to_string(),
                }),
            )
            .await;
        }

        let collected = drain_recorded(&emulator, &url).await;
        let mut paths: Vec<String> = collected
            .iter()
            .map(|recorded| recorded.path.clone())
            .collect();
        paths.sort();
        let mut expected: Vec<String> = (0..*waiting)
            .map(|record| format!("/consumed/{record}"))
            .collect();
        expected.sort();
        assert_eq!(
            paths, expected,
            "case {case:?}: expected the read to collect {expected:?}, got {paths:?}"
        );

        let left = sqs(
            &emulator,
            "GetQueueAttributes",
            &json!({
                "QueueUrl": url,
                "AttributeNames": [
                    "ApproximateNumberOfMessages",
                    "ApproximateNumberOfMessagesNotVisible",
                ],
            }),
        )
        .await;
        for name in [
            "ApproximateNumberOfMessages",
            "ApproximateNumberOfMessagesNotVisible",
        ] {
            let reported = left["Attributes"][name].as_str().unwrap_or_default();
            assert_eq!(
                reported, "0",
                "case {case:?}: expected the queue to hold nothing once its records were \
                 collected, it reports {name} as {reported:?} in {left}"
            );
        }

        let again = drain_recorded(&emulator, &url).await;
        assert!(
            again.is_empty(),
            "case {case:?}: expected a record collected once to be gone from the queue, a second \
             read collected {} of them: {:?}",
            again.len(),
            again
                .iter()
                .map(|recorded| recorded.path.clone())
                .collect::<Vec<String>>()
        );
    }
}
