// SPDX-License-Identifier: CC0-1.0

//! THE NETBIOS SHORT NAME RULE: which `ShortName` a create may give a directory, and the
//! sentence a create that breaks it is refused with.
//!
//! The rule is Directory Service's own, and it is the one the refusal PUBLISHES: a short
//! name is between [`MIN_SHORT_NAME`] and [`MAX_SHORT_NAME`] characters long and carries
//! only alphanumerics and the special characters of [`ADMITTED_SPECIALS`]. It is NARROWER
//! than the modelled `DirectoryShortName` `@pattern`
//! (`smithy_query(query="shape",shape="DirectoryShortName")`), which bounds the length not
//! at all and excludes only a handful of characters after the first one: a name of twenty
//! characters, or one carrying a period anywhere but at its start, satisfies the modelled
//! expression and is still not a name a Windows domain can be given. So the expression the
//! boundary applies cannot answer this, and a create that names such a short name is
//! refused by the operation with the modelled `InvalidParameterException` — before anything
//! is resolved or stored, so a refused create leaves no directory behind.
//!
//! The CHECK and the SENTENCE are built from the same three constants, so the rule the
//! message publishes to a caller is the rule the check applies: a character this module
//! admits cannot be one the message calls not allowed, and a bound it enforces cannot be
//! one the message states differently.

/// The fewest characters a short name carries, as the published rule states it.
const MIN_SHORT_NAME: usize = 1;

/// The most characters a short name carries, as the published rule states it — the NetBIOS
/// name length a Windows domain is limited to.
const MAX_SHORT_NAME: usize = 15;

/// The special characters a short name may carry beside the alphanumerics, as the published
/// rule lists them.
///
/// Where this list and [`REFUSED_SPECIALS`] disagree — the circumflex is in both, as the
/// sentence Directory Service publishes has it — the ADMITTED list decides, since it is the
/// one that says what a name may be made of.
const ADMITTED_SPECIALS: [char; 15] = [
    '!', '@', '#', '$', '%', '^', '&', '(', ')', '-', '_', '\'', '{', '}', '~',
];

/// The special characters the published rule names as not allowed, in the order it lists
/// them. Reported rather than applied: what a name may carry is [`ADMITTED_SPECIALS`], and
/// every character of this list but the circumflex is outside it.
const REFUSED_SPECIALS: [char; 11] = ['^', '\\', '/', ':', '*', '?', '"', '<', '>', '.', '|'];

/// Why the `ShortName` a create asked for is not a name a directory can be given, or `None`
/// when it is one.
///
/// Every clause of the published rule, applied to the name the request carried: its length
/// in CHARACTERS rather than bytes, and each of those characters an ASCII alphanumeric or
/// one of [`ADMITTED_SPECIALS`]. A name beginning with a period — which the sentence names
/// in its own right — is refused by that same character clause, since the period is not one
/// of the admitted characters anywhere in the name.
///
/// A create that names NO short name is not refused here: an absent `ShortName` is a member
/// the caller left to the service, which derives one from the directory's own DNS name.
///
/// LINEAR in the submitted value, which the request body bounds, and it allocates nothing:
/// the length is counted and each character decided in one pass each over the value the
/// caller sent.
#[must_use]
pub fn short_name_defect(short_name: Option<&str>) -> Option<String> {
    let requested = short_name?;
    let length = requested.chars().count();
    let admitted = (MIN_SHORT_NAME..=MAX_SHORT_NAME).contains(&length)
        && requested
            .chars()
            .all(admitted_in_a_short_name);
    (!admitted).then(invalid_short_name)
}

/// Whether one character may appear in a short name: an ASCII alphanumeric, or one of the
/// special characters the published rule admits.
fn admitted_in_a_short_name(character: char) -> bool {
    character.is_ascii_alphanumeric() || ADMITTED_SPECIALS.contains(&character)
}

/// The `InvalidParameterException` message a create whose `ShortName` breaks the rule is
/// refused with: the rule itself, published as Directory Service publishes it.
///
/// FIXED text — it quotes neither the name it rejected nor the member it arrived in — and it
/// opens with a SPACE, as the recorded body does. [`super::refusal`] appends the request id,
/// once. Built from the same constants the check applies, so the two cannot drift apart.
#[must_use]
pub fn invalid_short_name() -> String {
    format!(
        " Invalid directory short name. The short name must be between {MIN_SHORT_NAME} and \
         {MAX_SHORT_NAME} characters, and can consist of alphanumeric characters and some \
         special characters ({admitted}). The following special characters are not allowed: \
         {refused}. The name cannot begin with a period.",
        admitted = listed(&ADMITTED_SPECIALS, "or"),
        refused = listed(&REFUSED_SPECIALS, "and"),
    )
}

/// `characters` as the published sentence lists them: each one spaced from the next, with
/// `closing` before the last.
fn listed(characters: &[char], closing: &str) -> String {
    let Some((last, leading)) = characters.split_last() else {
        return String::new();
    };
    let leading: Vec<String> = leading.iter().map(char::to_string).collect();
    format!("{} {closing} {last}", leading.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the sentence a refusal publishes is the rule the check applies — every
    /// character the check admits is in the sentence's admitted list, and both bounds it
    /// enforces are the ones the sentence states.
    #[test]
    fn the_published_sentence_states_the_rule_the_check_applies() {
        let published = invalid_short_name();
        assert!(
            published.starts_with(' '),
            "the published sentence opens with a space, got {published:?}"
        );
        assert!(
            published.contains(&format!(
                "must be between {MIN_SHORT_NAME} and {MAX_SHORT_NAME} characters"
            )),
            "expected the enforced bounds in the sentence, got {published:?}"
        );
        for admitted in ADMITTED_SPECIALS {
            assert!(
                admitted_in_a_short_name(admitted),
                "case {admitted:?}: expected the published admitted character to be admitted"
            );
        }
        for refused in REFUSED_SPECIALS {
            assert_eq!(
                admitted_in_a_short_name(refused),
                ADMITTED_SPECIALS.contains(&refused),
                "case {refused:?}: expected the published refused character to be admitted \
                 only where the admitted list carries it"
            );
        }
    }
}
