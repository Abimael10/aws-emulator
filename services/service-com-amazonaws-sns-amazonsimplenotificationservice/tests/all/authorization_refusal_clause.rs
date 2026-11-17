// SPDX-License-Identifier: CC0-1.0

//! WHICH policy type the `AuthorizationError` sentence of a topic-addressed SNS call blames,
//! driven over the awsQuery wire through the real `MultiServiceRouter` with IAM and STS
//! mounted on the same engine — so the sentence is read where a client reads it: in the
//! answer to the signed call.
//!
//! The RULE asserted here: when a call that addresses a topic is refused because NEITHER the
//! calling principal's own identity-based documents NOR the topic's access policy allows the
//! action for that principal, the refusal's reason clause names the identity-based policies
//! — "because no identity-based policy allows the `<action>` action" — because within one
//! account IAM's default deny means the principal is missing a permission of its OWN; the
//! resource-based wording is reserved for an explicit `Deny` in the topic's own policy, and
//! the identity-based deny wording for an explicit `Deny` in the principal's documents. A
//! topic statement naming the ACCOUNT delegates to that account's identity-based policies
//! rather than granting, so it leaves the call in the default-deny position too.
//!
//! The error itself is the modelled one: `AuthorizationErrorException`, HTTP 403 with the
//! awsQuery code `AuthorizationError` (`smithy_query(query="operation",
//! operation="Subscribe")`), and the sentence's variable parts come from the request and the
//! stored records — the principal `sts:GetCallerIdentity` reports for the credential that
//! signed, the topic ARN the request addressed, and the IAM action the operation performs.
//!
//! The tables differ along what the rule generalises over: the FORM of the caller (an STS
//! role session, whose principal is `arn:aws:sts::<account>:assumed-role/<role>/<session>`,
//! and an IAM user), the OPERATION asked for (six of the actions a topic's policy can grant),
//! what the topic's policy says (none at all, a grant of the account, a grant of another
//! principal, a grant of another action to this caller, a grant on another topic, an explicit
//! deny of this caller, of the account, of every principal), what the caller's own
//! documents say (nothing, a grant of the action, an explicit deny of it), and whether a grant
//! that stood is later WITHDRAWN, leaving the topic with the policy SNS generates for one
//! whose owner has set none.

use serde_json::json;

use crate::publish_authorization::{
    emulator, given_assumed_role, given_user, iam, policy, sns_as, Emulator, AMBIENT_KEY, REGION,
};
use crate::topics_subscriptions_publish::text_of;
// The refused status and code, the two clauses and the sentence they stand in have ONE
// statement in this crate's suites, in the sibling that first pinned a topic refusal; they are
// read from there rather than written a second time, so a suite cannot assert a wording the
// other does not.
use crate::topic_policy_authorization::{
    no_identity_policy_allows, not_authorized, AUTHORIZATION_ERROR, RESOURCE_POLICY_DENY,
};

/// The clause IAM states when one of the principal's own documents explicitly denies the call.
const IDENTITY_POLICY_DENY: &str = "with an explicit deny in an identity-based policy";

/// One SNS fixture call, made with the account's own credential, which must succeed.
async fn fixture(emulator: &Emulator, action: &str, pairs: &[(&str, &str)]) -> String {
    let (status, body) = sns_as(emulator, AMBIENT_KEY, action, pairs).await;
    assert_eq!(
        status, 200,
        "the fixture SNS {action} must succeed, got {status} / {body}"
    );
    body
}

/// A topic created by the account itself, with its ARN.
async fn given_topic(emulator: &Emulator, name: &str) -> String {
    let created = fixture(emulator, "CreateTopic", &[("Name", name)]).await;
    let arn = text_of(&created, "TopicArn");
    assert!(
        !arn.is_empty(),
        "CreateTopic {name}: expected a TopicArn, got {created}"
    );
    arn
}

/// Write a topic's access policy, as its owner does.
async fn set_topic_policy(emulator: &Emulator, topic: &str, document: &str) {
    fixture(
        emulator,
        "SetTopicAttributes",
        &[
            ("TopicArn", topic),
            ("AttributeName", "Policy"),
            ("AttributeValue", document),
        ],
    )
    .await;
}

/// The form of the credential a call is signed with, which decides the shape of the principal
/// a refusal names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// Temporary credentials of an assumed role: the principal is the STS session.
    RoleSession,
    /// A long-term access key: the principal is the IAM user holding it.
    User,
}

/// A signed caller of this account, and everything a row needs to write documents for it.
struct Caller {
    /// Which kind of credential signs.
    form: Form,
    /// The friendly name of the IAM entity behind the credential, for a policy write.
    entity_name: String,
    /// The ARN of that entity — the spelling a resource policy names the caller by.
    entity_arn: String,
    /// The principal `sts:GetCallerIdentity` reports for the credential, which is what the
    /// refusal names: the SESSION for a role session, the user itself for a long-term key.
    reported: String,
    /// The access key id a request carries to speak for it.
    access_key_id: String,
}

/// A caller of `form`, named `name`, holding no identity-based document at all.
async fn given_caller(emulator: &Emulator, form: Form, name: &str) -> Caller {
    match form {
        Form::RoleSession => {
            let (role_arn, session_arn, access_key_id) = given_assumed_role(emulator, name).await;
            // "arn:aws:sts::<account>:assumed-role/<role-name>/<role-session-name>" is the
            // ARN STS reports for an assumed-role session (`AssumedRoleUser$Arn`), and the
            // principal a refusal of that session names.
            let expected = format!(
                "arn:aws:sts::{}:assumed-role/{name}/",
                engine::emulator_account_id()
            );
            assert!(
                session_arn.starts_with(&expected),
                "case \"an assumed-role session\": expected a session ARN under \
                 {expected:?}, got {session_arn:?}"
            );
            Caller {
                form,
                entity_name: name.to_string(),
                entity_arn: role_arn,
                reported: session_arn,
                access_key_id,
            }
        }
        Form::User => {
            let (user_arn, access_key_id) = given_user(emulator, name).await;
            Caller {
                form,
                entity_name: name.to_string(),
                entity_arn: user_arn.clone(),
                reported: user_arn,
                access_key_id,
            }
        }
    }
}

/// Write the caller's ONE inline identity-based document, under the same policy name every
/// time so a second write replaces the first.
///
/// A role session is judged on the ROLE's documents ("the temporary security credentials …
/// have the permissions of the role", `AssumeRole`), so the write goes to the role.
async fn grant(emulator: &Emulator, caller: &Caller, document: &str) {
    let (operation, entity) = match caller.form {
        Form::RoleSession => ("PutRolePolicy", "RoleName"),
        Form::User => ("PutUserPolicy", "UserName"),
    };
    iam(
        emulator,
        operation,
        &[
            (entity, &caller.entity_name),
            ("PolicyName", "inline"),
            ("PolicyDocument", document),
        ],
    )
    .await;
}

/// One topic-addressed operation: the IAM action it performs and the parameters it carries
/// beside `TopicArn`.
struct Call<'a> {
    operation: &'static str,
    action: &'static str,
    extra: Vec<(&'a str, &'a str)>,
}

/// Six operations that act on the topic the request names, each with the IAM action it
/// performs — the actions a topic's own access policy can grant ("Actions defined by Amazon
/// Simple Notification Service", IAM Service Authorization Reference), which is the set the
/// policy SNS generates for a topic with none enumerates.
fn topic_calls<'a>(queue_arn: &'a str) -> Vec<Call<'a>> {
    vec![
        Call {
            operation: "Subscribe",
            action: "SNS:Subscribe",
            extra: vec![
                ("Protocol", "sqs"),
                ("Endpoint", queue_arn),
                ("ReturnSubscriptionArn", "true"),
            ],
        },
        Call {
            operation: "Publish",
            action: "SNS:Publish",
            extra: vec![("Message", "probe")],
        },
        Call {
            operation: "GetTopicAttributes",
            action: "SNS:GetTopicAttributes",
            extra: Vec::new(),
        },
        Call {
            operation: "SetTopicAttributes",
            action: "SNS:SetTopicAttributes",
            extra: vec![("AttributeName", "DisplayName"), ("AttributeValue", "gated")],
        },
        Call {
            operation: "ListSubscriptionsByTopic",
            action: "SNS:ListSubscriptionsByTopic",
            extra: Vec::new(),
        },
        Call {
            operation: "DeleteTopic",
            action: "SNS:DeleteTopic",
            extra: Vec::new(),
        },
    ]
}

/// The form parameters one call carries for the topic `topic`.
fn parameters<'a>(call: &'a Call<'a>, topic: &'a str) -> Vec<(&'a str, &'a str)> {
    let mut pairs = vec![("TopicArn", topic)];
    pairs.extend(call.extra.iter().copied());
    pairs
}

/// A document built for one row from the topic's ARN and the ARN of the IAM entity behind the
/// caller, or `None` for a row that installs none.
type DocumentOf = fn(&str, &str) -> Option<String>;

/// The root principal of the account the ARN `arn` belongs to: the spelling a resource policy
/// names a whole account by, read off the caller's own ARN rather than written out.
fn account_root_of(arn: &str) -> String {
    let account = arn
        .split(':')
        .nth(4)
        .unwrap_or_else(|| panic!("no account in {arn}"));
    format!("arn:aws:iam::{account}:root")
}

/// RULE: a topic-addressed call that neither document allows is refused `403
/// AuthorizationError` with the clause naming the IDENTITY-based policies, whatever the form
/// of the caller, whichever action the call performs and however the topic's policy withholds
/// the grant — and that sentence says nothing about a resource-based policy.
#[tokio::test]
async fn an_ungranted_topic_call_blames_the_identity_based_policy() {
    // (case, the topic policy that withholds the grant)
    let policies: &[(&str, DocumentOf)] = &[
        // A topic whose owner set no policy carries the one SNS generates, whose only
        // statement is conditioned on a cross-service key a directly signed call carries no
        // value for.
        ("no policy of its own", |_, _| None),
        (
            "a grant of the ACCOUNT, which delegates rather than granting",
            |topic, entity| {
                Some(policy(json!({
                    "Effect": "Allow",
                    "Principal": {"AWS": account_root_of(entity)},
                    "Action": "SNS:*",
                    "Resource": topic,
                })))
            },
        ),
        ("a grant of another principal", |topic, entity| {
            Some(policy(json!({
                "Effect": "Allow",
                "Principal": {"AWS": format!("{entity}-elsewhere")},
                "Action": "SNS:*",
                "Resource": topic,
            })))
        }),
        (
            "a grant of this caller for another action only",
            |topic, entity| {
                Some(policy(json!({
                    "Effect": "Allow",
                    "Principal": {"AWS": entity},
                    "Action": "SNS:Unsubscribe",
                    "Resource": topic,
                })))
            },
        ),
        ("a grant of this caller on another topic", |topic, entity| {
            Some(policy(json!({
                "Effect": "Allow",
                "Principal": {"AWS": entity},
                "Action": "SNS:*",
                "Resource": format!("{topic}-elsewhere"),
            })))
        }),
    ];

    let account = engine::emulator_account_id().to_string();
    // The endpoint a `Subscribe` row names. SNS holds no state about a subscriber, so the
    // queue need not exist for the authorization decision to be the one under test.
    let queue_arn = format!("arn:aws:sqs:{REGION}:{account}:refusal-clause");

    for form in [Form::RoleSession, Form::User] {
        let emulator = emulator();
        let caller = given_caller(&emulator, form, "ungranted-caller").await;
        for (index, (policy_case, document)) in policies.iter().enumerate() {
            for call in topic_calls(&queue_arn) {
                let case = format!("{form:?} asking {} with {policy_case}", call.operation);
                // One topic per row: a row that deletes or rewrites the topic must not decide
                // the next row's answer.
                let topic = given_topic(
                    &emulator,
                    &format!("clause-{form:?}-{index:02}-{}", call.operation),
                )
                .await;
                if let Some(document) = document(&topic, &caller.entity_arn) {
                    set_topic_policy(&emulator, &topic, &document).await;
                }

                let (status, body) = sns_as(
                    &emulator,
                    &caller.access_key_id,
                    call.operation,
                    &parameters(&call, &topic),
                )
                .await;
                let code = text_of(&body, "Code");
                assert_eq!(
                    (status, code.as_str()),
                    AUTHORIZATION_ERROR,
                    "case {case:?}: expected the modelled refusal, got {status} / {body}"
                );
                let message = text_of(&body, "Message");
                assert_eq!(
                    message,
                    not_authorized(
                        &caller.reported,
                        call.action,
                        &topic,
                        &no_identity_policy_allows(call.action)
                    ),
                    "case {case:?}: expected IAM's default-deny sentence, got {message:?}"
                );
                assert!(
                    !message.contains("resource-based"),
                    "case {case:?}: the default-deny sentence must not blame the \
                     resource-based policy, got {message:?}"
                );
            }
        }
    }
}

/// What the documents of a row decide, and the clause the refusal must carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// The call is served.
    Served,
    /// The call is refused with this clause.
    Refused(&'static str),
}

/// RULE: the resource-based wording belongs to an explicit `Deny` in the topic's own policy
/// and to nothing else — a grant withheld rather than denied is the identity-based clause, and
/// a deny in the caller's own documents is the identity-based deny.
#[tokio::test]
async fn each_clause_names_the_document_that_decided_the_refusal() {
    // (case, the caller's identity document, the topic's policy, the verdict)
    let rows: &[(&str, DocumentOf, DocumentOf, Verdict)] = &[
        (
            "an identity grant and a topic policy that says nothing",
            |topic, _| {
                Some(policy(json!({
                    "Effect": "Allow",
                    "Action": "sns:Subscribe",
                    "Resource": topic,
                })))
            },
            |_, _| None,
            Verdict::Served,
        ),
        (
            "a topic policy naming the caller's own entity",
            |_, _| None,
            |topic, entity| {
                Some(policy(json!({
                    "Effect": "Allow",
                    "Principal": {"AWS": entity},
                    "Action": "SNS:Subscribe",
                    "Resource": topic,
                })))
            },
            Verdict::Served,
        ),
        (
            "a topic deny of the caller over an identity grant",
            |topic, _| {
                Some(policy(json!({
                    "Effect": "Allow",
                    "Action": "sns:Subscribe",
                    "Resource": topic,
                })))
            },
            |topic, entity| {
                Some(policy(json!({
                    "Effect": "Deny",
                    "Principal": {"AWS": entity},
                    "Action": "SNS:Subscribe",
                    "Resource": topic,
                })))
            },
            Verdict::Refused(RESOURCE_POLICY_DENY),
        ),
        (
            "a topic deny of the ACCOUNT over an identity grant",
            |topic, _| {
                Some(policy(json!({
                    "Effect": "Allow",
                    "Action": "sns:Subscribe",
                    "Resource": topic,
                })))
            },
            |topic, entity| {
                Some(policy(json!({
                    "Effect": "Deny",
                    "Principal": {"AWS": account_root_of(entity)},
                    "Action": "SNS:Subscribe",
                    "Resource": topic,
                })))
            },
            Verdict::Refused(RESOURCE_POLICY_DENY),
        ),
        (
            "a topic deny of every principal over an identity grant",
            |topic, _| {
                Some(policy(json!({
                    "Effect": "Allow",
                    "Action": "sns:Subscribe",
                    "Resource": topic,
                })))
            },
            |topic, _| {
                Some(policy(json!({
                    "Effect": "Deny",
                    "Principal": {"AWS": "*"},
                    "Action": "SNS:*",
                    "Resource": topic,
                })))
            },
            Verdict::Refused(RESOURCE_POLICY_DENY),
        ),
        (
            "an identity deny over a topic grant of the caller",
            |topic, _| {
                Some(policy(json!({
                    "Effect": "Deny",
                    "Action": "sns:Subscribe",
                    "Resource": topic,
                })))
            },
            |topic, entity| {
                Some(policy(json!({
                    "Effect": "Allow",
                    "Principal": {"AWS": entity},
                    "Action": "SNS:Subscribe",
                    "Resource": topic,
                })))
            },
            Verdict::Refused(IDENTITY_POLICY_DENY),
        ),
        (
            "a topic grant of the account with no identity document",
            |_, _| None,
            |topic, entity| {
                Some(policy(json!({
                    "Effect": "Allow",
                    "Principal": {"AWS": account_root_of(entity)},
                    "Action": "SNS:Subscribe",
                    "Resource": topic,
                })))
            },
            Verdict::Refused("because no identity-based policy allows the SNS:Subscribe action"),
        ),
    ];

    let account = engine::emulator_account_id().to_string();
    let queue_arn = format!("arn:aws:sqs:{REGION}:{account}:clause-attribution");

    for form in [Form::RoleSession, Form::User] {
        for (index, (case, identity, resource, verdict)) in rows.iter().enumerate() {
            let case = format!("{form:?} with {case}");
            // One emulator per row: a deny of every principal locks the topic for its owner
            // too, exactly as it does on AWS, so no row may inherit another's documents.
            let emulator = emulator();
            let caller = given_caller(&emulator, form, "attributed-caller").await;
            let topic = given_topic(&emulator, &format!("attribution-{index:02}")).await;
            if let Some(document) = identity(&topic, &caller.entity_arn) {
                grant(&emulator, &caller, &document).await;
            }
            if let Some(document) = resource(&topic, &caller.entity_arn) {
                set_topic_policy(&emulator, &topic, &document).await;
            }

            let (status, body) = sns_as(
                &emulator,
                &caller.access_key_id,
                "Subscribe",
                &[
                    ("TopicArn", &topic),
                    ("Protocol", "sqs"),
                    ("Endpoint", &queue_arn),
                    ("ReturnSubscriptionArn", "true"),
                ],
            )
            .await;
            match verdict {
                Verdict::Served => {
                    assert_eq!(
                        status, 200,
                        "case {case:?}: expected the subscribe to be served, got {status} / \
                         {body}"
                    );
                    let subscription = text_of(&body, "SubscriptionArn");
                    assert!(
                        subscription.starts_with(&format!("{topic}:")),
                        "case {case:?}: expected a subscription ARN under the topic, got \
                         {subscription:?}"
                    );
                }
                Verdict::Refused(clause) => {
                    let code = text_of(&body, "Code");
                    assert_eq!(
                        (status, code.as_str()),
                        AUTHORIZATION_ERROR,
                        "case {case:?}: expected the modelled refusal, got {status} / {body}"
                    );
                    let message = text_of(&body, "Message");
                    assert_eq!(
                        message,
                        not_authorized(&caller.reported, "SNS:Subscribe", &topic, clause),
                        "case {case:?}: expected IAM's refusal wording, got {message:?}"
                    );
                }
            }
        }
    }
}

/// RULE: the refusal is the absence of a grant and nothing else — the identical `Subscribe` is
/// served once the caller's own documents allow the action on that topic, and the refused call
/// created no subscription.
#[tokio::test]
async fn the_refused_subscribe_is_served_once_an_identity_grant_exists() {
    let account = engine::emulator_account_id().to_string();
    let queue_arn = format!("arn:aws:sqs:{REGION}:{account}:granted-later");

    for form in [Form::RoleSession, Form::User] {
        let case = format!("{form:?}");
        let emulator = emulator();
        let caller = given_caller(&emulator, form, "granted-later").await;
        let topic = given_topic(&emulator, "granted-later").await;
        // The topic's policy names the account root only, which delegates to that account's
        // identity-based policies rather than granting: the caller holds none, so the call is
        // in IAM's default-deny position.
        set_topic_policy(
            &emulator,
            &topic,
            &policy(json!({
                "Effect": "Allow",
                "Principal": {"AWS": account_root_of(&caller.entity_arn)},
                "Action": "SNS:*",
                "Resource": topic,
            })),
        )
        .await;
        let subscribe = [
            ("TopicArn", topic.as_str()),
            ("Protocol", "sqs"),
            ("Endpoint", queue_arn.as_str()),
            ("ReturnSubscriptionArn", "true"),
        ];

        let (status, body) = sns_as(&emulator, &caller.access_key_id, "Subscribe", &subscribe).await;
        let code = text_of(&body, "Code");
        assert_eq!(
            (status, code.as_str()),
            AUTHORIZATION_ERROR,
            "case {case:?}: expected the modelled refusal, got {status} / {body}"
        );
        let message = text_of(&body, "Message");
        assert_eq!(
            message,
            not_authorized(
                &caller.reported,
                "SNS:Subscribe",
                &topic,
                &no_identity_policy_allows("SNS:Subscribe")
            ),
            "case {case:?}: expected IAM's default-deny sentence, got {message:?}"
        );
        // Nothing was created: the owner's own listing of the topic is empty.
        let listed = fixture(
            &emulator,
            "ListSubscriptionsByTopic",
            &[("TopicArn", &topic)],
        )
        .await;
        assert!(
            text_of(&listed, "SubscriptionArn").is_empty(),
            "case {case:?}: a refused subscribe must create nothing, got {listed}"
        );

        // The grant the call was missing, written where the caller's permissions live.
        grant(
            &emulator,
            &caller,
            &policy(json!({
                "Effect": "Allow",
                "Action": "sns:Subscribe",
                "Resource": topic,
            })),
        )
        .await;
        let (status, body) = sns_as(&emulator, &caller.access_key_id, "Subscribe", &subscribe).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the granted subscribe to be served, got {status} / {body}"
        );
        let subscription = text_of(&body, "SubscriptionArn");
        assert!(
            !subscription.is_empty(),
            "case {case:?}: expected a SubscriptionArn, got {body}"
        );

        // And it is a record the service holds: the topic lists it, and its attributes read
        // back the endpoint and protocol the call asked for.
        let listed = fixture(
            &emulator,
            "ListSubscriptionsByTopic",
            &[("TopicArn", &topic)],
        )
        .await;
        assert_eq!(
            text_of(&listed, "SubscriptionArn"),
            subscription,
            "case {case:?}: expected the topic to list the new subscription, got {listed}"
        );
        let attributes = fixture(
            &emulator,
            "GetSubscriptionAttributes",
            &[("SubscriptionArn", &subscription)],
        )
        .await;
        for (name, expected) in [
            ("Endpoint", queue_arn.as_str()),
            ("Protocol", "sqs"),
            ("TopicArn", topic.as_str()),
        ] {
            let entry = attributes
                .find(&format!("<key>{name}</key>"))
                .map(|start| text_of(&attributes[start..], "value"))
                .unwrap_or_default();
            assert_eq!(
                entry, expected,
                "case {case:?}: expected the subscription's {name} to read back as \
                 {expected:?}, got {entry:?} in {attributes}"
            );
        }
    }
}

/// The IAM action a `Publish` is authorized under, as SNS's policy actions and its refusals
/// spell it.
const PUBLISH_ACTION: &str = "SNS:Publish";

/// RULE: the same attribution holds for the action a PUBLISH performs — a publish no document
/// in force grants the caller is refused with the clause naming the identity-based policies,
/// however the topic's own policy came to withhold that grant.
#[tokio::test]
async fn an_ungranted_publish_blames_the_identity_based_policy() {
    // (case, the topic policy that withholds the publish grant from this caller)
    let policies: &[(&str, DocumentOf)] = &[
        // A topic whose owner set no policy carries the one SNS generates, whose only
        // statement is conditioned on a cross-service key a directly signed call carries no
        // value for.
        ("no policy of its own", |_, _| None),
        (
            "a grant of ANOTHER action to this caller",
            |topic, entity| {
                Some(policy(json!({
                    "Effect": "Allow",
                    "Principal": {"AWS": entity},
                    "Action": "SNS:Subscribe",
                    "Resource": topic,
                })))
            },
        ),
        ("a grant of the publish to another principal", |topic, entity| {
            Some(policy(json!({
                "Effect": "Allow",
                "Principal": {"AWS": format!("{entity}-elsewhere")},
                "Action": PUBLISH_ACTION,
                "Resource": topic,
            })))
        }),
        (
            "a grant of the publish to this caller on another topic",
            |topic, entity| {
                Some(policy(json!({
                    "Effect": "Allow",
                    "Principal": {"AWS": entity},
                    "Action": PUBLISH_ACTION,
                    "Resource": format!("{topic}-elsewhere"),
                })))
            },
        ),
        (
            "a grant of the ACCOUNT, which delegates rather than granting",
            |topic, entity| {
                Some(policy(json!({
                    "Effect": "Allow",
                    "Principal": {"AWS": account_root_of(entity)},
                    "Action": "SNS:*",
                    "Resource": topic,
                })))
            },
        ),
    ];

    for form in [Form::RoleSession, Form::User] {
        let emulator = emulator();
        let caller = given_caller(&emulator, form, "ungranted-publisher").await;
        for (index, (policy_case, document)) in policies.iter().enumerate() {
            let case = format!("{form:?} publishing with {policy_case}");
            // One topic per row: a row's document decides that row's topic alone.
            let topic =
                given_topic(&emulator, &format!("publish-clause-{form:?}-{index:02}")).await;
            if let Some(document) = document(&topic, &caller.entity_arn) {
                set_topic_policy(&emulator, &topic, &document).await;
            }

            let (status, body) = sns_as(
                &emulator,
                &caller.access_key_id,
                "Publish",
                &[("TopicArn", &topic), ("Message", "probe")],
            )
            .await;
            let code = text_of(&body, "Code");
            assert_eq!(
                (status, code.as_str()),
                AUTHORIZATION_ERROR,
                "case {case:?}: expected the modelled refusal, got {status} / {body}"
            );
            let message = text_of(&body, "Message");
            assert_eq!(
                message,
                not_authorized(
                    &caller.reported,
                    PUBLISH_ACTION,
                    &topic,
                    &no_identity_policy_allows(PUBLISH_ACTION)
                ),
                "case {case:?}: expected IAM's default-deny sentence, got {message:?}"
            );
            assert!(
                !message.contains("resource-based"),
                "case {case:?}: the default-deny sentence must not blame the resource-based \
                 policy, got {message:?}"
            );
        }
    }
}

/// RULE: the refusal is the absence of a grant and nothing else — the identical publish is
/// served while the topic's policy names the caller, and once that statement is WITHDRAWN the
/// topic reports the document SNS generates for a topic whose owner has set none and the
/// publish falls back to the same default-deny sentence.
#[tokio::test]
async fn a_withdrawn_topic_grant_returns_the_publish_to_the_default_deny_sentence() {
    for form in [Form::RoleSession, Form::User] {
        let case = format!("{form:?} whose topic grant is withdrawn");
        let emulator = emulator();
        let caller = given_caller(&emulator, form, "withdrawn-publisher").await;
        let topic = given_topic(&emulator, "withdrawn-grant").await;
        set_topic_policy(
            &emulator,
            &topic,
            &policy(json!({
                "Effect": "Allow",
                "Principal": {"AWS": caller.entity_arn},
                "Action": PUBLISH_ACTION,
                "Resource": topic,
            })),
        )
        .await;
        let publish = [("TopicArn", topic.as_str()), ("Message", "probe")];

        let (status, body) = sns_as(&emulator, &caller.access_key_id, "Publish", &publish).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the granted publish to be served, got {status} / {body}"
        );
        assert!(
            !text_of(&body, "MessageId").is_empty(),
            "case {case:?}: expected a MessageId, got {body}"
        );

        // An empty value removes a policy-valued attribute, so the topic is back to the
        // document SNS generates for one whose owner has set none.
        set_topic_policy(&emulator, &topic, "").await;
        let attributes = fixture(&emulator, "GetTopicAttributes", &[("TopicArn", &topic)]).await;
        assert!(
            attributes.contains("__default_statement_ID"),
            "case {case:?}: expected the generated default policy to be reported, got \
             {attributes}"
        );

        let (status, body) = sns_as(&emulator, &caller.access_key_id, "Publish", &publish).await;
        let code = text_of(&body, "Code");
        assert_eq!(
            (status, code.as_str()),
            AUTHORIZATION_ERROR,
            "case {case:?}: expected the modelled refusal, got {status} / {body}"
        );
        assert_eq!(
            text_of(&body, "Message"),
            not_authorized(
                &caller.reported,
                PUBLISH_ACTION,
                &topic,
                &no_identity_policy_allows(PUBLISH_ACTION)
            ),
            "case {case:?}: expected IAM's default-deny sentence, got {body}"
        );
    }
}
