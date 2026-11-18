// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for WRITING the answers of a lens review when the lens under
//! review is a custom lens the caller imported: `UpdateAnswer` on a workload's review and
//! `UpdateReviewTemplateAnswer` on a review template's.
//!
//! The rule these pin, over a table of lens documents rather than one recorded request: the
//! questions of a lens review are the ones the LENS's own content defines — the AWS-official
//! catalog entry for an official lens, the published imported document for a custom one —
//! so every question a read of the review reports can be answered, and only a `QuestionId`
//! that lens does not ask names no answer to write. The question each case names is
//! discovered the way a client discovers it, from the review's own answer list, so a write
//! is judged against exactly what the read served.
//!
//! The rows differ along the SHAPE of the document — how many pillars it defines, how many
//! questions each pillar asks and how many choices each question offers — and, per
//! question, along WHICH of its choices the answer selects, because both are what the
//! recorded answer and the risk derived from it move with. The AWS-official half of the
//! catalog is written through the same resolution, which the official-lens test here pins.
//!
//! The second rule pinned here is how a custom lens review GRADES such an answer: by the
//! `riskRules` the lens document authored for that question, evaluated in order over the
//! choices the answer records as in place
//! (`an_answer_to_a_custom_lens_question_is_graded_by_the_authored_risk_rules`), whose rows
//! differ along the form of the authored conditions and the risks they assign.
//!
//! Requests go through the real `MultiServiceRouter`, so routing, restJson1
//! (de)serialization, boundary constraint validation and the handler all run as they do in
//! production. The HTTP client is the shared `super::workload_harness`; the import and
//! publish wire helpers are the ones `super::custom_lens_import` and
//! `super::custom_lens_versions` already spell.

use serde_json::{json, Value};

use super::custom_lens_import::{import, imported_arn, lens_document, wait_for_the_import_to_land};
use super::custom_lens_versions::create_version;
use super::workload_harness::{
    assert_field_validation, created_id, encode_path_segment, member, minimal_create,
    review_template, Answer, Client,
};

/// The shape of one lens document: `(pillar id, questions in it, choices per question)`,
/// exactly what `super::custom_lens_import::lens_document` builds a document from.
type Shape = &'static [(&'static str, usize, usize)];

/// The alias of the AWS-official lens this emulator ships a question set for, and one
/// question of it: the published Well-Architected Framework numbering, as the emulator's
/// own ids spell it (`tests/all/answer_updates.rs` answers the same question).
const FRAMEWORK_LENS: &str = "wellarchitected";
const FRAMEWORK_QUESTION: &str = "organization-priorities";

/// The documents under test: the rows differ along the number of pillars, the number of
/// questions a pillar asks and the number of choices a question offers — including a
/// question that offers exactly ONE choice, where "every choice" and "the first choice"
/// are the same selection and "all but the last" is none of them.
const SHAPES: &[(&str, Shape)] = &[
    ("one-question-one-choice", &[("governance", 1, 1)]),
    ("one-pillar-many-questions", &[("security", 2, 3)]),
    ("several-pillars", &[("operations", 1, 4), ("cost", 2, 2)]),
];

/// Which of a question's choices an answer selects. The plans span the risk the service
/// derives from a recorded answer: nothing selected, one choice, every choice and all but
/// the last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Selection {
    None,
    First,
    All,
    AllButLast,
}

impl Selection {
    /// The `ChoiceId`s this plan selects out of a question's own catalog, in the catalog's
    /// order.
    fn of(self, choices: &[String]) -> Vec<String> {
        match self {
            Selection::None => Vec::new(),
            Selection::First => choices.iter().take(1).cloned().collect(),
            Selection::All => choices.to_vec(),
            Selection::AllButLast => choices
                .iter()
                .take(choices.len().saturating_sub(1))
                .cloned()
                .collect(),
        }
    }
}

const SELECTIONS: [Selection; 4] = [
    Selection::None,
    Selection::First,
    Selection::All,
    Selection::AllButLast,
];

/// `Choice$Title` of the terminal choice every question of an AWS-official lens offers
/// after its best practices: the answer that none of those best practices are in place.
const NONE_OF_THESE: &str = "None of these";

/// The `Risk` the service derives from an applicable answer to a question of `summary`,
/// over the choices `selected` names.
///
/// A question whose answer selects none of its choices is still `UNANSWERED` — the
/// selection is what answers it. Beyond that the LENS's own rating decides: a lens that
/// publishes risk rules for its questions, as every AWS-official lens does, clears a
/// question only when every best practice is in place, reduces it to `MEDIUM` while at
/// least half of them are, and rates every thinner selection — and the terminal
/// [`NONE_OF_THESE`] choice, which says none of them are in place — at its default `HIGH`;
/// a lens whose content declares no rules this service evaluates rates an answer with some
/// choice still unselected `MEDIUM` and one with nothing left unselected `NONE`
/// (`smithy_query(query="shape", shape="Risk")` publishes the value set).
fn expected_risk(summary: &Value, selected: &[String]) -> &'static str {
    let best_practices = best_practice_ids(summary);
    let in_place = selected
        .iter()
        .filter(|choice_id| best_practices.contains(choice_id))
        .count();
    let none_of_these = selected.len() > in_place;
    let published_risk_rules = best_practices.len() < choice_ids(summary).len();
    if selected.is_empty() {
        "UNANSWERED"
    } else if none_of_these || in_place == 0 {
        "HIGH"
    } else if in_place == best_practices.len() {
        "NONE"
    } else if !published_risk_rules || in_place * 2 >= best_practices.len() {
        "MEDIUM"
    } else {
        "HIGH"
    }
}

/// `UpdateAnswer`: `PATCH /workloads/{WorkloadId}/lensReviews/{LensAlias}/answers/{QuestionId}`.
/// Each label is non-greedy, so it travels percent-encoded in one path segment — which is
/// what carries a custom lens's ARN, with its `:` and `/`, as one segment.
async fn update_answer(
    client: &Client,
    workload_id: &str,
    lens: &str,
    question_id: &str,
    body: Value,
) -> Answer {
    client
        .send(
            "PATCH",
            &format!(
                "/workloads/{}/lensReviews/{}/answers/{}",
                encode_path_segment(workload_id),
                encode_path_segment(lens),
                encode_path_segment(question_id)
            ),
            Some(body),
        )
        .await
}

/// `GetAnswer`: the same path read with `GET`.
async fn get_answer(client: &Client, workload_id: &str, lens: &str, question_id: &str) -> Answer {
    client
        .send(
            "GET",
            &format!(
                "/workloads/{}/lensReviews/{}/answers/{}",
                encode_path_segment(workload_id),
                encode_path_segment(lens),
                encode_path_segment(question_id)
            ),
            None,
        )
        .await
}

/// `ListAnswers` for one lens review, in one page: the question set as a CLIENT discovers
/// it before writing to it.
async fn listed_answers(client: &Client, case: &str, workload_id: &str, lens: &str) -> Vec<Value> {
    let answer = client
        .send(
            "GET",
            &format!(
                "/workloads/{}/lensReviews/{}/answers",
                encode_path_segment(workload_id),
                encode_path_segment(lens)
            ),
            None,
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: the review's answers must be readable, got {answer:?}"
    );
    answer
        .body
        .get("AnswerSummaries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Import a document of `shape` as a fresh custom lens and answer its ARN. The caller waits
/// for the import to land and publishes the version itself, so a table can import every
/// row's lens before waiting once.
async fn import_shape(client: &Client, case: &str, shape: Shape) -> String {
    let document = lens_document(&format!("Lens {case}"), None, shape).to_string();
    imported_arn(
        &import(client, &document, None, &format!("tok-import-{case}"), None).await,
        case,
    )
}

/// Publish version `1.0` of an imported lens, so it can be applied to a workload or a
/// review template: "The lens cannot be applied to workloads … until it's published with
/// CreateLensVersion" (`ImportLens` doc).
async fn publish(client: &Client, case: &str, lens_arn: &str) {
    let published = create_version(
        client,
        lens_arn,
        "1.0",
        Some(true),
        &format!("tok-publish-{case}"),
    )
    .await;
    assert_eq!(
        published.status, 200,
        "case {case:?}: CreateLensVersion must publish the imported lens, got {published:?}"
    );
}

/// A workload reviewed against `lens`, and its `WorkloadId`.
async fn workload_reviewing(client: &Client, case: &str, lens: &str) -> String {
    let mut request = minimal_create(&format!("wl-{case}"), &format!("tok-wl-{case}"));
    request["Lenses"] = json!([lens]);
    created_id(&client.create(request).await, case)
}

/// Import, land and publish ONE custom lens of `shape`, and answer its ARN.
async fn published_lens(client: &Client, case: &str, shape: Shape) -> String {
    let lens_arn = import_shape(client, case, shape).await;
    wait_for_the_import_to_land();
    publish(client, case, &lens_arn).await;
    lens_arn
}

fn string_member(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `ChoiceId`s a question's own catalog (`Choices`) offers, in the order served — the
/// only place a client discovers them.
fn choice_ids(value: &Value) -> Vec<String> {
    value
        .get("Choices")
        .and_then(Value::as_array)
        .map(|choices| {
            choices
                .iter()
                .map(|choice| string_member(choice, "ChoiceId"))
                .collect()
        })
        .unwrap_or_default()
}

/// The `ChoiceId`s of the BEST PRACTICES a question asks about: its catalog
/// ([`choice_ids`]) less the terminal [`NONE_OF_THESE`] choice, which is the answer that
/// none of them are in place rather than one of them.
fn best_practice_ids(value: &Value) -> Vec<String> {
    value
        .get("Choices")
        .and_then(Value::as_array)
        .map(|choices| {
            choices
                .iter()
                .filter(|choice| string_member(choice, "Title") != NONE_OF_THESE)
                .map(|choice| string_member(choice, "ChoiceId"))
                .collect()
        })
        .unwrap_or_default()
}

/// The `Answer` / `ReviewTemplateAnswer` a read or a write reported.
fn answer_of(answer: &Answer) -> Value {
    answer.body.get("Answer").cloned().unwrap_or(Value::Null)
}

/// One list-of-strings member of a JSON object, empty when it is absent.
fn strings(value: &Value, name: &str) -> Vec<String> {
    value
        .get(name)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|entry| entry.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The per-choice statuses an answer records, as `(ChoiceId, Status)` sorted by id.
fn choice_statuses(answer: &Value, member_name: &str) -> Vec<(String, String)> {
    let mut statuses: Vec<(String, String)> = answer
        .get(member_name)
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    (
                        string_member(entry, "ChoiceId"),
                        string_member(entry, "Status"),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    statuses.sort();
    statuses
}

/// The answer a question of `summary` must report once `selected` has been recorded against
/// it: the question as the LENS defines it, the choices in place in the lens's own choice
/// order, a status for every choice of the question and the derived risk.
fn assert_recorded(case: &str, answer: &Value, summary: &Value, selected: &[String], notes: &str) {
    let choices = choice_ids(summary);
    assert_eq!(
        string_member(answer, "QuestionId"),
        string_member(summary, "QuestionId"),
        "case {case:?}: the answer must name the question it was written for, got {answer:?}"
    );
    assert_eq!(
        string_member(answer, "PillarId"),
        string_member(summary, "PillarId"),
        "case {case:?}: the answer must carry the question's pillar, got {answer:?}"
    );
    assert_eq!(
        string_member(answer, "QuestionTitle"),
        string_member(summary, "QuestionTitle"),
        "case {case:?}: the answer must carry the question's title, got {answer:?}"
    );
    assert_eq!(
        choice_ids(answer),
        choices,
        "case {case:?}: the answer must report the choice catalog its lens defines, got {answer:?}"
    );
    assert_eq!(
        strings(answer, "SelectedChoices"),
        selected,
        "case {case:?}: the choices in place are the ones the request named, in the lens's own \
         choice order, got {answer:?}"
    );
    let expected_statuses: Vec<(String, String)> = {
        let mut statuses: Vec<(String, String)> = choices
            .iter()
            .filter(|choice_id| selected.contains(choice_id))
            .map(|choice_id| (choice_id.clone(), "SELECTED".to_string()))
            .collect();
        statuses.sort();
        statuses
    };
    assert_eq!(
        choice_statuses(answer, "ChoiceAnswers"),
        expected_statuses,
        "case {case:?}: the answer must report one entry per choice it answers, got {answer:?}"
    );
    assert_eq!(
        answer.get("Risk").and_then(Value::as_str),
        Some(expected_risk(summary, selected)),
        "case {case:?}: the risk is derived from the choices in place, got {answer:?}"
    );
    assert_eq!(
        string_member(answer, "Notes"),
        notes,
        "case {case:?}: the answer must carry the notes the request wrote, got {answer:?}"
    );
    assert_eq!(
        answer.get("IsApplicable"),
        Some(&Value::Bool(true)),
        "case {case:?}: a question applies until an answer says otherwise, got {answer:?}"
    );
}

/// Every question a published custom lens's document defines is answerable: the write is
/// accepted, records exactly the choices it named, and reads back through `GetAnswer`.
#[tokio::test]
async fn every_question_an_imported_lens_defines_can_be_answered() {
    let client = Client::new();

    // Every row's lens is imported first, so the whole table waits for the import window
    // once rather than per row.
    let mut lenses: Vec<(&str, String)> = Vec::new();
    for (case, shape) in SHAPES {
        lenses.push((case, import_shape(&client, case, shape).await));
    }
    wait_for_the_import_to_land();

    for (case, lens_arn) in &lenses {
        publish(&client, case, lens_arn).await;
        let workload_id = workload_reviewing(&client, case, lens_arn).await;

        // The question set as a CLIENT discovers it: the answer list of the review it is
        // about to write to.
        let summaries = listed_answers(&client, case, &workload_id, lens_arn).await;
        assert!(
            !summaries.is_empty(),
            "case {case:?}: the review must report the questions its lens defines, got \
             {summaries:?}"
        );

        for (position, summary) in summaries.iter().enumerate() {
            let question_id = string_member(summary, "QuestionId");
            let choices = choice_ids(summary);
            let selection = SELECTIONS[position % SELECTIONS.len()];
            let selected = selection.of(&choices);
            let case = format!("{case}-{question_id}-{selection:?}");
            let notes = format!("notes for {question_id}");

            // The request lists the choices in the REVERSE of the catalog's order: the
            // choices in place are a set, so what is reported back is the lens's order
            // whatever order they were sent in.
            let mut sent: Vec<String> = selected.clone();
            sent.reverse();
            let written = update_answer(
                &client,
                &workload_id,
                lens_arn,
                &question_id,
                json!({ "SelectedChoices": sent, "Notes": notes }),
            )
            .await;
            assert_eq!(
                written.status, 200,
                "case {case:?}: a question the lens defines must be answerable, got {written:?}"
            );
            assert_eq!(
                member(&written, "WorkloadId"),
                workload_id,
                "case {case:?}: the envelope must name the workload, got {written:?}"
            );
            assert_eq!(
                member(&written, "LensAlias"),
                lens_arn.as_str(),
                "case {case:?}: the envelope must name the lens as sent, got {written:?}"
            );
            assert_eq!(
                member(&written, "LensArn"),
                lens_arn.as_str(),
                "case {case:?}: the envelope must carry the lens ARN, got {written:?}"
            );
            assert_recorded(&case, &answer_of(&written), summary, &selected, &notes);

            // The write is persisted, not just echoed: the single-answer read reports it.
            let read = get_answer(&client, &workload_id, lens_arn, &question_id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: the recorded answer must read back, got {read:?}"
            );
            assert_recorded(
                &format!("{case}-read-back"),
                &answer_of(&read),
                summary,
                &selected,
                &notes,
            );
        }

        // The review's own answer list moves with the writes: every question whose row
        // selected a best practice is rated by it, and the rows that selected NONE leave
        // their question unanswered — the selection is what answers a question.
        for (position, summary) in listed_answers(&client, case, &workload_id, lens_arn)
            .await
            .iter()
            .enumerate()
        {
            let selection = SELECTIONS[position % SELECTIONS.len()];
            let expected_unanswered = selection.of(&choice_ids(summary)).is_empty();
            assert_eq!(
                summary.get("Risk").and_then(Value::as_str) == Some("UNANSWERED"),
                expected_unanswered,
                "case {case:?}: a question is UNANSWERED exactly while its answer selects \
                 nothing ({selection:?}), got {summary:?}"
            );
        }
    }
}

/// An answer to a custom lens's question records every member the request carries:
/// `ChoiceUpdates` writes one choice's status, reason and notes, and `IsApplicable` with a
/// `Reason` takes the question out of scope.
#[tokio::test]
async fn an_answer_to_an_imported_lenss_question_records_every_member() {
    const SHAPE: Shape = &[("governance", 2, 3)];
    // `ChoiceStatus` publishes SELECTED | UNSELECTED | NOT_APPLICABLE and `ChoiceReason`
    // the five reasons (`smithy_query(query="shape", shape="ChoiceStatus")` /
    // `shape="ChoiceReason"`); the rows below span the statuses and three of the reasons.
    const UPDATES: [(&str, &str); 3] = [
        ("SELECTED", "NONE"),
        ("UNSELECTED", "BUSINESS_PRIORITIES"),
        ("NOT_APPLICABLE", "OUT_OF_SCOPE"),
    ];

    let client = Client::new();
    let lens_arn = published_lens(&client, "members", SHAPE).await;
    let workload_id = workload_reviewing(&client, "members", &lens_arn).await;

    let summaries = listed_answers(&client, "members", &workload_id, &lens_arn).await;
    let summary = summaries
        .first()
        .cloned()
        .expect("the review must report the questions its lens defines");
    let question_id = string_member(&summary, "QuestionId");
    let choices = choice_ids(&summary);

    let mut choice_updates = serde_json::Map::new();
    let mut expected_statuses: Vec<(String, String)> = Vec::new();
    for (choice_id, (status, reason)) in choices.iter().zip(UPDATES) {
        choice_updates.insert(
            choice_id.clone(),
            json!({ "Status": status, "Reason": reason, "Notes": format!("why {choice_id}") }),
        );
        // An answer reports the choices it ANSWERS, so a choice this walk records
        // UNSELECTED for is not one of them.
        if status != "UNSELECTED" {
            expected_statuses.push((choice_id.clone(), status.to_string()));
        }
    }
    expected_statuses.sort();

    let written = update_answer(
        &client,
        &workload_id,
        &lens_arn,
        &question_id,
        json!({
            "ChoiceUpdates": Value::Object(choice_updates),
            "Notes": "the answer's own notes",
            "IsApplicable": false,
            "Reason": "OUT_OF_SCOPE",
        }),
    )
    .await;
    assert_eq!(
        written.status, 200,
        "case \"members\": the question the lens defines must be answerable, got {written:?}"
    );

    // Each member is read back through the single-answer read, so what is asserted is what
    // the store holds rather than what the write echoed.
    let read = get_answer(&client, &workload_id, &lens_arn, &question_id).await;
    assert_eq!(
        read.status, 200,
        "case \"members\": the recorded answer must read back, got {read:?}"
    );
    for (case, answer) in [
        ("members-written", answer_of(&written)),
        ("members-read-back", answer_of(&read)),
    ] {
        assert_eq!(
            choice_statuses(&answer, "ChoiceAnswers"),
            expected_statuses,
            "case {case:?}: each ChoiceUpdates entry must record its choice's status, got \
             {answer:?}"
        );
        assert_eq!(
            strings(&answer, "SelectedChoices"),
            vec![choices[0].clone()],
            "case {case:?}: the choices in place are the ones recorded SELECTED, got {answer:?}"
        );
        assert_eq!(
            answer.get("IsApplicable"),
            Some(&Value::Bool(false)),
            "case {case:?}: the question must record that it does not apply, got {answer:?}"
        );
        assert_eq!(
            string_member(&answer, "Reason"),
            "OUT_OF_SCOPE",
            "case {case:?}: the question must record why it does not apply, got {answer:?}"
        );
        assert_eq!(
            string_member(&answer, "Notes"),
            "the answer's own notes",
            "case {case:?}: the answer must carry its own notes, got {answer:?}"
        );
        assert_eq!(
            answer.get("Risk").and_then(Value::as_str),
            Some("NOT_APPLICABLE"),
            "case {case:?}: a question that does not apply carries no risk, got {answer:?}"
        );
    }
}

/// What a write to a custom lens's review still refuses: a `QuestionId` that lens's
/// document does not define — including one another lens does — is the modelled 400 naming
/// `QuestionId`, and a `ChoiceId`
/// that is not a choice of the question being answered is a 400; neither records anything.
#[tokio::test]
async fn a_question_an_imported_lens_does_not_define_is_refused() {
    const SHAPE: Shape = &[("governance", 2, 2)];
    const OTHER_SHAPE: Shape = &[("elsewhere", 1, 1)];

    let client = Client::new();
    let lens_arn = import_shape(&client, "refusals", SHAPE).await;
    let other_arn = import_shape(&client, "refusals-other", OTHER_SHAPE).await;
    wait_for_the_import_to_land();
    publish(&client, "refusals", &lens_arn).await;
    publish(&client, "refusals-other", &other_arn).await;
    let workload_id = workload_reviewing(&client, "refusals", &lens_arn).await;

    let summaries = listed_answers(&client, "refusals", &workload_id, &lens_arn).await;
    let answered = summaries
        .first()
        .cloned()
        .expect("the review must report the questions its lens defines");
    let question_id = string_member(&answered, "QuestionId");
    let other_question = string_member(
        &listed_answers(
            &client,
            "refusals-other",
            &workload_reviewing(&client, "refusals-other", &other_arn).await,
            &other_arn,
        )
        .await
        .first()
        .cloned()
        .expect("the other lens's review must report its own questions"),
        "QuestionId",
    );

    // A `QuestionId` of no lens at all, of the AWS-OFFICIAL catalog, and of ANOTHER custom
    // lens's document: none of them is a question the lens under review asks.
    for unasked in ["not-a-question", FRAMEWORK_QUESTION, other_question.as_str()] {
        let refused = update_answer(
            &client,
            &workload_id,
            &lens_arn,
            unasked,
            json!({ "Notes": "nothing to record" }),
        )
        .await;
        let case = format!("unasked-{unasked}");
        let sentence =
            format!("No question with ID {unasked} was found in the lens with alias {lens_arn}.");
        assert_field_validation(&case, &refused, "QuestionId", &sentence);
        let read = get_answer(&client, &workload_id, &lens_arn, unasked).await;
        assert_field_validation(&format!("{case}-read"), &read, "QuestionId", &sentence);
    }

    // A `ChoiceId` of ANOTHER question of the same document is not a choice of the question
    // being answered.
    let foreign_choice = choice_ids(
        summaries
            .last()
            .expect("the document defines more than one question"),
    )
    .first()
    .cloned()
    .expect("every question of the document offers a choice");
    for (case, body) in [
        (
            "selected-choices",
            json!({ "SelectedChoices": [foreign_choice.clone()] }),
        ),
        (
            "choice-updates",
            json!({ "ChoiceUpdates": { foreign_choice.clone(): { "Status": "SELECTED" } } }),
        ),
    ] {
        let refused = update_answer(&client, &workload_id, &lens_arn, &question_id, body).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: a choice of another question belongs to no answer of this one, got \
             {refused:?}"
        );
        assert_eq!(
            refused.error_code, "ValidationException",
            "case {case:?}: expected ValidationException, got {refused:?}"
        );
    }
    let unchanged = get_answer(&client, &workload_id, &lens_arn, &question_id).await;
    assert_eq!(
        answer_of(&unchanged).get("Risk").and_then(Value::as_str),
        Some("UNANSWERED"),
        "a refused write must record nothing, got {unchanged:?}"
    );
}

/// The AWS-official half of the lens catalog is written through the same resolution: a
/// question of the framework lens still records its answer and reads back.
#[tokio::test]
async fn an_official_lenss_question_is_still_answerable() {
    let client = Client::new();
    let workload_id = created_id(
        &client
            .create(minimal_create("wl-official-half", "tok-wl-official-half"))
            .await,
        "official-half",
    );

    // The question's own catalog, as a client discovers it.
    let summary = answer_of(
        &get_answer(&client, &workload_id, FRAMEWORK_LENS, FRAMEWORK_QUESTION).await,
    );
    let choices = choice_ids(&summary);
    assert!(
        !choices.is_empty(),
        "the framework lens's question must offer its best practices, got {summary:?}"
    );

    for selection in SELECTIONS {
        let selected = selection.of(&choices);
        let case = format!("official-{selection:?}");
        let notes = format!("notes for {case}");
        let written = update_answer(
            &client,
            &workload_id,
            FRAMEWORK_LENS,
            FRAMEWORK_QUESTION,
            json!({ "SelectedChoices": selected, "Notes": notes }),
        )
        .await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: a question of the official catalog must stay answerable, got \
             {written:?}"
        );
        assert_recorded(&case, &answer_of(&written), &summary, &selected, &notes);
        let read = get_answer(&client, &workload_id, FRAMEWORK_LENS, FRAMEWORK_QUESTION).await;
        assert_recorded(
            &format!("{case}-read-back"),
            &answer_of(&read),
            &summary,
            &selected,
            &notes,
        );
    }
}

/// A review template's review of a published custom lens is answerable by the same rule:
/// `UpdateReviewTemplateAnswer` records the answer to any question the document defines and
/// `GetReviewTemplateAnswer` reads it back.
#[tokio::test]
async fn a_review_templates_imported_lens_review_can_be_answered() {
    const SHAPE: Shape = &[("governance", 1, 2), ("cost", 2, 1)];

    let client = Client::new();
    let lens_arn = published_lens(&client, "template", SHAPE).await;
    let template_arn = review_template(&client, "tmpl-imported-answers", &[&lens_arn]).await;

    let listed = client
        .list_review_template_answers(&template_arn, &lens_arn, "")
        .await;
    assert_eq!(
        listed.status, 200,
        "the template's lens review must be readable, got {listed:?}"
    );
    let summaries = listed
        .body
        .get("AnswerSummaries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        !summaries.is_empty(),
        "the template's review must report the questions its lens defines, got {listed:?}"
    );

    for (position, summary) in summaries.iter().enumerate() {
        let question_id = string_member(summary, "QuestionId");
        let choices = choice_ids(summary);
        let selection = SELECTIONS[position % SELECTIONS.len()];
        let selected = selection.of(&choices);
        let case = format!("template-{question_id}-{selection:?}");
        let notes = format!("template notes for {question_id}");

        let written = client
            .update_review_template_answer(
                &template_arn,
                &lens_arn,
                &question_id,
                json!({ "SelectedChoices": selected, "Notes": notes }),
            )
            .await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: a question the lens defines must be answerable, got {written:?}"
        );
        assert_eq!(
            member(&written, "TemplateArn"),
            template_arn.as_str(),
            "case {case:?}: the envelope must name the review template, got {written:?}"
        );
        assert_eq!(
            member(&written, "LensAlias"),
            lens_arn.as_str(),
            "case {case:?}: the envelope must name the lens as sent, got {written:?}"
        );

        let read = client
            .get_review_template_answer(&template_arn, &lens_arn, &question_id)
            .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the recorded answer must read back, got {read:?}"
        );
        for (case, answer) in [
            (format!("{case}-written"), answer_of(&written)),
            (format!("{case}-read-back"), answer_of(&read)),
        ] {
            assert_eq!(
                string_member(&answer, "QuestionId"),
                question_id,
                "case {case:?}: the answer must name the question it was written for, got \
                 {answer:?}"
            );
            assert_eq!(
                choice_ids(&answer),
                choices,
                "case {case:?}: the answer must report the choice catalog its lens defines, got \
                 {answer:?}"
            );
            assert_eq!(
                strings(&answer, "SelectedChoices"),
                selected,
                "case {case:?}: the choices in place are the ones the request named, in the \
                 lens's own choice order, got {answer:?}"
            );
            assert_eq!(
                string_member(&answer, "Notes"),
                notes,
                "case {case:?}: the answer must carry the notes the request wrote, got {answer:?}"
            );
            assert_eq!(
                answer.get("AnswerStatus").and_then(Value::as_str),
                Some("ANSWERED"),
                "case {case:?}: a question the template has answered is ANSWERED, got {answer:?}"
            );
        }
    }

    // A `QuestionId` the lens's document does not define is still no answer of this
    // template's review.
    let refused = client
        .update_review_template_answer(
            &template_arn,
            &lens_arn,
            FRAMEWORK_QUESTION,
            json!({ "Notes": "nothing to record" }),
        )
        .await;
    assert_field_validation(
        "template-unasked",
        &refused,
        "QuestionId",
        &format!(
            "No question with ID {FRAMEWORK_QUESTION} was found in the lens with alias \
             {lens_arn}."
        ),
    );
}

/// One question whose grading is under test: its id, the choices its document offers, the
/// `riskRules` the document AUTHORS for it as `(condition, risk)` pairs in the document's
/// own order, and the selections to answer it with, each with the `Risk` those rules rate
/// that selection at.
///
/// The rows differ along the FORM of the authored conditions (a conjunction, a disjunction,
/// a negation, a parenthesised group, a bare choice id, an id that is not an identifier),
/// along the risks the rules assign (every member of the document's vocabulary, including a
/// `default` rule that CLEARS rather than raises), along how many choices a question offers
/// and along which of them a selection names — because every one of those is what the
/// derivation is a function of.
struct Graded {
    question_id: &'static str,
    choices: &'static [&'static str],
    rules: &'static [(&'static str, &'static str)],
    selections: &'static [(&'static [&'static str], &'static str)],
}

/// The risks a lens document's `riskRules` may assign, from the lens JSON format
/// specification the `ImportLens` doc points at, beside the `Risk` enum member each maps to
/// (`smithy_query(query="shape", shape="Risk")`).
const NO_RISK: &str = "NO_RISK";
const MEDIUM_RISK: &str = "MEDIUM_RISK";
const HIGH_RISK: &str = "HIGH_RISK";

/// The reserved condition the format requires on a question's LAST risk rule: the one that
/// applies when no rule above it matched.
const DEFAULT_CONDITION: &str = "default";

/// The questions whose authored rules this suite grades answers against.
const GRADED_QUESTIONS: &[Graded] = &[
    // A conjunction clears the question, a bare id reduces it and `default` rates what is
    // left — and an empty selection is UNANSWERED before any rule runs, so the `default`
    // rule does not rate it.
    Graded {
        question_id: "conjunction",
        choices: &["conjunction_c1", "conjunction_c2"],
        rules: &[
            ("conjunction_c1 && conjunction_c2", NO_RISK),
            ("conjunction_c1", MEDIUM_RISK),
            (DEFAULT_CONDITION, HIGH_RISK),
        ],
        selections: &[
            (&[], "UNANSWERED"),
            (&["conjunction_c2"], "HIGH"),
            (&["conjunction_c1"], "MEDIUM"),
            (&["conjunction_c1", "conjunction_c2"], "NONE"),
        ],
    },
    // The same rule set, ending on the selection that matches only `default`.
    Graded {
        question_id: "falls-through",
        choices: &["falls-through_c1", "falls-through_c2"],
        rules: &[
            ("falls-through_c1 && falls-through_c2", NO_RISK),
            ("falls-through_c1", MEDIUM_RISK),
            (DEFAULT_CONDITION, HIGH_RISK),
        ],
        selections: &[
            (&["falls-through_c1", "falls-through_c2"], "NONE"),
            (&["falls-through_c1"], "MEDIUM"),
            (&["falls-through_c2"], "HIGH"),
        ],
    },
    Graded {
        question_id: "reduced",
        choices: &["reduced_c1", "reduced_c2"],
        rules: &[
            ("reduced_c1 && reduced_c2", NO_RISK),
            ("reduced_c1", MEDIUM_RISK),
            (DEFAULT_CONDITION, HIGH_RISK),
        ],
        selections: &[(&["reduced_c1"], "MEDIUM")],
    },
    // A disjunction holds for any one of the choices it names.
    Graded {
        question_id: "disjunction",
        choices: &["disjunction_c1", "disjunction_c2"],
        rules: &[
            ("disjunction_c1 || disjunction_c2", NO_RISK),
            (DEFAULT_CONDITION, HIGH_RISK),
        ],
        selections: &[
            (&["disjunction_c1"], "NONE"),
            (&["disjunction_c2"], "NONE"),
            (&[], "UNANSWERED"),
            (&["disjunction_c1", "disjunction_c2"], "NONE"),
        ],
    },
    // A negated id holds while that choice is NOT in place.
    Graded {
        question_id: "negation",
        choices: &["negation_c1", "negation_c2"],
        rules: &[
            ("negation_c1 && !negation_c2", MEDIUM_RISK),
            ("negation_c1 && negation_c2", NO_RISK),
            (DEFAULT_CONDITION, HIGH_RISK),
        ],
        selections: &[
            (&["negation_c1"], "MEDIUM"),
            (&["negation_c1", "negation_c2"], "NONE"),
            (&["negation_c2"], "HIGH"),
        ],
    },
    // Parentheses group, so the condition is not read left to right.
    Graded {
        question_id: "grouped",
        choices: &["grouped_c1", "grouped_c2", "grouped_c3"],
        rules: &[
            ("(grouped_c1 || grouped_c2) && grouped_c3", NO_RISK),
            ("grouped_c3", MEDIUM_RISK),
            (DEFAULT_CONDITION, HIGH_RISK),
        ],
        selections: &[
            (&["grouped_c1", "grouped_c3"], "NONE"),
            (&["grouped_c2", "grouped_c3"], "NONE"),
            (&["grouped_c3"], "MEDIUM"),
            (&["grouped_c1", "grouped_c2"], "HIGH"),
        ],
    },
    // The rules are evaluated IN ORDER, so a selection matching two of them takes the
    // FIRST one's risk: the same two rules in the other order would clear this question.
    Graded {
        question_id: "ordered",
        choices: &["ordered_c1", "ordered_c2"],
        rules: &[
            ("ordered_c1", MEDIUM_RISK),
            ("ordered_c1 && ordered_c2", NO_RISK),
            (DEFAULT_CONDITION, HIGH_RISK),
        ],
        selections: &[
            (&["ordered_c1", "ordered_c2"], "MEDIUM"),
            (&["ordered_c2"], "HIGH"),
        ],
    },
    // `default` is the fallback, not a fixed rating: a document may clear a question with
    // it and raise one with a condition above it.
    Graded {
        question_id: "default-clears",
        choices: &["default-clears_c1", "default-clears_c2"],
        rules: &[
            ("default-clears_c1", HIGH_RISK),
            (DEFAULT_CONDITION, NO_RISK),
        ],
        selections: &[
            (&["default-clears_c1"], "HIGH"),
            (&["default-clears_c2"], "NONE"),
        ],
    },
    // One choice, so "every choice" and "the first choice" are the same selection.
    Graded {
        question_id: "single-choice",
        choices: &["single-choice_c1"],
        rules: &[
            ("single-choice_c1", MEDIUM_RISK),
            (DEFAULT_CONDITION, HIGH_RISK),
        ],
        selections: &[(&["single-choice_c1"], "MEDIUM")],
    },
    // A `ChoiceId` is any 1..64-character string (`smithy_query(query="shape",
    // shape="ChoiceId")`), so a condition's atoms are not restricted to identifiers.
    Graded {
        question_id: "punctuated-ids",
        choices: &["punctuated.c-1", "punctuated.c-2"],
        rules: &[
            ("punctuated.c-1 && punctuated.c-2", NO_RISK),
            ("punctuated.c-2", MEDIUM_RISK),
            (DEFAULT_CONDITION, HIGH_RISK),
        ],
        selections: &[
            (&["punctuated.c-2"], "MEDIUM"),
            (&["punctuated.c-1", "punctuated.c-2"], "NONE"),
            (&["punctuated.c-1"], "HIGH"),
        ],
    },
    // Answered, then recorded as not applying to this review: the review's own word
    // outranks what the rules would rate the selection at.
    Graded {
        question_id: "not-applicable",
        choices: &["not-applicable_c1", "not-applicable_c2"],
        rules: &[
            ("not-applicable_c1", NO_RISK),
            (DEFAULT_CONDITION, HIGH_RISK),
        ],
        selections: &[(&["not-applicable_c1"], "NONE")],
    },
    // Never answered, so still UNANSWERED however its rules would rate a selection.
    Graded {
        question_id: "untouched",
        choices: &["untouched_c1", "untouched_c2"],
        rules: &[("untouched_c1", NO_RISK), (DEFAULT_CONDITION, HIGH_RISK)],
        selections: &[],
    },
    // A condition that is not an expression grades nothing, so the rules after it decide:
    // the format bounds the TEXT of a condition and says nothing more about it, so a lens
    // may author text like this and an answer must still be rated.
    Graded {
        question_id: "unparsable",
        choices: &["unparsable_c1", "unparsable_c2"],
        rules: &[
            ("unparsable_c1 &&", NO_RISK),
            ("unparsable_c1", MEDIUM_RISK),
            (DEFAULT_CONDITION, HIGH_RISK),
        ],
        selections: &[
            (&["unparsable_c1", "unparsable_c2"], "MEDIUM"),
            (&["unparsable_c2"], "HIGH"),
            (&["unparsable_c1"], "MEDIUM"),
        ],
    },
];

/// The `QuestionId` of the question the table records as not applying to its review, after
/// answering it.
const NOT_APPLICABLE_QUESTION: &str = "not-applicable";

/// A lens document whose single pillar asks `questions`, each with the choices and the
/// authored `riskRules` its row spells out — the lens JSON format specification's own
/// properties, as `super::custom_lens_import::lens_document` writes them for a document
/// built from counts.
fn graded_lens_document(name: &str, questions: &[Graded]) -> Value {
    let questions_json: Vec<Value> = questions
        .iter()
        .map(|question| {
            let choices: Vec<Value> = question
                .choices
                .iter()
                .map(|choice_id| json!({ "id": choice_id, "title": format!("Choice {choice_id}") }))
                .collect();
            let rules: Vec<Value> = question
                .rules
                .iter()
                .map(|(condition, risk)| json!({ "condition": condition, "risk": risk }))
                .collect();
            json!({
                "id": question.question_id,
                "title": format!("How is {} governed?", question.question_id),
                "description": "what this question asks about",
                "choices": choices,
                "riskRules": rules,
            })
        })
        .collect();
    json!({
        "schemaVersion": "2021-11-01",
        "name": name,
        "pillars": [{
            "id": "governance",
            "name": "Pillar 0: governance",
            "questions": questions_json,
        }],
    })
}

/// The `Risk` an answer carries, from the `Answer` a read or a write reported.
fn risk_of(answer: &Answer) -> String {
    string_member(&answer_of(answer), "Risk")
}

/// `GetLensReview`: `GET /workloads/{WorkloadId}/lensReviews/{LensAlias}`.
async fn lens_review(client: &Client, workload_id: &str, lens: &str) -> Value {
    let answer = client
        .send(
            "GET",
            &format!(
                "/workloads/{}/lensReviews/{}",
                encode_path_segment(workload_id),
                encode_path_segment(lens)
            ),
            None,
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "GetLensReview must report the review, got {answer:?}"
    );
    answer
        .body
        .get("LensReview")
        .cloned()
        .unwrap_or(Value::Null)
}

/// The `LensReviewSummary` `ListLensReviews` reports for `lens`.
async fn lens_review_summary(client: &Client, workload_id: &str, lens: &str) -> Value {
    let answer = client
        .send(
            "GET",
            &format!(
                "/workloads/{}/lensReviews",
                encode_path_segment(workload_id)
            ),
            None,
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "ListLensReviews must report the workload's reviews, got {answer:?}"
    );
    answer
        .body
        .get("LensReviewSummaries")
        .and_then(Value::as_array)
        .and_then(|summaries| {
            summaries
                .iter()
                .find(|summary| string_member(summary, "LensArn") == lens)
                .cloned()
        })
        .unwrap_or_else(|| panic!("the review of {lens} must be listed, got {answer:?}"))
}

/// One `RiskCounts` map as a comparable histogram: every `Risk` the enum publishes, so a
/// bucket a roll-up left out is a difference rather than an absence.
fn risk_counts(value: &Value) -> std::collections::BTreeMap<String, i64> {
    const RISKS: [&str; 5] = ["HIGH", "MEDIUM", "NONE", "NOT_APPLICABLE", "UNANSWERED"];
    let counts = value.get("RiskCounts");
    RISKS
        .iter()
        .map(|risk| {
            let count = counts
                .and_then(|counts| counts.get(*risk))
                .and_then(Value::as_i64)
                .unwrap_or_default();
            ((*risk).to_string(), count)
        })
        .collect()
}

/// The rule: a question of a CUSTOM lens review is graded by the risk rules the lens
/// DOCUMENT authored for it — the first rule whose condition holds over the choices the
/// answer records as in place decides the `Risk`, the reserved `default` condition holding
/// for every selection the rules above it leave, and the document's risk names mapping to
/// the `Risk` enum (`NO_RISK` → `NONE`, `MEDIUM_RISK` → `MEDIUM`, `HIGH_RISK` → `HIGH`) —
/// while an answer that selects nothing is `UNANSWERED` before any rule runs and one the
/// review records as not applying is `NOT_APPLICABLE`.
///
/// The derived value is what every read of the review reports: the write's own answer, the
/// single-answer read, the review's answer list, and the `RiskCounts` of the lens review and
/// of its pillar, which equal the histogram of the review's own answers.
#[tokio::test]
async fn an_answer_to_a_custom_lens_question_is_graded_by_the_authored_risk_rules() {
    let client = Client::new();
    let case = "authored-rules";
    let document = graded_lens_document("Lens authored rules", GRADED_QUESTIONS).to_string();
    let lens_arn = imported_arn(
        &import(&client, &document, None, "tok-import-authored", None).await,
        case,
    );
    wait_for_the_import_to_land();
    publish(&client, case, &lens_arn).await;
    let workload_id = workload_reviewing(&client, case, &lens_arn).await;

    // The questions as a CLIENT discovers them, before anything is answered.
    let listed = listed_answers(&client, case, &workload_id, &lens_arn).await;
    let discovered: Vec<String> = listed
        .iter()
        .map(|summary| string_member(summary, "QuestionId"))
        .collect();
    let authored: Vec<String> = GRADED_QUESTIONS
        .iter()
        .map(|question| question.question_id.to_string())
        .collect();
    assert_eq!(
        discovered, authored,
        "case {case:?}: the review asks the questions the document defines, got {discovered:?}"
    );

    // The `Risk` each question ends up at: the last selection answered for it, and
    // `UNANSWERED` for a question no selection answered.
    let mut expected_final: Vec<(&str, &str)> = Vec::new();

    for question in GRADED_QUESTIONS {
        expected_final.push((
            question.question_id,
            question
                .selections
                .last()
                .map_or("UNANSWERED", |(_, risk)| *risk),
        ));
        for (selected, expected) in question.selections {
            let sent: Vec<String> = selected.iter().map(|id| (*id).to_string()).collect();
            let case = format!("{}-{selected:?}", question.question_id);
            let written = update_answer(
                &client,
                &workload_id,
                &lens_arn,
                question.question_id,
                json!({ "SelectedChoices": sent }),
            )
            .await;
            assert_eq!(
                written.status, 200,
                "case {case:?}: a question the lens defines must be answerable, got {written:?}"
            );
            let rated = risk_of(&written);
            assert_eq!(
                rated, *expected,
                "case {case:?}: the authored rules rate this selection {expected}, got {rated} \
                 in {written:?}"
            );
            assert_eq!(
                strings(&answer_of(&written), "SelectedChoices"),
                sent,
                "case {case:?}: the choices in place are the ones the request named, got \
                 {written:?}"
            );

            // Derived from what is recorded, so the single-answer read agrees with the
            // write that produced it.
            let read = get_answer(&client, &workload_id, &lens_arn, question.question_id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: the answer must be readable back, got {read:?}"
            );
            let read_risk = risk_of(&read);
            assert_eq!(
                read_risk, *expected,
                "case {case:?}: GetAnswer must report the same rating as the write, got \
                 {read_risk} in {read:?}"
            );
        }
    }

    // A question the review records as not applying to it is `NOT_APPLICABLE`, whatever its
    // rules rate the selection it still carries at.
    let not_applicable = update_answer(
        &client,
        &workload_id,
        &lens_arn,
        NOT_APPLICABLE_QUESTION,
        json!({ "IsApplicable": false, "Reason": "OUT_OF_SCOPE" }),
    )
    .await;
    assert_eq!(
        not_applicable.status, 200,
        "case {case:?}: the question must be recordable as not applying, got {not_applicable:?}"
    );
    let rated = risk_of(&not_applicable);
    assert_eq!(
        rated, "NOT_APPLICABLE",
        "case {case:?}: a question that does not apply is NOT_APPLICABLE, got {rated}"
    );
    for entry in &mut expected_final {
        if entry.0 == NOT_APPLICABLE_QUESTION {
            entry.1 = "NOT_APPLICABLE";
        }
    }

    // Every read of the review reports the same derived value, and the roll-ups count it.
    let answered = listed_answers(&client, case, &workload_id, &lens_arn).await;
    let listed_risks: Vec<(String, String)> = answered
        .iter()
        .map(|summary| {
            (
                string_member(summary, "QuestionId"),
                string_member(summary, "Risk"),
            )
        })
        .collect();
    let expected_risks: Vec<(String, String)> = expected_final
        .iter()
        .map(|(question_id, risk)| ((*question_id).to_string(), (*risk).to_string()))
        .collect();
    assert_eq!(
        listed_risks, expected_risks,
        "case {case:?}: ListAnswers must report the rating the authored rules derive for each \
         question, got {listed_risks:?}"
    );

    let mut expected_counts: std::collections::BTreeMap<String, i64> =
        ["HIGH", "MEDIUM", "NONE", "NOT_APPLICABLE", "UNANSWERED"]
            .iter()
            .map(|risk| ((*risk).to_string(), 0))
            .collect();
    for (_, risk) in &expected_final {
        *expected_counts
            .get_mut(*risk)
            .unwrap_or_else(|| panic!("{risk} is not a Risk the enum publishes")) += 1;
    }
    assert!(
        expected_counts.values().filter(|count| **count > 0).count() > 1,
        "the table must spread the review's questions over several Risk buckets, got \
         {expected_counts:?}"
    );

    let review = lens_review(&client, &workload_id, &lens_arn).await;
    assert_eq!(
        risk_counts(&review),
        expected_counts,
        "case {case:?}: the review's roll-up is the histogram of its own answers, got {review:?}"
    );
    let pillars = review
        .get("PillarReviewSummaries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        pillars.len(),
        1,
        "case {case:?}: the document defines one pillar, got {pillars:?}"
    );
    assert_eq!(
        risk_counts(&pillars[0]),
        expected_counts,
        "case {case:?}: the pillar's roll-up counts the questions it asks, got {pillars:?}"
    );

    let summary = lens_review_summary(&client, &workload_id, &lens_arn).await;
    assert_eq!(
        risk_counts(&summary),
        expected_counts,
        "case {case:?}: the listed summary must report the same roll-up, got {summary:?}"
    );
    assert_eq!(
        string_member(&summary, "LensVersion"),
        "1.0",
        "case {case:?}: the summary names the version the review is against, got {summary:?}"
    );
}
