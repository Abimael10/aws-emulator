// SPDX-License-Identifier: CC0-1.0

//! The CHARACTER SET a published message attribute's NAME is held to.
//!
//! The RULE asserted here: a message attribute name may carry alphanumeric characters,
//! hyphens, underscores and periods and nothing else (the Amazon SNS Developer Guide,
//! "Amazon SNS message attributes"), so a name holding any other character is REFUSED —
//! `InvalidParameterValueException` (awsQueryError `ParameterValueInvalid`, httpError 400,
//! per `Publish`'s modelled errors) naming the offending character as the character
//! reference SNS states it by — and nothing is published or delivered; while a name made of
//! those characters is published and delivered under the name it was given.
//!
//! The rule is an ARGUMENT rule — it judges the attribute map alone, with no record read —
//! so it is the SAME rule for every call that publishes a message: an attribute map
//! `Publish` refuses cannot be one `PublishBatch` takes, and a batch is judged whole, so one
//! entry naming an attribute outside the set publishes none of them.
//!
//! The rows differ along what the rule generalises over: which character outside the set the
//! name carries (a space, a control character, punctuation on either side of the digits and
//! letters in code-point order, one outside ASCII, and one beyond the Basic Multilingual
//! Plane, whose reference is five hex digits wide), where in the name it stands, whether the
//! name is that character alone, which publishing call carries it, and where in a batch the
//! refused entry stands — and, on the accepted side, whether the name uses each piece of the
//! permitted punctuation, letters outside ASCII, letters of another script, or digits alone.

use super::sqs_subscription_delivery::{
    drain, element, emulator, given_queue, sns, sns_call,
};
use super::topics_subscriptions_publish::{xml_unescape, Sns};

/// The error code SNS states a refused parameter VALUE with.
const PARAMETER_VALUE_INVALID: &str = "ParameterValueInvalid";

/// The sentence SNS states for a name carrying `reference`, which is that character's code
/// point in uppercase hex.
fn refusal(reference: &str) -> String {
    format!(
        "Invalid non-alphanumeric character '#x{reference}' was found in the message \
         attribute name. Can only include alphanumeric characters, hyphens, underscores, \
         or dots."
    )
}

/// One refused name: the case, the name, and the character reference the refusal names.
const REFUSED: &[(&str, &str, &str)] = &[
    ("a space inside the name", "a b", "20"),
    ("a tab inside the name", "a\tb", "9"),
    ("a leading exclamation mark", "!ab", "21"),
    ("a hash inside the name", "a#b", "23"),
    ("a slash inside the name", "a/b", "2F"),
    ("a colon inside the name", "a:b", "3A"),
    ("a trailing tilde", "ab~", "7E"),
    (
        "a character outside ASCII that is no letter",
        "a\u{20AC}b",
        "20AC",
    ),
];

/// One accepted name: the case and the name.
const ACCEPTED: &[(&str, &str)] = &[
    ("every piece of the permitted punctuation", "A-b_c.d9"),
    ("letters outside ASCII", "café"),
    ("periods between the parts", "a.b.c"),
    ("digits alone", "90210"),
    ("one letter", "n"),
];

/// A name carrying a character outside the set is refused, with the code, the status and the
/// sentence SNS states it with.
#[tokio::test]
async fn a_name_outside_the_character_set_is_refused() {
    let sns = Sns::new();
    let topic = sns.topic("attribute-name-charset").await;
    for (case, name, reference) in REFUSED {
        let answer = sns
            .call(
                "Publish",
                &[
                    ("TopicArn", &topic),
                    ("Message", "m"),
                    ("MessageAttributes.entry.1.Name", name),
                    ("MessageAttributes.entry.1.Value.DataType", "String"),
                    ("MessageAttributes.entry.1.Value.StringValue", "v"),
                ],
            )
            .await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400, got {} / {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.code(),
            PARAMETER_VALUE_INVALID,
            "case {case:?}: expected the {PARAMETER_VALUE_INVALID} code, got {}",
            answer.body
        );
        assert_eq!(
            answer.element("Message").as_deref(),
            Some(refusal(reference).as_str()),
            "case {case:?}: expected the service's own sentence, got {}",
            answer.body
        );
        assert!(
            answer.element("MessageId").is_none(),
            "case {case:?}: a refused publish mints no message id, got {}",
            answer.body
        );
    }
}

/// One refused name reaching the service through the BATCHED publishing call as well: the
/// case, the name, and the character reference the refusal names. None of these characters
/// is one [`REFUSED`] carries, and the last stands beyond the Basic Multilingual Plane, whose
/// reference is five hex digits wide.
const REFUSED_THROUGH_EVERY_CALL: &[(&str, &str, &str)] = &[
    ("a plus between the parts", "a+b", "2B"),
    ("a comma between the parts", "a,b", "2C"),
    ("a leading at sign", "@name", "40"),
    ("a trailing opening bracket", "name[", "5B"),
    ("a name that is one backtick", "`", "60"),
    ("an opening brace between the parts", "a{b", "7B"),
    ("a control character inside the name", "a\u{7F}b", "7F"),
    ("a newline inside the name", "a\nb", "A"),
    ("a character beyond the BMP", "a\u{1F600}b", "1F600"),
];

/// The form parameters of a `Publish` carrying the single String attribute `name`.
fn one_publish(topic: &str, name: &str) -> Vec<(String, String)> {
    vec![
        ("TopicArn".to_string(), topic.to_string()),
        ("Message".to_string(), "m".to_string()),
        (
            "MessageAttributes.entry.1.Name".to_string(),
            name.to_string(),
        ),
        (
            "MessageAttributes.entry.1.Value.DataType".to_string(),
            "String".to_string(),
        ),
        (
            "MessageAttributes.entry.1.Value.StringValue".to_string(),
            "v".to_string(),
        ),
    ]
}

/// The form parameters of a `PublishBatch` whose entries carry one String attribute each,
/// named by `names` in request order. Entry `n` publishes its own attribute name as its
/// message, so a delivered notification says which name it belongs to.
fn one_batch(topic: &str, names: &[&str]) -> Vec<(String, String)> {
    let mut params = vec![("TopicArn".to_string(), topic.to_string())];
    for (index, name) in names.iter().enumerate() {
        let prefix = format!("PublishBatchRequestEntries.member.{}", index + 1);
        params.push((format!("{prefix}.Id"), format!("e{}", index + 1)));
        params.push((format!("{prefix}.Message"), (*name).to_string()));
        params.push((
            format!("{prefix}.MessageAttributes.entry.1.Name"),
            (*name).to_string(),
        ));
        params.push((
            format!("{prefix}.MessageAttributes.entry.1.Value.DataType"),
            "String".to_string(),
        ));
        params.push((
            format!("{prefix}.MessageAttributes.entry.1.Value.StringValue"),
            "v".to_string(),
        ));
    }
    params
}

/// The owned form parameters as the wire harnesses take them.
fn borrowed(params: &[(String, String)]) -> Vec<(&str, &str)> {
    params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

/// A name carrying a character outside the set is refused the same way by EVERY publishing
/// call: the attribute rules judge the attribute map alone, so the batched call holds an
/// entry's attributes to exactly the rules a single publish holds them to.
#[tokio::test]
async fn a_name_outside_the_character_set_is_refused_by_every_publishing_call() {
    let sns = Sns::new();
    let topic = sns.topic("attribute-name-charset-per-call").await;
    for (case, name, reference) in REFUSED_THROUGH_EVERY_CALL {
        let single = one_publish(&topic, name);
        let batched = one_batch(&topic, &[name]);
        for (call, params) in [("Publish", &single), ("PublishBatch", &batched)] {
            let answer = sns.call(call, &borrowed(params)).await;
            assert_eq!(
                answer.status, 400,
                "case {case:?} through {call}: expected HTTP 400, got {} / {}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.code(),
                PARAMETER_VALUE_INVALID,
                "case {case:?} through {call}: expected the {PARAMETER_VALUE_INVALID} code, \
                 got {}",
                answer.body
            );
            assert_eq!(
                answer.element("Message").as_deref(),
                Some(refusal(reference).as_str()),
                "case {case:?} through {call}: expected the service's own sentence, got {}",
                answer.body
            );
            assert!(
                answer.element("MessageId").is_none(),
                "case {case:?} through {call}: a refused publish mints no message id, got {}",
                answer.body
            );
        }
    }
}

/// A name made of the characters the set admits is published, and the notification carries
/// that attribute under the name it was given.
#[tokio::test]
async fn a_name_inside_the_character_set_is_published_and_delivered() {
    let emulator = emulator();
    let topic = element(
        &sns(&emulator, "CreateTopic", &[("Name", "attribute-names")]).await,
        "TopicArn",
    );
    let (url, arn) = given_queue(&emulator, "attribute-names-queue").await;
    sns(
        &emulator,
        "Subscribe",
        &[
            ("TopicArn", &topic),
            ("Protocol", "sqs"),
            ("Endpoint", &arn),
            ("ReturnSubscriptionArn", "true"),
        ],
    )
    .await;

    for (case, name) in ACCEPTED {
        sns(
            &emulator,
            "Publish",
            &[
                ("TopicArn", &topic),
                ("Message", case),
                ("MessageAttributes.entry.1.Name", name),
                ("MessageAttributes.entry.1.Value.DataType", "String"),
                ("MessageAttributes.entry.1.Value.StringValue", "v"),
            ],
        )
        .await;
        let delivered = drain(&emulator, &url).await;
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: the publish must be delivered, got {delivered:?}"
        );
        let body = &delivered[0].0;
        let envelope: serde_json::Value = serde_json::from_str(body)
            .unwrap_or_else(|error| panic!("case {case:?}: {error} for {body}"));
        assert_eq!(
            envelope["MessageAttributes"][*name]["Value"].as_str(),
            Some("v"),
            "case {case:?}: the attribute must be carried under {name:?}, got {body}"
        );
    }
}

/// Names the set admits reach a subscribed queue through the BATCHED publishing call too,
/// each entry's notification carrying its own attribute under the name it was given —
/// including names written in another script.
#[tokio::test]
async fn names_inside_the_character_set_are_published_and_delivered_by_a_batch() {
    // (case, the name) — the scripts and the punctuation a name may be written with.
    let accepted: &[(&str, &str)] = &[
        ("letters of another script", "日本語"),
        ("Greek letters with a digit", "Δelta9"),
        (
            "the permitted punctuation between letters and digits",
            "Mixed-Case_9.x",
        ),
        ("one digit", "7"),
    ];

    let emulator = emulator();
    let topic = element(
        &sns(
            &emulator,
            "CreateTopic",
            &[("Name", "batched-attribute-names")],
        )
        .await,
        "TopicArn",
    );
    let (url, arn) = given_queue(&emulator, "batched-attribute-names-queue").await;
    sns(
        &emulator,
        "Subscribe",
        &[
            ("TopicArn", &topic),
            ("Protocol", "sqs"),
            ("Endpoint", &arn),
            ("ReturnSubscriptionArn", "true"),
        ],
    )
    .await;

    let names: Vec<&str> = accepted.iter().map(|(_, name)| *name).collect();
    let params = one_batch(&topic, &names);
    sns(&emulator, "PublishBatch", &borrowed(&params)).await;

    let delivered = drain(&emulator, &url).await;
    assert_eq!(
        delivered.len(),
        accepted.len(),
        "every entry of the batch must be delivered, got {delivered:?}"
    );
    // Each notification is matched to its row by the message that entry published, because
    // the queue hands back what it holds in no promised order.
    let envelopes: Vec<serde_json::Value> = delivered
        .iter()
        .map(|(body, _)| {
            serde_json::from_str(body).unwrap_or_else(|error| panic!("{error} for {body}"))
        })
        .collect();
    for (case, name) in accepted {
        let envelope = envelopes
            .iter()
            .find(|envelope| envelope["Message"].as_str() == Some(name))
            .unwrap_or_else(|| {
                panic!("case {case:?}: no notification published {name:?}, got {envelopes:?}")
            });
        assert_eq!(
            envelope["MessageAttributes"][*name]["Value"].as_str(),
            Some("v"),
            "case {case:?}: the attribute must be carried under {name:?}, got {envelope}"
        );
        assert_eq!(
            envelope["MessageAttributes"][*name]["Type"].as_str(),
            Some("String"),
            "case {case:?}: the attribute keeps the type it was published with, got {envelope}"
        );
    }
}

/// The attribute rules are judged for every entry BEFORE any entry is published, so a batch
/// holding one name outside the set publishes nothing: it is refused with that name's own
/// sentence and no entry of it reaches a subscribed queue, whichever position the refused
/// entry stands in.
#[tokio::test]
async fn a_batch_holding_one_name_outside_the_character_set_delivers_nothing() {
    // (case, the entries' attribute names, the reference the refusal names)
    let rows: &[(&str, &[&str], &str)] = &[
        ("the refused entry first", &["a+b", "kept"], "2B"),
        ("the refused entry last", &["kept", "a,b"], "2C"),
        (
            "the refused entry between accepted ones",
            &["kept", "a{b", "also-kept"],
            "7B",
        ),
    ];

    let emulator = emulator();
    let topic = element(
        &sns(
            &emulator,
            "CreateTopic",
            &[("Name", "batched-charset-refusal")],
        )
        .await,
        "TopicArn",
    );
    let (url, arn) = given_queue(&emulator, "batched-charset-refusal-queue").await;
    sns(
        &emulator,
        "Subscribe",
        &[
            ("TopicArn", &topic),
            ("Protocol", "sqs"),
            ("Endpoint", &arn),
            ("ReturnSubscriptionArn", "true"),
        ],
    )
    .await;

    for (case, names, reference) in rows {
        let params = one_batch(&topic, names);
        let (status, body) = sns_call(&emulator, "PublishBatch", &borrowed(&params)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} / {body}"
        );
        assert_eq!(
            xml_unescape(&element(&body, "Code")),
            PARAMETER_VALUE_INVALID,
            "case {case:?}: expected the {PARAMETER_VALUE_INVALID} code, got {body}"
        );
        assert_eq!(
            xml_unescape(&element(&body, "Message")),
            refusal(reference),
            "case {case:?}: expected the service's own sentence, got {body}"
        );
        let delivered = drain(&emulator, &url).await;
        assert!(
            delivered.is_empty(),
            "case {case:?}: a refused batch delivers none of its entries, got {delivered:?}"
        );
    }
}
