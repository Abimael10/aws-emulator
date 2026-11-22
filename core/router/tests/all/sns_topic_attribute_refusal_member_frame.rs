// SPDX-License-Identifier: CC0-1.0

//! Which INPUT MEMBER a refused topic-attribute value is reported against — on Amazon SNS's
//! own wire, and in the stack event CloudFormation forwards it on.
//!
//! RULE under test: **a condition Amazon SNS states as a sentence of its own — one that names
//! no attribute — is framed by the member the refused value arrived in: a write carried in
//! `CreateTopic`'s `Attributes` MAP is refused with `Invalid parameter: Attributes Reason: `
//! and that sentence, naming the member of the request the caller has to fix, while the same
//! value given to `SetTopicAttributes` through its `AttributeName`/`AttributeValue` pair is
//! refused with `Invalid parameter: ` and the sentence alone. A sentence that NAMES the
//! attribute already says which input is at fault, so it stands exactly as SNS words it
//! whichever call carried the write.** Either way the refusal is `400 InvalidParameter` and
//! the records are left as they were — the create makes no topic at all.
//!
//! It is the same rule this emulator already holds a SUBSCRIPTION attribute's refusal to
//! (`Subscribe`'s `Attributes` map against `SetSubscriptionAttributes`' named pair), read here
//! over a TOPIC's attributes.
//!
//! Two crates have to agree for the whole of it to hold, which is why the suite lives here: a
//! template's `AWS::SNS::Topic` carries its attributes in exactly that map, so what SNS
//! refuses the create with is what CloudFormation forwards as the resource's `CREATE_FAILED`
//! reason — the sentence a client reads off `DescribeStackEvents` while the stack rolls back,
//! which is the same text the direct call answers.
//!
//! The rows differ along what the rule generalises over: whether SNS's sentence for the
//! condition names the attribute, the KIND of domain the value fell outside (a closed value
//! set, a numeric range, a document grammar), the kind of topic the attribute belongs to
//! (standard and FIFO), and the rejected value's own text.

use std::time::Duration;

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use super::cloudformation_sns_custom_resource_protocol::{build_router, cloudformation};
use super::query_wire::{decoded_entries, element, elements, form_encode, members_of, opt_element};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// How long a stack's roll-back and the events it records are waited for, and how long the
/// runtime is handed back between two looks. Generous: what is asserted is the sentence the
/// stack recorded, never how fast it got there.
const WAIT: Duration = Duration::from_secs(60);
/// See [`WAIT`].
const LOOK: Duration = Duration::from_millis(25);

/// One row: the case, the topic kind the attribute belongs to, the attribute, a value outside
/// its domain, the sentence SNS states for that condition, and whether that sentence is one of
/// its OWN (so the member the value arrived in frames it) or one that names the attribute (so
/// it stands as it is).
struct Row {
    case: &'static str,
    /// Whether the attribute belongs to a FIFO topic, which decides the topic each half of the
    /// row is driven against.
    fifo: bool,
    attribute: &'static str,
    value: &'static str,
    /// The sentence, without the `Invalid parameter: ` opening every refusal of this class
    /// carries.
    sentence: &'static str,
    /// Whether the sentence names no attribute, and is therefore framed by the member.
    framed_by_the_member: bool,
}

/// The source of the catalog below: each attribute's domain is the one
/// `SetTopicAttributesInput$AttributeName` documents for it — `TracingConfig` ("By default
/// TracingConfig is set to PassThrough … If set to Active"), `FifoThroughputScope` ("Topic …
/// MessageGroup"), `SignatureVersion` ("By default SignatureVersion is set to 1"),
/// `MaximumMessageSize` ("Valid values are 1024 to 1048576") and the `Policy` document — and
/// each sentence is the one this emulator's SNS states for that condition.
const ROWS: &[Row] = &[
    // The two conditions SNS states as sentences of their own: neither names the attribute,
    // so each is framed by the member the value arrived in.
    Row {
        case: "a tracing mode SNS does not define",
        fifo: false,
        attribute: "TracingConfig",
        value: "Sampled",
        sentence: "Invalid tracing config value: Sampled",
        framed_by_the_member: true,
    },
    Row {
        case: "a tracing mode SNS does not define, spelled in another case",
        fifo: false,
        attribute: "TracingConfig",
        value: "active",
        sentence: "Invalid tracing config value: active",
        framed_by_the_member: true,
    },
    Row {
        case: "a throughput scope SNS does not define, on the kind of topic that carries it",
        fifo: true,
        attribute: "FifoThroughputScope",
        value: "Partition",
        sentence: "Invalid FifoThroughputScope: Partition",
        framed_by_the_member: true,
    },
    // ...and the conditions whose sentence NAMES the attribute, which say which input is at
    // fault themselves: a closed value set, a numeric range and a document grammar.
    Row {
        case: "a signature version SNS does not publish",
        fifo: false,
        attribute: "SignatureVersion",
        value: "3",
        sentence: "SignatureVersion: Invalid value '[3]'. Must be 1 or 2.",
        framed_by_the_member: false,
    },
    Row {
        case: "a message size below the documented floor",
        fifo: false,
        attribute: "MaximumMessageSize",
        value: "1023",
        sentence: "MaximumMessageSize: 1023 is not an integer between 1024 and 1048576 bytes",
        framed_by_the_member: false,
    },
    Row {
        case: "an access policy that is not a policy document",
        fifo: false,
        attribute: "Policy",
        value: "[]",
        sentence: "Policy Error: null",
        framed_by_the_member: false,
    },
];

/// The whole message a row expects from a call that carried the value in an `Attributes` MAP.
fn through_the_map(row: &Row) -> String {
    match row.framed_by_the_member {
        true => format!("Invalid parameter: Attributes Reason: {}", row.sentence),
        false => format!("Invalid parameter: {}", row.sentence),
    }
}

/// The whole message a row expects from the call that was given the attribute BY NAME.
fn through_the_name(row: &Row) -> String {
    format!("Invalid parameter: {}", row.sentence)
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
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{service}/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Amazon SNS call (API version 2010-03-31).
async fn sns(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "sns", "2010-03-31", action, members).await
}

/// Assert that the answer is the refusal this class carries, stating `message`.
///
/// The message is read through the DECODING reader: a sentence that quotes the rejected value
/// travels with the entities an XML text node cannot carry literally escaped, and what SNS
/// stated is the decoded text.
fn assert_refused(case: &str, status: u16, body: &str, message: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {body}"
    );
    assert_eq!(
        elements(body, "Code").first().map(String::as_str),
        Some("InvalidParameter"),
        "case {case:?}: expected the InvalidParameter code, got {body}"
    );
    assert_eq!(
        elements(body, "Message").first().map(String::as_str),
        Some(message),
        "case {case:?}: expected SNS's own sentence, got {body}"
    );
}

/// The `ResourceStatusReason` the stack recorded for `logical_id`'s `status` event, waited for:
/// a stack's events become readable as its own clock reaches them.
async fn recorded_reason(
    router: &MultiServiceRouter,
    stack: &str,
    logical_id: &str,
    status: &str,
) -> String {
    let looks = WAIT.as_millis() / LOOK.as_millis();
    for _ in 0..looks {
        let (code, body) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
        assert_eq!(
            code, 200,
            "DescribeStackEvents({stack:?}) must be answered, got {code} with {body}"
        );
        let found = members_of(&body).into_iter().find_map(|member| {
            let logical = elements(member, "LogicalResourceId").into_iter().next()?;
            let reported = elements(member, "ResourceStatus").into_iter().next()?;
            if logical != logical_id || reported != status {
                return None;
            }
            elements(member, "ResourceStatusReason").into_iter().next()
        });
        if let Some(reason) = found {
            return reason;
        }
        tokio::time::sleep(LOOK).await;
    }
    String::new()
}

/// The `StackStatus` the stack reports once it has stopped moving, or the last status seen.
async fn settled_stack(router: &MultiServiceRouter, stack: &str, wanted: &str) -> String {
    let looks = WAIT.as_millis() / LOOK.as_millis();
    let mut reported = String::new();
    for _ in 0..looks {
        let (code, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            code, 200,
            "DescribeStacks({stack:?}) must be answered, got {code} with {body}"
        );
        reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if reported == wanted {
            return reported;
        }
        tokio::time::sleep(LOOK).await;
    }
    reported
}

#[tokio::test]
async fn a_refused_topic_attribute_is_reported_against_the_member_it_arrived_in() {
    let router = build_router();
    // One topic of each kind, written to by the named-pair half of every row.
    let (status, body) = sns(&router, "CreateTopic", &[("Name", "member-frame-standard")]).await;
    assert_eq!(status, 200, "the standard topic must be created, got {body}");
    let standard = element(&body, "TopicArn");
    let mut ordered_params = vec![("Name", "member-frame-ordered.fifo")];
    ordered_params.push(("Attributes.entry.1.key", "FifoTopic"));
    ordered_params.push(("Attributes.entry.1.value", "true"));
    let (status, body) = sns(&router, "CreateTopic", &ordered_params).await;
    assert_eq!(status, 200, "the FIFO topic must be created, got {body}");
    let ordered = element(&body, "TopicArn");

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let topic = match row.fifo {
            true => &ordered,
            false => &standard,
        };
        let held_before = topic_attribute(&router, topic, row.attribute).await;

        // The named pair: the attribute is the whole input, so the sentence stands alone.
        let (status, body) = sns(
            &router,
            "SetTopicAttributes",
            &[
                ("TopicArn", topic),
                ("AttributeName", row.attribute),
                ("AttributeValue", row.value),
            ],
        )
        .await;
        assert_refused(case, status, &body, &through_the_name(row));

        // ...and the same value in a create's `Attributes` map, where the member frames a
        // sentence that names no attribute.
        let name = match row.fifo {
            true => format!("member-frame-created-{index:02}.fifo"),
            false => format!("member-frame-created-{index:02}"),
        };
        let mut params: Vec<(&str, &str)> = vec![
            ("Name", &name),
            ("Attributes.entry.1.key", row.attribute),
            ("Attributes.entry.1.value", row.value),
        ];
        if row.fifo {
            params.push(("Attributes.entry.2.key", "FifoTopic"));
            params.push(("Attributes.entry.2.value", "true"));
        }
        let (status, body) = sns(&router, "CreateTopic", &params).await;
        assert_refused(case, status, &body, &through_the_map(row));

        // A refused write changes nothing: the topic keeps the value it held, and the refused
        // create left no topic behind.
        let held_after = topic_attribute(&router, topic, row.attribute).await;
        assert_eq!(
            held_after, held_before,
            "case {case:?}: a refused write must leave the attribute as it was"
        );
        let (_, listed) = sns(&router, "ListTopics", &[]).await;
        assert!(
            !elements(&listed, "TopicArn")
                .iter()
                .any(|arn| arn.ends_with(&format!(":{name}"))),
            "case {case:?}: the refused CreateTopic must create no topic, got {listed}"
        );
    }
}

/// The value the topic holds for `attribute` right now, or `None` when it holds none.
async fn topic_attribute(
    router: &MultiServiceRouter,
    topic: &str,
    attribute: &str,
) -> Option<String> {
    let (status, body) = sns(router, "GetTopicAttributes", &[("TopicArn", topic)]).await;
    assert_eq!(
        status, 200,
        "GetTopicAttributes({topic:?}) must be answered, got {status} with {body}"
    );
    decoded_entries(&body).get(attribute).cloned()
}

/// A template's `AWS::SNS::Topic` carries its attributes in the same MAP member, so the
/// refusal CloudFormation forwards as the resource's `CREATE_FAILED` reason is the framed
/// sentence — and the stack that could not create the topic rolls back, leaving no topic.
#[tokio::test]
async fn a_stack_forwards_the_framed_sentence_as_the_topics_create_failed_reason() {
    let case = "a template declaring a tracing mode SNS does not define";
    let router = build_router();
    let stack = "member-frame-stack";
    let value = "Sampled";
    let topic_name = "member-frame-stack-topic";
    let template = serde_json::json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Topic": {
                "Type": "AWS::SNS::Topic",
                "Properties": { "TopicName": topic_name, "TracingConfig": value },
            },
        },
    })
    .to_string();
    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack must be accepted, got {status} with {body}"
    );

    let settled = settled_stack(&router, stack, "ROLLBACK_COMPLETE").await;
    assert_eq!(
        settled, "ROLLBACK_COMPLETE",
        "case {case:?}: the stack must roll back, got {settled:?}"
    );
    let reason = recorded_reason(&router, stack, "Topic", "CREATE_FAILED").await;
    let expected = format!("Invalid parameter: Attributes Reason: Invalid tracing config value: {value}");
    assert!(
        reason.contains(&expected),
        "case {case:?}: expected the resource's failure to carry {expected:?}, got {reason:?}"
    );
    let arn = format!("arn:aws:sns:{REGION}:000000000000:{topic_name}");
    let (status, body) = sns(&router, "GetTopicAttributes", &[("TopicArn", &arn)]).await;
    assert_ne!(
        status, 200,
        "case {case:?}: the rolled-back stack must leave no topic behind, got {body}"
    );
    let (_, listed) = sns(&router, "ListTopics", &[]).await;
    assert!(
        !elements(&listed, "TopicArn").contains(&arn),
        "case {case:?}: no topic of the rolled-back stack may be listed, got {listed}"
    );
}
