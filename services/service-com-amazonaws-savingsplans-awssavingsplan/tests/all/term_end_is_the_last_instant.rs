// SPDX-License-Identifier: CC0-1.0

//! Where a term ENDS: `end` is the last instant the term covers, not the first
//! instant after it.
//!
//! The rule: a plan's term runs for `termDurationInSeconds` from its `start`,
//! and the plan reports the last instant of it — `start + termDurationInSeconds
//! - 1s` — so two consecutive terms of one commitment do not both claim the
//! instant between them, and a caller computing a billing period from `start`
//! and `end` gets exactly the term it bought.
//!
//! The rows span what the rule generalises over: every term the catalog sells,
//! every plan type it sells them under, and where the purchase time falls
//! relative to now (dated by the service, dated in the past, dated in the
//! future), since that is what decides which state the plan is reported in.
//!
//! Everything goes through the same `MultiServiceRouter` the emulator boots,
//! reusing the harness in [`crate::savings_plans_lifecycle`].

use serde_json::{json, Value};
use service_com_amazonaws_savingsplans_awssavingsplan::state;

use crate::savings_plans_lifecycle::{text, Emulator, ONE_YEAR, THREE_YEARS};

/// Now, in whole seconds since the epoch: what a `purchaseTime` carries.
fn now_seconds() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock after 1970")
            .as_secs(),
    )
    .expect("a clock this century")
}

/// Every plan reports the last instant of its term as `end`, so the span from
/// `start` to `end` is one second short of the term it reports.
#[tokio::test]
async fn a_terms_end_is_its_last_instant() {
    let emulator = Emulator::new();
    let now = now_seconds();

    for plan_type in ["Compute", "EC2Instance", "SageMaker", "Database"] {
        for duration in [ONE_YEAR, THREE_YEARS] {
            for (when, offset) in [
                ("dated by the service", None),
                ("dated a fortnight ago", Some(-14 * 86_400)),
                ("dated for tomorrow", Some(86_400)),
            ] {
                let case = format!("{plan_type}/{duration}s {when}");
                let offering = emulator
                    .offering(plan_type, state::PAYMENT_NO_UPFRONT, duration)
                    .await;
                let mut request = json!({
                    "savingsPlanOfferingId": text(&offering, "offeringId"),
                    "commitment": "0.1",
                });
                if let Some(offset) = offset {
                    request["purchaseTime"] = json!(now + offset);
                }
                let plan = emulator.plan(&emulator.purchase(request).await).await;

                let term = plan
                    .get("termDurationInSeconds")
                    .and_then(Value::as_i64)
                    .unwrap_or_default();
                assert_eq!(
                    term, duration,
                    "case {case:?}: the plan must report the term it was bought for, got {term}"
                );
                let start = state::parse_millis(&text(&plan, "start")).unwrap_or_else(|| {
                    panic!("case {case:?}: start must be an instant, got {plan:?}")
                });
                let end = state::parse_millis(&text(&plan, "end")).unwrap_or_else(|| {
                    panic!("case {case:?}: end must be an instant, got {plan:?}")
                });
                assert_eq!(
                    end - start,
                    term * 1000 - state::TERM_END_INCLUSIVE_MILLIS,
                    "case {case:?}: expected the last instant of a {term}s term (start {start} \
                     ms), got end {end} ms"
                );
                // Stated the other way round, which is the arithmetic a caller
                // does: the term is exactly the span the two instants bound.
                assert_eq!(
                    (end + state::TERM_END_INCLUSIVE_MILLIS - start) / 1000,
                    term,
                    "case {case:?}: start {start} ms to end {end} ms must span the {term}s term"
                );
            }
        }
    }
}
