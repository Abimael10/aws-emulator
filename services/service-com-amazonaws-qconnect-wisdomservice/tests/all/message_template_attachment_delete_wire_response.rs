// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WIRE RESPONSE of `DeleteMessageTemplateAttachment` — the
//! success status line and the wording of its conflict refusal — driven over HTTP through
//! the same router the emulator serves with, so only the status, the `x-amzn-errortype`
//! header and the JSON body are inspected.
//!
//! The two rules under test, each asserted over a TABLE of inputs rather than one request:
//!
//! * SUCCESS STATUS: a successful delete answers the status this service puts on the wire
//!   for this operation, which is not the code the model's `@http` member declares
//!   (`smithy_query(query="operation",operation="DeleteMessageTemplateAttachment")`:
//!   `code: 204`), with a body carrying no document — the response structure declares no
//!   members — and no error code. The status does not vary with the request: the rows
//!   differ along the knowledge base the template belongs to, how many files the working
//!   copy holds, which of them is deleted, the bytes each file carries, the form of the
//!   label the route names the template by (a bare id or an ARN) and whether a version was
//!   published from the template first.
//! * CONFLICT WORDING: a delete of an `attachmentId` the `$LATEST` working copy holds no
//!   file under is the modelled `ConflictException` (HTTP 409) carrying the CONFLICT
//!   class's own fixed sentence — it states the conflict with the state the template is in
//!   rather than reporting a missing resource, and interpolates nothing from the request or
//!   from state, so it is compared for equality and must carry no id the request named. The
//!   rows differ along why the working copy holds no such file: an id nothing ever held, an
//!   id a previous delete removed, an id another template of the same knowledge base
//!   holds, an id a template of a DIFFERENT knowledge base holds, and an id only a
//!   published version's snapshot still carries.
//!
//! The control for both: a route naming a knowledge base this account does not hold, or a
//! template that knowledge base does not hold, stays in the NOT-FOUND class (HTTP 404)
//! with its own sentence about the missing resource, so neither answer borrows the other's
//! wording. Every row also reads the template back through `GetMessageTemplate`, so a
//! handler that answered a status without doing — or without declining — the work cannot
//! pass.

use serde_json::Value;

use super::message_template::content_for;
use super::message_template_attachment_deletion::{
    attachment_ids, delete_attachment, get_template, upload, DELETED_STATUS,
    MESSAGE_ATTACHMENT_DELETE_CONFLICT, MESSAGE_KNOWLEDGE_BASE_NOT_FOUND,
    MESSAGE_MESSAGE_TEMPLATE_NOT_FOUND,
};
use super::message_template_versions::{knowledge_base, message_template, publish};
use super::resource_tags::{wisdom_arn, Emulator, Reply, ABSENT_UUID, REGION};

/// The channel subtype the templates of this suite are created with, and the content the
/// modelled constraints of that subtype admit.
const CHANNEL_SUBTYPE: &str = "EMAIL";

/// The modelled failure a delete of an id the working copy holds no file under is
/// (`smithy_query(query="operation",operation="DeleteMessageTemplateAttachment")`:
/// `ConflictException`, `httpError` 409).
const CONFLICT_CODE: &str = "ConflictException";
const CONFLICT_STATUS: u16 = 409;

/// The status the NOT-FOUND class of the same route answers with
/// (`ResourceNotFoundException`, `httpError` 404).
const NOT_FOUND_STATUS: u16 = 404;

/// One file a row uploads: the name it is stored under and the bytes it carries.
struct File {
    name: &'static str,
    body: Vec<u8>,
}

/// The files the rows upload, differing along the byte classes an attachment may carry:
/// text, binary with high bytes and a NUL, a single byte, and several kilobytes.
fn files() -> Vec<File> {
    vec![
        File {
            name: "notes.txt",
            body: b"the attachment body, as a client uploaded it".to_vec(),
        },
        File {
            name: "logo.PNG",
            body: vec![0x89, 0x50, 0x4e, 0x47, 0x00, 0x01, 0xfa, 0xff],
        },
        File {
            name: "x.y",
            body: b"x".to_vec(),
        },
        File {
            name: "handbook.csv",
            body: (0..4096u32).map(|index| (index % 251) as u8).collect(),
        },
    ]
}

/// One row of the success table.
struct SuccessRow {
    case: &'static str,
    /// How many of [`files`] the template holds before the delete.
    held: usize,
    /// The index, in upload order, of the attachment the row deletes.
    target: usize,
    /// Whether the delete addresses the template by its ARN rather than by its bare id.
    by_arn: bool,
    /// Whether a version is published from the template before the delete.
    published_first: bool,
    /// Whether the row's template lives in a knowledge base of its own.
    own_knowledge_base: bool,
}

fn success_rows() -> Vec<SuccessRow> {
    vec![
        SuccessRow {
            case: "the only attachment the working copy holds",
            held: 1,
            target: 0,
            by_arn: false,
            published_first: false,
            own_knowledge_base: false,
        },
        SuccessRow {
            case: "the first of two",
            held: 2,
            target: 0,
            by_arn: false,
            published_first: false,
            own_knowledge_base: false,
        },
        SuccessRow {
            case: "one in the middle of three",
            held: 3,
            target: 1,
            by_arn: false,
            published_first: false,
            own_knowledge_base: false,
        },
        SuccessRow {
            case: "the last of four, addressed by the template's ARN",
            held: 4,
            target: 3,
            by_arn: true,
            published_first: false,
            own_knowledge_base: false,
        },
        SuccessRow {
            case: "one of three, after a version was published from the template",
            held: 3,
            target: 2,
            by_arn: false,
            published_first: true,
            own_knowledge_base: false,
        },
        SuccessRow {
            case: "the only attachment of a template in a knowledge base of its own",
            held: 1,
            target: 0,
            by_arn: false,
            published_first: false,
            own_knowledge_base: true,
        },
    ]
}

/// The ARN form of one message-template label, as this endpoint mints it.
fn template_arn(knowledge_base_id: &str, message_template_id: &str) -> String {
    wisdom_arn(
        "aws",
        REGION,
        engine::EMULATOR_ACCOUNT_ID,
        &format!("message-template/{knowledge_base_id}/{message_template_id}"),
    )
}

/// Upload `held` files to one template's working copy, answering the `attachmentId`s the
/// uploads minted in upload order.
async fn upload_files(
    emulator: &Emulator,
    case: &str,
    knowledge_base_id: &str,
    message_template_id: &str,
    held: usize,
) -> Vec<String> {
    let mut ids = Vec::new();
    for file in files().into_iter().take(held) {
        let uploaded = upload(
            emulator,
            knowledge_base_id,
            message_template_id,
            file.name,
            &file.body,
        )
        .await;
        assert_eq!(
            uploaded.status, 200,
            "case {case:?}: the attachment {:?} this row needs must be uploaded, got HTTP {} \
             with body {}",
            file.name, uploaded.status, uploaded.body
        );
        ids.push(uploaded.string("/attachment/attachmentId"));
    }
    ids
}

/// How many members a response's body carries: a member-less response carries none, and
/// anything that is no JSON object is a failure of the rule.
fn body_members(reply: &Reply, case: &str) -> usize {
    match &reply.body {
        Value::Null => 0,
        Value::Object(members) => members.len(),
        other => panic!("case {case:?}: expected no document in the body, got {other}"),
    }
}

/// RULE: every successful delete answers the wire status of this operation, with no
/// document and no error code, whatever the request named.
#[tokio::test]
async fn every_successful_delete_answers_the_wire_status_with_no_document() {
    let emulator = Emulator::new();
    let shared = knowledge_base(&emulator, "attachment-delete-status").await;

    for row in success_rows() {
        let SuccessRow {
            case,
            held,
            target,
            by_arn,
            published_first,
            own_knowledge_base,
        } = row;
        let knowledge_base_id = if own_knowledge_base {
            knowledge_base(&emulator, &format!("kb for {case}")).await
        } else {
            shared.clone()
        };
        let message_template_id = message_template(
            &emulator,
            &knowledge_base_id,
            case,
            CHANNEL_SUBTYPE,
            content_for(CHANNEL_SUBTYPE),
        )
        .await;
        let ids = upload_files(
            &emulator,
            case,
            &knowledge_base_id,
            &message_template_id,
            held,
        )
        .await;
        if published_first {
            let published = publish(&emulator, &knowledge_base_id, &message_template_id, None).await;
            assert_eq!(
                published.status, 200,
                "case {case:?}: the version this row publishes first must be created, got HTTP {} \
                 with body {}",
                published.status, published.body
            );
        }

        let addressed = if by_arn {
            template_arn(&knowledge_base_id, &message_template_id)
        } else {
            message_template_id.clone()
        };
        let deleted = delete_attachment(&emulator, &knowledge_base_id, &addressed, &ids[target])
            .await;
        assert_eq!(
            deleted.status, DELETED_STATUS,
            "case {case:?}: expected the wire status {DELETED_STATUS} for the delete, got HTTP {} \
             with body {}",
            deleted.status, deleted.body
        );
        assert!(
            deleted.error_type.is_empty(),
            "case {case:?}: expected no modelled failure, got x-amzn-errortype {:?} with body {}",
            deleted.error_type,
            deleted.body
        );
        let members = body_members(&deleted, case);
        assert_eq!(
            members, 0,
            "case {case:?}: expected a body carrying no members, got {}",
            deleted.body
        );

        // The status is not a constant the handler answers without working: what is left is
        // what a later read reports, in upload order.
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
            "case {case:?}: the delete must be persisted, got {:?}",
            after.field("/messageTemplate/attachments")
        );
    }
}

/// Why one row's `attachmentId` is one the addressed working copy holds no file under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Absent {
    /// An id no template of this account ever held.
    NeverHeld,
    /// An id a previous delete already removed from this very working copy.
    AlreadyDeleted,
    /// An id another template of the SAME knowledge base holds.
    AnotherTemplate,
    /// An id a template of a DIFFERENT knowledge base holds.
    AnotherKnowledgeBase,
    /// An id only a published version's snapshot still carries.
    PublishedVersionOnly,
}

/// RULE: every conflict refusal of this operation carries the conflict class's own fixed
/// sentence, interpolating nothing the request named, and removes nothing.
#[tokio::test]
async fn every_conflict_refusal_carries_the_conflict_sentence() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "attachment-delete-conflict").await;
    let other_knowledge_base = knowledge_base(&emulator, "attachment-delete-conflict-other").await;

    // The template under test, holding two files; one of them is deleted so its id becomes
    // one the working copy no longer holds.
    let subject = message_template(
        &emulator,
        &knowledge_base_id,
        "the subject of the conflict rows",
        CHANNEL_SUBTYPE,
        content_for(CHANNEL_SUBTYPE),
    )
    .await;
    let subject_ids = upload_files(&emulator, "subject", &knowledge_base_id, &subject, 2).await;
    let removed = subject_ids[0].clone();
    let kept = subject_ids[1].clone();
    let first = delete_attachment(&emulator, &knowledge_base_id, &subject, &removed).await;
    assert_eq!(
        first.status, DELETED_STATUS,
        "case \"the first delete\": the id the rows reuse must be removed first, got HTTP {} with \
         body {}",
        first.status, first.body
    );

    // A sibling template of the same knowledge base, and one of another knowledge base,
    // each holding a file of their own.
    let sibling = message_template(
        &emulator,
        &knowledge_base_id,
        "a sibling template of the same knowledge base",
        CHANNEL_SUBTYPE,
        content_for(CHANNEL_SUBTYPE),
    )
    .await;
    let sibling_id = upload_files(&emulator, "sibling", &knowledge_base_id, &sibling, 1).await[0]
        .clone();
    let stranger = message_template(
        &emulator,
        &other_knowledge_base,
        "a template of another knowledge base",
        CHANNEL_SUBTYPE,
        content_for(CHANNEL_SUBTYPE),
    )
    .await;
    let stranger_id =
        upload_files(&emulator, "stranger", &other_knowledge_base, &stranger, 1).await[0].clone();

    // A template whose published version keeps a file its working copy no longer holds:
    // "attachments on available message template versions will remain unchanged"
    // (`smithy_query(query="doc",shape="DeleteMessageTemplateAttachment")`).
    let versioned = message_template(
        &emulator,
        &knowledge_base_id,
        "a template with a published version",
        CHANNEL_SUBTYPE,
        content_for(CHANNEL_SUBTYPE),
    )
    .await;
    let versioned_id =
        upload_files(&emulator, "versioned", &knowledge_base_id, &versioned, 1).await[0].clone();
    let published = publish(&emulator, &knowledge_base_id, &versioned, None).await;
    assert_eq!(
        published.status, 200,
        "case \"the published version\": the snapshot the rows read must be published, got HTTP {} \
         with body {}",
        published.status, published.body
    );
    let dropped = delete_attachment(&emulator, &knowledge_base_id, &versioned, &versioned_id).await;
    assert_eq!(
        dropped.status, DELETED_STATUS,
        "case \"the published version\": the working copy's file must be removed, got HTTP {} with \
         body {}",
        dropped.status, dropped.body
    );

    let rows: Vec<(Absent, &str, &str, String)> = vec![
        (
            Absent::NeverHeld,
            "an id nothing ever held",
            subject.as_str(),
            ABSENT_UUID.to_string(),
        ),
        (
            Absent::AlreadyDeleted,
            "an id a previous delete removed",
            subject.as_str(),
            removed.clone(),
        ),
        (
            Absent::AnotherTemplate,
            "an id a sibling template of the same knowledge base holds",
            subject.as_str(),
            sibling_id.clone(),
        ),
        (
            Absent::AnotherKnowledgeBase,
            "an id a template of another knowledge base holds",
            subject.as_str(),
            stranger_id.clone(),
        ),
        (
            Absent::PublishedVersionOnly,
            "an id only a published version's snapshot carries",
            versioned.as_str(),
            versioned_id.clone(),
        ),
    ];

    for (absent, case, addressed, attachment_id) in rows {
        let before = get_template(&emulator, &knowledge_base_id, addressed).await;
        let held_before = attachment_ids(&before);

        let refused = delete_attachment(&emulator, &knowledge_base_id, addressed, &attachment_id)
            .await;
        assert_eq!(
            refused.status, CONFLICT_STATUS,
            "case {case:?} ({absent:?}): expected HTTP {CONFLICT_STATUS}, got HTTP {} with body {}",
            refused.status, refused.body
        );
        assert!(
            refused.error_type.contains(CONFLICT_CODE),
            "case {case:?} ({absent:?}): expected the {CONFLICT_CODE} code, got x-amzn-errortype \
             {:?} with body {}",
            refused.error_type,
            refused.body
        );
        let message = refused.string("/message");
        assert_eq!(
            message, MESSAGE_ATTACHMENT_DELETE_CONFLICT,
            "case {case:?} ({absent:?}): expected the message \
             {MESSAGE_ATTACHMENT_DELETE_CONFLICT:?}, got {message:?}"
        );
        // The sentence is fixed text: it interpolates nothing the request named.
        for named in [
            attachment_id.as_str(),
            addressed,
            knowledge_base_id.as_str(),
        ] {
            assert!(
                !message.contains(named),
                "case {case:?} ({absent:?}): the fixed sentence must interpolate nothing the \
                 request named, but it carries {named:?}: {message:?}"
            );
        }

        // A refusal removes nothing, so the rows that follow see the same state.
        let after = get_template(&emulator, &knowledge_base_id, addressed).await;
        assert_eq!(
            attachment_ids(&after),
            held_before,
            "case {case:?} ({absent:?}): a refused delete must remove nothing, got {:?}",
            after.field("/messageTemplate/attachments")
        );
    }

    // The file the subject still holds is untouched by every refusal above, and deleting it
    // is still served with the wire status.
    let served = delete_attachment(&emulator, &knowledge_base_id, &subject, &kept).await;
    assert_eq!(
        served.status, DELETED_STATUS,
        "case \"the file the subject still holds\": expected the wire status {DELETED_STATUS}, got \
         HTTP {} with body {}",
        served.status, served.body
    );
}

/// The CONTROL for both rules: the not-found class of the same route keeps its own
/// sentences, so neither class borrows the other's wording.
#[tokio::test]
async fn the_not_found_class_of_the_same_route_keeps_its_own_sentence() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "attachment-delete-not-found").await;
    let message_template_id = message_template(
        &emulator,
        &knowledge_base_id,
        "the template the control rows address",
        CHANNEL_SUBTYPE,
        content_for(CHANNEL_SUBTYPE),
    )
    .await;
    let attachment_id = upload_files(
        &emulator,
        "the control",
        &knowledge_base_id,
        &message_template_id,
        1,
    )
    .await[0]
        .clone();

    for (case, addressed_knowledge_base, addressed_template, expected) in [
        (
            "a knowledge base this account does not hold",
            ABSENT_UUID,
            message_template_id.as_str(),
            MESSAGE_KNOWLEDGE_BASE_NOT_FOUND,
        ),
        (
            "a template this knowledge base does not hold",
            knowledge_base_id.as_str(),
            ABSENT_UUID,
            MESSAGE_MESSAGE_TEMPLATE_NOT_FOUND,
        ),
    ] {
        let refused = delete_attachment(
            &emulator,
            addressed_knowledge_base,
            addressed_template,
            &attachment_id,
        )
        .await;
        assert_eq!(
            refused.status, NOT_FOUND_STATUS,
            "case {case:?}: expected HTTP {NOT_FOUND_STATUS}, got HTTP {} with body {}",
            refused.status, refused.body
        );
        let message = refused.string("/message");
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );
        assert_ne!(
            message, MESSAGE_ATTACHMENT_DELETE_CONFLICT,
            "case {case:?}: the not-found class must not carry the conflict sentence, got \
             {message:?}"
        );
    }

    // And the attachment the control rows named is still there: a refusal removed nothing.
    let after = get_template(&emulator, &knowledge_base_id, &message_template_id).await;
    assert_eq!(
        attachment_ids(&after),
        vec![attachment_id.clone()],
        "case \"after the control rows\": a refused delete must remove nothing, got {:?}",
        after.field("/messageTemplate/attachments")
    );
}
