// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the PER-CHOICE answer projection every read of an answer
//! reports: `Answer$ChoiceAnswers` on `UpdateAnswer` and `GetAnswer`, and
//! `AnswerSummary$ChoiceAnswerSummaries` on `ListAnswers`.
//!
//! The rule these pin, over a table of requests rather than one recorded one: a per-choice
//! answer list is "a list of selected choices to a question in your workload", so it
//! reports one entry per choice of the question the stored answer ANSWERS — a choice it
//! records as in place (`SELECTED`) or as not applying (`NOT_APPLICABLE`) — in the lens's
//! own choice order, and NO entry for a choice it leaves `UNSELECTED`: an unselected choice
//! is what every choice of a question starts out as, and it is reported through the
//! question's own `Choices` catalog, which every read carries beside this list. Each entry
//! carries every member its shape models — the choice, its status, the reason and (for the
//! full answer) the notes recorded with it — with the reason and the notes EMPTY until
//! something records one, so a client reading `entry["Reason"]` or `entry["Notes"]` always
//! finds them.
//!
//! The rows differ along what the rule generalises over: which of the question's choices
//! the request names in `SelectedChoices` (none, the first, the last, all but the last,
//! every one, and the member left out altogether) and what a `ChoiceUpdates` entry records
//! for one choice (each `ChoiceStatus` the enum publishes, with and without a reason and
//! notes). Every row is replayed against questions whose catalogs differ in size, and each
//! row's answer is read back through `GetAnswer` and summarised by `ListAnswers`, so what
//! is asserted is the stored answer rather than what the write echoed.
//!
//! Requests go through the real `MultiServiceRouter`, so routing, restJson1
//! (de)serialization, boundary constraint validation and the handler all run as they do in
//! production. The HTTP client and the request builders are the shared
//! `super::workload_harness`.

use serde_json::{json, Value};

use super::workload_harness::{created_id, encode_path_segment, minimal_create, Answer, Client};

/// The alias of the one lens this emulator ships a question set for.
const FRAMEWORK_LENS: &str = "wellarchitected";

/// The questions the rows are replayed against: questions of four pillars of that lens,
/// whose choice catalogs differ in size (the ids are this emulator's own stable spelling,
/// pinned over the wire by `super::question_choice_catalog`).
const QUESTIONS: [&str; 4] = [
    "organization-priorities",
    "identities",
    "back-up-data",
    "region-selection",
];

/// `ChoiceStatus` members (`smithy_query(query="shape", shape="ChoiceStatus")`): the whole
/// value set, which is also what a `ChoiceUpdate` may record for a choice.
const SELECTED: &str = "SELECTED";
const UNSELECTED: &str = "UNSELECTED";
const NOT_APPLICABLE: &str = "NOT_APPLICABLE";

/// Two `ChoiceReason` members (`smithy_query(query="shape", shape="ChoiceReason")`).
const OUT_OF_SCOPE: &str = "OUT_OF_SCOPE";
const BUSINESS_PRIORITIES: &str = "BUSINESS_PRIORITIES";

/// Which choices of a question a row names in `SelectedChoices`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Select {
    /// The member is left out of the request altogether.
    Absent,
    Nothing,
    First,
    Last,
    AllButLast,
    Every,
}

impl Select {
    /// The `ChoiceId`s this plan names out of the question's own catalog, in the catalog's
    /// order, and `None` for the plan that sends no member at all.
    fn of(self, choices: &[String]) -> Option<Vec<String>> {
        let selected = match self {
            Select::Absent => return None,
            Select::Nothing => Vec::new(),
            Select::First => choices.iter().take(1).cloned().collect(),
            Select::Last => choices.iter().rev().take(1).cloned().collect(),
            Select::AllButLast => choices
                .iter()
                .take(choices.len().saturating_sub(1))
                .cloned()
                .collect(),
            Select::Every => choices.to_vec(),
        };
        Some(selected)
    }
}

/// Which choice of a question a row's `ChoiceUpdates` entry names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    First,
    Last,
}

impl Which {
    fn of(self, choices: &[String]) -> String {
        match self {
            Which::First => choices.first().cloned(),
            Which::Last => choices.iter().next_back().cloned(),
        }
        .expect("every question of the lens offers at least one choice")
    }
}

/// One request under test: what it names, and nothing about what it must report — the
/// expectation is derived from the row itself ([`expected_entries`]), so the two cannot
/// drift.
struct Row {
    case: &'static str,
    select: Select,
    /// The `ChoiceUpdates` entry the request carries: the choice it names, the status it
    /// records, and the reason and notes it carries (empty for the entry that carries
    /// neither).
    update: Option<(Which, &'static str, &'static str, &'static str)>,
}

/// One per-choice answer as a read must report it: `(ChoiceId, Status, Reason, Notes)`.
type Entry = (String, String, String, String);

/// The rows: every `SelectedChoices` plan on its own, then each `ChoiceStatus` a
/// `ChoiceUpdate` may record — with and without the reason and notes it may carry — and a
/// row that writes a selection and a per-choice update together.
const ROWS: &[Row] = &[
    Row {
        case: "no-member",
        select: Select::Absent,
        update: None,
    },
    Row {
        case: "selects-nothing",
        select: Select::Nothing,
        update: None,
    },
    Row {
        case: "selects-the-first-choice",
        select: Select::First,
        update: None,
    },
    Row {
        case: "selects-the-last-choice",
        select: Select::Last,
        update: None,
    },
    Row {
        case: "selects-all-but-the-last",
        select: Select::AllButLast,
        update: None,
    },
    Row {
        case: "selects-every-choice",
        select: Select::Every,
        update: None,
    },
    Row {
        case: "updates-one-choice-to-selected",
        select: Select::Absent,
        update: Some((Which::First, SELECTED, "", "")),
    },
    Row {
        case: "updates-one-choice-to-not-applicable-with-reason-and-notes",
        select: Select::Absent,
        update: Some((
            Which::Last,
            NOT_APPLICABLE,
            OUT_OF_SCOPE,
            "this best practice is out of scope here",
        )),
    },
    Row {
        case: "updates-one-choice-to-unselected-with-reason-and-notes",
        select: Select::Absent,
        update: Some((
            Which::First,
            UNSELECTED,
            BUSINESS_PRIORITIES,
            "not this quarter",
        )),
    },
    Row {
        case: "selects-the-first-choice-and-excuses-the-last",
        select: Select::First,
        update: Some((Which::Last, NOT_APPLICABLE, OUT_OF_SCOPE, "")),
    },
    Row {
        case: "selects-every-choice-and-unselects-the-first",
        select: Select::Every,
        update: Some((Which::First, UNSELECTED, "", "")),
    },
];

/// The request body one row sends.
fn body_of(row: &Row, choices: &[String]) -> Value {
    let mut body = json!({});
    if let Some(selected) = row.select.of(choices) {
        body["SelectedChoices"] = json!(selected);
    }
    if let Some((which, status, reason, notes)) = row.update {
        let mut update = json!({ "Status": status });
        if !reason.is_empty() {
            update["Reason"] = json!(reason);
        }
        if !notes.is_empty() {
            update["Notes"] = json!(notes);
        }
        body["ChoiceUpdates"] = json!({ which.of(choices): update });
    }
    body
}

/// The per-choice answers a read must report once `row` has been applied to a question
/// nothing had answered: the choices the answer ANSWERS, in the catalog's order, each with
/// the status, reason and notes the row recorded for it — the reason and the notes empty
/// where the row recorded none.
fn expected_entries(row: &Row, choices: &[String]) -> Vec<Entry> {
    let mut table: Vec<Entry> = choices
        .iter()
        .map(|choice_id| {
            (
                choice_id.clone(),
                UNSELECTED.to_string(),
                String::new(),
                String::new(),
            )
        })
        .collect();
    if let Some(selected) = row.select.of(choices) {
        for entry in table.iter_mut() {
            entry.1 = if selected.contains(&entry.0) {
                SELECTED
            } else {
                UNSELECTED
            }
            .to_string();
        }
    }
    if let Some((which, status, reason, notes)) = row.update {
        let named = which.of(choices);
        for entry in table.iter_mut().filter(|entry| entry.0 == named) {
            // A `ChoiceUpdate` carries one choice's answer in full, so it replaces the
            // status, the reason and the notes stored for that choice.
            entry.1 = status.to_string();
            entry.2 = reason.to_string();
            entry.3 = notes.to_string();
        }
    }
    table
        .into_iter()
        .filter(|(_, status, _, _)| status != UNSELECTED)
        .collect()
}

/// `UpdateAnswer`: `PATCH /workloads/{WorkloadId}/lensReviews/{LensAlias}/answers/{QuestionId}`.
async fn update_answer(
    client: &Client,
    workload_id: &str,
    question_id: &str,
    body: Value,
) -> Answer {
    client
        .send(
            "PATCH",
            &answer_path(workload_id, question_id),
            Some(body),
        )
        .await
}

/// `GetAnswer`: the same path read with `GET`.
async fn get_answer(client: &Client, workload_id: &str, question_id: &str) -> Answer {
    client.send("GET", &answer_path(workload_id, question_id), None).await
}

fn answer_path(workload_id: &str, question_id: &str) -> String {
    format!(
        "/workloads/{}/lensReviews/{}/answers/{}",
        encode_path_segment(workload_id),
        encode_path_segment(FRAMEWORK_LENS),
        encode_path_segment(question_id)
    )
}

/// The `AnswerSummary` `ListAnswers` reports for one question of the review. The read is
/// scoped to that question's own pillar, so the question is on the first page whichever
/// pillar it belongs to.
async fn summary_of(
    client: &Client,
    case: &str,
    workload_id: &str,
    question_id: &str,
    pillar_id: &str,
) -> Value {
    let answer = client
        .send(
            "GET",
            &format!(
                "/workloads/{}/lensReviews/{}/answers?PillarId={pillar_id}&MaxResults=50",
                encode_path_segment(workload_id),
                encode_path_segment(FRAMEWORK_LENS)
            ),
            None,
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected ListAnswers 200, got {answer:?}"
    );
    answer
        .body
        .get("AnswerSummaries")
        .and_then(Value::as_array)
        .and_then(|summaries| {
            summaries
                .iter()
                .find(|summary| {
                    summary.get("QuestionId").and_then(Value::as_str) == Some(question_id)
                })
                .cloned()
        })
        .unwrap_or_else(|| panic!("case {case:?}: ListAnswers must report {question_id}"))
}

fn string_member(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `Answer` a read or a write reported.
fn answer_of(case: &str, answer: &Answer) -> Value {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200, got {answer:?}"
    );
    answer
        .body
        .get("Answer")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the response must carry an Answer, got {answer:?}"))
}

/// The `ChoiceId`s a question's own catalog offers, in the order served — the only place a
/// client discovers them.
fn catalog_ids(value: &Value) -> Vec<String> {
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

/// A list member as a vector of strings, empty when the member is absent.
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

/// The `ChoiceAnswers` of a full answer, as `(ChoiceId, Status, Reason, Notes)` in the
/// order served. A member a reported entry does NOT carry reads back as the sentinel below,
/// so an absent `Reason` or `Notes` cannot be mistaken for an empty one.
const MEMBER_ABSENT: &str = "<absent>";

fn reported_entries(case: &str, value: &Value, member: &str) -> Vec<Entry> {
    value
        .get(member)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: {member} must be a list, got {value:?}"))
        .iter()
        .map(|entry| {
            (
                string_member(entry, "ChoiceId"),
                string_member(entry, "Status"),
                member_or_absent(entry, "Reason"),
                member_or_absent(entry, "Notes"),
            )
        })
        .collect()
}

fn member_or_absent(entry: &Value, name: &str) -> String {
    match entry.get(name) {
        None => MEMBER_ABSENT.to_string(),
        Some(value) => value.as_str().unwrap_or_default().to_string(),
    }
}

/// A workload reviewing the framework lens, and its `WorkloadId`. One per row, so each row
/// writes to a question nothing has answered.
async fn framework_workload(client: &Client, name: &str) -> String {
    created_id(
        &client
            .create(minimal_create(name, &format!("tok-{name}")))
            .await,
        name,
    )
}

/// Rule: every read of an answer reports one per-choice entry per choice the answer
/// ANSWERS, with the reason and the notes recorded for that choice, and none for a choice
/// the answer leaves unselected.
#[tokio::test]
async fn an_answer_reports_the_choices_it_answers_and_no_others() {
    let client = Client::new();

    for (position, row) in ROWS.iter().enumerate() {
        let workload_id =
            framework_workload(&client, &format!("choice-projection-{position}")).await;

        for question_id in QUESTIONS {
            let case = format!("{}/{question_id}", row.case);

            // The catalog as a CLIENT discovers it, from the read of the unanswered
            // question — and an unanswered question answers no choice at all.
            let before = answer_of(&case, &get_answer(&client, &workload_id, question_id).await);
            let choices = catalog_ids(&before);
            let pillar_id = string_member(&before, "PillarId");
            assert!(
                !pillar_id.is_empty(),
                "case {case:?}: the answer must name the pillar that asks the question, got \
                 {before:?}"
            );
            assert!(
                !choices.is_empty(),
                "case {case:?}: the question must publish the choices an answer names, got {before:?}"
            );
            assert_eq!(
                reported_entries(&case, &before, "ChoiceAnswers"),
                Vec::<Entry>::new(),
                "case {case:?}: a question nothing has answered answers no choice, got {before:?}"
            );

            let written = answer_of(
                &case,
                &update_answer(&client, &workload_id, question_id, body_of(row, &choices)).await,
            );
            let expected = expected_entries(row, &choices);

            // The write reports the answer it recorded, and the read reports the same one:
            // the read is what proves the projection is of stored state.
            let read = answer_of(&case, &get_answer(&client, &workload_id, question_id).await);
            assert_eq!(
                read, written,
                "case {case:?}: GetAnswer must report the answer UpdateAnswer recorded, got {read:?}"
            );
            for (reader, answer) in [("written", &written), ("read-back", &read)] {
                let actual = reported_entries(&case, answer, "ChoiceAnswers");
                assert_eq!(
                    actual, expected,
                    "case {case:?} ({reader}): expected the per-choice answers {expected:?}, \
                     got {actual:?}"
                );
                // The catalog describes the QUESTION, so answering it leaves the catalog
                // where it was, and the choices in place are the entries recorded SELECTED.
                assert_eq!(
                    catalog_ids(answer),
                    choices,
                    "case {case:?} ({reader}): answering a question must not change its \
                     choice catalog, got {answer:?}"
                );
                let selected: Vec<String> = expected
                    .iter()
                    .filter(|(_, status, _, _)| status == SELECTED)
                    .map(|(choice_id, _, _, _)| choice_id.clone())
                    .collect();
                assert_eq!(
                    strings(answer, "SelectedChoices"),
                    selected,
                    "case {case:?} ({reader}): expected SelectedChoices {selected:?}, got \
                     {answer:?}"
                );
            }

            // The summary of the same answer says the same about the same choices, less the
            // notes `ChoiceAnswerSummary` models no member for.
            let summary =
                summary_of(&client, &case, &workload_id, question_id, &pillar_id).await;
            let summarised = reported_entries(&case, &summary, "ChoiceAnswerSummaries");
            let expected_summary: Vec<Entry> = expected
                .iter()
                .map(|(choice_id, status, reason, _)| {
                    (
                        choice_id.clone(),
                        status.clone(),
                        reason.clone(),
                        MEMBER_ABSENT.to_string(),
                    )
                })
                .collect();
            assert_eq!(
                summarised, expected_summary,
                "case {case:?}: expected the summarised per-choice answers \
                 {expected_summary:?}, got {summarised:?}"
            );
        }
    }
}

/// Rule, stated as the invariants that hold of the list whatever a request wrote: every
/// entry names a choice of the question, no entry repeats one, no entry reports a choice
/// the answer leaves unselected, and every entry carries the reason and the notes its shape
/// models. Checked over the same rows, so a request that breaks one is caught whichever
/// member it wrote through.
#[tokio::test]
async fn every_reported_per_choice_answer_names_an_answered_choice_of_the_question() {
    let client = Client::new();
    let workload_id = framework_workload(&client, "choice-projection-invariants").await;

    for (position, row) in ROWS.iter().enumerate() {
        // The rows are applied one after another to the SAME answer, so each is judged
        // against whatever the previous one left stored rather than against a fresh answer.
        let question_id = QUESTIONS[position % QUESTIONS.len()];
        let case = format!("{}/{question_id}", row.case);
        let before = answer_of(&case, &get_answer(&client, &workload_id, question_id).await);
        let choices = catalog_ids(&before);

        let answer = answer_of(
            &case,
            &update_answer(&client, &workload_id, question_id, body_of(row, &choices)).await,
        );
        let entries = reported_entries(&case, &answer, "ChoiceAnswers");

        let mut seen: Vec<&str> = Vec::new();
        for (choice_id, status, reason, notes) in &entries {
            assert!(
                choices.contains(choice_id),
                "case {case:?}: {choice_id:?} is not a choice of the question {choices:?}, \
                 got {answer:?}"
            );
            assert!(
                !seen.contains(&choice_id.as_str()),
                "case {case:?}: {choice_id:?} must be reported once, got {entries:?}"
            );
            seen.push(choice_id);
            assert_ne!(
                status, UNSELECTED,
                "case {case:?}: a choice the answer leaves unselected is not one it answers, \
                 got {entries:?}"
            );
            assert!(
                [SELECTED, NOT_APPLICABLE].contains(&status.as_str()),
                "case {case:?}: {status:?} is not a ChoiceStatus an answered choice carries, \
                 got {entries:?}"
            );
            assert_ne!(
                reason, MEMBER_ABSENT,
                "case {case:?}: every entry carries a Reason, got {entries:?}"
            );
            assert_ne!(
                notes, MEMBER_ABSENT,
                "case {case:?}: every entry carries Notes, got {entries:?}"
            );
            assert!(
                notes.chars().count() <= 250,
                "case {case:?}: ChoiceAnswer$Notes must satisfy @length(max 250), got \
                 {entries:?}"
            );
        }
        // The entries come in the lens's own choice order, which is the catalog's.
        let ordered: Vec<&str> = choices
            .iter()
            .map(String::as_str)
            .filter(|choice_id| seen.contains(choice_id))
            .collect();
        assert_eq!(
            seen, ordered,
            "case {case:?}: the per-choice answers come in the lens's choice order, got \
             {entries:?}"
        );
    }
}
