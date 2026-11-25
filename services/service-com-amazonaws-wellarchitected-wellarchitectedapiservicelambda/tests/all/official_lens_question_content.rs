// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the question-level CONTENT a lens publishes and for the way
//! the lens's own risk rules rate an answer to one of its questions.
//!
//! The rules these pin, over TABLES of inputs rather than one recorded request:
//!
//! 1. Every question an AWS-official lens asks offers, AFTER the best practices it asks
//!    about, the terminal "None of these" choice — the only way to answer a question
//!    negatively — named in that question's own choice-id namespace.
//! 2. Every projection of a question reports the question-level prose and documents its
//!    LENS's content carries: the question's description, the helpful-resources document
//!    the lens serves for it and that document's improvement plan, present for a lens whose
//!    question content is published and absent for content that carries none.
//! 3. An answer is rated by the lens's own risk rules: a rule clears or reduces the
//!    question's risk only when the best practices it names are in place, and the lens's
//!    default rule rates every other answer — so the terminal choice, and every selection
//!    too thin to satisfy a rule, is `HIGH` rather than a count of what is selected.
//! 4. A choice a question does not offer is refused by naming that question's whole
//!    published choice set, the terminal choice included, in the lens's own order.
//! 5. A CUSTOM lens's question-level content is the one ITS document declares, and its
//!    questions offer exactly the choices the document declares.
//! 6. A choice's `Description` is the lens's PUBLISHED prose for that best practice, carried
//!    verbatim as fixed text: the same words on every path that projects the choice
//!    (`GetAnswer`, `UpdateAnswer`, `ListAnswers`), for every workload, with no part of it
//!    taken from the request or from the answer recorded against the question.
//! 7. The documents an answer points at are addressed by the question's own PUBLISHED LABEL
//!    rather than assembled from the lens's `LensVersion`, so a revision of the lens does
//!    not move a document the lens publishes.
//!
//! Requests go through the real `MultiServiceRouter`, so routing, restJson1
//! (de)serialization, boundary constraint validation and the handler all run as they do in
//! production. The HTTP client and the request builders are the shared
//! `super::workload_harness`.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use service_com_amazonaws_wellarchitected_wellarchitectedapiservicelambda::state;

use super::custom_lens_import::{import, imported_arn, lens_document};
use super::workload_harness::{
    assert_fields_failed, created_id, encode_path_segment, minimal_create, Answer, Client,
};

/// `Answer$HelpfulResourceUrl` / `$ImprovementPlanUrl` are `@length(min: 1, max: 2048)` and
/// `Answer$QuestionDescription` `@length(min: 1, max: 1024)`
/// (`smithy_query(query="shape", shape="Answer")`).
const URL_LENGTH: (usize, usize) = (1, 2048);
const DESCRIPTION_LENGTH: (usize, usize) = (1, 1024);

/// `Choice$ChoiceId` is `@length(min: 1, max: 64)` and `Choice$Description`
/// `@length(min: 1, max: 1024)` (`smithy_query(query="shape", shape="Choice")`).
const CHOICE_ID_LENGTH: (usize, usize) = (1, 64);
const CHOICE_DESCRIPTION_LENGTH: (usize, usize) = (1, 1024);

/// The page size `ListAnswers` is asked for while sweeping a lens's whole question set: the
/// ceiling its `MaxResults` member allows (`@range(min: 1, max: 50)`).
const FULL_PAGE: usize = 50;

/// `GetAnswer`: `GET /workloads/{WorkloadId}/lensReviews/{LensAlias}/answers/{QuestionId}`.
/// Each label is non-greedy, so it travels percent-encoded in one path segment — which is
/// what carries a custom lens's ARN, with its `:` and `/`, as one segment.
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

/// `UpdateAnswer`: the same path written with `PATCH`.
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

/// Every `AnswerSummary` of one lens review, following the pages `ListAnswers` serves.
async fn all_answers(client: &Client, workload_id: &str, lens: &str, case: &str) -> Vec<Value> {
    let mut summaries = Vec::new();
    let mut next_token: Option<String> = None;
    loop {
        let query = match &next_token {
            Some(token) => format!("MaxResults={FULL_PAGE}&NextToken={token}"),
            None => format!("MaxResults={FULL_PAGE}"),
        };
        let page = client
            .send(
                "GET",
                &format!(
                    "/workloads/{}/lensReviews/{}/answers?{query}",
                    encode_path_segment(workload_id),
                    encode_path_segment(lens)
                ),
                None,
            )
            .await;
        assert_eq!(
            page.status, 200,
            "case {case:?}: expected ListAnswers 200, got {page:?}"
        );
        summaries.extend(
            page.body
                .get("AnswerSummaries")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        next_token = page
            .body
            .get("NextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if next_token.is_none() {
            return summaries;
        }
        assert!(
            summaries.len() <= 1_000,
            "case {case:?}: paging over a lens's questions must terminate, got {} summaries",
            summaries.len()
        );
    }
}

/// A workload whose only lens review is of `lens`.
async fn workload_reviewing(client: &Client, lens: &str, case: &str) -> String {
    let mut request = minimal_create(&format!("wl-{case}"), &format!("tok-wl-{case}"));
    request["Lenses"] = json!([lens]);
    created_id(&client.create(request).await, case)
}

/// The `Answer` a read or a write of one answer reported.
fn answer_of(answer: &Answer) -> Value {
    answer.body.get("Answer").cloned().unwrap_or(Value::Null)
}

/// One string member of a JSON object, `None` when it is absent.
fn text(value: &Value, member: &str) -> Option<String> {
    value
        .get(member)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// One member of every entry of a reported `Choices` catalog, in the order served.
fn choice_members(value: &Value, member: &str) -> Vec<Option<String>> {
    value
        .get("Choices")
        .and_then(Value::as_array)
        .map(|choices| choices.iter().map(|choice| text(choice, member)).collect())
        .unwrap_or_default()
}

/// Rule 1, over the WHOLE AWS-official lens catalog: every question of every lens AWS
/// publishes offers the terminal "None of these" choice after the best practices it asks
/// about, named in that question's own choice-id namespace.
///
/// The rows are the catalog (`state::OFFICIAL_LENS_CATALOG`) times every question each lens
/// asks, read over the wire through `ListAnswers` — so they span both halves of the catalog:
/// the framework lens, whose choice ids the lens publishes, and the specialised lenses,
/// whose choices are named by their position in the question.
#[tokio::test]
async fn every_official_lens_question_offers_the_terminal_choice_after_its_best_practices() {
    let client = Client::new();
    for lens in state::OFFICIAL_LENS_CATALOG {
        let alias = lens.alias;
        let workload_id = workload_reviewing(&client, alias, alias).await;
        let summaries = all_answers(&client, &workload_id, alias, alias).await;
        assert_eq!(
            summaries.len(),
            lens.questions.len(),
            "case {alias:?}: the review must report every question the lens asks, got {} \
             summaries",
            summaries.len()
        );

        for (summary, question) in summaries.iter().zip(lens.questions) {
            let case = format!("{alias}/{}", question.question_id);
            assert_eq!(
                text(summary, "QuestionId").as_deref(),
                Some(question.question_id),
                "case {case:?}: the summaries must be reported in the lens's own order, got \
                 {summary:?}"
            );
            let ids: Vec<Option<String>> = choice_members(summary, "ChoiceId");
            let titles: Vec<Option<String>> = choice_members(summary, "Title");
            let descriptions: Vec<Option<String>> = choice_members(summary, "Description");
            assert_eq!(
                ids.len(),
                question.best_practices.len() + 1,
                "case {case:?}: the question offers one choice per best practice and the \
                 terminal one, got {ids:?}"
            );

            // The best practices come first, in the lens's own order...
            let best_practices: Vec<Option<String>> = (0..question.best_practices.len())
                .map(|index| Some(question.choice_id(index)))
                .collect();
            assert_eq!(
                ids.get(..question.best_practices.len()).unwrap_or_default(),
                best_practices.as_slice(),
                "case {case:?}: the best practices must be reported first, in the lens's \
                 order, got {ids:?}"
            );
            // ...and the terminal choice last, with the lens's own words for it.
            let terminal = question.none_of_these_id();
            assert_eq!(
                ids.last().cloned().flatten(),
                Some(terminal.clone()),
                "case {case:?}: the question's last choice is the terminal one, got {ids:?}"
            );
            assert_eq!(
                titles.last().cloned().flatten().as_deref(),
                Some(state::NONE_OF_THESE_TITLE),
                "case {case:?}: the terminal choice carries its own title, got {titles:?}"
            );
            assert_eq!(
                descriptions.last().cloned().flatten().as_deref(),
                Some(state::NONE_OF_THESE_DESCRIPTION),
                "case {case:?}: the terminal choice says what selecting it means, got \
                 {descriptions:?}"
            );
            assert!(
                (CHOICE_ID_LENGTH.0..=CHOICE_ID_LENGTH.1).contains(&terminal.len()),
                "case {case:?}: ChoiceId is @length{CHOICE_ID_LENGTH:?}, got {terminal:?}"
            );

            // One id per choice, and the terminal id lives in the same namespace as the
            // question's best-practice ids rather than in one of its own.
            let unique: BTreeSet<Option<String>> = ids.iter().cloned().collect();
            assert_eq!(
                unique.len(),
                ids.len(),
                "case {case:?}: a ChoiceId names one choice of the question, got {ids:?}"
            );
            let namespace = terminal
                .strip_suffix(state::NONE_OF_THESE_ID_SUFFIX)
                .or_else(|| terminal.strip_suffix("-no"))
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the terminal id must end in the terminal suffix")
                })
                .to_string();
            for id in best_practices.iter().flatten() {
                assert!(
                    id.starts_with(&namespace),
                    "case {case:?}: the terminal choice must share the question's choice-id \
                     namespace {namespace:?}, got the best practice {id:?}"
                );
            }
        }
    }
}

/// Rule 2: the question-level prose and documents a lens's content carries are reported by
/// every projection of the question — the read of one answer and the write that records one.
///
/// The rows span both halves of the catalog and several pillars: questions of the framework
/// lens, whose content the lens publishes, and a question of a specialised lens, whose
/// content this emulator authored and which therefore publishes no document to link. Two
/// rows also pin the prose the framework publishes under the question itself, word for word.
#[tokio::test]
async fn a_questions_published_description_and_documents_are_reported_by_every_projection() {
    let client = Client::new();
    let rows: [(&str, &str, Option<&str>); 7] = [
        (
            "wellarchitected",
            "organization-priorities",
            Some(
                "Everyone must understand their part in enabling business success. Have \
                 shared goals in order to set priorities for resources. This will maximize \
                 the benefits of your efforts.",
            ),
        ),
        ("wellarchitected", "identities", None),
        ("wellarchitected", "plan-for-dr", None),
        (
            "wellarchitected",
            "cloud-financial-management",
            Some(
                "Implementing Cloud Financial Management enables organizations to realize \
                 business value and financial success as they optimize their cost and usage \
                 and scale on AWS.",
            ),
        ),
        ("wellarchitected", "region-selection", None),
        ("wellarchitected", "performance-monitoring", None),
        // A lens whose question set this emulator authored: it publishes no question
        // description and no document, so neither member is reported.
        ("serverless", "serverless-ops-01", None),
    ];

    let mut documents: BTreeSet<String> = BTreeSet::new();
    for (alias, question_id, published_prose) in rows {
        let case = format!("{alias}/{question_id}");
        let lens = state::official_lens(alias)
            .unwrap_or_else(|| panic!("case {case:?}: the catalog must carry the lens"));
        let question = state::lens_question(alias, question_id)
            .unwrap_or_else(|| panic!("case {case:?}: the lens must ask the question"));
        let workload_id = workload_reviewing(&client, alias, &case.replace('/', "-")).await;

        // The read of the answer, and the write that records one, report the same
        // question-level content: it describes the QUESTION, not the answer.
        let read = get_answer(&client, &workload_id, alias, question_id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected GetAnswer 200, got {read:?}"
        );
        let written = update_answer(
            &client,
            &workload_id,
            alias,
            question_id,
            json!({ "SelectedChoices": [question.choice_id(0)] }),
        )
        .await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: expected UpdateAnswer 200, got {written:?}"
        );

        for (projection, answer) in [("read", answer_of(&read)), ("write", answer_of(&written))] {
            let case = format!("{case}/{projection}");
            let description = text(&answer, "QuestionDescription");
            let helpful = text(&answer, "HelpfulResourceUrl");
            let improvement = text(&answer, "ImprovementPlanUrl");
            match question.description {
                None => {
                    // Content this emulator authored carries no published prose and no
                    // document, and every one of those members is optional.
                    assert_eq!(
                        (description.as_deref(), helpful.as_deref(), improvement.as_deref()),
                        (None, None, None),
                        "case {case:?}: a question whose content is not published reports no \
                         description and no document, got {answer:?}"
                    );
                }
                Some(published) => {
                    assert_eq!(
                        description.as_deref(),
                        Some(published),
                        "case {case:?}: the answer must carry the question's published \
                         description, got {answer:?}"
                    );
                    if let Some(prose) = published_prose {
                        assert_eq!(
                            description.as_deref(),
                            Some(prose),
                            "case {case:?}: the description must be the prose the framework \
                             publishes under this question, got {answer:?}"
                        );
                    }
                    let described = description.unwrap_or_default();
                    assert!(
                        (DESCRIPTION_LENGTH.0..=DESCRIPTION_LENGTH.1).contains(&described.len()),
                        "case {case:?}: QuestionDescription is @length{DESCRIPTION_LENGTH:?}, \
                         got {} character(s)",
                        described.len()
                    );
                    let helpful = helpful.unwrap_or_else(|| {
                        panic!("case {case:?}: a published question serves a document, got {answer:?}")
                    });
                    let improvement = improvement.unwrap_or_else(|| {
                        panic!("case {case:?}: a published question carries an improvement plan, got {answer:?}")
                    });
                    // The document is the LENS's own, addressed by the label the lens
                    // publishes the question under rather than by the version its content
                    // is published at, so the document does not move when `LensVersion`
                    // does; the improvement plan is a section of that same document.
                    let label = state::published_question_label(lens, question_id)
                        .unwrap_or_else(|| panic!("case {case:?}: a published question carries a label"));
                    for (member, url) in [("HelpfulResourceUrl", &helpful), ("ImprovementPlanUrl", &improvement)] {
                        assert!(
                            url.starts_with("https://"),
                            "case {case:?}: {member} must be an https document, got {url:?}"
                        );
                        assert!(
                            (URL_LENGTH.0..=URL_LENGTH.1).contains(&url.len()),
                            "case {case:?}: {member} is @length{URL_LENGTH:?}, got {} \
                             character(s)",
                            url.len()
                        );
                        assert!(
                            url.contains(&label),
                            "case {case:?}: {member} must address the lens's own content by \
                             the question's published label {label:?}, got {url:?}"
                        );
                        assert!(
                            !url.contains(lens.version),
                            "case {case:?}: {member} must not be assembled from the lens \
                             version {:?}, got {url:?}",
                            lens.version
                        );
                    }
                    assert!(
                        improvement.starts_with(&helpful) && improvement.len() > helpful.len(),
                        "case {case:?}: the improvement plan is a section of the question's \
                         own document, got {improvement:?} against {helpful:?}"
                    );
                    // One document per question: two questions of a lens cannot share one.
                    if projection == "read" {
                        assert!(
                            documents.insert(helpful.clone()),
                            "case {case:?}: each question must serve its own document, got \
                             {helpful:?} twice"
                        );
                    }
                }
            }
        }
    }
}

/// `count` questions of the framework lens that ask about DIFFERENT numbers of best
/// practices, each about more than two of them — so a selection of "half" and one of "all
/// but one" differ, and differ differently per row. Read off the lens's own content
/// (`state::FRAMEWORK_LENS_QUESTIONS`), so a row does not restate a published count.
fn questions_with_distinct_choice_counts(count: usize) -> Vec<&'static str> {
    let mut seen: BTreeSet<usize> = BTreeSet::new();
    state::FRAMEWORK_LENS_QUESTIONS
        .iter()
        .filter(|question| question.best_practices.len() > 2)
        .filter(|question| seen.insert(question.best_practices.len()))
        .take(count)
        .map(|question| question.question_id)
        .collect()
}

/// Rule 3: the lens's own risk rules rate an answer to one of its questions.
///
/// The rows span what the rules generalise over — how many of the question's best practices
/// the answer puts in place (none, one, half, all but one, all), whether it selects the
/// terminal choice instead of or beside them, and whether the question applies at all — over
/// questions of differing choice counts, and each row is read back through `GetAnswer` so the
/// rating is the stored answer's and not the write's.
#[tokio::test]
async fn the_lenss_risk_rules_rate_an_answer_from_the_best_practices_in_place() {
    let client = Client::new();
    // Three questions of the framework lens with DIFFERENT numbers of best practices, so
    // "half of them" and "all but one" are different selections in each row. Read off the
    // lens's own content, so a row states the shape it needs and not a question's count.
    let questions: Vec<&'static str> = questions_with_distinct_choice_counts(3);
    assert_eq!(
        questions.len(),
        3,
        "the framework lens must ask questions about differing numbers of best practices, \
         got {questions:?}"
    );

    for question_id in questions {
        let question = state::lens_question(state::FRAMEWORK_LENS_ALIAS, question_id)
            .unwrap_or_else(|| panic!("the framework lens must ask {question_id:?}"));
        let best_practices: Vec<String> = (0..question.best_practices.len())
            .map(|index| question.choice_id(index))
            .collect();
        let terminal = question.none_of_these_id();
        assert!(
            best_practices.len() > 2,
            "the questions under test must ask about several best practices, got \
             {best_practices:?}"
        );
        let half = best_practices.len().div_ceil(2);

        let rows: [(&str, Vec<String>, bool, &str); 7] = [
            // Nothing answered: the selection is what answers a question.
            ("nothing-in-place", Vec::new(), true, "UNANSWERED"),
            // One best practice of several satisfies no risk-reducing rule of the lens, so
            // the answer takes the lens's default rating.
            (
                "one-best-practice",
                best_practices[..1].to_vec(),
                true,
                "HIGH",
            ),
            (
                "half-the-best-practices",
                best_practices[..half].to_vec(),
                true,
                "MEDIUM",
            ),
            (
                "all-but-one-best-practice",
                best_practices[..best_practices.len() - 1].to_vec(),
                true,
                "MEDIUM",
            ),
            (
                "every-best-practice",
                best_practices.clone(),
                true,
                "NONE",
            ),
            // The terminal choice says no best practice is in place, whatever else the
            // answer records.
            ("none-of-these", vec![terminal.clone()], true, "HIGH"),
            // A question the workload says does not apply is rated by that alone.
            (
                "does-not-apply",
                best_practices.clone(),
                false,
                "NOT_APPLICABLE",
            ),
        ];

        let workload_id =
            workload_reviewing(&client, state::FRAMEWORK_LENS_ALIAS, question_id).await;
        for (row, selected, is_applicable, risk) in rows {
            let case = format!("{question_id}/{row}");
            let mut body = json!({
                "SelectedChoices": selected,
                "IsApplicable": is_applicable,
            });
            if !is_applicable {
                body["Reason"] = json!("OUT_OF_SCOPE");
            }
            let written = update_answer(
                &client,
                &workload_id,
                state::FRAMEWORK_LENS_ALIAS,
                question_id,
                body,
            )
            .await;
            assert_eq!(
                written.status, 200,
                "case {case:?}: expected UpdateAnswer 200, got {written:?}"
            );
            assert_eq!(
                text(&answer_of(&written), "Risk").as_deref(),
                Some(risk),
                "case {case:?}: {} of {} best practice(s) in place must rate {risk}, got \
                 {written:?}",
                selected.iter().filter(|id| **id != terminal).count(),
                best_practices.len()
            );

            // The rating is of the STORED answer: the read reports the same one.
            let read = get_answer(
                &client,
                &workload_id,
                state::FRAMEWORK_LENS_ALIAS,
                question_id,
            )
            .await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: expected GetAnswer 200, got {read:?}"
            );
            assert_eq!(
                text(&answer_of(&read), "Risk").as_deref(),
                Some(risk),
                "case {case:?}: the read-back answer must carry the same rating, got {read:?}"
            );
        }
    }
}

/// Rule 4: a `ChoiceId` the question does not offer is refused by naming that question's
/// whole choice set — the terminal choice included — in the lens's own order.
///
/// The rows differ along the question refused for (three of them, from three pillars) and
/// along the member the bad id arrived in: `SelectedChoices` and `ChoiceUpdates`, the two
/// members of `UpdateAnswer` that name a choice.
#[tokio::test]
async fn an_undefined_choice_is_refused_with_the_questions_whole_choice_set() {
    let client = Client::new();
    let workload_id =
        workload_reviewing(&client, state::FRAMEWORK_LENS_ALIAS, "undefined-choice").await;

    for question_id in ["cloud-financial-management", "securely-operate", "plan-for-dr"] {
        let question = state::lens_question(state::FRAMEWORK_LENS_ALIAS, question_id)
            .unwrap_or_else(|| panic!("the framework lens must ask {question_id:?}"));
        // The valid values the refusal has to hand back: every choice the question offers,
        // in the lens's order, ending in the terminal one.
        let mut valid: Vec<String> = (0..question.best_practices.len())
            .map(|index| question.choice_id(index))
            .collect();
        valid.push(question.none_of_these_id());
        let listed = valid.join(", ");

        let absent = "not-a-choice-of-this-question".to_string();
        let mut choice_updates = json!({ "ChoiceUpdates": {} });
        choice_updates["ChoiceUpdates"][absent.clone()] = json!({ "Status": "SELECTED" });
        for (member, body) in [
            (
                "SelectedChoices",
                json!({ "SelectedChoices": [absent.clone()] }),
            ),
            ("ChoiceUpdates", choice_updates),
        ] {
            let case = format!("{question_id}/{member}");
            let refused = update_answer(
                &client,
                &workload_id,
                state::FRAMEWORK_LENS_ALIAS,
                question_id,
                body,
            )
            .await;
            assert_eq!(
                refused.status, 400,
                "case {case:?}: a choice the question does not offer is a 400, got {refused:?}"
            );
            assert_fields_failed(
                &case,
                &refused,
                member,
                &format!("{member} contains invalid choices. Valid choices are: [{listed}]."),
            );
        }
    }
}

/// Rule 5: a CUSTOM lens's question-level content is the one its DOCUMENT declares — the
/// question's `description` and its `helpfulResource` entry's display text and URL — and its
/// questions offer exactly the choices the document declares, with no terminal choice added
/// to them.
#[tokio::test]
async fn a_custom_lenss_question_content_is_the_one_its_document_declares() {
    let client = Client::new();
    let display_text = "How this lens's author explains the question";
    let url = "https://example.invalid/custom-lens/governance.html";

    // Two questions of one pillar: the first declares a helpful resource, the second
    // declares none, so the rows differ along what the document carries.
    let mut document = lens_document("Question content lens", None, &[("governance", 2, 3)]);
    document["pillars"][0]["questions"][0]["helpfulResource"] =
        json!({ "displayText": display_text, "url": url });
    let lens_arn = imported_arn(
        &import(
            &client,
            &document.to_string(),
            None,
            "tok-question-content",
            None,
        )
        .await,
        "import",
    );
    let published = client
        .send(
            "POST",
            &format!("/lenses/{}/versions", encode_path_segment(&lens_arn)),
            Some(json!({ "LensVersion": "1.0", "ClientRequestToken": "tok-content-publish" })),
        )
        .await;
    assert_eq!(
        published.status, 200,
        "the lens under test must publish, got {published:?}"
    );
    let mut request = minimal_create("wl-custom-content", "tok-wl-custom-content");
    request["Lenses"] = json!([lens_arn]);
    let workload_id = created_id(&client.create(request).await, "custom-content");

    for (question_id, expected_resource) in [
        ("governance_q0", Some((display_text, url))),
        ("governance_q1", None),
    ] {
        let case = format!("custom/{question_id}");
        let read = get_answer(&client, &workload_id, &lens_arn, question_id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected GetAnswer 200, got {read:?}"
        );
        let answer = answer_of(&read);
        assert_eq!(
            text(&answer, "QuestionDescription").as_deref(),
            Some("what this question asks about"),
            "case {case:?}: the question's description is the document's own, got {answer:?}"
        );
        assert_eq!(
            text(&answer, "HelpfulResourceUrl"),
            expected_resource.map(|(_, url)| url.to_string()),
            "case {case:?}: the helpful resource is the document's own, got {answer:?}"
        );
        assert_eq!(
            text(&answer, "HelpfulResourceDisplayText"),
            expected_resource.map(|(display, _)| display.to_string()),
            "case {case:?}: the resource's display text is the document's own, got {answer:?}"
        );
        // The lens JSON format declares no question-level improvement plan: that content is
        // per choice, so a custom lens's question reports no improvement-plan URL.
        assert_eq!(
            answer.get("ImprovementPlanUrl"),
            None,
            "case {case:?}: a custom lens's question serves no improvement-plan document, \
             got {answer:?}"
        );
        // And the question offers the three choices the document declares, with no terminal
        // choice of this emulator's making.
        assert_eq!(
            choice_members(&answer, "ChoiceId"),
            (0..3)
                .map(|choice| Some(format!("{question_id}_c{choice}")))
                .collect::<Vec<Option<String>>>(),
            "case {case:?}: the question offers the document's own choices, got {answer:?}"
        );
    }
}

/// One row of the published-prose table: a question of a lens, and the prose the lens
/// publishes for each of the choices under test — `None` where the lens publishes none.
type ProseRow = (
    &'static str,
    &'static str,
    &'static [(&'static str, Option<&'static str>)],
);

/// The `Description` each entry of a projected `Choices` catalog carries, paired with its
/// `ChoiceId` — the mapping every path that projects a question's choices must agree on.
fn descriptions_by_choice(value: &Value) -> Vec<(String, Option<String>)> {
    choice_members(value, "ChoiceId")
        .into_iter()
        .zip(choice_members(value, "Description"))
        .filter_map(|(id, description)| id.map(|id| (id, description)))
        .collect()
}

/// Rule 6: a `Choice$Description` of a lens AWS publishes is that lens's published prose for
/// the best practice, carried verbatim as fixed text — the same words on every path that
/// projects the choice, for every workload, with no part of it taken from the request or
/// from the answer recorded against the question.
///
/// The rows are the prose the Well-Architected Framework lens publishes for the best
/// practices of two of its questions, in two different pillars, beside a question of a lens
/// whose question set this emulator authored and which therefore publishes no prose at all.
/// Each quoted sentence is the WHOLE published description of that best practice, so the
/// assertion is an equality against the words rather than against a shape.
///
/// SOURCE of the quoted prose: the framework lens's own published content, as
/// `state::FRAMEWORK_LENS_QUESTIONS` carries it.
#[tokio::test]
async fn a_choice_description_is_the_lenss_published_prose_on_every_path() {
    let rows: [ProseRow; 3] = [
        (
            "wellarchitected",
            "cloud-financial-management",
            &[
                (
                    "cost_cloud_financial_management_function",
                    Some(
                        "Create a team (Cloud Business Office, Cloud Center of Excellence, \
                         or FinOps team) that is responsible for establishing and \
                         maintaining cost awareness across your organization. The owner of \
                         cost optimization can be individual or a team (requires people \
                         from finance, technology, and business teams) that understands the \
                         entire organization and cloud finance.",
                    ),
                ),
                (
                    "cost_cloud_financial_management_partnership",
                    Some(
                        "Involve finance and technology teams in cost and usage discussions \
                         at all stages of your cloud journey. Teams regularly meet and \
                         discuss topics such as organizational goals and targets, current \
                         state of cost and usage, and financial and accounting practices.",
                    ),
                ),
                (
                    "cost_cloud_financial_management_budget_forecast",
                    Some(
                        "Adjust existing organizational budgeting and forecasting processes \
                         to be compatible with the highly variable nature of cloud costs \
                         and usage. Processes must be dynamic, using trend-based or business \
                         driver-based algorithms or a combination of both.",
                    ),
                ),
                (
                    "cost_cloud_financial_management_cost_awareness",
                    Some(
                        "Implement cost awareness, create transparency, and accountability \
                         of costs into new or existing processes that impact usage, and \
                         leverage existing processes for cost awareness. Implement cost \
                         awareness into employee training.",
                    ),
                ),
                (
                    "cost_cloud_financial_management_scheduled",
                    Some(
                        "Consult regularly with experts or AWS Partners to consider which \
                         services and features provide lower cost. Review AWS blogs and \
                         other information sources.",
                    ),
                ),
                // The terminal choice is not a best practice, so what it carries is what
                // selecting it says about the workload.
                (
                    "cost_cloud_financial_management_no",
                    Some(state::NONE_OF_THESE_DESCRIPTION),
                ),
            ],
        ),
        (
            "wellarchitected",
            "organization-priorities",
            &[
                (
                    "ops_priorities_ext_cust_needs",
                    Some(
                        "Involve key stakeholders, including business, development and \
                         operations teams, when determining where to focus efforts on \
                         external customer needs.",
                    ),
                ),
                ("ops_priorities_no", Some(state::NONE_OF_THESE_DESCRIPTION)),
            ],
        ),
        // A lens whose question set this emulator authored publishes no prose for a best
        // practice, and the member is optional, so it is left off rather than filled in.
        (
            "serverless",
            "serverless-ops-01",
            &[("serverless-ops-01-bp-01", None)],
        ),
    ];

    let client = Client::new();
    for (alias, question_id, choices) in rows {
        let group = format!("{alias}/{question_id}");
        let slug = group.replace('/', "-");
        let question = state::lens_question(alias, question_id)
            .unwrap_or_else(|| panic!("case {group:?}: the lens must ask the question"));

        // Two workloads, with different names, one of them carrying an ANSWER to the
        // question: a choice's prose describes the LENS, so neither the request nor what is
        // recorded against the question can change a word of it.
        let answered = workload_reviewing(&client, alias, &slug).await;
        let untouched = workload_reviewing(&client, alias, &format!("{slug}-again")).await;

        let read = get_answer(&client, &answered, alias, question_id).await;
        assert_eq!(
            read.status, 200,
            "case {group:?}: expected GetAnswer 200, got {read:?}"
        );
        let written = update_answer(
            &client,
            &answered,
            alias,
            question_id,
            json!({ "SelectedChoices": [question.choice_id(0)] }),
        )
        .await;
        assert_eq!(
            written.status, 200,
            "case {group:?}: expected UpdateAnswer 200, got {written:?}"
        );
        let unanswered = get_answer(&client, &untouched, alias, question_id).await;
        assert_eq!(
            unanswered.status, 200,
            "case {group:?}: expected GetAnswer 200, got {unanswered:?}"
        );
        let summary = all_answers(&client, &answered, alias, &group)
            .await
            .into_iter()
            .find(|summary| text(summary, "QuestionId").as_deref() == Some(question_id))
            .unwrap_or_else(|| panic!("case {group:?}: ListAnswers must report the question"));

        let projections = [
            ("GetAnswer", descriptions_by_choice(&answer_of(&read))),
            ("UpdateAnswer", descriptions_by_choice(&answer_of(&written))),
            ("ListAnswers", descriptions_by_choice(&summary)),
            (
                "GetAnswer/unanswered",
                descriptions_by_choice(&answer_of(&unanswered)),
            ),
        ];

        for (choice_id, published) in choices {
            for (path, projected) in &projections {
                let case = format!("{group}/{choice_id}/{path}");
                let reported = projected
                    .iter()
                    .find(|(id, _)| id == choice_id)
                    .map(|(_, description)| description.clone())
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: the question must offer this choice, got \
                             {projected:?}"
                        )
                    });
                assert_eq!(
                    reported.as_deref(),
                    *published,
                    "case {case:?}: expected the lens's published prose {published:?}, got \
                     {reported:?}"
                );
                if let Some(prose) = reported {
                    assert!(
                        (CHOICE_DESCRIPTION_LENGTH.0..=CHOICE_DESCRIPTION_LENGTH.1)
                            .contains(&prose.len()),
                        "case {case:?}: Description is @length{CHOICE_DESCRIPTION_LENGTH:?}, \
                         got {} character(s)",
                        prose.len()
                    );
                    for borrowed in [answered.as_str(), untouched.as_str(), alias, question_id] {
                        assert!(
                            !prose.contains(borrowed),
                            "case {case:?}: published prose is fixed text, so it cannot \
                             carry {borrowed:?}, got {prose:?}"
                        );
                    }
                }
            }
        }
    }
}

/// Rule 7: the documents an answer points at are the LENS's own, addressed by the label the
/// lens publishes the question under, and therefore unchanged when the lens's `LensVersion`
/// moves.
///
/// The rows are one question from each pillar of the framework lens, including the first and
/// last published number of three of them (`SEC 1`, `REL 13`, `PERF 5`), beside a question
/// of a lens whose content this emulator authored, which publishes no document.
///
/// SOURCE of the labels: the framework's own published question numbering (OPS 1-11,
/// SEC 1-11, REL 1-13, PERF 1-5, COST 1-11, SUS 1-6), which is what
/// `state::published_question_label` reads off the lens's question table.
#[tokio::test]
async fn a_questions_documents_are_addressed_by_its_published_label_not_the_lens_version() {
    let rows: [(&str, &str, Option<&str>); 7] = [
        ("wellarchitected", "organization-priorities", Some("OPS_1")),
        ("wellarchitected", "securely-operate", Some("SEC_1")),
        ("wellarchitected", "plan-for-dr", Some("REL_13")),
        ("wellarchitected", "performance-monitoring", Some("PERF_5")),
        (
            "wellarchitected",
            "cloud-financial-management",
            Some("COST_1"),
        ),
        ("wellarchitected", "region-selection", Some("SUS_1")),
        ("serverless", "serverless-ops-01", None),
    ];

    let client = Client::new();
    let mut documents: BTreeSet<String> = BTreeSet::new();
    for (alias, question_id, label) in rows {
        let case = format!("{alias}/{question_id}");
        let lens = state::official_lens(alias)
            .unwrap_or_else(|| panic!("case {case:?}: the catalog must carry the lens"));
        let question = state::lens_question(alias, question_id)
            .unwrap_or_else(|| panic!("case {case:?}: the lens must ask the question"));
        assert_eq!(
            state::published_question_label(lens, question_id).as_deref(),
            label,
            "case {case:?}: the question's published label must be the framework's own \
             numbering"
        );

        let workload_id = workload_reviewing(&client, alias, &case.replace('/', "-")).await;
        let read = get_answer(&client, &workload_id, alias, question_id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected GetAnswer 200, got {read:?}"
        );
        let written = update_answer(
            &client,
            &workload_id,
            alias,
            question_id,
            json!({ "SelectedChoices": [question.choice_id(0)] }),
        )
        .await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: expected UpdateAnswer 200, got {written:?}"
        );

        // A question whose content this emulator authored publishes no document to point at,
        // so both members are left off rather than pointed at a page the lens does not serve.
        let expected =
            label.map(|label| format!("https://wa.aws.amazon.com/wat.question.{label}.en.html"));
        for (path, answer) in [
            ("GetAnswer", answer_of(&read)),
            ("UpdateAnswer", answer_of(&written)),
        ] {
            let case = format!("{case}/{path}");
            let helpful = text(&answer, "HelpfulResourceUrl");
            let improvement = text(&answer, "ImprovementPlanUrl");
            assert_eq!(
                helpful, expected,
                "case {case:?}: HelpfulResourceUrl must be the document the lens publishes \
                 for the question, got {answer:?}"
            );
            assert_eq!(
                improvement,
                expected
                    .as_ref()
                    .map(|url| format!("{url}{}", state::IMPROVEMENT_PLAN_FRAGMENT)),
                "case {case:?}: ImprovementPlanUrl must be the improvement plan of that same \
                 document, got {answer:?}"
            );
            if let Some(url) = &helpful {
                assert!(
                    (URL_LENGTH.0..=URL_LENGTH.1).contains(&url.len()),
                    "case {case:?}: HelpfulResourceUrl is @length{URL_LENGTH:?}, got {} \
                     character(s)",
                    url.len()
                );
                // The document is the lens's published one, so no part of the pair is
                // assembled from the version string the lens happens to carry now.
                assert!(
                    !url.contains(lens.version),
                    "case {case:?}: the document must not be assembled from the lens version \
                     {:?}, got {url:?}",
                    lens.version
                );
            }
        }

        // One document per question: two questions of a lens cannot share one.
        if let Some(url) = expected {
            assert!(
                documents.insert(url.clone()),
                "case {case:?}: each question must address its own document, got {url:?} twice"
            );
        }
    }

    // And EVERY pillar the framework lens is organized into has published documents: a
    // pillar whose questions carried no label would silently report no document at all.
    for (pillar_id, pillar_name, _) in state::framework_lens().pillars() {
        let first = state::FRAMEWORK_LENS_QUESTIONS
            .iter()
            .find(|question| question.pillar_id == pillar_id)
            .unwrap_or_else(|| panic!("case {pillar_name:?}: the pillar must ask a question"));
        assert!(
            state::published_question_label(state::framework_lens(), first.question_id)
                .is_some_and(|label| label.ends_with("_1")),
            "case {pillar_name:?}: the pillar's first question must carry its published \
             number, got {:?}",
            state::published_question_label(state::framework_lens(), first.question_id)
        );
    }
}
