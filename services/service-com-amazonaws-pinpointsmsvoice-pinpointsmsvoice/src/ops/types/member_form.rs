// SPDX-License-Identifier: CC0-1.0

//! The form a value a request binds to one of this model's NAMED string shapes must take.
//!
//! Two of this model's string shapes are named for the FORM their value takes rather than
//! for the thing it names — `WordCharactersWithDelimiters` and `NonEmptyString`
//! (`smithy_query(query="shape",shape="WordCharactersWithDelimiters,NonEmptyString",model_path="services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/smithy-model/com-amazonaws-pinpointsmsvoice-pinpointsmsvoice.json")`:
//! both are plain string shapes carrying no `@pattern` and no `@length`). The form a shape
//! admits belongs to THE SHAPE, so it is the same for every member targeting it, whichever
//! operation that member belongs to: this module is the single home of one predicate per
//! shape, so two operations cannot admit different values for one shape — which is what
//! would let one of them record a name another one says can never exist.
//!
//! What a predicate here decides is only WHETHER a value is usable:
//!
//! * the wording of the refusal is [`super::refusal_wording`]'s, so a value this service's
//!   validation cannot use is refused the same way wherever it was bound;
//! * a value a predicate admits is stored EXACTLY as sent — trimming is only how blankness
//!   is detected, never what is recorded, because every operation that resolves a resource
//!   by name resolves it by the raw value the caller sent.

/// The characters a `WordCharactersWithDelimiters` value may carry BESIDE a word character.
///
/// The shape carries no `@pattern`, so the form it admits is the one its name states: word
/// characters, separated by the delimiters a name is written with. These are those
/// delimiters; everything else (a space, punctuation) is not part of a name. A word
/// character itself — an ASCII letter, a digit or `_`, the `\w` class — is admitted wherever
/// it appears and is not listed here.
const NAME_DELIMITERS: [char; 2] = ['-', '.'];

/// Whether `value` is word characters with delimiters: the form `WordCharactersWithDelimiters`
/// states (see [`NAME_DELIMITERS`]).
///
/// A word character is an ASCII letter, a digit or `_`; a delimiter is one of
/// [`NAME_DELIMITERS`]. Such a value has to be WORDS written with delimiters, so it carries
/// at least one word character — a value of delimiters alone (`-`, `.`, `-.-`) spells no
/// word, and neither does the empty one. Anything else is not of this form either: a space,
/// punctuation, or a letter outside ASCII.
pub fn is_word_characters_with_delimiters(value: &str) -> bool {
    value.chars().any(is_word_character)
        && value
            .chars()
            .all(|character| is_word_character(character) || NAME_DELIMITERS.contains(&character))
}

/// Whether `character` is a word character: an ASCII letter, a digit, or `_`.
fn is_word_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

/// Whether `value` is a non-empty string: the form `NonEmptyString` states.
///
/// A value of whitespace alone carries no content, so it is as empty as the empty string —
/// the shape admits neither.
pub fn is_non_empty_string(value: &str) -> bool {
    !value.trim().is_empty()
}

/// Why the value a request bound to one OPTIONAL member cannot be served, or `None` when
/// the member bound nothing at all or bound a value its shape (`admits`) accepts.
///
/// An absent optional member is a request that made no choice, which is served; a member
/// the request DID bind reached this service's validation, so a value outside its shape's
/// form is a parameter that validation cannot use ([`super::refusal_wording::Unusable::Unreadable`]),
/// named in the one refusal the request earns. The two cases are kept apart here so no
/// handler reads a value it cannot use as though the request had named none.
///
/// WHAT COUNTS AS BOUND is what separates this from
/// [`super::refusal_wording::optional_parameter`], which answers the same question for a
/// value travelling as a URI label or a query parameter: there an EMPTY value is no value
/// at all, because neither carrier can distinguish an empty one from an absent one, so it
/// is read as the absence it is indistinguishable from. A STRUCTURE MEMBER is the other
/// case — the request either carries the member or it does not, so an empty value is one
/// the request chose to send, and it is judged against its shape's form like any other
/// bound value. That is why `""` is served as "no choice" for a query parameter and refused
/// for a body member, and why a member's reader asks this function rather than that one.
pub fn unusable_optional_member(
    value: Option<&str>,
    admits: fn(&str) -> bool,
) -> Option<super::refusal_wording::Unusable> {
    value
        .filter(|bound| !admits(bound))
        .map(|_| super::refusal_wording::Unusable::Unreadable)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a `WordCharactersWithDelimiters` value is word characters written with the
    /// delimiters, and nothing else — so the predicate admits exactly that form whatever
    /// member it is reached from.
    ///
    /// The rows differ along what the rule generalises over: the character class, where in
    /// the value the offending character sits, and the length of the value.
    #[test]
    fn the_name_form_admits_word_characters_written_with_delimiters() {
        for (case, value, expected) in [
            ("lower case word", "lowercase", true),
            ("upper case word", "UPPERCASE", true),
            ("digits alone", "0123456789", true),
            ("an underscore alone", "_", true),
            ("every admitted class together", "voice.events_2026-config", true),
            ("an inner space", "voice events", false),
            ("a trailing space", "voice-events ", false),
            ("punctuation", "voice-events!!", false),
            ("a non-ASCII letter", "vöice-events", false),
            ("the empty value", "", false),
            ("whitespace alone", "   ", false),
            ("delimiters alone", "-.-", false),
        ] {
            let answer = is_word_characters_with_delimiters(value);
            assert_eq!(
                answer, expected,
                "case {case:?} ({value:?}): expected {expected}, got {answer}"
            );
        }
    }

    /// RULE: a `NonEmptyString` value carries content, so neither the empty value nor
    /// whitespace alone is one — in whichever form that whitespace arrives.
    #[test]
    fn the_non_empty_form_admits_only_a_value_carrying_content() {
        for (case, value, expected) in [
            ("content", "+12065550100", true),
            ("content with padding", "  +12065550100  ", true),
            ("content with whitespace inside it", "a b", true),
            ("the empty value", "", false),
            ("one space", " ", false),
            ("one tab", "\t", false),
            ("a newline", "\n", false),
            ("several kinds of whitespace", " \t\r\n ", false),
        ] {
            let answer = is_non_empty_string(value);
            assert_eq!(
                answer, expected,
                "case {case:?} ({value:?}): expected {expected}, got {answer}"
            );
        }
    }

    /// RULE: an optional member is unusable only when the request BOUND a value its shape
    /// does not admit; binding nothing is a choice this service serves.
    ///
    /// The rows differ along which shape's form the member targets and whether the request
    /// bound a value at all.
    #[test]
    fn an_optional_member_is_unusable_only_when_it_bound_a_value_its_shape_rejects() {
        for (case, value, admits, expected) in [
            (
                "the name form, absent",
                None,
                is_word_characters_with_delimiters as fn(&str) -> bool,
                None,
            ),
            (
                "the name form, admitted",
                Some("voice-events"),
                is_word_characters_with_delimiters as fn(&str) -> bool,
                None,
            ),
            (
                "the name form, a value it does not admit",
                Some("voice events!!"),
                is_word_characters_with_delimiters as fn(&str) -> bool,
                Some(super::super::refusal_wording::Unusable::Unreadable),
            ),
            (
                "the name form, the empty value",
                Some(""),
                is_word_characters_with_delimiters as fn(&str) -> bool,
                Some(super::super::refusal_wording::Unusable::Unreadable),
            ),
            (
                "the non-empty form, absent",
                None,
                is_non_empty_string as fn(&str) -> bool,
                None,
            ),
            (
                "the non-empty form, content",
                Some("+12065550199"),
                is_non_empty_string as fn(&str) -> bool,
                None,
            ),
            (
                "the non-empty form, whitespace alone",
                Some("   "),
                is_non_empty_string as fn(&str) -> bool,
                Some(super::super::refusal_wording::Unusable::Unreadable),
            ),
        ] {
            let answer = unusable_optional_member(value, admits);
            assert_eq!(
                answer, expected,
                "case {case:?}: expected {expected:?}, got {answer:?}"
            );
        }
    }
}
