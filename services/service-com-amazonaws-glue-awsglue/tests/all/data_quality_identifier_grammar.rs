// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH request member a Glue data quality read blames when an
//! identifier it was handed is malformed.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! RULE under test: each request member carrying a service-minted data quality identifier is
//! read against THAT identifier's own prefix grammar, and a value outside it is refused as a
//! malformed request naming the member that failed and the prefix that member's identifiers
//! begin with — never a constraint on another member of the same request. A value inside the
//! grammar is past that refusal and is resolved, so it earns the surface's miss instead.
//!
//! The rows span the FORMS a malformed identifier takes (a bare word, the other minted id of
//! this same surface, a prefix that is nearly the right one, the minted form with its prefix
//! stripped, a case-shifted prefix) crossed with the two reads that take a `StatisticId` and
//! with which of the two members of one request is malformed.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    is_data_quality_statistic_id, messages, new_data_quality_statistic_id,
    DATA_QUALITY_PROFILE_ID_PREFIX, DATA_QUALITY_STATISTIC_ID_PREFIX,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, error_message};

/// A `ProfileId` inside the grammar the service mints one with. Nothing minted it, so a
/// request carrying it is past every grammar refusal and reaches the resolution of the
/// profile.
fn well_formed_profile_id() -> String {
    format!("{DATA_QUALITY_PROFILE_ID_PREFIX}0123456789abcdef0123456789abcdef")
}

/// A `StatisticId` inside the grammar the service mints one with, minted through the one mint
/// the reads share — so the value a row calls "well formed" cannot drift from the form the
/// store produces.
fn well_formed_statistic_id() -> String {
    new_data_quality_statistic_id()
}

/// The forms a value outside a minted id's grammar takes, built from the prefix the store
/// mints that id with so a row cannot quote a stale spelling.
///
/// `minted` is a value the mint produced, whose prefix the last row strips: the minted form
/// without its prefix is inside no grammar at all.
fn malformed_forms(prefix: &str, minted: &str) -> Vec<(&'static str, String)> {
    let without_prefix = minted
        .strip_prefix(prefix)
        .expect("a minted id carries the prefix the store mints it with")
        .to_string();
    vec![
        ("a bare word", "identifier-1".to_string()),
        (
            "the other minted id of this surface",
            format!("{}abc123", other_prefix(prefix)),
        ),
        (
            "a prefix that is nearly the right one",
            format!("{}abc123", nearly_prefix(prefix)),
        ),
        ("the minted form without its prefix", without_prefix),
        ("a case-shifted prefix", format!("{}abc123", prefix.to_uppercase())),
        ("an empty-looking id", "-".to_string()),
    ]
}

/// A prefix that is nearly `prefix`: the same spelling one letter short, which is the typo a
/// client makes and is inside no grammar the service mints an id with.
fn nearly_prefix(prefix: &str) -> String {
    let body = prefix.trim_end_matches('-');
    let kept = body.chars().count().saturating_sub(1);
    format!("{}-", body.chars().take(kept).collect::<String>())
}

/// The prefix of the OTHER minted id of this surface, so a row can offer a profile id where a
/// statistic id belongs and the other way round.
fn other_prefix(prefix: &str) -> &'static str {
    if prefix == DATA_QUALITY_STATISTIC_ID_PREFIX {
        DATA_QUALITY_PROFILE_ID_PREFIX
    } else {
        DATA_QUALITY_STATISTIC_ID_PREFIX
    }
}

/// Assert that `operation` refused `body` as a malformed request worded `expected`.
async fn assert_malformed(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
    expected: &str,
) {
    let (status, answer) = call(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case} through {operation}: expected 400, got {status} with {answer}"
    );
    assert!(
        error_code(&answer).contains("InvalidInputException"),
        "case {case} through {operation}: expected InvalidInputException, got {answer}"
    );
    assert_eq!(
        error_message(&answer),
        expected,
        "case {case} through {operation}: expected the refusal naming the malformed member, got \
         {answer}"
    );
}

/// A `StatisticId` outside the grammar the service mints one with is refused as a malformed
/// request naming the STATISTIC member and its prefix — on the read that requires the member
/// and on the read where it is optional — whatever the other member of the request carries.
#[tokio::test]
async fn a_malformed_statistic_id_is_blamed_on_the_statistic_member() {
    let router = emulator();
    let profile = well_formed_profile_id();
    let expected = messages::invalid_data_quality_statistic_id();
    assert_ne!(
        expected,
        messages::invalid_data_quality_profile_id(),
        "the two members of this surface are refused in their own words"
    );

    let minted = well_formed_statistic_id();
    for (case, statistic_id) in malformed_forms(DATA_QUALITY_STATISTIC_ID_PREFIX, &minted) {
        assert!(
            !is_data_quality_statistic_id(&statistic_id),
            "case {case}: the row must be outside the statistic id grammar, got {statistic_id:?}"
        );
        // The same malformed statistic id beside a well-formed profile id, and beside a
        // profile id that is malformed too: the member the refusal blames is the statistic
        // either way, because that is the member whose value failed its own grammar.
        for (companion, profile_id) in [
            ("a well-formed profile id", profile.clone()),
            ("a malformed profile id", "profile-1".to_string()),
        ] {
            let case = format!("{case} with {companion}");
            for operation in ["GetDataQualityModelResult", "GetDataQualityModel"] {
                assert_malformed(
                    &router,
                    &case,
                    operation,
                    &json!({ "ProfileId": &profile_id, "StatisticId": &statistic_id }),
                    &expected,
                )
                .await;
            }
        }
    }
}

/// The complement, so the rule is pinned in both directions: a malformed `ProfileId` beside a
/// WELL-FORMED `StatisticId` is refused naming the profile member and its prefix.
#[tokio::test]
async fn a_malformed_profile_id_is_blamed_on_the_profile_member() {
    let router = emulator();
    let statistic = well_formed_statistic_id();
    let expected = messages::invalid_data_quality_profile_id();

    let minted_profile = format!("{DATA_QUALITY_PROFILE_ID_PREFIX}abcdef0123456789");
    for (case, profile_id) in malformed_forms(DATA_QUALITY_PROFILE_ID_PREFIX, &minted_profile) {
        for (operation, body) in [
            (
                "GetDataQualityModelResult",
                json!({ "ProfileId": &profile_id, "StatisticId": &statistic }),
            ),
            (
                "GetDataQualityModel",
                json!({ "ProfileId": &profile_id, "StatisticId": &statistic }),
            ),
            (
                "GetDataQualityModel without a statistic",
                json!({ "ProfileId": &profile_id }),
            ),
            (
                "PutDataQualityProfileAnnotation",
                json!({ "ProfileId": &profile_id, "InclusionAnnotation": "INCLUDE" }),
            ),
        ] {
            let name = operation
                .split_whitespace()
                .next()
                .expect("the operation name");
            assert_malformed(&router, case, name, &body, &expected).await;
        }
    }
}

/// A value INSIDE the grammar is past the malformed refusal: the ids the mint produces satisfy
/// the grammar the reads check, so a well-formed pair nothing minted earns the surface's miss
/// — which is what shows the grammar check refuses malformed values only.
#[tokio::test]
async fn a_well_formed_pair_is_resolved_instead_of_refused_as_malformed() {
    let router = emulator();
    let profile = well_formed_profile_id();

    for (case, statistic_id) in [
        ("a minted statistic id", well_formed_statistic_id()),
        (
            "a statistic id of another width",
            format!("{DATA_QUALITY_STATISTIC_ID_PREFIX}abc123"),
        ),
        (
            "the shortest id the grammar admits",
            DATA_QUALITY_STATISTIC_ID_PREFIX.to_string(),
        ),
    ] {
        assert!(
            is_data_quality_statistic_id(&statistic_id),
            "case {case}: the row must be inside the statistic id grammar, got {statistic_id:?}"
        );
        for operation in ["GetDataQualityModelResult", "GetDataQualityModel"] {
            let (status, answer) = call(
                &router,
                operation,
                &json!({ "ProfileId": &profile, "StatisticId": &statistic_id }),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case} through {operation}: expected 400, got {status} with {answer}"
            );
            assert!(
                error_code(&answer).contains("EntityNotFoundException"),
                "case {case} through {operation}: a well-formed address that resolves to nothing \
                 is the surface's miss, got {answer}"
            );
            assert_eq!(
                error_message(&answer),
                messages::DATA_QUALITY_MODEL_RESULT_NOT_FOUND,
                "case {case} through {operation}: expected the shared model-result miss, got \
                 {answer}"
            );
        }
    }
}
