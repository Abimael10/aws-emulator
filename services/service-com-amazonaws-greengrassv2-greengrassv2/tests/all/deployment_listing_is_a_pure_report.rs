// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests pinning `ListDeployments` as a pure REPORT of the deployments this
//! account recorded, and pinning the `historyFilter` it accepts to the modelled enum.
//!
//! The rules under test:
//!
//! * listing is a report, so it changes nothing a later caller sees: however many
//!   listings are made and wherever in a deployment's lifecycle they fall, every listing
//!   answers the same body, each listed deployment's own reader still reports exactly what
//!   it reported before, and the `nextToken` a page hands back is not consumed — resuming
//!   the same cursor twice answers the same page. Only a WRITE (a cancel) moves what a
//!   later listing reports, and the reads before it leave that write the state it would
//!   have found without them;
//! * `historyFilter` is a `DeploymentHistoryFilter`, so a value outside that enum names no
//!   history and is refused as bad input — HTTP 400 `ValidationException` naming the member
//!   that failed — rather than answered with a list; and a refusal writes nothing, so the
//!   listing after it is the listing before it.
//!
//! The rows differ along what the rules generalise over: how many listings are interleaved
//! at each point of the lifecycle (none, one, several), which queries those listings use
//! (the default, the whole history, one target, one page at a time), and the FORM of the
//! rejected filter value (wrong case, a value no enum member names, empty, two values at
//! once, a padded one) — not along the deployment names, which the rules say nothing about.
//!
//! Every assertion drives the real `restJson1` front door through the harness the sibling
//! suite `tests/all/deployment_history_and_tag_removal.rs` owns, and is judged on the HTTP
//! status and the JSON body, so a listing served from anything but what the creates
//! persisted — or one that quietly disturbed a deployment — fails here.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::deployment_history_and_tag_removal::{
    cancel_deployment, create_deployment, deploy_to, emulator, encoded_label, error_code,
    get_deployment, list_deployments, listed_ids, text, thing_group_target, thing_target,
};

/// The code the modelled 400 carries
/// (`smithy_query(query="shape",shape="ValidationException")`: `client`, HTTP 400).
const VALIDATION_CODE: &str = "ValidationException";

/// The `DeploymentHistoryFilter` values the model declares
/// (`smithy_query(query="shape",shape="DeploymentHistoryFilter")`: `ALL`, `LATEST_ONLY`) —
/// the only two a listing accepts, and the catalog the rejected forms below are built
/// against.
const HISTORY_FILTER_VALUES: [&str; 2] = ["ALL", "LATEST_ONLY"];

/// The `DeploymentStatus` a fresh deployment carries, and the one a cancel moves it to.
const STATUS_ACTIVE: &str = "ACTIVE";
const STATUS_CANCELED: &str = "CANCELED";

/// One `ListDeployments` that must answer 200, as its body.
async fn listing(router: &MultiServiceRouter, case: &str, query: &str) -> Value {
    let (status, body) = list_deployments(router, query).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListDeployments {query:?} must answer 200, got {status} {body}"
    );
    body
}

/// The queries one row lists through. Each is a different reading of the same recorded
/// history — the default page of the latest revisions, the whole history, one target's
/// history, and a single-entry page — so a listing that disturbed the store through any of
/// them shows up in the comparison the caller makes.
fn queries(target: &str) -> Vec<(&'static str, String)> {
    vec![
        ("the default listing", String::new()),
        ("the whole history", "historyFilter=ALL".to_string()),
        (
            "one target's whole history",
            format!("historyFilter=ALL&targetArn={}", encoded_label(target)),
        ),
        (
            "one entry at a time",
            "historyFilter=ALL&maxResults=1".to_string(),
        ),
    ]
}

/// List `reads` times through each query of [`queries`] and assert every listing answers
/// the body `expected` holds for that query, filling `expected` from the first listing of
/// each query when it holds none yet.
///
/// That the last listing still agrees with the first is the report rule: a listing that
/// consumed, re-ordered or re-stamped anything would show up here.
async fn listings_agree(
    router: &MultiServiceRouter,
    case: &str,
    stage: &str,
    target: &str,
    reads: usize,
    expected: &mut Vec<(String, Value)>,
) {
    for (query_case, query) in queries(target) {
        for read in 1..=reads.max(1) {
            let body = listing(router, case, &query).await;
            match expected.iter().find(|(stored, _)| stored == &query) {
                Some((_, first)) => assert_eq!(
                    &body, first,
                    "case {case:?} {stage} {query_case:?} read {read}: expected the body \
                     the first listing answered {first}, got {body}"
                ),
                None => expected.push((query.clone(), body)),
            }
            // Only the row's own read count is driven; the extra pass when a row asks for
            // no reads is what records the body the stage is compared against.
            if reads == 0 {
                break;
            }
        }
    }
}

/// Every deployment's own reader, by id: what a listing must leave untouched.
async fn stored(router: &MultiServiceRouter, case: &str, ids: &[String]) -> Vec<(String, Value)> {
    let mut readings = Vec::new();
    for id in ids {
        let (status, body) = get_deployment(router, id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetDeployment {id:?} must answer 200, got {status} {body}"
        );
        readings.push((id.clone(), body));
    }
    readings
}

#[tokio::test]
async fn listing_the_deployments_reports_them_without_changing_them() {
    // (case, listings interleaved at each point of the lifecycle)
    for (case, reads) in [
        ("no listings interleaved at all", 0usize),
        ("one listing at each point", 1),
        ("a handful of listings at each point", 3),
        ("many listings at each point", 7),
    ] {
        let router = emulator();
        let revised = thing_group_target("ReportedFleet");
        let lone = thing_target("ReportedCore");

        // Nothing is recorded yet, so every listing answers the same empty list — and none
        // of those listings may bring a deployment into being.
        let mut empty: Vec<(String, Value)> = Vec::new();
        listings_agree(&router, case, "before any deployment", &revised, reads, &mut empty).await;
        for (query, body) in &empty {
            assert_eq!(
                listed_ids(body),
                Vec::<String>::new(),
                "case {case:?}: {query:?} on an account with no deployment must report an \
                 empty list, got {body}"
            );
        }

        // Two revisions of one target and one deployment to another: enough that the
        // default filter, the whole history and a one-entry page are each a different
        // answer, so a listing that disturbed the order or the latest-revision flag is
        // visible in any of them.
        let mut ids = vec![deploy_to(&router, &lone).await];
        for revision in 1..=2 {
            ids.push(
                create_deployment(
                    &router,
                    &json!({ "targetArn": revised, "deploymentName": format!("r{revision}") }),
                )
                .await,
            );
        }

        let before = stored(&router, case, &ids).await;
        let mut recorded: Vec<(String, Value)> = Vec::new();
        listings_agree(
            &router,
            case,
            "with the deployments recorded",
            &revised,
            reads,
            &mut recorded,
        )
        .await;

        // The listings consumed nothing: every deployment's own reader still answers
        // exactly what it answered before them.
        let after = stored(&router, case, &ids).await;
        assert_eq!(
            after, before,
            "case {case:?}: listing must leave every deployment's reader answering what it \
             answered before, got {after:?}"
        );

        // A cursor is a token, not a turn: resuming the same `nextToken` twice answers the
        // same page, so paging a listing twice reads the same history.
        let first_page = listing(&router, case, "historyFilter=ALL&maxResults=1").await;
        let cursor = text(&first_page, "nextToken");
        assert!(
            !cursor.is_empty(),
            "case {case:?}: a page of one over {} deployments must hand back a cursor, got \
             {first_page}",
            ids.len()
        );
        let resumed = listing(
            &router,
            case,
            &format!("historyFilter=ALL&maxResults=1&nextToken={cursor}"),
        )
        .await;
        let resumed_again = listing(
            &router,
            case,
            &format!("historyFilter=ALL&maxResults=1&nextToken={cursor}"),
        )
        .await;
        assert_eq!(
            resumed, resumed_again,
            "case {case:?}: resuming the same cursor twice must answer the same page, got \
             {resumed} then {resumed_again}"
        );
        assert_ne!(
            listed_ids(&resumed),
            listed_ids(&first_page),
            "case {case:?}: the resumed page must start after the first, got {resumed}"
        );

        // Only a WRITE moves what a later listing reports — and the listings before it left
        // this cancel the deployment it would have found without them.
        let newest = ids.last().expect("the fixture recorded a deployment").clone();
        assert_eq!(
            text(
                &before
                    .iter()
                    .find(|(id, _)| id == &newest)
                    .expect("the newest deployment is readable")
                    .1,
                "deploymentStatus"
            ),
            STATUS_ACTIVE,
            "case {case:?}: a fresh deployment is {STATUS_ACTIVE}"
        );
        let (status, cancelled) = cancel_deployment(&router, &newest).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CancelDeployment must answer 200, got {status} {cancelled}"
        );

        let mut cancelled_listings: Vec<(String, Value)> = Vec::new();
        listings_agree(
            &router,
            case,
            "after the cancel",
            &revised,
            reads,
            &mut cancelled_listings,
        )
        .await;
        let (_, whole_history) = list_deployments(&router, "historyFilter=ALL").await;
        let entry = whole_history["deployments"]
            .as_array()
            .expect("a listing with entries")
            .iter()
            .find(|entry| text(entry, "deploymentId") == newest)
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the cancelled deployment must still be listed")
            });
        assert_eq!(
            text(&entry, "deploymentStatus"),
            STATUS_CANCELED,
            "case {case:?}: the listing must report the cancelled status the write left, \
             got {entry}"
        );
    }
}

#[tokio::test]
async fn a_history_filter_outside_the_modelled_enum_is_refused() {
    let router = emulator();
    let target = thing_group_target("FilteredFleet");
    let first = deploy_to(&router, &target).await;
    let second = deploy_to(&router, &target).await;

    // Both modelled values are accepted — including the one the default does not name —
    // and each reports the history that value selects.
    for value in HISTORY_FILTER_VALUES {
        let body = listing(&router, value, &format!("historyFilter={value}")).await;
        let expected = if value == "ALL" {
            vec![second.clone(), first.clone()]
        } else {
            vec![second.clone()]
        };
        assert_eq!(
            listed_ids(&body),
            expected,
            "case {value:?}: expected exactly {expected:?} in that order, got {body}"
        );
    }

    // The forms differ along HOW the value misses the enum: the wrong case, a value no
    // member names, no value at all, two values at once, and a padded one.
    let rejected = [
        ("the wrong case", "all".to_string()),
        ("the wrong case, the other value", "latest_only".to_string()),
        ("a value no member names", "LATEST".to_string()),
        ("an empty value", String::new()),
        ("two values at once", "ALL,LATEST_ONLY".to_string()),
        ("a padded value", "ALL%20".to_string()),
    ];

    let before = listing(&router, "before the refusals", "historyFilter=ALL").await;
    for (case, value) in rejected {
        let (status, body) = list_deployments(&router, &format!("historyFilter={value}")).await;
        assert_eq!(
            status, 400,
            "case {case:?} ({value:?}): expected 400, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            VALIDATION_CODE,
            "case {case:?} ({value:?}): expected {VALIDATION_CODE}, got {body}"
        );
        assert!(
            body.to_string().contains("historyFilter"),
            "case {case:?} ({value:?}): the refusal must name the member that failed, got \
             {body}"
        );
        assert!(
            body.get("deployments").is_none(),
            "case {case:?} ({value:?}): a refusal reports no list, got {body}"
        );
    }

    // A refusal writes nothing, so the history after them is the history before them.
    let after = listing(&router, "after the refusals", "historyFilter=ALL").await;
    assert_eq!(
        after, before,
        "case the refused filters: the recorded history must be untouched, got {after}"
    );
}
