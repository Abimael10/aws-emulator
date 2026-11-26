// SPDX-License-Identifier: CC0-1.0

//! ONE sentence for every way a create reaches this service's name-uniqueness guard.
//!
//! RULE: a create refused because the identifier it sent already names a resource this
//! service holds answers 409 `ConflictException` with that error class's own sentence, which
//! is FIXED text carrying no identifier at all — not the name the request sent, not the ARN
//! of the record it collided with, not the account or the region either. `ConflictException`
//! models a single `Message` member, so that prose is the whole detail a client branches on,
//! and it must read the same however the refusal was reached.
//!
//! Where [`crate::refusal_prose`] owns the WORDING and [`crate::slo_conflict_invariance`] the
//! invariance over the refused BODY, this suite spans the ENTRY PATHS into the one guard: a
//! repeated create, a repeated create under the other service level indicator, a race of two
//! callers, a race of eight, and a create under a name that was freed by a delete and taken
//! again. Every refusal any of them earns must be byte-identical.
//!
//! The admitted side is driven throughout: exactly one racer wins, a free name is created on
//! the same store, and a name released by a delete is creatable again — so the refusals are
//! about stored state rather than about the request.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::refusal_prose::SAME_IDENTIFIER_ALREADY_EXISTS;
use crate::resource_tags::slo_arn;
use crate::slo_evaluation_kind::request_based_sli_config;
use crate::slo_lifecycle::{
    call, create, name_path_segment, router, sli_config, ACCOUNT, REGION,
};

/// The name forms the rule is asserted over: the character classes the modelled
/// `ServiceLevelObjectiveName` pattern `^[0-9A-Za-z][-._0-9A-Za-z ]{0,126}[0-9A-Za-z]$`
/// admits (`smithy_query(query="operation",operation="CreateServiceLevelObjective")`),
/// including the shortest name it admits and one of its maximum length. None of them is a
/// substring of the sentence under test, so "the sentence names no identifier" is checkable
/// on each.
fn names() -> Vec<String> {
    vec![
        "taken-identifier-slo".to_string(),
        "taken.identifier.slo".to_string(),
        "taken_identifier_slo".to_string(),
        "taken identifier slo".to_string(),
        "8k".to_string(),
        format!("t{}z", "k".repeat(126)),
    ]
}

/// Assert `message` is the taken-identifier sentence and carries no identifier of any kind:
/// not the name the request sent, not the ARN that addresses the record it collided with,
/// and nothing of the account or region that ARN would name.
fn assert_identifier_free(case: &str, name: &str, message: &str) {
    assert_eq!(
        message, SAME_IDENTIFIER_ALREADY_EXISTS,
        "case {case}: expected {SAME_IDENTIFIER_ALREADY_EXISTS:?}, got {message:?}"
    );
    for withheld in [name, &slo_arn(name), "arn:", ACCOUNT, REGION] {
        assert!(
            !message.contains(withheld),
            "case {case}: expected a sentence carrying no {withheld:?}, got {message:?}"
        );
    }
}

/// Read the refusal of one create: its status, its error code and its message.
fn refusal(case: &str, status: u16, answer: &Value) -> String {
    assert_eq!(
        status, 409,
        "case {case}: expected HTTP 409 for a taken identifier, got {status} with {answer}"
    );
    assert_eq!(
        answer["__type"].as_str(),
        Some("ConflictException"),
        "case {case}: expected a ConflictException, got {answer}"
    );
    answer["Message"].as_str().unwrap_or_default().to_string()
}

/// RULE: every SEQUENTIAL entry into the guard — the same request again, the other service
/// level indicator under the same name, and a name re-taken after a delete freed it — reads
/// the one identifier-free sentence.
#[tokio::test]
async fn every_sequential_conflict_reads_one_identifier_free_sentence() {
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for name in names() {
        let router = router();
        let (status, created) = create(
            &router,
            json!({ "Name": name, "Description": "the stored one",
                    "SliConfig": sli_config("checkout", "GET /cart", 100.0) }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected the first create to succeed, got {status} with {created}"
        );

        for (why, body) in [
            (
                "the same request again",
                json!({ "Name": name, "Description": "the stored one",
                        "SliConfig": sli_config("checkout", "GET /cart", 100.0) }),
            ),
            (
                "the other service level indicator",
                json!({ "Name": name, "RequestBasedSliConfig": request_based_sli_config("x") }),
            ),
            (
                "another body entirely",
                json!({ "Name": name, "Description": "a different description",
                        "SliConfig": sli_config("orders", "POST /orders", 9.0),
                        "Goal": { "AttainmentGoal": 95.0 },
                        "Tags": [{ "Key": "team", "Value": "orders" }] }),
            ),
        ] {
            let case = format!("{name:?} / {why}");
            let (status, answer) = create(&router, body).await;
            let message = refusal(&case, status, &answer);
            assert_identifier_free(&case, &name, &message);
            seen.insert(message);
        }

        // The admitted side: a DELETE frees the identifier, so the very same create is served
        // — the guard answers stored state, not the request.
        let segment = name_path_segment(&name);
        let (status, deleted) = call(&router, "DELETE", &format!("/slo/{segment}"), None).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected the stored SLO to be deletable, got {status} with {deleted}"
        );
        let (status, recreated) = create(
            &router,
            json!({ "Name": name, "Description": "the second one",
                    "SliConfig": sli_config("checkout", "GET /cart", 100.0) }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected a freed identifier to be creatable again, got {status} \
             with {recreated}"
        );

        // And the identifier is taken again, by a DIFFERENT record than the first: the same
        // sentence, about a record the first refusals never saw.
        let case = format!("{name:?} / taken again after a delete");
        let (status, answer) = create(
            &router,
            json!({ "Name": name, "SliConfig": sli_config("checkout", "GET /cart", 100.0) }),
        )
        .await;
        let message = refusal(&case, status, &answer);
        assert_identifier_free(&case, &name, &message);
        seen.insert(message);

        // Round trip: the refused creates wrote nothing, so the record under that name is
        // still the one the admitted create answered.
        let (status, fetched) = call(&router, "GET", &format!("/slo/{segment}"), None).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected the stored SLO to be readable, got {status} with {fetched}"
        );
        assert_eq!(
            fetched["Slo"], recreated["Slo"],
            "case {name:?}: expected a refused create to leave the record untouched, got {:?}",
            fetched["Slo"]
        );
    }

    assert_eq!(
        seen.len(),
        1,
        "expected one sentence for every sequential conflict, got {seen:?}"
    );
}

/// RULE, the CONCURRENT entry into the same guard: of N creates arriving together on one
/// name exactly one is admitted, and each loser reads the SAME identifier-free sentence a
/// sequential duplicate reads.
///
/// The rows differ along how many callers race (two, and a pool large enough that losers are
/// refused from several threads) and along the name's form, and each racer sends a DIFFERENT
/// body, so nothing but the identifier can be what the refusal is about.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_racing_loser_reads_the_same_identifier_free_sentence() {
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for (racers, name) in [
        (2usize, "raced-identifier-slo"),
        (8, "raced identifier slo"),
        (8, "8k"),
    ] {
        let case = format!("{racers} racers on {name:?}");
        let router = Arc::new(router());

        let mut tasks = Vec::new();
        for racer in 0..racers {
            let router = Arc::clone(&router);
            let body = json!({
                "Name": name,
                "Description": format!("racer {racer}"),
                "SliConfig": sli_config("checkout", "GET /cart", 100.0 + racer as f64),
            });
            tasks.push(tokio::spawn(async move { create(&router, body).await }));
        }

        let mut winners: Vec<Value> = Vec::new();
        let mut losers: Vec<(u16, Value)> = Vec::new();
        for task in tasks {
            let (status, answer) = task.await.expect("the racing create completed");
            if status == 200 {
                winners.push(answer);
            } else {
                losers.push((status, answer));
            }
        }

        assert_eq!(
            winners.len(),
            1,
            "case {case}: expected exactly one create to be admitted, got {} with {winners:?}",
            winners.len()
        );
        assert_eq!(
            losers.len(),
            racers - 1,
            "case {case}: expected every other racer to be refused, got {} with {losers:?}",
            losers.len()
        );

        for (index, (status, answer)) in losers.iter().enumerate() {
            let label = format!("{case} / loser {index}");
            let message = refusal(&label, *status, answer);
            assert_identifier_free(&label, name, &message);
            assert!(
                !message.contains("racer"),
                "case {label}: expected a sentence carrying nothing of the refused body, got \
                 {message:?}"
            );
            seen.insert(message);
        }

        // One winner means one stored record, and it is the one the admitted create answered.
        let (status, fetched) = call(
            &router,
            "GET",
            &format!("/slo/{}", name_path_segment(name)),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: expected the admitted create's record to be readable, got {status} \
             with {fetched}"
        );
        assert_eq!(
            fetched["Slo"], winners[0]["Slo"],
            "case {case}: expected the stored record to be the winner's, got {:?}",
            fetched["Slo"]
        );
    }

    assert_eq!(
        seen.len(),
        1,
        "expected one sentence for every racing loser, got {seen:?}"
    );
    assert_eq!(
        seen.iter().next().map(String::as_str),
        Some(SAME_IDENTIFIER_ALREADY_EXISTS),
        "expected the racing losers to read the sentence a sequential duplicate reads, got \
         {seen:?}"
    );
}
