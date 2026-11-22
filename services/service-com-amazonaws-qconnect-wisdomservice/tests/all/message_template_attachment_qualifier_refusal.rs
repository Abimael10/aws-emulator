// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the REFUSAL `DeleteMessageTemplateAttachment` answers a
//! `messageTemplateId` that carries a version qualifier with, driven over HTTP through the
//! same router the emulator serves with.
//!
//! The rule under test: on a message-template operation whose `messageTemplateId` the model
//! documents as a label that "cannot contain any qualifier"
//! (`smithy_query(query="doc",shape="DeleteMessageTemplateAttachmentRequest",member="messageTemplateId")`),
//! a label that carries one is refused with the modelled `ValidationException` (HTTP 400)
//! whose message is the member's `@pattern`-constraint violation, stating the expression the
//! service enforces for it — the one that admits no `:<QUALIFIER>` group — and nothing is
//! written. The member's own modelled pattern DOES admit a qualifier, so this requirement is
//! the service's rather than the model's, and the message is the same for every qualifier
//! form and every label form.
//!
//! The sibling suite `message_template_version_refusals` asserts that rule over the seven
//! operations of the plane whose route needs nothing but the two labels; this one covers the
//! eighth, whose route also names an attachment, over a table of label forms, qualifier
//! forms and `attachmentId` forms. Both the message it pins and the requests it sends come
//! from the helpers the suites of this plane already own.

use super::message_template::content_for;
use super::message_template_attachment_deletion::{
    attachment_ids, delete_attachment, get_template, upload, MESSAGE_QUALIFIER_NOT_ACCEPTED,
};
use super::message_template_versions::{knowledge_base, message_template, publish};
use super::resource_tags::{wisdom_arn, Emulator, ABSENT_UUID, REGION};
use serde_json::Value;

/// Every qualifier form a label may carry: the two managed qualifiers the read's own
/// documentation names (`smithy_query(query="doc",shape="GetMessageTemplate")`: "either an
/// actual version number or an Amazon Q Connect managed qualifier $ACTIVE_VERSION |
/// $LATEST"), the number of a version this template DID publish, the number of one it did
/// not, and a qualifier this service defines no meaning for — every one of them admitted by
/// the member's own pattern `(:[A-Z0-9_$]+){0,1}`
/// (`smithy_query(query="shape",shape="UuidOrArnOrEitherWithQualifier")`).
const QUALIFIERS: [&str; 5] = ["$LATEST", "$ACTIVE_VERSION", "1", "7", "$NO_SUCH_QUALIFIER"];

/// The channel subtype the templates of this suite are created under: the rule is about the
/// label, so one subtype carries it.
const CHANNEL_SUBTYPE: &str = "EMAIL";

/// The rule: every qualified `messageTemplateId` is refused with the member's
/// pattern-constraint violation — whichever qualifier form it carries, whether the label is
/// written as the id or as the ARN, and whether or not the `attachmentId` names a file the
/// working copy holds — and nothing is removed; the same request against the unqualified
/// label is served.
#[tokio::test]
async fn a_qualified_message_template_label_is_refused_as_a_pattern_violation() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "attachment-qualifier-refusal").await;
    let message_template_id = message_template(
        &emulator,
        &knowledge_base_id,
        "qualified attachment delete",
        CHANNEL_SUBTYPE,
        content_for(CHANNEL_SUBTYPE),
    )
    .await;

    // Two files, so the positive control below can delete one through each label form and
    // both deletes are served rather than one of them answering the state the first left.
    let files: [(&str, Vec<u8>); 2] = [
        (
            "notes.txt",
            b"the attachment body, as a client uploaded it".to_vec(),
        ),
        (
            "logo.PNG",
            vec![0x89, 0x50, 0x4e, 0x47, 0x00, 0x01, 0xfa, 0xff],
        ),
    ];
    let mut ids: Vec<String> = Vec::new();
    let mut urls: Vec<String> = Vec::new();
    for (name, file) in &files {
        let uploaded = upload(
            &emulator,
            &knowledge_base_id,
            &message_template_id,
            name,
            file,
        )
        .await;
        assert_eq!(
            uploaded.status, 200,
            "case {name:?}: the attachment these rows address must be uploaded, got HTTP {} with \
             body {}",
            uploaded.status, uploaded.body
        );
        ids.push(uploaded.string("/attachment/attachmentId"));
        urls.push(uploaded.string("/attachment/url"));
    }

    // One published version, so a numbered qualifier row names a version that EXISTS and the
    // refusal cannot be read as a report that it does not.
    let published = publish(&emulator, &knowledge_base_id, &message_template_id, None).await;
    assert_eq!(
        published.status, 200,
        "the version a numbered qualifier row names must be published, got HTTP {} with body {}",
        published.status, published.body
    );
    assert_eq!(
        published
            .field("/messageTemplate/versionNumber")
            .and_then(Value::as_i64),
        Some(1),
        "the first publish of this template mints version 1, got {:?}",
        published.field("/messageTemplate/versionNumber")
    );

    let template_arn = wisdom_arn(
        "aws",
        REGION,
        engine::EMULATOR_ACCOUNT_ID,
        &format!("message-template/{knowledge_base_id}/{message_template_id}"),
    );
    // Both label forms the member documents — "Can be either the ID or the ARN".
    let forms: [(&str, &str); 2] = [("the id", &message_template_id), ("the ARN", &template_arn)];
    // The attachment the working copy holds, and one it holds no file under: the refusal is
    // about the label, so it does not depend on what the route's attachment names.
    let attachments: [(&str, &str); 2] = [
        ("an attachment the template holds", &ids[0]),
        ("an id no template ever held", ABSENT_UUID),
    ];

    for (form, written) in forms {
        for qualifier in QUALIFIERS {
            for (held, addressed) in attachments {
                let case = format!("{form}/{qualifier}/{held}");
                let refused = delete_attachment(
                    &emulator,
                    &knowledge_base_id,
                    &format!("{written}:{qualifier}"),
                    addressed,
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
                assert_eq!(
                    refused.string("/message"),
                    MESSAGE_QUALIFIER_NOT_ACCEPTED,
                    "case {case:?}: expected the message {MESSAGE_QUALIFIER_NOT_ACCEPTED:?}, got \
                     {:?}",
                    refused.string("/message")
                );
            }
        }
    }

    // None of those refusals removed anything: both files are still reported, in upload
    // order, and each locator still serves its own bytes.
    let read = get_template(&emulator, &knowledge_base_id, &message_template_id).await;
    assert_eq!(
        attachment_ids(&read),
        ids,
        "case \"after every refusal\": expected the stored attachments {ids:?}, got {:?}",
        read.field("/messageTemplate/attachments")
    );
    for (index, (name, file)) in files.iter().enumerate() {
        let served = emulator.follow("GET", &urls[index]).await;
        assert_eq!(
            served.status, 200,
            "case {name:?}: the stored file must still be served, got HTTP {}",
            served.status
        );
        assert_eq!(
            &served.bytes, file,
            "case {name:?}: the stored file must be unchanged, got {} bytes",
            served.bytes.len()
        );
    }

    // The positive control: the same delete against a label with NO qualifier is served,
    // through either label form, and what it removed is what a later read reports.
    for (index, (form, written)) in forms.iter().enumerate() {
        let accepted = delete_attachment(&emulator, &knowledge_base_id, written, &ids[index]).await;
        assert_eq!(
            accepted.status, 204,
            "case {form:?}: the unqualified label carries no pattern violation, so the delete must \
             be served, got HTTP {} with body {}",
            accepted.status, accepted.body
        );
    }
    let emptied = get_template(&emulator, &knowledge_base_id, &message_template_id).await;
    assert_eq!(
        attachment_ids(&emptied),
        Vec::<String>::new(),
        "case \"the unqualified label\": the delete it served must be persisted, got {:?}",
        emptied.field("/messageTemplate/attachments")
    );
}
