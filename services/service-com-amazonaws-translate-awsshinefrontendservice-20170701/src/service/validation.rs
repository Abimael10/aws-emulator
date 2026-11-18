// SPDX-License-Identifier: CC0-1.0

//! The WORDS of this service's input-validation refusal: how the constraint violations the
//! framework's own boundary walk found become the `message` a 400 `ValidationException`
//! carries.
//!
//! [`crate::service`] owns the SEAM — which refusals this service words at all, and the two
//! classes that are a modelled size refusal of one operation rather than a constraint
//! failure — and this module owns the sentence, so the clause rules hold for every member of
//! every operation instead of for the members one report happened to cite. What is ENFORCED
//! is always the model's: every violation handed to [`refusal`] comes from the framework's
//! own walk, and nothing here accepts or refuses anything.
//!
//! The rules, each over every operation of this service:
//!
//! 1. the envelope is the `<N> validation error(s) detected: ` frame over one clause per
//!    violation, and a clause states the value, the member's dotted path and the requirement
//!    it failed — all of it from the shared renderer
//!    (`service_common::validation_message::aws_validation_refusal`), so no requirement and
//!    no member path is spelled in this crate;
//! 2. a reported `@pattern` is the expression WITHOUT the `^`/`$` pair a model writes around
//!    it to turn a find into a whole-value match (`service_common::validation::unanchored`),
//!    which is the expression this API publishes for the member; the CHECK still uses the
//!    model's pattern exactly as written;
//! 3. an enumerated type's accepted-value set is listed as the front door's own hash table
//!    over those values is walked
//!    (`service_common::validation_message::in_hashed_enum_order`), which is neither the
//!    model's declaration order nor lexicographic. The order follows from the values
//!    themselves, so it belongs to the enumerated TYPE: every member targeting one type is
//!    listed alike, wherever and however deeply it is carried, and a type the model gains
//!    needs no row here.
//!
//! The status (400) and the error code are what the framework already answers a constraint
//! failure with on this `awsJson1_1` service; the shared renderer restates them so the code
//! always travels with the message it belongs to.

use ::smithy_server_runtime::{Refusal, Refused, Violation};

/// This service's wording for the violations the framework's walk found, or `None` to keep
/// the framework's own — which is the answer when there is nothing to word.
pub(super) fn refusal(violations: &[Violation]) -> Option<Refused> {
    // The value set of an enumerated type is re-ordered BEFORE the clauses are rendered, so
    // the clause itself — the member path, the echoed value and the requirement's wording —
    // still comes from the one place that writes it.
    let stated: Vec<Violation> = violations
        .iter()
        .map(::service_common::validation_message::in_hashed_enum_order)
        .collect();
    ::service_common::validation_message::aws_validation_refusal(&Refusal::Invalid(stated))
}

#[cfg(test)]
mod tests {
    //! The two renderings this module chooses, each over a SET of inputs rather than one
    //! recorded case. The same rules are pinned on the wire, through real requests, in
    //! `tests/all/rejection_paths.rs`.

    use super::*;
    use ::aws_smithy_schema_server::constraints::ViolationKind;

    /// One violation of `kind` at `path`, carrying `value` as its echo.
    fn violation(path: &str, kind: ViolationKind, value: Option<&str>) -> Violation {
        Violation {
            path: path.to_string(),
            kind,
            value: value.map(str::to_string),
            sensitive: false,
        }
    }

    /// The message [`refusal`] states for one violation.
    fn stated(one: Violation) -> String {
        refusal(&[one])
            .expect("a constraint refusal is worded")
            .message
    }

    /// A `@pattern` requirement is quoted without the whole-value pair, over this service's
    /// own expressions — including ones no report cited — and an expression carrying no such
    /// pair is quoted exactly as the model declares it.
    #[test]
    fn a_pattern_requirement_is_quoted_without_its_whole_value_anchors() {
        // (case, the pattern the model declares, the expression a clause quotes). Source:
        // `smithy_query(query="shape", shape="com.amazonaws.translate#<Shape>")` for each.
        let rows: [(&str, &str, &str); 6] = [
            (
                "S3Uri",
                r"^s3://[a-z0-9][\.\-a-z0-9]{1,61}[a-z0-9](/.*)?$",
                r"s3://[a-z0-9][\.\-a-z0-9]{1,61}[a-z0-9](/.*)?",
            ),
            ("ResourceName", r"^([A-Za-z0-9-]_?)+$", r"([A-Za-z0-9-]_?)+"),
            ("ClientTokenString", r"^[a-zA-Z0-9-]+$", r"[a-zA-Z0-9-]+"),
            ("NextToken", r"^\p{ASCII}{0,8192}$", r"\p{ASCII}{0,8192}"),
            (
                "IamRoleArn",
                r"^arn:aws(-[^:]+)?:iam::[0-9]{12}:role/.+$",
                r"arn:aws(-[^:]+)?:iam::[0-9]{12}:role/.+",
            ),
            // An expression with no whole-value pair at all is quoted as written.
            ("an unanchored expression", r"[a-z]+", r"[a-z]+"),
        ];

        for (case, modelled, quoted) in rows {
            let expected = format!(
                "1 validation error detected: Value 'nope' at 'member' failed to satisfy \
                 constraint: Member must satisfy regular expression pattern: {quoted}"
            );
            let actual = stated(violation(
                "member",
                ViolationKind::Pattern(modelled.to_string()),
                Some("nope"),
            ));
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the expression quoted as {quoted:?}, got {actual:?}"
            );
        }
    }

    /// An enumerated type's value set is listed in the order the front door's hash table
    /// over those values is walked, whatever order the model declares them in — and the
    /// order is the TYPE's, so the same set reaches the same listing from any member path.
    #[test]
    fn an_enum_value_set_is_listed_in_the_front_doors_own_order() {
        // (case, the set as the model declares it, the set as a clause lists it). Source:
        // `smithy_query(query="shape", shape="com.amazonaws.translate#<Enum>")`.
        let rows: [(&str, &[&str], &str); 5] = [
            (
                "DisplayLanguageCode",
                &["de", "en", "es", "fr", "it", "ja", "ko", "pt", "zh", "zh-TW"],
                "[de, zh-TW, pt, ko, en, it, fr, zh, es, ja]",
            ),
            ("TerminologyDataFormat", &["CSV", "TMX", "TSV"], "[CSV, TSV, TMX]"),
            // The same three values, declared in another order by another type: one set,
            // one listing.
            ("ParallelDataFormat", &["TSV", "CSV", "TMX"], "[CSV, TSV, TMX]"),
            ("Directionality", &["UNI", "MULTI"], "[UNI, MULTI]"),
            ("MergeStrategy", &["OVERWRITE"], "[OVERWRITE]"),
        ];

        for (case, declared, listed) in rows {
            let declared: Vec<String> = declared.iter().map(|value| (*value).to_string()).collect();
            for path in ["member", "config.1.member.nested"] {
                let expected = format!(
                    "1 validation error detected: Value 'nope' at '{path}' failed to satisfy \
                     constraint: Member must satisfy enum value set: {listed}"
                );
                let actual = stated(violation(
                    path,
                    ViolationKind::EnumValue(declared.clone()),
                    Some("nope"),
                ));
                assert_eq!(
                    actual, expected,
                    "case {case:?} at {path:?}: expected the set listed as {listed:?}, got \
                     {actual:?}"
                );
            }
        }
    }

    /// A refusal with no violation to word keeps the framework's own wording.
    #[test]
    fn a_refusal_with_nothing_to_word_keeps_the_frameworks_wording() {
        assert!(
            refusal(&[]).is_none(),
            "case no violations: expected the framework's own wording"
        );
    }
}
