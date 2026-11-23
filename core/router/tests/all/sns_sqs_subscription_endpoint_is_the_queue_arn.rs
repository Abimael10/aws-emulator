// SPDX-License-Identifier: CC0-1.0

//! WHICH sentence Amazon SNS refuses a `Subscribe` whose `sqs` `Endpoint` is a queue handle
//! that is not the queue's ARN with — asserted over the production service set
//! (`aws_emulator_router::build_router` on one engine, exactly as the binary assembles it),
//! so every value in it is one Amazon SQS itself handed the caller back.
//!
//! RULE under test: **for a protocol whose endpoint is the ARN of the service the protocol
//! string names (`smithy_query(query="doc",shape="SubscribeInput",member="Endpoint")`: "For
//! the sqs protocol, the endpoint is the ARN of an Amazon SQS queue"), a value SNS cannot read
//! as an ARN at all is refused `400`/`InvalidParameter` with the FORM that protocol requires,
//! `Invalid parameter: <Service> endpoint ARN` — fixed text, with nothing of the request in
//! it — while a value SNS DOES read as an ARN and does not accept names the input member,
//! `Invalid parameter: Endpoint`.** Neither refusal creates anything, and the ARN the queue
//! itself reports subscribes and is listed exactly once.
//!
//! Why the suite is here rather than in the SNS crate: the values that matter are the OTHER
//! service's. A queue url is what `CreateQueue`, `GetQueueUrl` and `ListQueues` hand a caller
//! back, so it is the value a caller wiring SNS -> SQS has at hand and the mistake actually
//! made; the queue ARN is what `GetQueueAttributes` reports; and the ARN SNS reads but does
//! not accept is a real resource of another service in the same emulator. Each handle below is
//! first shown to ADDRESS the queue in Amazon SQS (`GetQueueAttributes` through it reports the
//! queue's own ARN), so what SNS refuses is the endpoint's FORM and not an unknown queue. The
//! SNS crate's own
//! `tests/all/subscribe_endpoint_form_refusals.rs` holds the same rule over every protocol
//! with values written by hand; the two are to be kept in step — a change to either sentence
//! fails both.
//!
//! The rows differ along what the rule generalises over: the kind of queue (standard and
//! FIFO), the queue name's own text, which SQS operation minted the handle, the handle's form
//! (a full url, a url with a trailing separator, the bare name), and — for the member-named
//! half — which ARN-shaped value of which service was given to which protocol.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{authorization, router};
use crate::query_wire::{element, form_encode, members_of, opt_element};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The code SNS names a refused parameter by, as the modelled `InvalidParameterException`'s
/// `awsQueryError` declares it (`httpResponseCode` 400).
const INVALID_PARAMETER: &str = "InvalidParameter";

/// The sentence for a value SNS could not read as the ARN the `sqs` protocol takes: the FORM
/// required, naming the service the protocol string names.
const ARN_FORM_REFUSAL: &str = "Invalid parameter: SQS endpoint ARN";

/// The sentence for a value SNS DID read as an ARN and did not accept: the input member's
/// name, with nothing of the request in it.
const MEMBER_REFUSAL: &str = "Invalid parameter: Endpoint";

/// One row: a queue this emulator really holds, and the topic it is subscribed to.
struct Row {
    case: &'static str,
    /// The queue's name — `.fifo`-suffixed exactly when `fifo` is set, as Amazon SQS requires
    /// of a FIFO queue.
    queue: &'static str,
    /// The topic's name, suffixed the same way: a FIFO queue may be subscribed only to a FIFO
    /// topic, so the row carries the topic kind its queue can be delivered to.
    topic: &'static str,
    fifo: bool,
}

/// Each kind of queue, with a name of its own: what the refusal says must not depend on the
/// queue's kind or on the text of its name.
const ROWS: &[Row] = &[
    Row {
        case: "a standard queue subscribed to a standard topic",
        queue: "endpoint-arn-standard",
        topic: "endpoint-arn-standard-topic",
        fifo: false,
    },
    Row {
        case: "a standard queue whose name carries every character class SQS allows",
        queue: "endpoint_arn-Mixed09",
        topic: "endpoint_arn-Mixed09-topic",
        fifo: false,
    },
    Row {
        case: "a FIFO queue subscribed to a FIFO topic",
        queue: "endpoint-arn-ordered.fifo",
        topic: "endpoint-arn-ordered-topic.fifo",
        fifo: true,
    },
];

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("sqs", REGION))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
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

/// One Amazon SNS `awsQuery` call (API version 2010-03-31), as its status and XML body — a
/// refusal is an answer this suite reads rather than a failure.
async fn sns(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-03-31");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sns.{REGION}.amazonaws.com"))
        .header("authorization", authorization("sns", REGION))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// `Subscribe(topic, protocol, endpoint)`, asking for the subscription ARN so a success is
/// read off the answer.
async fn subscribe(
    router: &MultiServiceRouter,
    topic: &str,
    protocol: &str,
    endpoint: &str,
) -> (u16, String) {
    sns(
        router,
        "Subscribe",
        &[
            ("TopicArn", topic),
            ("Protocol", protocol),
            ("Endpoint", endpoint),
            ("ReturnSubscriptionArn", "true"),
        ],
    )
    .await
}

/// Assert that `answer` is the refusal `want`, stated as SNS states this class: HTTP 400 under
/// the modelled `InvalidParameter` code, with exactly that message.
fn assert_refused(case: &str, answer: &(u16, String), want: &str) {
    let (status, body) = answer;
    assert_eq!(
        *status, 400,
        "case {case:?}: expected 400, got {status} with {body}"
    );
    assert_eq!(
        opt_element(body, "Code").as_deref(),
        Some(INVALID_PARAMETER),
        "case {case:?}: expected the {INVALID_PARAMETER} code, got {body}"
    );
    assert_eq!(
        opt_element(body, "Message").as_deref(),
        Some(want),
        "case {case:?}: expected {want:?}, got {body}"
    );
}

/// The `(SubscriptionArn, Protocol, Endpoint)` triples the topic reports right now.
async fn subscriptions_of(
    router: &MultiServiceRouter,
    case: &str,
    topic: &str,
) -> Vec<(String, String, String)> {
    let (status, body) = sns(router, "ListSubscriptionsByTopic", &[("TopicArn", topic)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListSubscriptionsByTopic expected 200, got {status} with {body}"
    );
    members_of(&body)
        .into_iter()
        .map(|member| {
            // Every member of the modelled list carries all three, so a missing one fails
            // loudly rather than reading as a subscription that is not there.
            let read = |name: &str| {
                opt_element(member, name).unwrap_or_else(|| {
                    panic!("case {case:?}: expected <{name}> on the listed subscription {member}")
                })
            };
            (
                read("SubscriptionArn"),
                read("Protocol"),
                read("Endpoint"),
            )
        })
        .collect()
}

/// The queue `row` names: the handles Amazon SQS hands a caller back for it, and the ARN it
/// reports for itself.
async fn given_queue(router: &MultiServiceRouter, row: &Row) -> (Vec<(String, String)>, String) {
    let case = row.case;
    let mut body = json!({ "QueueName": row.queue });
    if row.fifo {
        body["Attributes"] = json!({ "FifoQueue": "true" });
    }
    let created = sqs(router, case, "CreateQueue", &body).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
        .to_string();

    let fetched = sqs(
        router,
        case,
        "GetQueueUrl",
        &json!({ "QueueName": row.queue }),
    )
    .await;
    let fetched_url = fetched["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {fetched}"))
        .to_string();

    let listed = sqs(
        router,
        case,
        "ListQueues",
        &json!({ "QueueNamePrefix": row.queue }),
    )
    .await;
    let listed_url = listed["QueueUrls"]
        .as_array()
        .and_then(|urls| urls.first())
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected the queue's url, got {listed}"))
        .to_string();

    let handles = vec![
        ("the url CreateQueue returned".to_string(), url.clone()),
        ("the url GetQueueUrl reports".to_string(), fetched_url),
        ("the url ListQueues reports".to_string(), listed_url),
        (
            "that url with a trailing separator".to_string(),
            format!("{url}/"),
        ),
        ("the queue's bare name".to_string(), row.queue.to_string()),
    ];

    let reported = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let queue_arn = reported["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn, got {reported}"))
        .to_string();

    // Each handle really addresses THIS queue in Amazon SQS: the refusals below are about the
    // endpoint's form, not about a queue SNS could not find.
    for (how, handle) in &handles {
        let through = sqs(
            router,
            case,
            "GetQueueAttributes",
            &json!({ "QueueUrl": handle, "AttributeNames": ["QueueArn"] }),
        )
        .await;
        assert_eq!(
            through["Attributes"]["QueueArn"].as_str(),
            Some(queue_arn.as_str()),
            "case {case:?}: {how} must address the queue in Amazon SQS, got {through}"
        );
    }

    (handles, queue_arn)
}

/// The topic `row` names, created with the kind its queue may be delivered to.
async fn given_topic(router: &MultiServiceRouter, row: &Row) -> String {
    let mut params: Vec<(&str, &str)> = vec![("Name", row.topic)];
    if row.fifo {
        params.push(("Attributes.entry.1.key", "FifoTopic"));
        params.push(("Attributes.entry.1.value", "true"));
    }
    let (status, body) = sns(router, "CreateTopic", &params).await;
    assert_eq!(
        status, 200,
        "case {:?}: CreateTopic expected 200, got {status} with {body}",
        row.case
    );
    element(&body, "TopicArn")
}

/// Every handle Amazon SQS hands back for a queue EXCEPT its ARN is refused with the endpoint
/// FORM the `sqs` protocol requires, nothing is created by any of them, and the ARN the queue
/// reports for itself subscribes and is listed exactly once — still refusing the url
/// afterwards, with the one subscription left as it was.
#[tokio::test]
async fn a_queue_url_given_as_an_sqs_endpoint_is_refused_with_the_arn_form_required() {
    let router = router();

    for row in ROWS {
        let case = row.case;
        let (handles, queue_arn) = given_queue(&router, row).await;
        let topic = given_topic(&router, row).await;

        for (how, handle) in &handles {
            let case = format!("{case}, subscribing {how}");
            let answer = subscribe(&router, &topic, "sqs", handle).await;
            assert_refused(&case, &answer, ARN_FORM_REFUSAL);
        }

        // Judged before anything is written: the topic holds no subscription at all.
        let listed = subscriptions_of(&router, case, &topic).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: a refused Subscribe must create nothing, got {listed:?}"
        );

        // The ARN the queue reports for itself is the endpoint the protocol takes.
        let (status, body) = subscribe(&router, &topic, "sqs", &queue_arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: Subscribe with the queue's ARN expected 200, got {status} with \
             {body}"
        );
        let subscription = element(&body, "SubscriptionArn");
        let listed = subscriptions_of(&router, case, &topic).await;
        assert_eq!(
            listed,
            vec![(subscription.clone(), "sqs".to_string(), queue_arn.clone())],
            "case {case:?}: the queue's ARN must be listed exactly once"
        );

        // And the url is still refused the same way once the subscription exists, leaving it
        // as it was: the refusal is about the value's form, not about what the topic holds.
        for (how, handle) in &handles {
            let refused = format!("{case}, re-subscribing {how} after the ARN subscription");
            let answer = subscribe(&router, &topic, "sqs", handle).await;
            assert_refused(&refused, &answer, ARN_FORM_REFUSAL);
        }
        let listed = subscriptions_of(&router, case, &topic).await;
        assert_eq!(
            listed,
            vec![(subscription, "sqs".to_string(), queue_arn)],
            "case {case:?}: a refused Subscribe must leave the subscription it did not create \
             alone"
        );
    }
}

/// An `Endpoint` SNS DOES read as an ARN but does not accept — a real resource of another
/// service of this emulator, or the queue's own ARN handed to a protocol that addresses
/// another service — names the input MEMBER instead of a form, and creates nothing.
#[tokio::test]
async fn an_arn_sns_reads_and_does_not_accept_names_the_member() {
    let case = "a standard queue and a standard topic of this emulator";
    let router = router();
    let row = &ROWS[0];
    let (_, queue_arn) = given_queue(&router, row).await;
    let topic = given_topic(&router, row).await;

    // `(the protocol asked for, an ARN-shaped value of this emulator that is not the resource
    // that protocol addresses)`: the topic's own ARN given to `sqs`, and the queue's ARN given
    // to the protocols whose endpoint is another service's ARN or no ARN at all.
    let wrong: &[(&str, &str)] = &[
        ("sqs", topic.as_str()),
        ("lambda", queue_arn.as_str()),
        ("firehose", queue_arn.as_str()),
        ("application", queue_arn.as_str()),
        ("https", queue_arn.as_str()),
    ];
    for (protocol, endpoint) in wrong {
        let refused = format!("{case}, the {protocol} protocol given {endpoint:?}");
        let answer = subscribe(&router, &topic, protocol, endpoint).await;
        assert_refused(&refused, &answer, MEMBER_REFUSAL);
    }

    let listed = subscriptions_of(&router, case, &topic).await;
    assert!(
        listed.is_empty(),
        "case {case:?}: a refused Subscribe must create nothing, got {listed:?}"
    );
}
