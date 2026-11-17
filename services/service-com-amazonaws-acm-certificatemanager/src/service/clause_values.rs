// SPDX-License-Identifier: CC0-1.0

//! Which POSITIONS a constraint clause QUOTES the submitted value at, and which ones it
//! states without it.
//!
//! A constraint clause reports the value that broke the constraint as the request carried
//! it — `Value '<the submitted text>' at '<member path>' failed to satisfy constraint:
//! <requirement>`. That quoted text is the only report a caller gets of WHICH of its
//! values was rejected: the code is one and the same for every input-constraint refusal
//! ([`super::VALIDATION_EXCEPTION`]), so a client that echoes the offending value back, or
//! corrects the parameter and resends, has nothing else to read it from. An empty value is
//! quoted as the empty text (`Value '' at …`), which is what distinguishes a member sent
//! empty from one that was never sent at all ([`super::ABSENT_SUBJECT`]).
//!
//! Two kinds of position state the requirement WITHOUT a quoted value
//! ([`super::SUBJECT`]).
//!
//! The first is the positions whose text this service does not reflect back, each of them
//! a class rather than one member:
//!
//! * a member that NAMES A RESOURCE — an ARN, whose text carries the account the caller
//!   addressed as well as the id it asked about;
//! * a REQUEST TOKEN member, which a caller mints rather than composes;
//! * the TAG surface's collections, keys and values, which are the caller's own labels for
//!   its resources rather than parameters of the operation;
//! * a VALUE SET violation, whose requirement already lists every value the member admits,
//!   so the one that arrived adds nothing to it.
//!
//! The second is a position the validating walk recorded no submitted TEXT for, because
//! what it keeps there is a bounded rendering of its own rather than the caller's bytes: a
//! collection, rendered as its element count, a blob, rendered as its byte count, a
//! document, rendered as the word `<document>`, and a value past the walk's echo ceiling,
//! kept as a prefix plus the count of what was dropped
//! (`vendor/smithy-schema-server/src/constraints.rs`). None of those is anything a caller
//! sent, so none of them may be quoted as one.
//!
//! The readings of those renderings are the tree's shared ones — the collection summaries
//! and the blob's byte count
//! ([`::service_common::validation_message::list_item_count`] and its siblings) and the
//! elision marker ([`::service_common::validation::named_byte_count`]) — so what counts as
//! the walk's own rendering here is what counts as one everywhere. A value past that ceiling
//! is the one position where this seam cannot state what the caller sent: `override_refusal`
//! is handed the violations alone, and the whole value is restorable only from the request
//! ([`::service_common::quoted_refusals`] wraps an operation to do it), which no recording of
//! this service's refusals needs today — every member whose text a clause quotes is bounded
//! by its own `@length` maximum far below that ceiling.

use super::ViolationKind;

/// How the validating walk renders a document member it length-checked: the word, not the
/// document.
const DOCUMENT_RENDERING: &str = "<document>";

/// The tag collections this service's inputs bind, by the name a member path spells them
/// with: a tag list and a tag key list. A qualified member ending in one of these names
/// binds the same collection ([`names_collection`]).
const TAG_COLLECTIONS: [&str; 2] = ["tags", "tagKeys"];

/// Whether a clause about the position `path` quotes `stored` — the text the validating
/// walk recorded for it — or states the requirement alone.
///
/// `kind` is the constraint that was broken: a value set states which values the member
/// admits and never which one arrived.
pub(super) fn quotes_submitted_text(path: &str, kind: &ViolationKind, stored: &str) -> bool {
    !withheld_position(path, kind) && is_submitted_text(stored)
}

/// Whether `path` is one of the positions this service states without the text submitted
/// there (see the module docs for the classes and why each withholds).
fn withheld_position(path: &str, kind: &ViolationKind) -> bool {
    names_a_resource(path)
        || names_a_request_token(path)
        || names_tag_content(path)
        || matches!(kind, ViolationKind::EnumValue(_))
}

/// The member a clause is about: the LAST step of a member path, since the steps before it
/// name the structures, unions and collections it was reached through.
///
/// A clause about a collection's own ELEMENT never reaches here: it is reported against the
/// collection member and stated without the element's text (`super::constraint_clause`), so a
/// member of any class held as a bare element is withheld before this is asked.
fn member(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or_default()
}

/// Whether the member at `path` names a resource by ARN.
///
/// Read off the member's own name, which a member path spells in lower camel
/// (`resourceArn`, `certificateArn`, `acmeEndpointArn`, `certificateAuthorityArn`): a
/// violation carries the member path and the constraint that was broken, not the shape that
/// declared it, and an ARN member's violated constraint is not always the ARN `@pattern` —
/// a value short of the shape's `@length` minimum breaks that bound alone.
fn names_a_resource(path: &str) -> bool {
    let member = member(path);
    member == "arn" || member.ends_with("Arn")
}

/// Whether the member at `path` carries a token minted for one request — an idempotency
/// token, a pagination token — which the same naming convention names.
fn names_a_request_token(path: &str) -> bool {
    member(path).ends_with("Token")
}

/// Whether `path` is reached through the tag surface — a tag collection, or a key or value
/// inside one — at whatever depth the operation binds it.
fn names_tag_content(path: &str) -> bool {
    path.split('.').any(|step| {
        TAG_COLLECTIONS
            .iter()
            .any(|collection| names_collection(step, collection))
    })
}

/// Whether the member name `step` names `collection` — that name itself, or a QUALIFIED
/// name ending in it (`certificateTags` names `tags`).
fn names_collection(step: &str, collection: &str) -> bool {
    if step == collection {
        return true;
    }
    let mut characters = collection.chars();
    let qualified_suffix = match characters.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + characters.as_str(),
        None => return false,
    };
    step.len() > qualified_suffix.len() && step.ends_with(&qualified_suffix)
}

/// Whether `stored` is TEXT THE CALLER SENT, rather than one of the bounded renderings the
/// validating walk keeps in place of a value it has none for (see the module docs).
fn is_submitted_text(stored: &str) -> bool {
    ::service_common::validation_message::list_item_count(stored).is_none()
        && ::service_common::validation_message::map_entry_count(stored).is_none()
        && ::service_common::validation_message::blob_byte_count(stored).is_none()
        && stored != DOCUMENT_RENDERING
        // The walk's ELISION of a longer value: a prefix plus the marker naming what was
        // dropped, read through the tree's one reading of that marker — which is also what
        // keeps a caller's own string that merely ends that way from being read as one.
        && ::service_common::validation::named_byte_count(stored).is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a clause quotes the text the request carried at the position it names, except
    /// at the positions this service withholds and where the walk kept no submitted text.
    ///
    /// The rows span the withheld classes (an ARN member at two depths, a token member, a
    /// tag key list, a tag value inside a list element, a qualified tag member, a value
    /// set), the walk's own renderings (a list, a map, a blob, a document, an elision) and
    /// the positions that quote (a top-level string, a string under a union member, a
    /// number, a member of a list element, the empty string, and a string that merely ENDS
    /// like one of the walk's renderings).
    #[test]
    fn a_position_quotes_the_submitted_text_unless_it_is_withheld() {
        let pattern = ViolationKind::Pattern("^Z[A-Z0-9]+$".to_string());
        let min_length = ViolationKind::MinLength(1);
        let max_length = ViolationKind::MaxLength(32);
        let range = ViolationKind::MinValue("1".to_string());
        let value_set = ViolationKind::EnumValue(vec!["ISSUED".to_string()]);
        let cases: Vec<(&str, &str, &ViolationKind, &str, bool)> = vec![
            ("a top-level string member", "domainName", &pattern, "nope_1", true),
            (
                "a string under a union member",
                "prevalidationOptions.dnsPrevalidation.hostedZoneId",
                &min_length,
                "",
                true,
            ),
            ("a number", "maxItems", &range, "0", true),
            (
                "a string member of a list element",
                "domainValidationOptions.1.member.domainName",
                &pattern,
                "nope_2",
                true,
            ),
            ("a top-level ARN member", "resourceArn", &pattern, "not-an-arn", false),
            (
                "an ARN member reached through a structure",
                "options.certificateAuthorityArn",
                &min_length,
                "arn:aws:acm:x",
                false,
            ),
            ("a request token member", "idempotencyToken", &pattern, "has-dashes", false),
            ("a tag key list", "tagKeys", &max_length, "{nope}", false),
            ("a tag value inside a list element", "tags.1.member.value", &pattern, "x", false),
            (
                "a value of a qualified tag member",
                "certificateTags.1.member.value",
                &pattern,
                "x",
                false,
            ),
            (
                "a member whose name merely contains a tag collection's",
                "notTagsAtAll",
                &pattern,
                "x",
                true,
            ),
            ("a value set", "certificateType", &value_set, "NOT_A_TYPE", false),
            ("a list, rendered as its element count", "tagKeys", &max_length, "51 items", false),
            ("a map, rendered as its entry count", "expiryEvents", &max_length, "7 entries", false),
            ("a blob, rendered as its byte count", "certificate", &max_length, "4096 bytes", false),
            // A caller's own string that reads like that summary but states a count no
            // longer than itself is still the caller's text.
            (
                "a string that merely ends like a byte-count summary",
                "domainName",
                &pattern,
                "5 bytes",
                true,
            ),
            ("a document, rendered as a word", "policy", &max_length, "<document>", false),
            (
                "a value past the walk's echo ceiling",
                "domainName",
                &max_length,
                "aaaa... [1012 bytes]",
                false,
            ),
            // A caller's own string that ends like one of those renderings but states a
            // count no longer than itself is still the caller's text.
            (
                "a string that merely ends like an elision",
                "domainName",
                &pattern,
                "a-very-long-prefix-here... [3 bytes]",
                true,
            ),
        ];

        for (what, path, kind, stored, expected) in cases {
            let actual = quotes_submitted_text(path, kind, stored);
            assert_eq!(
                actual, expected,
                "case {what:?} ({path} = {stored:?}): expected quotes_submitted_text == \
                 {expected}, got {actual}"
            );
        }
    }
}
