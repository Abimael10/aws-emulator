// SPDX-License-Identifier: CC0-1.0

//! `UpdateAct`: an act that has FINISHED is frozen against a further status write.
//!
//! RULE: a status write to an act whose status has already settled (one of the statuses
//! that finish an act) is refused as a STATE error — HTTP 400 `ValidationException` with
//! reason `InvalidStatus`
//! (`smithy_query(query="shape",shape="ValidationExceptionReason")`) carrying this
//! service's fixed sentence for a resource whose state the write did not expect — and
//! leaves the stored act exactly as it was, status, `endedAt` and error information
//! included, whichever status the request named and whatever it carried beside it. Only an
//! act still under way takes a status, and only re-sending the status a finished act
//! already holds is accepted, as the no-op retry this `@idempotent` operation promises.
//!
//! The tables below vary what the rule generalises over: the status the act settled in
//! (each of the three that finish one), the status then asked for (each of the other five
//! `ActStatus` declares, so both an admissible value and an inadmissible one), whether the
//! request carries an `error` member, whether the act already holds error information, and
//! how many times the refused write is repeated. The recorded case — an act that SUCCEEDED,
//! asked to move to FAILED with an `error` — is one row among them.

use serde_json::json;
use service_com_amazonaws_novaact_amazonnovaagentsdataplane::ops::types::ACT_STATUS_VALUES;

use crate::act_status_transition_refusal::expected_message;
use crate::act_status_update::{
    chain, move_to, new_act, stored_error, summary, update_act, Chain, FINISHED_STATUSES,
    INITIAL_ACT_STATUS, REPORTED_OUTCOMES,
};
use crate::workflow_lifecycle::{text, Emulator};

/// The definition each case builds its chain under.
const DEFINITION: &str = "act-terminal-freeze";

/// The sentence this service states when the act a status write addresses is in a state
/// that write did not expect. Fixed text: it interpolates no identifier, no current status
/// and no requested one, so every caller that finds a settled act reads the same refusal.
///
/// `pub(crate)` so the sibling suite that owns the OTHER refusal on this member distinguishes
/// the two sentences against this one reading of it.
pub(crate) const ACT_IN_UNEXPECTED_STATE: &str = "Act in unexpected state";

/// Everything a read can see of one act: what the listing reports, and the error
/// information only the store holds (`ActError` is reported by no modelled operation).
type Snapshot = (serde_json::Value, Option<(String, Option<String>)>);

/// One act's whole observable state, so a refusal can be checked to have changed none of it.
async fn snapshot(emulator: &Emulator, chain: &Chain, act_id: &str, label: &str) -> Snapshot {
    (
        summary(emulator, chain, act_id).await,
        stored_error(emulator, chain, act_id, label),
    )
}

/// One act driven to `settled`, carrying the error information `error` if any.
async fn settled_act(
    emulator: &Emulator,
    chain: &Chain,
    label: &str,
    settled: &str,
    error: Option<serde_json::Value>,
) -> String {
    let act_id = new_act(emulator, chain, label).await;
    let body = match error {
        Some(error) => json!({ "status": settled, "error": error }),
        None => json!({ "status": settled }),
    };
    let (status, answer) = update_act(emulator, chain, &act_id, body).await;
    assert_eq!(
        status, 200,
        "case {label:?}: the act must settle in {settled} first, got {status} {answer}"
    );
    act_id
}

/// The refusal a settled act answers a status write with: 400 / `ValidationException` /
/// reason `InvalidStatus` / this service's settled-resource sentence, and no `fieldList`
/// (that list is what the `FieldValidationFailed` reason carries to point at an offending
/// member, and the value here is admissible in itself).
fn assert_refusal(answer: &serde_json::Value, status: u16, label: &str) {
    assert_eq!(
        status, 400,
        "case {label:?}: expected HTTP 400, got {status} {answer}"
    );
    assert_eq!(
        answer["__type"], "ValidationException",
        "case {label:?}: expected ValidationException, got {answer}"
    );
    assert_eq!(
        answer["reason"], "InvalidStatus",
        "case {label:?}: expected reason \"InvalidStatus\", got {answer}"
    );
    let message = text(answer, "message");
    assert_eq!(
        message, ACT_IN_UNEXPECTED_STATE,
        "case {label:?}: expected message {ACT_IN_UNEXPECTED_STATE:?}, got {message:?}"
    );
    assert!(
        answer["fieldList"].is_null(),
        "case {label:?}: expected no fieldList, got {answer}"
    );
}

/// Every status that settles an act, asked to move to every admissible outcome that is not
/// the one it holds: refused with the settled-act sentence, and the act keeps its outcome.
#[tokio::test]
async fn a_settled_act_refuses_an_outcome_reported_after_it_finished() {
    let emulator = Emulator::new("us-east-1");
    let chain = chain(&emulator, DEFINITION).await;

    for settled in FINISHED_STATUSES {
        for requested in REPORTED_OUTCOMES
            .iter()
            .filter(|value| **value != settled)
        {
            let label = format!("{settled} then {requested}");
            let act_id = settled_act(&emulator, &chain, &label, settled, None).await;
            let before = snapshot(&emulator, &chain, &act_id, &label).await;

            // The recorded call shape: a status change carrying error information for it.
            let (status, answer) = update_act(
                &emulator,
                &chain,
                &act_id,
                json!({
                    "status": requested,
                    "error": { "message": "reported after the act had finished" },
                }),
            )
            .await;
            assert_refusal(&answer, status, &label);

            let after = snapshot(&emulator, &chain, &act_id, &label).await;
            assert_eq!(
                after, before,
                "case {label:?}: the refused update must change nothing, got {after:?}"
            );
            assert_eq!(
                after.0["status"], settled,
                "case {label:?}: the listing must still report {settled:?}, got {}",
                after.0
            );
            assert!(
                after.1.is_none(),
                "case {label:?}: no error information may be attached, got {:?}",
                after.1
            );
        }
    }
}

/// The refusal does not depend on what else the request carries, how often it is repeated,
/// or what the act already holds: a settled act is frozen against every status but its own.
#[tokio::test]
async fn a_settled_act_is_frozen_against_every_other_status() {
    let emulator = Emulator::new("eu-central-1");
    let chain = chain(&emulator, DEFINITION).await;

    // Rows differ in whether the settled act already holds error information and in whether
    // the refused write carries one, so neither presence decides the refusal.
    let rows: [(&str, Option<serde_json::Value>, bool); 3] = [
        ("no error either side", None, false),
        (
            "an error recorded with the outcome",
            Some(json!({ "message": "the page never loaded", "type": "ToolFailed" })),
            false,
        ),
        ("an error only on the refused write", None, true),
    ];

    for (row, held_error, send_error) in rows {
        for settled in FINISHED_STATUSES {
            for requested in ACT_STATUS_VALUES.iter().filter(|value| **value != settled) {
                let label = format!("{row}: {settled} then {requested}");
                let act_id =
                    settled_act(&emulator, &chain, &label, settled, held_error.clone()).await;
                let before = snapshot(&emulator, &chain, &act_id, &label).await;

                // Repeated, so a refusal cannot be a one-shot that lets the second attempt
                // through.
                for attempt in 1..=2 {
                    let body = if send_error {
                        json!({ "status": requested, "error": { "message": "late report" } })
                    } else {
                        json!({ "status": requested })
                    };
                    let (status, answer) = update_act(&emulator, &chain, &act_id, body).await;
                    let label = format!("{label} attempt {attempt}");
                    // A value this act's state does not admit at all is refused by the other
                    // branch on this member, with the sentence naming the admissible set; the
                    // requirement shared by both is the 400 and the absent effect.
                    if REPORTED_OUTCOMES.contains(requested) {
                        assert_refusal(&answer, status, &label);
                    } else {
                        assert_eq!(
                            status, 400,
                            "case {label:?}: expected HTTP 400, got {status} {answer}"
                        );
                        assert_eq!(
                            answer["__type"], "ValidationException",
                            "case {label:?}: expected ValidationException, got {answer}"
                        );
                        // The two branches stay distinguishable: an inadmissible value is
                        // refused with the sentence naming the set the act does admit, not
                        // with the settled-act one.
                        let message = text(&answer, "message");
                        let admissible_set = expected_message(settled);
                        assert_eq!(
                            message, admissible_set,
                            "case {label:?}: expected the admissible-set sentence \
                             {admissible_set:?}, got {message:?}"
                        );
                    }
                    let after = snapshot(&emulator, &chain, &act_id, &label).await;
                    assert_eq!(
                        after, before,
                        "case {label:?}: the refused update must change nothing, got {after:?}"
                    );
                }

                // Re-sending the status the act already holds is the retry this
                // `@idempotent` operation promises: accepted, and still nothing moves.
                let (status, answer) = move_to(&emulator, &chain, &act_id, settled).await;
                assert_eq!(
                    status, 200,
                    "case {label:?}: re-sending {settled:?} must be accepted, got {status} \
                     {answer}"
                );
                let after = snapshot(&emulator, &chain, &act_id, &label).await;
                assert_eq!(
                    after, before,
                    "case {label:?}: the retry must change nothing, got {after:?}"
                );
            }
        }
    }
}

/// The positive control of the rule: only a SETTLED act is frozen. An act still under way
/// takes the same call, whichever unfinished status it holds and whichever outcome is
/// reported for it, and the listing then reports that outcome.
#[tokio::test]
async fn an_act_still_under_way_takes_the_same_write() {
    let emulator = Emulator::new("ap-southeast-2");
    let chain = chain(&emulator, DEFINITION).await;

    // Every status that leaves an act under way (the three `ActStatus` declares beside the
    // finishing ones), including the status `CreateAct` stores a new act in, each asked for
    // every status that finishes an act.
    let under_way: Vec<&str> = ACT_STATUS_VALUES
        .iter()
        .copied()
        .filter(|value| !FINISHED_STATUSES.contains(value))
        .collect();
    assert!(
        under_way.contains(&INITIAL_ACT_STATUS),
        "the status a new act is created in must be one that leaves it under way, got \
         {under_way:?}"
    );

    for current in &under_way {
        for requested in FINISHED_STATUSES {
            let label = format!("{current} then {requested}");
            let act_id = new_act(&emulator, &chain, &label).await;
            let (status, answer) = move_to(&emulator, &chain, &act_id, current).await;
            assert_eq!(
                status, 200,
                "case {label:?}: the act must hold {current:?} first, got {status} {answer}"
            );

            let (status, answer) = update_act(
                &emulator,
                &chain,
                &act_id,
                json!({
                    "status": requested,
                    "error": { "message": "reported by the worker that ran it" },
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {label:?}: an act still under way takes the write, got {status} {answer}"
            );
            let (listed, recorded) = snapshot(&emulator, &chain, &act_id, &label).await;
            assert_eq!(
                listed["status"], requested,
                "case {label:?}: expected the listing to report {requested:?}, got {listed}"
            );
            assert!(
                !listed["endedAt"].is_null(),
                "case {label:?}: a finished act reports endedAt, got {listed}"
            );
            assert_eq!(
                recorded,
                Some(("reported by the worker that ran it".to_string(), None)),
                "case {label:?}: expected the submitted error to be recorded, got {recorded:?}"
            );
        }
    }
}

/// The sentence is fixed text: it names neither the status the request asked for, nor the
/// one the act holds, nor any identifier of the chain the request addressed — so two
/// callers that find two different settled acts read the very same refusal.
#[tokio::test]
async fn the_sentence_carries_nothing_of_the_request() {
    let emulator = Emulator::new("ap-northeast-1");
    let chain = chain(&emulator, DEFINITION).await;

    // Two rows whose settled status, requested status and act differ, so a sentence derived
    // from any of them would differ between the rows.
    let rows: [(&str, &str); 2] = [("SUCCEEDED", "FAILED"), ("TIMED_OUT", "SUCCEEDED")];
    let mut sentences: Vec<String> = Vec::new();
    for (settled, requested) in rows {
        let label = format!("{settled} then {requested}");
        let act_id = settled_act(&emulator, &chain, &label, settled, None).await;
        let (status, answer) = move_to(&emulator, &chain, &act_id, requested).await;
        assert_refusal(&answer, status, &label);
        let message = text(&answer, "message");
        for named in [
            settled,
            requested,
            DEFINITION,
            chain.run_id.as_str(),
            chain.session_id.as_str(),
            act_id.as_str(),
        ] {
            assert!(
                !message.contains(named),
                "case {label:?}: {named:?} must not appear in the refusal, got {message:?}"
            );
        }
        sentences.push(message);
    }
    assert_eq!(
        sentences[0], sentences[1],
        "both rows must read the same refusal, got {sentences:?}"
    );
}
