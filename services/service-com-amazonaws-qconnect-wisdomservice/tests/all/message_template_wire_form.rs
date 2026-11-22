// SPDX-License-Identifier: CC0-1.0

//! The WIRE FORM of the Amazon Q in Connect message-template family, and the members every
//! projection of a stored template derives.
//!
//! Every assertion goes over the wire through the same [`Emulator`] harness the other
//! message-template suites drive the production router with, so a response that is not
//! computed from persisted state cannot pass.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than one request:
//!
//! * RULE 1 — a timestamp member whose model declares `@timestampFormat("date-time")`
//!   reaches the JSON wire as an RFC 3339 / ISO-8601 date-time STRING, never as the
//!   `restJson1` default of epoch seconds; the members that declare `epoch-seconds` keep
//!   that form. The rows span every path of this family that reports an instant (the create,
//!   the read of the working copy, both updates, the version publish and the read of a
//!   published version, the attachment upload and the render) and every channel subtype the
//!   model declares, plus a quick response as the rule's negative side. Which members the
//!   model declares each form on is re-derived from the model file itself, so the service's
//!   own list cannot drift from it.
//! * RULE 2 — `channel` is the Amazon Connect contact channel the template's `channelSubtype`
//!   is a subtype of: the EMAIL subtype is the EMAIL channel, and the messaging subtypes are
//!   subtypes of the CHAT channel. Every projection of the record reports it — the data
//!   shapes and a version summary alike.
//! * RULE 3 — `attributeTypes` is ALWAYS reported, as the empty list for a template that
//!   holds no attribute of any category, so a client can tell "none" from "not returned".
//! * RULE 4 — a template always HAS a language: the create's own `language` when it named
//!   one, and the endpoint's default when it named none.
//! * RULE 5 — `UpdateMessageTemplateMetadata` writes the name, description and grouping
//!   configuration onto the working copy AND onto every published version, leaving what each
//!   version published (its content and its attributes) alone, re-stamps `lastModifiedTime`,
//!   and refuses a name another template of the knowledge base holds, a label carrying a
//!   version qualifier and a resource this account does not hold.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_qconnect_wisdomservice::service::DATE_TIME_MEMBERS;

use super::message_template::{content_for, expected_channel, DEFAULT_LANGUAGE, CHANNEL_SUBTYPES};
use super::message_template_qualifier::MESSAGE_QUALIFIER_NOT_ACCEPTED;
use super::resource_tags::{label, Emulator, Reply, ABSENT_UUID};

/// The `contentDisposition` every attachment of this family carries
/// (`smithy_query(query="shape",shape="ContentDisposition")` declares `ATTACHMENT`).
const CONTENT_DISPOSITION: &str = "ATTACHMENT";

/// The media type a plain-text quick response declares
/// (`smithy_query(query="operation",operation="CreateQuickResponse")`), used by the row that
/// asserts the negative side of RULE 1.
const CONTENT_TYPE_PLAIN: &str = "application/x.quickresponse;format=plain";

/// The managed qualifier that names a template's working copy.
const QUALIFIER_LATEST: &str = "$LATEST";

/// The sentence a create (or a rename) claiming a name a live template of the knowledge base
/// already holds is refused with.
const MESSAGE_NAME_IN_USE: &str = "Name is already in use";

/// This crate's Smithy model, as the one source of which members declare which timestamp
/// format. Read here rather than transcribed, so RULE 1's catalog is the model's own.
const MODEL: &str = include_str!("../../smithy-model/com-amazonaws-qconnect-wisdomservice.json");

/// One instant, read back from the `date-time` text a response reports it as, or `None` when
/// the value is no date-time string at all — which is what makes a member reported in the
/// protocol's default epoch-number form fail a row.
fn instant_of(value: &Value) -> Option<f64> {
    let text = value.as_str()?.to_string();
    ::aws_smithy_types::DateTime::from_str(&text, ::aws_smithy_types::date_time::Format::DateTime)
        .ok()
        .map(|parsed| parsed.as_secs_f64())
}

/// Every member NAME this model carries a `date-time` instant under, read off the service's
/// own catalog of member ids (`<namespace>#<Shape>$<member>`).
fn date_time_member_names() -> BTreeSet<String> {
    DATE_TIME_MEMBERS
        .iter()
        .filter_map(|id| id.split('$').next_back())
        .map(str::to_string)
        .collect()
}

/// Every `(json pointer, value)` in `body`, at any depth, whose member name is one
/// [`date_time_member_names`] names.
fn reported_instants(
    names: &BTreeSet<String>,
    path: &str,
    body: &Value,
    found: &mut Vec<(String, Value)>,
) {
    match body {
        Value::Object(members) => {
            for (name, value) in members {
                let child = format!("{path}/{name}");
                if names.contains(name) {
                    found.push((child.clone(), value.clone()));
                }
                reported_instants(names, &child, value, found);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                reported_instants(names, &format!("{path}/{index}"), item, found);
            }
        }
        _ => {}
    }
}

/// Hold one response to RULE 1: every instant it reports is an RFC 3339 date-time STRING,
/// and it reports at least `expected` of them — so a path that stopped reporting one does
/// not pass by silence.
fn assert_instants_are_date_time_strings(case: &str, reply: &Reply, expected: usize) {
    let mut found = Vec::new();
    reported_instants(&date_time_member_names(), "", &reply.body, &mut found);
    assert!(
        found.len() >= expected,
        "case {case:?}: expected at least {expected} instant member(s) in the response, found \
         {} in {}",
        found.len(),
        reply.body
    );
    for (pointer, value) in found {
        assert!(
            instant_of(&value).is_some_and(|seconds| seconds > 0.0),
            "case {case:?}: {pointer} is modelled @timestampFormat(\"date-time\"), so it must be \
             an RFC 3339 date-time string naming an instant, got {value:?}"
        );
    }
}

/// The message-template surface these rows drive, over the shared harness.
impl Emulator {
    /// A knowledge base of the type that holds message templates, answering with its id.
    async fn message_template_knowledge_base(&self, name: &str) -> String {
        let reply = self
            .send(
                "POST",
                "/knowledgeBases",
                Some(json!({ "name": name, "knowledgeBaseType": "MESSAGE_TEMPLATES" })),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case {name:?}: the knowledge base these rows nest under must be created, got HTTP \
             {} with body {}",
            reply.status, reply.body
        );
        reply.string("/knowledgeBase/knowledgeBaseId")
    }

    async fn create_template(&self, knowledge_base: &str, body: Value) -> Reply {
        self.send(
            "POST",
            &format!("/knowledgeBases/{}/messageTemplates", label(knowledge_base)),
            Some(body),
        )
        .await
    }

    async fn read_template(&self, knowledge_base: &str, message_template: &str) -> Reply {
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

    async fn update_template(&self, knowledge_base: &str, message_template: &str, body: Value) -> Reply {
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

    async fn update_template_metadata(
        &self,
        knowledge_base: &str,
        message_template: &str,
        body: Value,
    ) -> Reply {
        self.send(
            "POST",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}/metadata",
                label(knowledge_base),
                label(message_template)
            ),
            Some(body),
        )
        .await
    }

    async fn publish_version(&self, knowledge_base: &str, message_template: &str) -> Reply {
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

    async fn list_versions(&self, knowledge_base: &str, message_template: &str) -> Reply {
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

    async fn upload_attachment(
        &self,
        knowledge_base: &str,
        message_template: &str,
        name: &str,
        file: &[u8],
    ) -> Reply {
        self.send(
            "POST",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}/attachments",
                label(knowledge_base),
                label(message_template)
            ),
            Some(json!({
                "name": name,
                "contentDisposition": CONTENT_DISPOSITION,
                "body": ::aws_smithy_types::base64::encode(file),
            })),
        )
        .await
    }

    async fn render_template(&self, knowledge_base: &str, message_template: &str) -> Reply {
        self.send(
            "POST",
            &format!(
                "/knowledgeBases/{}/messageTemplates/{}/render",
                label(knowledge_base),
                label(message_template)
            ),
            Some(json!({ "attributes": { "customAttributes": { "orderId": "A-1" } } })),
        )
        .await
    }
}

/// Every timestamp member this model declares, as `(member id, the format it declares or
/// None)` — derived from the model file itself.
fn modelled_timestamp_members() -> Vec<(String, Option<String>)> {
    let model: Value = serde_json::from_str(MODEL).expect("the crate's Smithy model parses");
    let shapes = model["shapes"]
        .as_object()
        .expect("the model declares shapes")
        .clone();
    let mut members = Vec::new();
    for (shape_id, shape) in &shapes {
        let Some(declared) = shape.get("members").and_then(Value::as_object) else {
            continue;
        };
        for (member, definition) in declared {
            let target = definition["target"].as_str().unwrap_or_default();
            let targets_timestamp = target == "smithy.api#Timestamp"
                || shapes
                    .get(target)
                    .is_some_and(|target| target["type"] == "timestamp");
            if !targets_timestamp {
                continue;
            }
            let format = definition
                .get("traits")
                .and_then(|traits| traits.get("smithy.api#timestampFormat"))
                .and_then(Value::as_str)
                .map(str::to_string);
            members.push((format!("{shape_id}${member}"), format));
        }
    }
    members
}

/// RULE 1's catalog: the service names exactly the members its model declares `date-time`
/// on, and names none of the members it declares another form on.
#[test]
fn the_service_names_exactly_the_date_time_members_its_model_declares() {
    let members = modelled_timestamp_members();
    assert!(
        members.len() > DATE_TIME_MEMBERS.len(),
        "case \"the model\": this model declares both timestamp forms, so it must carry more \
         timestamp members than the date-time ones, found {} in all",
        members.len()
    );

    let declared_date_time: BTreeSet<&str> = members
        .iter()
        .filter(|(_, format)| format.as_deref() == Some("date-time"))
        .map(|(id, _)| id.as_str())
        .collect();
    let named: BTreeSet<&str> = DATE_TIME_MEMBERS.iter().copied().collect();
    assert_eq!(
        named, declared_date_time,
        "case \"date-time members\": the service must name exactly the members the model \
         declares date-time on; it names {named:?} and the model declares {declared_date_time:?}"
    );

    let other_form: BTreeSet<&str> = members
        .iter()
        .filter(|(_, format)| format.as_deref() != Some("date-time"))
        .map(|(id, _)| id.as_str())
        .collect();
    assert!(
        !other_form.is_empty(),
        "case \"other members\": this model declares epoch-seconds and format-less timestamp \
         members too, so the catalog must be selective"
    );
    let overlap: Vec<&&str> = other_form.intersection(&named).collect();
    assert!(
        overlap.is_empty(),
        "case \"other members\": a member the model does not declare date-time on must not be \
         named, got {overlap:?}"
    );
}

/// RULE 1 over every message-template path that reports an instant, for every channel
/// subtype the model declares.
#[tokio::test]
async fn every_message_template_path_reports_its_instants_as_date_time_strings() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator.message_template_knowledge_base("instants").await;

    for (channel_subtype, member) in CHANNEL_SUBTYPES {
        let case = format!("{channel_subtype} create");
        let created = emulator
            .create_template(
                &knowledge_base_id,
                json!({
                    "name": format!("instants-{member}"),
                    "channelSubtype": channel_subtype,
                    "content": content_for(channel_subtype),
                }),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected HTTP 200 from the create, got HTTP {} with body {}",
            created.status, created.body
        );
        assert_instants_are_date_time_strings(&case, &created, 2);
        let message_template_id = created.string("/messageTemplate/messageTemplateId");
        let created_at = instant_of(
            created
                .field("/messageTemplate/createdTime")
                .expect("the create reports createdTime"),
        )
        .expect("createdTime names an instant");

        let read = emulator
            .read_template(&knowledge_base_id, &message_template_id)
            .await;
        assert_instants_are_date_time_strings(&format!("{channel_subtype} read"), &read, 2);

        let published = emulator
            .publish_version(&knowledge_base_id, &message_template_id)
            .await;
        assert_instants_are_date_time_strings(
            &format!("{channel_subtype} publish a version"),
            &published,
            2,
        );
        let version = emulator
            .read_template(&knowledge_base_id, &format!("{message_template_id}:1"))
            .await;
        assert_instants_are_date_time_strings(
            &format!("{channel_subtype} read the version"),
            &version,
            2,
        );

        let updated = emulator
            .update_template(
                &knowledge_base_id,
                &message_template_id,
                json!({ "language": "ja_JP" }),
            )
            .await;
        assert_instants_are_date_time_strings(&format!("{channel_subtype} update"), &updated, 2);
        let metadata = emulator
            .update_template_metadata(
                &knowledge_base_id,
                &message_template_id,
                json!({ "description": "the metadata update" }),
            )
            .await;
        assert_instants_are_date_time_strings(
            &format!("{channel_subtype} update the metadata"),
            &metadata,
            2,
        );

        // Both writes happened after the create, so the instant the record reports as last
        // modified is at or after the one it was created at. Nothing here reads the test
        // host's clock.
        let modified_at = instant_of(
            metadata
                .field("/messageTemplate/lastModifiedTime")
                .expect("the update reports lastModifiedTime"),
        )
        .expect("lastModifiedTime names an instant");
        assert!(
            modified_at >= created_at,
            "case {channel_subtype:?}: lastModifiedTime must not precede createdTime, got \
             {modified_at} and {created_at}"
        );
    }
}

/// RULE 1 on the attachment path: the attachment's own two instants, on the upload, on a read
/// of the template that holds it and on a render of that template.
#[tokio::test]
async fn an_attachment_reports_its_instants_as_date_time_strings_on_every_path() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator.message_template_knowledge_base("attachments").await;
    let rows: [(&str, &str, &[u8]); 2] = [
        ("a text file", "notes.txt", b"the attachment body"),
        ("a binary file", "logo.PNG", &[0x89, 0x50, 0x4e, 0x47, 0x00]),
    ];

    for (case, name, file) in rows {
        let created = emulator
            .create_template(
                &knowledge_base_id,
                json!({
                    "name": format!("attachment-{name}"),
                    "channelSubtype": "EMAIL",
                    "content": content_for("EMAIL"),
                }),
            )
            .await;
        let message_template_id = created.string("/messageTemplate/messageTemplateId");

        let uploaded = emulator
            .upload_attachment(&knowledge_base_id, &message_template_id, name, file)
            .await;
        assert_eq!(
            uploaded.status, 200,
            "case {case:?}: expected HTTP 200 from the upload, got HTTP {} with body {}",
            uploaded.status, uploaded.body
        );
        // `uploadedTime` and `urlExpiry`, both date-time members of the attachment shape.
        assert_instants_are_date_time_strings(&format!("{case} uploaded"), &uploaded, 2);
        let uploaded_at = instant_of(
            uploaded
                .field("/attachment/uploadedTime")
                .expect("the upload reports uploadedTime"),
        )
        .expect("uploadedTime names an instant");
        let expires_at = instant_of(
            uploaded
                .field("/attachment/urlExpiry")
                .expect("the upload reports urlExpiry"),
        )
        .expect("urlExpiry names an instant");
        assert!(
            expires_at > uploaded_at,
            "case {case:?}: the url expires after the upload, got {expires_at} and {uploaded_at}"
        );

        // The read of the template carries its own two instants plus the attachment's two.
        let read = emulator
            .read_template(&knowledge_base_id, &message_template_id)
            .await;
        assert_instants_are_date_time_strings(&format!("{case} read"), &read, 4);
        let rendered = emulator
            .render_template(&knowledge_base_id, &message_template_id)
            .await;
        assert_eq!(
            rendered.status, 200,
            "case {case:?}: expected HTTP 200 from the render, got HTTP {} with body {}",
            rendered.status, rendered.body
        );
        assert_instants_are_date_time_strings(&format!("{case} rendered"), &rendered, 2);
    }
}

/// RULE 1's negative side: the members this model declares `epoch-seconds` on keep that form
/// — a quick response's `createdTime` and `lastModifiedTime` are NUMBERS, not date-time
/// strings, on the same endpoint and in the same process.
#[tokio::test]
async fn a_quick_response_keeps_the_epoch_seconds_instants_its_model_declares() {
    let emulator = Emulator::new();
    let knowledge_base = emulator
        .send(
            "POST",
            "/knowledgeBases",
            Some(json!({ "name": "epoch-seconds", "knowledgeBaseType": "QUICK_RESPONSES" })),
        )
        .await;
    let knowledge_base_id = knowledge_base.string("/knowledgeBase/knowledgeBaseId");
    let created = emulator
        .send(
            "POST",
            &format!("/knowledgeBases/{knowledge_base_id}/quickResponses"),
            Some(json!({
                "name": "epoch",
                "content": { "content": "the reply an agent sends" },
                "contentType": CONTENT_TYPE_PLAIN,
            })),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case \"create a quick response\": expected HTTP 200, got HTTP {} with body {}",
        created.status, created.body
    );
    let quick_response_id = created.string("/quickResponse/quickResponseId");
    let read = emulator
        .send(
            "GET",
            &format!("/knowledgeBases/{knowledge_base_id}/quickResponses/{quick_response_id}"),
            None,
        )
        .await;

    for (case, reply) in [("the create", &created), ("the read", &read)] {
        for member in ["createdTime", "lastModifiedTime"] {
            let value = reply.field(&format!("/quickResponse/{member}")).cloned();
            assert!(
                value.as_ref().and_then(Value::as_f64).is_some_and(|seconds| seconds > 0.0),
                "case {case:?}: {member} is modelled @timestampFormat(\"epoch-seconds\"), so it \
                 must stay a number, got {value:?}"
            );
        }
    }
}

/// RULES 2, 3 and 4 over every channel subtype and every projection of the record.
#[tokio::test]
async fn channel_attribute_types_and_language_are_reported_by_every_projection() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator.message_template_knowledge_base("projections").await;

    for (channel_subtype, member) in CHANNEL_SUBTYPES {
        let expected = expected_channel(channel_subtype);
        let created = emulator
            .create_template(
                &knowledge_base_id,
                json!({
                    "name": format!("projection-{member}"),
                    "channelSubtype": channel_subtype,
                    "content": content_for(channel_subtype),
                }),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {channel_subtype:?}: expected HTTP 200 from the create, got HTTP {} with body \
             {}",
            created.status, created.body
        );
        let message_template_id = created.string("/messageTemplate/messageTemplateId");
        let published = emulator
            .publish_version(&knowledge_base_id, &message_template_id)
            .await;
        let version = emulator
            .read_template(&knowledge_base_id, &format!("{message_template_id}:1"))
            .await;
        let updated = emulator
            .update_template(&knowledge_base_id, &message_template_id, json!({}))
            .await;
        let metadata = emulator
            .update_template_metadata(&knowledge_base_id, &message_template_id, json!({}))
            .await;
        let read = emulator
            .read_template(
                &knowledge_base_id,
                &format!("{message_template_id}:{QUALIFIER_LATEST}"),
            )
            .await;

        let projections = [
            ("the create", &created),
            ("the read", &read),
            ("the update", &updated),
            ("the metadata update", &metadata),
            ("the version publish", &published),
            ("the version read", &version),
        ];
        for (projection, reply) in projections {
            let case = format!("{channel_subtype} reported by {projection}");
            assert_eq!(
                reply.field("/messageTemplate/channel").cloned(),
                Some(json!(expected)),
                "case {case:?}: expected channel {expected:?}, got {:?}",
                reply.field("/messageTemplate/channel")
            );
            assert_eq!(
                reply.field("/messageTemplate/attributeTypes").cloned(),
                Some(json!([] as [&str; 0])),
                "case {case:?}: a template that holds no attribute reports the EMPTY list, got \
                 {:?}",
                reply.field("/messageTemplate/attributeTypes")
            );
            assert_eq!(
                reply.field("/messageTemplate/language").cloned(),
                Some(json!(DEFAULT_LANGUAGE)),
                "case {case:?}: a template created without a language reports the default \
                 {DEFAULT_LANGUAGE:?}, got {:?}",
                reply.field("/messageTemplate/language")
            );
        }

        // A version SUMMARY carries the channel too, derived the same way.
        let listed = emulator
            .list_versions(&knowledge_base_id, &message_template_id)
            .await;
        let summary = listed
            .field("/messageTemplateVersionSummaries/0")
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            summary.get("channel").cloned(),
            Some(json!(expected)),
            "case {channel_subtype:?} listed: expected the summary's channel {expected:?}, got \
             {summary:?}"
        );
    }
}

/// RULE 3's entries: each attribute category the template holds contributes its own type,
/// and a template that holds none reports the empty list. One row per category, plus all four
/// at once and none at all.
#[tokio::test]
async fn attribute_types_reports_the_categories_the_template_holds() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator.message_template_knowledge_base("attributes").await;

    // Every value the modelled `MessageTemplateAttributeType` enumeration declares
    // (`smithy_query(query="shape",shape="MessageTemplateAttributeType")`), paired with the
    // `MessageTemplateAttributes` member that carries it
    // (`smithy_query(query="shape",shape="MessageTemplateAttributes")`).
    let rows: [(&str, Value, Vec<&str>); 6] = [
        ("none at all", Value::Null, Vec::new()),
        (
            "an empty custom map, which defines no variable",
            json!({ "customAttributes": {} }),
            Vec::new(),
        ),
        (
            "the system category",
            json!({ "systemAttributes": { "name": "Contact" } }),
            vec!["SYSTEM"],
        ),
        (
            "the agent category",
            json!({ "agentAttributes": { "firstName": "Ada" } }),
            vec!["AGENT"],
        ),
        (
            "the customer-profile category",
            json!({ "customerProfileAttributes": { "firstName": "Grace" } }),
            vec!["CUSTOMER_PROFILE"],
        ),
        (
            "all four categories",
            json!({
                "systemAttributes": { "name": "Contact" },
                "agentAttributes": { "firstName": "Ada" },
                "customerProfileAttributes": { "firstName": "Grace" },
                "customAttributes": { "orderId": "A-1" },
            }),
            vec!["SYSTEM", "AGENT", "CUSTOMER_PROFILE", "CUSTOM"],
        ),
    ];

    for (case, attributes, expected) in rows {
        let mut body = json!({
            "name": format!("attributes-{}", case.replace(' ', "-").replace(',', "")),
            "channelSubtype": "SMS",
            "content": content_for("SMS"),
        });
        if !attributes.is_null() {
            body["defaultAttributes"] = attributes.clone();
        }
        let created = emulator.create_template(&knowledge_base_id, body).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected HTTP 200 from the create, got HTTP {} with body {}",
            created.status, created.body
        );
        let message_template_id = created.string("/messageTemplate/messageTemplateId");
        let read = emulator
            .read_template(&knowledge_base_id, &message_template_id)
            .await;
        for (reader, reply) in [("the create", &created), ("the read", &read)] {
            assert_eq!(
                reply.field("/messageTemplate/attributeTypes").cloned(),
                Some(json!(expected)),
                "case {case:?} reported by {reader}: expected attributeTypes {expected:?}, got \
                 {:?}",
                reply.field("/messageTemplate/attributeTypes")
            );
        }
    }
}

/// RULE 4: a create that names a language keeps it, and one that names none reports the
/// endpoint's default. The rows span documented codes the recorded instance did not use.
#[tokio::test]
async fn a_template_reports_the_language_it_was_created_with_or_the_default() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator.message_template_knowledge_base("languages").await;
    // Codes from the member's own documentation
    // (`smithy_query(query="doc",shape="CreateMessageTemplateRequest",member="language")`).
    let rows: [(&str, Option<&str>, &str); 4] = [
        ("no language at all", None, DEFAULT_LANGUAGE),
        ("a language of the same script", Some("de_DE"), "de_DE"),
        ("a language of another script", Some("ja_JP"), "ja_JP"),
        ("the longest documented code", Some("zh_TW"), "zh_TW"),
    ];

    for (case, named, expected) in rows {
        let mut body = json!({
            "name": format!("language-{}", named.unwrap_or("default")),
            "channelSubtype": "EMAIL",
            "content": content_for("EMAIL"),
        });
        if let Some(named) = named {
            body["language"] = json!(named);
        }
        let created = emulator.create_template(&knowledge_base_id, body).await;
        let message_template_id = created.string("/messageTemplate/messageTemplateId");
        let read = emulator
            .read_template(&knowledge_base_id, &message_template_id)
            .await;
        for (reader, reply) in [("the create", &created), ("the read", &read)] {
            assert_eq!(
                reply.field("/messageTemplate/language").cloned(),
                Some(json!(expected)),
                "case {case:?} reported by {reader}: expected language {expected:?}, got {:?}",
                reply.field("/messageTemplate/language")
            );
        }
    }
}

/// RULE 5: the metadata update writes the working copy and every published version, is
/// partial, and re-stamps `lastModifiedTime`.
#[tokio::test]
async fn the_metadata_update_writes_the_working_copy_and_every_published_version() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator.message_template_knowledge_base("metadata").await;
    let created = emulator
        .create_template(
            &knowledge_base_id,
            json!({
                "name": "before the metadata update",
                "channelSubtype": "EMAIL",
                "content": content_for("EMAIL"),
                "description": "the description the create carried",
                "groupingConfiguration": { "criteria": "RoutingProfileArn", "values": ["before"] },
            }),
        )
        .await;
    let message_template_id = created.string("/messageTemplate/messageTemplateId");
    let created_time = created.string("/messageTemplate/createdTime");
    let published = emulator
        .publish_version(&knowledge_base_id, &message_template_id)
        .await;
    assert_eq!(
        published.status, 200,
        "case \"publish\": expected HTTP 200, got HTTP {} with body {}",
        published.status, published.body
    );
    let published_content = published
        .field("/messageTemplate/content")
        .cloned()
        .unwrap_or(Value::Null);

    let written = emulator
        .update_template_metadata(
            &knowledge_base_id,
            &message_template_id,
            json!({
                "name": "after the metadata update",
                "description": "the description the update wrote",
                "groupingConfiguration": { "criteria": "RoutingProfileArn", "values": ["after"] },
            }),
        )
        .await;
    assert_eq!(
        written.status, 200,
        "case \"the metadata update\": expected HTTP 200, got HTTP {} with body {}",
        written.status, written.body
    );

    // What the update answers, what a read of the working copy answers and what a read of
    // the PUBLISHED version answers must all carry the written metadata.
    let read = emulator
        .read_template(&knowledge_base_id, &message_template_id)
        .await;
    let version = emulator
        .read_template(&knowledge_base_id, &format!("{message_template_id}:1"))
        .await;
    for (reader, reply) in [
        ("the update", &written),
        ("the read", &read),
        ("the published version", &version),
    ] {
        assert_eq!(
            reply.string("/messageTemplate/name"),
            "after the metadata update",
            "case \"metadata\" reported by {reader}: expected the written name, got {:?}",
            reply.string("/messageTemplate/name")
        );
        assert_eq!(
            reply.string("/messageTemplate/description"),
            "the description the update wrote",
            "case \"metadata\" reported by {reader}: expected the written description, got {:?}",
            reply.field("/messageTemplate/description")
        );
        assert_eq!(
            reply
                .field("/messageTemplate/groupingConfiguration/values/0")
                .cloned(),
            Some(json!("after")),
            "case \"metadata\" reported by {reader}: expected the written grouping \
             configuration, got {:?}",
            reply.field("/messageTemplate/groupingConfiguration")
        );
    }
    // The version keeps what it PUBLISHED: the metadata reaches it, the content does not
    // change under it.
    assert_eq!(
        version.field("/messageTemplate/content").cloned(),
        Some(published_content.clone()),
        "case \"metadata\": a version keeps the content it was published from, got {:?}",
        version.field("/messageTemplate/content")
    );
    // The write re-stamps the working copy's lastModifiedTime, which the create left equal
    // to createdTime.
    assert_ne!(
        read.string("/messageTemplate/lastModifiedTime"),
        created_time,
        "case \"metadata\": the write must re-stamp lastModifiedTime, which still reads \
         {created_time:?}"
    );

    // Partial: a write that names only the description leaves the name alone.
    let partial = emulator
        .update_template_metadata(
            &knowledge_base_id,
            &message_template_id,
            json!({ "description": "only the description" }),
        )
        .await;
    assert_eq!(
        partial.string("/messageTemplate/name"),
        "after the metadata update",
        "case \"partial\": a member the request does not carry stays as it was, got {:?}",
        partial.string("/messageTemplate/name")
    );
    assert_eq!(
        partial.string("/messageTemplate/description"),
        "only the description",
        "case \"partial\": expected the written description, got {:?}",
        partial.field("/messageTemplate/description")
    );
}

/// RULE 5's refusals: a name another live template of the knowledge base holds, a label
/// carrying a version qualifier, and a resource this account does not hold.
#[tokio::test]
async fn the_metadata_update_refuses_a_taken_name_a_qualifier_and_an_absent_resource() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator.message_template_knowledge_base("refusals").await;
    let held = emulator
        .create_template(
            &knowledge_base_id,
            json!({
                "name": "the name another template holds",
                "channelSubtype": "EMAIL",
                "content": content_for("EMAIL"),
            }),
        )
        .await;
    let held_id = held.string("/messageTemplate/messageTemplateId");
    let renamed = emulator
        .create_template(
            &knowledge_base_id,
            json!({
                "name": "the name this template holds",
                "channelSubtype": "SMS",
                "content": content_for("SMS"),
            }),
        )
        .await;
    let renamed_id = renamed.string("/messageTemplate/messageTemplateId");

    let clash = emulator
        .update_template_metadata(
            &knowledge_base_id,
            &renamed_id,
            json!({ "name": "the name another template holds" }),
        )
        .await;
    assert_eq!(
        clash.status, 409,
        "case \"a taken name\": expected HTTP 409, got HTTP {} with body {}",
        clash.status, clash.body
    );
    assert_eq!(
        clash.string("/message"),
        MESSAGE_NAME_IN_USE,
        "case \"a taken name\": expected {MESSAGE_NAME_IN_USE:?}, got {:?}",
        clash.string("/message")
    );
    // The refusal wrote nothing: both templates keep the names they had.
    for (case, id, expected) in [
        ("the holder", &held_id, "the name another template holds"),
        ("the renamed", &renamed_id, "the name this template holds"),
    ] {
        let read = emulator.read_template(&knowledge_base_id, id).await;
        assert_eq!(
            read.string("/messageTemplate/name"),
            expected,
            "case {case:?}: a refused rename leaves the record untouched, got {:?}",
            read.string("/messageTemplate/name")
        );
    }
    // Re-stating its OWN name conflicts with nothing.
    let same = emulator
        .update_template_metadata(
            &knowledge_base_id,
            &renamed_id,
            json!({ "name": "the name this template holds" }),
        )
        .await;
    assert_eq!(
        same.status, 200,
        "case \"its own name\": a template may re-state the name it already holds, got HTTP {} \
         with body {}",
        same.status, same.body
    );

    let qualified = emulator
        .update_template_metadata(
            &knowledge_base_id,
            &format!("{renamed_id}:{QUALIFIER_LATEST}"),
            json!({ "description": "through a qualifier" }),
        )
        .await;
    assert_eq!(
        qualified.status, 400,
        "case \"a qualifier\": expected HTTP 400, got HTTP {} with body {}",
        qualified.status, qualified.body
    );
    assert_eq!(
        qualified.string("/message"),
        MESSAGE_QUALIFIER_NOT_ACCEPTED,
        "case \"a qualifier\": expected {MESSAGE_QUALIFIER_NOT_ACCEPTED:?}, got {:?}",
        qualified.string("/message")
    );

    let rows = [
        ("a template this knowledge base does not hold", knowledge_base_id.clone(), ABSENT_UUID.to_string()),
        ("a knowledge base this account does not hold", ABSENT_UUID.to_string(), renamed_id.clone()),
    ];
    for (case, knowledge_base, message_template) in rows {
        let absent = emulator
            .update_template_metadata(
                &knowledge_base,
                &message_template,
                json!({ "description": "onto nothing" }),
            )
            .await;
        assert_eq!(
            absent.status, 404,
            "case {case:?}: expected HTTP 404, got HTTP {} with body {}",
            absent.status, absent.body
        );
    }
}
