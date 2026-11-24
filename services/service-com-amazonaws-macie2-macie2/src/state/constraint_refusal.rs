// SPDX-License-Identifier: CC0-1.0

//! How this service refuses a request member that carries more elements than
//! the size its own documentation states.
//!
//! A size a MODELLED constraint declares is refused ahead of every handler by
//! the framework's constraint walk, which words one clause per violation inside
//! AWS's `ValidationException` envelope. A size the model states only in PROSE
//! is invisible to that walk, so the handler applies it — and a handler that
//! words its own sentence for it makes one service report two different ways
//! about the same kind of violation. [`list_over_cap_message`] is the one place
//! such a refusal is worded here: the envelope, the member path and the
//! requirement all come from the framework's own renderer, so a cap this
//! service enforces reads exactly like one the model declares.
//!
//! The clause quotes the list the caller submitted, which is the only part of
//! the sentence a request decides; it is rendered the way a list renders and
//! held to the ceiling the shared module holds any quoted value to, so no
//! refusal is sized by the request that earned it.

use ::aws_smithy_schema_server::constraints::path;
use ::aws_smithy_schema_server::{envelope, Violation, ViolationKind};
use ::service_common::validation::{quoted_echo, rendered_list};

/// AWS's constraint-violation refusal for `member`, a list the request filled
/// with more than `max` elements.
///
/// `submitted` is the list AS SENT — every element, in request order — because
/// the value half of the clause is where a caller sees its own input reported
/// back. The CHECK is made by the caller of this function against the list
/// itself; this only renders.
pub fn list_over_cap_message(member: &str, submitted: &[&str], max: usize) -> String {
    let rendered = rendered_list(submitted);
    let violation = Violation {
        path: path::member(member),
        kind: ViolationKind::MaxLength(max as i64),
        value: Some(quoted_echo(&rendered).into_owned()),
        sensitive: false,
    };
    // One violation always renders an envelope; the fallback keeps this
    // infallible rather than answering a refusal with no sentence at all.
    envelope(&[violation]).unwrap_or_default()
}

/// [`list_over_cap_message`] over a list of owned strings, which is the form
/// every such member arrives in.
pub fn strings_over_cap_message(member: &str, submitted: &[String], max: usize) -> String {
    let elements: Vec<&str> = submitted.iter().map(String::as_str).collect();
    list_over_cap_message(member, &elements, max)
}

#[cfg(test)]
mod tests {
    use super::{list_over_cap_message, strings_over_cap_message};

    /// The refusal states AWS's envelope over one clause: the submitted list as
    /// a list renders, the member it sat at and the cap it broke — whatever the
    /// member, the cap and the elements are.
    #[test]
    fn the_refusal_states_the_envelope_the_member_and_the_cap() {
        for (case, member, submitted, max, expected) in [
            (
                "one element past a cap of one",
                "findingIds",
                vec!["a", "b"],
                1usize,
                "1 validation error detected: Value '[a, b]' at 'findingIds' failed to satisfy \
                 constraint: Member must have length less than or equal to 1",
            ),
            (
                "another member and another cap",
                "keywords",
                vec!["one", "two", "three"],
                2,
                "1 validation error detected: Value '[one, two, three]' at 'keywords' failed to \
                 satisfy constraint: Member must have length less than or equal to 2",
            ),
            (
                "an element carrying a space",
                "accountIds",
                vec!["an account"],
                0,
                "1 validation error detected: Value '[an account]' at 'accountIds' failed to \
                 satisfy constraint: Member must have length less than or equal to 0",
            ),
        ] {
            let actual = list_over_cap_message(member, &submitted, max);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
            let owned: Vec<String> = submitted.iter().map(|value| (*value).to_string()).collect();
            let from_owned = strings_over_cap_message(member, &owned, max);
            assert_eq!(
                from_owned, expected,
                "case {case:?}: the owned-string form must word the same refusal, \
                 got {from_owned:?}"
            );
        }
    }

    /// A list far longer than any clause may carry is quoted under the shared
    /// ceiling rather than in full, so the response is not sized by the
    /// request: the elision marker the shared module writes is what appears.
    #[test]
    fn a_list_past_the_quoting_ceiling_is_elided() {
        let elements: Vec<String> = (0..4096).map(|index| format!("id-{index:08}")).collect();
        let message = strings_over_cap_message("findingIds", &elements, 50);
        assert!(
            message.contains("bytes]"),
            "a list past the ceiling must be quoted under it, got {} bytes: {message:?}",
            message.len()
        );
        assert!(
            message.len() < 17 * 1024,
            "the refusal must not be sized by the request, got {} bytes",
            message.len()
        );
    }
}
