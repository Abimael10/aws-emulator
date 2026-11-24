Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: a6a61ced6b2cd7272d50263471688627aff13b44

# Deduplicate the "cut a string to N bytes at a character boundary" computation implemented twice in this crate

_Filed: 2026-11-24T20:04:01Z_
_Operation: ListRecords | Class: maintainability_

> _Note: this issue was identified against the code base at commit `a6a61ced6b2cd7272d50263471688627aff13b44`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate computes the same thing in two places: the index of the last UTF-8 character boundary at or below a byte bound, used to cut a string to that bound. `crate::state::bounded_message` cuts a refusal message to the modelled `Message` maximum, and `crate::state::page_cursor::leading_characters` cuts a record identifier to the maximum prefix a page token can carry. The two copies are structurally identical (`(0..=bound).rev().find(|at| s.is_char_boundary(*at)).unwrap_or_default()`), carry the same stated rationale in their doc comments, and are reached from the same handler (`ListRecords` words its refusals through the first and mints its `NextToken` through the second), so they can drift: a fix to the boundary search in one leaves the other wrong, and only one of the two bounds would then still hold in bytes AND in characters. The second copy should be the single statement of the computation and the first should call it (e.g. `message.truncate(leading_characters(&message, MAX_MESSAGE_LENGTH).len())`), which changes no response and no existing test — both cut indexes are computed identically today.

## Evidence

Copy 1 — services/service-com-amazonaws-sagemakerfeaturestoreruntime-amazonsagemakerfeaturestoreruntime/src/state.rs:899-912:

    /// The cut is at the last character boundary within the bound, counted in bytes, so the
    /// message stays valid UTF-8 and is inside the bound whether a string's length is counted
    /// in bytes or in characters.
    pub fn bounded_message(message: impl Into<String>) -> String {
        let mut message = message.into();
        if message.len() > MAX_MESSAGE_LENGTH {
            let cut = (0..=MAX_MESSAGE_LENGTH)
                .rev()
                .find(|at| message.is_char_boundary(*at))
                .unwrap_or_default();
            message.truncate(cut);
        }
        message
    }

Copy 2 — services/service-com-amazonaws-sagemakerfeaturestoreruntime-amazonsagemakerfeaturestoreruntime/src/state/page_cursor.rs:243-256:

    /// `identifier` cut to at most `max_bytes`, at a character boundary.
    ///
    /// The cut is in bytes and lands on a boundary, so the prefix is still valid UTF-8 and is
    /// inside the bound whether the token's length is counted in bytes or in characters.
    fn leading_characters(identifier: &str, max_bytes: usize) -> &str {
        if identifier.len() <= max_bytes {
            return identifier;
        }
        let cut = (0..=max_bytes)
            .rev()
            .find(|at| identifier.is_char_boundary(*at))
            .unwrap_or_default();
        &identifier[..cut]
    }

Both copies are reached from the one handler: `src/ops/list_records.rs:247-252` (`validation(...)` -> `crate::state::bounded_message`) and `src/ops/list_records.rs:233` (`next_cursor.map(|cursor| cursor.encode())`, whose cursor is built by `cursor_at` -> `leading_characters`, page_cursor.rs:199).

Test coverage that must stay green and unchanged: tests/all/error_message_length_bound.rs:418 `a_refusal_quoting_a_request_value_is_cut_at_a_character_boundary`, :639 `a_refusal_an_operation_words_itself_is_cut_to_the_bound_too`, :735 `a_refusal_is_answered_whole_up_to_the_bound_and_cut_only_past_it` (copy 1), and tests/all/online_store_record_lifecycle.rs:1791 `every_page_token_is_inside_the_modelled_bound_whatever_identifier_it_resumes_at`, :1873 `a_cursor_at_an_identifier_too_long_for_a_token_still_names_its_record` (copy 2).

## Steps to Reproduce

1. Open services/service-com-amazonaws-sagemakerfeaturestoreruntime-amazonsagemakerfeaturestoreruntime/src/state.rs and read `bounded_message` (lines 902-912): it searches `(0..=MAX_MESSAGE_LENGTH).rev()` for `is_char_boundary` and truncates there.
2. Open src/state/page_cursor.rs and read `leading_characters` (lines 247-256): it performs the identical search over `(0..=max_bytes).rev()` and slices there.
3. Observe that `src/ops/list_records.rs` reaches both — `validation()` at line 247 goes through `bounded_message`, and the `NextToken` minted at line 233 goes through `cursor_at` -> `leading_characters` — so one handler depends on two copies of one computation.
4. Confirm the de-duplication is behaviour-preserving: for any `s` and bound `n`, `bounded_message` keeps `s[..cut]` where `cut` is exactly the index `leading_characters(s, n)` ends at, so routing copy 1 through copy 2 leaves every response byte-identical and the tests named in the evidence unchanged.
