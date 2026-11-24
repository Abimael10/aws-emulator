// SPDX-License-Identifier: CC0-1.0

//! What Amazon SQS answers a call on a queue that was TORN DOWN after serving an SNS fan-out —
//! over the WHOLE mounted service set, the one `core/router/src/lib.rs` assembles for the
//! binary.
//!
//! The RULE under test: **a request naming a queue this account does not hold is refused HTTP
//! 400 in the `QueueDoesNotExist` class — reported beside the legacy query code the model binds
//! to that class, `AWS.SimpleQueueService.NonExistentQueue` — carrying the FIXED sentence the
//! wire protocol the request arrived in words that class with, whatever operation was called
//! and however the queue was addressed (its `QueueUrl`, or the `QueueName` the one
//! name-addressed operation resolves).** On this service's `awsJson1_0` surface, with or without
//! the query-compatibility opt-in a migrating SDK sends, that sentence is "The specified queue
//! does not exist."; on its WSDL-versioned AWS Query surface it is the same statement naming
//! that surface's version. Either sentence is a CONSTANT — the error code is what identifies the
//! condition, so nothing of the request is interpolated into it — and neither surface's wording
//! carries a clause of the other's.
//!
//! That refusal is the TEARDOWN control of an SNS → SQS pipeline: the call a client makes to
//! prove the destination it was publishing to a moment earlier is really gone. So it has to be
//! taken on a queue that WAS serving, which is what this suite builds first — a topic with the
//! queue subscribed to it, a publish that reaches it, and the envelope read back off it — and
//! then deletes.
//!
//! Overlap, stated rather than hidden: the Amazon SQS crate's `queue_does_not_exist` suite
//! drives the table of operations and absence forms against that service mounted alone, and the
//! sibling `sns_sqs_pipeline_absent_queue_refusal` suite takes the two JSON dialects on a queue
//! deleted out from under a single `Publish`. That sibling owns the harness both suites share —
//! the call seam, the readings of an answer, the table of url-addressed operations and the
//! refusal assertion, all imported below rather than re-typed here. What only this suite holds is
//! the BATCHED fan-out as the state the torn-down queue served in (one envelope per entry, each
//! carrying its subject and its typed message attributes) and the AWS Query form wire taken on
//! that same teardown, so which sentence a client reads is pinned to the protocol of its request
//! and to nothing else.
//!
//! The rows differ along what the rule generalises over: the fan-out FORM the queue served
//! before the teardown (a single publish, a batch of several entries), the FORM of the absence
//! (the url of the torn-down queue, a url of a queue the account never created, the name of the
//! torn-down queue, a name that never existed), the OPERATION called, and the SURFACE the client
//! speaks (the native JSON one, the query-compatible JSON one, and the AWS Query form wire).

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{
    authorization, given_topic_with_subscriber, router, sns, Grant,
};
use crate::query_wire::{element, form_encode};
// The harness this rule is asserted through, owned by the sibling suite that asserts it on a
// single publish: the Region the wiring lives in, the JSON call seam and its served form, the
// readings of an answer, the table of url-addressed operations, and the whole refusal assertion.
use crate::sns_sqs_pipeline_absent_queue_refusal::{
    assert_absence_refusal, message_of, sqs_answer, sqs_ok, url_of_name, url_requests, Dialect,
    REFUSAL_STATUS, REGION,
};

/// The sentence the WSDL-versioned AWS Query surface of this API words the absence with, which
/// the JSON surface never serves. One API, two protocols, two sentences for one class, with the
/// same code and status on both.
///
/// The service's own `query_compatible_error_envelope` suite owns this wording for the surface
/// that serves it; it is written out again here because what this suite asserts is that the
/// sentence a client reads is selected by the PROTOCOL its request arrived in — the JSON rows
/// above read the other sentence, and neither carries a clause of the other's.
const QUERY_API_SENTENCE: &str = "The specified queue does not exist for this wsdl version.";

/// The clause that tells the two apart, asserted ABSENT from the JSON surface's answer: a client
/// matching AWS's own sentence on that surface cannot match one carrying the other surface's
/// wording.
const VERSION_CLAUSE: &str = "wsdl";

/// The legacy query code the model's `awsQueryError` trait binds to the class, which a query
/// client reads as `Error.Code`.
const NON_EXISTENT_QUEUE: &str = "AWS.SimpleQueueService.NonExistentQueue";

/// The API version an AWS Query client of this endpoint names in every request
/// (`smithy_query(query="service_info",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`:
/// version `2012-11-05`).
const QUERY_API_VERSION: &str = "2012-11-05";

/// The media type that surface carries its requests in.
const FORM_CONTENT_TYPE: &str = "application/x-www-form-urlencoded";

/// How many times a leg polls the queue for the fan-out before calling it undelivered, and the
/// long poll each of those calls makes. The wait is for what has ARRIVED and never for a span, so
/// a loaded machine delays a leg instead of failing it.
const POLLS: usize = 30;
const WAIT_TIME_SECONDS: u32 = 1;

/// The subject every publish of this suite carries, and which the envelope must report back.
const SUBJECT: &str = "the pipeline under teardown";

/// One form parameter of an AWS Query request: the name, which is this suite's own literal, and
/// the value it carries.
type Param = (&'static str, String);

/// One Amazon SQS call on the AWS Query form wire, as its status and the XML text it answered.
async fn sqs_query(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    let mut body = format!("Action={action}&Version={QUERY_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("sqs", REGION))
        .header("content-type", FORM_CONTENT_TYPE)
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One attribute every publish of this suite carries: its name, the `DataType` the request spells,
/// the value, and whether the value travels in `BinaryValue` rather than `StringValue`.
///
/// The three logical types `MessageAttributeValue$DataType` names ("String, String.Array, Number,
/// and Binary"), so the envelope the torn-down queue served carried a TYPED attribute map and not
/// just text.
struct Attribute {
    name: &'static str,
    data_type: &'static str,
    value: &'static str,
    binary: bool,
}

/// The attributes one publish carries.
const ATTRIBUTES: &[Attribute] = &[
    Attribute {
        name: "kind",
        data_type: "String",
        value: "created",
        binary: false,
    },
    Attribute {
        name: "attempt",
        data_type: "Number",
        value: "2",
        binary: false,
    },
    Attribute {
        name: "icon",
        data_type: "Binary",
        // "GIF89a", as the base64 a blob travels the wire as.
        value: "R0lGODlh",
        binary: true,
    },
];

/// The form parameters one publish carries for `ATTRIBUTES`, under `prefix` — the empty prefix
/// for a single `Publish`, the entry's own prefix inside a batch.
fn attribute_params(prefix: &str) -> Vec<(String, String)> {
    let mut params = Vec::with_capacity(ATTRIBUTES.len() * 3);
    for (at, attribute) in ATTRIBUTES.iter().enumerate() {
        let entry = format!("{prefix}MessageAttributes.entry.{}", at + 1);
        params.push((format!("{entry}.Name"), attribute.name.to_string()));
        params.push((
            format!("{entry}.Value.DataType"),
            attribute.data_type.to_string(),
        ));
        let member = if attribute.binary {
            "BinaryValue"
        } else {
            "StringValue"
        };
        params.push((
            format!("{entry}.Value.{member}"),
            attribute.value.to_string(),
        ));
    }
    params
}

/// One row: the fan-out the queue served before it was torn down.
struct Row {
    case: &'static str,
    /// The queue's name, and the topic's.
    name: &'static str,
    /// The messages published to the topic — one for the single form, several for the batch.
    messages: &'static [&'static str],
    /// Whether they are published as ONE batch (`PublishBatch`) or as a single `Publish`.
    batched: bool,
}

/// The row whose queue served a single enveloped publish.
const SINGLE: Row = Row {
    case: "a queue torn down after a single enveloped publish",
    name: "sns-teardown-single",
    messages: &["the only notification"],
    batched: false,
};

/// The row whose queue served a batch, one envelope per entry.
const BATCHED: Row = Row {
    case: "a queue torn down after a batched publish of three entries",
    name: "sns-teardown-batched",
    messages: &["the first", "the second", "the third"],
    batched: true,
};

/// The rows: see this suite's header for what they differ along.
const ROWS: &[&Row] = &[&SINGLE, &BATCHED];

/// The name of a queue this account never creates, probed in every form of the absence.
const NEVER: &str = "sns-teardown-never-created";

/// Publish `row`'s messages to `topic`, each carrying the subject and the typed attributes above,
/// in the form the row names.
async fn publish(router: &MultiServiceRouter, row: &Row, topic: &str) {
    let mut params: Vec<(String, String)> = vec![("TopicArn".to_string(), topic.to_string())];
    if row.batched {
        for (index, message) in row.messages.iter().enumerate() {
            let prefix = format!("PublishBatchRequestEntries.member.{}.", index + 1);
            params.push((format!("{prefix}Id"), format!("entry-{}", index + 1)));
            params.push((format!("{prefix}Message"), (*message).to_string()));
            params.push((format!("{prefix}Subject"), SUBJECT.to_string()));
            params.extend(attribute_params(&prefix));
        }
    } else {
        params.push(("Message".to_string(), row.messages[0].to_string()));
        params.push(("Subject".to_string(), SUBJECT.to_string()));
        params.extend(attribute_params(""));
    }
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let action = if row.batched { "PublishBatch" } else { "Publish" };
    let answered = sns(router, row.case, REGION, action, &borrowed).await;
    assert!(
        !element(&answered, "MessageId").is_empty(),
        "case {:?}: an accepted {action} must report the message it took, got {answered}",
        row.case
    );
}

/// The notification envelopes waiting on the queue at `url`, polled until `wanted` have arrived or
/// the polls run out, deleting each one it hands over.
async fn envelopes_on(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    wanted: usize,
) -> Vec<Value> {
    let mut arrived: Vec<Value> = Vec::new();
    for _ in 0..POLLS {
        if arrived.len() >= wanted {
            break;
        }
        let received = sqs_ok(
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
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            let body = message["Body"].as_str().unwrap_or_default().to_string();
            arrived.push(serde_json::from_str(&body).unwrap_or_else(|error| {
                panic!("case {case:?}: the body is not the SNS envelope ({error}): {body}")
            }));
            if let Some(handle) = message["ReceiptHandle"].as_str() {
                sqs_ok(
                    router,
                    case,
                    "DeleteMessage",
                    &json!({"QueueUrl": url, "ReceiptHandle": handle}),
                )
                .await;
            }
        }
    }
    arrived
}

/// Build one row's pipeline, read the fan-out off its queue — the positive control that the queue
/// was SERVING — and answer the topic's ARN and the queue's url.
async fn given_served_pipeline(router: &MultiServiceRouter, row: &Row) -> (String, String) {
    let case = row.case;
    let (topic, url) =
        given_topic_with_subscriber(router, case, REGION, row.name, Grant::Unscoped).await;
    publish(router, row, &topic).await;

    let envelopes = envelopes_on(router, case, &url, row.messages.len()).await;
    assert_eq!(
        envelopes.len(),
        row.messages.len(),
        "case {case:?}: expected one envelope per published message, got {envelopes:?}"
    );
    let mut delivered: Vec<String> = envelopes
        .iter()
        .map(|envelope| envelope["Message"].as_str().unwrap_or_default().to_string())
        .collect();
    delivered.sort();
    let mut published: Vec<String> = row.messages.iter().map(ToString::to_string).collect();
    published.sort();
    assert_eq!(
        delivered, published,
        "case {case:?}: expected the envelopes to carry every published message, got \
         {delivered:?}"
    );
    for envelope in &envelopes {
        assert_eq!(
            envelope["Subject"].as_str(),
            Some(SUBJECT),
            "case {case:?}: the envelope must carry the subject as published, got {envelope}"
        );
        for attribute in ATTRIBUTES {
            let carried = &envelope["MessageAttributes"][attribute.name];
            assert_eq!(
                (
                    carried["Type"].as_str().unwrap_or_default(),
                    carried["Value"].as_str().unwrap_or_default()
                ),
                (attribute.data_type, attribute.value),
                "case {case:?}: the envelope must carry {:?} as published, got {envelope}",
                attribute.name
            );
        }
    }
    (topic, url)
}

/// The shared refusal assertion, with the one reading this suite adds to it: the sentence of this
/// surface carries no clause of the other surface's wording.
fn assert_json_absence(
    case: &str,
    operation: &str,
    dialect: Dialect,
    probed: &[&str],
    answered: (u16, http::HeaderMap, Value),
) {
    let message = message_of(&answered.2);
    assert!(
        !message.contains(VERSION_CLAUSE),
        "case {case:?} / {operation} as a {dialect:?} client: this surface's sentence carries no \
         clause of the other's, got {message:?}"
    );
    assert_absence_refusal(case, operation, dialect, probed, answered);
}

/// A queue torn down after it served an SNS fan-out refuses every call that resolves a queue,
/// in both JSON dialects and over every form of the absence, with AWS's own fixed sentence.
#[tokio::test]
async fn a_queue_torn_down_after_its_fan_out_refuses_every_sqs_call_with_awss_absence_sentence() {
    let router = router();

    for row in ROWS {
        let case = row.case;
        let (topic, url) = given_served_pipeline(&router, row).await;

        // Only the queue goes away: the subscription that named it stays exactly as it was, so
        // the refusals below are the absent queue's own and nothing of the wiring's.
        sqs_ok(&router, case, "DeleteQueue", &json!({"QueueUrl": url})).await;
        let subscriptions = sns(
            &router,
            case,
            REGION,
            "ListSubscriptionsByTopic",
            &[("TopicArn", &topic)],
        )
        .await;
        assert!(
            subscriptions.contains(row.name),
            "case {case:?}: expected the subscription to go on naming the torn-down queue, got \
             {subscriptions}"
        );

        let probed = [row.name, NEVER, url.as_str(), topic.as_str()];
        for (form, probed_url) in [
            ("the url of the torn-down queue", url.clone()),
            (
                "a url of a queue this account never created",
                url_of_name(&url, NEVER),
            ),
        ] {
            for (operation, request) in url_requests(&probed_url) {
                for dialect in [Dialect::Native, Dialect::QueryCompatible] {
                    let answered = sqs_answer(&router, operation, &request, dialect).await;
                    assert_json_absence(form, operation, dialect, &probed, answered);
                }
            }
        }

        // The name-addressed operation resolves neither name either.
        for (form, probed_name) in [
            ("the name of the torn-down queue", row.name),
            ("a name this account never created", NEVER),
        ] {
            for dialect in [Dialect::Native, Dialect::QueryCompatible] {
                let answered = sqs_answer(
                    &router,
                    "GetQueueUrl",
                    &json!({"QueueName": probed_name}),
                    dialect,
                )
                .await;
                assert_json_absence(form, "GetQueueUrl", dialect, &probed, answered);
            }
        }
    }
}

/// The AWS Query surface of the same API answers the same class, with the same code and status, in
/// the sentence that surface words it with — so which sentence a client reads is decided by the
/// protocol its request arrived in and by nothing else.
#[tokio::test]
async fn the_query_surface_words_the_same_teardown_absence_in_its_own_sentence() {
    let router = router();
    let row = &BATCHED;
    let case = row.case;
    let (_, url) = given_served_pipeline(&router, row).await;
    sqs_ok(&router, case, "DeleteQueue", &json!({"QueueUrl": url})).await;

    // The actions are spelled in this protocol's own request form: a `QueueUrl` field, the
    // `Attribute.N.Name` / `Attribute.N.Value` pairs an attribute map arrives as, and the
    // `QueueName` the name-addressed action takes.
    let never = url_of_name(&url, NEVER);
    let rows: Vec<(&str, &str, Vec<Param>)> = vec![
        (
            "SendMessage to the torn-down queue",
            "SendMessage",
            vec![
                ("QueueUrl", url.clone()),
                ("MessageBody", "gone".to_string()),
            ],
        ),
        (
            "SendMessage to a queue the account never created",
            "SendMessage",
            vec![("QueueUrl", never), ("MessageBody", "gone".to_string())],
        ),
        (
            "ReceiveMessage on the torn-down queue",
            "ReceiveMessage",
            vec![("QueueUrl", url.clone())],
        ),
        (
            "GetQueueAttributes on the torn-down queue",
            "GetQueueAttributes",
            vec![
                ("QueueUrl", url.clone()),
                ("AttributeName.1", "All".to_string()),
            ],
        ),
        (
            "SetQueueAttributes on the torn-down queue",
            "SetQueueAttributes",
            vec![
                ("QueueUrl", url.clone()),
                ("Attribute.1.Name", "VisibilityTimeout".to_string()),
                ("Attribute.1.Value", "10".to_string()),
            ],
        ),
        (
            "GetQueueUrl for the torn-down name",
            "GetQueueUrl",
            vec![("QueueName", row.name.to_string())],
        ),
    ];

    for (form, action, params) in &rows {
        let (status, answered) = sqs_query(&router, action, params).await;
        let code = element(&answered, "Code");
        let message = element(&answered, "Message");
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (REFUSAL_STATUS, NON_EXISTENT_QUEUE, QUERY_API_SENTENCE),
            "case {form:?}: expected HTTP {REFUSAL_STATUS} {NON_EXISTENT_QUEUE} reading \
             {QUERY_API_SENTENCE:?}, got {status} with {answered}"
        );
        assert!(
            !message.contains(row.name) && !message.contains(url.as_str()),
            "case {form:?}: the sentence must interpolate nothing of the request, got {message:?}"
        );
    }
}
