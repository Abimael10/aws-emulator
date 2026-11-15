// SPDX-License-Identifier: CC0-1.0

//! WHICH key a publish is judged by, and over which requests that judgement holds — driven over
//! the wire through the real `MultiServiceRouter` with KMS mounted on the same engine, so what a
//! client sees is what this suite asserts.
//!
//! The RULE asserted here: a topic carrying `KmsMasterKeyId` has every message published to it
//! sealed under the key THAT topic's identifier names, so a publish is refused when, and only
//! when, that one key cannot be used in a cryptographic operation at the moment of the call —
//! `KMSDisabled` (HTTP 400) for a key that is merely disabled. The refusal is therefore scoped to
//! the key the topic names rather than to the endpoint: a second topic on another, usable key and
//! a topic on no customer managed key go on being accepted while it holds, two topics naming one
//! disabled key are both refused, and the refusal stands on every retry while the key stays
//! disabled rather than being a one-off. Because the judgement is about the TOPIC the request
//! addresses, it is the same whichever member of the request names that topic: "If you don't
//! specify a value for the TopicArn parameter, you must specify a value for the PhoneNumber or
//! TargetArn parameters"
//! (`smithy_query(query="doc",shape="PublishInput",member="TopicArn")`), so a topic ARN carried
//! by `TargetArn` addresses the same topic as one carried by `TopicArn`.
//!
//! The rows differ along what the rule generalises over: which MEMBER names the topic
//! (`TopicArn`, `TargetArn`), how many topics share a key and how they name it (a key ARN, an
//! alias), which of several keys is disabled when the publish arrives (neither, one, both), and
//! how many publishes are made while each key stands as it is.
//!
//! The single-shot legs of the key-state rule (one publish per key condition) are
//! `topic_encryption_key_state.rs`, and the re-resolution of one topic's key per publish is
//! `topic_encryption_key_resolved_per_publish.rs`; what this suite adds is the SCOPE of the
//! refusal — the member that addresses the topic, and the other topics and keys it must leave
//! alone.

use std::collections::HashSet;

use serde_json::json;

use crate::sqs_subscription_delivery::{emulator, kms};
use crate::topic_encryption_key_state::{
    assert_relayed, given_key, given_topic, publish_through, set_key_enabled,
};

/// The awsQuery error code a publish under a merely disabled key is refused with: "`KMSDisabled`
/// (HTTP 400) — The request was rejected because the specified Amazon Web Services KMS key isn't
/// enabled" (`smithy_query(query="operation",operation="Publish")`).
const DISABLED_CODE: &str = "KMSDisabled";

/// The HTTP status that refusal carries, from the same model entry.
const DISABLED_STATUS: u16 = 400;

/// KMS's own error code behind the refusal, which the relayed sentence reports.
const KMS_DISABLED_CODE: &str = "DisabledException";

/// The members of a `Publish` request that can carry the ARN of the topic published to: a
/// `TargetArn` "is returned when making a call with the CreatePlatformEndpoint action"
/// (`smithy_query(query="doc",shape="PublishInput",member="TargetArn")`) but a topic ARN is
/// equally a target, so both members address a topic.
const DESTINATION_MEMBERS: &[&str] = &["TopicArn", "TargetArn"];

/// How many publishes each topic is given per leg below: a refusal driven by the key's state
/// stands on every call while that state holds, so one look cannot tell it from a one-off.
const ATTEMPTS_PER_LEG: usize = 3;

#[tokio::test]
async fn the_topics_key_is_judged_whichever_member_names_the_topic() {
    let emulator = emulator();

    for member in DESTINATION_MEMBERS {
        let (key_id, key_arn) = given_key(&emulator).await;
        let topic = given_topic(
            &emulator,
            &format!("addressed-by-{}", member.to_lowercase()),
            Some(&key_arn),
        )
        .await;

        // The control, with nothing yet broken: the key is enabled, so the publish is accepted
        // through this member and answers with a message id.
        let case = format!("{member} while the key is enabled");
        let (status, code, sentence) =
            publish_through(&emulator, member, &topic, "while enabled").await;
        assert_eq!(
            (status, code.as_str()),
            (200, ""),
            "case {case:?}: a publish under an enabled key must be accepted, got {status} / \
             {code} / {sentence:?}"
        );
        assert!(
            !sentence.is_empty(),
            "case {case:?}: an accepted publish must answer with a MessageId, got {sentence:?}"
        );

        set_key_enabled(&emulator, &key_id, false).await;
        for attempt in 1..=ATTEMPTS_PER_LEG {
            let case = format!("{member} attempt {attempt} while the key is disabled");
            let (status, code, sentence) =
                publish_through(&emulator, member, &topic, "while disabled").await;
            assert_eq!(
                (status, code.as_str()),
                (DISABLED_STATUS, DISABLED_CODE),
                "case {case:?}: a publish under a disabled key must be refused {DISABLED_CODE}, \
                 got {status} / {code} / {sentence:?}"
            );
            assert_relayed(
                &case,
                &sentence,
                &format!("{key_arn} is disabled."),
                KMS_DISABLED_CODE,
            );
        }

        set_key_enabled(&emulator, &key_id, true).await;
        let case = format!("{member} after the key is enabled again");
        let (status, code, sentence) =
            publish_through(&emulator, member, &topic, "after enabling").await;
        assert_eq!(
            (status, code.as_str()),
            (200, ""),
            "case {case:?}: a publish under a re-enabled key must be accepted again, got \
             {status} / {code} / {sentence:?}"
        );
    }
}

/// One leg of the scope case: which of the two keys can be used while the publishes of that leg
/// are made. The topics themselves are never touched between legs.
struct Leg {
    case: &'static str,
    first_enabled: bool,
    second_enabled: bool,
}

/// The legs the scope case runs, in order — every combination of the two keys' conditions, and
/// back again, so neither key's condition can be mistaken for the endpoint's.
const LEGS: &[Leg] = &[
    Leg {
        case: "both keys are enabled",
        first_enabled: true,
        second_enabled: true,
    },
    Leg {
        case: "only the first key is disabled",
        first_enabled: false,
        second_enabled: true,
    },
    Leg {
        case: "both keys are disabled",
        first_enabled: false,
        second_enabled: false,
    },
    Leg {
        case: "only the second key is disabled",
        first_enabled: true,
        second_enabled: false,
    },
    Leg {
        case: "both keys are enabled again",
        first_enabled: true,
        second_enabled: true,
    },
];

/// Which key a topic of the scope case stands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnKey {
    /// The first of the two keys.
    First,
    /// The second of the two keys.
    Second,
    /// No customer managed key at all.
    None,
}

#[tokio::test]
async fn a_disabled_key_refuses_only_the_topics_that_name_it() {
    let emulator = emulator();
    let (first_key_id, first_key_arn) = given_key(&emulator).await;
    let (second_key_id, second_key_arn) = given_key(&emulator).await;

    // The first key is also reachable by an alias, so one of the topics on it names it in
    // another form: the refusal follows the KEY, not the spelling the topic carries.
    let alias = "alias/shared-topic-key";
    kms(
        &emulator,
        "CreateAlias",
        &json!({"AliasName": alias, "TargetKeyId": first_key_id}),
    )
    .await;

    // (case, which key the topic stands on, the `KmsMasterKeyId` it carries)
    let topics: Vec<(&str, OnKey, Option<String>)> = vec![
        (
            "a topic naming the first key by its ARN",
            OnKey::First,
            Some(first_key_arn.clone()),
        ),
        (
            "a second topic naming the first key by an alias",
            OnKey::First,
            Some(alias.to_string()),
        ),
        (
            "a topic naming the second key by its ARN",
            OnKey::Second,
            Some(second_key_arn.clone()),
        ),
        ("a topic on no customer managed key", OnKey::None, None),
    ];

    let mut arns: Vec<(&str, OnKey, String)> = Vec::new();
    for (index, (case, on_key, master_key_id)) in topics.iter().enumerate() {
        let arn = given_topic(
            &emulator,
            &format!("key-scope-{index}"),
            master_key_id.as_deref(),
        )
        .await;
        arns.push((case, *on_key, arn));
    }

    // Every message id this case was answered with: an accepted publish mints one of its own, so
    // an acceptance is never one answer served twice.
    let mut minted: HashSet<String> = HashSet::new();

    for leg in LEGS {
        set_key_enabled(&emulator, &first_key_id, leg.first_enabled).await;
        set_key_enabled(&emulator, &second_key_id, leg.second_enabled).await;

        for (topic_case, on_key, topic) in &arns {
            let refused_by = match on_key {
                OnKey::First if !leg.first_enabled => Some(&first_key_arn),
                OnKey::Second if !leg.second_enabled => Some(&second_key_arn),
                _ => None,
            };
            for attempt in 1..=ATTEMPTS_PER_LEG {
                let case = format!(
                    "{topic_case}, publish {attempt} of {ATTEMPTS_PER_LEG} while {}",
                    leg.case
                );
                let (status, code, sentence) =
                    publish_through(&emulator, "TopicArn", topic, "sealed under the topic's key")
                        .await;
                match refused_by {
                    None => {
                        assert_eq!(
                            (status, code.as_str()),
                            (200, ""),
                            "case {case:?}: the topic's own key is usable, so the publish must be \
                             accepted, got {status} / {code} / {sentence:?}"
                        );
                        assert!(
                            minted.insert(sentence.clone()),
                            "case {case:?}: every accepted publish mints its own MessageId, got \
                             the already-answered {sentence:?}"
                        );
                    }
                    Some(key_arn) => {
                        assert_eq!(
                            (status, code.as_str()),
                            (DISABLED_STATUS, DISABLED_CODE),
                            "case {case:?}: the topic's own key is disabled, so the publish must \
                             be refused {DISABLED_CODE}, got {status} / {code} / {sentence:?}"
                        );
                        // The refusal names the key the TOPIC stands on, which is how a client
                        // learns which key to enable again.
                        assert_relayed(
                            &case,
                            &sentence,
                            &format!("{key_arn} is disabled."),
                            KMS_DISABLED_CODE,
                        );
                    }
                }
            }
        }
    }
}
