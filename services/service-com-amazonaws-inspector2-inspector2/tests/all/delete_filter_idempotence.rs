// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DeleteFilter`'s idempotence, driven through `CreateFilter`,
//! `DeleteFilter` and `ListFilters` over the real `MultiServiceRouter` — routing, restJson1
//! deserialization, modelled-constraint validation, the handler and restJson1
//! serialization — so what is asserted here is the wire behaviour a client sees.
//!
//! The rule under test: `DeleteFilter` answers HTTP 200 echoing the `arn` the request named
//! for EVERY well-formed filter ARN, whether or not a filter is stored under it — the call's
//! whole postcondition is that no filter is stored under that ARN, and it reports nothing
//! but the ARN itself, so a retry, a defensive teardown and two callers racing on one ARN
//! all observe success. Only a value that is not a well-formed filter ARN is refused, with
//! HTTP 400 `ValidationException`.

use std::sync::Arc;

use serde_json::json;

use crate::filter_arn_and_tags::{create_filter, unknown_filter_arns, DELETE};
use crate::filters_and_enablement::{post_in, router};

/// `ListFilters`' route.
/// Source: `smithy_query(query="operation", operation="ListFilters")` — `POST
/// /filters/list`.
const LIST_URI: &str = "/filters/list";

/// Delete `arn` once and assert the answer is the idempotent success: HTTP 200 whose only
/// `arn` is the one the request named.
async fn assert_delete_echoes(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    arn: &str,
) {
    let (status, error_type, response) =
        post_in(router, "us-east-1", DELETE.0, &json!({ "arn": arn })).await;
    assert_eq!(
        status, 200,
        "case {case}: expected 200, got {status} / {response} (errortype {error_type:?})"
    );
    assert_eq!(
        response["arn"].as_str(),
        Some(arn),
        "case {case}: expected the requested ARN echoed back, got {response}"
    );
}

/// The ARNs of the filters `ListFilters` reports, in listing order.
async fn listed_arns(router: &smithy_server_runtime::MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, _, listed) = post_in(router, "us-east-1", LIST_URI, &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case}: ListFilters: expected 200, got {status} / {listed}"
    );
    listed["filters"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: expected a filter list, got {listed}"))
        .iter()
        .map(|filter| {
            filter["arn"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: expected each filter its ARN, got {listed}"))
                .to_string()
        })
        .collect()
}

/// A well-formed filter ARN the store does not hold is deleted successfully, twice over,
/// and the delete touches nothing else.
///
/// The rows are the shared table of well-formed ARNs this emulator never minted, which
/// differ along which part of the ARN is unfamiliar — the partition, the region, the
/// account, the owner (an account and an organization) and the id. None of them names a
/// stored filter, and every one of them is still the caller's to ask about.
#[tokio::test]
async fn deleting_a_well_formed_arn_that_names_no_stored_filter_succeeds() {
    for (row, candidate) in unknown_filter_arns() {
        let router = router();
        let survivor = create_filter(&router, "survivor").await;

        assert_delete_echoes(&router, &format!("{row} / first delete"), &candidate).await;
        assert_delete_echoes(&router, &format!("{row} / second delete"), &candidate).await;

        assert_eq!(
            listed_arns(&router, row).await,
            vec![survivor],
            "case {row}: deleting an ARN the store does not hold must leave every stored \
             filter alone"
        );
    }
}

/// A filter this emulator minted is deleted by the first call and by every call after it:
/// each answers 200 with the minted ARN, and the record is gone from `ListFilters` from the
/// first delete on.
///
/// The rows differ along HOW MANY times the same ARN is deleted — one call, a retry, and
/// several retries — which is what an SDK-level retry after a timeout and a cleanup path
/// that deletes defensively each look like.
#[tokio::test]
async fn deleting_one_filter_repeatedly_answers_the_same_echo() {
    for deletes in [1usize, 2, 3, 5] {
        let case = format!("{deletes} deletes");
        let router = router();
        let arn = create_filter(&router, "doomed").await;

        for attempt in 1..=deletes {
            assert_delete_echoes(&router, &format!("{case} / attempt {attempt}"), &arn).await;
            assert_eq!(
                listed_arns(&router, &case).await,
                Vec::<String>::new(),
                "case {case}: the filter must be gone from the first delete on (attempt \
                 {attempt})"
            );
        }
    }
}

/// Callers racing on one ARN ALL observe success: the delete is a single state transition,
/// so no racer is told the filter it asked to delete is missing.
///
/// The rows differ along how many callers race, from the two of a pair of workers up to a
/// whole batch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_deletes_of_one_filter_all_succeed() {
    for racers in [2usize, 4, 8] {
        let case = format!("{racers} concurrent deletes");
        let router = Arc::new(router());
        let arn = create_filter(&router, "contended").await;

        let mut racing = Vec::with_capacity(racers);
        for _ in 0..racers {
            let router = Arc::clone(&router);
            let arn = arn.clone();
            racing.push(tokio::spawn(async move {
                post_in(&router, "us-east-1", DELETE.0, &json!({ "arn": arn })).await
            }));
        }

        for (index, racer) in racing.into_iter().enumerate() {
            let (status, error_type, response) = racer
                .await
                .unwrap_or_else(|error| panic!("case {case}: racer {index} panicked: {error}"));
            assert_eq!(
                status, 200,
                "case {case}: racer {index}: expected 200, got {status} / {response} (errortype \
                 {error_type:?})"
            );
            assert_eq!(
                response["arn"].as_str(),
                Some(arn.as_str()),
                "case {case}: racer {index}: expected the requested ARN echoed back, got \
                 {response}"
            );
        }

        assert_eq!(
            listed_arns(&router, &case).await,
            Vec::<String>::new(),
            "case {case}: the contended filter must be gone once the racers are done"
        );
    }
}

/// The other half of the rule: a value that is not a well-formed filter ARN is still
/// refused — HTTP 400 `ValidationException` quoting the value exactly as sent — and the
/// refused request leaves the store alone.
///
/// The rows differ along WHICH part of the ARN shape is wrong: no ARN at all, a bare name,
/// another service's namespace, another resource type of this service, and a filter ARN
/// missing its id.
#[tokio::test]
async fn a_malformed_filter_arn_is_still_refused() {
    let malformed: [(&str, &str); 5] = [
        ("not an arn at all", "not-an-arn-at-all"),
        ("a bare name", "my-filter"),
        (
            "another service's namespace",
            "arn:aws:inspector:us-east-1:000000000000:owner/000000000000/filter/1",
        ),
        (
            "another resource type of this service",
            "arn:aws:inspector2:us-east-1:000000000000:owner/000000000000/cis-configuration/\
             11111111-2222-3333-4444-555555555555",
        ),
        (
            "a filter ARN with no id",
            "arn:aws:inspector2:us-east-1:000000000000:owner/000000000000/filter/",
        ),
    ];

    for (case, candidate) in malformed {
        let router = router();
        let survivor = create_filter(&router, "survivor").await;

        let (status, error_type, response) =
            post_in(&router, "us-east-1", DELETE.0, &json!({ "arn": candidate })).await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} / {response}"
        );
        assert_eq!(
            error_type.as_deref(),
            Some("ValidationException"),
            "case {case}: expected ValidationException, got {error_type:?} / {response}"
        );
        assert_eq!(
            response["message"].as_str(),
            Some(format!("Invalid Filter Arn {candidate}").as_str()),
            "case {case}: unexpected refusal wording, got {response}"
        );

        assert_eq!(
            listed_arns(&router, case).await,
            vec![survivor],
            "case {case}: a refused request must leave the store alone"
        );
    }
}
