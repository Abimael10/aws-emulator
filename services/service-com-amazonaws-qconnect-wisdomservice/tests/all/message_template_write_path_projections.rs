// SPDX-License-Identifier: CC0-1.0

//! The members the WRITE and VERSION paths of the Amazon Q in Connect message-template
//! family derive, asserted over the wire through the production router.
//!
//! The create and the read of a template are pinned by `message_template.rs` and
//! `message_template_wire_form.rs`; this suite pins the three paths a WRITE travels —
//! `UpdateMessageTemplate`, `CreateMessageTemplateVersion` and
//! `ListMessageTemplateVersions` — for a template whose derived members the write itself
//! changes, which is where a projection that answered out of the REQUEST rather than out
//! of the stored record would still pass.
//!
//! The rules under test, each asserted over a TABLE whose rows differ in channel subtype,
//! language and which categories of attribute the template holds:
//!
//! * RULE 1 — every projection of a stored message template reports the members it derives
//!   from the record: `channel`, the Amazon Connect contact channel the immutable
//!   `channelSubtype` is a subtype of (`smithy_query(query="shape",shape="MessageTemplateData")`
//!   declares `channel` beside the required `channelSubtype`, and
//!   `smithy_query(query="shape",shape="MessageTemplateVersionSummary")` declares it on a
//!   version summary too); the template's effective `language`; and `attributeTypes`, the
//!   categories of attribute the stored `defaultAttributes` hold, ALWAYS reported — the
//!   empty list for a template that holds none. The projections are
//!   `UpdateMessageTemplateResponse.messageTemplate` (`MessageTemplateData`),
//!   `CreateMessageTemplateVersionResponse.messageTemplate`
//!   (`ExtendedMessageTemplateData`) and each
//!   `ListMessageTemplateVersionsResponse.messageTemplateVersionSummaries` entry.
//! * RULE 2 — `createdTime` and `lastModifiedTime` reach the JSON wire as RFC 3339 /
//!   ISO-8601 date-time STRINGS, the form their member schemas declare
//!   (`@timestampFormat("date-time")`), never as the `restJson1` default of epoch seconds;
//!   `createdTime` stays the instant the template was created and `lastModifiedTime` is
//!   stamped at the write, so it never precedes `createdTime`.
//! * RULE 3 — a version is an immutable SNAPSHOT: the publish answers, the version's own
//!   read and the version's summary report what the working copy held when that version was
//!   published, so a version cut before a write keeps the values that write replaced while
//!   the one cut after it carries the new ones.

use serde_json::{json, Value};

use super::message_template::{content_for, expected_channel, CHANNEL_SUBTYPES, DEFAULT_LANGUAGE};
use super::resource_tags::{label, Emulator, Reply};

/// The message-template surface this suite drives. An inherent impl in this module: the
/// other suites' harness methods are private to their own modules, so the four calls this
/// suite makes are named here, each under a name of its own.
impl Emulator {
    /// A knowledge base of the type that holds message templates
    /// (`smithy_query(query="shape",shape="KnowledgeBaseType")` declares
    /// `MESSAGE_TEMPLATES`), answering with the id its create minted.
    async fn write_path_knowledge_base(&self, name: &str) -> String {
        let reply = self
            .send(
                "POST",
                "/knowledgeBases",
                Some(json!({
                    "name": format!("message-template-write-path-{name}"),
                    "knowledgeBaseType": "MESSAGE_TEMPLATES",
                })),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case {name:?}: the knowledge base this suite nests under must be created, got \
             HTTP {} with body {}",
            reply.status, reply.body
        );
        reply.string("/knowledgeBase/knowledgeBaseId")
    }

    async fn write_path_create(&self, knowledge_base: &str, body: Value) -> Reply {
        self.send(
            "POST",
            &format!("/knowledgeBases/{}/messageTemplates", label(knowledge_base)),
            Some(body),
        )
        .await
    }

    async fn write_path_update(
        &self,
        knowledge_base: &str,
        message_template: &str,
        body: Value,
    ) -> Reply {
        self.send(
            "POST",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}",
                label(knowledge_base),
                label(message_template)
            ),
            Some(body),
        )
        .await
    }

    async fn write_path_publish(&self, knowledge_base: &str, message_template: &str) -> Reply {
        self.send(
            "POST",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}/versions",
                label(knowledge_base),
                label(message_template)
            ),
            Some(json!({})),
        )
        .await
    }

    async fn write_path_list_versions(
        &self,
        knowledge_base: &str,
        message_template: &str,
    ) -> Reply {
        self.send(
            "GET",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}/versions",
                label(knowledge_base),
                label(message_template)
            ),
            None,
        )
        .await
    }

    async fn write_path_read(&self, knowledge_base: &str, message_template: &str) -> Reply {
        self.send(
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
}

/// One row of the write table: the template the row creates, and what the update writes
/// onto it.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    /// One value of the modelled `ChannelSubtype` enumeration, fixed at create time.
    channel_subtype: &'static str,
    /// The language the UPDATE writes, which is never the one the create left behind
    /// ([`DEFAULT_LANGUAGE`]) unless the row varies something else instead.
    language: &'static str,
    /// The `defaultAttributes` the UPDATE writes, or `None` for a row whose update carries
    /// none at all.
    attributes: Option<Value>,
    /// The `attributeTypes` the updated record must report, in the order the model declares
    /// the values in (`smithy_query(query="shape",shape="MessageTemplateAttributeType")`:
    /// SYSTEM, AGENT, CUSTOMER_PROFILE, CUSTOM).
    attribute_types: Vec<&'static str>,
}

impl Row {
    /// The partial update this row sends: the members it names and no other, so a row that
    /// omits `defaultAttributes` exercises an update that leaves them as they were.
    fn update_body(&self) -> Value {
        let mut body = json!({ "language": self.language });
        if let Some(attributes) = &self.attributes {
            body["defaultAttributes"] = attributes.clone();
        }
        body
    }

    /// The name the row's template is created under, unique inside its knowledge base.
    fn name(&self) -> String {
        format!("write path {}", self.case)
    }
}

/// The table: one row per channel subtype the model declares, each paired with a different
/// documented language code
/// (`smithy_query(query="doc",shape="CreateMessageTemplateRequest",member="language")`: "de_DE,
/// en_US, es_ES, fr_FR, id_ID, it_IT, ja_JP, ko_KR, pt_BR, zh_CN, zh_TW") and a different
/// category of attribute (`smithy_query(query="shape",shape="MessageTemplateAttributes")`
/// carries one member per category, `customAttributes` as a MAP whose keys are the
/// attributes), plus a row that writes every category at once and one that writes none.
fn rows() -> Vec<Row> {
    let categories: [(&str, Option<Value>, Vec<&str>); 4] = [
        (
            "the system category",
            Some(json!({ "systemAttributes": { "name": "Contact" } })),
            vec!["SYSTEM"],
        ),
        (
            "the agent category",
            Some(json!({ "agentAttributes": { "firstName": "Ada" } })),
            vec!["AGENT"],
        ),
        (
            "the customer-profile category",
            Some(json!({ "customerProfileAttributes": { "firstName": "Grace" } })),
            vec!["CUSTOMER_PROFILE"],
        ),
        (
            "the custom category",
            Some(json!({ "customAttributes": { "orderId": "A-1" } })),
            vec!["CUSTOM"],
        ),
    ];
    // A language per subtype, so no two rows of the table write the same one.
    let languages = ["ja_JP", "de_DE", "pt_BR", "zh_TW"];
    let mut rows: Vec<Row> = CHANNEL_SUBTYPES
        .iter()
        .zip(categories)
        .zip(languages)
        .map(
            |(((channel_subtype, _), (case, attributes, attribute_types)), language)| Row {
                case,
                channel_subtype,
                language,
                attributes,
                attribute_types,
            },
        )
        .collect();
    rows.push(Row {
        case: "every category at once",
        channel_subtype: "EMAIL",
        language: "fr_FR",
        attributes: Some(json!({
            "systemAttributes": { "name": "Contact" },
            "agentAttributes": { "firstName": "Ada" },
            "customerProfileAttributes": { "firstName": "Grace" },
            "customAttributes": { "orderId": "A-1" },
        })),
        attribute_types: vec!["SYSTEM", "AGENT", "CUSTOMER_PROFILE", "CUSTOM"],
    });
    rows.push(Row {
        case: "no attribute of any category",
        channel_subtype: "SMS",
        language: "ko_KR",
        attributes: None,
        attribute_types: Vec::new(),
    });
    rows
}

/// One instant, read back from the date-time text a response reports it under. A member
/// reported in the protocol's default epoch-number form is no date-time string at all, which
/// is what fails the row.
fn reported_instant(case: &str, reply: &Reply, pointer: &str) -> f64 {
    let reported = reply.field(pointer).cloned().unwrap_or(Value::Null);
    let Some(text) = reported.as_str() else {
        panic!(
            "case {case:?}: {pointer} must be reported as a date-time string, got {reported:?} \
             in body {}",
            reply.body
        );
    };
    match ::aws_smithy_types::DateTime::from_str(
        text,
        ::aws_smithy_types::date_time::Format::DateTime,
    ) {
        Ok(parsed) => parsed.as_secs_f64(),
        Err(error) => panic!(
            "case {case:?}: {pointer} must parse as an RFC 3339 date-time, got {text:?} \
             ({error})"
        ),
    }
}

/// RULES 1 and 2 over one `messageTemplate` projection: the members it derives, and the two
/// instants it carries. Answers with `(createdTime, lastModifiedTime)` as instants.
fn assert_projection(
    case: &str,
    reply: &Reply,
    channel_subtype: &str,
    language: &str,
    attribute_types: &[&str],
) -> (f64, f64) {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected HTTP 200, got HTTP {} with body {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.field("/messageTemplate/channel").cloned(),
        Some(json!(expected_channel(channel_subtype))),
        "case {case:?}: expected channel {:?}, got {:?}",
        expected_channel(channel_subtype),
        reply.field("/messageTemplate/channel")
    );
    assert_eq!(
        reply.field("/messageTemplate/channelSubtype").cloned(),
        Some(json!(channel_subtype)),
        "case {case:?}: expected channelSubtype {channel_subtype:?}, got {:?}",
        reply.field("/messageTemplate/channelSubtype")
    );
    assert_eq!(
        reply.field("/messageTemplate/language").cloned(),
        Some(json!(language)),
        "case {case:?}: expected language {language:?}, got {:?}",
        reply.field("/messageTemplate/language")
    );
    assert_eq!(
        reply.field("/messageTemplate/attributeTypes").cloned(),
        Some(json!(attribute_types)),
        "case {case:?}: expected attributeTypes {attribute_types:?}, got {:?}",
        reply.field("/messageTemplate/attributeTypes")
    );
    let created_at = reported_instant(case, reply, "/messageTemplate/createdTime");
    let modified_at = reported_instant(case, reply, "/messageTemplate/lastModifiedTime");
    assert!(
        modified_at >= created_at,
        "case {case:?}: lastModifiedTime must not precede createdTime, got {modified_at} and \
         {created_at}"
    );
    (created_at, modified_at)
}

/// RULES 1 and 2 on the two WRITE responses — the update of the working copy and the publish
/// of a version from it — and on the summary a listing of that version reports.
#[tokio::test]
async fn update_and_version_paths_report_the_members_they_derive() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator.write_path_knowledge_base("derived").await;

    for row in rows() {
        let created = emulator
            .write_path_create(
                &knowledge_base_id,
                json!({
                    "name": row.name(),
                    "channelSubtype": row.channel_subtype,
                    "content": content_for(row.channel_subtype),
                }),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {:?}: expected HTTP 200 from the create, got HTTP {} with body {}",
            row.case, created.status, created.body
        );
        let message_template_id = created.string("/messageTemplate/messageTemplateId");
        let created_time = created.string("/messageTemplate/createdTime");

        // The update writes the row's language and attributes onto the working copy, so
        // every member RULE 1 covers is one the stored record now holds and the create did
        // not.
        let updated = emulator
            .write_path_update(&knowledge_base_id, &message_template_id, row.update_body())
            .await;
        let (update_created_at, update_modified_at) = assert_projection(
            &format!("{} reported by the update", row.case),
            &updated,
            row.channel_subtype,
            row.language,
            &row.attribute_types,
        );
        assert_eq!(
            updated.string("/messageTemplate/createdTime"),
            created_time,
            "case {:?}: the update must report the instant the template was created, got {:?} \
             where the create reported {created_time:?}",
            row.case,
            updated.string("/messageTemplate/createdTime")
        );
        assert!(
            update_modified_at > update_created_at,
            "case {:?}: the update must stamp lastModifiedTime at the write, got {} where the \
             template was created at {}",
            row.case,
            update_modified_at,
            update_created_at
        );

        // The publish snapshots that working copy, so the version reports the same derived
        // members — out of the snapshot it holds.
        let published = emulator
            .write_path_publish(&knowledge_base_id, &message_template_id)
            .await;
        assert_projection(
            &format!("{} reported by the version publish", row.case),
            &published,
            row.channel_subtype,
            row.language,
            &row.attribute_types,
        );
        assert_eq!(
            published.field("/messageTemplate/versionNumber").cloned(),
            Some(json!(1)),
            "case {:?}: the first version of a template is version 1, got {:?}",
            row.case,
            published.field("/messageTemplate/versionNumber")
        );
        assert_eq!(
            published.string("/messageTemplate/createdTime"),
            created_time,
            "case {:?}: the version carries the instant the template was created, got {:?} \
             where the create reported {created_time:?}",
            row.case,
            published.string("/messageTemplate/createdTime")
        );

        // And the LISTING of that version reports the channel the summary declares, derived
        // the same way as the data shapes above.
        let listed = emulator
            .write_path_list_versions(&knowledge_base_id, &message_template_id)
            .await;
        assert_eq!(
            listed.status, 200,
            "case {:?}: expected HTTP 200 from the listing, got HTTP {} with body {}",
            row.case, listed.status, listed.body
        );
        let summaries = listed
            .field("/messageTemplateVersionSummaries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            summaries.len(),
            1,
            "case {:?}: one published version is one summary, got {summaries:?}",
            row.case
        );
        let summary = &summaries[0];
        assert_eq!(
            summary.get("channel").cloned(),
            Some(json!(expected_channel(row.channel_subtype))),
            "case {:?}: expected the summary's channel {:?}, got {summary:?}",
            row.case,
            expected_channel(row.channel_subtype)
        );
        assert_eq!(
            summary.get("channelSubtype").cloned(),
            Some(json!(row.channel_subtype)),
            "case {:?}: expected the summary's channelSubtype {:?}, got {summary:?}",
            row.case,
            row.channel_subtype
        );
        assert_eq!(
            summary.get("name").cloned(),
            Some(json!(row.name())),
            "case {:?}: expected the summary's name {:?}, got {summary:?}",
            row.case,
            row.name()
        );
        assert_eq!(
            summary.get("versionNumber").cloned(),
            Some(json!(1)),
            "case {:?}: expected the summary of version 1, got {summary:?}",
            row.case
        );
    }
}

/// RULE 3: a version cut BEFORE the write keeps the values that write replaced, while the one
/// cut after it carries the new ones — through the publish, the version's own read and the
/// listing alike.
#[tokio::test]
async fn published_versions_report_the_working_copy_they_snapshotted() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator.write_path_knowledge_base("snapshots").await;

    for row in rows() {
        let created = emulator
            .write_path_create(
                &knowledge_base_id,
                json!({
                    "name": row.name(),
                    "channelSubtype": row.channel_subtype,
                    "content": content_for(row.channel_subtype),
                }),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {:?}: expected HTTP 200 from the create, got HTTP {} with body {}",
            row.case, created.status, created.body
        );
        let message_template_id = created.string("/messageTemplate/messageTemplateId");

        // Version 1 is cut from the template as the create left it: the endpoint's default
        // language, and no attribute of any category.
        let first = emulator
            .write_path_publish(&knowledge_base_id, &message_template_id)
            .await;
        assert_projection(
            &format!("{} published before the update", row.case),
            &first,
            row.channel_subtype,
            DEFAULT_LANGUAGE,
            &[],
        );

        let updated = emulator
            .write_path_update(&knowledge_base_id, &message_template_id, row.update_body())
            .await;
        assert_eq!(
            updated.status, 200,
            "case {:?}: expected HTTP 200 from the update, got HTTP {} with body {}",
            row.case, updated.status, updated.body
        );

        // Version 2 is cut from the UPDATED working copy, and version 1 is left as it was —
        // which is what a read of each version reports.
        let second = emulator
            .write_path_publish(&knowledge_base_id, &message_template_id)
            .await;
        assert_projection(
            &format!("{} published after the update", row.case),
            &second,
            row.channel_subtype,
            row.language,
            &row.attribute_types,
        );
        assert_eq!(
            second.field("/messageTemplate/versionNumber").cloned(),
            Some(json!(2)),
            "case {:?}: the second publish numbers version 2, got {:?}",
            row.case,
            second.field("/messageTemplate/versionNumber")
        );

        let first_read = emulator
            .write_path_read(&knowledge_base_id, &format!("{message_template_id}:1"))
            .await;
        assert_projection(
            &format!("{} read back as version 1", row.case),
            &first_read,
            row.channel_subtype,
            DEFAULT_LANGUAGE,
            &[],
        );
        let second_read = emulator
            .write_path_read(&knowledge_base_id, &format!("{message_template_id}:2"))
            .await;
        assert_projection(
            &format!("{} read back as version 2", row.case),
            &second_read,
            row.channel_subtype,
            row.language,
            &row.attribute_types,
        );

        // Both versions are listed, each summary carrying the channel of the template they
        // were cut from.
        let listed = emulator
            .write_path_list_versions(&knowledge_base_id, &message_template_id)
            .await;
        let summaries = listed
            .field("/messageTemplateVersionSummaries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let versions: Vec<Value> = summaries
            .iter()
            .map(|summary| summary.get("versionNumber").cloned().unwrap_or(Value::Null))
            .collect();
        assert_eq!(
            versions,
            vec![json!(1), json!(2)],
            "case {:?}: both published versions are listed in ascending order, got \
             {summaries:?}",
            row.case
        );
        for summary in &summaries {
            assert_eq!(
                summary.get("channel").cloned(),
                Some(json!(expected_channel(row.channel_subtype))),
                "case {:?}: every summary carries the channel {:?}, got {summary:?}",
                row.case,
                expected_channel(row.channel_subtype)
            );
            assert_eq!(
                summary.get("isActive").cloned(),
                Some(json!(false)),
                "case {:?}: a published version is not activated by the publish, got \
                 {summary:?}",
                row.case
            );
        }
    }
}
