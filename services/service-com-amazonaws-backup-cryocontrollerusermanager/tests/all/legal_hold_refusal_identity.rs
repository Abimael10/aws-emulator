// SPDX-License-Identifier: CC0-1.0

//! The members a legal-hold refusal does NOT state, and the one body every
//! hold-scoped call states for one condition — driven as real HTTP through the
//! router.
//!
//! The rules under test:
//!
//! * a legal-hold id that resolves to no stored hold is ONE condition, and this
//!   service's refusal for it states `Code` and `Message` and NOTHING ELSE: the
//!   shape's other optional members, `Context` and `Type`
//!   (`smithy_query(query="shape", shape="InvalidParameterValueException")`:
//!   optional `Code`/`Message`/`Type`/`Context`), are absent, since the
//!   condition rejects no MEMBER of the request — a populated `Context` reads as
//!   "this named member carried a value the service cannot accept", which is a
//!   different condition;
//! * the condition is identity, not wording per operation: the status, the error
//!   shape named to the client and the whole body one hold-scoped call answers
//!   for an unheld id are what every other one answers for it.
//!
//! The rows differ in the FORM of the id that resolves to nothing (its alphabet,
//! its length, its punctuation and whether it is shaped like an id this service
//! mints) and in which operation reached the condition, which is what "one
//! condition, one identity" generalises over: the refusal quotes no part of the
//! request, so no row's answer may depend on its id.
//!
//! What the siblings own, and this suite does not repeat: the CLASS, the token
//! and the sentence of this refusal are read by `legal_hold_vault_selection.rs`
//! (whose constants for them this suite imports rather than spelling a second
//! copy), and the blocked-by-state refusal a cancellation of a HELD hold answers
//! — its own token, the stored status in `Context` and interpolated into its
//! sentence — by `legal_hold_cancel_state_refusal.rs`, across the statuses that
//! block a release.
//!
//! The request harness (`router`, `call`, `call_reported`, `error_code`,
//! `string_at`) is the one `vault_and_backup_job.rs` exposes to this test
//! binary, so there is ONE wire path for the service's suites.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::legal_hold_vault_selection::{
    INVALID_PARAMETER, LEGAL_HOLD_NOT_FOUND_CODE, LEGAL_HOLD_NOT_FOUND_MESSAGE,
};
use super::vault_and_backup_job::{call, call_reported, error_code, router, string_at};

/// The members the refusal states, and the members it does not — the four the
/// shape declares, split by what this condition is about.
const STATED: [&str; 2] = ["Code", "Message"];
const UNSTATED: [&str; 2] = ["Context", "Type"];

/// Ids no hold of the account answers to, differing in form: a word, the shape
/// this service mints, an upper-case spelling of it, the shortest id a path
/// label can carry, a long one, and one carrying every punctuation character an
/// id can hold without leaving the URI path segment it binds to.
fn unheld_ids() -> Vec<(&'static str, String)> {
    vec![
        ("a word that is no id at all", "never-minted".to_string()),
        (
            "the shape this service mints",
            "00000000-0000-0000-0000-000000000000".to_string(),
        ),
        (
            "the same shape spelled upper case",
            "DEADBEEF-DEAD-BEEF-DEAD-BEEFDEADBEEF".to_string(),
        ),
        ("one character", "z".to_string()),
        ("a long id", "9".repeat(256)),
        ("punctuation an id can carry", "a.b_c-d~e".to_string()),
    ]
}

/// Each call that addresses a legal hold BY ID, by the HTTP binding the model
/// gives it (`smithy_query`: `GET /legal-holds/{LegalHoldId}`,
/// `GET /legal-holds/{LegalHoldId}/recovery-points`,
/// `DELETE /legal-holds/{LegalHoldId}`).
fn hold_scoped_calls(id: &str) -> Vec<(&'static str, &'static str, String)> {
    vec![
        ("GetLegalHold", "GET", format!("/legal-holds/{id}")),
        (
            "ListRecoveryPointsByLegalHold",
            "GET",
            format!("/legal-holds/{id}/recovery-points"),
        ),
        (
            "CancelLegalHold",
            "DELETE",
            format!("/legal-holds/{id}?cancelDescription=nothing-to-cancel"),
        ),
    ]
}

/// `POST /legal-holds` — `CreateLegalHold`'s HTTP binding. The control every
/// case needs: with one hold stored, each refusal below is about the id it was
/// given rather than about an empty store.
async fn one_held_hold(router: &MultiServiceRouter) -> String {
    let request = json!({
        "Title": "Held",
        "Description": "the one hold this account holds",
        "RecoveryPointSelection": {"VaultNames": ["refusal-identity-vault"]},
    });
    let (status, created) = call(router, "POST", "/legal-holds", Some(&request.to_string())).await;
    assert_eq!(
        status, 200,
        "setup: expected CreateLegalHold 200, got {status} {created:?}"
    );
    string_at(&created, "LegalHoldId")
}

/// The refusal for an unheld id states `Code` and `Message` and neither
/// `Context` nor `Type`, whichever hold-scoped operation was reached and
/// whatever the id looked like.
#[tokio::test]
async fn the_unheld_id_refusal_states_no_context_and_no_type() {
    let router = router();
    let held = one_held_hold(&router).await;

    for (form, id) in unheld_ids() {
        assert_ne!(
            id, held,
            "case {form:?}: the row's id must be one the account does not hold"
        );
        for (operation, method, path) in hold_scoped_calls(&id) {
            let case = format!("{operation} for {form} ({id:?})");
            let (status, errortype, refused) = call_reported(&router, method, &path, None).await;
            assert_eq!(
                (status, errortype.as_str(), error_code(&refused).as_str()),
                (400, INVALID_PARAMETER, INVALID_PARAMETER),
                "case {case:?}: expected 400 {INVALID_PARAMETER}, got {status} {refused:?}"
            );
            for member in UNSTATED {
                assert!(
                    refused.get(member).is_none(),
                    "case {case:?}: this condition rejects no member of the request, so it \
                     states no {member}, got {refused:?}"
                );
            }
            for member in STATED {
                assert!(
                    refused.get(member).is_some(),
                    "case {case:?}: expected the condition's {member}, got {refused:?}"
                );
            }
            assert_eq!(
                (
                    string_at(&refused, "Code").as_str(),
                    string_at(&refused, "Message").as_str()
                ),
                (LEGAL_HOLD_NOT_FOUND_CODE, LEGAL_HOLD_NOT_FOUND_MESSAGE),
                "case {case:?}: expected the condition's token and sentence, got {refused:?}"
            );
        }
    }

    // The control: the id the account does hold still reads back, so the
    // refusals above follow the id and not the operation.
    let (status, read) = call(&router, "GET", &format!("/legal-holds/{held}"), None).await;
    assert_eq!(
        (status, string_at(&read, "LegalHoldId").as_str()),
        (200, held.as_str()),
        "the held id still reads back, got {status} {read:?}"
    );
}

/// The condition is worded ONCE: the body one hold-scoped call answers for an
/// unheld id is the body every other one answers for it, so a client keyed on
/// `Code` never has to know which operation it called.
#[tokio::test]
async fn the_unheld_id_refusal_is_the_same_body_through_every_operation() {
    let router = router();
    let _held = one_held_hold(&router).await;

    for (form, id) in unheld_ids() {
        let mut answered: Vec<(&str, u16, String, Value)> = Vec::new();
        for (operation, method, path) in hold_scoped_calls(&id) {
            let (status, errortype, refused) = call_reported(&router, method, &path, None).await;
            answered.push((operation, status, errortype, refused));
        }
        let (first_operation, first_status, first_errortype, first_body) = answered[0].clone();
        // Anchored, so three identical WRONG answers cannot satisfy the
        // invariant: the body the others are compared against is the
        // condition's own refusal.
        assert_eq!(
            (
                first_status,
                first_errortype.as_str(),
                string_at(&first_body, "Code").as_str()
            ),
            (400, INVALID_PARAMETER, LEGAL_HOLD_NOT_FOUND_CODE),
            "case {form:?}: expected {first_operation} to refuse the unheld id with the \
             condition's identity, got {first_status} {first_body:?}"
        );
        for (operation, status, errortype, body) in &answered[1..] {
            assert_eq!(
                (*status, errortype, body),
                (first_status, &first_errortype, &first_body),
                "case {form:?}: {operation} must answer this condition exactly as \
                 {first_operation} does, got {status} {errortype:?} {body:?}"
            );
        }
    }
}
