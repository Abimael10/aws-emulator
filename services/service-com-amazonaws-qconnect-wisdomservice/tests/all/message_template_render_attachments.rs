// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ATTACHMENT projection of a message template, driven over HTTP
//! through the same router the emulator serves with: only the HTTP status and the JSON body
//! are inspected, so a response that is not computed from persisted state cannot pass.
//!
//! The rule under test, asserted over a TABLE of inputs rather than one recorded request:
//! every operation that projects a message-template attachment reports the attachment set
//! the ADDRESSED copy of the template currently holds — the working copy for the `$LATEST`
//! qualifier, a version's snapshot for a published one — in upload order, and each element
//! carries both of the shape's instants in the `date-time` form the model declares on them.
//!
//! That rule has two wire-visible halves, and the rows below pin both on the render path,
//! which is how a client discovers the attachments of a message it is about to send:
//!
//! * `uploadedTime` and `urlExpiry` both declare `@timestampFormat("date-time")`
//!   (`smithy_query(query="shape",shape="MessageTemplateAttachment")`), so each is an
//!   RFC 3339 / ISO 8601 STRING on the wire, never the epoch-seconds number the
//!   `restJson1` protocol defaults to, and the same instant is reported by the upload that
//!   minted it and by every later projection of it;
//! * an attachment the working copy no longer holds is reported by no later render of
//!   `$LATEST`: `DeleteMessageTemplateAttachment` removes the file "from the Amazon Q in
//!   Connect message template that is referenced by $LATEST qualifier"
//!   (`smithy_query(query="doc",shape="DeleteMessageTemplateAttachment")`), while
//!   "attachments on available message template versions will remain unchanged", so a
//!   version-addressed render still reports the set its snapshot was published with.
//!
//! `tests/all/message_template_wire_form.rs` pins the same `date-time` rule across the
//! whole message-template family and `tests/all/message_template_attachment_deletion.rs`
//! pins what a delete does to the template READS; this suite is the RENDER projection of
//! both, which neither exercises.

use serde_json::{json, Map, Value};

use super::message_template::content_for;
use super::message_template_attachment_deletion::{delete_attachment, get_template, upload};
use super::resource_tags::{label, wisdom_arn, Emulator, Reply, REGION};

/// The channel subtype every template in this suite is created with: attachments are
/// uploaded to an email template in the recorded flow, and `content_for` supplies content
/// the modelled constraints of that subtype admit.
const CHANNEL_SUBTYPE: &str = "EMAIL";

/// The managed version qualifiers the read's own documentation names
/// (`smithy_query(query="doc",shape="GetMessageTemplate")`: "either an actual version
/// number or an Amazon Q Connect managed qualifier $ACTIVE_VERSION | $LATEST"). Written out
/// here rather than imported from the implementation.
const QUALIFIER_LATEST: &str = "$LATEST";

/// The two members of `MessageTemplateAttachment` whose model declares
/// `@timestampFormat("date-time")`
/// (`smithy_query(query="shape",shape="MessageTemplateAttachment")`), by the JSON pointer
/// they are read at inside one element.
const ATTACHMENT_INSTANT_POINTERS: [&str; 2] = ["/uploadedTime", "/urlExpiry"];

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

/// `RenderMessageTemplate` on whatever label the row renders with. The `attributes` member
/// is `@required`, and an email template's content names no variable here, so the empty map
/// is what a client that fills nothing sends.
async fn render(
    emulator: &Emulator,
    knowledge_base_id: &str,
    message_template_label: &str,
) -> Reply {
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
        .map(|attachment| {
            attachment
                .pointer("/attachmentId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// One instant an element reports, as the seconds since the epoch it names — read back
/// through the `date-time` parser, so a member carrying anything but an RFC 3339 string
/// (an epoch-seconds NUMBER above all) answers `None`.
fn date_time_secs(element: &Value, pointer: &str) -> Option<f64> {
    let text = element.pointer(pointer)?.as_str()?;
    ::aws_smithy_types::DateTime::from_str(text, ::aws_smithy_types::date_time::Format::DateTime)
        .ok()
        .map(|parsed| parsed.as_secs_f64())
}

/// Assert that one projected attachment carries both of its modelled instants in the
/// declared `date-time` form, and that the expiry of its download URL is after the upload
/// it was minted by.
fn assert_instants_are_date_time_strings(case: &str, where_from: &str, element: &Value) {
    for pointer in ATTACHMENT_INSTANT_POINTERS {
        let reported = element.pointer(pointer);
        assert!(
            reported.and_then(Value::as_str).is_some(),
            "case {case:?}: {where_from} must report {pointer} as a date-time string, got \
             {reported:?}",
        );
        assert!(
            date_time_secs(element, pointer).is_some(),
            "case {case:?}: {where_from}'s {pointer} must parse as an RFC 3339 date-time, got \
             {reported:?}",
        );
        let text = reported.and_then(Value::as_str).unwrap_or_default();
        assert!(
            text.ends_with('Z'),
            "case {case:?}: {where_from}'s {pointer} must name the instant in UTC, got {text:?}",
        );
    }
    let uploaded = date_time_secs(element, "/uploadedTime").unwrap_or_default();
    let expiry = date_time_secs(element, "/urlExpiry").unwrap_or_default();
    assert!(
        expiry > uploaded,
        "case {case:?}: {where_from}'s urlExpiry ({expiry}) must be after its uploadedTime \
         ({uploaded})",
    );
}

/// One attachment a row uploads: the name it is stored under and the file it carries.
struct Upload {
    name: &'static str,
    file: Vec<u8>,
}

/// The file set the multi-attachment rows draw from: every byte class an upload may carry
/// (text, binary, a single byte, several kilobytes), each under a name the modelled
/// `AttachmentFileName` pattern admits.
fn files() -> Vec<Upload> {
    vec![
        Upload {
            name: "notes.txt",
            file: b"the attachment body, as a client uploaded it".to_vec(),
        },
        Upload {
            name: "logo.PNG",
            file: vec![0x89, 0x50, 0x4e, 0x47, 0x00, 0x01, 0xfa, 0xff],
        },
        Upload {
            name: "x.y",
            file: b"x".to_vec(),
        },
        Upload {
            name: "handbook.csv",
            file: (0..4096u32).map(|index| (index % 251) as u8).collect(),
        },
    ]
}

/// One row: the files it uploads, the attachments it then deletes (in the order it deletes
/// them) and the label form it renders with.
struct RenderCase {
    case: &'static str,
    uploads: Vec<Upload>,
    /// The indexes, in upload order, of the attachments this row deletes one after another.
    deletes: Vec<usize>,
    /// The version qualifier the render's label carries, if any.
    qualifier: Option<&'static str>,
    /// Whether the render addresses the template by its ARN rather than by its bare id.
    by_arn: bool,
}

/// The rows differ along what the rule generalises over: how many attachments the template
/// holds, which of them are deleted (none, the only one, the first, one in the middle, the
/// last, several, all of them) and the form of the label the render addresses the working
/// copy by (a bare id, an explicit `$LATEST` qualifier, the template's ARN).
fn render_cases() -> Vec<RenderCase> {
    vec![
        RenderCase {
            case: "one attachment, none deleted",
            uploads: files().into_iter().take(1).collect(),
            deletes: vec![],
            qualifier: None,
            by_arn: false,
        },
        RenderCase {
            case: "four attachments, none deleted",
            uploads: files(),
            deletes: vec![],
            qualifier: Some(QUALIFIER_LATEST),
            by_arn: false,
        },
        RenderCase {
            case: "the only attachment deleted",
            uploads: files().into_iter().take(1).collect(),
            deletes: vec![0],
            qualifier: None,
            by_arn: false,
        },
        RenderCase {
            case: "the first of three deleted",
            uploads: files().into_iter().take(3).collect(),
            deletes: vec![0],
            qualifier: None,
            by_arn: false,
        },
        RenderCase {
            case: "one in the middle of three deleted",
            uploads: files().into_iter().take(3).collect(),
            deletes: vec![1],
            qualifier: Some(QUALIFIER_LATEST),
            by_arn: false,
        },
        RenderCase {
            case: "the last of three deleted, rendered by the template's ARN",
            uploads: files().into_iter().take(3).collect(),
            deletes: vec![2],
            qualifier: None,
            by_arn: true,
        },
        RenderCase {
            case: "two of four deleted, one after the other",
            uploads: files(),
            deletes: vec![0, 3],
            qualifier: None,
            by_arn: false,
        },
        RenderCase {
            case: "every attachment of two deleted",
            uploads: files().into_iter().take(2).collect(),
            deletes: vec![0, 1],
            qualifier: None,
            by_arn: false,
        },
    ]
}

/// The rule on the render path: a render reports the attachments the working copy currently
/// holds, in upload order, each projected exactly as the upload that minted it reported it —
/// both instants in the modelled `date-time` form — and reports no element for an attachment
/// a delete removed.
#[tokio::test]
async fn a_render_reports_the_attachments_the_working_copy_holds_as_date_time_strings() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "render-attachments").await;

    for row in render_cases() {
        let RenderCase {
            case,
            uploads,
            deletes,
            qualifier,
            by_arn,
        } = row;
        let message_template_id = message_template(&emulator, &knowledge_base_id, case).await;
        let addressed = if by_arn {
            wisdom_arn(
                "aws",
                REGION,
                engine::EMULATOR_ACCOUNT_ID,
                &format!("message-template/{knowledge_base_id}/{message_template_id}"),
            )
        } else {
            message_template_id.clone()
        };
        let render_label = match qualifier {
            Some(qualifier) => format!("{addressed}:{qualifier}"),
            None => addressed.clone(),
        };

        // A template holding no attachment reports no member for them at all, so the
        // element the rows below assert on cannot come from an empty default.
        let bare = render(&emulator, &knowledge_base_id, &render_label).await;
        assert_eq!(
            bare.status, 200,
            "case {case:?}: the render of a template holding no attachment must answer HTTP 200, \
             got HTTP {} with body {}",
            bare.status, bare.body
        );
        assert!(
            bare.field("/attachments").is_none(),
            "case {case:?}: a template holding no attachment reports no attachments member, got \
             {:?}",
            bare.field("/attachments")
        );

        // Every file this row uploads, keeping the element the UPLOAD reported for it: the
        // same attachment projected by a later render must be reported identically.
        let mut ids: Vec<String> = Vec::new();
        let mut uploaded_elements: Vec<Value> = Vec::new();
        for upload_case in &uploads {
            let uploaded = upload(
                &emulator,
                &knowledge_base_id,
                &message_template_id,
                upload_case.name,
                &upload_case.file,
            )
            .await;
            assert_eq!(
                uploaded.status, 200,
                "case {case:?}: the attachment {:?} this row renders must be uploaded, got HTTP {} \
                 with body {}",
                upload_case.name, uploaded.status, uploaded.body
            );
            let element = uploaded
                .field("/attachment")
                .cloned()
                .unwrap_or(Value::Null);
            assert_instants_are_date_time_strings(
                case,
                &format!("the upload of {:?}", upload_case.name),
                &element,
            );
            ids.push(uploaded.string("/attachment/attachmentId"));
            uploaded_elements.push(element);
        }

        // The render before any delete: every uploaded attachment, in upload order, each
        // element the one its upload answered with.
        let mut live: Vec<usize> = (0..uploads.len()).collect();
        let rendered = render(&emulator, &knowledge_base_id, &render_label).await;
        assert_rendered(
            case,
            "before any delete",
            &rendered,
            &live,
            &ids,
            &uploaded_elements,
        );

        // A render REPORTS: rendering again changes nothing a later caller sees.
        let again = render(&emulator, &knowledge_base_id, &render_label).await;
        assert_eq!(
            rendered_attachments(&again),
            rendered_attachments(&rendered),
            "case {case:?}: two renders of one template must report the same attachments",
        );

        for target in deletes {
            let deleted = delete_attachment(
                &emulator,
                &knowledge_base_id,
                &message_template_id,
                &ids[target],
            )
            .await;
            assert_eq!(
                deleted.status, 204,
                "case {case:?}: expected HTTP 204 for the delete of {:?}, got HTTP {} with body {}",
                uploads[target].name, deleted.status, deleted.body
            );
            live.retain(|index| *index != target);

            let after = render(&emulator, &knowledge_base_id, &render_label).await;
            assert!(
                !rendered_ids(&after).contains(&ids[target]),
                "case {case:?}: the render after the delete of {:?} must report no element for \
                 {:?}, got {:?}",
                uploads[target].name,
                ids[target],
                after.field("/attachments")
            );
            assert_rendered(
                case,
                &format!("after the delete of {:?}", uploads[target].name),
                &after,
                &live,
                &ids,
                &uploaded_elements,
            );

            // The read of the template agrees with the render: one projection, one answer.
            let read = get_template(&emulator, &knowledge_base_id, &message_template_id).await;
            let expected: Vec<String> = live.iter().map(|index| ids[*index].clone()).collect();
            assert_eq!(
                super::message_template_attachment_deletion::attachment_ids(&read),
                expected,
                "case {case:?}: the read after the delete of {:?} must report {expected:?}, got \
                 {:?}",
                uploads[target].name,
                read.field("/messageTemplate/attachments")
            );
        }
    }
}

/// The attachments a render must report: exactly the elements of `live`, in upload order,
/// each identical to the one the upload that minted it answered with, and reported as no
/// member at all when nothing is left.
fn assert_rendered(
    case: &str,
    when: &str,
    reply: &Reply,
    live: &[usize],
    ids: &[String],
    uploaded_elements: &[Value],
) {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the render {when} must answer HTTP 200, got HTTP {} with body {}",
        reply.status, reply.body
    );
    if live.is_empty() {
        assert!(
            reply.field("/attachments").is_none(),
            "case {case:?}: the render {when} holds no attachment, so it reports no attachments \
             member, got {:?}",
            reply.field("/attachments")
        );
        return;
    }
    let expected: Vec<String> = live.iter().map(|index| ids[*index].clone()).collect();
    assert_eq!(
        rendered_ids(reply),
        expected,
        "case {case:?}: the render {when} must report {expected:?} in upload order, got {:?}",
        reply.field("/attachments")
    );
    for (element, index) in rendered_attachments(reply).iter().zip(live) {
        assert_instants_are_date_time_strings(case, &format!("the render {when}"), element);
        assert_eq!(
            element, &uploaded_elements[*index],
            "case {case:?}: the render {when} must project the attachment exactly as its upload \
             reported it ({:?}), got {element:?}",
            uploaded_elements[*index]
        );
    }
}

/// The rule on a version-addressed render: the delete reaches the working copy ALONE, so a
/// render of a published version reports the attachment set its snapshot was published with,
/// with the same `date-time` instants, however much the working copy has since lost.
#[tokio::test]
async fn a_render_of_a_published_version_keeps_the_attachments_it_was_published_with() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "render-attachment-versions").await;

    // The rows differ along which published attachment the working-copy delete removes.
    for (case, target) in [
        ("the first of three", 0usize),
        ("one in the middle of three", 1usize),
        ("the last of three", 2usize),
    ] {
        let message_template_id = message_template(&emulator, &knowledge_base_id, case).await;
        let uploads: Vec<Upload> = files().into_iter().take(3).collect();
        let mut ids: Vec<String> = Vec::new();
        for upload_case in &uploads {
            let uploaded = upload(
                &emulator,
                &knowledge_base_id,
                &message_template_id,
                upload_case.name,
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
        }

        let published = publish(&emulator, &knowledge_base_id, &message_template_id).await;
        assert_eq!(
            published.status, 200,
            "case {case:?}: the version this row renders must be published, got HTTP {} with body \
             {}",
            published.status, published.body
        );
        let version = published
            .field("/messageTemplate/versionNumber")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let version_label = format!("{message_template_id}:{version}");

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

        // The working copy's render lost exactly that attachment...
        let working_copy = render(
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
            rendered_ids(&working_copy),
            expected,
            "case {case:?}: the render of the working copy must report {expected:?} after the \
             delete, got {:?}",
            working_copy.field("/attachments")
        );

        // ...while the version's render still reports every attachment it was published
        // with, each with its own date-time instants.
        let version_render = render(&emulator, &knowledge_base_id, &version_label).await;
        assert_eq!(
            rendered_ids(&version_render),
            ids,
            "case {case:?}: the render of version {version} must report {ids:?}, got {:?}",
            version_render.field("/attachments")
        );
        for element in rendered_attachments(&version_render) {
            assert_instants_are_date_time_strings(
                case,
                &format!("the render of version {version}"),
                &element,
            );
        }
    }
}
