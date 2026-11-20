// SPDX-License-Identifier: CC0-1.0

//! The SIZE of the payload a schedule hands its target: `Target$Input`'s maximum.
//!
//! `Target$Input` is "the text, or well-formed JSON, passed to the target"
//! (`smithy_query(query="doc", shape="com.amazonaws.scheduler#Target", member="Input")`),
//! and the model bounds it only from BELOW — `TargetInput` is `@length` min 1 with no
//! maximum (`smithy_query(query="shape", shape="com.amazonaws.scheduler#TargetInput")`).
//! The upper bound is therefore a fact of the SERVICE: EventBridge Scheduler accepts a
//! payload of at most [`MAX_TARGET_INPUT_BYTES`] and refuses anything larger, so a client
//! that sizes its payloads against that bound is told when it crosses it instead of
//! having a schedule stored that the service would never have taken.
//!
//! The bound is on the payload's SIZE — the bytes the text occupies, which is what travels
//! to the target — and not on the number of characters it is written in, so text outside
//! ASCII is measured as the bytes it encodes to.
//!
//! One concern, shared by the two operations that WRITE a schedule (`CreateSchedule` and
//! `UpdateSchedule`): both carry the member, so both are held to the bound here rather
//! than each spelling its own — an update cannot move a stored schedule onto a payload a
//! create would have refused.
//!
//! The refusal is the 400 `ValidationException` every broken bound of this surface takes.
//! Its sentence is this service's own statement ABOUT THE TARGET's payload rather than the
//! member-and-constraint report the framework words a modelled bound with — the bound is
//! the service's, so the sentence names the subject (the target's input), the limit and its
//! unit. That wording is the service's recorded refusal of an over-size payload, which is
//! the only place this bound's sentence is stated: neither the model nor the member's
//! documentation carries it ([`input_size_exceeded`]).

/// The largest `Target$Input` this service accepts, in bytes: the service's maximum target
/// payload, which the refusal below states as its limit. A payload of exactly this many
/// bytes is accepted; one byte more is not.
pub const MAX_TARGET_INPUT_BYTES: usize = 262_144;

/// How this service states a target payload past that maximum: the subject of the bound,
/// the limit and the unit it is counted in. Fixed text but for the limit, which is the
/// service's own ([`MAX_TARGET_INPUT_BYTES`]) and is reported so a client learns the bound
/// it crossed.
fn input_size_exceeded() -> String {
    format!("The input size for the target cannot exceed {MAX_TARGET_INPUT_BYTES} bytes.")
}

/// Refuse a `Target$Input` past the maximum payload this service takes; `None` for a
/// payload inside the bound, and for a target carrying none at all — "if you do not specify
/// anything for this field, EventBridge Scheduler delivers a default notification to the
/// target" (`Target$Input`).
pub fn target_input_violation(input: Option<&str>) -> Option<String> {
    let input = input?;
    // The bound is a SIZE: the bytes of the payload, as it is handed to the target, rather
    // than the characters it is written in.
    (input.len() > MAX_TARGET_INPUT_BYTES).then(input_size_exceeded)
}

#[cfg(test)]
mod tests {
    //! The bound this module owns, over a SET of payloads rather than one recorded
    //! payload. The same rule is driven through real requests, on both operations that
    //! write a schedule, in `tests/all/target_input_size.rs`.

    use super::*;

    /// Everything up to and including the maximum is a payload the service takes, and a
    /// target carrying no payload is not a payload at all. The size is counted in BYTES,
    /// so a payload of multi-byte text is admitted up to the same byte count.
    #[test]
    fn a_payload_inside_the_bound_is_not_refused() {
        let rows: [(&str, Option<String>); 6] = [
            ("no payload at all", None),
            ("the shortest payload the model admits", Some("x".to_string())),
            ("a small JSON document", Some("{\"a\":1}".to_string())),
            (
                "a payload one byte short of the maximum",
                Some("x".repeat(MAX_TARGET_INPUT_BYTES - 1)),
            ),
            (
                "a payload exactly at the maximum",
                Some("x".repeat(MAX_TARGET_INPUT_BYTES)),
            ),
            (
                // Half as many characters as the bound admits bytes, which is exactly the
                // bound once each is encoded.
                "two-byte characters filling the maximum exactly",
                Some("é".repeat(MAX_TARGET_INPUT_BYTES / 2)),
            ),
        ];

        for (case, payload) in rows {
            let refusal = target_input_violation(payload.as_deref());
            assert!(
                refusal.is_none(),
                "case {case:?}: expected no refusal for a payload of {:?} bytes, got \
                 {refusal:?}",
                payload.map(|payload| payload.len())
            );
        }
    }

    /// Anything past the maximum is refused, with the service's sentence about the target's
    /// payload — the same sentence whatever the payload's form or how far past the bound it
    /// is.
    #[test]
    fn a_payload_past_the_bound_states_the_limit_and_its_unit() {
        let rows: [(&str, String); 4] = [
            (
                "a payload one byte past the maximum",
                "x".repeat(MAX_TARGET_INPUT_BYTES + 1),
            ),
            (
                "a payload twice the maximum",
                "x".repeat(MAX_TARGET_INPUT_BYTES * 2),
            ),
            (
                "a JSON document past the maximum",
                format!("{{\"a\":\"{}\"}}", "x".repeat(MAX_TARGET_INPUT_BYTES)),
            ),
            (
                // One character past half the bound: the byte count crosses it, the
                // character count is nowhere near it.
                "two-byte characters one character past the maximum",
                "é".repeat(MAX_TARGET_INPUT_BYTES / 2 + 1),
            ),
        ];

        for (case, payload) in rows {
            let refusal = target_input_violation(Some(&payload));
            assert_eq!(
                refusal.as_deref(),
                Some(input_size_exceeded().as_str()),
                "case {case:?}: expected the limit stated for a payload of {} bytes, got \
                 {refusal:?}",
                payload.len()
            );
        }
    }
}
