// SPDX-License-Identifier: CC0-1.0

//! The ONE spelling of a request member as a JSON pointer, for every service whose
//! refusal addresses a failing input that way.
//!
//! A service that reports which member of a request it refused has two spellings to
//! choose between, and they are not interchangeable: the framework's constraint walk
//! reports a DOTTED path (`vectors.1.member.data`), while several AWS services address
//! the same member as the RFC 6901 JSON pointer into the request body
//! (`/vectors/0/data`). The translation between them is not a matter of taste — a
//! collection step, an element's base and the two characters a pointer segment escapes
//! all have one right answer — so it is stated here once rather than per service.
//!
//! This module judges only the SPELLING of a member path. Which violations a refusal
//! states, and in what words, is the service's own
//! (see [`crate::validation`] for the collecting half). What a collection step is
//! spelled by does depend on one fact the dotted path does not carry — whether the
//! collection is a list or a map — so a service that has maps a caller keys supplies
//! that fact through [`json_pointer_keyed`]; the spelling it is turned into stays here.
//!
//! Services still carrying their own copy: this rule predates this module and is inlined
//! in the service crates that needed it before there was anywhere shared to put it —
//! `grep -rln 'fn json_pointer' services/*/src` lists them. Those copies are to be
//! MIGRATED onto this module; they are left in place only because this change does not
//! own those services' issues, and each is covered by that service's own suite. Nothing
//! new should inline the rule again — call this module.

/// The segment the framework's dotted member path puts between a collection and one of
/// its elements (`vectors.1.member.data`). A JSON pointer names the element itself, so
/// this step has no counterpart in one.
const COLLECTION_STEP: &str = "member";

/// One member of the request as a JSON pointer rooted at the request body, built from
/// the dotted path the framework walks its constraints with
/// (`aws_smithy_schema_server::constraints::path`), for a request surface whose every
/// collection step is a step into a LIST.
///
/// [`json_pointer_keyed`] states the whole rule and this is that rule with no map
/// declared, which is the right answer for a surface that has no map a caller keys, and
/// for one whose maps hold SCALAR values: the walk spells a scalar map value's path with
/// the key alone, so no collection step arrives for it. A surface with a map whose values
/// are structured — a structure, a union or another collection — reaches a step this
/// cannot tell from a list position and must call [`json_pointer_keyed`] instead.
#[must_use]
pub fn json_pointer(dotted: &str) -> String {
    json_pointer_keyed(dotted, |_| false)
}

/// One member of the request as a JSON pointer rooted at the request body, with each
/// step into a collection spelled by what the MODEL declares that collection to be.
///
/// Each dotted segment becomes one pointer segment. A step from a collection into one of
/// its elements arrives as `<position-or-key>.member` whichever kind of collection it
/// was, and a pointer spells it as the element alone — but what names that element
/// differs: an element of a list is renumbered from the framework's 1-based count to the
/// 0-based index RFC 6901 gives an array element, while a map entry's key — a value the
/// caller chose rather than a model name — is kept exactly as sent, with the two
/// characters a pointer segment escapes (`~` and `/`) escaped. The dotted form carries
/// nothing that tells the two apart, so `declares_map` is asked about the member one step
/// up (the segment before the element, which is the collection being stepped into) and
/// answers whether the model declares that member a MAP. A key that reads like a position
/// is therefore still the caller's key, and the pointer resolves against the body the
/// caller sent either way.
#[must_use]
pub fn json_pointer_keyed(dotted: &str, declares_map: impl Fn(&str) -> bool) -> String {
    let mut pointer = String::with_capacity(dotted.len() + 1);
    let mut segments = dotted.split('.').peekable();
    // The member one step up: the collection a `<position-or-key>.member` step steps
    // into is the member the segment before it names.
    let mut enclosing = "";
    while let Some(segment) = segments.next() {
        pointer.push('/');
        if segments.peek() == Some(&COLLECTION_STEP) {
            // The step into the element is spelled by the element alone, so the
            // `member` segment that followed it is consumed here.
            segments.next();
            match segment.parse::<usize>() {
                Ok(position) if !declares_map(enclosing) => {
                    pointer.push_str(&position.saturating_sub(1).to_string());
                }
                _ => pointer.push_str(&escaped_segment(segment)),
            }
        } else {
            pointer.push_str(&escaped_segment(segment));
        }
        enclosing = segment;
    }
    pointer
}

/// One pointer segment with the two characters a JSON pointer gives a meaning of its own
/// escaped: `~` as `~0` and `/` as `~1` (RFC 6901), so a caller-chosen key holding either
/// still names one member instead of splitting into two.
///
/// `pub` because a few services SPELL a value the way a pointer segment is spelled — a map
/// key a service documents as a reference token of a method path, for one — and the escape
/// is the same rule there: one spelling of it, called rather than inlined again.
#[must_use]
pub fn escaped_segment(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

/// The text one pointer segment stands for: [`escaped_segment`] read back, the separator's
/// escape resolved BEFORE the tilde's (RFC 6901), so a segment that spells a literal `~1`
/// reads back as that text rather than as a separator.
///
/// `pub` for the reason [`escaped_segment`] is: a service that spells a value the way a
/// pointer segment is spelled also has to READ one a caller spelled that way — the resource
/// path of a method-setting declaration, for one — and that is the same rule, stated once.
#[must_use]
pub fn decoded_segment(segment: &str) -> String {
    segment.replace("~1", "/").replace("~0", "~")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dotted walk path becomes the pointer that names the same member, at every depth
    /// and through every kind of step.
    #[test]
    fn a_dotted_path_becomes_the_pointer_naming_the_same_member() {
        // The rows differ along what the rule generalises over: the depth, the kind of
        // step (struct member, list element, map entry), the element's position, and the
        // characters a caller-chosen key can carry.
        let rows: [(&str, &str, &str); 10] = [
            ("a top-level member", "dimension", "/dimension"),
            (
                "a nested member",
                "encryptionConfiguration.sseType",
                "/encryptionConfiguration/sseType",
            ),
            ("the first element of a list", "vectors.1.member", "/vectors/0"),
            (
                "a member of a later list element",
                "vectors.3.member.data",
                "/vectors/2/data",
            ),
            ("a map entry", "tags.team.member", "/tags/team"),
            ("a list of lists", "outer.2.member.1.member", "/outer/1/0"),
            (
                "a map key carrying a slash",
                "tags.team/web.member",
                "/tags/team~1web",
            ),
            (
                "a map key carrying a tilde",
                "tags.a~b.member",
                "/tags/a~0b",
            ),
            (
                "a map key carrying both, in order",
                "tags.~/.member",
                "/tags/~0~1",
            ),
            (
                "a collection step the walk reports with no position before it",
                "member",
                "/member",
            ),
        ];

        for (case, dotted, expected) in rows {
            let actual = json_pointer(dotted);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// A step into a collection the caller KEYS keeps that key, however it reads, while a
    /// step into a list is still renumbered — the one fact the dotted path does not carry,
    /// supplied by the caller of [`json_pointer_keyed`].
    #[test]
    fn a_step_into_a_declared_map_keeps_the_key_the_caller_sent() {
        // `attributes` and `context` are the maps in these rows; every other member is a
        // list, so each row also pins that a list position is unaffected by the answer.
        let declares_map = |member: &str| matches!(member, "attributes" | "context");

        // The rows differ along how the key READS (a year, the position a first element
        // renumbers to, zero, digits with a leading zero, a word, a key carrying the two
        // characters a segment escapes), along which kind of collection the step enters,
        // and along what the step is nested in — so the rows that keep a key and the rows
        // that renumber one are the same shape.
        let rows: [(&str, &str, &str); 11] = [
            (
                "an all-digit key on a map",
                "attributes.2024.member.ipaddr",
                "/attributes/2024/ipaddr",
            ),
            (
                "the key a 1-based count would renumber to the first element",
                "attributes.1.member.ipaddr",
                "/attributes/1/ipaddr",
            ),
            ("the key zero", "attributes.0.member.ipaddr", "/attributes/0/ipaddr"),
            (
                "digits with a leading zero",
                "attributes.007.member.ipaddr",
                "/attributes/007/ipaddr",
            ),
            (
                "a word key",
                "attributes.team.member.ipaddr",
                "/attributes/team/ipaddr",
            ),
            (
                "a key carrying both characters a segment escapes",
                "attributes.a/b~c.member.ipaddr",
                "/attributes/a~1b~0c/ipaddr",
            ),
            (
                "an entry whose value is what failed",
                "attributes.2024.member",
                "/attributes/2024",
            ),
            (
                "a list inside a map entry",
                "attributes.2024.member.set.2.member",
                "/attributes/2024/set/1",
            ),
            (
                "a map inside a list element",
                "entities.2.member.attributes.2024.member.ipaddr",
                "/entities/1/attributes/2024/ipaddr",
            ),
            (
                "a map inside a map entry",
                "context.42.member.attributes.7.member",
                "/context/42/attributes/7",
            ),
            (
                "a plain list element, which no declared map covers",
                "requests.3.member.principal.entityId",
                "/requests/2/principal/entityId",
            ),
        ];

        for (case, dotted, expected) in rows {
            let actual = json_pointer_keyed(dotted, declares_map);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a segment spells the text it was given — the two characters a pointer reserves
    /// escaped — and reading that segment back answers that same text.
    #[test]
    fn a_segment_spells_its_text_and_reads_back_as_it() {
        // The rows differ along which reserved characters the text carries, where they sit,
        // and whether the text already READS like an escape.
        let rows: [(&str, &str, &str); 8] = [
            ("a plain name", "pets", "pets"),
            ("a path name", "/pets/{id}", "~1pets~1{id}"),
            ("a name carrying a tilde", "a~b", "a~0b"),
            ("the separator escape as literal text", "~1", "~01"),
            ("the tilde escape as literal text", "~0", "~00"),
            ("both reserved characters", "~/", "~0~1"),
            ("nothing at all", "", ""),
            ("a verb", "GET", "GET"),
        ];

        for (case, text, expected) in rows {
            let escaped = escaped_segment(text);
            assert_eq!(
                escaped, expected,
                "case {case:?}: expected the segment {expected:?}, got {escaped:?}"
            );
            let read = decoded_segment(&escaped);
            assert_eq!(
                read, text,
                "case {case:?}: expected the segment to read back as {text:?}, got {read:?}"
            );
        }
    }
}
