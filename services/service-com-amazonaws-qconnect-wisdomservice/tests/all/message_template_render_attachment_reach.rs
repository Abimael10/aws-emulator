// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the REACH of the message-template attachment projection on the
//! render path, driven over HTTP through the same router the emulator serves with: only the
//! HTTP status, the JSON body and the bytes a published URL serves are inspected, so a
//! response that is not computed from persisted state cannot pass.
//!
//! The rule under test, asserted over a TABLE of inputs rather than one recorded request:
//! a render reports, under `attachments`, one FULLY POPULATED element per attachment the
//! copy of the template its label addresses currently holds — whatever form that label
//! takes — and the `url` each element carries can be followed to download exactly the file
//! that attachment was uploaded with.
//!
//! `MessageTemplateAttachment` declares every one of its six members `@required`
//! (`smithy_query(query="shape",shape="MessageTemplateAttachment")`), so an element that
//! omits one is not the shape a client reads; and `RenderMessageTemplate`'s label is a
//! `UuidOrArnOrEitherWithQualifier` (`smithy_query(query="operation",
//! operation="RenderMessageTemplate")`), so the same template is addressed by its bare id,
//! by its ARN, and by either of those carrying a version qualifier — the working copy for
//! `$LATEST`, a published snapshot for a version number or for `$ACTIVE_VERSION`.
//!
//! `tests/all/message_template_render_attachments.rs` pins what a render reports as the
//! working copy gains and loses attachments, and the `date-time` form of its instants;
//! this suite is the other half of the same projection, which that suite does not
//! exercise: every LABEL FORM reaching the copy it names (a version number and
//! `$ACTIVE_VERSION`, and an ARN carrying a qualifier), the presence of every `@required`
//! member of each element, that one template's attachments are never another's, and that
//! the locator a render hands a client is backed by the stored file rather than being a
//! string that resolves to nothing.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::message_template::content_for;
use super::message_template_attachment_deletion::{attachment_ids, delete_attachment, get_template, upload};
use super::message_template_versions::activate;
use super::resource_tags::{label, wisdom_arn, Emulator, Reply, REGION};

/// The channel subtype every template in this suite is created with; `content_for`
/// supplies content the modelled constraints of that subtype admit.
const CHANNEL_SUBTYPE: &str = "EMAIL";

/// The managed version qualifiers the read's own documentation names
/// (`smithy_query(query="doc",shape="GetMessageTemplate")`: "either an actual version
/// number or an Amazon Q Connect managed qualifier $ACTIVE_VERSION | $LATEST"). Written
/// out here rather than imported from the implementation.
const QUALIFIER_LATEST: &str = "$LATEST";
const QUALIFIER_ACTIVE: &str = "$ACTIVE_VERSION";

/// Every member of `MessageTemplateAttachment`, each of which the model declares
/// `@required` (`smithy_query(query="shape",shape="MessageTemplateAttachment")`), by the
/// JSON pointer it is read at inside one element.
const ATTACHMENT_MEMBERS: [&str; 6] = [
    "/attachmentId",
    "/contentDisposition",
    "/name",
    "/uploadedTime",
    "/url",
    "/urlExpiry",
];

/// The only `ContentDisposition` the model declares
/// (`smithy_query(query="shape",shape="ContentDisposition")`), which is therefore what
/// every projected attachment reports.
const CONTENT_DISPOSITION: &str = "ATTACHMENT";

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
                "channelSubtype": CHANNEL_SUBTYPE,
                "content": content_for(CHANNEL_SUBTYPE),
            })),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "case {name:?}: the message template these rows render must be created, got HTTP {} with \
         body {}",
        reply.status, reply.body
    );
    reply.string("/messageTemplate/messageTemplateId")
}

/// `CreateMessageTemplateVersion`, answering with the `versionNumber` the publish minted.
async fn publish(emulator: &Emulator, knowledge_base_id: &str, message_template_id: &str) -> i64 {
    let reply = emulator
        .send(
            "POST",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}/versions",
                label(knowledge_base_id),
                label(message_template_id)
            ),
            Some(json!({})),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "case \"publish\": the version these rows render must be published, got HTTP {} with body \
         {}",
        reply.status, reply.body
    );
    reply
        .field("/messageTemplate/versionNumber")
        .and_then(Value::as_i64)
        .unwrap_or_default()
}

/// `RenderMessageTemplate` on whatever label the row renders with. The `attributes` member
/// is `@required`, and an email template's content names no variable here, so the empty map
/// is what a client that fills nothing sends.
async fn render(emulator: &Emulator, knowledge_base_id: &str, message_template_label: &str) -> Reply {
    emulator
        .send(
            "POST",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}/render",
                label(knowledge_base_id),
                label(message_template_label)
            ),
            Some(json!({ "attributes": Map::new() })),
        )
        .await
}

/// The attachment elements a render reports, in the order it reports them. The empty list
/// for a render that reports no `attachments` member at all, which is how a template holding
/// none answers.
fn rendered_attachments(reply: &Reply) -> Vec<Value> {
    reply
        .field("/attachments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The `attachmentId`s of those elements, in the same order.
fn rendered_ids(reply: &Reply) -> Vec<String> {
    rendered_attachments(reply)
        .iter()
        .map(|attachment| text(attachment, "/attachmentId"))
        .collect()
}

/// One string member of an element, empty when the element does not carry it.
fn text(element: &Value, pointer: &str) -> String {
    element
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The ARN of one message template of this account, written out here rather than taken
/// from the handler's helper so the row pins the form the model's `ArnWithQualifier`
/// pattern describes.
fn template_arn(knowledge_base_id: &str, message_template_id: &str) -> String {
    wisdom_arn(
        "aws",
        REGION,
        engine::EMULATOR_ACCOUNT_ID,
        &format!("message-template/{knowledge_base_id}/{message_template_id}"),
    )
}

/// One attachment a row uploads: the name it is stored under and the file it carries.
struct Upload {
    name: &'static str,
    file: Vec<u8>,
}

/// The file set this suite draws from: every byte class an upload may carry (text, binary
/// with a zero byte in it, a single byte, several kilobytes), each under a name the
/// modelled `AttachmentFileName` pattern admits
/// (`smithy_query(query="shape",shape="AttachmentFileName")`: 1..256, ending in a file
/// extension).
fn files() -> Vec<Upload> {
    vec![
        Upload {
            name: "reach-notes.txt",
            file: b"the attachment body, as a client uploaded it".to_vec(),
        },
        Upload {
            name: "reach-logo.PNG",
            file: vec![0x89, 0x50, 0x4e, 0x47, 0x00, 0x01, 0xfa, 0xff],
        },
        Upload {
            name: "reach-one.b",
            file: b"1".to_vec(),
        },
        Upload {
            name: "reach-handbook.csv",
            file: (0..4096u32).map(|index| (index % 251) as u8).collect(),
        },
    ]
}

/// What one upload persisted, kept so a later render can be checked against it rather than
/// against a value written into the test.
struct Persisted {
    name: &'static str,
    file: Vec<u8>,
    /// The element the upload itself answered with.
    element: Value,
}

/// Upload one file and keep what it persisted, asserting the upload itself succeeded.
async fn upload_file(
    emulator: &Emulator,
    knowledge_base_id: &str,
    message_template_id: &str,
    upload_case: &Upload,
) -> (String, Persisted) {
    let uploaded = upload(
        emulator,
        knowledge_base_id,
        message_template_id,
        upload_case.name,
        &upload_case.file,
    )
    .await;
    assert_eq!(
        uploaded.status, 200,
        "case {:?}: the attachment this suite renders must be uploaded, got HTTP {} with body {}",
        upload_case.name, uploaded.status, uploaded.body
    );
    let element = uploaded
        .field("/attachment")
        .cloned()
        .unwrap_or(Value::Null);
    (
        uploaded.string("/attachment/attachmentId"),
        Persisted {
            name: upload_case.name,
            file: upload_case.file.clone(),
            element,
        },
    )
}

/// Assert that one projected element carries every `@required` member of the shape, agrees
/// with what the upload that minted it reported, and hands out a locator that downloads the
/// file itself.
async fn assert_element_is_backed_by_the_stored_file(
    emulator: &Emulator,
    case: &str,
    element: &Value,
    persisted: &Persisted,
) {
    for pointer in ATTACHMENT_MEMBERS {
        assert!(
            element
                .pointer(pointer)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty()),
            "case {case:?}: a rendered attachment carries {pointer} as a non-empty string, got \
             {:?} in {element:?}",
            element.pointer(pointer)
        );
    }
    assert_eq!(
        text(element, "/name"),
        persisted.name,
        "case {case:?}: expected the uploaded file name {:?}, got {:?}",
        persisted.name,
        text(element, "/name")
    );
    assert_eq!(
        text(element, "/contentDisposition"),
        CONTENT_DISPOSITION,
        "case {case:?}: expected contentDisposition {CONTENT_DISPOSITION:?}, got {:?}",
        text(element, "/contentDisposition")
    );
    assert_eq!(
        element, &persisted.element,
        "case {case:?}: the render must project the attachment exactly as its upload reported it \
         ({:?}), got {element:?}",
        persisted.element
    );

    // The locator is backed by the file: a client that follows it downloads the bytes the
    // upload carried, so the member a render reports is not a string that resolves to
    // nothing.
    let url = text(element, "/url");
    let followed = emulator.follow("GET", &url).await;
    assert_eq!(
        followed.status, 200,
        "case {case:?}: the rendered url {url:?} must be followable, got HTTP {}",
        followed.status
    );
    assert_eq!(
        followed.bytes,
        persisted.file,
        "case {case:?}: the rendered url {url:?} must serve the uploaded file, got {} bytes",
        followed.bytes.len()
    );
}

/// The rule: every label form a render accepts reaches the copy of the template it names —
/// the working copy for a bare id, an ARN or the `$LATEST` qualifier, the published
/// snapshot for a version number or for `$ACTIVE_VERSION` — and reports the attachments
/// THAT copy holds, in upload order, each element fully populated and each `url` serving
/// the stored file.
#[tokio::test]
async fn every_label_form_renders_the_attachments_of_the_copy_it_addresses() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "render-attachment-reach").await;
    let message_template_id = message_template(&emulator, &knowledge_base_id, "reach").await;
    let uploads = files();

    // Three files onto the working copy, then a version published from it: the snapshot
    // carries exactly the attachments the working copy held at that moment.
    let mut ids: Vec<String> = Vec::new();
    let mut persisted: BTreeMap<String, Persisted> = BTreeMap::new();
    for upload_case in uploads.iter().take(3) {
        let (id, record) = upload_file(
            &emulator,
            &knowledge_base_id,
            &message_template_id,
            upload_case,
        )
        .await;
        ids.push(id.clone());
        persisted.insert(id, record);
    }
    let published: Vec<String> = ids.clone();

    let version = publish(&emulator, &knowledge_base_id, &message_template_id).await;
    let activated = activate(&emulator, &knowledge_base_id, &message_template_id, version).await;
    assert_eq!(
        activated.status, 200,
        "case \"activate\": the version the {QUALIFIER_ACTIVE} rows name must be activated, got \
         HTTP {} with body {}",
        activated.status, activated.body
    );

    // The working copy then moves on from the snapshot in BOTH directions: one more file is
    // uploaded onto it, and one of the published files is removed from it.
    let (fourth, fourth_record) = upload_file(
        &emulator,
        &knowledge_base_id,
        &message_template_id,
        &uploads[3],
    )
    .await;
    persisted.insert(fourth.clone(), fourth_record);
    let removed = ids[1].clone();
    let deleted = delete_attachment(&emulator, &knowledge_base_id, &message_template_id, &removed).await;
    // The delete's own success status is pinned by its own suite; what matters here is that
    // it succeeded, so the two copies now hold different attachment sets.
    assert!(
        (200..300).contains(&deleted.status),
        "case \"delete\": the delete of {removed:?} must succeed, got HTTP {} with body {}",
        deleted.status, deleted.body
    );
    let working_copy: Vec<String> = vec![ids[0].clone(), ids[2].clone(), fourth.clone()];

    let arn = template_arn(&knowledge_base_id, &message_template_id);
    // The rows differ along what the rule generalises over: the form of the label (a bare
    // id, an ARN, either carrying a managed qualifier, either carrying a version number)
    // and therefore which copy of the template it addresses.
    let rows: Vec<(&str, String, &Vec<String>)> = vec![
        ("the bare id", message_template_id.clone(), &working_copy),
        (
            "the bare id with the $LATEST qualifier",
            format!("{message_template_id}:{QUALIFIER_LATEST}"),
            &working_copy,
        ),
        ("the ARN", arn.clone(), &working_copy),
        (
            "the ARN with the $LATEST qualifier",
            format!("{arn}:{QUALIFIER_LATEST}"),
            &working_copy,
        ),
        (
            "the bare id with the version number",
            format!("{message_template_id}:{version}"),
            &published,
        ),
        (
            "the ARN with the version number",
            format!("{arn}:{version}"),
            &published,
        ),
        (
            "the bare id with the $ACTIVE_VERSION qualifier",
            format!("{message_template_id}:{QUALIFIER_ACTIVE}"),
            &published,
        ),
        (
            "the ARN with the $ACTIVE_VERSION qualifier",
            format!("{arn}:{QUALIFIER_ACTIVE}"),
            &published,
        ),
    ];

    for (case, render_label, expected) in rows {
        let rendered = render(&emulator, &knowledge_base_id, &render_label).await;
        assert_eq!(
            rendered.status, 200,
            "case {case:?}: the render must answer HTTP 200, got HTTP {} with body {}",
            rendered.status, rendered.body
        );
        assert_eq!(
            &rendered_ids(&rendered),
            expected,
            "case {case:?}: the render must report {expected:?} in upload order, got {:?}",
            rendered.field("/attachments")
        );
        for element in rendered_attachments(&rendered) {
            let id = text(&element, "/attachmentId");
            let record = persisted
                .get(&id)
                .unwrap_or_else(|| panic!("case {case:?}: the render reported an unknown {id:?}"));
            assert_element_is_backed_by_the_stored_file(&emulator, case, &element, record).await;
        }

        // One projection, one answer: the read of the same copy reports the same
        // attachments the render does.
        let read = get_template(&emulator, &knowledge_base_id, &render_label).await;
        assert_eq!(
            &attachment_ids(&read),
            expected,
            "case {case:?}: the read of the same copy must report {expected:?}, got {:?}",
            read.field("/messageTemplate/attachments")
        );
    }
}

/// The rule, on the other axis it generalises over: an attachment belongs to ONE message
/// template, so a render reports the attachments of the template its route addresses and
/// never another's — not another template of the same knowledge base, and not a template of
/// another knowledge base, even when the file was uploaded under the same name.
#[tokio::test]
async fn a_render_reports_only_the_attachments_of_the_template_it_addresses() {
    let emulator = Emulator::new();
    let first_knowledge_base = knowledge_base(&emulator, "render-reach-isolation-one").await;
    let second_knowledge_base = knowledge_base(&emulator, "render-reach-isolation-two").await;

    // One file name, three templates, three different files: the rows differ along where
    // the template sits (two in one knowledge base, one in another).
    let shared_name = "shared.txt";
    let mut rows: Vec<(&str, String, String, String, Persisted)> = Vec::new();
    for (case, knowledge_base_id, body) in [
        ("the first template", first_knowledge_base.clone(), b"one".to_vec()),
        ("the second template", first_knowledge_base.clone(), b"two".to_vec()),
        (
            "a template of another knowledge base",
            second_knowledge_base.clone(),
            b"three".to_vec(),
        ),
    ] {
        let message_template_id = message_template(&emulator, &knowledge_base_id, case).await;
        let (id, record) = upload_file(
            &emulator,
            &knowledge_base_id,
            &message_template_id,
            &Upload {
                name: shared_name,
                file: body,
            },
        )
        .await;
        rows.push((case, knowledge_base_id, message_template_id, id, record));
    }

    for (case, knowledge_base_id, message_template_id, id, record) in &rows {
        let rendered = render(&emulator, knowledge_base_id, message_template_id).await;
        assert_eq!(
            rendered.status, 200,
            "case {case:?}: the render must answer HTTP 200, got HTTP {} with body {}",
            rendered.status, rendered.body
        );
        assert_eq!(
            rendered_ids(&rendered),
            vec![id.clone()],
            "case {case:?}: the render must report this template's attachment alone, got {:?}",
            rendered.field("/attachments")
        );
        let element = rendered_attachments(&rendered)
            .into_iter()
            .next()
            .unwrap_or(Value::Null);
        assert_element_is_backed_by_the_stored_file(&emulator, case, &element, record).await;
    }

    // And a delete on one template leaves the others' renders exactly as they were: the
    // attachment the delete removed was never theirs.
    let (_, target_knowledge_base, target_template, target_id, _) = &rows[1];
    let deleted = delete_attachment(&emulator, target_knowledge_base, target_template, target_id).await;
    assert!(
        (200..300).contains(&deleted.status),
        "case \"delete the second template's attachment\": the delete must succeed, got HTTP {} \
         with body {}",
        deleted.status, deleted.body
    );
    for (case, knowledge_base_id, message_template_id, id, _) in &rows {
        let rendered = render(&emulator, knowledge_base_id, message_template_id).await;
        let expected: Vec<String> = if id == target_id {
            Vec::new()
        } else {
            vec![id.clone()]
        };
        assert_eq!(
            rendered_ids(&rendered),
            expected,
            "case {case:?}: after the delete the render must report {expected:?}, got {:?}",
            rendered.field("/attachments")
        );
    }
}
