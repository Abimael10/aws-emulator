// SPDX-License-Identifier: CC0-1.0

//! The unit tests of [`super`]'s stand-in re-pointing, in a file of their own: this module's
//! own file is at the length a change to this repository may touch.

use super::retokenised;

/// A stand-in CloudFormation computed is re-pointed wherever a template composed it into
/// one of its own strings — and left exactly as written where it is only part of a longer
/// identifier, which is a value of its own rather than a reference to the resource.
///
/// The rows span the delimiters a template puts around such a reference and the ways a
/// longer identifier can carry the same characters.
#[test]
fn a_composed_stand_in_is_re_pointed_at_token_boundaries_only() {
    let cases: &[(&str, &str, Option<&str>)] = &[
        // The whole value, and the forms an ARN composes the reference into.
        ("abc123", "abc123", Some("zzz999")),
        (
            "between a colon and a slash",
            "arn:aws:execute-api:us-east-1:000000000000:abc123/*/*",
            Some("arn:aws:execute-api:us-east-1:000000000000:zzz999/*/*"),
        ),
        (
            "at the end of the value",
            "arn:aws:execute-api:us-east-1:000000000000:abc123",
            Some("arn:aws:execute-api:us-east-1:000000000000:zzz999"),
        ),
        (
            "one whole slash-separated segment",
            "integrations/abc123",
            Some("integrations/zzz999"),
        ),
        (
            "more than once in one value",
            "abc123/stage/abc123",
            Some("zzz999/stage/zzz999"),
        ),
        // ...and the negative branch: part of a longer identifier, so not a reference.
        ("a suffixed identifier", "abc123-assoc", None),
        ("a prefixed identifier", "the-abc123", None),
        ("an underscore away from a boundary", "abc123_two", None),
        ("a longer identifier around it", "xabc123x", None),
        ("a value that does not carry it", "arn:aws:sns:us-east-1:000000000000:q", None),
    ];
    for (case, text, expected) in cases {
        let answered = retokenised(text, "abc123", "zzz999");
        assert_eq!(
            answered.as_deref(),
            *expected,
            "case {case:?}: re-pointing {text:?} answered {answered:?}"
        );
    }
}

/// A stand-in that CARRIES a slash is matched by the same rule: a slash is no character
/// of an identifier, so such a stand-in is re-pointed where the whole of it is a token of
/// a longer value and left alone where a longer identifier runs into it.
#[test]
fn a_slash_bearing_stand_in_follows_the_same_boundary_rule() {
    let cases: &[(&str, &str, Option<&str>)] = &[
        ("the whole value", "apis/abc123", Some("zzz999")),
        (
            "composed into a longer value",
            "arn:aws:execute-api:us-east-1:0:apis/abc123/*",
            Some("arn:aws:execute-api:us-east-1:0:zzz999/*"),
        ),
        ("a longer identifier ahead of it", "the-apis/abc123", None),
        ("a longer identifier after it", "apis/abc123-assoc", None),
    ];
    for (case, text, expected) in cases {
        let answered = retokenised(text, "apis/abc123", "zzz999");
        assert_eq!(
            answered.as_deref(),
            *expected,
            "case {case:?}: re-pointing {text:?} answered {answered:?}"
        );
    }
}

/// A stand-in that is not there, and the empty one, re-point nothing: an empty value is
/// no identifier a reference could have resolved to.
#[test]
fn nothing_is_re_pointed_for_an_empty_stand_in() {
    let answered = retokenised("arn:aws:execute-api:us-east-1:0:abc123/*", "", "zzz999");
    assert_eq!(
        answered, None,
        "expected an empty stand-in to re-point nothing, got {answered:?}"
    );
}
