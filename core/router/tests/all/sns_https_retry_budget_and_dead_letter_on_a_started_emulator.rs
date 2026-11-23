// SPDX-License-Identifier: CC0-1.0

//! What a FAILING `https` subscription's retry budget and redrive really come to, on the
//! emulator a client STARTS: the built binary, on its own port, with request authentication
//! on, reached over TLS with SigV4-signed calls.
//!
//! The endpoint is a Lambda function URL this same emulator published, whose handler records
//! every request it was sent on an Amazon SQS queue and then REFUSES each notification with a
//! server error — so the POSTs Amazon SNS made are counted from outside the endpoint, exactly
//! as a webhook's owner counts them, and every attempt the subscription's policy budgets is a
//! request that was really made.
//!
//! RULES, both about one subscription to a URL whose endpoint does not take its notifications:
//!
//! * "DeliveryPolicy – The policy that defines how Amazon SNS retries failed deliveries to
//!   HTTP/S endpoints" (`SetSubscriptionAttributesInput$AttributeName`): the attempts ONE
//!   notification costs are one plus the `healthyRetryPolicy`'s `numRetries` of the policy in
//!   force on that subscription — the one `GetSubscriptionAttributes` reports as
//!   `EffectiveDeliveryPolicy` — so a subscription written `numRetries: 0` is POSTed to
//!   exactly once per publish, whichever publish it is, and one written a larger budget is
//!   attempted again for the SAME publish.
//! * "RedrivePolicy – When specified, sends undeliverable messages to the specified Amazon
//!   SQS dead-letter queue. Messages that can't be delivered due to client errors […] are
//!   held in the dead-letter queue for further analysis or reprocessing" (same member): once
//!   those attempts are spent, the notification is held in the queue that policy names, once
//!   per publish, carrying that publish's own `MessageId`.
//!
//! The rows differ along what the rules generalise over: the retry budget the subscription
//! was written (none, one, two), whether a TWIN subscription over another budget shares the
//! very same endpoint, and the address the subscriber was handed — a path of its own under the
//! endpoint, and the published endpoint EXACTLY as `CreateFunctionUrlConfig` announced it,
//! which is the address a client that stood its receiver up a moment earlier actually holds.
//! Each row is published to TWICE, so the attempt count is a function of the configured policy
//! rather than of which publish it was. The dead-letter queue of every row carries the queue
//! policy that admits this service, as a client writes it for a redrive target.
//!
//! The owning crate's own `https_subscription_delivery` suite spans the same rules on a
//! router it assembles in-process with authentication off. The emulator a client runs is a
//! different configuration of the same code: the credentials are verified, each call arrives
//! over its own TLS connection, the notification is POSTed while the client is long-polling
//! the capture queue, and ONE process serves every call — so a retry budget or a redrive that
//! holds in-process and not on the started emulator is a failure this suite reports rather
//! than one only a client sees. The harness that starts the binary is the shared one
//! ([`crate::cfn_deployed_pipeline_on_a_started_emulator::Started`]).

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::{deployed, output, Started};
use crate::query_wire::{block_of, decoded_entries, opt_element};

/// The stack the execution role and the refusing recording function are deployed by.
const STACK: &str = "started-emulator-https-retry-budget";

/// How long work the clock owes is waited for (the stack settling, the function reaching
/// `Active`, a POST reaching the endpoint and its record reaching the queue, a dead letter
/// reaching the redrive target), and how often this suite looks meanwhile. Every wait is on a
/// CONDITION and leaves as soon as it holds.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// How long the client waits on one `ReceiveMessage`, in seconds — "the duration (in seconds)
/// for which the call waits for a message to arrive in the queue before returning"
/// (`ReceiveMessageRequest$WaitTimeSeconds`): the wait a client makes for a delivery it cannot
/// otherwise be told about.
const WAIT_TIME_SECONDS: u64 = 2;

/// The header a recorded request carries the kind of message in.
const MESSAGE_TYPE_HEADER: &str = "x-amz-sns-message-type";

/// The header a recorded request carries the id of the message it is about in.
const MESSAGE_ID_HEADER: &str = "x-amz-sns-message-id";

/// The two kinds of message a subscription to a URL is POSTed here: the request that confirms
/// it, and the notification of a published message.
const CONFIRMATION_TYPE: &str = "SubscriptionConfirmation";
const NOTIFICATION_TYPE: &str = "Notification";

/// The handler behind the function URL: it records the request it was sent — the path, the
/// headers and the body — on the queue its environment names, and then answers from the path
/// it was reached on: a request about the SUBSCRIPTION is taken, unless the path asks for
/// everything to be refused, and every notification is refused with a server error, which is
/// the condition a retry budget and a redrive exist for.
const SINK_HANDLER: &str = r#"import base64
import json
import os

import boto3


def handler(event, context):
    headers = event.get("headers") or {}
    path = event.get("rawPath") or "/"
    body = event.get("body") or ""
    if event.get("isBase64Encoded"):
        body = base64.b64decode(body).decode("utf-8")
    boto3.client("sqs").send_message(
        QueueUrl=os.environ["QUEUE_URL"],
        MessageBody=json.dumps(
            {
                "path": path,
                "headers": headers,
                "body": body,
            }
        ),
    )
    confirmation = headers.get("x-amz-sns-message-type") == "SubscriptionConfirmation"
    if confirmation and not path.startswith("/refuse-all"):
        return {"statusCode": 200, "body": "confirmed"}
    return {"statusCode": 500, "body": "the sink could not take this request"}
"#;

/// `text` as JSON, or null for a body that is not JSON at all.
fn parsed(text: &str) -> Value {
    if text.is_empty() {
        return Value::Null;
    }
    serde_json::from_str(text).unwrap_or(Value::Null)
}

/// One SNS call that must be answered, as the XML text it answered.
fn sns(emulator: &Started, action: &str, members: &[(&str, &str)]) -> String {
    let (status, text) = emulator.form_call("sns", "2010-03-31", action, members);
    assert_eq!(
        status, 200,
        "SNS {action} must answer 200, got {status} with {text}; the log:\n{}",
        emulator.log()
    );
    text
}

/// One Amazon SQS call, which must answer 200.
fn sqs(emulator: &Started, operation: &str, body: &Value) -> Value {
    let (status, text) = emulator.rpc(
        "sqs",
        &format!("AmazonSQS.{operation}"),
        "application/x-amz-json-1.0",
        body,
    );
    let answered = parsed(&text);
    assert_eq!(
        status, 200,
        "Amazon SQS {operation} must answer 200, got {status} with {answered}; the log:\n{}",
        emulator.log()
    );
    answered
}

/// One Lambda `restJson1` call, as its status and parsed body.
fn lambda(emulator: &Started, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
    let payload = body.map(Value::to_string).unwrap_or_default();
    let (status, text) = emulator.call(
        "lambda",
        method,
        path,
        &[("content-type", "application/json")],
        payload.into_bytes(),
    );
    (status, parsed(&text))
}

/// The stack this suite deploys: the execution role the handler's own write is judged against,
/// and the recording function running as it.
fn wiring(queue_url: &str) -> Value {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "SinkRole": {
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
                    // The ONE action the handler itself takes and nothing else.
                    "Policies": [{
                        "PolicyName": "record-what-it-was-sent",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": ["sqs:SendMessage"],
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            "Sink": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["SinkRole", "Arn"] },
                    "Timeout": 30,
                    "Environment": { "Variables": { "QUEUE_URL": queue_url } },
                    "Code": { "ZipFile": SINK_HANDLER },
                },
            },
        },
        "Outputs": { "FunctionName": { "Value": { "Ref": "Sink" } } },
    })
}

/// One request the endpoint recorded.
#[derive(Debug, Clone)]
struct Recorded {
    path: String,
    headers: Value,
    body: String,
}

impl Recorded {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(Value::as_str)
    }

    /// Whether this record is a message of `message_type` POSTed to `path`.
    fn is(&self, path: &str, message_type: &str) -> bool {
        self.path == path && self.header(MESSAGE_TYPE_HEADER) == Some(message_type)
    }

    /// Whether this record is the request that confirms a subscription to `topic`: the
    /// document names the topic, which is what tells one row's confirmation from another's
    /// where two rows are POSTed to the same path.
    fn confirms(&self, topic: &str) -> bool {
        self.header(MESSAGE_TYPE_HEADER) == Some(CONFIRMATION_TYPE)
            && parsed(&self.body)["TopicArn"].as_str() == Some(topic)
    }
}

/// Everything the endpoint has recorded on the capture queue, drained as it arrives: each
/// message is deleted as it is read, so the records are accumulated here instead of being read
/// twice.
struct Captures<'a> {
    emulator: &'a Started,
    url: String,
    seen: Vec<Recorded>,
}

impl<'a> Captures<'a> {
    fn new(emulator: &'a Started, url: &str) -> Self {
        Self {
            emulator,
            url: url.to_string(),
            seen: Vec::new(),
        }
    }

    /// Read whatever the queue holds right now onto [`Self::seen`].
    fn drain(&mut self) {
        let received = sqs(
            self.emulator,
            "ReceiveMessage",
            &json!({
                "QueueUrl": self.url,
                "MaxNumberOfMessages": 10,
                "WaitTimeSeconds": WAIT_TIME_SECONDS,
            }),
        );
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            let body = message["Body"].as_str().unwrap_or_default().to_string();
            sqs(
                self.emulator,
                "DeleteMessage",
                &json!({
                    "QueueUrl": self.url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            );
            let captured = parsed(&body);
            self.seen.push(Recorded {
                path: captured["path"].as_str().unwrap_or_default().to_string(),
                headers: captured["headers"].clone(),
                body: captured["body"].as_str().unwrap_or_default().to_string(),
            });
        }
    }

    /// Look again until `ready` holds of everything recorded so far, or fail quoting what was
    /// recorded: a wait on the CONDITION, which leaves as soon as it holds.
    fn until(&mut self, what: &str, ready: impl Fn(&[Recorded]) -> bool) {
        let giving_up_at = Instant::now() + SETTLE_DEADLINE;
        loop {
            self.drain();
            if ready(&self.seen) {
                return;
            }
            assert!(
                Instant::now() < giving_up_at,
                "expected {what}, the endpoint recorded {:?}; the log:\n{}",
                self.seen,
                self.emulator.log()
            );
            std::thread::sleep(LOOK_INTERVAL);
        }
    }

    /// How many messages of `message_type` carrying `message_id` were POSTed to `path`.
    fn posts(&self, path: &str, message_type: &str, message_id: &str) -> usize {
        self.seen
            .iter()
            .filter(|record| {
                record.is(path, message_type) && record.header(MESSAGE_ID_HEADER) == Some(message_id)
            })
            .count()
    }
}

/// The messages a redrive target holds, drained the same way the captures are.
struct DeadLetters<'a> {
    emulator: &'a Started,
    url: String,
    seen: Vec<Value>,
}

impl<'a> DeadLetters<'a> {
    fn new(emulator: &'a Started, url: &str) -> Self {
        Self {
            emulator,
            url: url.to_string(),
            seen: Vec::new(),
        }
    }

    fn drain(&mut self) {
        let received = sqs(
            self.emulator,
            "ReceiveMessage",
            &json!({
                "QueueUrl": self.url,
                "MaxNumberOfMessages": 10,
                "WaitTimeSeconds": WAIT_TIME_SECONDS,
            }),
        );
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            sqs(
                self.emulator,
                "DeleteMessage",
                &json!({
                    "QueueUrl": self.url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            );
            self.seen
                .push(parsed(message["Body"].as_str().unwrap_or_default()));
        }
    }

    /// Look again until the redrive target holds a message of `message_id`.
    fn until_held(&mut self, case: &str, message_id: &str) {
        let giving_up_at = Instant::now() + SETTLE_DEADLINE;
        loop {
            self.drain();
            if self.held(message_id) > 0 {
                return;
            }
            assert!(
                Instant::now() < giving_up_at,
                "case {case:?}: expected the undeliverable notification of {message_id} in the \
                 subscription's dead-letter queue, it holds {:?}; the log:\n{}",
                self.seen,
                self.emulator.log()
            );
            std::thread::sleep(LOOK_INTERVAL);
        }
    }

    /// How many of the messages held are the notification of `message_id`.
    fn held(&self, message_id: &str) -> usize {
        self.seen
            .iter()
            .filter(|held| held["MessageId"].as_str() == Some(message_id))
            .count()
    }
}

/// Create a queue and answer its url.
fn given_queue(emulator: &Started, name: &str) -> String {
    let created = sqs(emulator, "CreateQueue", &json!({"QueueName": name}));
    created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("the queue {name} reports no url, got {created}"))
        .to_string()
}

/// The ARN of the queue `url` names, as its own attributes report it.
fn queue_arn(emulator: &Started, url: &str) -> String {
    let attributes = sqs(
        emulator,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["QueueArn"]}),
    );
    attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("the queue {url} reports no ARN, got {attributes}"))
        .to_string()
}

/// Stand the endpoint up: the capture queue, the refusing recording function behind a public
/// function URL, and the base address that URL was published at.
fn given_sink(emulator: &Started, case: &str) -> (String, String) {
    let capture_url = given_queue(emulator, &format!("{STACK}-capture"));

    let outputs = deployed(emulator, case, STACK, &wiring(&capture_url).to_string());
    let function = output(&outputs, case, "FunctionName");

    // An invocation needs the function Active, which is a span the service owes.
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (_, configuration) = lambda(
            emulator,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            None,
        );
        if configuration["State"].as_str() == Some("Active") {
            break;
        }
        assert!(
            Instant::now() < giving_up_at,
            "the function {function} never reached Active, last read {configuration}; the \
             log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }

    let (status, url) = lambda(
        emulator,
        "POST",
        &format!("/2021-10-31/functions/{function}/url"),
        Some(&json!({"AuthType": "NONE"})),
    );
    assert_eq!(
        status, 201,
        "CreateFunctionUrlConfig must answer 201, got {status} with {url}; the log:\n{}",
        emulator.log()
    );
    // `AuthType=NONE` leaves IAM out of it; the function's own resource policy is what admits
    // the anonymous caller an SNS delivery is.
    let (status, granted) = lambda(
        emulator,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        Some(&json!({
            "StatementId": "public-url",
            "Action": "lambda:InvokeFunctionUrl",
            "Principal": "*",
            "FunctionUrlAuthType": "NONE",
        })),
    );
    assert_eq!(
        status, 201,
        "AddPermission must answer 201, got {status} with {granted}; the log:\n{}",
        emulator.log()
    );
    let sink = url["FunctionUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("no FunctionUrl in {url}"))
        .trim_end_matches('/')
        .to_string();
    (capture_url, sink)
}

/// The delivery policy that budgets `retries` retries of a failed delivery, with the shortest
/// delay a policy may name between them ("minDelayTarget" and "maxDelayTarget" are delays in
/// seconds of at least 1, SNS Developer Guide, "Amazon SNS message delivery retries").
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
        }
    })
    .to_string()
}

/// The `numRetries` of the `healthyRetryPolicy` the subscription's `EffectiveDeliveryPolicy`
/// reports, read through the attribute map `GetSubscriptionAttributes` answers.
fn effective_num_retries(emulator: &Started, case: &str, subscription: &str) -> i64 {
    let answered = sns(
        emulator,
        "GetSubscriptionAttributes",
        &[("SubscriptionArn", subscription)],
    );
    let attributes = block_of(&answered, "Attributes")
        .map(decoded_entries)
        .unwrap_or_default();
    let reported = attributes
        .get("EffectiveDeliveryPolicy")
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: no EffectiveDeliveryPolicy reported, got {attributes:?}")
        });
    let document = parsed(&reported);
    document["healthyRetryPolicy"]["numRetries"]
        .as_i64()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the effective policy must state a healthyRetryPolicy.numRetries, \
                 got {reported}"
            )
        })
}

/// One row: the retry budget a subscription is written, and the POSTs one notification to its
/// refusing endpoint must then cost.
struct Row {
    case: &'static str,
    /// What this row's subscription addresses under the published endpoint: the path the
    /// subscriber supplies, or nothing at all for a subscriber handed the endpoint itself.
    suffix: &'static str,
    /// The path within the URL the endpoint must then be told about, which is what the
    /// recorded requests of this row carry.
    reached: &'static str,
    /// The `numRetries` written on the subscription.
    retries: i64,
    /// The POSTs one notification must cost: one attempt plus that budget.
    posts: usize,
}

const ROWS: &[Row] = &[
    Row {
        case: "a refusing endpoint and no retry budget at all",
        suffix: "/fail/shared",
        reached: "/fail/shared",
        retries: 0,
        posts: 1,
    },
    Row {
        // The TWIN of the row above: the very same endpoint, on a topic of its own, written a
        // budget of one retry — so what decides the attempts is the subscription's own policy
        // and not the endpoint they share.
        case: "a refusing endpoint shared with a twin subscription, and one retry",
        suffix: "/fail/shared",
        reached: "/fail/shared",
        retries: 1,
        posts: 2,
    },
    Row {
        case: "a refusing endpoint and two retries",
        suffix: "/fail/two",
        reached: "/fail/two",
        retries: 2,
        posts: 3,
    },
    Row {
        // The endpoint EXACTLY as it was published, which is the address a client that stood
        // its receiver up a moment earlier holds: the subscriber supplies no path of its own,
        // so the request reaches the URL's own root.
        case: "the published endpoint itself, with no retry budget",
        suffix: "/",
        reached: "/",
        retries: 0,
        posts: 1,
    },
    Row {
        // An endpoint that refuses EVERYTHING, the request that confirms the subscription
        // included: the confirmation still reaches it, so its owner holds the token that
        // confirms the subscription, and the notifications it then refuses are attempted on
        // its own budget and held in its redrive target.
        case: "an endpoint that refuses even the request that confirms it",
        suffix: "/refuse-all",
        reached: "/refuse-all",
        retries: 0,
        posts: 1,
    },
];

/// How many publishes each row makes: the attempt count is a function of the configured
/// policy, so the same budget has to hold for a second notification as for the first.
const TRIALS: usize = 2;

/// RULE: on the emulator a client starts, a confirmed `https` subscription whose endpoint
/// refuses the notification is POSTed one attempt plus its effective `healthyRetryPolicy`'s
/// `numRetries` per publish, and the notification it never took is then held in the queue its
/// `RedrivePolicy` names, once per publish.
#[test]
fn a_failing_https_subscription_is_attempted_on_its_budget_and_then_dead_lettered() {
    let case = "the retry budget of an https subscription on a started emulator";
    let emulator = Started::start();
    let (capture_url, sink) = given_sink(&emulator, case);
    let mut captures = Captures::new(&emulator, &capture_url);

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let topic = opt_element(
            &sns(
                &emulator,
                "CreateTopic",
                &[("Name", &format!("{STACK}-{index}"))],
            ),
            "TopicArn",
        )
        .unwrap_or_default();
        assert!(
            !topic.is_empty(),
            "case {case:?}: CreateTopic must answer the topic's ARN"
        );

        let endpoint = format!("{sink}{}", row.suffix);
        let subscribed = sns(
            &emulator,
            "Subscribe",
            &[
                ("TopicArn", &topic),
                ("Protocol", "https"),
                ("Endpoint", &endpoint),
                ("ReturnSubscriptionArn", "true"),
            ],
        );
        let subscription = opt_element(&subscribed, "SubscriptionArn").unwrap_or_default();
        assert!(
            !subscription.is_empty(),
            "case {case:?}: Subscribe must answer the subscription's ARN, got {subscribed}"
        );

        // The confirmation request carries the token no response of this service does, so it
        // is what the subscription is confirmed with.
        let path = row.reached;
        let of_topic = topic.clone();
        captures.until(
            &format!("case {case:?}: the {CONFIRMATION_TYPE} POST of {of_topic} at {path}"),
            move |seen| seen.iter().any(|record| record.confirms(&of_topic)),
        );
        let confirmation = captures
            .seen
            .iter()
            .find(|record| record.confirms(&topic))
            .cloned()
            .expect("the wait above returned only once a confirmation was recorded");
        assert_eq!(
            confirmation.path, path,
            "case {case:?}: the confirmation must reach the path within the endpoint the \
             subscriber was handed, got {:?}",
            confirmation.path
        );
        let token = parsed(&confirmation.body)["Token"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !token.is_empty(),
            "case {case:?}: expected a Token in the confirmation document, got {:?}",
            confirmation.body
        );
        let confirmed = sns(
            &emulator,
            "ConfirmSubscription",
            &[("TopicArn", &topic), ("Token", &token)],
        );
        assert_eq!(
            opt_element(&confirmed, "SubscriptionArn").as_deref(),
            Some(subscription.as_str()),
            "case {case:?}: the POSTed token must confirm {subscription:?}, got {confirmed}"
        );

        // The budget this row's subscription is written, and the redrive target that holds
        // what its endpoint never takes — a queue whose own policy admits this service, as a
        // client writes it for a redrive target.
        let dead_letter_url = given_queue(&emulator, &format!("{STACK}-dlq-{index}"));
        let dead_letter_arn = queue_arn(&emulator, &dead_letter_url);
        sqs(
            &emulator,
            "SetQueueAttributes",
            &json!({
                "QueueUrl": dead_letter_url,
                "Attributes": {
                    "Policy": json!({
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Sid": "take-the-undeliverable-notifications",
                            "Effect": "Allow",
                            "Principal": { "Service": "sns.amazonaws.com" },
                            "Action": "sqs:SendMessage",
                            "Resource": dead_letter_arn,
                        }],
                    })
                    .to_string(),
                },
            }),
        );
        let mut dead_letters = DeadLetters::new(&emulator, &dead_letter_url);

        sns(
            &emulator,
            "SetSubscriptionAttributes",
            &[
                ("SubscriptionArn", &subscription),
                ("AttributeName", "DeliveryPolicy"),
                ("AttributeValue", &retry_policy(row.retries)),
            ],
        );
        sns(
            &emulator,
            "SetSubscriptionAttributes",
            &[
                ("SubscriptionArn", &subscription),
                ("AttributeName", "RedrivePolicy"),
                (
                    "AttributeValue",
                    &json!({"deadLetterTargetArn": dead_letter_arn}).to_string(),
                ),
            ],
        );

        // What the subscription's deliveries really run under is what the read reports.
        let reported = effective_num_retries(&emulator, case, &subscription);
        assert_eq!(
            reported, row.retries,
            "case {case:?}: EffectiveDeliveryPolicy must report the configured numRetries, got \
             {reported}"
        );

        for trial in 1..=TRIALS {
            let message = format!("the body nobody takes, trial {trial}");
            let published = sns(
                &emulator,
                "Publish",
                &[("TopicArn", &topic), ("Message", &message)],
            );
            let message_id = opt_element(&published, "MessageId").unwrap_or_default();
            assert!(
                !message_id.is_empty(),
                "case {case:?} trial {trial}: Publish must answer a MessageId, got {published}"
            );

            // The attempts the policy budgets, counted on the requests the endpoint recorded.
            let expected = row.posts;
            let wanted = message_id.clone();
            captures.until(
                &format!(
                    "case {case:?} trial {trial}: {expected} {NOTIFICATION_TYPE} POST(s) of \
                     {wanted} at {path}"
                ),
                move |seen| {
                    seen.iter()
                        .filter(|record| {
                            record.is(path, NOTIFICATION_TYPE)
                                && record.header(MESSAGE_ID_HEADER) == Some(wanted.as_str())
                        })
                        .count()
                        >= expected
                },
            );

            // And the notification no attempt handed over, held in the redrive target. It is
            // written once the attempts are spent, so what the endpoint recorded is complete
            // by the time it arrives.
            dead_letters.until_held(case, &message_id);
            captures.drain();
            let attempted = captures.posts(path, NOTIFICATION_TYPE, &message_id);
            assert_eq!(
                attempted, row.posts,
                "case {case:?} trial {trial}: expected {} POST(s) of {message_id} at {path}, the \
                 endpoint recorded {attempted}; the log:\n{}",
                row.posts,
                emulator.log()
            );
            let held = dead_letters.held(&message_id);
            assert_eq!(
                held, 1,
                "case {case:?} trial {trial}: expected exactly one dead letter of {message_id}, \
                 the queue held {held}; the log:\n{}",
                emulator.log()
            );
        }
    }
}
