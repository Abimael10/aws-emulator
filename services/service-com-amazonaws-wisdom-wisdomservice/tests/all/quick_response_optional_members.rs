// SPDX-License-Identifier: CC0-1.0

//! What a quick response reports for the members its create did NOT name, and how the
//! content is reported back — over the wire, through the same [`MultiServiceRouter`] the
//! production emulator mounts.
//!
//! The rules under test, each asserted over a TABLE of creates rather than one recorded
//! request:
//!
//! * a create stores every optional member it names AS GIVEN, and the ones it names none
//!   of with this service's own default for them, so every later read of the record —
//!   `GetQuickResponse`, `ListQuickResponses`, `UpdateQuickResponse` — reports a value for
//!   each: the richtext media type for `contentType`, every contact channel the service
//!   supports for `channels`, the default language code for `language`, and active for
//!   `isActive`;
//! * `contents` reports the content in EVERY media type the quick response can be read as
//!   — plain text always, `markdown` as well when it is written in richtext — so
//!   switching the media type on an update changes which members carry it;
//! * `attributeList` reports the contact attributes the CONTENT names, each once and in
//!   the order the content first names them, and the empty list when it names none;
//! * a `QuickResponseSummary` carries the members the summary shape declares and no
//!   others (`smithy_query(query="shape",shape="QuickResponseSummary")`: no `language`,
//!   and no `attributeList` either).
//!
//! The last test pins the local `attributeList` declaration against the model itself:
//! every shape the overlay restates is compared declaration for declaration with the
//! snapshot's, and `QuickResponseData` must differ from it by exactly that one member. A
//! refreshed snapshot that declares the member, or that changes a target, value set,
//! constraint or timestamp format the overlay restates, fails there rather than silently
//! narrowing what the service reports.
//!
//! The suite reuses `quick_response.rs`'s harness: the same router, the same signed
//! region, the same knowledge-base helper.

use serde_json::{json, Map, Value};

use super::quick_response::{
    Emulator, Reply, ACTIVE_BY_DEFAULT, CHANNEL_CHAT, CONTENT_TYPE_BY_DEFAULT,
    CONTENT_TYPE_MARKDOWN, CONTENT_TYPE_PLAIN, LANGUAGE_BY_DEFAULT, STATUS_CREATED,
};

/// The published model snapshot this crate is generated from, embedded at compile time so
/// the local `attributeList` declaration is checked against the model rather than against
/// a copy of it.
const SNAPSHOT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/smithy-model/com-amazonaws-wisdom-wisdomservice.json"
));

/// The overlay model the local `QuickResponseData` declaration is generated from, embedded
/// for the same reason: what it restates of the snapshot must still match it.
const OVERLAY: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/smithy-model/local-additions.json"
));

/// The shape the local declaration is of.
const DATA_SHAPE: &str = "com.amazonaws.wisdom#QuickResponseData";

/// The member this crate declares locally, which the snapshot does not.
const LOCAL_MEMBER: &str = "attributeList";

/// The shapes the overlay restates for their TYPE alone: codegen resolves a structure
/// member through the Rust field's type (the modelled type in `shared_types`), reading only
/// the shape's type from the model.
const MEMBERLESS_SHAPES: [&str; 2] = [
    "com.amazonaws.wisdom#QuickResponseContents",
    "com.amazonaws.wisdom#GroupingConfiguration",
];

/// Language codes the model documents for `language`
/// (`smithy_query(query="doc",shape="CreateQuickResponseRequest",member="language")`: "The
/// supported language codes include de_DE, en_US, es_ES, fr_FR, id_ID, it_IT, ja_JP,
/// ko_KR, pt_BR, zh_CN, zh_TW"), taken from both ends of that catalog and from its middle
/// so the rows do not all use one code.
const LANGUAGES: [&str; 3] = ["de_DE", "ja_JP", "zh_TW"];

/// One create, and what the record it writes must report.
#[derive(Debug, Clone)]
struct Case {
    label: &'static str,
    name: &'static str,
    content: &'static str,
    /// `None` is a create that names no `contentType` at all.
    content_type: Option<&'static str>,
    is_active: Option<bool>,
    /// Empty is a create that names no channel at all.
    channels: &'static [&'static str],
    /// `None` is a create that names no `language` at all.
    language: Option<&'static str>,
    /// The contact attributes `content` names, in the order it first names them.
    attributes: &'static [&'static str],
}

impl Case {
    fn create_body(&self) -> Value {
        let mut body = json!({ "name": self.name, "content": { "content": self.content } });
        if let Some(content_type) = self.content_type {
            body["contentType"] = json!(content_type);
        }
        if let Some(is_active) = self.is_active {
            body["isActive"] = json!(is_active);
        }
        if !self.channels.is_empty() {
            body["channels"] = json!(self.channels);
        }
        if let Some(language) = self.language {
            body["language"] = json!(language);
        }
        body
    }

    fn expected_content_type(&self) -> &str {
        self.content_type.unwrap_or(CONTENT_TYPE_BY_DEFAULT)
    }

    fn expected_channels(&self) -> Vec<String> {
        if self.channels.is_empty() {
            vec![CHANNEL_CHAT.to_string()]
        } else {
            self.channels.iter().map(|value| value.to_string()).collect()
        }
    }

    fn expected_language(&self) -> &str {
        self.language.unwrap_or(LANGUAGE_BY_DEFAULT)
    }

    fn expected_is_active(&self) -> bool {
        self.is_active.unwrap_or(ACTIVE_BY_DEFAULT)
    }

    fn expected_attributes(&self) -> Vec<String> {
        self.attributes
            .iter()
            .map(|value| value.to_string())
            .collect()
    }
}

/// The table: one row per axis the rules generalise over — which optional member the
/// create names, both modelled media types and a create naming none, one channel and
/// several, the documented language codes, and the attributes the content names (none,
/// one, several, a repeated one, and a placeholder the content never closes).
fn cases() -> Vec<Case> {
    let mut cases = vec![
        Case {
            label: "a create naming no optional member at all",
            name: "bare-create",
            content: "Hello, how can I help?",
            content_type: None,
            is_active: None,
            channels: &[],
            language: None,
            attributes: &[],
        },
        Case {
            label: "the plain-text media type named explicitly",
            name: "plain-named",
            content: "Thanks for waiting.",
            content_type: Some(CONTENT_TYPE_PLAIN),
            is_active: None,
            channels: &[],
            language: None,
            attributes: &[],
        },
        Case {
            label: "the richtext media type named explicitly",
            name: "markdown-named",
            content: "# Heading\n\nthe rich reply",
            content_type: Some(CONTENT_TYPE_MARKDOWN),
            is_active: None,
            channels: &[],
            language: None,
            attributes: &[],
        },
        Case {
            label: "created inactive, every other optional member defaulted",
            name: "inactive-only",
            content: "Out of service.",
            content_type: None,
            is_active: Some(false),
            channels: &[],
            language: None,
            attributes: &[],
        },
        Case {
            label: "several channels named, every other optional member defaulted",
            name: "channels-only",
            content: "We will be in touch.",
            content_type: None,
            is_active: None,
            channels: &["Chat", "Email"],
            language: None,
            attributes: &[],
        },
        Case {
            label: "every optional member named at once",
            name: "everything-named",
            content: "Votre commande est expediee.",
            content_type: Some(CONTENT_TYPE_PLAIN),
            is_active: Some(false),
            channels: &["Email"],
            language: Some("pt_BR"),
            attributes: &[],
        },
        Case {
            label: "a content naming one contact attribute",
            name: "one-attribute",
            content: "Hi {{Customer.FirstName}}, how can I help?",
            content_type: None,
            is_active: None,
            channels: &[],
            language: None,
            attributes: &["Customer.FirstName"],
        },
        Case {
            label: "a content naming two attributes, one of them twice, with whitespace",
            name: "repeated-attributes",
            content: "{{ Customer.FirstName }} ordered {{Order.Id}} — thanks, {{Customer.FirstName}}!",
            content_type: Some(CONTENT_TYPE_MARKDOWN),
            is_active: None,
            channels: &[],
            language: None,
            attributes: &["Customer.FirstName", "Order.Id"],
        },
        Case {
            label: "a content whose placeholder is never closed, and an empty one",
            name: "unclosed-attribute",
            content: "{{Order.Id and {{}} are not attributes",
            content_type: None,
            is_active: None,
            channels: &[],
            language: None,
            attributes: &[],
        },
    ];
    // One row per documented language code taken above, so the rule is not asserted on a
    // single code.
    for (index, language) in LANGUAGES.into_iter().enumerate() {
        cases.push(Case {
            label: "a documented language code named explicitly",
            name: match index {
                0 => "localized-0",
                1 => "localized-1",
                _ => "localized-2",
            },
            content: "Localized reply",
            content_type: None,
            is_active: None,
            channels: &[],
            language: Some(language),
            attributes: &[],
        });
    }
    cases
}

/// Every member of a reported `QuickResponseData`, checked against the create that wrote
/// the record.
fn assert_quick_response_data(case: &Case, reply: &Reply, source: &str) {
    let label = case.label;
    assert_eq!(
        reply.status, 200,
        "case {label:?} ({source}): expected HTTP 200, got {} with body {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.string("/quickResponse/contentType").as_deref(),
        Some(case.expected_content_type()),
        "case {label:?} ({source}): expected the stored contentType, got {:?}",
        reply.field("/quickResponse/contentType")
    );
    assert_eq!(
        reply.string("/quickResponse/status").as_deref(),
        Some(STATUS_CREATED),
        "case {label:?} ({source}): expected status {STATUS_CREATED}, got {:?}",
        reply.field("/quickResponse/status")
    );
    assert_eq!(
        reply
            .string("/quickResponse/contents/plainText/content")
            .as_deref(),
        Some(case.content),
        "case {label:?} ({source}): the content is readable as plain text whatever it is \
         marked up as, got body {}",
        reply.body
    );
    let expected_markdown =
        (case.expected_content_type() == CONTENT_TYPE_MARKDOWN).then_some(case.content);
    assert_eq!(
        reply
            .string("/quickResponse/contents/markdown/content")
            .as_deref(),
        expected_markdown,
        "case {label:?} ({source}): contents.markdown must carry the content for contentType \
         {} and be absent otherwise, got body {}",
        case.expected_content_type(),
        reply.body
    );
    assert_eq!(
        reply
            .field("/quickResponse/isActive")
            .and_then(Value::as_bool),
        Some(case.expected_is_active()),
        "case {label:?} ({source}): expected the stored isActive, got {:?}",
        reply.field("/quickResponse/isActive")
    );
    assert_eq!(
        strings(reply.field("/quickResponse/channels")),
        Some(case.expected_channels()),
        "case {label:?} ({source}): expected the stored channels, got {:?}",
        reply.field("/quickResponse/channels")
    );
    assert_eq!(
        reply.string("/quickResponse/language").as_deref(),
        Some(case.expected_language()),
        "case {label:?} ({source}): expected the stored language, got {:?}",
        reply.field("/quickResponse/language")
    );
    assert_eq!(
        strings(reply.field("/quickResponse/attributeList")),
        Some(case.expected_attributes()),
        "case {label:?} ({source}): expected the contact attributes the content names, got \
         {:?}",
        reply.field("/quickResponse/attributeList")
    );
}

/// A JSON array of strings as a `Vec`, or `None` when the member is absent.
fn strings(value: Option<&Value>) -> Option<Vec<String>> {
    Some(
        value?
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
    )
}

/// The `shapes` object of one model document.
fn model_shapes(model: &str) -> Value {
    let parsed: Value = serde_json::from_str(model).expect("a model document must be JSON");
    let shapes = parsed["shapes"]
        .as_object()
        .expect("a model document must carry a shapes object")
        .clone();
    Value::Object(shapes)
}

/// `declaration` with every documentation trait dropped, recursively — and with a traits
/// object left empty by that dropped too, so a member the snapshot documents and nothing
/// else compares equal to the same member restated with no traits at all.
///
/// Documentation is prose: the overlay restates what codegen READS — targets, types, enum
/// values, constraint traits and the timestamp format — so comparing declarations has to
/// ignore the traits that carry no behaviour.
fn without_docs(declaration: &Value) -> Value {
    const PROSE: [&str; 2] = ["smithy.api#documentation", "smithy.api#examples"];
    match declaration {
        Value::Object(members) => Value::Object(
            members
                .iter()
                .filter(|(key, _)| !PROSE.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), without_docs(value)))
                .filter(|(key, value)| {
                    key.as_str() != "traits" || !value.as_object().is_some_and(Map::is_empty)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(without_docs).collect()),
        other => other.clone(),
    }
}

#[test]
fn the_overlay_restates_the_snapshot_exactly_but_for_the_member_it_adds() {
    // The overlay is what the local `QuickResponseData` is generated from, so anything it
    // restates of the snapshot — a member's target and traits, a shape's type, an enum's
    // values, a constraint — must still BE what the snapshot says, or the type on the wire
    // would be narrower or wider than the model without any other test noticing.
    let snapshot = model_shapes(SNAPSHOT);
    let overlay = model_shapes(OVERLAY);
    let restated = overlay
        .as_object()
        .expect("the overlay carries a shapes object");

    for (shape_id, declared) in restated {
        let modelled = &snapshot[shape_id];
        assert!(
            !modelled.is_null(),
            "the overlay restates {shape_id}, which the snapshot does not declare"
        );

        if shape_id == DATA_SHAPE {
            // The one shape that differs on purpose: the snapshot's members, restated as
            // the snapshot declares them, plus the member AWS carries and it does not.
            let declared_members = declared["members"]
                .as_object()
                .expect("the overlay declares the quick-response members");
            let modelled_members = modelled["members"]
                .as_object()
                .expect("the snapshot declares the quick-response members");
            assert!(
                !modelled_members.contains_key(LOCAL_MEMBER),
                "the snapshot now declares {LOCAL_MEMBER}, so the local declaration should \
                 give way to the modelled member"
            );
            for (member, modelled_member) in modelled_members {
                assert_eq!(
                    without_docs(&declared_members[member]),
                    without_docs(modelled_member),
                    "member {member} of {shape_id} must be restated as the snapshot declares \
                     it, got {:?}",
                    declared_members.get(member)
                );
            }
            let added: Vec<&String> = declared_members
                .keys()
                .filter(|member| !modelled_members.contains_key(*member))
                .collect();
            assert_eq!(
                added,
                vec![LOCAL_MEMBER],
                "{shape_id} may add {LOCAL_MEMBER} and nothing else, added {added:?}"
            );
            continue;
        }

        if MEMBERLESS_SHAPES.contains(&shape_id.as_str()) {
            // Restated for its type alone (see the constant): the modelled Rust type
            // carries the members.
            assert_eq!(
                declared["type"], modelled["type"],
                "{shape_id} must be restated with the type the snapshot gives it"
            );
            assert!(
                declared["members"].as_object().is_some_and(Map::is_empty),
                "{shape_id} is restated for its type alone, so it must declare no members"
            );
            continue;
        }

        assert_eq!(
            without_docs(declared),
            without_docs(modelled),
            "{shape_id} must be restated exactly as the snapshot declares it"
        );
    }
}

#[tokio::test]
async fn a_create_stores_the_service_defaults_for_the_members_it_does_not_name() {
    let emulator = Emulator::new();
    let knowledge_base = emulator.knowledge_base_id("optional-members-kb").await;

    for case in cases() {
        let label = case.label;
        let created = emulator
            .send(
                "POST",
                &format!("/knowledgeBases/{knowledge_base}/quickResponses"),
                Some(case.create_body()),
            )
            .await;
        assert_quick_response_data(&case, &created, "the create's answer");
        let quick_response_id = created
            .string("/quickResponse/quickResponseId")
            .unwrap_or_else(|| panic!("case {label:?}: the create answered no quickResponseId"));

        // The read path answers out of the same record, member for member.
        let read = emulator
            .send(
                "GET",
                &format!("/knowledgeBases/{knowledge_base}/quickResponses/{quick_response_id}"),
                None,
            )
            .await;
        assert_quick_response_data(&case, &read, "the read of the stored record");

        // An update that names ONE member keeps every stored value it did not name,
        // defaults included.
        let updated = emulator
            .send(
                "POST",
                &format!("/knowledgeBases/{knowledge_base}/quickResponses/{quick_response_id}"),
                Some(json!({ "description": "touched by an update" })),
            )
            .await;
        assert_quick_response_data(&case, &updated, "the update's answer");
        assert_eq!(
            updated.string("/quickResponse/description").as_deref(),
            Some("touched by an update"),
            "case {label:?}: expected the update's own description, got {:?}",
            updated.field("/quickResponse/description")
        );
    }
}

#[tokio::test]
async fn the_listed_summary_carries_the_stored_values_and_only_its_own_members() {
    let emulator = Emulator::new();
    let knowledge_base = emulator
        .knowledge_base_id("optional-members-listing-kb")
        .await;

    for case in cases() {
        let created = emulator
            .send(
                "POST",
                &format!("/knowledgeBases/{knowledge_base}/quickResponses"),
                Some(case.create_body()),
            )
            .await;
        assert_eq!(
            created.status,
            200,
            "case {:?}: the create must be accepted, got {} with body {}",
            case.label,
            created.status,
            created.body
        );
    }

    let listed = emulator
        .send(
            "GET",
            &format!("/knowledgeBases/{knowledge_base}/quickResponses"),
            None,
        )
        .await;
    assert_eq!(
        listed.status, 200,
        "the listing must answer HTTP 200, got {} with body {}",
        listed.status, listed.body
    );
    let summaries = listed
        .field("/quickResponseSummaries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for case in cases() {
        let label = case.label;
        let summary = summaries
            .iter()
            .find(|summary| summary.get("name").and_then(Value::as_str) == Some(case.name))
            .unwrap_or_else(|| {
                panic!(
                    "case {label:?}: the listing holds no summary named {:?}",
                    case.name
                )
            });
        assert_eq!(
            strings(summary.get("channels")),
            Some(case.expected_channels()),
            "case {label:?}: the summary must report the stored channels, got {summary}"
        );
        assert_eq!(
            summary.get("isActive").and_then(Value::as_bool),
            Some(case.expected_is_active()),
            "case {label:?}: the summary must report the stored isActive, got {summary}"
        );
        assert_eq!(
            summary.get("contentType").and_then(Value::as_str),
            Some(case.expected_content_type()),
            "case {label:?}: the summary must report the stored contentType, got {summary}"
        );
        // The summary shape declares none of these, so the listing claims none of them.
        for absent in ["language", "attributeList", "contents"] {
            assert!(
                summary.get(absent).is_none(),
                "case {label:?}: QuickResponseSummary declares no {absent}, got {summary}"
            );
        }
    }
}

#[tokio::test]
async fn switching_the_media_type_changes_which_contents_members_carry_the_content() {
    let emulator = Emulator::new();
    let knowledge_base = emulator.knowledge_base_id("optional-members-media-kb").await;

    // (the media type the update names, whether `markdown` must then carry the content)
    let rows = [
        (CONTENT_TYPE_PLAIN, false),
        (CONTENT_TYPE_MARKDOWN, true),
        (CONTENT_TYPE_PLAIN, false),
    ];
    let content = "The reply an agent inserts";
    let created = emulator
        .send(
            "POST",
            &format!("/knowledgeBases/{knowledge_base}/quickResponses"),
            Some(json!({ "name": "media-switch", "content": { "content": content } })),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the create must be accepted, got {} with body {}",
        created.status, created.body
    );
    let quick_response_id = created
        .string("/quickResponse/quickResponseId")
        .expect("the create answered no quickResponseId");

    for (content_type, expects_markdown) in rows {
        let updated = emulator
            .send(
                "POST",
                &format!("/knowledgeBases/{knowledge_base}/quickResponses/{quick_response_id}"),
                Some(json!({ "contentType": content_type })),
            )
            .await;
        assert_eq!(
            updated.status, 200,
            "case {content_type:?}: expected HTTP 200, got {} with body {}",
            updated.status, updated.body
        );
        assert_eq!(
            updated
                .string("/quickResponse/contents/plainText/content")
                .as_deref(),
            Some(content),
            "case {content_type:?}: plain text carries the content whatever the media type, \
             got body {}",
            updated.body
        );
        assert_eq!(
            updated
                .string("/quickResponse/contents/markdown/content")
                .as_deref(),
            expects_markdown.then_some(content),
            "case {content_type:?}: expected contents.markdown present={expects_markdown}, got \
             body {}",
            updated.body
        );
    }
}
