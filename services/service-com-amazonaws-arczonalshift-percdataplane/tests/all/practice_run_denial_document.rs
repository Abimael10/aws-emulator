// SPDX-License-Identifier: CC0-1.0

//! The document a practice-run access denial is SERVED with, byte for byte.
//!
//! RULE: ARC decides the caller's authorization on a call's target in the layer in front of
//! the operation and answers the denial from there, so a 403 `AccessDeniedException` is
//! serialized in that layer's envelope rather than in the operation's: the class is named in
//! `x-amzn-errortype` alone and the document carries the denial text alone, under `Message`,
//! with no `__type` discriminator and no modelled lowercase `message`. A denial that states
//! no reason — `AccessDeniedException$message` is optional, and a denial taken on a shift id
//! has no resolved target to report about — therefore answers a JSON object with NO MEMBERS.
//!
//! `access_denial_envelope.rs` states that rule over this service's whole denial surface, by
//! the members a parsed document carries. What this suite adds is the answer AS SERIALIZED —
//! the bytes an SDK, a proxy or a comparison against a recorded response reads — for the
//! denial keyed by a `zonalShiftId` this service holds no shift for, on the route a
//! practice-run cancel takes (`DELETE /practiceruns/{zonalShiftId}`), where the document is
//! the whole of what distinguishes the answer: every member a parse reports absent is absent
//! because nothing was written, so the bytes are the assertion.
//!
//! The rows span what the rule generalises over: the FORM and SIZE of the identifier the
//! denial was taken on (each end of `ZonalShiftId`'s modelled length range and each kind of
//! character its pattern admits), and the STATE of the store the denial is read from
//! (holding no shift, and holding a practice run of its own) — the denial is computed from
//! the store, so neither the empty store nor one with a run in progress changes the answer.

use super::access_denied_precedence::UNKNOWN_ZONAL_SHIFT_IDS;
use super::practice_run_start::{given_configuration, start_practice_run};
use super::zonal_shift_lifecycle::{emulator, seeded, served};

/// The whole document ARC's authorization layer answers a denial with when it states no
/// reason: a JSON object with no members.
const EMPTY_DOCUMENT: &str = "{}";

/// RULE, on the route a practice-run cancel takes: a `zonalShiftId` this service holds no
/// shift for is denied 403, naming its class in `x-amzn-errortype`, and the denial is served
/// as the empty JSON document — not as an error document of the operation's own, and not as
/// an absent body.
#[tokio::test]
async fn a_practice_run_denial_is_served_as_the_empty_json_document() {
    // What the store holds when the denial is read, as `(the state, whether a run is started
    // to reach it)`: the flag drives the setup, so the branch does not depend on the prose a
    // failure is reported with.
    for (population, start_a_run) in [
        ("a store holding no shift", false),
        ("a store holding practice runs", true),
    ] {
        // One emulator per state, so a failing row stands on its own rather than on what the
        // row before it left in the store.
        let (router, engine) = emulator();
        if start_a_run {
            // The denial is computed from the store, so the rule is asserted again once the
            // store does hold a practice run. One resource the store seeds carries it — which
            // resource is `practice_run_cancel.rs`'s subject, not this one's.
            let (resource, zones) = seeded(&engine)[0].clone();
            given_configuration(&router, &resource, &[]).await;
            let (status, started) =
                start_practice_run(&router, &resource, &zones[0], "a run of its own").await;
            assert_eq!(
                status, 200,
                "setup {population:?}: a practice run for {resource} starts first, \
                 got {status} {started}"
            );
        }
        // The table's second element is the CLASS of identifier the row stands for, named
        // the way its two other readers name it.
        for (id, class) in UNKNOWN_ZONAL_SHIFT_IDS {
            let case = format!("{population} / {class}");
            let (status, answered, document) =
                served(&router, "DELETE", &format!("/practiceruns/{id}"), "").await;
            assert_eq!(
                status, 403,
                "case {case:?}: a target this service holds no shift for is denied 403, \
                 got {status} {document:?}"
            );
            assert_eq!(
                answered, "AccessDeniedException",
                "case {case:?}: expected the class in x-amzn-errortype, got {answered:?} \
                 with {document:?}"
            );
            assert_eq!(
                document, EMPTY_DOCUMENT,
                "case {case:?}: expected the denial served as {EMPTY_DOCUMENT:?}, \
                 got {document:?}"
            );
        }
    }
}
