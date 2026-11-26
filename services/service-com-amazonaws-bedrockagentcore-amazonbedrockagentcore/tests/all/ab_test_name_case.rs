// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how the A/B-test plane treats the CASE of an `ABTestName`, and
//! for the two sentences the plane refuses a create or an update with.
//!
//! The rules under test:
//!
//! * an A/B test's name is case-insensitive — whatever case the request used, the name
//!   the service persists and reports (on the create's own 202, through `GetABTest` and
//!   in every `ListABTests` summary) is that name mapped to lower case, so name
//!   uniqueness within the account is case-insensitive and two creates differing only in
//!   case collide;
//! * the `ConflictException` a taken name is refused with is the fixed sentence
//!   `AB test with name '<name>' already exists`, whose only variable part is the name as
//!   the REQUEST spelled it;
//! * the `ValidationException` variant weights that do not total 100 are refused with is
//!   the fixed sentence `Variant weights must sum to 100`, with no variable part — the
//!   total a particular request came to is not in it.
//!
//! Every case drives real HTTP through the same `MultiServiceRouter` the emulator serves
//! with, so the routes, status codes, `restJson1` bodies and modelled error codes are
//! exercised and not just the handler functions.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::ab_test_support::*;

/// Every A/B test the emulator lists, as (name, abTestId) pairs.
async fn listed_names(router: &MultiServiceRouter) -> Vec<(String, String)> {
    let (status, body) = list_ab_tests(router, "maxResults=100").await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from ListABTests, got {status} with {body:?}"
    );
    member(&body, "abTests")
        .as_array()
        .unwrap_or_else(|| panic!("expected abTests to be a list, got {body:?}"))
        .iter()
        .map(|summary| {
            (
                text(summary, "name").to_string(),
                text(summary, "abTestId").to_string(),
            )
        })
        .collect()
}

/// A create's name is normalised to lower case once, on the write path: the 202, the
/// `GetABTest` read and the `ListABTests` summary all report the normalised spelling, and
/// the `abTestId` minted from the name carries it too.
#[tokio::test]
async fn a_created_ab_test_reports_its_name_lower_cased() {
    let (router, _engine) = emulator();

    // `ABTestName` is ^[a-zA-Z][a-zA-Z0-9_]{0,47}$ (source: smithy_query query="shape"
    // shape="ABTestName"), so the rows differ along what the rule generalises over: how
    // much of the name is upper case (none, the leading letter, every letter, mixed), the
    // characters beside letters the pattern admits (digits, `_`), and the length at both
    // published bounds (one character and 48).
    // Each row names a DIFFERENT test, since two names differing only in case are one
    // name here (the next suite pins that).
    let cases: Vec<(&str, String)> = vec![
        ("already-lower-case", "checkout_flow".to_string()),
        ("leading-capital", "Search_ranking".to_string()),
        ("mixed-case-camel", "Sim2RealSummaryTone".to_string()),
        ("all-upper-case", "PAYMENT_FLOW".to_string()),
        ("upper-case-with-digits", "RANKING_V2_2026".to_string()),
        ("shortest-name-upper-case", "Z".to_string()),
        ("longest-name-mixed-case", format!("N{}", "aB".repeat(23) + "c")),
    ];

    let mut expected_listed: Vec<String> = Vec::new();
    for (case, requested) in cases {
        let normalised = requested.to_ascii_lowercase();
        let (status, created) = create_ab_test(&router, minimal_request(&requested)).await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected HTTP 202 from CreateABTest, got {status} with {created:?}"
        );
        assert_eq!(
            text(&created, "name"),
            normalised,
            "case {case:?}: sent {requested:?}, expected the name reported as {normalised:?}, got {:?}",
            text(&created, "name")
        );

        let ab_test_id = text(&created, "abTestId").to_string();
        assert!(
            ab_test_id.starts_with(&format!("{normalised}-")),
            "case {case:?}: expected the id to be minted from the normalised name {normalised:?}, got {ab_test_id:?}"
        );

        let (status, read) = get_ab_test(&router, &ab_test_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetABTest, got {status} with {read:?}"
        );
        assert_eq!(
            text(&read, "name"),
            normalised,
            "case {case:?}: expected GetABTest to report {normalised:?}, got {read:?}"
        );

        expected_listed.push(normalised.clone());
        let listed = listed_names(&router).await;
        assert!(
            listed
                .iter()
                .any(|(name, id)| name == &normalised && id == &ab_test_id),
            "case {case:?}: expected ListABTests to report {normalised:?} for {ab_test_id:?}, got {listed:?}"
        );
        assert!(
            !listed.iter().any(|(name, _)| name == &requested && requested != normalised),
            "case {case:?}: expected no summary to carry the spelling as sent, got {listed:?}"
        );
    }

    let listed = listed_names(&router).await;
    assert_eq!(
        listed.len(),
        expected_listed.len(),
        "expected one test per create, got {listed:?}"
    );
}

/// Because the stored name is the normalised one, uniqueness within the account is
/// case-insensitive: a second create whose name differs from a held one only in case is
/// the documented 409 `ConflictException`, worded with the name as the request spelled it,
/// and nothing new is stored.
#[tokio::test]
async fn a_case_variant_of_a_held_name_is_refused_as_a_duplicate() {
    // Rows: the name the first create took, then the name the second create asks for —
    // differing from it only in case (lower against upper, upper against lower, and a
    // mixed spelling of each), plus the identical name as the baseline.
    let cases: Vec<(&str, &str, &str)> = vec![
        ("identical-name", "checkout_flow", "checkout_flow"),
        ("lower-held-upper-asked", "checkout_flow", "CHECKOUT_FLOW"),
        ("upper-held-lower-asked", "CHECKOUT_FLOW", "checkout_flow"),
        ("mixed-held-mixed-asked", "Sim2RealFlow", "sIM2rEALfLOW"),
        ("digits-and-underscores", "ranking_v2_2026", "Ranking_V2_2026"),
    ];

    for (case, held, asked) in cases {
        // A fresh emulator per row, so one row's names cannot collide with another's.
        let (router, _engine) = emulator();

        let (status, created) = create_ab_test(&router, minimal_request(held)).await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected the first create to be accepted, got {status} with {created:?}"
        );
        let first_id = text(&created, "abTestId").to_string();

        let (status, refused) = create_ab_test(&router, minimal_request(asked)).await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected HTTP 409 for a name already held in another case, got {status} with {refused:?}"
        );
        assert!(
            error_code(&refused).ends_with("ConflictException"),
            "case {case:?}: expected the ConflictException error code, got {:?}",
            error_code(&refused)
        );
        let expected = format!("AB test with name '{asked}' already exists");
        assert_eq!(
            text(&refused, "message"),
            expected,
            "case {case:?}: expected the message {expected:?}, got {:?}",
            text(&refused, "message")
        );

        let listed = listed_names(&router).await;
        assert_eq!(
            listed,
            vec![(held.to_ascii_lowercase(), first_id)],
            "case {case:?}: expected the refused create to store nothing, got {listed:?}"
        );
    }
}

/// A rename normalises the new name on the same write path, so every later read reports
/// the normalised spelling (`UpdateABTestResponse` models no `name` member of its own —
/// source: smithy_query query="operation" operation="UpdateABTest"), renaming a test to a
/// case-variant of the name it already holds is accepted (it is the same name), and a
/// rename onto a case-variant of ANOTHER test's name is the 409 worded with the name as
/// asked for.
#[tokio::test]
async fn a_rename_normalises_the_new_name_and_collides_case_insensitively() {
    let (router, _engine) = emulator();
    let case = "rename to a mixed-case name";

    let (status, created) = create_ab_test(&router, minimal_request("first_flow")).await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected the create to be accepted, got {status} with {created:?}"
    );
    let ab_test_id = text(&created, "abTestId").to_string();

    for (row, renamed, expected) in [
        ("mixed-case-rename", "Sim2RealSecondFlow", "sim2realsecondflow"),
        ("upper-case-rename", "THIRD_FLOW", "third_flow"),
        // A rename that only changes the case of the name this test already holds is a
        // rename to the same name, so it is accepted and changes nothing.
        ("case-variant-of-its-own-name", "Third_Flow", "third_flow"),
    ] {
        let (status, updated) =
            update_ab_test(&router, &ab_test_id, json!({"name": renamed})).await;
        assert_eq!(
            status, 202,
            "case {case:?}/{row}: expected HTTP 202 from UpdateABTest, got {status} with {updated:?}"
        );
        let (_, read) = get_ab_test(&router, &ab_test_id).await;
        assert_eq!(
            text(&read, "name"),
            expected,
            "case {case:?}/{row}: sent {renamed:?}, expected GetABTest to report {expected:?}, got {read:?}"
        );
    }

    // A second test cannot be renamed onto a case-variant of the first's held name.
    let (status, second) = create_ab_test(&router, minimal_request("other_flow")).await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected the second create to be accepted, got {status} with {second:?}"
    );
    let second_id = text(&second, "abTestId").to_string();

    let asked = "Third_FLOW";
    let (status, refused) = update_ab_test(&router, &second_id, json!({"name": asked})).await;
    assert_eq!(
        status, 409,
        "case {case:?}: expected HTTP 409 from a rename onto a held name in another case, got {status} with {refused:?}"
    );
    let expected = format!("AB test with name '{asked}' already exists");
    assert_eq!(
        text(&refused, "message"),
        expected,
        "case {case:?}: expected the message {expected:?}, got {:?}",
        text(&refused, "message")
    );
    let (_, held) = get_ab_test(&router, &second_id).await;
    assert_eq!(
        text(&held, "name"),
        "other_flow",
        "case {case:?}: a refused rename must leave the name alone, got {held:?}"
    );
}

/// One `CreateABTest` body with the two variants weighted as the row asks.
fn weighted_request(name: &str, control: i64, treatment: i64) -> Value {
    json!({
        "name": name,
        "gatewayArn": GATEWAY_ARN,
        "roleArn": ROLE_ARN,
        "variants": [
            variant("C", control, target_configuration("control")),
            variant("T1", treatment, target_configuration("treatment")),
        ],
        "evaluationConfig": {"onlineEvaluationConfigArn": EVALUATION_CONFIG_ARN},
    })
}

/// Variant weights that do not total 100 are refused 400 `ValidationException` with the
/// one fixed sentence, whatever total the request came to — on the create and on the
/// update alike.
#[tokio::test]
async fn weights_that_do_not_total_one_hundred_are_refused_with_one_sentence() {
    let (router, _engine) = emulator();

    // `Variant$weight` is range 1..100 and "weights across all variants must sum to 100"
    // (source: smithy_query query="shape" shape="Variant", member="weight"), so the rows
    // span totals below and above 100 at both published bounds of the weight itself.
    let splits = [
        ("smallest-total", 1, 1),
        ("just-below-the-total", 50, 49),
        ("just-above-the-total", 50, 51),
        ("well-above-the-total", 60, 60),
        ("largest-total", 100, 100),
    ];
    // The whole message is fixed text, so every row expects exactly this.
    let expected = "Variant weights must sum to 100";

    for (row, control, treatment) in splits {
        let (status, refused) =
            create_ab_test(&router, weighted_request("weighted_flow", control, treatment)).await;
        assert_eq!(
            status, 400,
            "case {row:?}: expected HTTP 400 from CreateABTest for {control}/{treatment}, got {status} with {refused:?}"
        );
        assert!(
            error_code(&refused).ends_with("ValidationException"),
            "case {row:?}: expected the ValidationException error code, got {:?}",
            error_code(&refused)
        );
        assert_eq!(
            text(&refused, "message"),
            expected,
            "case {row:?}: expected the message {expected:?} for {control}/{treatment}, got {:?}",
            text(&refused, "message")
        );
    }

    let (status, created) = create_ab_test(&router, weighted_request("split_flow", 50, 50)).await;
    assert_eq!(
        status, 202,
        "expected a split totalling 100 to be accepted, got {status} with {created:?}"
    );
    let ab_test_id = text(&created, "abTestId").to_string();

    for (row, control, treatment) in splits {
        let patch = json!({"variants": [
            variant("C", control, target_configuration("control")),
            variant("T1", treatment, target_configuration("treatment")),
        ]});
        let (status, refused) = update_ab_test(&router, &ab_test_id, patch).await;
        assert_eq!(
            status, 400,
            "case {row:?}: expected HTTP 400 from UpdateABTest for {control}/{treatment}, got {status} with {refused:?}"
        );
        assert_eq!(
            text(&refused, "message"),
            expected,
            "case {row:?}: expected UpdateABTest to word it {expected:?}, got {:?}",
            text(&refused, "message")
        );
    }
}
