// SPDX-License-Identifier: CC0-1.0

//! The refusal that enforces the generated-template in-flight cap: what the MODEL fixes about
//! it, end to end over the awsQuery wire.
//!
//! RULE: a `CreateGeneratedTemplate` that would start one more generation than
//! [`generated::MAX_CONCURRENT_GENERATIONS`] allows is refused with the modelled
//! `ConcurrentResourcesLimitExceededException` — HTTP 429, awsQuery code
//! `ConcurrentResourcesLimitExceeded`, carrying a `Message` the model bounds to
//! `@length(min: 1, max: 255)`.
//!
//! What this suite deliberately does NOT assert is the message's WORDING. The sentence AWS
//! answers for this error is not among the recordings this emulator has: three readings of
//! the model's own text have been tried against the recorded refusal and none of them is
//! AWS's — the documented sentence, that sentence with its terminal period inside this
//! surface's SDK envelope, and the whole documented paragraph — so a wording assertion here
//! could only re-state the literal the handler carries, which cannot fail against the wrong
//! wording. That cause is left open on its own issue, which is where the recorded sentence has
//! to come from. The bound this suite does assert is the part of the message the model fixes,
//! and it is what rules the enveloped form out: the documented sentence wrapped in this
//! surface's SDK envelope is longer than the `Message` the model admits.

use std::time::SystemTime;

use service_com_amazonaws_cloudformation_cloudformation::state::generated_template as generated;

use crate::generated_templates::create_generated_template;
use crate::harness::{refusal, router_with_engine, still_running, store, REGION};

/// The most characters `ErrorMessage` admits: the modelled `@length(min: 1, max: 255)` every
/// error message of this service is bounded by.
const MAX_ERROR_MESSAGE: usize = 255;

/// RULE: with the in-flight set full, the next create is refused 429
/// `ConcurrentResourcesLimitExceeded`, carrying a message inside the bound the model fixes.
#[tokio::test]
async fn the_in_flight_cap_refusal_carries_the_modelled_error_and_a_bounded_message() {
    let (router, engine) = router_with_engine();
    let case = "the in-flight set full";

    // The cap is metered over the records whose generation has not concluded, so the set is
    // seeded as records still in flight: their conclusion is due at an INSTANT far enough
    // ahead that the case below reads them as running, and nothing sleeps for it.
    {
        let state = store(&engine);
        let mut held = state.lock();
        for index in 0..generated::MAX_CONCURRENT_GENERATIONS {
            let mut record = generated::StoredGeneratedTemplate::starting(
                format!(
                    "arn:aws:cloudformation:{REGION}:000000000000:generatedtemplate/\
                     00000000-0000-0000-0000-00000000000{index}"
                ),
                format!("in-flight-{index}"),
                REGION.to_string(),
                SystemTime::now(),
            );
            if let Some(pending) = record.pending.as_mut() {
                pending.due_at = still_running();
            }
            held.generated_templates.push(record);
        }
    }

    let (status, body) = create_generated_template(&router, "one-too-many", &[]).await;
    assert_eq!(
        status, 429,
        "case {case:?}: expected the modelled 429, got {status} with {body}"
    );
    let (code, message) = refusal(&body);
    assert_eq!(
        code, "ConcurrentResourcesLimitExceeded",
        "case {case:?}: expected the modelled awsQuery code, got {code:?} in {body}"
    );
    // The bound the model fixes on the message: present, and no longer than `ErrorMessage`
    // admits — which the documented sentence inside this surface's SDK envelope would exceed.
    assert!(
        (1..=MAX_ERROR_MESSAGE).contains(&message.chars().count()),
        "case {case:?}: expected a message of 1..={MAX_ERROR_MESSAGE} characters, got {} in \
         {message:?}",
        message.chars().count()
    );
    // And the refusal left no record behind: the cap is metered, not consumed.
    let held = store(&engine).lock().generated_templates.len();
    assert_eq!(
        held,
        generated::MAX_CONCURRENT_GENERATIONS,
        "case {case:?}: expected the refused call to create no record, got {held} records"
    );
}
