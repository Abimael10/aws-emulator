// SPDX-License-Identifier: CC0-1.0

//! A filter policy naming a match SNS does not perform is refused when it is WRITTEN,
//! driven end-to-end over the awsQuery wire.
//!
//! The RULE asserted here: a stored `FilterPolicy` decides, for the life of the
//! subscription, which messages its endpoint receives ("the simple JSON object that lets
//! your subscriber receive only a subset of messages", `SubscribeInput$Attributes` doc),
//! so a condition naming a match type the service does not perform is refused at the
//! write — the modelled `InvalidParameterException` (awsQuery code `InvalidParameter`,
//! HTTP 400 — `smithy_query(query="operation",operation="SetSubscriptionAttributes")`) —
//! rather than stored to withhold every message in silence. The sentence is the reader's
//! own: the attribute, the reason, and the position in the policy the reader stopped at.
//! A refused write leaves the policy the subscription holds, and a policy of the
//! operators the service does perform round-trips byte for byte.
//!
//! The vocabulary a NEGATION may name is narrower than the vocabulary of a condition:
//! `anything-but` excludes what a STRING pattern picks out, and a match type that is not
//! a string pattern (a network, a numeric range) names nothing it can exclude.
//!
//! The rows differ along what the rule generalises over: the match type the condition
//! names, whether it stands on its own or inside the one operator that nests another,
//! and the kind of argument the reader stopped on (a string, a list, a number).

use super::topics_subscriptions_publish::{attribute_map, Answer, Sns};
use super::validation_rules::subscribe_queue;

/// The status and awsQuery code SNS refuses a filter policy it would not apply with.
const REFUSAL_STATUS: u16 = 400;
const REFUSAL_CODE: &str = "InvalidParameter";

/// A queue a subscription of this suite delivers to.
const QUEUE: &str = "arn:aws:sqs:us-east-1:000000000000:grammar-queue";

/// The policy a subscription of this suite holds before each refused write, so a refusal
/// is seen to leave the stored policy standing.
const HELD: &str = r#"{"kind":["alert"]}"#;

/// Write `policy` as the subscription's `FilterPolicy`.
async fn set_policy(sns: &Sns, subscription: &str, policy: &str) -> Answer {
    sns.call(
        "SetSubscriptionAttributes",
        &[
            ("SubscriptionArn", subscription),
            ("AttributeName", "FilterPolicy"),
            ("AttributeValue", policy),
        ],
    )
    .await
}

/// The `FilterPolicy` the subscription reads back as.
async fn stored_policy(sns: &Sns, subscription: &str) -> Option<String> {
    attribute_map(
        &sns.call(
            "GetSubscriptionAttributes",
            &[("SubscriptionArn", subscription)],
        )
        .await,
    )
    .get("FilterPolicy")
    .cloned()
}

/// The COLUMN the reader reports for a refusal it decided on the node written `node` in
/// `document`: the reader reports the position just past that node's first character —
/// the opening quote or bracket of a string, a list or an object, which is as much of it
/// as it had read — so the column is one past where the node begins, counting from 1.
fn column_of(document: &str, node: &str) -> usize {
    let at = document
        .find(node)
        .unwrap_or_else(|| panic!("{node:?} is not part of {document:?}"));
    at + 2
}

/// The sentence the policy READER states for a fault it decided on the node written
/// `node` in `document`: its own reason, and the position in the policy it stopped at.
/// The frame around it is the member the document arrived in
/// ([`a_subscription_attribute_refusal_is_framed_by_the_member_it_arrived_in`]).
fn reader_sentence(document: &str, reason: &str, node: &str) -> String {
    format!(
        "{reason}\n at [Source: (String)\"{document}\"; line: 1, column: {}]",
        column_of(document, node)
    )
}

/// The whole sentence a policy whose grammar SNS could not read is refused with when the
/// attribute was given BY NAME, which is the path this suite writes through.
fn refusal(document: &str, reason: &str, node: &str) -> String {
    format!(
        "Invalid parameter: FilterPolicy: {}",
        reader_sentence(document, reason, node)
    )
}

/// RULE: a condition naming a match SNS does not perform is refused at the write, and the
/// policy the subscription holds is left standing.
#[tokio::test]
async fn a_filter_policy_naming_a_match_the_service_does_not_perform_is_refused() {
    let sns = Sns::new();
    let topic = sns.topic("filter-policy-grammar").await;
    let subscribed = sns
        .call(
            "Subscribe",
            &[
                ("TopicArn", &topic),
                ("Protocol", "sqs"),
                ("Endpoint", QUEUE),
                ("ReturnSubscriptionArn", "true"),
            ],
        )
        .await;
    let arn = subscribed
        .element("SubscriptionArn")
        .unwrap_or_else(|| panic!("no SubscriptionArn in {}", subscribed.body));
    assert_eq!(
        set_policy(&sns, &arn, HELD).await.status,
        200,
        "the policy the refusals are judged against must be stored, got {}",
        subscribed.body
    );

    // (case, the policy, the reason the reader states, the node it stopped on)
    let rows: &[(&str, &str, &str, &str)] = &[
        (
            "a network inside a negation",
            r#"{"ip":[{"anything-but":{"cidr":"10.0.0.0/8"}}]}"#,
            "Unsupported anything-but pattern: cidr",
            r#""10.0.0.0/8""#,
        ),
        (
            "a numeric range inside a negation",
            r#"{"price":[{"anything-but":{"numeric":[">",7]}}]}"#,
            "Unsupported anything-but pattern: numeric",
            r#"[">",7]"#,
        ),
        (
            "a negation inside a negation",
            r#"{"kind":[{"anything-but":{"anything-but":"excluded"}}]}"#,
            "Unsupported anything-but pattern: anything-but",
            r#""excluded""#,
        ),
        (
            "a presence test whose pattern is not a flag",
            r#"{"coupon":[{"exists":"yes"}]}"#,
            "exists match pattern must be either true or false.",
            r#""yes""#,
        ),
        (
            "a numeric relation the service does not compare with",
            r#"{"price":[{"numeric":["!=",7]}]}"#,
            "Unrecognized numeric range operator: !=",
            "7",
        ),
        (
            "a match type the service does not implement",
            r#"{"price":[{"greater-than":5}]}"#,
            "Unrecognized match type greater-than",
            "5",
        ),
    ];

    for (case, policy, reason, node) in rows {
        let expected = refusal(policy, reason, node);
        let answer = set_policy(&sns, &arn, policy).await;
        assert_eq!(
            answer.status, REFUSAL_STATUS,
            "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {} / {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.code(),
            REFUSAL_CODE,
            "case {case:?}: expected the {REFUSAL_CODE} code, got {}",
            answer.body
        );
        assert_eq!(
            answer.element("Message").as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected the reader's own sentence, got {}",
            answer.body
        );
        let held = stored_policy(&sns, &arn).await;
        assert_eq!(
            held.as_deref(),
            Some(HELD),
            "case {case:?}: a refused write must leave the stored policy as it was, got \
             {held:?}"
        );
    }
}

/// RULE: whichever layer refused the document, its sentence is reported under the member
/// the document arrived in — a write carried in `Subscribe`'s `Attributes` map is told
/// which member it has to change, and a write that was given the attribute BY NAME needs
/// no such frame, the name being the whole input. `Subscribe` has no `FilterPolicy`
/// member of its own (`smithy_query(query="operation",operation="Subscribe")` lists
/// `TopicArn`, `Protocol`, `Endpoint`, `Attributes` and `ReturnSubscriptionArn`), so the
/// member a caller of it can fix is the map.
///
/// The rows differ along what the rule generalises over: the LAYER that refused the value
/// (the policy reader's own grammar, a published policy limit, an attribute's own value
/// set) and, within the reader, the kind of condition it stopped on.
#[tokio::test]
async fn a_subscription_attribute_refusal_is_framed_by_the_member_it_arrived_in() {
    let sns = Sns::new();
    let topic = sns.topic("filter-policy-framing").await;
    let subscribed = sns
        .call(
            "Subscribe",
            &[
                ("TopicArn", &topic),
                ("Protocol", "sqs"),
                ("Endpoint", QUEUE),
                ("ReturnSubscriptionArn", "true"),
            ],
        )
        .await;
    let arn = subscribed
        .element("SubscriptionArn")
        .unwrap_or_else(|| panic!("no SubscriptionArn in {}", subscribed.body));

    // Six keys: one past the published filter-policy key limit, which the service refuses
    // as a constraint on the policy as a whole rather than as something the reader could
    // not read.
    let six_keys = r#"{"a":["1"],"b":["1"],"c":["1"],"d":["1"],"e":["1"],"f":["1"]}"#;
    // (case, attribute, value, the sentence the refusing layer states)
    let rows: &[(&str, &str, &str, String)] = &[
        (
            "a numeric relation given a non-numeric operand",
            "FilterPolicy",
            r#"{"price":[{"numeric":[">","abc"]}]}"#,
            reader_sentence(
                r#"{"price":[{"numeric":[">","abc"]}]}"#,
                "Value of > must be numeric",
                r#""abc""#,
            ),
        ),
        (
            "a match type the service does not implement",
            "FilterPolicy",
            r#"{"price":[{"greater-than":5}]}"#,
            reader_sentence(
                r#"{"price":[{"greater-than":5}]}"#,
                "Unrecognized match type greater-than",
                "5",
            ),
        ),
        (
            "a presence test whose pattern is not a flag",
            "FilterPolicy",
            r#"{"coupon":[{"exists":"yes"}]}"#,
            reader_sentence(
                r#"{"coupon":[{"exists":"yes"}]}"#,
                "exists match pattern must be either true or false.",
                r#""yes""#,
            ),
        ),
        (
            "a condition whose network is no network",
            "FilterPolicy",
            r#"{"source":[{"cidr":"10.0.0.0/33"}]}"#,
            reader_sentence(
                r#"{"source":[{"cidr":"10.0.0.0/33"}]}"#,
                "source: cidr takes an IPv4 or IPv6 CIDR block",
                r#""10.0.0.0/33""#,
            ),
        ),
        (
            "a policy one key past the published limit",
            "FilterPolicy",
            six_keys,
            "Filter policy can not have more than 5 keys".to_string(),
        ),
        (
            "a flag attribute given a word that is no flag",
            "RawMessageDelivery",
            "yes",
            "Invalid value [yes]. Must be true or false.".to_string(),
        ),
    ];

    for (index, (case, attribute, value, sentence)) in rows.iter().enumerate() {
        let through_the_map = subscribe_queue(
            &sns,
            &topic,
            &format!("arn:aws:sqs:us-east-1:000000000000:framed-queue-{index:02}"),
            &[(attribute, value)],
        )
        .await;
        let expected = format!("Invalid parameter: Attributes Reason: {attribute}: {sentence}");
        assert_eq!(
            through_the_map.status, REFUSAL_STATUS,
            "case {case:?}: Subscribe expected HTTP {REFUSAL_STATUS}, got {} / {}",
            through_the_map.status, through_the_map.body
        );
        assert_eq!(
            through_the_map.code(),
            REFUSAL_CODE,
            "case {case:?}: Subscribe expected the {REFUSAL_CODE} code, got {}",
            through_the_map.body
        );
        assert_eq!(
            through_the_map.element("Message").as_deref(),
            Some(expected.as_str()),
            "case {case:?}: Subscribe must name the member the map arrived in, got {}",
            through_the_map.body
        );

        let by_name = sns
            .call(
                "SetSubscriptionAttributes",
                &[
                    ("SubscriptionArn", &arn),
                    ("AttributeName", attribute),
                    ("AttributeValue", value),
                ],
            )
            .await;
        let expected = format!("Invalid parameter: {attribute}: {sentence}");
        assert_eq!(
            by_name.status, REFUSAL_STATUS,
            "case {case:?}: SetSubscriptionAttributes expected HTTP {REFUSAL_STATUS}, got \
             {} / {}",
            by_name.status, by_name.body
        );
        assert_eq!(
            by_name.code(),
            REFUSAL_CODE,
            "case {case:?}: SetSubscriptionAttributes expected the {REFUSAL_CODE} code, \
             got {}",
            by_name.body
        );
        assert_eq!(
            by_name.element("Message").as_deref(),
            Some(expected.as_str()),
            "case {case:?}: an attribute given by name needs no member frame, got {}",
            by_name.body
        );
    }
}

/// The other side of the rule: every pattern a negation DOES exclude, and the conditions
/// those patterns stand in on their own, are stored exactly as they were written.
#[tokio::test]
async fn a_filter_policy_of_the_matches_the_service_performs_round_trips() {
    let sns = Sns::new();
    let topic = sns.topic("filter-policy-accepted").await;
    let subscribed = sns
        .call(
            "Subscribe",
            &[
                ("TopicArn", &topic),
                ("Protocol", "sqs"),
                ("Endpoint", QUEUE),
                ("ReturnSubscriptionArn", "true"),
            ],
        )
        .await;
    let arn = subscribed
        .element("SubscriptionArn")
        .unwrap_or_else(|| panic!("no SubscriptionArn in {}", subscribed.body));

    // The string patterns a negation excludes, beside the bare value and list forms it
    // takes, and the same patterns outside a negation.
    let rows: &[(&str, &str)] = &[
        ("a negated value", r#"{"kind":[{"anything-but":"silence"}]}"#),
        ("a negated number", r#"{"price":[{"anything-but":5}]}"#),
        ("a negated list", r#"{"kind":[{"anything-but":["silence","noise"]}]}"#),
        ("a negated prefix", r#"{"kind":[{"anything-but":{"prefix":"al"}}]}"#),
        ("a negated suffix", r#"{"kind":[{"anything-but":{"suffix":"rt"}}]}"#),
        (
            "a negated case-folded value",
            r#"{"kind":[{"anything-but":{"equals-ignore-case":"ALERT"}}]}"#,
        ),
        ("a negated wildcard", r#"{"kind":[{"anything-but":{"wildcard":"al*rt"}}]}"#),
        (
            "a negated list of prefixes",
            r#"{"kind":[{"anything-but":{"prefix":["al","wa"]}}]}"#,
        ),
        ("a network", r#"{"ip":[{"cidr":"10.0.0.0/8"}]}"#),
        ("a numeric range", r#"{"price":[{"numeric":[">",7,"<=",9]}]}"#),
        ("a presence test", r#"{"coupon":[{"exists":true}]}"#),
    ];

    for (case, policy) in rows {
        let answer = set_policy(&sns, &arn, policy).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: a match the service performs must be accepted, got {} / {}",
            answer.status, answer.body
        );
        let held = stored_policy(&sns, &arn).await;
        assert_eq!(
            held.as_deref(),
            Some(*policy),
            "case {case:?}: the policy must read back as it was written, got {held:?}"
        );
    }
}
