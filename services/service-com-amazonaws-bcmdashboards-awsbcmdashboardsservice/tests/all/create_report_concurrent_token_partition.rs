// SPDX-License-Identifier: CC0-1.0

//! What a CONCURRENT fleet of `CreateScheduledReport` calls carrying SEVERAL distinct
//! `clientToken`s is answered, end to end over the wire.
//!
//! The rule under test: a `clientToken` keys at most ONE create and keys only its own, so
//! of a fleet of concurrent calls exactly one call PER DISTINCT token is served (HTTP 200
//! and one minted scheduled-report ARN) while every other call sharing that token is
//! refused with the modelled `ConflictException` (HTTP 409) — the creation-in-progress text
//! while the call holding the token has not stored its report yet, the already-exists text
//! naming that stored ARN once it has — and a call carrying NO token is keyed by nothing, so
//! it always creates. The account therefore ends up holding exactly one report per distinct
//! token plus one per token-less call.
//!
//! The single-token fleet is `tests/all/create_report_concurrent_token.rs`; this suite is
//! the same rule over the PARTITION of a fleet, which the single-token rows cannot show: a
//! refusal that answers a token with another token's claim — or one claim shared by the
//! whole fleet, which would leave a single winner for all of it — passes every single-token
//! row and fails here.
//!
//! The rows differ along what the rule generalises over: how many distinct tokens race at
//! once, how many callers crowd each of them (one, two, three, four), and how many
//! token-less calls run beside them (none, some, all of them).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use service_com_amazonaws_bcmdashboards_awsbcmdashboardsservice::state::service_state;

use super::create_report_client_token::stored_arns;
// The ARN shape a minted scheduled-report ARN carries, the racer fan-out and the reading of
// the two admitted conflict wordings all come from the single-token fleet suite, so this
// suite restates none of them; what it adds is the KEY each racing call presented.
use super::create_report_concurrent_token::{
    assert_conflict_refusal, race_keyed, ARN_PREFIX, ARN_RESOURCE,
};
// The terminal wording itself, from the suite that pins both texts character for character.
use super::create_report_conflict_message::already_exists_message;
use super::dashboard_lifecycle::emulator;
use super::scheduled_report_lifecycle::{acceptable_period, source_dashboard, EXECUTION_ROLE_ARN};

/// Which key a racing call presented: the index of its token group, or `None` for a call
/// that carried no `clientToken` at all.
type Key = Option<usize>;

/// One row: how a concurrent fleet is partitioned over `clientToken`s.
struct PartitionCase {
    label: &'static str,
    /// One entry per distinct token, holding how many concurrent calls present it.
    crowds: &'static [usize],
    /// How many concurrent calls carry no token at all.
    untokened: usize,
}

fn cases() -> &'static [PartitionCase] {
    &[
        PartitionCase {
            label: "two tokens, two callers each",
            crowds: &[2, 2],
            untokened: 0,
        },
        PartitionCase {
            label: "three tokens crowded one, two and three deep",
            crowds: &[1, 2, 3],
            untokened: 0,
        },
        PartitionCase {
            label: "a token four callers deep beside a token with one",
            crowds: &[4, 1],
            untokened: 0,
        },
        PartitionCase {
            label: "two crowded tokens beside two token-less creates",
            crowds: &[2, 3],
            untokened: 2,
        },
        PartitionCase {
            label: "no token at all, every call its own create",
            crowds: &[],
            untokened: 4,
        },
    ]
}

/// The token the calls of group `group` present. Every character is in the printable range
/// `ClientToken`'s pattern admits (length 1..128, pattern `^[\u0021-\u007E]+$`, source:
/// `smithy_query(query="shape",shape="ClientToken")`).
fn token_of(group: usize) -> String {
    format!("partition-token-{group:02}")
}

/// The request a racing call issues: identical for every caller of one group, so what
/// separates the groups is the token alone.
fn create_request(key: Key, dashboard_arn: &str, widget_id: &str) -> Value {
    let name = match key {
        Some(group) => format!("Partition Rollup {group}"),
        None => "Partition Unkeyed Rollup".to_string(),
    };
    let mut request = json!({
        "scheduledReport": {
            "name": name,
            "dashboardArn": dashboard_arn,
            "scheduledReportExecutionRoleArn": EXECUTION_ROLE_ARN,
            "scheduleConfig": {
                "scheduleExpression": "cron(0 8 ? * MON *)",
                "scheduleExpressionTimeZone": "UTC",
                // Built relative to this instant: each bound of a submitted period is held
                // to a rule measured from the request that carries it.
                "schedulePeriod": acceptable_period()
            },
            "widgetIds": [widget_id]
        },
        "resourceTags": [{"key": "team", "value": "finops"}]
    });
    if let Some(group) = key {
        request["clientToken"] = json!(token_of(group));
    }
    request
}

/// The fleet a row describes, interleaved so the calls of different groups overlap rather
/// than one group's whole crowd running before the next group starts.
fn fleet(case: &PartitionCase, dashboard_arn: &str, widget_id: &str) -> Vec<(Key, Value)> {
    let deepest = case.crowds.iter().copied().max().unwrap_or(0);
    let mut requests: Vec<(Key, Value)> = Vec::new();
    for round in 0..deepest.max(case.untokened) {
        for (group, crowd) in case.crowds.iter().enumerate() {
            if round < *crowd {
                requests.push((
                    Some(group),
                    create_request(Some(group), dashboard_arn, widget_id),
                ));
            }
        }
        if round < case.untokened {
            requests.push((None, create_request(None, dashboard_arn, widget_id)));
        }
    }
    requests
}

/// The ARN a served call answers, asserted to be a minted scheduled-report ARN.
fn minted_arn(case: &str, key: Key, body: &Value) -> String {
    let arn = body["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?} ({key:?}): a served create answers an arn, got {body}"))
        .to_string();
    assert!(
        arn.starts_with(ARN_PREFIX) && arn.contains(ARN_RESOURCE),
        "case {case:?} ({key:?}): expected a minted scheduled-report arn, got {arn:?}"
    );
    arn
}

/// Assert the refusal every loser of `group` is answered with: the modelled conflict in one of
/// its two wordings, read through the sibling suite's one reading of them, and the
/// already-exists wording naming THIS group's winner rather than another group's.
fn assert_refused_for(case: &str, group: usize, winner: &str, status: u16, body: &Value) {
    let case = format!("{case} (token {group})");
    assert_conflict_refusal(&case, winner, status, body);
    let message = body["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains(&token_of(group)),
        "case {case:?}: the presented clientToken must not appear in the refusal, got \
         {message:?}"
    );
}

/// RULE: a concurrent fleet leaves exactly one winner per distinct `clientToken`, plus one
/// per token-less call, and the account holds exactly those reports.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_concurrent_fleet_leaves_one_winner_per_distinct_client_token() {
    for case in cases() {
        let label = case.label;
        let (router, engine) = emulator();
        let (dashboard_arn, widget_id) = source_dashboard(&router, "PartitionTokenSource").await;

        let outcomes = race_keyed(&router, fleet(case, &dashboard_arn, &widget_id)).await;
        assert_eq!(
            outcomes.len(),
            case.crowds.iter().sum::<usize>() + case.untokened,
            "case {label:?}: every call of the fleet must be answered, got {outcomes:?}"
        );

        // Grouped by the key each call presented, so each token is judged on its own crowd.
        let mut by_key: BTreeMap<Key, Vec<(u16, Value)>> = BTreeMap::new();
        for (key, status, body) in outcomes {
            by_key.entry(key).or_default().push((status, body));
        }

        let mut winners: BTreeSet<String> = BTreeSet::new();
        for (group, crowd) in case.crowds.iter().enumerate() {
            let answered = by_key.get(&Some(group)).map(Vec::as_slice).unwrap_or(&[]);
            assert_eq!(
                answered.len(),
                *crowd,
                "case {label:?} (token {group}): its whole crowd must be answered, got {answered:?}"
            );
            let served: Vec<&Value> = answered
                .iter()
                .filter(|(status, _)| *status == 200)
                .map(|(_, body)| body)
                .collect();
            assert_eq!(
                served.len(),
                1,
                "case {label:?} (token {group}): exactly one call presenting the token is \
                 served, got {answered:?}"
            );
            let winner = minted_arn(label, Some(group), served[0]);
            for (status, body) in answered.iter().filter(|(status, _)| *status != 200) {
                assert_refused_for(label, group, &winner, *status, body);
            }
            assert!(
                winners.insert(winner.clone()),
                "case {label:?} (token {group}): each token mints an arn of its own, got \
                 {winner} twice"
            );
        }

        // A call carrying no token is keyed by nothing, so it is never refused for another
        // call's claim: every one of them creates a report of its own.
        let untokened = by_key.get(&None).map(Vec::as_slice).unwrap_or(&[]);
        assert_eq!(
            untokened.len(),
            case.untokened,
            "case {label:?}: every token-less call must be answered, got {untokened:?}"
        );
        for (status, body) in untokened {
            assert_eq!(
                *status, 200,
                "case {label:?}: a create carrying no clientToken is served, got {status} with {body}"
            );
            let arn = minted_arn(label, None, body);
            assert!(
                winners.insert(arn.clone()),
                "case {label:?}: every token-less create mints its own arn, got {arn} twice"
            );
        }

        // What the account is left holding: exactly the reports the served calls minted,
        // one per distinct token plus one per token-less call.
        let stored: BTreeSet<String> = stored_arns(&router).await.into_iter().collect();
        assert_eq!(
            stored, winners,
            "case {label:?}: the account must hold exactly the reports the served calls \
             created, got {stored:?}"
        );
        assert_eq!(
            stored.len(),
            case.crowds.len() + case.untokened,
            "case {label:?}: expected one report per distinct token plus one per token-less \
             call, got {stored:?}"
        );

        // One claim per token the fleet presented, and no claim at all for the token-less
        // calls: the claims are what the refusals above were worded from. A white-box
        // supplement to the assertions above, which state the rule over the wire on their
        // own: this one adds that the claims a fleet leaves behind are bounded by the
        // tokens it presented.
        let claimed: BTreeSet<String> = service_state(&*engine)
            .expect("the dashboard store")
            .lock()
            .scheduled_report_client_tokens
            .keys()
            .cloned()
            .collect();
        let expected: BTreeSet<String> = (0..case.crowds.len()).map(token_of).collect();
        assert_eq!(
            claimed, expected,
            "case {label:?}: expected a claim for each presented token and no other, got \
             {claimed:?}"
        );
    }
}

/// The partition holds over TIME as well as within one burst: a token whose create has
/// already settled is refused in the name of its own report while a fresh token racing
/// beside it still creates, so a settled claim never suppresses another token's create.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_settled_claim_refuses_only_its_own_token_while_a_fresh_one_creates() {
    let label = "a settled claim beside a fresh token";
    let (router, _engine) = emulator();
    let (dashboard_arn, widget_id) = source_dashboard(&router, "PartitionSettledSource").await;

    // Round one: two tokens, each crowded by two callers.
    let first = race_keyed(
        &router,
        vec![
            (Some(0), create_request(Some(0), &dashboard_arn, &widget_id)),
            (Some(1), create_request(Some(1), &dashboard_arn, &widget_id)),
            (Some(0), create_request(Some(0), &dashboard_arn, &widget_id)),
            (Some(1), create_request(Some(1), &dashboard_arn, &widget_id)),
        ],
    )
    .await;
    let mut settled: BTreeMap<usize, String> = BTreeMap::new();
    for (key, status, body) in &first {
        if *status == 200 {
            let group = key.expect("every call of round one presented a token");
            let arn = minted_arn(label, *key, body);
            assert!(
                settled.insert(group, arn).is_none(),
                "case {label:?}: one winner per token in round one, got {first:?}"
            );
        }
    }
    assert_eq!(
        settled.len(),
        2,
        "case {label:?}: both tokens must have created in round one, got {first:?}"
    );

    // Round two: the two settled tokens again, beside a third this service has not seen.
    let second = race_keyed(
        &router,
        vec![
            (Some(0), create_request(Some(0), &dashboard_arn, &widget_id)),
            (Some(2), create_request(Some(2), &dashboard_arn, &widget_id)),
            (Some(1), create_request(Some(1), &dashboard_arn, &widget_id)),
        ],
    )
    .await;
    let mut fresh: Option<String> = None;
    for (key, status, body) in &second {
        let group = key.expect("every call of round two presented a token");
        match settled.get(&group) {
            // A token whose create settled is refused in the name of ITS report, which by
            // now exists, so the terminal wording is the only one left for it.
            Some(winner) => {
                assert_refused_for(label, group, winner, *status, body);
                let expected = already_exists_message(winner);
                assert_eq!(
                    body["message"].as_str(),
                    Some(expected.as_str()),
                    "case {label:?} (token {group}): a settled claim refuses with {expected:?}, \
                     got {body}"
                );
            }
            // The token this service has not seen is keyed by nothing yet, so the claims
            // beside it leave it alone.
            None => {
                assert_eq!(
                    *status, 200,
                    "case {label:?} (token {group}): a token no create has claimed must be \
                     served, got {status} with {body}"
                );
                fresh = Some(minted_arn(label, *key, body));
            }
        }
    }

    let fresh = fresh.unwrap_or_else(|| panic!("case {label:?}: the fresh token must have created"));
    let mut expected: BTreeSet<String> = settled.values().cloned().collect();
    expected.insert(fresh);
    let stored: BTreeSet<String> = stored_arns(&router).await.into_iter().collect();
    assert_eq!(
        stored, expected,
        "case {label:?}: the account must hold one report per distinct token, got {stored:?}"
    );
}
