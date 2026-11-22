// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DeleteMessageTemplate` and for what a READ of a message template
//! this knowledge base no longer holds answers.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than one request:
//!
//! * a delete whose label carries NO version qualifier forgets the template "and all
//!   available versions", so the knowledge base holds it no more: it is gone whatever
//!   channel subtype it had, whichever label form addressed it, and however many versions
//!   it had published, and the `name` it held is free for another create;
//! * a delete whose label carries a version qualifier forgets THAT published version and
//!   leaves the working copy and the other versions exactly as they were;
//! * reading a message template is asking for one named resource's content, and the
//!   service decides whether the caller may have it before reporting any of it: a label
//!   pair that names no message template this knowledge base holds — never created,
//!   deleted, another knowledge base's, or an ARN of another account or partition — is
//!   refused with the modelled `UnauthorizedException` (HTTP 401) carrying the service's
//!   fixed sentence and nothing from the request, rather than served or reported as a
//!   missing resource;
//! * a refused delete changes nothing: the version it could not find is the only thing it
//!   reports, and the template and its other versions survive.
//!
//! Every assertion goes over the wire through the same in-process router the production
//! emulator mounts, so only the HTTP status, the `x-amzn-errortype` header and the JSON
//! body are inspected.

use serde_json::{json, Value};

use super::message_template::{content_for, CHANNEL_SUBTYPES};
use super::resource_tags::{label, wisdom_arn, Emulator, Reply, ABSENT_UUID, OTHER_ACCOUNT, REGION};

/// What a READ of a message template this knowledge base does not hold answers: the
/// modelled `UnauthorizedException` (`@httpError(401)`, whose one member is `message`),
/// carrying this service's fixed sentence for the condition. Written out here rather than
/// imported from the implementation, so the assertions pin the answer a client sees.
const UNAUTHORIZED: (u16, &str, &str) = (401, "UnauthorizedException", "Unauthorized");

/// What a request addressed to a message template, or to a version of one, that this
/// account does not hold answers on the routes that report the resource: the modelled
/// `ResourceNotFoundException` (`@httpError(404)`) and the sentence for the resource that
/// is missing.
const TEMPLATE_NOT_FOUND: (u16, &str, &str) = (
    404,
    "ResourceNotFoundException",
    "MessageTemplate does not exist.",
);
const VERSION_NOT_FOUND: (u16, &str, &str) = (
    404,
    "ResourceNotFoundException",
    "Message template version does not exist.",
);
const KNOWLEDGE_BASE_NOT_FOUND: (u16, &str, &str) = (
    404,
    "ResourceNotFoundException",
    "Knowledge Base does not exist.",
);

/// The status the model binds to `DeleteMessageTemplate`'s success
/// (`smithy_query(query="operation",operation="DeleteMessageTemplate")`: `http` code 204),
/// which carries no body.
const DELETED_STATUS: u16 = 204;

/// The version qualifiers the read's own documentation names
/// (`smithy_query(query="doc",shape="GetMessageTemplate")`: "either an actual version
/// number or an Amazon Q Connect managed qualifier $ACTIVE_VERSION | $LATEST"), and one
/// that is neither.
const QUALIFIER_LATEST: &str = "$LATEST";
const QUALIFIER_ACTIVE: &str = "$ACTIVE_VERSION";
const QUALIFIER_UNKNOWN: &str = "$PREVIOUS";

/// `CreateKnowledgeBase` of the type that holds message templates.
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

/// `CreateMessageTemplate`, answering the whole reply so a row can read the id and the ARN
/// the create minted.
async fn create(
    emulator: &Emulator,
    knowledge_base_id: &str,
    name: &str,
    channel_subtype: &str,
) -> Reply {
    emulator
        .send(
            "POST",
            &format!(
                "/knowledgeBases/{}/messageTemplates",
                label(knowledge_base_id)
            ),
            Some(json!({
                "name": name,
                "channelSubtype": channel_subtype,
                "content": content_for(channel_subtype),
            })),
        )
        .await
}

/// `GetMessageTemplate`, whose label may carry a version qualifier.
async fn read(emulator: &Emulator, knowledge_base: &str, message_template: &str) -> Reply {
    emulator
        .send(
            "GET",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}",
                label(knowledge_base),
                label(message_template)
            ),
            None,
        )
        .await
}

/// `DeleteMessageTemplate`, whose label may carry the version qualifier that selects one
/// version to delete.
async fn delete(emulator: &Emulator, knowledge_base: &str, message_template: &str) -> Reply {
    emulator
        .send(
            "DELETE",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}",
                label(knowledge_base),
                label(message_template)
            ),
            None,
        )
        .await
}

/// `CreateMessageTemplateVersion`, answering the `versionNumber` it published.
async fn publish(emulator: &Emulator, knowledge_base_id: &str, message_template: &str) -> i64 {
    let reply = emulator
        .send(
            "POST",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}/versions",
                label(knowledge_base_id),
                label(message_template)
            ),
            Some(json!({})),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "setup: a version this suite deletes must be published, got HTTP {} with body {}",
        reply.status, reply.body
    );
    reply
        .field("/messageTemplate/versionNumber")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("no versionNumber in {}", reply.body))
}

/// `UpdateMessageTemplate` with whatever members the row changes.
async fn update(
    emulator: &Emulator,
    knowledge_base_id: &str,
    message_template: &str,
    body: Value,
) -> Reply {
    emulator
        .send(
            "POST",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}",
                label(knowledge_base_id),
                label(message_template)
            ),
            Some(body),
        )
        .await
}

/// Publish `count` versions of one template, each from a working copy the previous version
/// does not hold — a publish of unchanged content answers with the version that already
/// holds it — by changing `language`, one of the four things the content checksum is
/// calculated over. Answers the version numbers that were published.
async fn publish_versions(
    emulator: &Emulator,
    knowledge_base_id: &str,
    message_template: &str,
    count: usize,
) -> Vec<i64> {
    /// Language codes the member documents
    /// (`smithy_query(query="doc",shape="CreateMessageTemplateRequest",member="language")`).
    const LANGUAGES: [&str; 4] = ["en_US", "fr_FR", "de_DE", "es_ES"];

    let mut published = Vec::new();
    for round in 0..count {
        let changed = update(
            emulator,
            knowledge_base_id,
            message_template,
            json!({ "language": LANGUAGES[round % LANGUAGES.len()] }),
        )
        .await;
        assert_eq!(
            changed.status, 200,
            "setup: the working copy must change before it is published again, got HTTP {} with \
             body {}",
            changed.status, changed.body
        );
        published.push(publish(emulator, knowledge_base_id, message_template).await);
    }
    published
}

/// `ActivateMessageTemplate` for one `versionNumber`.
async fn activate(
    emulator: &Emulator,
    knowledge_base_id: &str,
    message_template: &str,
    version_number: i64,
) -> Reply {
    emulator
        .send(
            "POST",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}/activate",
                label(knowledge_base_id),
                label(message_template)
            ),
            Some(json!({ "versionNumber": version_number })),
        )
        .await
}

/// `ListMessageTemplateVersions` for one template.
async fn list_versions(
    emulator: &Emulator,
    knowledge_base_id: &str,
    message_template: &str,
) -> Reply {
    emulator
        .send(
            "GET",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}/versions",
                label(knowledge_base_id),
                label(message_template)
            ),
            None,
        )
        .await
}

/// The `versionNumber`s one listing reports, in the order it reported them.
fn listed_versions(reply: &Reply) -> Vec<i64> {
    reply
        .field("/messageTemplateVersionSummaries")
        .and_then(Value::as_array)
        .map(|summaries| {
            summaries
                .iter()
                .filter_map(|summary| summary.get("versionNumber").and_then(Value::as_i64))
                .collect()
        })
        .unwrap_or_default()
}

/// Assert one reply is exactly the refusal `expected` names, and that it reports nothing
/// the request carried.
fn assert_refused(case: &str, reply: &Reply, expected: (u16, &str, &str), addressed: &str) {
    let (status, error_type, message) = expected;
    assert_eq!(
        reply.status, status,
        "case {case:?}: expected HTTP {status}, got HTTP {} with body {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_type, error_type,
        "case {case:?}: expected error type {error_type}, got {:?}",
        reply.error_type
    );
    assert_eq!(
        reply.string("/message"),
        message,
        "case {case:?}: expected the fixed sentence {message:?}, got {:?}",
        reply.string("/message")
    );
    if status == UNAUTHORIZED.0 {
        assert!(
            !reply.body.to_string().contains(addressed),
            "case {case:?}: an authorization refusal reports nothing the request carried, got {}",
            reply.body
        );
    }
}

/// A delete that names no version forgets the template and every version of it, and every
/// later read of it is refused — whatever channel subtype the template had, whichever
/// label form the delete addressed it by, and however many versions it had published.
#[tokio::test]
async fn a_delete_forgets_the_template_and_every_later_read_of_it_is_refused() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "deletes").await;

    // The rows differ along the three things the rule generalises over: the channel
    // subtype, the label form the delete addresses the template by, and how many versions
    // the template had published (none, and more than one).
    let mut row = 0;
    for (channel_subtype, _) in CHANNEL_SUBTYPES {
        for by_arn in [false, true] {
            for versions in [0, 2] {
                row += 1;
                let case = format!("{channel_subtype}/by_arn={by_arn}/versions={versions}");
                let name = format!("deleted {row}");
                let created = create(&emulator, &knowledge_base_id, &name, channel_subtype).await;
                assert_eq!(
                    created.status, 200,
                    "case {case:?}: the template this row deletes must be created, got HTTP {} \
                     with body {}",
                    created.status, created.body
                );
                let message_template_id = created.string("/messageTemplate/messageTemplateId");
                let message_template_arn = created.string("/messageTemplate/messageTemplateArn");
                publish_versions(&emulator, &knowledge_base_id, &message_template_id, versions)
                    .await;

                // A second template of the same knowledge base, so the delete can be shown
                // to take exactly one.
                let survivor_name = format!("survivor {row}");
                let survivor = create(
                    &emulator,
                    &knowledge_base_id,
                    &survivor_name,
                    channel_subtype,
                )
                .await;
                let survivor_id = survivor.string("/messageTemplate/messageTemplateId");

                // The control: the template is readable before the delete.
                let before = read(&emulator, &knowledge_base_id, &message_template_id).await;
                assert_eq!(
                    before.status, 200,
                    "case {case:?}: expected the template to be readable before the delete, got \
                     HTTP {} with body {}",
                    before.status, before.body
                );

                let addressed = if by_arn {
                    message_template_arn.clone()
                } else {
                    message_template_id.clone()
                };
                let deleted = delete(&emulator, &knowledge_base_id, &addressed).await;
                assert_eq!(
                    deleted.status, DELETED_STATUS,
                    "case {case:?}: expected HTTP {DELETED_STATUS} from the delete, got HTTP {} \
                     with body {}",
                    deleted.status, deleted.body
                );
                assert_eq!(
                    deleted.body.as_object().map_or(0, serde_json::Map::len),
                    0,
                    "case {case:?}: the modelled output has no member, so the delete answers no \
                     body, got {}",
                    deleted.body
                );

                // Every label form and every qualifier the read admits now names nothing
                // this knowledge base holds, so each is refused alike.
                let mut labels = vec![
                    ("the id".to_string(), message_template_id.clone()),
                    ("the ARN".to_string(), message_template_arn.clone()),
                    (
                        "the working copy by qualifier".to_string(),
                        format!("{message_template_id}:{QUALIFIER_LATEST}"),
                    ),
                    (
                        "the activated version".to_string(),
                        format!("{message_template_id}:{QUALIFIER_ACTIVE}"),
                    ),
                ];
                for version_number in 1..=versions {
                    labels.push((
                        format!("version {version_number}"),
                        format!("{message_template_id}:{version_number}"),
                    ));
                }
                for (form, template_label) in labels {
                    let reply = read(&emulator, &knowledge_base_id, &template_label).await;
                    assert_refused(
                        &format!("{case}/read {form}"),
                        &reply,
                        UNAUTHORIZED,
                        &template_label,
                    );
                }

                // The versions went with it, so the plane that reports them reports the
                // template as the resource that is missing.
                let listed = list_versions(&emulator, &knowledge_base_id, &message_template_id).await;
                assert_refused(
                    &format!("{case}/list versions"),
                    &listed,
                    TEMPLATE_NOT_FOUND,
                    &message_template_id,
                );

                // And it took only itself: the other template of this knowledge base, and
                // the knowledge base, are untouched.
                let survivor_read = read(&emulator, &knowledge_base_id, &survivor_id).await;
                assert_eq!(
                    survivor_read.status, 200,
                    "case {case:?}: the other template of this knowledge base survives the \
                     delete, got HTTP {} with body {}",
                    survivor_read.status, survivor_read.body
                );
                let parent = emulator
                    .send(
                        "GET",
                        &format!("/knowledgeBases/{}", label(&knowledge_base_id)),
                        None,
                    )
                    .await;
                assert_eq!(
                    parent.status, 200,
                    "case {case:?}: the knowledge base survives the delete of a template in it, \
                     got HTTP {} with body {}",
                    parent.status, parent.body
                );

                // The name the deleted template held is free again: a create may claim it,
                // and it mints a NEW template rather than reviving the old record.
                let reclaimed = create(&emulator, &knowledge_base_id, &name, channel_subtype).await;
                assert_eq!(
                    reclaimed.status, 200,
                    "case {case:?}: the deleted template's name is free for another create, got \
                     HTTP {} with body {}",
                    reclaimed.status, reclaimed.body
                );
                assert_ne!(
                    reclaimed.string("/messageTemplate/messageTemplateId"),
                    message_template_id,
                    "case {case:?}: a create after a delete mints a new template, got the deleted \
                     id {message_template_id}"
                );
            }
        }
    }
}

/// A read refuses every label that names no message template of the knowledge base, not
/// only one a delete forgot: a bare id nothing holds, an ARN of this account for such an
/// id, another knowledge base's template, and ARNs that differ from the minted one in the
/// account or the partition. The knowledge base in the path keeps being reported as the
/// resource it is.
#[tokio::test]
async fn a_read_of_a_template_this_knowledge_base_does_not_hold_is_refused() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "refusals").await;
    let other_knowledge_base_id = knowledge_base(&emulator, "refusals-elsewhere").await;

    let elsewhere = create(&emulator, &other_knowledge_base_id, "held there", "SMS").await;
    let elsewhere_id = elsewhere.string("/messageTemplate/messageTemplateId");
    let here = create(&emulator, &knowledge_base_id, "held here", "SMS").await;
    let here_id = here.string("/messageTemplate/messageTemplateId");

    let resource = format!("message-template/{knowledge_base_id}/{here_id}");
    for (case, template_label) in [
        ("a bare id nothing holds", ABSENT_UUID.to_string()),
        (
            "an ARN of this account nothing holds",
            wisdom_arn(
                "aws",
                REGION,
                engine::EMULATOR_ACCOUNT_ID,
                &format!("message-template/{knowledge_base_id}/{ABSENT_UUID}"),
            ),
        ),
        ("another knowledge base's template", elsewhere_id.clone()),
        (
            "an ARN of another account for a held template",
            wisdom_arn("aws", REGION, OTHER_ACCOUNT, &resource),
        ),
        (
            "an ARN of another partition for a held template",
            wisdom_arn("aws-cn", REGION, engine::EMULATOR_ACCOUNT_ID, &resource),
        ),
    ] {
        let reply = read(&emulator, &knowledge_base_id, &template_label).await;
        assert_refused(case, &reply, UNAUTHORIZED, &template_label);
    }

    // The knowledge base is resolved first and reported as the resource that is missing,
    // so the refusal above is about the template and not about every absent label.
    let absent_parent = read(&emulator, ABSENT_UUID, &here_id).await;
    assert_refused(
        "a knowledge base this account does not hold",
        &absent_parent,
        KNOWLEDGE_BASE_NOT_FOUND,
        ABSENT_UUID,
    );

    // And the template this knowledge base DOES hold still reads, so the rows above are
    // refused because they name nothing here.
    let held = read(&emulator, &knowledge_base_id, &here_id).await;
    assert_eq!(
        held.status, 200,
        "case \"the template this knowledge base holds\": expected HTTP 200, got HTTP {} with \
         body {}",
        held.status, held.body
    );
}

/// A delete whose label carries a version qualifier takes THAT published version and
/// nothing else; the working copy and the other versions are read back unchanged. The
/// `$LATEST` qualifier addresses the working copy, which IS the resource, so deleting it
/// takes the template with every version.
#[tokio::test]
async fn a_qualified_delete_takes_one_version_and_latest_takes_the_template() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "qualified-deletes").await;
    let created = create(&emulator, &knowledge_base_id, "versioned", "EMAIL").await;
    let message_template_id = created.string("/messageTemplate/messageTemplateId");

    let published = publish_versions(&emulator, &knowledge_base_id, &message_template_id, 3).await;
    assert_eq!(
        published,
        vec![1, 2, 3],
        "setup: expected three versions numbered from one, got {published:?}"
    );
    let activated = activate(&emulator, &knowledge_base_id, &message_template_id, 2).await;
    assert_eq!(
        activated.status, 200,
        "setup: the version this row activates must activate, got HTTP {} with body {}",
        activated.status, activated.body
    );

    // One numbered version goes, and it is the activated one, so the `$ACTIVE_VERSION`
    // qualifier names nothing afterwards while the other versions still read.
    let deleted = delete(
        &emulator,
        &knowledge_base_id,
        &format!("{message_template_id}:2"),
    )
    .await;
    assert_eq!(
        deleted.status, DELETED_STATUS,
        "case \"delete version 2\": expected HTTP {DELETED_STATUS}, got HTTP {} with body {}",
        deleted.status, deleted.body
    );
    assert_eq!(
        listed_versions(&list_versions(&emulator, &knowledge_base_id, &message_template_id).await),
        vec![1, 3],
        "case \"delete version 2\": expected the other two versions to remain"
    );
    for (case, qualifier, expected_status) in [
        ("the working copy", QUALIFIER_LATEST, 200),
        ("version 1", "1", 200),
        ("version 3", "3", 200),
    ] {
        let reply = read(
            &emulator,
            &knowledge_base_id,
            &format!("{message_template_id}:{qualifier}"),
        )
        .await;
        assert_eq!(
            reply.status, expected_status,
            "case {case:?}: expected HTTP {expected_status} after one version was deleted, got \
             HTTP {} with body {}",
            reply.status, reply.body
        );
    }
    for (case, qualifier, expected) in [
        ("the deleted version", "2", VERSION_NOT_FOUND),
        (
            "the activated version, which was the deleted one",
            QUALIFIER_ACTIVE,
            VERSION_NOT_FOUND,
        ),
    ] {
        let template_label = format!("{message_template_id}:{qualifier}");
        let reply = read(&emulator, &knowledge_base_id, &template_label).await;
        assert_refused(case, &reply, expected, &template_label);
    }

    // A refused delete changes nothing: a version this template never published, the
    // activated one while none is activated, and a qualifier that names neither a version
    // nor a managed qualifier.
    for (case, qualifier, expected) in [
        ("a version never published", "4", VERSION_NOT_FOUND),
        (
            "the activated version while none is activated",
            QUALIFIER_ACTIVE,
            VERSION_NOT_FOUND,
        ),
        (
            "a qualifier that names neither",
            QUALIFIER_UNKNOWN,
            (
                400,
                "ValidationException",
                "The qualifier $PREVIOUS is not valid. Use a version number, $ACTIVE_VERSION or \
                 $LATEST.",
            ),
        ),
    ] {
        let template_label = format!("{message_template_id}:{qualifier}");
        let reply = delete(&emulator, &knowledge_base_id, &template_label).await;
        assert_refused(case, &reply, expected, &template_label);
        assert_eq!(
            listed_versions(
                &list_versions(&emulator, &knowledge_base_id, &message_template_id).await
            ),
            vec![1, 3],
            "case {case:?}: a refused delete removes nothing"
        );
    }

    // `$LATEST` addresses the working copy, which is the resource itself: deleting it
    // takes the template and the versions it still had.
    let deleted = delete(
        &emulator,
        &knowledge_base_id,
        &format!("{message_template_id}:{QUALIFIER_LATEST}"),
    )
    .await;
    assert_eq!(
        deleted.status, DELETED_STATUS,
        "case \"delete the working copy\": expected HTTP {DELETED_STATUS}, got HTTP {} with body \
         {}",
        deleted.status, deleted.body
    );
    let reply = read(&emulator, &knowledge_base_id, &message_template_id).await;
    assert_refused(
        "read after the working copy was deleted",
        &reply,
        UNAUTHORIZED,
        &message_template_id,
    );
    let listed = list_versions(&emulator, &knowledge_base_id, &message_template_id).await;
    assert_refused(
        "list versions after the working copy was deleted",
        &listed,
        TEMPLATE_NOT_FOUND,
        &message_template_id,
    );
}

/// A delete reports the resource it could not resolve: the knowledge base in the path
/// first, then the template inside it — including a second delete of a template the first
/// one already took, which has nothing left to forget.
#[tokio::test]
async fn a_delete_that_resolves_nothing_reports_the_resource_that_is_missing() {
    let emulator = Emulator::new();
    let knowledge_base_id = knowledge_base(&emulator, "delete-refusals").await;
    let other_knowledge_base_id = knowledge_base(&emulator, "delete-refusals-elsewhere").await;
    let elsewhere = create(&emulator, &other_knowledge_base_id, "held there", "PUSH").await;
    let elsewhere_id = elsewhere.string("/messageTemplate/messageTemplateId");
    let created = create(&emulator, &knowledge_base_id, "deleted twice", "PUSH").await;
    let message_template_id = created.string("/messageTemplate/messageTemplateId");

    let first = delete(&emulator, &knowledge_base_id, &message_template_id).await;
    assert_eq!(
        first.status, DELETED_STATUS,
        "case \"the first delete\": expected HTTP {DELETED_STATUS}, got HTTP {} with body {}",
        first.status, first.body
    );

    for (case, parent, template_label, expected) in [
        (
            "a knowledge base this account does not hold",
            ABSENT_UUID.to_string(),
            elsewhere_id.clone(),
            KNOWLEDGE_BASE_NOT_FOUND,
        ),
        (
            "a template id this knowledge base holds nothing under",
            knowledge_base_id.clone(),
            ABSENT_UUID.to_string(),
            TEMPLATE_NOT_FOUND,
        ),
        (
            "another knowledge base's template",
            knowledge_base_id.clone(),
            elsewhere_id.clone(),
            TEMPLATE_NOT_FOUND,
        ),
        (
            "a template ARN of another account",
            knowledge_base_id.clone(),
            wisdom_arn(
                "aws",
                REGION,
                OTHER_ACCOUNT,
                &format!("message-template/{other_knowledge_base_id}/{elsewhere_id}"),
            ),
            TEMPLATE_NOT_FOUND,
        ),
        (
            "a second delete of the template the first one took",
            knowledge_base_id.clone(),
            message_template_id.clone(),
            TEMPLATE_NOT_FOUND,
        ),
    ] {
        let reply = delete(&emulator, &parent, &template_label).await;
        assert_refused(case, &reply, expected, &template_label);
    }

    // The template the refusals named in another knowledge base is still there, so none of
    // them removed anything.
    let survivor = read(&emulator, &other_knowledge_base_id, &elsewhere_id).await;
    assert_eq!(
        survivor.status, 200,
        "case \"the template the refusals named\": expected HTTP 200, got HTTP {} with body {}",
        survivor.status, survivor.body
    );
}
