// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the DOCUMENT `ExportLens` answers — the `LensJSON` string.
//!
//! `LensJSON` is "the JSON representation of a lens" and is modelled as a bounded STRING
//! (`smithy_query(query="shape", shape="ExportLensOutput")`), so its BYTES are the value a
//! client checksums, diffs between versions and feeds back into `ImportLens`. The rule
//! pinned here is that the service publishes ONE serialization of a lens, derived from the
//! lens it holds rather than echoed from whatever bytes an import carried:
//!
//! 1. The exported document carries its properties in the order `name`, `schemaVersion`,
//!    `description`, `pillars`, with no whitespace between them, and a `number` on every
//!    question and every choice — the same one on each of them, so it is not the object's
//!    position in the array that declares it.
//! 2. Two imports of the SAME lens content spelled differently export the SAME bytes, and
//!    re-importing an export exports those bytes again — so an export is a fixed point and
//!    a client that diffs or hashes two exports sees a difference only where the lens
//!    differs.
//! 3. A version pinned on the export is serialized the same way, from the content that
//!    version published.
//!
//! Rows differ along the SHAPE of the lens — how many pillars, questions and choices it
//! declares, which optional properties (`description`, `helpfulResource`,
//! `improvementPlan`, a content entry's `url`) it carries, and what characters its text
//! needs escaped — because the rule is about the serializer and not about one document.
//!
//! The expected bytes are written out here member by member rather than asked of the
//! service, so this suite states the document's form independently of the code that emits
//! it. Requests go through the real `MultiServiceRouter`, so routing, restJson1
//! (de)serialization, boundary constraint validation and the handler all run as they do in
//! production; the wire helpers are the ones `super::custom_lens_import` and
//! `super::custom_lens_versions` already spell.

use serde_json::{json, Value};

use super::custom_lens_import::{export, import, imported_arn, member};
use super::custom_lens_versions::create_version;
use super::workload_harness::Client;

use service_com_amazonaws_wellarchitected_wellarchitectedapiservicelambda::state;

/// The one `schemaVersion` the lens JSON format specification this service accepts
/// declares, read from the crate rather than spelled again here.
const SCHEMA_VERSION: &str = state::LENS_SCHEMA_VERSION;

/// A `{displayText, url}` content entry of a lens document — what a question's or a
/// choice's `helpfulResource` and a choice's `improvementPlan` carry.
struct Content {
    display_text: &'static str,
    url: Option<&'static str>,
}

/// One choice a question offers, as a lens document declares it.
struct ChoiceSpec {
    id: &'static str,
    title: &'static str,
    helpful_resource: Option<Content>,
    improvement_plan: Option<Content>,
}

/// One question a pillar asks, as a lens document declares it. `risk_rules` is
/// `(condition, risk)`, the last of which carries the `default` condition.
struct QuestionSpec {
    id: &'static str,
    title: &'static str,
    description: Option<&'static str>,
    helpful_resource: Option<Content>,
    choices: Vec<ChoiceSpec>,
    risk_rules: Vec<(&'static str, &'static str)>,
}

struct PillarSpec {
    id: &'static str,
    name: &'static str,
    questions: Vec<QuestionSpec>,
}

/// One lens, as a lens document declares it.
struct LensSpec {
    name: &'static str,
    description: Option<&'static str>,
    pillars: Vec<PillarSpec>,
}

/// A JSON string, escaped as JSON escapes one.
fn quoted(text: &str) -> String {
    Value::String(text.to_string()).to_string()
}

/// A JSON object with these members in THIS order and no whitespace anywhere.
fn object(members: &[(&str, String)]) -> String {
    let written: Vec<String> = members
        .iter()
        .map(|(name, value)| format!("{}:{value}", quoted(name)))
        .collect();
    format!("{{{}}}", written.join(","))
}

/// A JSON array of already-written items, with no whitespace anywhere.
fn array(items: &[String]) -> String {
    format!("[{}]", items.join(","))
}

/// The `number` every question and every choice of an exported document carries. The same
/// on each of them — a question's second choice carries what its first does — so it is not
/// the object's position in the array that declares it, and no document a client writes can
/// set it: the lens JSON format specification declares no `number`.
const DOCUMENT_NUMBER: i64 = 0;

fn expected_content(content: &Content) -> String {
    let mut members = vec![("displayText", quoted(content.display_text))];
    if let Some(url) = content.url {
        members.push(("url", quoted(url)));
    }
    object(&members)
}

/// One choice as the export writes it: its id, its `number`, its title and the content
/// entries it carries.
fn expected_choice(choice: &ChoiceSpec) -> String {
    let mut members = vec![
        ("id", quoted(choice.id)),
        ("number", DOCUMENT_NUMBER.to_string()),
        ("title", quoted(choice.title)),
    ];
    if let Some(helpful_resource) = &choice.helpful_resource {
        members.push(("helpfulResource", expected_content(helpful_resource)));
    }
    if let Some(improvement_plan) = &choice.improvement_plan {
        members.push(("improvementPlan", expected_content(improvement_plan)));
    }
    object(&members)
}

/// One question as the export writes it: its id, its `number`, what it asks, its choices
/// and its risk rules.
fn expected_question(question: &QuestionSpec) -> String {
    let mut members = vec![
        ("id", quoted(question.id)),
        ("number", DOCUMENT_NUMBER.to_string()),
        ("title", quoted(question.title)),
    ];
    if let Some(description) = question.description {
        members.push(("description", quoted(description)));
    }
    if let Some(helpful_resource) = &question.helpful_resource {
        members.push(("helpfulResource", expected_content(helpful_resource)));
    }
    let choices: Vec<String> = question.choices.iter().map(expected_choice).collect();
    members.push(("choices", array(&choices)));
    let rules: Vec<String> = question
        .risk_rules
        .iter()
        .map(|(condition, risk)| {
            object(&[("condition", quoted(condition)), ("risk", quoted(risk))])
        })
        .collect();
    members.push(("riskRules", array(&rules)));
    object(&members)
}

/// The document `ExportLens` must answer for this lens, byte for byte.
fn expected_export(spec: &LensSpec) -> String {
    let mut members = vec![
        ("name", quoted(spec.name)),
        ("schemaVersion", quoted(SCHEMA_VERSION)),
    ];
    if let Some(description) = spec.description {
        members.push(("description", quoted(description)));
    }
    let pillars: Vec<String> = spec
        .pillars
        .iter()
        .map(|pillar| {
            let questions: Vec<String> =
                pillar.questions.iter().map(expected_question).collect();
            object(&[
                ("id", quoted(pillar.id)),
                ("name", quoted(pillar.name)),
                ("questions", array(&questions)),
            ])
        })
        .collect();
    members.push(("pillars", array(&pillars)));
    object(&members)
}

fn sent_content(content: &Content) -> Value {
    let mut entry = json!({ "displayText": content.display_text });
    if let Some(url) = content.url {
        entry["url"] = json!(url);
    }
    entry
}

/// The lens document a CLIENT writes for this lens: the properties the lens JSON format
/// specification declares, and no `number` — the ordinals are the service's.
fn sent_document(spec: &LensSpec) -> Value {
    let pillars: Vec<Value> = spec
        .pillars
        .iter()
        .map(|pillar| {
            let questions: Vec<Value> = pillar
                .questions
                .iter()
                .map(|question| {
                    let choices: Vec<Value> = question
                        .choices
                        .iter()
                        .map(|choice| {
                            let mut written =
                                json!({ "id": choice.id, "title": choice.title });
                            if let Some(helpful_resource) = &choice.helpful_resource {
                                written["helpfulResource"] = sent_content(helpful_resource);
                            }
                            if let Some(improvement_plan) = &choice.improvement_plan {
                                written["improvementPlan"] = sent_content(improvement_plan);
                            }
                            written
                        })
                        .collect();
                    let rules: Vec<Value> = question
                        .risk_rules
                        .iter()
                        .map(|(condition, risk)| {
                            json!({ "condition": condition, "risk": risk })
                        })
                        .collect();
                    let mut written = json!({
                        "id": question.id,
                        "title": question.title,
                        "choices": choices,
                        "riskRules": rules,
                    });
                    if let Some(description) = question.description {
                        written["description"] = json!(description);
                    }
                    if let Some(helpful_resource) = &question.helpful_resource {
                        written["helpfulResource"] = sent_content(helpful_resource);
                    }
                    written
                })
                .collect();
            json!({ "id": pillar.id, "name": pillar.name, "questions": questions })
        })
        .collect();
    let mut document = json!({
        "schemaVersion": SCHEMA_VERSION,
        "name": spec.name,
        "pillars": pillars,
    });
    if let Some(description) = spec.description {
        document["description"] = json!(description);
    }
    document
}

/// The same document spelled as a client that writes `schemaVersion` first and puts a
/// space after every separator spells it — one of the spellings the service must
/// canonicalize away.
fn schema_version_first(spec: &LensSpec) -> String {
    let document = sent_document(spec);
    let written: Vec<String> = ["schemaVersion", "name", "description", "pillars"]
        .iter()
        .filter_map(|name| {
            document
                .get(name)
                .map(|value| format!("{}: {value}", quoted(name)))
        })
        .collect();
    format!("{{{}}}", written.join(", "))
}

/// A content entry a choice or a question points at.
fn content(display_text: &'static str, url: Option<&'static str>) -> Option<Content> {
    Some(Content { display_text, url })
}

/// A question with `choices` choices, one `NO_RISK` rule per choice and the `default`
/// rule last, so a row can vary a question's shape without spelling every rule out.
fn question(
    id: &'static str,
    title: &'static str,
    description: Option<&'static str>,
    choices: Vec<ChoiceSpec>,
) -> QuestionSpec {
    let mut risk_rules: Vec<(&'static str, &'static str)> = choices
        .iter()
        .map(|choice| (choice.id, "NO_RISK"))
        .collect();
    risk_rules.push(("default", "HIGH_RISK"));
    QuestionSpec {
        id,
        title,
        description,
        helpful_resource: None,
        choices,
        risk_rules,
    }
}

/// A choice carrying nothing but the two properties every choice has.
fn plain_choice(id: &'static str, title: &'static str) -> ChoiceSpec {
    ChoiceSpec {
        id,
        title,
        helpful_resource: None,
        improvement_plan: None,
    }
}

/// The lenses this suite exports, spanning the shapes a lens document takes: how many
/// pillars, questions and choices it declares, which optional properties it carries and
/// what characters its text needs escaped.
fn cases() -> Vec<(&'static str, LensSpec)> {
    vec![
        (
            "minimal",
            LensSpec {
                name: "Minimal lens",
                description: None,
                pillars: vec![PillarSpec {
                    id: "governance",
                    name: "Governance",
                    questions: vec![question(
                        "governance_q0",
                        "Which controls are reviewed?",
                        None,
                        vec![
                            plain_choice("governance_q0_c0", "Controls are reviewed"),
                            plain_choice("governance_q0_c1", "Controls are audited"),
                        ],
                    )],
                }],
            },
        ),
        (
            "fully-described",
            LensSpec {
                name: "Described lens",
                description: Some("the questions company policy adds to a review"),
                pillars: vec![PillarSpec {
                    id: "security",
                    name: "Security",
                    questions: vec![QuestionSpec {
                        id: "security_q0",
                        title: "How is access reviewed?",
                        description: Some("what this question asks about"),
                        helpful_resource: content(
                            "the policy this question comes from",
                            Some("https://example.invalid/policy"),
                        ),
                        choices: vec![
                            ChoiceSpec {
                                id: "security_q0_c0",
                                title: "Access is reviewed quarterly",
                                helpful_resource: content("why this matters", None),
                                improvement_plan: content(
                                    "schedule the review",
                                    Some("https://example.invalid/plan"),
                                ),
                            },
                            ChoiceSpec {
                                id: "security_q0_c1",
                                title: "Access is reviewed on every change",
                                helpful_resource: None,
                                improvement_plan: content("watch for changes", None),
                            },
                        ],
                        risk_rules: vec![
                            ("security_q0_c0", "NO_RISK"),
                            ("security_q0_c1", "MEDIUM_RISK"),
                            ("default", "HIGH_RISK"),
                        ],
                    }],
                }],
            },
        ),
        (
            "several-pillars",
            LensSpec {
                name: "Broad lens",
                description: Some("three pillars of differing size"),
                pillars: vec![
                    PillarSpec {
                        id: "operations",
                        name: "Operations",
                        questions: vec![question(
                            "operations_q0",
                            "How is work tracked?",
                            Some("one question with one choice"),
                            vec![plain_choice("operations_q0_c0", "Work is tracked")],
                        )],
                    },
                    PillarSpec {
                        id: "cost",
                        name: "Cost optimization",
                        questions: vec![
                            question(
                                "cost_q0",
                                "How is spend reviewed?",
                                None,
                                vec![
                                    plain_choice("cost_q0_c0", "Spend is reviewed"),
                                    plain_choice("cost_q0_c1", "Spend is forecast"),
                                    plain_choice("cost_q0_c2", "Spend is alarmed on"),
                                ],
                            ),
                            question(
                                "cost_q1",
                                "How is waste found?",
                                Some("the second question of this pillar"),
                                vec![
                                    plain_choice("cost_q1_c0", "Idle resources are found"),
                                    plain_choice("cost_q1_c1", "Idle resources are removed"),
                                ],
                            ),
                            question(
                                "cost_q2",
                                "How are commitments chosen?",
                                None,
                                vec![plain_choice("cost_q2_c0", "Commitments are modelled")],
                            ),
                        ],
                    },
                    PillarSpec {
                        id: "reliability",
                        name: "Reliability",
                        questions: vec![
                            question(
                                "reliability_q0",
                                "How is failure handled?",
                                None,
                                vec![
                                    plain_choice("reliability_q0_c0", "Failure is retried"),
                                    plain_choice("reliability_q0_c1", "Failure is shed"),
                                    plain_choice("reliability_q0_c2", "Failure is alarmed on"),
                                    plain_choice("reliability_q0_c3", "Failure is rehearsed"),
                                ],
                            ),
                            question(
                                "reliability_q1",
                                "How is recovery tested?",
                                None,
                                vec![plain_choice("reliability_q1_c0", "Recovery is tested")],
                            ),
                        ],
                    },
                ],
            },
        ),
        (
            "escaped-text",
            LensSpec {
                // Text the JSON encoding has to escape, and text outside ASCII: the
                // document's bytes are its value, so every character of a lens's prose has
                // to survive the export as JSON writes it.
                name: "Lens \"quoted\" \\ durabilité",
                description: Some("a line\nand a tab\tin the description"),
                pillars: vec![PillarSpec {
                    id: "durabilité",
                    name: "Durabilité ☁",
                    questions: vec![question(
                        "durabilité_q0",
                        "Est-ce « documenté »?",
                        Some("control\u{7}character"),
                        vec![
                            plain_choice("durabilité_q0_c0", "Oui — \"documenté\""),
                            plain_choice("durabilité_q0_c1", "Non \\ pas encore"),
                        ],
                    )],
                }],
            },
        ),
    ]
}

/// Every question's and every choice's `number`, read out of an exported document as
/// `(path, number)` — what the serializer wrote for each of them.
fn numbers(exported: &str) -> Vec<(String, i64)> {
    let document: Value = serde_json::from_str(exported)
        .unwrap_or_else(|error| panic!("an export must be JSON, got {error} in {exported:?}"));
    let mut found: Vec<(String, i64)> = Vec::new();
    let pillars = document["pillars"].as_array().cloned().unwrap_or_default();
    for pillar in &pillars {
        let pillar_id = pillar["id"].as_str().unwrap_or_default().to_string();
        let questions = pillar["questions"].as_array().cloned().unwrap_or_default();
        for question in &questions {
            let question_id = question["id"].as_str().unwrap_or_default().to_string();
            found.push((
                format!("{pillar_id}/{question_id}"),
                question["number"].as_i64().unwrap_or(-1),
            ));
            let choices = question["choices"].as_array().cloned().unwrap_or_default();
            for choice in &choices {
                let choice_id = choice["id"].as_str().unwrap_or_default().to_string();
                found.push((
                    format!("{pillar_id}/{question_id}/{choice_id}"),
                    choice["number"].as_i64().unwrap_or(-1),
                ));
            }
        }
    }
    found
}

/// The `number` every question and every choice of this lens must carry: the same one on
/// each of them, whatever its position — which is what tells this member from an ordinal.
fn expected_numbers(spec: &LensSpec) -> Vec<(String, i64)> {
    let number = DOCUMENT_NUMBER;
    let mut expected: Vec<(String, i64)> = Vec::new();
    for pillar in &spec.pillars {
        for question in &pillar.questions {
            expected.push((format!("{}/{}", pillar.id, question.id), number));
            for choice in &question.choices {
                expected.push((
                    format!("{}/{}/{}", pillar.id, question.id, choice.id),
                    number,
                ));
            }
        }
    }
    expected
}

/// Rule 1 and rule 2. Whatever spelling an import carried, `ExportLens` answers the one
/// canonical document for the lens: `name` first, `schemaVersion` second, no whitespace,
/// and a `number` on every question and every choice.
#[tokio::test]
async fn an_exported_lens_is_the_services_canonical_document() {
    let client = Client::new();
    for (case, spec) in cases() {
        let expected = expected_export(&spec);
        // The expected document states the compactness rule: if a row's own text ever
        // carried a spaced separator, the checks below would stop meaning anything.
        assert!(
            !expected.contains(", ") && !expected.contains(": "),
            "case {case:?}: the expected export must carry no spaced separator, got \
             {expected:?}"
        );

        let document = sent_document(&spec);
        let spellings: Vec<(&str, String)> = vec![
            (
                "compact",
                serde_json::to_string(&document).expect("a JSON document"),
            ),
            (
                "indented",
                serde_json::to_string_pretty(&document).expect("a JSON document"),
            ),
            ("schema-version-first", schema_version_first(&spec)),
            // The export fed straight back in: a client that re-imports what it exported
            // must get that same document back out.
            ("re-imported-export", expected.clone()),
        ];

        for (spelling, sent) in &spellings {
            let case = format!("{case}/{spelling}");
            let answer = import(&client, sent, None, &format!("import-{case}"), None).await;
            let arn = imported_arn(&answer, &case);

            let exported = export(&client, &arn, None).await;
            assert_eq!(
                exported.status, 200,
                "case {case:?}: an accepted import must export, got {exported:?}"
            );
            let lens_json = member(&exported, "LensJSON");
            assert_eq!(
                lens_json, expected,
                "case {case:?}: ExportLens must answer the canonical document, got \
                 {lens_json:?}"
            );

            // The two rules the byte comparison above encodes, stated on their own so a
            // failure says which one broke.
            let name_at = lens_json.find("\"name\"");
            let schema_at = lens_json.find("\"schemaVersion\"");
            assert!(
                name_at < schema_at && name_at == Some(1),
                "case {case:?}: the document must open with name then schemaVersion, got \
                 {lens_json:?}"
            );
            assert_eq!(
                numbers(lens_json),
                expected_numbers(&spec),
                "case {case:?}: every question and choice must carry the document number, \
                 got {lens_json:?}"
            );

            // Reading it again answers the same bytes: the export is derived, so nothing
            // about it moves between two reads.
            let again = export(&client, &arn, None).await;
            assert_eq!(
                member(&again, "LensJSON"),
                expected,
                "case {case:?}: a second export must answer the same document, got {again:?}"
            );
        }
    }
}

/// Rule 3. A version pinned on the export is serialized the same way, from the content
/// that version published — and the draft the lens carries exports identically, because
/// both are the same lens content.
#[tokio::test]
async fn an_exported_lens_version_is_canonical_too() {
    let client = Client::new();
    for (case, spec) in cases() {
        let expected = expected_export(&spec);
        // Published from the spelling a client is least likely to have written: the
        // version's content is the LENS's, not the bytes the import carried.
        let sent = serde_json::to_string_pretty(&sent_document(&spec)).expect("a JSON document");
        let answer = import(&client, &sent, None, &format!("version-import-{case}"), None).await;
        let arn = imported_arn(&answer, case);

        let version = "1.0";
        let published =
            create_version(&client, &arn, version, Some(true), &format!("pub-{case}")).await;
        assert_eq!(
            published.status, 200,
            "case {case:?}: expected CreateLensVersion 200, got {published:?}"
        );

        let pinned = export(&client, &arn, Some(version)).await;
        assert_eq!(
            pinned.status, 200,
            "case {case:?}: a published version must export, got {pinned:?}"
        );
        assert_eq!(
            member(&pinned, "LensJSON"),
            expected,
            "case {case:?}: a pinned version must export the canonical document, got \
             {pinned:?}"
        );

        let current = export(&client, &arn, None).await;
        assert_eq!(
            member(&current, "LensJSON"),
            member(&pinned, "LensJSON"),
            "case {case:?}: the lens and its published version hold one content, got \
             {current:?}"
        );
    }
}
