// SPDX-License-Identifier: CC0-1.0

//! `UpdateAct`: the SENTENCE the refusal of a status the act's current status does not
//! admit as a VALUE carries.
//!
//! The rule: a status write naming a value the resource's current status does not admit is
//! HTTP 400 `ValidationException` with reason `InvalidStatus` (the value
//! `smithy_query(query="shape",shape="ValidationExceptionReason")` declares for it) whose
//! message states the SET of statuses that state DOES admit — `Status must be one of [`,
//! the values separated by `", "`, then `]` — so a caller can read the admissible values
//! out of the refusal and pick a legal one. The set is computed from the act's current
//! status, not written per operation, and the sentence echoes nothing of the request.
//!
//! A value the act's state DOES admit is then weighed against that state, which is the
//! separate refusal the sibling suite `act_terminal_status_freeze.rs` asserts; the rows
//! below name which sentence each case expects, so the two branches cannot be confused.
//!
//! So the table below varies what the rule generalises over: which status the act finished
//! in (each of the three that finish one), which status is then asked for (each of the six
//! `ActStatus` declares), and therefore how many values the set holds. The recorded case —
//! an act that finished, asked for a status that would put it back under way — is one row
//! among them.

use service_com_amazonaws_novaact_amazonnovaagentsdataplane::ops::types::ACT_STATUS_VALUES;

use crate::act_status_update::{
    chain, move_to, new_act, summary, Chain, FINISHED_STATUSES, REPORTED_OUTCOMES,
};
use crate::act_terminal_status_freeze::ACT_IN_UNEXPECTED_STATE;
use crate::workflow_lifecycle::{text, Emulator};

/// The definition each case builds its chain under.
const DEFINITION: &str = "act-status-refusal";

/// The set of VALUES an act whose current status is `current` admits, spelled out here
/// rather than taken from the crate so the handler and the expectation cannot be reworded
/// together: an act still under way admits every status the model declares, and a finished
/// one admits the outcomes a caller reports for a finished act plus the status it already
/// holds (which an `@idempotent` retry re-sends, and which moves nothing).
pub(crate) fn admitted(current: &str) -> Vec<&'static str> {
    let mut admitted: Vec<&'static str> = ACT_STATUS_VALUES
        .iter()
        .copied()
        .filter(|status| {
            *status == current
                || !FINISHED_STATUSES.contains(&current)
                || REPORTED_OUTCOMES.contains(status)
        })
        .collect();
    // The service states the set in ascending order of the wire spelling.
    admitted.sort_unstable();
    admitted
}

/// The sentence this service states for a status the act's current status does not admit.
///
/// `pub(crate)` so the sibling suite that owns the OTHER refusal on this member tells the
/// two sentences apart against this one reading of it.
pub(crate) fn expected_message(current: &str) -> String {
    format!("Status must be one of [{}]", admitted(current).join(", "))
}

/// One act driven to `finished`, then asked for `requested`.
async fn refusal(
    emulator: &Emulator,
    chain: &Chain,
    finished: &str,
    requested: &str,
) -> (u16, serde_json::Value) {
    let label = format!("{finished} then {requested}");
    let act_id = new_act(emulator, chain, &label).await;
    let (status, answer) = move_to(emulator, chain, &act_id, finished).await;
    assert_eq!(
        status, 200,
        "case {label:?}: the act must finish first, got {status} {answer}"
    );
    let answered = move_to(emulator, chain, &act_id, requested).await;
    // Nothing the refusal says may have been written: the act is still the one it finished
    // as, whichever way the request was answered.
    let stored = summary(emulator, chain, &act_id).await;
    assert!(
        !stored["endedAt"].is_null(),
        "case {label:?}: the act must still be finished, got {stored}"
    );
    answered
}

/// Every finished status, asked for every status the model declares: a status the act's
/// current one does not admit is refused with AWS's sentence over the set it DOES admit.
#[tokio::test]
async fn the_refusal_names_the_statuses_the_act_admits() {
    let emulator = Emulator::new("us-east-1");
    let chain = chain(&emulator, DEFINITION).await;

    for finished in FINISHED_STATUSES {
        for requested in ACT_STATUS_VALUES {
            let label = format!("{finished} then {requested}");
            let (status, answer) = refusal(&emulator, &chain, finished, requested).await;
            if *requested == finished {
                assert_eq!(
                    status, 200,
                    "case {label:?}: re-sending the status the act holds must be accepted, \
                     got {status} {answer}"
                );
                continue;
            }
            assert_eq!(
                status, 400,
                "case {label:?}: expected HTTP 400, got {status} {answer}"
            );
            if admitted(finished).contains(requested) {
                // The VALUE is admissible for an act in this state, so what refuses the call
                // is the act's own settled state: the sibling suite owns that sentence, and
                // the only requirement here is that the admissible-set sentence is NOT the
                // one answered.
                let message = text(&answer, "message");
                assert_eq!(
                    message, ACT_IN_UNEXPECTED_STATE,
                    "case {label:?}: an admissible value refused by the act's state carries \
                     the settled-act sentence, got {message:?}"
                );
                continue;
            }
            assert_eq!(
                answer["__type"], "ValidationException",
                "case {label:?}: expected ValidationException, got {answer}"
            );
            assert_eq!(
                answer["reason"], "InvalidStatus",
                "case {label:?}: expected reason \"InvalidStatus\", got {answer}"
            );
            let expected = expected_message(finished);
            let message = text(&answer, "message");
            assert_eq!(
                message, expected,
                "case {label:?}: expected message {expected:?}, got {message:?}"
            );
            // `fieldList` is what the `FieldValidationFailed` reason carries to point at an
            // offending member; the value here is admissible in itself, so the refusal
            // carries none.
            assert!(
                answer["fieldList"].is_null(),
                "case {label:?}: expected no fieldList, got {answer}"
            );
        }
    }
}

/// The sentence carries the admissible set and NOTHING of the request: neither the status
/// it named nor any identifier of the chain it addressed, so two callers that find an act
/// in the same state read the same refusal.
#[tokio::test]
async fn the_refusal_echoes_nothing_the_caller_sent() {
    let emulator = Emulator::new("ap-northeast-1");
    let chain = chain(&emulator, DEFINITION).await;

    // The two rows differ in the status asked for and in the status the act finished in, so
    // the invariant is checked on refusals whose admissible sets differ in size.
    let rows: [(&str, &str); 2] = [("SUCCEEDED", "RUNNING"), ("TIMED_OUT", "PENDING_HUMAN_ACTION")];
    for (finished, requested) in rows {
        let label = format!("{finished} then {requested}");
        let (status, answer) = refusal(&emulator, &chain, finished, requested).await;
        assert_eq!(
            status, 400,
            "case {label:?}: expected HTTP 400, got {status} {answer}"
        );
        let message = text(&answer, "message");
        assert!(
            !message.contains(requested),
            "case {label:?}: the rejected status must not be echoed, got {message:?}"
        );
        for named in [DEFINITION, chain.run_id.as_str(), chain.session_id.as_str()] {
            assert!(
                !message.contains(named),
                "case {label:?}: {named:?} must not appear in the refusal, got {message:?}"
            );
        }
        assert_eq!(
            message,
            expected_message(finished),
            "case {label:?}: expected the admissible set, got {message:?}"
        );
    }
}
