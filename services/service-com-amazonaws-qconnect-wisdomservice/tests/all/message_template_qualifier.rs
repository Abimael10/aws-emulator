// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the version QUALIFIER a message-template label may carry, driven
//! over HTTP through the same router the emulator serves with: only the HTTP status, the
//! `x-amzn-errortype` header and the JSON body are inspected, so a response that is not
//! computed from persisted state cannot pass.
//!
//! The rule under test, and its boundary:
//!
//! * an operation whose `messageTemplateId` the model documents as a label that "cannot
//!   contain any qualifier"
//!   (`smithy_query(query="doc",shape="<the operation>Request",member="messageTemplateId")`)
//!   holds that label to the expression that admits no qualifier group, so a value carrying
//!   one is refused with the modelled `ValidationException` (HTTP 400) whose message is that
//!   member's `@pattern`-constraint violation — and ONE wording serves every such operation,
//!   every label form and every qualifier form, because it is one requirement about one
//!   member rather than prose each operation invents;
//! * an operation whose `messageTemplateId` carries no such sentence (its documentation stops
//!   at "Can be either the ID or the ARN") serves the qualified label, addressing the version
//!   the qualifier names.
//!
//! This suite owns [`MESSAGE_QUALIFIER_NOT_ACCEPTED`] for the whole crate: the sibling suites
//! of this plane import it from here rather than each writing out a copy, so the wording a
//! client reads is pinned in exactly one place. It is still written out rather than imported
//! from the implementation, so what the assertions pin is the message itself.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::message_template_attachment_deletion::{attachment_ids, upload};
use super::message_template_versions::{
    activate, integer, knowledge_base, list_versions, listed_active_versions, listed_versions,
    message_template, publish, publish_versions, read, sms_content, update,
};
use super::resource_tags::{label, wisdom_arn, Emulator, Reply, REGION};

/// The message a label carrying a version qualifier is refused with, on an operation whose
/// own documentation says its label may not carry one: AWS's report of the member's
/// `@pattern` violation, stating the expression the service enforces for it — `UuidOrArn`
/// (`smithy_query(query="shape",shape="UuidOrArn")`), which admits no qualifier group, quoted
/// without the `^`/`$` pair a whole-value pattern is wrapped in, which is how this endpoint
/// quotes every other pattern requirement.
pub(crate) const MESSAGE_QUALIFIER_NOT_ACCEPTED: &str = concat!(
    "1 validation error detected: Value at 'messageTemplateId' failed to satisfy constraint: ",
    "Member must satisfy regular expression pattern: ",
    "[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$|",
    "^arn:[a-z-]*?:wisdom:[a-z0-9-]*?:[0-9]{12}:[a-z-]*?/",
    "[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}",
    "(?:/[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}){0,2}",
);

/// The two managed version qualifiers a label may name
/// (`smithy_query(query="doc",shape="GetMessageTemplate")`: the qualifier is "either an actual
/// version number or an Amazon Q Connect managed qualifier $ACTIVE_VERSION | $LATEST").
const QUALIFIER_LATEST: &str = "$LATEST";
const QUALIFIER_ACTIVE: &str = "$ACTIVE_VERSION";

/// The channel subtype the templates of this suite are created under: the rule is about the
/// label, so one subtype carries it.
const CHANNEL_SUBTYPE: &str = "SMS";

/// Where a row's path suffix names an attachment of the template, replaced by the id of the
/// file this suite uploaded.
const ATTACHMENT_MARKER: &str = "{attachmentId}";

/// The request body one operation of a table carries, if any: a body filled with values the
/// operation's own modelled constraints admit, so a refusal can only be about the label.
#[derive(Debug, Clone, Copy)]
enum Payload {
    /// The operation carries no body at all.
    None,
    /// A `versionNumber` of the modelled `Version` range (`min 1`).
    Version,
    /// An empty structure: every member of the body is optional.
    Empty,
    /// `UpdateMessageTemplate`'s content.
    Content,
    /// `UpdateMessageTemplateMetadata`'s description.
    Metadata,
    /// `CreateMessageTemplateAttachment`'s file.
    Attachment,
    /// `RenderMessageTemplate`'s required `attributes`.
    Attributes,
}

impl Payload {
    fn body(self) -> Option<Value> {
        match self {
            Payload::None => None,
            Payload::Version => Some(json!({ "versionNumber": 1 })),
            Payload::Empty => Some(json!({})),
            Payload::Content => Some(json!({ "content": sms_content("through a qualifier") })),
            Payload::Metadata => Some(json!({ "description": "through a qualifier" })),
            Payload::Attachment => Some(json!({
                "contentDisposition": "ATTACHMENT",
                "name": "through-a-qualifier.txt",
                "body": ::aws_smithy_types::base64::encode("through a qualifier"),
            })),
            Payload::Attributes => Some(json!({ "attributes": {} })),
        }
    }
}

/// One operation reached through a message-template label, with the route it travels on.
#[derive(Debug, Clone, Copy)]
struct Addressed {
    operation: &'static str,
    method: &'static str,
    /// The path suffix after the message-template label, if any, which may name the
    /// attachment the route addresses ([`ATTACHMENT_MARKER`]).
    suffix: &'static str,
    payload: Payload,
}

/// Every operation of this plane whose `messageTemplateId` the model documents as a label
/// that "cannot contain any qualifier" — the whole set, so the message one of them answers
/// cannot drift from the rest unnoticed.
const FORBIDS_QUALIFIER: [Addressed; 8] = [
    Addressed {
        operation: "ActivateMessageTemplate",
        method: "POST",
        suffix: "/activate",
        payload: Payload::Version,
    },
    Addressed {
        operation: "DeactivateMessageTemplate",
        method: "POST",
        suffix: "/deactivate",
        payload: Payload::Version,
    },
    Addressed {
        operation: "CreateMessageTemplateVersion",
        method: "POST",
        suffix: "/versions",
        payload: Payload::Empty,
    },
    Addressed {
        operation: "ListMessageTemplateVersions",
        method: "GET",
        suffix: "/versions",
        payload: Payload::None,
    },
    Addressed {
        operation: "UpdateMessageTemplate",
        method: "POST",
        suffix: "",
        payload: Payload::Content,
    },
    Addressed {
        operation: "UpdateMessageTemplateMetadata",
        method: "POST",
        suffix: "/metadata",
        payload: Payload::Metadata,
    },
    Addressed {
        operation: "CreateMessageTemplateAttachment",
        method: "POST",
        suffix: "/attachments",
        payload: Payload::Attachment,
    },
    Addressed {
        operation: "DeleteMessageTemplateAttachment",
        method: "DELETE",
        suffix: "/attachments/{attachmentId}",
        payload: Payload::None,
    },
];

/// Send one operation of a table against a message-template label, with the attachment the
/// route may name.
async fn send_addressed(
    emulator: &Emulator,
    addressed: Addressed,
    knowledge_base_id: &str,
    message_template_label: &str,
    attachment_id: &str,
) -> Reply {
    let suffix = addressed
        .suffix
        .replace(ATTACHMENT_MARKER, &label(attachment_id));
    let uri = format!(
        "/knowledgeBases/{}/messageTemplates/{}{suffix}",
        label(knowledge_base_id),
        label(message_template_label)
    );
    emulator
        .send(addressed.method, &uri, addressed.payload.body())
        .await
}

/// The body text one read reports as the template's `sms` content.
fn sms_body(reply: &Reply) -> String {
    reply.string("/messageTemplate/content/sms/body/plainText/content")
}

/// RULE: every operation whose `messageTemplateId` may not carry a version qualifier refuses
/// a label that carries one with the member's `@pattern`-constraint violation — the SAME
/// message on every one of them, whichever qualifier form the label carries and whether it is
/// written as the id or as the ARN — and writes nothing.
#[tokio::test]
async fn every_operation_that_forbids_a_qualifier_answers_the_same_pattern_violation() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "qualifier-one-message").await;
    let message_template_id = message_template(
        &emulator,
        &knowledge_base_id,
        "one message for every operation",
        CHANNEL_SUBTYPE,
        sms_content("as created"),
    )
    .await;

    // One attachment, so the row whose route names one addresses a file the working copy
    // really holds, and two versions, so a numbered qualifier row can name a version that
    // was published and one that was not.
    let uploaded = upload(
        &emulator,
        &knowledge_base_id,
        &message_template_id,
        "notes.txt",
        b"the attachment body, as a client uploaded it",
    )
    .await;
    assert_eq!(
        uploaded.status, 200,
        "case \"the attachment these rows address\": it must be uploaded, got HTTP {} with body {}",
        uploaded.status, uploaded.body
    );
    let attachment_id = uploaded.string("/attachment/attachmentId");
    let published = publish_versions(&emulator, &knowledge_base_id, &message_template_id, 2).await;
    assert_eq!(
        published,
        vec![1, 2],
        "case \"the versions these rows name\": they must be published, got {published:?}"
    );
    // One activated version, so the `$ACTIVE_VERSION` row names something that exists.
    let activated = activate(&emulator, &knowledge_base_id, &message_template_id, 1).await;
    assert_eq!(
        activated.status, 200,
        "case \"the version the managed qualifier names\": it must be activated, got HTTP {} with \
         body {}",
        activated.status, activated.body
    );
    // A working copy whose content this suite chose, so the state check below reads a value
    // no refusal could have written.
    let settled = update(
        &emulator,
        &knowledge_base_id,
        &message_template_id,
        json!({ "content": sms_content("before every refusal") }),
    )
    .await;
    assert_eq!(
        settled.status, 200,
        "case \"the working copy these rows must leave alone\": it must be written, got HTTP {} \
         with body {}",
        settled.status, settled.body
    );

    let template_arn = wisdom_arn(
        "aws",
        REGION,
        engine::EMULATOR_ACCOUNT_ID,
        &format!("message-template/{knowledge_base_id}/{message_template_id}"),
    );
    // Both label forms the member documents — "Can be either the ID or the ARN" — and every
    // qualifier form the member's own pattern admits (`(:[A-Z0-9_$]+){0,1}`,
    // `smithy_query(query="shape",shape="UuidOrArnOrEitherWithQualifier")`): the two managed
    // qualifiers, the number of the activated version, the number of another published one,
    // the number of a version this template never published, and a qualifier this service
    // defines no meaning for.
    let forms: [(&str, &str); 2] = [("the id", &message_template_id), ("the ARN", &template_arn)];
    let qualifiers = [
        QUALIFIER_LATEST,
        QUALIFIER_ACTIVE,
        "1",
        "2",
        "9",
        "$NO_SUCH_QUALIFIER",
    ];

    // Which cases answered which message: one key means the whole plane states the one
    // requirement, and a second key names the operations whose wording drifted.
    let mut answered: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for addressed in FORBIDS_QUALIFIER {
        for (form, written) in forms {
            for qualifier in qualifiers {
                let case = format!("{}/{form}/{qualifier}", addressed.operation);
                let refused = send_addressed(
                    &emulator,
                    addressed,
                    &knowledge_base_id,
                    &format!("{written}:{qualifier}"),
                    &attachment_id,
                )
                .await;
                assert_eq!(
                    refused.status, 400,
                    "case {case:?}: expected HTTP 400, got HTTP {} with body {}",
                    refused.status, refused.body
                );
                assert_eq!(
                    refused.error_type, "ValidationException",
                    "case {case:?}: expected error type ValidationException, got {:?}",
                    refused.error_type
                );
                answered
                    .entry(refused.string("/message"))
                    .or_default()
                    .push(case);
            }
        }
    }
    assert_eq!(
        answered.keys().cloned().collect::<Vec<String>>(),
        vec![MESSAGE_QUALIFIER_NOT_ACCEPTED.to_string()],
        "case \"every operation that forbids a qualifier\": expected the one message \
         {MESSAGE_QUALIFIER_NOT_ACCEPTED:?} from all of them, got {answered:?}"
    );

    // None of those refusals wrote anything: the versions are the two that were published,
    // the activated one is still the one that was activated, the working copy still holds the
    // content it was left with, and the attachment is still the one file it holds.
    let listing = list_versions(&emulator, &knowledge_base_id, &message_template_id, "").await;
    assert_eq!(
        listed_versions(&listing),
        published,
        "case \"after every refusal\": no refusal publishes or removes a version, got {:?}",
        listed_versions(&listing)
    );
    assert_eq!(
        listed_active_versions(&listing),
        vec![1],
        "case \"after every refusal\": no refusal changes which version is active, got {:?}",
        listed_active_versions(&listing)
    );
    let working_copy = read(&emulator, &knowledge_base_id, &message_template_id).await;
    assert_eq!(
        sms_body(&working_copy),
        "before every refusal",
        "case \"after every refusal\": no refusal writes the working copy, got {:?}",
        sms_body(&working_copy)
    );
    assert_eq!(
        attachment_ids(&working_copy),
        vec![attachment_id.clone()],
        "case \"after every refusal\": no refusal adds or removes an attachment, got {:?}",
        working_copy.field("/messageTemplate/attachments")
    );
}

/// RULE (the boundary of the one above): an operation whose `messageTemplateId` the model
/// documents without that sentence serves a qualified label, addressing the version the
/// qualifier names — so the refusal belongs to the operations that document it, not to the
/// label form.
#[tokio::test]
async fn an_operation_whose_label_admits_a_qualifier_serves_it() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "qualifier-admitted").await;
    let message_template_id = message_template(
        &emulator,
        &knowledge_base_id,
        "a label that may carry a qualifier",
        CHANNEL_SUBTYPE,
        sms_content("the first version"),
    )
    .await;

    // Two published versions and a working copy none of them holds, so every qualifier row
    // below names content only that version carries.
    let first = publish(&emulator, &knowledge_base_id, &message_template_id, None).await;
    assert_eq!(
        first.status, 200,
        "case \"version 1\": it must be published, got HTTP {} with body {}",
        first.status, first.body
    );
    for (round, body) in [(2, "the second version"), (3, "the working copy")] {
        let written = update(
            &emulator,
            &knowledge_base_id,
            &message_template_id,
            json!({ "content": sms_content(body) }),
        )
        .await;
        assert_eq!(
            written.status, 200,
            "case {body:?}: the working copy must be written, got HTTP {} with body {}",
            written.status, written.body
        );
        if round == 2 {
            let second = publish(&emulator, &knowledge_base_id, &message_template_id, None).await;
            assert_eq!(
                second.status, 200,
                "case {body:?}: it must be published, got HTTP {} with body {}",
                second.status, second.body
            );
        }
    }
    let activated = activate(&emulator, &knowledge_base_id, &message_template_id, 1).await;
    assert_eq!(
        activated.status, 200,
        "case \"the active version\": it must be activated, got HTTP {} with body {}",
        activated.status, activated.body
    );

    let template_arn = wisdom_arn(
        "aws",
        REGION,
        engine::EMULATOR_ACCOUNT_ID,
        &format!("message-template/{knowledge_base_id}/{message_template_id}"),
    );
    let forms: [(&str, &str); 2] = [("the id", &message_template_id), ("the ARN", &template_arn)];
    // Each qualifier form a read may carry, with the content the version it names holds and
    // the `versionNumber` that version reports — `None` for the working copy, which is no
    // version.
    let rows: [(&str, &str, Option<i64>); 4] = [
        ("1", "the first version", Some(1)),
        ("2", "the second version", Some(2)),
        (QUALIFIER_ACTIVE, "the first version", Some(1)),
        (QUALIFIER_LATEST, "the working copy", None),
    ];

    for (form, written) in forms {
        for (qualifier, content, version_number) in rows {
            let case = format!("GetMessageTemplate/{form}/{qualifier}");
            let served = read(
                &emulator,
                &knowledge_base_id,
                &format!("{written}:{qualifier}"),
            )
            .await;
            assert_eq!(
                served.status, 200,
                "case {case:?}: the label admits a qualifier, so the read must be served, got \
                 HTTP {} with body {}",
                served.status, served.body
            );
            assert_eq!(
                sms_body(&served),
                content,
                "case {case:?}: expected the content {content:?}, got {:?}",
                sms_body(&served)
            );
            assert_eq!(
                integer(&served, "/messageTemplate/versionNumber"),
                version_number,
                "case {case:?}: expected the version number {version_number:?}, got {:?}",
                integer(&served, "/messageTemplate/versionNumber")
            );
        }
    }

    // `RenderMessageTemplate` carries the same label, and its own documentation names no
    // qualifier restriction either: a qualified label renders the version it names.
    for (qualifier, content, _) in rows {
        let case = format!("RenderMessageTemplate/{qualifier}");
        let rendered = emulator
            .send(
                "POST",
                &format!(
                    "/knowledgeBases/{}/messageTemplates/{}/render",
                    label(&knowledge_base_id),
                    label(&format!("{message_template_id}:{qualifier}"))
                ),
                Payload::Attributes.body(),
            )
            .await;
        assert_eq!(
            rendered.status, 200,
            "case {case:?}: the label admits a qualifier, so the render must be served, got HTTP \
             {} with body {}",
            rendered.status, rendered.body
        );
        assert_eq!(
            rendered.string("/content/sms/body/plainText/content"),
            content,
            "case {case:?}: expected the rendered content {content:?}, got {:?}",
            rendered.string("/content/sms/body/plainText/content")
        );
    }

    // `DeleteMessageTemplate` carries the same label too: a numbered qualifier reaches that
    // ONE version, and what it removed is what a later listing reports.
    let deleted = emulator
        .send(
            "DELETE",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}",
                label(&knowledge_base_id),
                label(&format!("{message_template_id}:2"))
            ),
            None,
        )
        .await;
    assert_eq!(
        deleted.status, 204,
        "case \"DeleteMessageTemplate/2\": the label admits a qualifier, so the delete must be \
         served, got HTTP {} with body {}",
        deleted.status, deleted.body
    );
    let listing = list_versions(&emulator, &knowledge_base_id, &message_template_id, "").await;
    assert_eq!(
        listed_versions(&listing),
        vec![1],
        "case \"DeleteMessageTemplate/2\": exactly the version the qualifier named is gone, got \
         {:?}",
        listed_versions(&listing)
    );
    let working_copy = read(&emulator, &knowledge_base_id, &message_template_id).await;
    assert_eq!(
        working_copy.status, 200,
        "case \"DeleteMessageTemplate/2\": a versioned delete leaves the template itself, got \
         HTTP {} with body {}",
        working_copy.status, working_copy.body
    );
    assert_eq!(
        sms_body(&working_copy),
        "the working copy",
        "case \"DeleteMessageTemplate/2\": the working copy is untouched, got {:?}",
        sms_body(&working_copy)
    );
}
