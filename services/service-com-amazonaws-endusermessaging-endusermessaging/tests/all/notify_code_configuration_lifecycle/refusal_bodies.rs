// SPDX-License-Identifier: CC0-1.0

//! How an UPDATE of a notify code configuration that is outside a modelled `@range` is
//! worded, over the wire.
//!
//! Every assertion goes through `MultiServiceRouter::dispatch` (this suite's parent
//! harness), so what is read is the rendered restJson1 error body an SDK client parses —
//! the status, the `x-amzn-errortype` it resolves the error shape from, the `message` it
//! surfaces and the `fieldList` it walks to find the member at fault.
//!
//! The rule under test, over a SET of inputs rather than one recorded request: a value
//! outside a member's `@range` is refused in AWS's constraint envelope — "<N> validation
//! error(s) detected. " over one clause per violated member, "Value at '<json-pointer>'
//! failed to satisfy constraint: …" — where a member the model bounds on BOTH sides states
//! that whole window ("Member must be between <min> and <max>, inclusive") rather than the
//! single bound the value fell outside, no submitted value is echoed anywhere in it, and
//! `fieldList` carries one `{path, message}` entry per violation, under that member's own
//! JSON pointer and stating that same clause.
//!
//! What this suite adds rather than repeats: the UPDATE path, whose own suite
//! ([`super::update`]) asserts only the status and the error code of such a refusal, and
//! `maxAttempts`, which no suite's rows reach. The same envelope on the CREATE path, the
//! several-violation count and the no-echo rule are already pinned by
//! `tests/all/brand_profile_refusal_bodies.rs`, which drives this service's whole refusal
//! renderer; the window is read off the model, so the two paths cannot disagree about one
//! member's bounds.

use serde_json::json;

use super::{Emulator, Reply};

/// The `fieldList` entries an error body carries, as `(path, message)` pairs. An absent
/// list answers an empty vector rather than panicking, so an assertion reports what was
/// missing.
fn field_list(reply: &Reply) -> Vec<(String, String)> {
    reply.body["fieldList"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    (
                        entry["path"].as_str().unwrap_or_default().to_string(),
                        entry["message"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The bounded members of the passcode policy an update carries, each with the window the
/// model declares for it.
///
/// Source: `smithy_query(query="shape",shape="UpdateCodeConfigurationParameters")` —
/// `CodeLength` @range(4..8), `ValidityPeriodMinutes` @range(1..60) and
/// `MaxVerificationAttempts` @range(1..5).
fn bounded_policy_members() -> Vec<(&'static str, i64, i64)> {
    vec![
        ("codeLength", 4, 8),
        ("validityPeriodMinutes", 1, 60),
        ("maxAttempts", 1, 5),
    ]
}

/// The clause AWS states about a member outside its `@range`: the member addressed by its
/// JSON pointer, and the whole inclusive window the model declares.
fn window_clause(pointer: &str, min: i64, max: i64) -> String {
    format!(
        "Value at '{pointer}' failed to satisfy constraint: Member must be between \
         {min} and {max}, inclusive"
    )
}

#[tokio::test]
async fn an_updates_range_refusal_states_the_whole_window_and_names_the_member() {
    let em = Emulator::new();
    let created = em.create_named("bounded").await;
    let id = created.string("notifyCodeConfigurationId");
    let stored = created.resource().clone();

    for (member, min, max) in bounded_policy_members() {
        let pointer = format!("/codeConfigurationParameters/{member}");
        let clause = window_clause(&pointer, min, max);
        // One step outside EACH published bound, so neither clause is the single bound the
        // value fell outside.
        for value in [min - 1, max + 1] {
            let case = format!("{member}={value}");
            let refused = em
                .update(
                    &id,
                    json!({ "codeConfigurationParameters": { member: value } }),
                )
                .await;
            assert_eq!(
                refused.status, 400,
                "case {case}: expected 400, got {} / {:?}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.error.as_deref(),
                Some("ValidationException"),
                "case {case}: expected ValidationException, got {:?} / {:?}",
                refused.error,
                refused.body
            );
            assert_eq!(
                refused.top("message"),
                format!("1 validation error detected. {clause}"),
                "case {case}: expected the envelope over {clause:?}, got {:?}",
                refused.body
            );
            assert_eq!(
                field_list(&refused),
                vec![(pointer.clone(), clause.clone())],
                "case {case}: the fieldList must carry that clause under the member's own \
                 pointer, got {:?}",
                refused.body
            );
            assert!(
                !refused.top("message").contains(&format!("Value '{value}'")),
                "case {case}: the refusal must quote no submitted value, got {:?}",
                refused.body
            );
            // The refusal is ahead of the write, so the configuration is as it was.
            let read_back = em.get(&id).await;
            assert_eq!(
                read_back.resource(),
                &stored,
                "case {case}: a refused update must change nothing, got {:?}",
                read_back.body
            );
        }
    }
}
