// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the per-choice improvement plan a lens review's improvement
//! summaries report: `ImprovementSummary$ImprovementPlans` on `ListLensReviewImprovements`,
//! and the same content on the question's own choice catalog
//! (`Choice$ImprovementPlan`).
//!
//! The rule these pin, over a table of lens documents rather than one recorded request: a
//! question is an improvement item because its stored answer leaves best practices out, and
//! what there is to improve is exactly those best practices — so the summary reports one
//! `ChoiceImprovementPlan` per choice of the question the answer leaves unselected, in the
//! lens's own choice order, each carrying the `displayText` and the `url` that lens's own
//! content declares as that choice's improvement plan. The per-choice improvement plan is
//! content only a custom lens's document declares ("this value is only applicable to custom
//! lenses"), so a review whose lens declares none reports no plan detail at all and the
//! optional member is left off.
//!
//! The rows differ along what the rule generalises over: how many choices the question
//! offers, which of them the answer leaves out, whether the document declares a `url`
//! beside the `displayText`, whether it declares a plan for only some of the question's
//! choices, and whether it declares one at all. The AWS-official half of the lens catalog
//! declares no per-choice content, which `super::lens_review_improvements` pins.
//!
//! Requests go through the real `MultiServiceRouter`, so routing, restJson1
//! (de)serialization, boundary constraint validation and the handler all run as they do in
//! production. The HTTP client is the shared `super::workload_harness`; the import and
//! publish wire helpers are the ones `super::custom_lens_import` and
//! `super::custom_lens_versions` already spell.

use serde_json::{json, Value};

use super::custom_lens_import::{import, imported_arn};
use super::custom_lens_versions::create_version;
use super::workload_harness::{created_id, encode_path_segment, minimal_create, Answer, Client};

/// The `schemaVersion` the lens JSON format specification the `ImportLens` doc points at
/// requires, as `super::custom_lens_import` spells it.
const SCHEMA_VERSION: &str = "2021-11-01";

/// The pillar and question one document under test defines. One question is enough: the
/// rule is about the CHOICES of an at-risk question, which the rows vary.
const PILLAR_ID: &str = "governance";
const QUESTION_ID: &str = "governance_q0";

/// The risks the improvement plan selects questions at
/// (`smithy_query(query="shape", shape="Risk")` publishes these beside `NONE`,
/// `NOT_APPLICABLE` and `UNANSWERED`).
const IDENTIFIED_RISKS: [&str; 2] = ["HIGH", "MEDIUM"];

/// The improvement plan one choice of a document under test declares: the text it displays
/// and the URL it points at, either of which a document may leave out.
#[derive(Debug, Clone, Copy)]
struct Plan {
    display_text: Option<&'static str>,
    url: Option<&'static str>,
}

/// One row: the document's choices (one `Plan` each), the positions of the choices the
/// answer records as in place, and what the summary must then report.
struct Row {
    case: &'static str,
    choices: &'static [Plan],
    /// Which choices of the question the answer selects, by position in the document's own
    /// choice order. Every other choice is one the answer leaves out.
    selected: &'static [usize],
}

/// A plan with text and a URL.
const fn plan(display_text: &'static str, url: &'static str) -> Plan {
    Plan {
        display_text: Some(display_text),
        url: Some(url),
    }
}

/// A plan the document declares without a URL: `url` is optional in the lens JSON format
/// specification, and so is `ChoiceImprovementPlan$ImprovementPlanUrl`.
const fn text_only(display_text: &'static str) -> Plan {
    Plan {
        display_text: Some(display_text),
        url: None,
    }
}

/// A choice the document declares no improvement plan on at all.
const NO_PLAN: Plan = Plan {
    display_text: None,
    url: None,
};

const ROWS: &[Row] = &[
    Row {
        case: "two-choices-one-left-out",
        choices: &[
            plan("Tag every account", "https://example.invalid/tagging"),
            plan("Review the tags monthly", "https://example.invalid/review"),
        ],
        selected: &[0],
    },
    Row {
        case: "three-choices-two-left-out",
        choices: &[
            plan("Write the policy down", "https://example.invalid/policy"),
            text_only("Train the on-call rotation"),
            plan("Audit the policy", "https://example.invalid/audit"),
        ],
        selected: &[1],
    },
    Row {
        case: "the-last-choice-is-the-one-in-place",
        choices: &[
            text_only("Record the decision"),
            text_only("Publish the decision"),
            plan("Revisit the decision", "https://example.invalid/revisit"),
        ],
        selected: &[2],
    },
    Row {
        case: "plan-declared-on-only-one-of-the-choices-left-out",
        choices: &[
            text_only("Start with the smallest change"),
            NO_PLAN,
            plan("Measure the result", "https://example.invalid/measure"),
        ],
        selected: &[0],
    },
    Row {
        case: "no-choice-declares-a-plan",
        choices: &[NO_PLAN, NO_PLAN],
        selected: &[0],
    },
];

/// A lens document defining ONE pillar with ONE question whose choices carry `choices`'
/// improvement plans, following the lens JSON format specification: the `schemaVersion` the
/// import requires, a per-choice `improvementPlan` object with its `displayText` and
/// optional `url`, and risk rules with the `default` condition on the last
/// (`super::custom_lens_import::authored_risk_rules`).
fn document(name: &str, choices: &[Plan]) -> Value {
    let choice_ids = choice_ids(choices.len());
    let choices_json: Vec<Value> = choice_ids
        .iter()
        .zip(choices)
        .map(|(choice_id, plan)| {
            let mut choice = json!({ "id": choice_id, "title": format!("Choice {choice_id}") });
            if let Some(display_text) = plan.display_text {
                let mut improvement_plan = json!({ "displayText": display_text });
                if let Some(url) = plan.url {
                    improvement_plan["url"] = json!(url);
                }
                choice["improvementPlan"] = improvement_plan;
            }
            choice
        })
        .collect();
    let rules = super::custom_lens_import::authored_risk_rules(&choice_ids);
    json!({
        "schemaVersion": SCHEMA_VERSION,
        "name": name,
        "pillars": [{
            "id": PILLAR_ID,
            "name": "Pillar governance",
            "questions": [{
                "id": QUESTION_ID,
                "title": "How is the policy governed?",
                "choices": choices_json,
                "riskRules": rules,
            }],
        }],
    })
}

/// The `ChoiceId`s a document of `count` choices declares, in the document's own order.
fn choice_ids(count: usize) -> Vec<String> {
    (0..count)
        .map(|position| format!("{QUESTION_ID}_c{position}"))
        .collect()
}

/// Import a document and publish version `1.0` of it, so it can be applied to a workload:
/// "The lens cannot be applied to workloads … until it's published with CreateLensVersion"
/// (`ImportLens` doc).
async fn published_lens(client: &Client, case: &str, choices: &[Plan]) -> String {
    let body = document(&format!("Lens {case}"), choices).to_string();
    let lens_arn = imported_arn(
        &import(client, &body, None, &format!("tok-import-{case}"), None).await,
        case,
    );
    let published = create_version(
        client,
        &lens_arn,
        "1.0",
        Some(true),
        &format!("tok-publish-{case}"),
    )
    .await;
    assert_eq!(
        published.status, 200,
        "case {case:?}: CreateLensVersion must publish the imported lens, got {published:?}"
    );
    lens_arn
}

/// A workload reviewed against `lens`, and its `WorkloadId`.
async fn workload_reviewing(client: &Client, case: &str, lens: &str) -> String {
    let mut request = minimal_create(&format!("wl-{case}"), &format!("tok-wl-{case}"));
    request["Lenses"] = json!([lens]);
    created_id(&client.create(request).await, case)
}

/// `UpdateAnswer` on the one question the document defines.
async fn update_answer(
    client: &Client,
    workload_id: &str,
    lens: &str,
    selected: &[String],
) -> Answer {
    client
        .send(
            "PATCH",
            &format!(
                "/workloads/{}/lensReviews/{}/answers/{}",
                encode_path_segment(workload_id),
                encode_path_segment(lens),
                encode_path_segment(QUESTION_ID)
            ),
            Some(json!({ "SelectedChoices": selected })),
        )
        .await
}

/// `ListLensReviewImprovements` for one lens review, in one page.
async fn list_improvements(client: &Client, case: &str, workload_id: &str, lens: &str) -> Vec<Value> {
    let answer = client
        .send(
            "GET",
            &format!(
                "/workloads/{}/lensReviews/{}/improvements",
                encode_path_segment(workload_id),
                encode_path_segment(lens)
            ),
            None,
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected ListLensReviewImprovements 200, got {answer:?}"
    );
    answer
        .body
        .get("ImprovementSummaries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// `GetAnswer` on the one question the document defines.
async fn get_answer(client: &Client, case: &str, workload_id: &str, lens: &str) -> Value {
    let answer = client
        .send(
            "GET",
            &format!(
                "/workloads/{}/lensReviews/{}/answers/{}",
                encode_path_segment(workload_id),
                encode_path_segment(lens),
                encode_path_segment(QUESTION_ID)
            ),
            None,
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected GetAnswer 200, got {answer:?}"
    );
    answer
        .body
        .get("Answer")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the read must carry an Answer, got {answer:?}"))
}

fn string_member(value: &Value, name: &str) -> Option<String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// One reported `ChoiceImprovementPlan`, as `(ChoiceId, DisplayText, ImprovementPlanUrl)`.
type ReportedPlan = (Option<String>, Option<String>, Option<String>);

fn reported_plans(summary: &Value) -> Option<Vec<ReportedPlan>> {
    Some(
        summary
            .get("ImprovementPlans")?
            .as_array()?
            .iter()
            .map(|entry| {
                (
                    string_member(entry, "ChoiceId"),
                    string_member(entry, "DisplayText"),
                    string_member(entry, "ImprovementPlanUrl"),
                )
            })
            .collect(),
    )
}

/// Rule: the improvement summary of an at-risk question of a CUSTOM lens review reports one
/// per-choice improvement plan per choice the answer leaves out, with the detail the lens
/// document declares for that choice — and no plan member at all when the document declares
/// none.
#[tokio::test]
async fn an_improvement_summary_reports_the_declared_plan_of_each_choice_left_out() {
    let client = Client::new();

    for row in ROWS {
        let Row {
            case,
            choices,
            selected,
        } = row;
        let lens_arn = published_lens(&client, case, choices).await;
        let workload_id = workload_reviewing(&client, case, &lens_arn).await;
        let ids = choice_ids(choices.len());
        let in_place: Vec<String> = selected
            .iter()
            .map(|position| ids[*position].clone())
            .collect();

        let written = update_answer(&client, &workload_id, &lens_arn, &in_place).await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: the question the lens defines must be answerable, got {written:?}"
        );

        // The question's own choice catalog reports the per-choice improvement plan the
        // document declares, which is where a client reads the plan of a choice that is
        // already in place.
        let answer = get_answer(&client, case, &workload_id, &lens_arn).await;
        let catalog = answer
            .get("Choices")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            catalog.len(),
            choices.len(),
            "case {case:?}: the catalog must offer the document's choices, got {answer:?}"
        );
        for ((choice, declared), choice_id) in catalog.iter().zip(choices.iter()).zip(&ids) {
            let reported = choice.get("ImprovementPlan").cloned();
            let expected = declared.display_text.map(|display_text| {
                let mut content = json!({ "DisplayText": display_text });
                if let Some(url) = declared.url {
                    content["Url"] = json!(url);
                }
                content
            });
            assert_eq!(
                reported, expected,
                "case {case:?}: choice {choice_id:?} must report the improvement plan its \
                 document declares, expected {expected:?}, got {reported:?}"
            );
        }

        // The answer leaves a best practice out, so the review identified a risk in the
        // question and the plan carries it.
        let risk = string_member(&answer, "Risk").unwrap_or_default();
        assert!(
            IDENTIFIED_RISKS.contains(&risk.as_str()),
            "case {case:?}: an answer that leaves a best practice out identifies a risk, \
             got {risk:?}"
        );
        let summaries = list_improvements(&client, case, &workload_id, &lens_arn).await;
        assert_eq!(
            summaries.len(),
            1,
            "case {case:?}: the one at-risk question is the whole plan, got {summaries:?}"
        );
        let summary = &summaries[0];
        assert_eq!(
            string_member(summary, "QuestionId"),
            Some(QUESTION_ID.to_string()),
            "case {case:?}: the summary must name the at-risk question, got {summary:?}"
        );
        assert_eq!(
            string_member(summary, "PillarId"),
            Some(PILLAR_ID.to_string()),
            "case {case:?}: the summary must name the pillar that asks it, got {summary:?}"
        );

        // One entry per choice the answer leaves out, in the document's own choice order,
        // each carrying what that choice declares — and the member left off entirely when
        // no choice left out declares a plan.
        let left_out: Vec<(usize, &Plan)> = choices
            .iter()
            .enumerate()
            .filter(|(position, _)| !selected.contains(position))
            .collect();
        let expected: Option<Vec<ReportedPlan>> = left_out
            .iter()
            .any(|(_, plan)| plan.display_text.is_some())
            .then(|| {
                left_out
                    .iter()
                    .map(|(position, plan)| {
                        (
                            Some(ids[*position].clone()),
                            plan.display_text.map(ToString::to_string),
                            plan.url.map(ToString::to_string),
                        )
                    })
                    .collect()
            });
        let actual = reported_plans(summary);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the improvement plans {expected:?}, got {summary:?}"
        );
    }
}

/// Rule, on the other side of the selection: a question whose answer leaves NO best
/// practice out is rated `NONE`, so it is no improvement item at all and no plan detail is
/// reported for it — the plan details follow the ANSWER, not the document.
#[tokio::test]
async fn a_question_with_nothing_left_out_is_not_an_improvement_item() {
    let client = Client::new();
    let choices: &[Plan] = &[
        plan("Tag every account", "https://example.invalid/tagging"),
        text_only("Review the tags monthly"),
    ];
    let case = "nothing-left-out";
    let lens_arn = published_lens(&client, case, choices).await;
    let workload_id = workload_reviewing(&client, case, &lens_arn).await;
    let ids = choice_ids(choices.len());

    // Every best practice in place: nothing is left to improve.
    let written = update_answer(&client, &workload_id, &lens_arn, &ids).await;
    assert_eq!(
        written.status, 200,
        "case {case:?}: the question must be answerable, got {written:?}"
    );
    let answer = get_answer(&client, case, &workload_id, &lens_arn).await;
    assert_eq!(
        string_member(&answer, "Risk"),
        Some("NONE".to_string()),
        "case {case:?}: an answer that leaves nothing out is rated NONE, got {answer:?}"
    );
    let summaries = list_improvements(&client, case, &workload_id, &lens_arn).await;
    assert!(
        summaries.is_empty(),
        "case {case:?}: a question with nothing outstanding is no improvement item, got \
         {summaries:?}"
    );

    // Dropping one choice from the selection puts the question back in the plan, with the
    // plan detail of exactly the choice that was dropped.
    let still_in_place = vec![ids[0].clone()];
    let rewritten = update_answer(&client, &workload_id, &lens_arn, &still_in_place).await;
    assert_eq!(
        rewritten.status, 200,
        "case {case:?}: the answer must be correctable, got {rewritten:?}"
    );
    let summaries = list_improvements(&client, case, &workload_id, &lens_arn).await;
    let actual = summaries.first().and_then(reported_plans);
    let expected = Some(vec![(
        Some(ids[1].clone()),
        Some("Review the tags monthly".to_string()),
        None,
    )]);
    assert_eq!(
        actual, expected,
        "case {case:?}: the plan must carry the choice the answer stopped selecting, \
         expected {expected:?}, got {summaries:?}"
    );
}
