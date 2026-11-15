// SPDX-License-Identifier: CC0-1.0

//! Resuming a campaign's run, end to end over HTTP.
//!
//! The rule under test: a campaign's run state is a state machine, and a lifecycle
//! operation applies its transition only from the states that transition admits — a
//! campaign already in the target state is an idempotent success that writes nothing,
//! and a campaign in any other state is refused with `ConflictException` (409) naming
//! the campaign and the state it is in, leaving that state alone. `ResumeCampaign` is
//! the transition into `Running` from `Paused`, so it is asserted over EVERY state a
//! campaign of this service can hold rather than over the paused one alone, and the
//! state it leaves behind is read back through `GetCampaignState` and
//! `GetCampaignStateBatch`.
//!
//! The state catalog these cases span is the model's own `CampaignState` enum
//! (`smithy_query(query="shape", shape="CampaignState")`, exposed to this crate as
//! `CAMPAIGN_STATE_VALUES`); `Failed` is the one value no operation of this service
//! enters, so no case can put a campaign there.

use serde_json::{json, Value};

use super::common::{call, router, Answer, Harness};

/// The region every case here signs for.
const REGION: &str = "us-west-2";

/// The Amazon Connect instance the campaigns below are created for.
const INSTANCE: &str = "resume-instance";

/// A minimal `CreateCampaign` body: every member the model marks required, and nothing
/// else.
fn create_body(name: &str) -> Value {
    json!({
        "name": name,
        "connectInstanceId": INSTANCE,
        "dialerConfig": {"progressiveDialerConfig": {"bandwidthAllocation": 0.5}},
        "outboundCallConfig": {"connectContactFlowId": "flow-1"},
    })
}

/// Create one campaign and hand back the id it was minted under.
async fn created_campaign(harness: &Harness, name: &str) -> String {
    harness.register_connect_instance(REGION, INSTANCE);
    let created = call(harness, REGION, "PUT", "/campaigns", &create_body(name)).await;
    assert_eq!(
        created.status, 200,
        "case {name:?}: expected CreateCampaign to answer 200, got {} {}",
        created.status, created.body
    );
    created.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: no id in {}", created.body))
        .to_string()
}

/// One lifecycle call (`start`, `pause`, `resume`, `stop`) on a campaign.
async fn lifecycle(harness: &Harness, id: &str, action: &str) -> Answer {
    call(
        harness,
        REGION,
        "POST",
        &format!("/campaigns/{id}/{action}"),
        &Value::Null,
    )
    .await
}

/// The run state `GetCampaignState` reports for a campaign.
async fn reported_state(harness: &Harness, id: &str) -> String {
    let answer = call(
        harness,
        REGION,
        "GET",
        &format!("/campaigns/{id}/state"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "reading the state of {id:?}: expected 200, got {} {}",
        answer.status, answer.body
    );
    answer.body["state"]
        .as_str()
        .unwrap_or_else(|| panic!("reading the state of {id:?}: no state in {}", answer.body))
        .to_string()
}

/// The run state `GetCampaignStateBatch` reports for a campaign — the same state read
/// through the batch reader, so the two cannot disagree about what a resume left behind.
async fn batch_reported_state(harness: &Harness, id: &str) -> String {
    let answer = call(
        harness,
        REGION,
        "POST",
        "/campaigns-state",
        &json!({"campaignIds": [id]}),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "batch-reading the state of {id:?}: expected 200, got {} {}",
        answer.status, answer.body
    );
    answer
        .body
        .pointer("/successfulRequests/0/state")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "batch-reading the state of {id:?}: no state in {}",
                answer.body
            )
        })
        .to_string()
}

/// The lifecycle calls that drive a fresh campaign into `target`, asserting as it goes
/// that each step the state machine admits really is a success, and hand back its id.
async fn campaign_in_state(harness: &Harness, target: &str) -> String {
    let id = created_campaign(harness, &format!("in-{target}")).await;
    let path: &[&str] = match target {
        "Initialized" => &[],
        "Running" => &["start"],
        "Paused" => &["start", "pause"],
        "Stopped" => &["start", "stop"],
        other => panic!("no lifecycle path to {other}"),
    };
    for action in path {
        let answer = lifecycle(harness, &id, action).await;
        assert_eq!(
            answer.status, 200,
            "driving to {target}: expected {action} to answer 200, got {} {}",
            answer.status, answer.body
        );
    }
    let reached = reported_state(harness, &id).await;
    assert_eq!(
        reached, target,
        "driving to {target}: the campaign ended in {reached:?}"
    );
    id
}

#[tokio::test]
async fn resume_moves_only_a_paused_campaign_and_refuses_every_other_state() {
    // One row per state a campaign of this service can hold, with the state the campaign
    // must be in after a resume: `Some(state)` for a success, `None` for a refusal that
    // leaves the campaign where it was. A paused campaign has a suspended run to resume;
    // a running one already holds the state the call asks for, so it is the idempotent
    // success; a campaign that has never run is started rather than resumed, and a
    // stopped run cannot be resumed at all.
    let rows: &[(&str, Option<&str>)] = &[
        ("Initialized", None),
        ("Running", Some("Running")),
        ("Paused", Some("Running")),
        ("Stopped", None),
    ];

    let harness = router();

    for (from, expected) in rows {
        let case = format!("resume a {from} campaign");
        let id = campaign_in_state(&harness, from).await;
        let answer = lifecycle(&harness, &id, "resume").await;

        match expected {
            Some(to) => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: expected 200, got {} {}",
                    answer.status, answer.body
                );
                let reported = reported_state(&harness, &id).await;
                assert_eq!(
                    reported, *to,
                    "case {case:?}: expected the campaign to be {to}, got {reported:?}"
                );
                let batched = batch_reported_state(&harness, &id).await;
                assert_eq!(
                    batched, *to,
                    "case {case:?}: expected the batch reader to agree on {to}, got \
                     {batched:?}"
                );
            }
            None => {
                assert_eq!(
                    answer.status, 409,
                    "case {case:?}: expected 409, got {} {}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.error_code, "ConflictException",
                    "case {case:?}: expected ConflictException, got {:?}",
                    answer.error_code
                );
                let expected_message =
                    format!("Campaign: {id} is in invalid state: {}", from.to_uppercase());
                assert_eq!(
                    answer.message(),
                    expected_message,
                    "case {case:?}: expected the refusal to name the campaign and the \
                     upper-case state it is in, got {:?}",
                    answer.message()
                );
                let reported = reported_state(&harness, &id).await;
                assert_eq!(
                    reported, *from,
                    "case {case:?}: a refused resume must leave the campaign in {from}, \
                     got {reported:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn pausing_and_resuming_round_trips_as_often_as_a_caller_asks() {
    // The transition is a state machine step, not a one-shot flag: a campaign may be
    // suspended and resumed over and over, and a repeated call of either asks for the
    // state the campaign already holds and so changes nothing. The rows differ along how
    // many times the pair is driven and how many times each call is repeated.
    let rows: &[(&str, usize, usize)] = &[
        ("one pause/resume pair, each call once", 1, 1),
        ("three pause/resume pairs, each call once", 3, 1),
        ("two pause/resume pairs, each call twice over", 2, 2),
        ("one pause/resume pair, each call four times over", 1, 4),
    ];

    let harness = router();

    for (case, pairs, repeats) in rows {
        let id = campaign_in_state(&harness, "Running").await;
        for round in 0..*pairs {
            for (action, expected) in [("pause", "Paused"), ("resume", "Running")] {
                for repeat in 0..*repeats {
                    let answer = lifecycle(&harness, &id, action).await;
                    assert_eq!(
                        answer.status, 200,
                        "case {case:?} round {round} {action} #{repeat}: expected 200, \
                         got {} {}",
                        answer.status, answer.body
                    );
                }
                let reported = reported_state(&harness, &id).await;
                assert_eq!(
                    reported, expected,
                    "case {case:?} round {round}: expected {action} to leave the campaign \
                     {expected}, got {reported:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn resuming_a_campaign_the_store_does_not_hold_is_a_404() {
    // Ids that are well-formed `CampaignId`s (len 0..256, pattern `^[\S]*$`, from
    // `smithy_query(query="shape", shape="CampaignId")`) but name no stored campaign:
    // the uuid form a minted id takes, the longest id the bound admits, and the id of a
    // campaign that was created and then deleted.
    let harness = router();
    let deleted = created_campaign(&harness, "deleted").await;
    let dropped = call(
        &harness,
        REGION,
        "DELETE",
        &format!("/campaigns/{deleted}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        dropped.status, 200,
        "expected DeleteCampaign to answer 200, got {} {}",
        dropped.status, dropped.body
    );

    let rows: Vec<(&str, String)> = vec![
        (
            "the uuid form a minted campaign id takes",
            "11111111-2222-4333-8444-555555555555".to_string(),
        ),
        ("256 characters (the length maximum)", "c".repeat(256)),
        ("a campaign that was created and then deleted", deleted),
    ];

    for (case, id) in rows {
        let answer = lifecycle(&harness, &id, "resume").await;
        assert_eq!(
            answer.status, 404,
            "case {case:?}: expected 404, got {} {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code, "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {:?}",
            answer.error_code
        );
        assert_eq!(
            answer.message(),
            format!("Campaign: {id} doesn't exist"),
            "case {case:?}: expected the run-state wording for a campaign that does not \
             exist, got {:?}",
            answer.message()
        );
    }
}

/// Submit one dial request for campaign `id` under `token`. Only a RUNNING campaign
/// takes dial requests, so this is what shows a resumed campaign really is running again
/// rather than merely reported so.
async fn dial(harness: &Harness, id: &str, token: &str) -> Answer {
    call(
        harness,
        REGION,
        "PUT",
        &format!("/campaigns/{id}/dial-requests"),
        &json!({
            "dialRequests": [{
                "clientToken": token,
                "phoneNumber": "+15550100",
                // `DialRequest$expirationTime` carries @timestampFormat("date-time"),
                // so an RFC3339 instant far in the future.
                "expirationTime": "2099-01-01T00:00:00Z",
                "attributes": {"campaign": "nightly"},
            }],
        }),
    )
    .await
}

#[tokio::test]
async fn a_resumed_campaign_takes_dial_requests_again() {
    // The resume writes real state rather than answering an empty success: the dial
    // plane admits only a running campaign, so each step of this walk is read through it
    // — accepted while running, refused while paused, accepted again once resumed.
    let harness = router();
    let id = campaign_in_state(&harness, "Running").await;

    let rows: &[(&str, Option<&str>, &str, u16)] = &[
        ("dial a running campaign", None, "token-running", 200),
        ("dial a paused campaign", Some("pause"), "token-paused", 409),
        ("dial a resumed campaign", Some("resume"), "token-resumed", 200),
    ];

    for (case, action, token, expected) in rows {
        if let Some(action) = action {
            let moved = lifecycle(&harness, &id, action).await;
            assert_eq!(
                moved.status, 200,
                "case {case:?}: expected {action} to answer 200, got {} {}",
                moved.status, moved.body
            );
        }
        let answer = dial(&harness, &id, token).await;
        assert_eq!(
            answer.status, *expected,
            "case {case:?}: expected HTTP {expected}, got {} {}",
            answer.status, answer.body
        );
    }
}
