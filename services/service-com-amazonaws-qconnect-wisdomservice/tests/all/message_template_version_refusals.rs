// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the two REFUSALS of the Amazon Q in Connect message-template
//! version plane that this suite owns:
//!
//! * `DeactivateMessageTemplate` deactivates the version in ACTIVE status — the counterpart
//!   of `ActivateMessageTemplate` — so a version the template never activated, and one a
//!   later activation or deactivation left inactive, is refused with the modelled
//!   `ValidationException` (HTTP 400) naming the `versionNumber` the request carried, and
//!   nothing is written. A version the template never published is still the modelled
//!   `ResourceNotFoundException` (HTTP 404): one rule per question, state before identity;
//! * every operation whose own documentation says its `messageTemplateId` "cannot contain
//!   any qualifier" holds that label to the expression that admits NO qualifier group — the
//!   model's `UuidOrArn` (`smithy_query(query="shape",shape="UuidOrArn")`), stricter than
//!   the `UuidOrArnOrEitherWithQualifier` the member targets — so a label that carries one
//!   is refused as that member's `@pattern` violation, in the same constraint-violation
//!   report this endpoint states for every other constrained member, rather than in prose
//!   of its own.
//!
//! Every assertion goes over the wire, through the same [`Emulator`] harness the rest of
//! this crate's suites drive the production router with: only the HTTP status, the
//! `x-amzn-errortype` header and the JSON body are inspected, so a response that is not
//! computed from persisted state cannot pass. The tables span the class each rule holds
//! over — how many versions were published, which of them is activated, which one the
//! request names, the channel subtype of the template, the operation the label travelled
//! on, the form that label took and the qualifier it carried — rather than the one request
//! a recording made.

use serde_json::{json, Value};

use super::message_template::{content_for, CHANNEL_SUBTYPES};
use super::message_template_qualifier::MESSAGE_QUALIFIER_NOT_ACCEPTED;
use super::message_template_versions::{
    activate, deactivate, integer, knowledge_base, list_versions, listed_active_versions,
    listed_versions, message_template, publish, publish_versions, read, sms_content, update,
};
use super::resource_tags::{label, wisdom_arn, Emulator, Reply, REGION};

/// The sentence a `DeactivateMessageTemplate` naming a version that is not the template's
/// active one is refused with: the fixed text, whose one variable part is the
/// `versionNumber` the request named. Written out here rather than imported from the
/// implementation, so the assertion pins the message a client sees.
pub(crate) fn message_version_not_active(version_number: i64) -> String {
    format!("Version {version_number} is not an active version")
}

/// The sentence a request addressed to a version this template never published is refused
/// with.
const MESSAGE_VERSION_NOT_FOUND: &str = "Message template version does not exist.";

/// The two managed version qualifiers a label may name
/// (`smithy_query(query="doc",shape="GetMessageTemplate")`: the qualifier is "either an
/// actual version number or an Amazon Q Connect managed qualifier $ACTIVE_VERSION |
/// $LATEST").
const QUALIFIER_ACTIVE: &str = "$ACTIVE_VERSION";
const QUALIFIER_LATEST: &str = "$LATEST";

/// What a deactivate answers, which is what each row of the table below expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// HTTP 200: the version named was the one in active status, and is active no longer.
    Deactivated,
    /// HTTP 400 `ValidationException`: the template published this version, but it is not
    /// the one in active status.
    NotActive,
    /// HTTP 404 `ResourceNotFoundException`: the template published no such version.
    NoSuchVersion,
}

/// One row of the deactivate table: a template with `published` versions, the versions
/// activated on it in order, the version the deactivate names, and the answer the rule
/// gives.
#[derive(Debug, Clone, Copy)]
struct DeactivateCase {
    label: &'static str,
    channel_subtype: &'static str,
    published: usize,
    activated: &'static [i64],
    asked: i64,
    answer: Answer,
}

/// The rows: they differ along how many versions the template published, which of them was
/// activated and in which order, which version the deactivate names, and the channel
/// subtype of the template — the recorded case (one version, never activated, deactivate 1)
/// is the first row alone.
const DEACTIVATE_CASES: [DeactivateCase; 8] = [
    DeactivateCase {
        label: "the only version, never activated",
        channel_subtype: "SMS",
        published: 1,
        activated: &[],
        asked: 1,
        answer: Answer::NotActive,
    },
    DeactivateCase {
        label: "the only version, activated",
        channel_subtype: "WHATSAPP",
        published: 1,
        activated: &[1],
        asked: 1,
        answer: Answer::Deactivated,
    },
    DeactivateCase {
        label: "a version an activation of another one left inactive",
        channel_subtype: "EMAIL",
        published: 3,
        activated: &[3],
        asked: 2,
        answer: Answer::NotActive,
    },
    DeactivateCase {
        label: "the version activated last of several",
        channel_subtype: "EMAIL",
        published: 3,
        activated: &[1, 3],
        asked: 3,
        answer: Answer::Deactivated,
    },
    DeactivateCase {
        label: "a version a later activation took the activation from",
        channel_subtype: "SMS",
        published: 4,
        activated: &[2, 4],
        asked: 2,
        answer: Answer::NotActive,
    },
    DeactivateCase {
        label: "the highest published version, never activated",
        channel_subtype: "EMAIL",
        published: 5,
        activated: &[1],
        asked: 5,
        answer: Answer::NotActive,
    },
    DeactivateCase {
        label: "the lowest published version, activated",
        channel_subtype: "SMS",
        published: 5,
        activated: &[4, 1],
        asked: 1,
        answer: Answer::Deactivated,
    },
    DeactivateCase {
        label: "a version the template never published",
        channel_subtype: "PUSH",
        published: 1,
        activated: &[1],
        asked: 7,
        answer: Answer::NoSuchVersion,
    },
];

/// The content union member that applies to one channel subtype, carrying the text of one
/// REVISION of the working copy: a publish "made while the message template remains the
/// same" answers with the version already published, so every round of
/// [`publish_revisions`] has to change the working copy first.
fn content_revision(channel_subtype: &str, round: usize) -> Value {
    match channel_subtype {
        "EMAIL" => json!({
            "email": {
                "subject": format!("revision {round}"),
                "body": { "plainText": { "content": format!("revision {round}") } },
            }
        }),
        "SMS" => sms_content(&format!("revision {round}")),
        other => panic!("this suite publishes no revision for channel subtype {other:?}"),
    }
}

/// Publish `count` versions of one template, changing the working copy before each publish
/// past the first so the publish numbers a new version rather than answering with the one
/// already published, and answering with the version numbers that were published.
async fn publish_revisions(
    emulator: &Emulator,
    knowledge_base_id: &str,
    message_template_id: &str,
    channel_subtype: &str,
    count: usize,
) -> Vec<i64> {
    let mut published = Vec::new();
    for round in 0..count {
        if round > 0 {
            let changed = update(
                emulator,
                knowledge_base_id,
                message_template_id,
                json!({ "content": content_revision(channel_subtype, round) }),
            )
            .await;
            assert_eq!(
                changed.status, 200,
                "case \"revision {round}\": the working copy must change before it is published \
                 again, got HTTP {} with body {}",
                changed.status, changed.body
            );
        }
        let reply = publish(emulator, knowledge_base_id, message_template_id, None).await;
        assert_eq!(
            reply.status, 200,
            "case \"publish {round}\": expected HTTP 200, got HTTP {} with body {}",
            reply.status, reply.body
        );
        published.push(integer(&reply, "/messageTemplate/versionNumber").unwrap_or_default());
    }
    published
}

/// RULE: `DeactivateMessageTemplate` deactivates the version of a message template that is
/// in ACTIVE status, so a version that is not the active one is refused with the modelled
/// `ValidationException` naming that `versionNumber` and nothing is written — only a version
/// the template never published is a missing resource.
#[tokio::test]
async fn only_the_version_in_active_status_can_be_deactivated() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "deactivate-active-only").await;

    for case in DEACTIVATE_CASES {
        let message_template_id = message_template(
            &emulator,
            &knowledge_base_id,
            case.label,
            case.channel_subtype,
            content_for(case.channel_subtype),
        )
        .await;
        let published = publish_revisions(
            &emulator,
            &knowledge_base_id,
            &message_template_id,
            case.channel_subtype,
            case.published,
        )
        .await;
        for version in case.activated {
            let activated =
                activate(&emulator, &knowledge_base_id, &message_template_id, *version).await;
            assert_eq!(
                activated.status, 200,
                "case {:?}: activating version {version} must succeed, got HTTP {} with body {}",
                case.label, activated.status, activated.body
            );
        }
        // The version that is active going into the deactivate is the one activated last:
        // "the previous active version will be deactivated automatically".
        let active_before = case.activated.iter().next_back().copied();

        let reply = deactivate(
            &emulator,
            &knowledge_base_id,
            &message_template_id,
            case.asked,
        )
        .await;
        let (expected_status, expected_type, expected_message) = match case.answer {
            Answer::Deactivated => (200, String::new(), String::new()),
            Answer::NotActive => (
                400,
                "ValidationException".to_string(),
                message_version_not_active(case.asked),
            ),
            Answer::NoSuchVersion => (
                404,
                "ResourceNotFoundException".to_string(),
                MESSAGE_VERSION_NOT_FOUND.to_string(),
            ),
        };
        assert_eq!(
            reply.status, expected_status,
            "case {:?}: expected HTTP {expected_status}, got HTTP {} with body {}",
            case.label, reply.status, reply.body
        );
        if case.answer == Answer::Deactivated {
            assert_eq!(
                integer(&reply, "/versionNumber"),
                Some(case.asked),
                "case {:?}: the deactivation reports the version it deactivated, got {:?}",
                case.label,
                integer(&reply, "/versionNumber")
            );
        } else {
            assert_eq!(
                reply.error_type, expected_type,
                "case {:?}: expected error type {expected_type:?}, got {:?}",
                case.label, reply.error_type
            );
            assert_eq!(
                reply.string("/message"),
                expected_message,
                "case {:?}: expected the message {expected_message:?}, got {:?}",
                case.label,
                reply.string("/message")
            );
        }

        // What the store holds afterwards: a refusal writes nothing, so the version that
        // was active is still active; a deactivation leaves none active. Either way every
        // published version is still there.
        let active_after = match case.answer {
            Answer::Deactivated => None,
            _ => active_before,
        };
        let listing = list_versions(&emulator, &knowledge_base_id, &message_template_id, "").await;
        assert_eq!(
            listed_active_versions(&listing),
            active_after.into_iter().collect::<Vec<i64>>(),
            "case {:?}: expected the active versions {:?}, got {:?}",
            case.label,
            active_after.into_iter().collect::<Vec<i64>>(),
            listed_active_versions(&listing)
        );
        assert_eq!(
            listed_versions(&listing),
            published,
            "case {:?}: neither answer publishes or forgets a version; expected {published:?}, \
             got {:?}",
            case.label,
            listed_versions(&listing)
        );

        // And `$ACTIVE_VERSION` resolves to exactly what the listing reports as active.
        let qualified = read(
            &emulator,
            &knowledge_base_id,
            &format!("{message_template_id}:{QUALIFIER_ACTIVE}"),
        )
        .await;
        let expected_qualified_status = if active_after.is_some() { 200 } else { 404 };
        assert_eq!(
            qualified.status, expected_qualified_status,
            "case {:?}: {QUALIFIER_ACTIVE} names the activated version while there is one and \
             nothing once there is none; expected HTTP {expected_qualified_status}, got HTTP {} \
             with body {}",
            case.label, qualified.status, qualified.body
        );
    }
}

/// RULE: the refusal is about the version's STATE, so it is not terminal — once an
/// `ActivateMessageTemplate` makes that version the active one, the same
/// `DeactivateMessageTemplate` succeeds and flips `isActive` back to false, and the next
/// one is refused again.
#[tokio::test]
async fn a_version_refused_once_deactivates_after_it_is_activated() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "deactivate-after-activate").await;
    let message_template_id = message_template(
        &emulator,
        &knowledge_base_id,
        "activate then deactivate",
        "SMS",
        sms_content("the working copy"),
    )
    .await;
    let published = publish_versions(&emulator, &knowledge_base_id, &message_template_id, 2).await;
    assert_eq!(
        published,
        vec![1, 2],
        "the two versions this test addresses must be published, got {published:?}"
    );

    /// Whether the version at `version_number` reports itself as the active one.
    async fn reports_active(
        emulator: &Emulator,
        knowledge_base_id: &str,
        message_template_id: &str,
        version_number: i64,
    ) -> Option<bool> {
        read(
            emulator,
            knowledge_base_id,
            &format!("{message_template_id}:{version_number}"),
        )
        .await
        .field("/messageTemplate/isActive")
        .and_then(Value::as_bool)
    }

    // Each step: whether it activates (true) or deactivates (false) version 1, the status
    // it expects, and what the version reports afterwards.
    let steps: [(&str, bool, u16, bool); 5] = [
        ("deactivate before any activation", false, 400, false),
        ("activate", true, 200, true),
        ("deactivate the version just activated", false, 200, false),
        ("deactivate it a second time", false, 400, false),
        ("activate it again", true, 200, true),
    ];
    for (case, activates, expected_status, expected_active) in steps {
        let reply = if activates {
            activate(&emulator, &knowledge_base_id, &message_template_id, 1).await
        } else {
            deactivate(&emulator, &knowledge_base_id, &message_template_id, 1).await
        };
        assert_eq!(
            reply.status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got HTTP {} with body {}",
            reply.status, reply.body
        );
        if expected_status == 400 {
            assert_eq!(
                reply.error_type, "ValidationException",
                "case {case:?}: expected error type ValidationException, got {:?}",
                reply.error_type
            );
            assert_eq!(
                reply.string("/message"),
                message_version_not_active(1),
                "case {case:?}: expected the message {:?}, got {:?}",
                message_version_not_active(1),
                reply.string("/message")
            );
        }
        assert_eq!(
            reports_active(&emulator, &knowledge_base_id, &message_template_id, 1).await,
            Some(expected_active),
            "case {case:?}: the version must report isActive {expected_active}, got {:?}",
            reports_active(&emulator, &knowledge_base_id, &message_template_id, 1).await
        );
        assert_eq!(
            reports_active(&emulator, &knowledge_base_id, &message_template_id, 2).await,
            Some(false),
            "case {case:?}: the version nothing addressed stays inactive, got {:?}",
            reports_active(&emulator, &knowledge_base_id, &message_template_id, 2).await
        );
    }
}

/// One operation of the message-template plane whose label the model documents as one that
/// "cannot contain any qualifier", with the route it is reached through.
#[derive(Debug, Clone, Copy)]
struct QualifierOperation {
    label: &'static str,
    method: &'static str,
    /// The path suffix after the message-template label, if any.
    suffix: &'static str,
    /// Whether the operation carries a request body, and which one.
    body: Option<&'static str>,
}

/// Every operation of the plane that refuses a qualifier, with the body kind each needs:
/// the model documents `messageTemplateId` as a label that "cannot contain any qualifier"
/// for all of them (`smithy_query(query="operation",operation="<the operation>")`).
const QUALIFIER_OPERATIONS: [QualifierOperation; 7] = [
    QualifierOperation {
        label: "ActivateMessageTemplate",
        method: "POST",
        suffix: "/activate",
        body: Some("version"),
    },
    QualifierOperation {
        label: "DeactivateMessageTemplate",
        method: "POST",
        suffix: "/deactivate",
        body: Some("version"),
    },
    QualifierOperation {
        label: "CreateMessageTemplateVersion",
        method: "POST",
        suffix: "/versions",
        body: Some("empty"),
    },
    QualifierOperation {
        label: "ListMessageTemplateVersions",
        method: "GET",
        suffix: "/versions",
        body: None,
    },
    QualifierOperation {
        label: "UpdateMessageTemplate",
        method: "POST",
        suffix: "",
        body: Some("content"),
    },
    QualifierOperation {
        label: "UpdateMessageTemplateMetadata",
        method: "POST",
        suffix: "/metadata",
        body: Some("metadata"),
    },
    QualifierOperation {
        label: "CreateMessageTemplateAttachment",
        method: "POST",
        suffix: "/attachments",
        body: Some("attachment"),
    },
];

/// The request body one operation of the table carries, filled with values its own modelled
/// constraints admit.
fn body_for(kind: &str) -> Value {
    match kind {
        "version" => json!({ "versionNumber": 1 }),
        "empty" => json!({}),
        "content" => json!({ "content": sms_content("through a qualifier") }),
        "metadata" => json!({ "description": "through a qualifier" }),
        "attachment" => json!({
            "contentDisposition": "ATTACHMENT",
            "name": "note.txt",
            "body": ::aws_smithy_types::base64::encode("through a qualifier"),
        }),
        other => panic!("no body is modelled for the kind {other:?}"),
    }
}

/// Send one operation of the table against a message-template label.
async fn send_qualified(
    emulator: &Emulator,
    operation: QualifierOperation,
    knowledge_base_id: &str,
    message_template: &str,
) -> Reply {
    let uri = format!(
        "/knowledgeBases/{}/messageTemplates/{}{}",
        label(knowledge_base_id),
        label(message_template),
        operation.suffix
    );
    emulator
        .send(operation.method, &uri, operation.body.map(body_for))
        .await
}

/// RULE: a `messageTemplateId` the operation documents as a label that "cannot contain any
/// qualifier" is held to the expression that admits none, so a value carrying one is refused
/// as that member's `@pattern` violation — AWS's constraint-violation report naming the
/// member and the expression it had to satisfy — whatever operation it travelled on, whether
/// it was written as an id or as an ARN, and whichever qualifier it carried.
#[tokio::test]
async fn a_qualified_message_template_id_is_refused_as_a_pattern_violation() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "qualifier-refusals").await;
    let message_template_id = message_template(
        &emulator,
        &knowledge_base_id,
        "qualified",
        "SMS",
        sms_content("the working copy"),
    )
    .await;
    let published = publish_versions(&emulator, &knowledge_base_id, &message_template_id, 2).await;
    assert_eq!(
        published,
        vec![1, 2],
        "the versions whose numbers this table addresses must be published, got {published:?}"
    );
    let template_arn = wisdom_arn(
        "aws",
        REGION,
        engine::EMULATOR_ACCOUNT_ID,
        &format!("message-template/{knowledge_base_id}/{message_template_id}"),
    );

    // The label forms a caller may write — "Can be either the ID or the ARN" — each with
    // every qualifier form the member's own pattern admits: the managed qualifiers, the
    // number of a version that WAS published, the number of one that was not, and a
    // qualifier this service does not define.
    let forms: [(&str, &str); 2] = [("the id", &message_template_id), ("the ARN", &template_arn)];
    let qualifiers = [QUALIFIER_LATEST, QUALIFIER_ACTIVE, "1", "2", "9", "$NO_SUCH_QUALIFIER"];

    for operation in QUALIFIER_OPERATIONS {
        for (form, written) in forms {
            for qualifier in qualifiers {
                let case = format!("{}/{form}/{qualifier}", operation.label);
                let reply = send_qualified(
                    &emulator,
                    operation,
                    &knowledge_base_id,
                    &format!("{written}:{qualifier}"),
                )
                .await;
                assert_eq!(
                    reply.status, 400,
                    "case {case:?}: expected HTTP 400, got HTTP {} with body {}",
                    reply.status, reply.body
                );
                assert_eq!(
                    reply.error_type, "ValidationException",
                    "case {case:?}: expected error type ValidationException, got {:?}",
                    reply.error_type
                );
                assert_eq!(
                    reply.string("/message"),
                    MESSAGE_QUALIFIER_NOT_ACCEPTED,
                    "case {case:?}: expected the message {MESSAGE_QUALIFIER_NOT_ACCEPTED:?}, got \
                     {:?}",
                    reply.string("/message")
                );
            }
        }
    }

    // None of those refusals wrote anything: the versions are what they were, none is
    // activated, and the working copy still holds the content it was created with.
    let listing = list_versions(&emulator, &knowledge_base_id, &message_template_id, "").await;
    assert_eq!(
        listed_versions(&listing),
        published,
        "case \"after every refusal\": no refusal publishes a version, got {:?}",
        listed_versions(&listing)
    );
    assert_eq!(
        listed_active_versions(&listing),
        Vec::<i64>::new(),
        "case \"after every refusal\": no refusal activates a version, got {:?}",
        listed_active_versions(&listing)
    );

    // The positive control: the SAME write against the label with no qualifier is served,
    // and what it wrote is what a later read reports.
    let accepted = update(
        &emulator,
        &knowledge_base_id,
        &message_template_id,
        json!({ "content": sms_content("without a qualifier") }),
    )
    .await;
    assert_eq!(
        accepted.status, 200,
        "case \"the unqualified label\": the write must be served, got HTTP {} with body {}",
        accepted.status, accepted.body
    );
    let working_copy = read(&emulator, &knowledge_base_id, &message_template_id).await;
    assert_eq!(
        working_copy
            .field("/messageTemplate/content/sms/body/plainText/content")
            .and_then(Value::as_str),
        Some("without a qualifier"),
        "case \"the unqualified label\": the read reports what the write stored, got {}",
        working_copy.body
    );

    // And the read whose own documentation says its label MAY carry a qualifier still
    // resolves one, so the refusal belongs to the members documented without it.
    let versioned = read(
        &emulator,
        &knowledge_base_id,
        &format!("{message_template_id}:{QUALIFIER_LATEST}"),
    )
    .await;
    assert_eq!(
        versioned.status, 200,
        "case \"GetMessageTemplate with a qualifier\": this label may carry one, got HTTP {} \
         with body {}",
        versioned.status, versioned.body
    );

    // Each channel subtype's own template answers the same way: the rule is about the
    // label, not about what the template holds.
    for (channel_subtype, _) in CHANNEL_SUBTYPES {
        let other_id = message_template(
            &emulator,
            &knowledge_base_id,
            &format!("qualified {channel_subtype}"),
            channel_subtype,
            content_for(channel_subtype),
        )
        .await;
        let reply = send_qualified(
            &emulator,
            QUALIFIER_OPERATIONS[0],
            &knowledge_base_id,
            &format!("{other_id}:{QUALIFIER_LATEST}"),
        )
        .await;
        assert_eq!(
            reply.string("/message"),
            MESSAGE_QUALIFIER_NOT_ACCEPTED,
            "case {channel_subtype:?}: expected the message {MESSAGE_QUALIFIER_NOT_ACCEPTED:?}, \
             got {:?}",
            reply.string("/message")
        );
    }
}
