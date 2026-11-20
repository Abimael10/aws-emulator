// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the refusal a value bound OUTSIDE the request body gets when
//! the parameter it was read as does not admit it — the `PillarId` a read of a lens review
//! is SCOPED to above all.
//!
//! The rule pinned here, over a TABLE of parameters, lenses, values and operations rather
//! than the one request an issue cited: such a value is refused `400 ValidationException`
//! whose `Reason` is `FIELD_VALIDATION_FAILED`, whose ONE `Fields` entry names that
//! parameter and states `The <Parameter> that was passed in is invalid.`, and whose
//! `Message` is this service's `[Validation]` tag followed by that entry's sentence
//! VERBATIM — so a client that attributes the failure from `Message` reads the same
//! diagnostic as one that reads `Fields`, instead of a summary that names nothing. The
//! sentence states the parameter and nothing more: not the value, and not the values this
//! emulator holds for the resource the request addresses, which a caller discovers from a
//! READ of that resource rather than from a refusal.
//!
//! The rows differ along what the rule generalises over: WHERE the admitted set comes from
//! — the MODEL, for a parameter targeting an enumerated type, and the addressed lens's own
//! CONTENT, for a `PillarId` — the lens (an AWS-official one, an imported custom one whose
//! pillars are its document's), the FORM of the value (a word no lens defines, a pillar of
//! this lens in another case, a pillar of the OTHER lens, a value at the member's published
//! `@length` maximum) and the operation the value arrives at (`ListAnswers`,
//! `ListLensReviewImprovements`, `ListReviewTemplateAnswers`, `DeleteLens`, `ListLenses`).
//! Each lens's own pillars are the positive control: they still SELECT their questions.
//!
//! The pillars each lens defines are read back over the wire from `GetLensReview`'s
//! `PillarReviewSummaries`, and are what the refusal is asserted to publish NOWHERE.
//!
//! Requests go through the real `MultiServiceRouter`, so routing, restJson1
//! (de)serialization, boundary constraint validation and the handler all run as they do in
//! production. The HTTP client, the workload/template seeding and the envelope assertion are
//! the shared `super::workload_harness`; the import and publish wire helpers are the ones
//! `super::custom_lens_review_answers` already spells.

use serde_json::{json, Value};

use super::custom_lens_import::wait_for_the_import_to_land;
use super::custom_lens_review_answers::{import_shape, publish, Shape};
use super::workload_harness::{
    assert_field_validation, created_id, encode_path_segment, minimal_create, review_template,
    Answer, Client,
};

/// The AWS-official lens this emulator ships a question set for.
const FRAMEWORK_LENS: &str = "wellarchitected";

/// The document the imported row publishes: `(pillar id, questions in it, choices per
/// question)`, with pillar ids of its OWN, so a row sending one of them to the other lens
/// proves the admitted set is the ADDRESSED lens's content.
const CUSTOM_SHAPE: Shape = &[("governance", 2, 2), ("platform", 1, 3)];

/// `PillarId`'s published bound, `@length(1, 64)`
/// (`smithy_query(query="shape", shape="PillarId")`): the longest value the member accepts
/// is still judged by the pillar rule rather than by that bound.
const PILLAR_ID_MAX_LENGTH: usize = 64;

/// A `WorkloadId`-shaped value (`^[0-9a-f]{32}$`) that names nothing: a parameter value the
/// service will not accept is refused before any lookup, so such a row needs no workload.
const ABSENT_HEX_ID: &str = "0123456789abcdef0123456789abcdef";

/// A custom lens ARN that is well formed and names no imported lens, for the same reason.
const ABSENT_LENS_ARN: &str =
    "arn:aws:wellarchitected:us-west-2:000000000000:lens/0123456789abcdef0123456789abcdef";

/// The sentence this service refuses a parameter value with, over the parameter the value
/// was read as — the same wording for a set the MODEL publishes and for one the addressed
/// lens's content defines.
fn invalid_parameter(parameter: &str) -> String {
    format!("The {parameter} that was passed in is invalid.")
}

/// A workload reviewed against `lens`, and its `WorkloadId`.
async fn workload_reviewing(client: &Client, case: &str, lens: &str) -> String {
    let mut request = minimal_create(&format!("wl-{case}"), &format!("tok-wl-{case}"));
    request["Lenses"] = json!([lens]);
    created_id(&client.create(request).await, case)
}

/// The `PillarId`s one lens review of a workload reports a summary for, in the order it
/// serves them — the lens's own pillar order, and the values a refusal must publish none
/// of. Shared with `super::lens_content_validation`, which pins the same finding over the
/// official lens alone.
pub(crate) async fn reported_pillars(
    client: &Client,
    case: &str,
    workload_id: &str,
    lens: &str,
) -> Vec<String> {
    let answer = client
        .send(
            "GET",
            &format!(
                "/workloads/{workload_id}/lensReviews/{}",
                encode_path_segment(lens)
            ),
            None,
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: GetLensReview must report the lens's pillars, got {answer:?}"
    );
    let pillars: Vec<String> = answer
        .body
        .get("LensReview")
        .and_then(|review| review.get("PillarReviewSummaries"))
        .and_then(Value::as_array)
        .map(|summaries| {
            summaries
                .iter()
                .filter_map(|summary| summary.get("PillarId"))
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();
    assert!(
        pillars.len() > 1,
        "case {case:?}: the lens under test must define several pillars, got {pillars:?}"
    );
    pillars
}

/// The three reads a `PillarId` scopes, each answered for one value: the answer list and
/// the improvement plan of a workload's lens review, and the answer list of a review
/// template's.
async fn scoped_reads(
    client: &Client,
    workload_id: &str,
    template_arn: &str,
    lens: &str,
    pillar_id: &str,
) -> Vec<(&'static str, Answer)> {
    let encoded_lens = encode_path_segment(lens);
    let review = format!("/workloads/{workload_id}/lensReviews/{encoded_lens}");
    vec![
        (
            "ListAnswers",
            client
                .send("GET", &format!("{review}/answers?PillarId={pillar_id}"), None)
                .await,
        ),
        (
            "ListLensReviewImprovements",
            client
                .send(
                    "GET",
                    &format!("{review}/improvements?PillarId={pillar_id}"),
                    None,
                )
                .await,
        ),
        (
            "ListReviewTemplateAnswers",
            client
                .list_review_template_answers(template_arn, lens, &format!("PillarId={pillar_id}"))
                .await,
        ),
    ]
}

/// A `PillarId` outside the lens under review is refused on every read it scopes, with the
/// finding naming `PillarId` and publishing none of that lens's pillars; a pillar the lens
/// DOES define still selects.
#[tokio::test]
async fn a_pillar_outside_the_lens_is_refused_naming_the_parameter() {
    let client = Client::new();

    let custom_arn = import_shape(&client, "pillar-filter", CUSTOM_SHAPE).await;
    wait_for_the_import_to_land();
    publish(&client, "pillar-filter", &custom_arn).await;

    // The two lenses the rule is checked over, each with a pillar of the OTHER as one of
    // the values it must refuse.
    let rows: [(&str, String, &str); 2] = [
        ("official-lens", FRAMEWORK_LENS.to_string(), CUSTOM_SHAPE[0].0),
        ("imported-lens", custom_arn.clone(), "security"),
    ];

    let sentence = invalid_parameter("PillarId");
    for (lens_case, lens, foreign_pillar) in rows {
        let workload_id = workload_reviewing(&client, lens_case, &lens).await;
        let template_arn = review_template(&client, &format!("tpl-{lens_case}"), &[&lens]).await;
        let defined = reported_pillars(&client, lens_case, &workload_id, &lens).await;

        let undefined: [(&str, String); 4] = [
            ("word-no-lens-defines", "notapillar".to_string()),
            ("own-pillar-in-another-case", defined[0].to_uppercase()),
            ("pillar-of-the-other-lens", foreign_pillar.to_string()),
            ("value-at-the-published-maximum", "p".repeat(PILLAR_ID_MAX_LENGTH)),
        ];

        for (form, pillar_id) in undefined {
            for (operation, answer) in
                scoped_reads(&client, &workload_id, &template_arn, &lens, &pillar_id).await
            {
                let case = format!("{lens_case}/{form}/{operation}");
                assert_field_validation(&case, &answer, "PillarId", &sentence);
                // The pillars the lens DOES define are its content, served by the lens's
                // own review: refusing one value may not publish them.
                let body = answer.body.to_string();
                for pillar in &defined {
                    assert!(
                        !body.contains(pillar.as_str()),
                        "case {case:?}: the refusal must not publish the lens's pillar \
                         {pillar:?}, got {answer:?}"
                    );
                }
            }
        }

        // Every pillar the lens DOES define still selects on every one of those reads, so
        // the rule refuses values rather than the filter itself.
        for pillar_id in &defined {
            for (operation, answer) in
                scoped_reads(&client, &workload_id, &template_arn, &lens, pillar_id).await
            {
                let case = format!("{lens_case}/{pillar_id}/{operation}");
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: a pillar the lens defines must still select, got {answer:?}"
                );
            }
        }
    }
}

/// The same envelope for a parameter whose admitted set the MODEL publishes: the refusal
/// names the parameter the value was read as, in `Message` as well as in `Fields`, whether
/// that set comes from an enumerated type or from the addressed lens's content.
///
/// The rows span three operations and three forms of refused value (a value of a sibling
/// type, a published member lowercased, a word no type publishes), beside one `PillarId`
/// row judged against lens content — the two halves of the class in one table.
#[tokio::test]
async fn a_refused_parameter_is_worded_the_same_whatever_publishes_its_set() {
    let client = Client::new();
    let workload_id = workload_reviewing(&client, "modelled-set", FRAMEWORK_LENS).await;

    let rows: [(&str, &str, String, &str); 4] = [
        (
            "DeleteLens$LensStatus/value-of-no-published-member",
            "DELETE",
            format!(
                "/lenses/{}?LensStatus=SIM2REAL&ClientRequestToken=tok-delete-lens",
                encode_path_segment(ABSENT_LENS_ARN)
            ),
            "LensStatusType",
        ),
        (
            "ListLenses$LensStatus/published-member-lowercased",
            "GET",
            "/lenses?LensStatus=draft".to_string(),
            "LensStatusType",
        ),
        (
            "ListWorkloadShares$Status/value-of-no-published-member",
            "GET",
            format!("/workloads/{ABSENT_HEX_ID}/shares?Status=WITHDRAWN"),
            "ShareStatus",
        ),
        (
            "ListAnswers$PillarId/word-no-lens-defines",
            "GET",
            format!(
                "/workloads/{workload_id}/lensReviews/{FRAMEWORK_LENS}/answers?\
                 PillarId=notapillar"
            ),
            "PillarId",
        ),
    ];

    for (case, method, path, parameter) in rows {
        let answer = client.send(method, &path, None).await;
        assert_field_validation(case, &answer, parameter, &invalid_parameter(parameter));
    }
}
