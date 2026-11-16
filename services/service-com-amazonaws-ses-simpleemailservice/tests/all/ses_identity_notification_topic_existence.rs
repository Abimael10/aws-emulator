// SPDX-License-Identifier: CC0-1.0

//! The Amazon SNS topic an identity's notifications are published to, resolved
//! against Amazon SNS before it is bound — end to end over the wire, with Amazon
//! SNS mounted beside this service on ONE engine exactly as the assembled router
//! mounts them.
//!
//! THE RULE: `SetIdentityNotificationTopicRequest$SnsTopic` is "the Amazon
//! Resource Name (ARN) of the Amazon SNS topic", so the topic is Amazon SNS's
//! resource and Amazon SNS is the one answer to whether it is there. A value that
//! does not name a topic Amazon SNS holds for this account names nothing Amazon
//! SES could publish a notification to, so the call is refused — HTTP 400,
//! `InvalidParameterValue`, `"SNS topic "` + the `SnsTopic` the request carried +
//! `" is invalid."` — and NOTHING is stored: the identity keeps the topic it
//! already had. The same call becomes legal the moment the topic exists.
//!
//! The table spans what the rule generalises over: whether Amazon SNS holds the
//! topic (it never did, it did and the topic was deleted, it holds it), the
//! notification class the request names, and the FORM of the value — a topic ARN
//! of this account and Region, one of another account or Region, another vendor's
//! ARN, and a bare topic name — so the refusal is shown to echo whatever value
//! the request named rather than one fixture's.
//!
//! Every topic here is made and deleted through Amazon SNS's OWN operations over
//! the in-process callback, so what the admission check reads is what Amazon SNS
//! really holds.

use crate::harness::{
    account, call, element, emulator_with_sns, error_envelope, field, map_entries, sns, topic_arn,
    Field, REGION,
};
use service_com_amazonaws_ses_simpleemailservice::ops::NOTIFICATION_TYPE_VALUES;
use smithy_server_runtime::MultiServiceRouter;

/// Create the topic `name` through Amazon SNS, and answer the ARN Amazon SNS
/// resolved it to.
async fn create_topic(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let created = sns(router)
        .create_topic()
        .name(name)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: creating the topic {name}: {error}"));
    created
        .topic_arn()
        .unwrap_or_else(|| panic!("case {case:?}: the created topic has an ARN"))
        .to_string()
}

/// Delete the topic `arn` through Amazon SNS.
async fn delete_topic(router: &MultiServiceRouter, case: &str, arn: &str) {
    sns(router)
        .delete_topic()
        .topic_arn(arn)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: deleting the topic {arn}: {error}"));
}

/// `VerifyEmailIdentity`, which is what puts an address in the identity list.
async fn verify_email(router: &MultiServiceRouter, address: &str) {
    let (status, body) = call(
        router,
        "VerifyEmailIdentity",
        &[field("EmailAddress", address)],
    )
    .await;
    assert_eq!(
        status, 200,
        "VerifyEmailIdentity({address:?}): expected 200, got {status} with {body}"
    );
}

/// `SetIdentityNotificationTopic`, as `(status, body)`.
async fn set_topic(
    router: &MultiServiceRouter,
    identity: &str,
    notification_type: &str,
    topic: &str,
) -> (u16, String) {
    call(
        router,
        "SetIdentityNotificationTopic",
        &[
            field("Identity", identity),
            field("NotificationType", notification_type),
            field("SnsTopic", topic),
        ],
    )
    .await
}

/// The topic `GetIdentityNotificationAttributes` reports for `notification_type`,
/// read from the identity's own entry.
async fn reported_topic(
    router: &MultiServiceRouter,
    identity: &str,
    notification_type: &str,
) -> String {
    let params: Vec<Field> = vec![field("Identities.member.1", identity)];
    let (status, body) = call(router, "GetIdentityNotificationAttributes", &params).await;
    assert_eq!(
        status, 200,
        "GetIdentityNotificationAttributes({identity:?}): expected 200, got {status} with {body}"
    );
    let attributes = map_entries(&body)
        .into_iter()
        .find(|(key, _)| key == identity)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("{identity:?} must be reported"));
    element(&attributes, &format!("{notification_type}Topic")).unwrap_or_default()
}

/// The sentence Amazon SES refuses an `SnsTopic` naming no topic with.
fn invalid_message(value: &str) -> String {
    format!("SNS topic {value} is invalid.")
}

/// What Amazon SNS holds under the value a row names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TopicSetup {
    /// Amazon SNS holds the topic: it was created and never deleted.
    Held,
    /// Amazon SNS never held a topic the value names.
    NeverCreated,
    /// Amazon SNS held the topic and the topic was deleted before the call.
    Deleted,
}

/// One row: what Amazon SNS holds, and the value the request carries.
struct Row {
    case: &'static str,
    /// The topic NAME, for a row whose value is a topic ARN this suite builds.
    topic: &'static str,
    setup: TopicSetup,
    /// The value the request carries, when it is not the published ARN form of
    /// [`Self::topic`] of this account in this Region.
    value: Option<String>,
}

/// The rows the table spans.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a topic Amazon SNS holds",
            topic: "ses-bounce-live",
            setup: TopicSetup::Held,
            value: None,
        },
        Row {
            case: "a topic Amazon SNS holds under an underscored name",
            topic: "ses_bounce_live_2",
            setup: TopicSetup::Held,
            value: None,
        },
        Row {
            case: "a hyphenated name Amazon SNS never held",
            topic: "ses-bounce-ghost",
            setup: TopicSetup::NeverCreated,
            value: None,
        },
        Row {
            case: "a mixed-case name Amazon SNS never held",
            topic: "SesBounceGhost",
            setup: TopicSetup::NeverCreated,
            value: None,
        },
        Row {
            case: "a long name Amazon SNS never held",
            topic: "ses-bounce-ghost-with-a-rather-long-but-legal-name-0123456789",
            setup: TopicSetup::NeverCreated,
            value: None,
        },
        Row {
            case: "a topic Amazon SNS held until it was deleted",
            topic: "ses-bounce-deleted",
            setup: TopicSetup::Deleted,
            value: None,
        },
        Row {
            case: "a topic ARN of another account",
            topic: "ses-bounce-live",
            setup: TopicSetup::NeverCreated,
            value: Some(format!("arn:aws:sns:{REGION}:210987654321:ses-bounce-live")),
        },
        Row {
            case: "a topic ARN of another Region",
            topic: "ses-bounce-live",
            setup: TopicSetup::NeverCreated,
            value: Some(format!("arn:aws:sns:eu-west-3:{}:ses-bounce-live", account())),
        },
        Row {
            case: "an ARN of another vendor",
            topic: "ses-bounce-live",
            setup: TopicSetup::NeverCreated,
            value: Some(format!("arn:aws:sqs:{REGION}:{}:ses-bounce-live", account())),
        },
        Row {
            case: "a bare topic name rather than an ARN",
            topic: "ses-bounce-live",
            setup: TopicSetup::NeverCreated,
            value: Some("ses-bounce-live".to_string()),
        },
    ]
}

/// Put `row`'s topic into the state the row describes, and answer the value its
/// request carries.
async fn given_topic(router: &MultiServiceRouter, row: &Row) -> String {
    let arn = match row.setup {
        TopicSetup::Held => create_topic(router, row.case, row.topic).await,
        TopicSetup::NeverCreated => topic_arn(row.topic),
        TopicSetup::Deleted => {
            let arn = create_topic(router, row.case, row.topic).await;
            delete_topic(router, row.case, &arn).await;
            arn
        }
    };
    row.value.clone().unwrap_or(arn)
}

#[tokio::test]
async fn a_bound_notification_topic_is_one_amazon_sns_holds() {
    let router = emulator_with_sns();
    // The rule holds for every class of notification, so each row is driven
    // through one of them in turn.
    for (index, row) in rows().iter().enumerate() {
        let notification_type = NOTIFICATION_TYPE_VALUES[index % NOTIFICATION_TYPE_VALUES.len()];
        let case = format!("{} as the {notification_type} topic", row.case);
        let identity = format!("notify-{index}@example.com");
        verify_email(&router, &identity).await;
        // The identity starts out with a topic Amazon SNS really holds, so what a
        // refused call must leave behind is an observable last-good state.
        let live = create_topic(&router, &case, &format!("ses-control-{index}")).await;
        let (status, body) = set_topic(&router, &identity, notification_type, &live).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the identity's first topic: expected 200, got {status} with {body}"
        );

        let value = given_topic(&router, row).await;
        let (status, body) = set_topic(&router, &identity, notification_type, &value).await;
        let reported = reported_topic(&router, &identity, notification_type).await;

        if row.setup == TopicSetup::Held && row.value.is_none() {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the topic to be bound, got {status} with {body}"
            );
            assert_eq!(
                reported, value,
                "case {case:?}: expected the identity to report {value}, got {reported}"
            );
            continue;
        }

        assert_eq!(
            (status, error_envelope(&body).0, error_envelope(&body).1),
            (
                400,
                Some("InvalidParameterValue".to_string()),
                Some(invalid_message(&value))
            ),
            "case {case:?}: expected 400 InvalidParameterValue reading {:?}, got {status} with \
             {body}",
            invalid_message(&value)
        );
        assert_eq!(
            reported, live,
            "case {case:?}: the refused call must leave the bound topic on {live}, got {reported}"
        );
    }
}

#[tokio::test]
async fn the_same_call_is_served_once_amazon_sns_holds_the_topic() {
    // The restore leg: the refusal is about the topic, not about the request, so
    // creating the topic the ARN names and re-issuing the IDENTICAL call is served
    // and the identity reports that same ARN.
    let router = emulator_with_sns();
    let case = "a call re-issued once the topic exists";
    let identity = "restore@example.com";
    verify_email(&router, identity).await;
    let arn = topic_arn("ses-bounce-restored");

    let (status, body) = set_topic(&router, identity, "Bounce", &arn).await;
    assert_eq!(
        (status, error_envelope(&body).0, error_envelope(&body).1),
        (
            400,
            Some("InvalidParameterValue".to_string()),
            Some(invalid_message(&arn))
        ),
        "case {case:?}: expected the first call to be refused, got {status} with {body}"
    );
    assert_eq!(
        reported_topic(&router, identity, "Bounce").await,
        "",
        "case {case:?}: the refused call must bind no topic"
    );

    let minted = create_topic(&router, case, "ses-bounce-restored").await;
    assert_eq!(
        minted, arn,
        "case {case:?}: Amazon SNS must mint the ARN the refused call named, got {minted}"
    );

    let (status, body) = set_topic(&router, identity, "Bounce", &arn).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the re-issued call to be served, got {status} with {body}"
    );
    assert_eq!(
        reported_topic(&router, identity, "Bounce").await,
        arn,
        "case {case:?}: expected the identity to report {arn}"
    );
}

#[tokio::test]
async fn clearing_the_topic_asks_amazon_sns_nothing() {
    // The negative control: "if the parameter is omitted from the request or a
    // null value is passed, `SnsTopic` is cleared and publishing is disabled" (the
    // member's own documentation) — there is no topic to resolve, so the call is
    // served with Amazon SNS mounted beside this service exactly as the rows above
    // are refused against it.
    let router = emulator_with_sns();
    let identity = "cleared@example.com";
    verify_email(&router, identity).await;
    let live = create_topic(&router, "the clearing fixtures", "ses-cleared").await;

    for (case, cleared) in [("an empty SnsTopic", Some("")), ("no SnsTopic", None)] {
        let (status, body) = set_topic(&router, identity, "Complaint", &live).await;
        assert_eq!(
            status, 200,
            "case {case:?}: binding the topic first: expected 200, got {status} with {body}"
        );

        let mut params = vec![
            field("Identity", identity),
            field("NotificationType", "Complaint"),
        ];
        if let Some(cleared) = cleared {
            params.push(field("SnsTopic", cleared));
        }
        let (status, body) = call(&router, "SetIdentityNotificationTopic", &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the topic to be cleared, got {status} with {body}"
        );
        assert_eq!(
            reported_topic(&router, identity, "Complaint").await,
            "",
            "case {case:?}: expected the topic to be cleared"
        );
    }
}
