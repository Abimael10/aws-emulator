// SPDX-License-Identifier: CC0-1.0

//! WHAT A RESTART REACHES: the broker the request names, and nothing else — driven end
//! to end through the router, so routing, restJson1 (de)serialization and the handlers
//! are exercised together.
//!
//! The rules these tables pin, each Amazon MQ's and not one broker's:
//!
//! 1. `RebootBroker` names ONE broker (`RebootBrokerRequest$BrokerId` —
//!    `smithy_query(query="operation", operation="RebootBroker")`), so the restart it
//!    starts is confined to that broker: every other broker of the account keeps
//!    reporting the state it was in, on every read path that reports a broker state
//!    (`DescribeBroker` and `ListBrokers`), and the two read paths agree about the
//!    restarted broker as well.
//! 2. A restart returns the broker to the state it was in: once the clock has made the
//!    return to RUNNING due, every member of the broker's own read answers what it
//!    answered before the restart, and reading it again answers that same document —
//!    materialising what the clock owed is not something a read re-runs.
//!
//! The tables span what those rules generalise over: which broker of a fleet is
//! restarted (its first, a middle one and its last), and the engine types the model
//! enumerates (`ENGINE_TYPE_VALUES`).
//!
//! Every wait here is a poll for the CONDITION with a generous deadline, never a sleep
//! for a span: the return a restart owes is due at a stored instant, so a loaded host
//! just looks again. A case that needs to observe the transient state is retried on a
//! fresh restart when a loaded host let the span come due first, rather than asserting
//! on how fast the host ran.
//!
//! The wire harness is shared with [`crate::broker_lifecycle`] and the restart helpers
//! with [`crate::broker_restart`], so every call goes through the same
//! `MultiServiceRouter::dispatch` the front door uses.

use serde_json::{json, Value};

use service_com_amazonaws_mq_mq::ops::types::ENGINE_TYPE_VALUES;
use service_com_amazonaws_mq_mq::state::{BROKER_STATE_REBOOT_IN_PROGRESS, BROKER_STATE_RUNNING};

use crate::broker_lifecycle::{create_body, string_at, Mq};
use crate::broker_restart::{poll_until_state, reboot};

/// How many fresh restarts a case gets to observe the transient state before it gives
/// up. Each attempt is a whole restart, so a host that never let one be observed in this
/// many is a host on which the broker never reported the state a restart puts it in.
const ATTEMPTS: usize = 8;

/// A fleet of brokers, each polled until it is RUNNING, named `prefix-<index>`.
async fn running_fleet(mq: &Mq, prefix: &str, count: usize) -> Vec<String> {
    let mut ids = Vec::new();
    for index in 0..count {
        let name = format!("{prefix}-{index}");
        let (status, created) = mq.create_broker(create_body(&name)).await;
        assert_eq!(
            status, 200,
            "create-broker {name:?}: expected 200, got {status} {created:?}"
        );
        ids.push(string_at(&created, "/brokerId"));
    }
    for id in &ids {
        poll_until_state(mq, id, BROKER_STATE_RUNNING).await;
    }
    ids
}

/// The `brokerState` the LIST read reports for `broker_id` — the other read path a
/// broker's state reaches a caller through.
async fn listed_state(mq: &Mq, broker_id: &str) -> String {
    let (status, listed) = mq.list_brokers("").await;
    assert_eq!(
        status, 200,
        "list-brokers: expected 200, got {status} {listed:?}"
    );
    let summaries = listed
        .pointer("/brokerSummaries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let summary = summaries
        .iter()
        .find(|summary| string_at(summary, "/brokerId") == broker_id)
        .unwrap_or_else(|| {
            panic!("list-brokers must report broker {broker_id:?}, got {listed:?}")
        });
    string_at(summary, "/brokerState")
}

/// RULE 1: a restart reaches only the broker the request names. Whichever broker of a
/// fleet is restarted, every other one keeps reporting RUNNING on both read paths, and
/// the restarted one reports the state a restart puts it in.
#[tokio::test]
async fn a_restart_is_confined_to_the_broker_it_names() {
    let mq = Mq::new("us-east-2");

    // Rows: the fleet's first broker, a middle one and its last — a restart of any of
    // them is one broker's business.
    let fleet = running_fleet(&mq, "fleet", 3).await;

    for target in 0..fleet.len() {
        let mut saw_transient = false;
        for attempt in 0..ATTEMPTS {
            let case = format!(
                "fleet of {}, restarting broker {target}, attempt {attempt}",
                fleet.len()
            );
            let (status, code, answer) = reboot(&mq, &fleet[target]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200, got {status} {code:?} {answer:?}"
            );

            let (status, described) = mq.describe_broker(&fleet[target]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200 describing the restarted broker, got {status} \
                 {described:?}"
            );
            let restarted_state = string_at(&described, "/brokerState");
            assert!(
                [BROKER_STATE_REBOOT_IN_PROGRESS, BROKER_STATE_RUNNING]
                    .contains(&restarted_state.as_str()),
                "case {case:?}: a restarted broker is either restarting or back running, got \
                 {restarted_state:?}"
            );
            saw_transient |= restarted_state == BROKER_STATE_REBOOT_IN_PROGRESS;

            // Every other broker of the fleet is untouched, on either read path: a
            // restart of one broker is not a restart of another.
            for (index, id) in fleet.iter().enumerate() {
                if index == target {
                    continue;
                }
                let (status, sibling) = mq.describe_broker(id).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected 200 describing broker {index}, got {status} \
                     {sibling:?}"
                );
                assert_eq!(
                    string_at(&sibling, "/brokerState"),
                    BROKER_STATE_RUNNING,
                    "case {case:?}: broker {index} must keep reporting \
                     {BROKER_STATE_RUNNING}, got {sibling:?}"
                );
                let listed = listed_state(&mq, id).await;
                assert_eq!(
                    listed, BROKER_STATE_RUNNING,
                    "case {case:?}: the list read must report broker {index} as \
                     {BROKER_STATE_RUNNING} too, got {listed:?}"
                );
            }

            // And the list read agrees with the describe read about the restarted broker
            // itself, whichever of the two states the clock has it in by then.
            let listed = listed_state(&mq, &fleet[target]).await;
            assert!(
                [BROKER_STATE_REBOOT_IN_PROGRESS, BROKER_STATE_RUNNING]
                    .contains(&listed.as_str()),
                "case {case:?}: the list read must report a restarting or running broker, got \
                 {listed:?}"
            );

            poll_until_state(&mq, &fleet[target], BROKER_STATE_RUNNING).await;
            if saw_transient {
                break;
            }
        }
        assert!(
            saw_transient,
            "restarting broker {target} must report {BROKER_STATE_REBOOT_IN_PROGRESS} while \
             the restart is in flight"
        );
    }
}

/// RULE 2: a restart returns the broker to the state it was in — the broker's whole read
/// answers again what it answered before the restart, whatever engine it runs, and
/// reading it once more answers that same document.
#[tokio::test]
async fn a_restart_returns_the_broker_to_the_state_it_was_in() {
    let mq = Mq::new("eu-central-1");

    // Rows: each engine the model enumerates (`ENGINE_TYPE_VALUES`), because what a
    // restart leaves alone is every member of the read, and which members a broker
    // answers depends on its engine.
    for (index, engine_type) in ENGINE_TYPE_VALUES.iter().enumerate() {
        let case = format!("{engine_type} broker");
        let mut body = create_body(&format!("unchanged-{index}"));
        body["engineType"] = json!(engine_type);
        if *engine_type == "RABBITMQ" {
            body["deploymentMode"] = json!("CLUSTER_MULTI_AZ");
        }
        let (status, created) = mq.create_broker(body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from create, got {status} {created:?}"
        );
        let broker_id = string_at(&created, "/brokerId");
        let before = poll_until_state(&mq, &broker_id, BROKER_STATE_RUNNING).await;

        let (status, code, answer) = reboot(&mq, &broker_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from reboot, got {status} {code:?} {answer:?}"
        );

        let after = poll_until_state(&mq, &broker_id, BROKER_STATE_RUNNING).await;
        assert_eq!(
            after, before,
            "case {case:?}: a completed restart must leave every member of the broker's read \
             as it was, got {after:?} where it was {before:?}"
        );

        // Reading it again answers the same document: what the clock owed the broker is
        // materialised once, and no read re-runs or undoes it.
        let (status, again) = mq.describe_broker(&broker_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 describing the restarted broker, got {status} \
             {again:?}"
        );
        assert_eq!(
            again, before,
            "case {case:?}: reading a restarted broker again must answer the same document, \
             got {again:?} where it was {before:?}"
        );
    }
}
