// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DeleteMessageTemplateAttachment`, driven over HTTP through the
//! same router the emulator serves with: only the HTTP status, the `x-amzn-errortype`
//! header and the JSON body are inspected, so a response that is not computed from
//! persisted state cannot pass.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than the one recorded
//! request:
//!
//! * a delete removes EXACTLY the attachment its `attachmentId` names from the template's
//!   working copy and persists that: every later read reports the attachments that are
//!   left, in upload order, the working copy's `messageTemplateContentSha256` changes —
//!   the model says attachments are part of what it is calculated over — its
//!   `lastModifiedTime` is re-stamped, the removed file's locator stops serving it, and
//!   every surviving locator still serves its own file;
//! * attachments on published versions "remain unchanged": a delete reaches the working
//!   copy alone, so a version-addressed read reports the attachments its snapshot was
//!   published with and their locators still serve those files;
//! * an `attachmentId` the working copy holds no file under — one no template ever held,
//!   one a previous delete already removed, one another template holds, and one only a
//!   published version still carries — is the modelled `ConflictException` (HTTP 409),
//!   since the template the route names exists and it is the state it is in that the
//!   request cannot be processed against, and removes nothing;
//! * a label that names no template of this knowledge base, and a knowledge base this
//!   account does not hold, are HTTP 404; a `messageTemplateId` carrying a version
//!   qualifier is the modelled `ValidationException` (HTTP 400), since the operation
//!   documents the label as one that "cannot contain any qualifier".

use serde_json::{json, Value};

use super::message_template::content_for;
use super::message_template_qualifier::MESSAGE_QUALIFIER_NOT_ACCEPTED;
use super::resource_tags::{label, wisdom_arn, Emulator, Reply, ABSENT_UUID, OTHER_ACCOUNT, REGION};

/// The only value the modelled `ContentDisposition` enumeration declares
/// (`smithy_query(query="shape",shape="ContentDisposition")`), written out here rather than
/// imported from the implementation.
const CONTENT_DISPOSITION: &str = "ATTACHMENT";

/// The fixed sentences this endpoint answers the resources of this family with when it
/// holds none, written out here rather than imported from the implementation, so the
/// assertions pin what a client reads.
const MESSAGE_ATTACHMENT_NOT_FOUND: &str = "Message template attachment does not exist.";
const MESSAGE_MESSAGE_TEMPLATE_NOT_FOUND: &str = "MessageTemplate does not exist.";
const MESSAGE_KNOWLEDGE_BASE_NOT_FOUND: &str = "Knowledge Base does not exist.";

/// The two managed version qualifiers the read's own documentation names
/// (`smithy_query(query="doc",shape="GetMessageTemplate")`: "either an actual version
/// number or an Amazon Q Connect managed qualifier $ACTIVE_VERSION | $LATEST").
const QUALIFIER_LATEST: &str = "$LATEST";
const QUALIFIER_ACTIVE: &str = "$ACTIVE_VERSION";

/// `CreateKnowledgeBase` of the type that holds message templates, answering with the id
/// its create minted.
async fn knowledge_base(emulator: &Emulator, name: &str) -> String {
    let reply = emulator
        .send(
            "POST",
            "/knowledgeBases",
            Some(json!({ "name": name, "knowledgeBaseType": "MESSAGE_TEMPLATES" })),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "case {name:?}: the knowledge base this suite nests under must be created, got HTTP {} \
         with body {}",
        reply.status, reply.body
    );
    reply.string("/knowledgeBase/knowledgeBaseId")
}

/// `CreateMessageTemplate`, answering with the `messageTemplateId` its create minted.
async fn message_template(emulator: &Emulator, knowledge_base_id: &str, name: &str) -> String {
    let reply = emulator
        .send(
            "POST",
            &format!(
                "/knowledgeBases/{}/messageTemplates",
                label(knowledge_base_id)
            ),
            Some(json!({
                "name": name,
                "channelSubtype": "EMAIL",
                "content": content_for("EMAIL"),
            })),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "case {name:?}: the message template these rows attach to must be created, got HTTP {} \
         with body {}",
        reply.status, reply.body
    );
    reply.string("/messageTemplate/messageTemplateId")
}

/// `CreateMessageTemplateAttachment`: one file uploaded to the template's working copy.
pub(crate) async fn upload(
    emulator: &Emulator,
    knowledge_base_id: &str,
    message_template_label: &str,
    name: &str,
    file: &[u8],
) -> Reply {
    emulator
        .send(
            "POST",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}/attachments",
                label(knowledge_base_id),
                label(message_template_label)
            ),
            Some(json!({
                "name": name,
                "contentDisposition": CONTENT_DISPOSITION,
                "body": ::aws_smithy_types::base64::encode(file),
            })),
        )
        .await
}

/// `DeleteMessageTemplateAttachment`.
pub(crate) async fn delete_attachment(
    emulator: &Emulator,
    knowledge_base_label: &str,
    message_template_label: &str,
    attachment_id: &str,
) -> Reply {
    emulator
        .send(
            "DELETE",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}/attachments/{}",
                label(knowledge_base_label),
                label(message_template_label),
                label(attachment_id)
            ),
            None,
        )
        .await
}

/// `GetMessageTemplate` on whatever label the row reads with.
pub(crate) async fn get_template(
    emulator: &Emulator,
    knowledge_base_id: &str,
    message_template_label: &str,
) -> Reply {
    emulator
        .send(
            "GET",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}",
                label(knowledge_base_id),
                label(message_template_label)
            ),
            None,
        )
        .await
}

/// `CreateMessageTemplateVersion`: publish the working copy as the next version.
async fn publish(emulator: &Emulator, knowledge_base_id: &str, message_template_id: &str) -> Reply {
    emulator
        .send(
            "POST",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}/versions",
                label(knowledge_base_id),
                label(message_template_id)
            ),
            Some(json!({})),
        )
        .await
}

/// The `attachmentId`s a read of a message template reports, in the order it reports them.
/// The empty list for a template that reports no `attachments` member at all, which is how
/// a template holding none answers.
pub(crate) fn attachment_ids(reply: &Reply) -> Vec<String> {
    reply
        .field("/messageTemplate/attachments")
        .and_then(Value::as_array)
        .map(|attachments| {
            attachments
                .iter()
                .map(|attachment| {
                    attachment
                        .pointer("/attachmentId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One instant a response reports, as the number of seconds since the epoch it names.
///
/// `lastModifiedTime` carries `@timestampFormat("date-time")`, so what a response carries
/// is an RFC 3339 STRING; this reads it back to an instant so a row can assert that a write
/// did not move it backwards. `None` for an absent member and for a value that is no
/// date-time string at all.
fn instant(reply: &Reply, pointer: &str) -> Option<f64> {
    let text = reply.field(pointer)?.as_str()?.to_string();
    ::aws_smithy_types::DateTime::from_str(&text, ::aws_smithy_types::date_time::Format::DateTime)
        .ok()
        .map(|parsed| parsed.as_secs_f64())
}

/// One attachment this suite uploads: the name it is stored under and the file it carries.
struct Upload {
    name: String,
    file: Vec<u8>,
}

/// The files a row uploads before it deletes one of them. The rows differ along what the
/// rule generalises over: the number of attachments the template holds, which of them is
/// deleted (the only one, the first, one in the middle, the last) and the bytes each file
/// carries (text, binary, a single byte, several kilobytes).
struct DeleteCase {
    case: &'static str,
    uploads: Vec<Upload>,
    /// The index, in upload order, of the attachment this row deletes.
    target: usize,
    /// Whether the delete addresses the template by its ARN rather than by its bare id.
    by_arn: bool,
}

/// The file set the multi-attachment rows share: every byte class an upload may carry.
fn files() -> Vec<Upload> {
    vec![
        Upload {
            name: "notes.txt".to_string(),
            file: b"the attachment body, as a client uploaded it".to_vec(),
        },
        Upload {
            name: "logo.PNG".to_string(),
            file: vec![0x89, 0x50, 0x4e, 0x47, 0x00, 0x01, 0xfa, 0xff],
        },
        Upload {
            name: "x.y".to_string(),
            file: b"x".to_vec(),
        },
        Upload {
            name: "handbook.csv".to_string(),
            file: (0..4096u32).map(|index| (index % 251) as u8).collect(),
        },
    ]
}

fn delete_cases() -> Vec<DeleteCase> {
    vec![
        DeleteCase {
            case: "the only attachment the template holds",
            uploads: vec![Upload {
                name: "only.txt".to_string(),
                file: b"the only file".to_vec(),
            }],
            target: 0,
            by_arn: false,
        },
        DeleteCase {
            case: "the first of three",
            uploads: files().into_iter().take(3).collect(),
            target: 0,
            by_arn: false,
        },
        DeleteCase {
            case: "one in the middle of three",
            uploads: files().into_iter().take(3).collect(),
            target: 1,
            by_arn: false,
        },
        DeleteCase {
            case: "the last of three",
            uploads: files().into_iter().take(3).collect(),
            target: 2,
            by_arn: false,
        },
        DeleteCase {
            case: "one of four, addressed by the template's ARN",
            uploads: files(),
            target: 3,
            by_arn: true,
        },
    ]
}

/// The rule: a delete removes exactly the attachment its id names from the working copy and
/// persists that — the reads after it report what is left, out of the stored record.
#[tokio::test]
async fn a_delete_removes_exactly_the_attachment_its_id_names() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "attachment-deletion").await;

    for case in delete_cases() {
        let DeleteCase {
            case,
            uploads,
            target,
            by_arn,
        } = case;
        let message_template_id = message_template(&emulator, &knowledge_base_id, case).await;
        let template_label = if by_arn {
            wisdom_arn(
                "aws",
                REGION,
                engine::EMULATOR_ACCOUNT_ID,
                &format!("message-template/{knowledge_base_id}/{message_template_id}"),
            )
        } else {
            message_template_id.clone()
        };

        // The checksum of the template BEFORE any attachment: deleting every attachment
        // again must bring it back to exactly this value, since the model says the
        // attachments are part of what it is calculated over.
        let empty = get_template(&emulator, &knowledge_base_id, &message_template_id).await;
        let checksum_without_attachments = empty.string("/messageTemplate/messageTemplateContentSha256");

        let mut ids: Vec<String> = Vec::new();
        let mut urls: Vec<String> = Vec::new();
        for upload_case in &uploads {
            let uploaded = upload(
                &emulator,
                &knowledge_base_id,
                &message_template_id,
                &upload_case.name,
                &upload_case.file,
            )
            .await;
            assert_eq!(
                uploaded.status, 200,
                "case {case:?}: the attachment {:?} this row deletes from must be uploaded, got \
                 HTTP {} with body {}",
                upload_case.name, uploaded.status, uploaded.body
            );
            ids.push(uploaded.string("/attachment/attachmentId"));
            urls.push(uploaded.string("/attachment/url"));
        }

        let before = get_template(&emulator, &knowledge_base_id, &message_template_id).await;
        assert_eq!(
            attachment_ids(&before),
            ids,
            "case {case:?}: the template must hold every uploaded attachment before the delete, \
             got {:?}",
            before.field("/messageTemplate/attachments")
        );
        let checksum_before = before.string("/messageTemplate/messageTemplateContentSha256");
        let modified_before = instant(&before, "/messageTemplate/lastModifiedTime");
        assert!(
            modified_before.is_some(),
            "case {case:?}: the read must report a date-time lastModifiedTime, got {:?}",
            before.field("/messageTemplate/lastModifiedTime")
        );

        let deleted = delete_attachment(
            &emulator,
            &knowledge_base_id,
            &template_label,
            &ids[target],
        )
        .await;
        assert_eq!(
            deleted.status, 204,
            "case {case:?}: expected HTTP 204 for the delete, got HTTP {} with body {}",
            deleted.status, deleted.body
        );
        // A response whose output structure declares no members carries no document.
        let members = match &deleted.body {
            Value::Null => 0,
            Value::Object(members) => members.len(),
            other => panic!("case {case:?}: expected no document in the body, got {other}"),
        };
        assert_eq!(
            members, 0,
            "case {case:?}: the delete answers a body carrying no members, got {}",
            deleted.body
        );

        // Exactly the deleted attachment is gone, and the rest keep their upload order.
        let expected: Vec<String> = ids
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != target)
            .map(|(_, id)| id.clone())
            .collect();
        let after = get_template(&emulator, &knowledge_base_id, &message_template_id).await;
        assert_eq!(
            attachment_ids(&after),
            expected,
            "case {case:?}: expected the attachments {expected:?} after the delete, got {:?}",
            after.field("/messageTemplate/attachments")
        );
        let checksum_after = after.string("/messageTemplate/messageTemplateContentSha256");
        assert_ne!(
            checksum_after, checksum_before,
            "case {case:?}: the attachments are part of what the content checksum is calculated \
             over, so removing one must change it away from {checksum_before:?}"
        );
        let modified_after = instant(&after, "/messageTemplate/lastModifiedTime");
        assert!(
            modified_after >= modified_before,
            "case {case:?}: the delete writes the working copy, so lastModifiedTime must not \
             move back from {modified_before:?}, got {modified_after:?}"
        );

        // The locator the deleted file was served under stops serving it, while every
        // surviving locator still answers its own file.
        let gone = emulator.follow("GET", &urls[target]).await;
        assert!(
            gone.status >= 400,
            "case {case:?}: the url of a deleted attachment must not be served, got HTTP {} with \
             {} bytes",
            gone.status,
            gone.bytes.len()
        );
        assert_ne!(
            gone.bytes, uploads[target].file,
            "case {case:?}: no file bytes may come back after the delete, got {} bytes",
            gone.bytes.len()
        );
        for (index, upload_case) in uploads.iter().enumerate() {
            if index == target {
                continue;
            }
            let served = emulator.follow("GET", &urls[index]).await;
            assert_eq!(
                served.status, 200,
                "case {case:?}: the surviving attachment {:?} must still be served, got HTTP {}",
                upload_case.name, served.status
            );
            assert_eq!(
                served.bytes, upload_case.file,
                "case {case:?}: the surviving attachment {:?} must serve its own file, got {} \
                 bytes",
                upload_case.name,
                served.bytes.len()
            );
        }

        // A second delete of the same id names nothing to remove.
        let again = delete_attachment(
            &emulator,
            &knowledge_base_id,
            &message_template_id,
            &ids[target],
        )
        .await;
        assert_eq!(
            again.status, 409,
            "case {case:?}: a second delete of the same id must be HTTP 409, got HTTP {} with \
             body {}",
            again.status, again.body
        );
        assert_eq!(
            again.error_type, "ConflictException",
            "case {case:?}: expected error type ConflictException, got {:?}",
            again.error_type
        );
        let expected_message = MESSAGE_ATTACHMENT_NOT_FOUND;
        assert_eq!(
            again.string("/message"),
            expected_message,
            "case {case:?}: expected the message {expected_message:?}, got {:?}",
            again.string("/message")
        );

        // Deleting what is left empties the member and brings the checksum back to the one
        // the template carried before any attachment was uploaded.
        for id in &expected {
            let removed =
                delete_attachment(&emulator, &knowledge_base_id, &message_template_id, id).await;
            assert_eq!(
                removed.status, 204,
                "case {case:?}: expected HTTP 204 deleting the remaining {id:?}, got HTTP {} with \
                 body {}",
                removed.status, removed.body
            );
        }
        let emptied = get_template(&emulator, &knowledge_base_id, &message_template_id).await;
        assert!(
            emptied.field("/messageTemplate/attachments").is_none(),
            "case {case:?}: a template holding no attachment reports no attachments member, got \
             {:?}",
            emptied.field("/messageTemplate/attachments")
        );
        assert_eq!(
            emptied.string("/messageTemplate/messageTemplateContentSha256"),
            checksum_without_attachments,
            "case {case:?}: with every attachment deleted the checksum must be the one the \
             template carried before any upload ({checksum_without_attachments:?}), got {:?}",
            emptied.string("/messageTemplate/messageTemplateContentSha256")
        );
    }
}

/// The rule the operation documents: the delete reaches the working copy ALONE, so
/// "attachments on available message template versions will remain unchanged" — a
/// version-addressed read reports what its snapshot was published with, and the files those
/// locators name are still served.
#[tokio::test]
async fn a_delete_leaves_published_versions_untouched() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "attachment-versions").await;

    // The rows differ along which published attachment the working-copy delete removes.
    for (case, target) in [("the first of two", 0usize), ("the second of two", 1usize)] {
        let message_template_id = message_template(&emulator, &knowledge_base_id, case).await;
        let uploads = files().into_iter().take(2).collect::<Vec<Upload>>();
        let mut ids: Vec<String> = Vec::new();
        let mut urls: Vec<String> = Vec::new();
        for upload_case in &uploads {
            let uploaded = upload(
                &emulator,
                &knowledge_base_id,
                &message_template_id,
                &upload_case.name,
                &upload_case.file,
            )
            .await;
            assert_eq!(
                uploaded.status, 200,
                "case {case:?}: the attachment {:?} must be uploaded before the publish, got HTTP \
                 {} with body {}",
                upload_case.name, uploaded.status, uploaded.body
            );
            ids.push(uploaded.string("/attachment/attachmentId"));
            urls.push(uploaded.string("/attachment/url"));
        }

        let published = publish(&emulator, &knowledge_base_id, &message_template_id).await;
        assert_eq!(
            published.status, 200,
            "case {case:?}: the version this row reads must be published, got HTTP {} with body \
             {}",
            published.status, published.body
        );
        let version = published
            .field("/messageTemplate/versionNumber")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let version_label = format!("{message_template_id}:{version}");
        let version_before = get_template(&emulator, &knowledge_base_id, &version_label).await;
        let version_checksum =
            version_before.string("/messageTemplate/messageTemplateContentSha256");

        let deleted = delete_attachment(
            &emulator,
            &knowledge_base_id,
            &message_template_id,
            &ids[target],
        )
        .await;
        assert_eq!(
            deleted.status, 204,
            "case {case:?}: expected HTTP 204 for the delete, got HTTP {} with body {}",
            deleted.status, deleted.body
        );

        // The working copy lost exactly that file...
        let working_copy = get_template(
            &emulator,
            &knowledge_base_id,
            &format!("{message_template_id}:{QUALIFIER_LATEST}"),
        )
        .await;
        let expected: Vec<String> = ids
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != target)
            .map(|(_, id)| id.clone())
            .collect();
        assert_eq!(
            attachment_ids(&working_copy),
            expected,
            "case {case:?}: the working copy must report {expected:?} after the delete, got {:?}",
            working_copy.field("/messageTemplate/attachments")
        );

        // ...while the published version reports, and serves, both of them still.
        let version_after = get_template(&emulator, &knowledge_base_id, &version_label).await;
        assert_eq!(
            attachment_ids(&version_after),
            ids,
            "case {case:?}: a published version keeps the attachments it was published with \
             ({ids:?}), got {:?}",
            version_after.field("/messageTemplate/attachments")
        );
        assert_eq!(
            version_after.string("/messageTemplate/messageTemplateContentSha256"),
            version_checksum,
            "case {case:?}: the published version's checksum must be unchanged at \
             {version_checksum:?}, got {:?}",
            version_after.string("/messageTemplate/messageTemplateContentSha256")
        );
        let served = emulator.follow("GET", &urls[target]).await;
        assert_eq!(
            served.status, 200,
            "case {case:?}: the file a published version still carries must still be served, got \
             HTTP {}",
            served.status
        );
        assert_eq!(
            served.bytes, uploads[target].file,
            "case {case:?}: the published version's attachment must serve its own file, got {} \
             bytes",
            served.bytes.len()
        );

        // An id only the published version carries names nothing on the working copy.
        let again = delete_attachment(
            &emulator,
            &knowledge_base_id,
            &message_template_id,
            &ids[target],
        )
        .await;
        assert_eq!(
            again.status, 409,
            "case {case:?}: an id only a published version carries must be HTTP 409 on the \
             working copy, got HTTP {} with body {}",
            again.status, again.body
        );
        let expected_message = MESSAGE_ATTACHMENT_NOT_FOUND;
        assert_eq!(
            again.string("/message"),
            expected_message,
            "case {case:?}: expected the message {expected_message:?}, got {:?}",
            again.string("/message")
        );
    }
}

/// The rule: a delete addressed to a resource this account does not hold, or through a
/// label the operation does not accept, is refused with the modelled error and removes
/// nothing.
#[tokio::test]
async fn a_delete_addressed_to_nothing_this_account_holds_is_refused() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "attachment-refusals").await;
    let message_template_id = message_template(&emulator, &knowledge_base_id, "refusals").await;
    let file = b"the file every refusal below leaves alone".to_vec();
    let uploaded = upload(
        &emulator,
        &knowledge_base_id,
        &message_template_id,
        "kept.txt",
        &file,
    )
    .await;
    assert_eq!(
        uploaded.status, 200,
        "case \"the file the refusals leave alone\": it must be uploaded, got HTTP {} with body {}",
        uploaded.status, uploaded.body
    );
    let attachment_id = uploaded.string("/attachment/attachmentId");
    let url = uploaded.string("/attachment/url");

    // Another knowledge base's template, holding its own attachment: neither its template
    // nor its attachment is reachable through this knowledge base.
    let other_base_id = knowledge_base(&emulator, "attachment-refusals-elsewhere").await;
    let elsewhere_id = message_template(&emulator, &other_base_id, "held-elsewhere").await;
    let elsewhere = upload(
        &emulator,
        &other_base_id,
        &elsewhere_id,
        "elsewhere.txt",
        b"another template's file",
    )
    .await;
    assert_eq!(
        elsewhere.status, 200,
        "case \"another template's attachment\": it must be uploaded, got HTTP {} with body {}",
        elsewhere.status, elsewhere.body
    );
    let elsewhere_attachment_id = elsewhere.string("/attachment/attachmentId");

    // Rows: every label form that names no template of THIS knowledge base.
    let missing = [
        ("an id no template holds", ABSENT_UUID.to_string()),
        (
            "an ARN of this account naming nothing",
            wisdom_arn(
                "aws",
                REGION,
                engine::EMULATOR_ACCOUNT_ID,
                &format!("message-template/{knowledge_base_id}/{ABSENT_UUID}"),
            ),
        ),
        (
            "an ARN of another account",
            wisdom_arn(
                "aws",
                REGION,
                OTHER_ACCOUNT,
                &format!("message-template/{knowledge_base_id}/{message_template_id}"),
            ),
        ),
        ("another knowledge base's template", elsewhere_id.clone()),
    ];
    for (case, template_label) in missing {
        let refused =
            delete_attachment(&emulator, &knowledge_base_id, &template_label, &attachment_id).await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: expected HTTP 404, got HTTP {} with body {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_type, "ResourceNotFoundException",
            "case {case:?}: expected error type ResourceNotFoundException, got {:?}",
            refused.error_type
        );
        assert_eq!(
            refused.string("/message"),
            MESSAGE_MESSAGE_TEMPLATE_NOT_FOUND,
            "case {case:?}: expected the message {MESSAGE_MESSAGE_TEMPLATE_NOT_FOUND:?}, got {:?}",
            refused.string("/message")
        );
    }

    // A knowledge base this account does not hold is reported as the missing resource.
    let no_parent =
        delete_attachment(&emulator, ABSENT_UUID, &message_template_id, &attachment_id).await;
    assert_eq!(
        no_parent.status, 404,
        "case \"a knowledge base this account does not hold\": expected HTTP 404, got HTTP {} \
         with body {}",
        no_parent.status, no_parent.body
    );
    assert_eq!(
        no_parent.string("/message"),
        MESSAGE_KNOWLEDGE_BASE_NOT_FOUND,
        "case \"a knowledge base this account does not hold\": expected the message \
         {MESSAGE_KNOWLEDGE_BASE_NOT_FOUND:?}, got {:?}",
        no_parent.string("/message")
    );

    // Rows: every `attachmentId` form the working copy holds no file under.
    let unheld = [
        ("an id no template ever held", ABSENT_UUID.to_string()),
        (
            "another template's attachment",
            elsewhere_attachment_id.clone(),
        ),
    ];
    for (case, unheld_id) in unheld {
        let refused = delete_attachment(
            &emulator,
            &knowledge_base_id,
            &message_template_id,
            &unheld_id,
        )
        .await;
        assert_eq!(
            refused.status, 409,
            "case {case:?}: expected HTTP 409, got HTTP {} with body {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_type, "ConflictException",
            "case {case:?}: expected error type ConflictException, got {:?}",
            refused.error_type
        );
        let expected_message = MESSAGE_ATTACHMENT_NOT_FOUND;
        assert_eq!(
            refused.string("/message"),
            expected_message,
            "case {case:?}: expected the message {expected_message:?}, got {:?}",
            refused.string("/message")
        );
    }

    // Rows: every qualifier form the label may carry, each refused rather than read as
    // naming a version to delete from.
    for qualifier in [QUALIFIER_LATEST, QUALIFIER_ACTIVE, "1"] {
        let refused = delete_attachment(
            &emulator,
            &knowledge_base_id,
            &format!("{message_template_id}:{qualifier}"),
            &attachment_id,
        )
        .await;
        assert_eq!(
            refused.status, 400,
            "case {qualifier:?}: expected HTTP 400, got HTTP {} with body {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_type, "ValidationException",
            "case {qualifier:?}: expected error type ValidationException, got {:?}",
            refused.error_type
        );
        assert_eq!(
            refused.string("/message"),
            MESSAGE_QUALIFIER_NOT_ACCEPTED,
            "case {qualifier:?}: expected the message {MESSAGE_QUALIFIER_NOT_ACCEPTED:?}, got {:?}",
            refused.string("/message")
        );
    }

    // None of the refusals removed anything: the file is still reported and still served.
    let read = get_template(&emulator, &knowledge_base_id, &message_template_id).await;
    assert_eq!(
        attachment_ids(&read),
        vec![attachment_id.clone()],
        "case \"after every refusal\": expected the one stored attachment {attachment_id:?}, got \
         {:?}",
        read.field("/messageTemplate/attachments")
    );
    let served = emulator.follow("GET", &url).await;
    assert_eq!(
        served.status, 200,
        "case \"after every refusal\": the stored file must still be served, got HTTP {}",
        served.status
    );
    assert_eq!(
        served.bytes, file,
        "case \"after every refusal\": the stored file must be unchanged, got {} bytes",
        served.bytes.len()
    );
}
