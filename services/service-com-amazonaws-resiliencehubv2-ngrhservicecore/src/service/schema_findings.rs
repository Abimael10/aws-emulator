// SPDX-License-Identifier: CC0-1.0

//! How this service states a request value one of the rules its request schema declares
//! refused, before any handler ran.
//!
//! # The rule
//!
//! This service validates a request against the operation's modelled input as a SCHEMA and
//! reports what that validation found as the validator's own REPORT — a bracketed,
//! comma-separated list of findings ([`message::bracketed_findings`]) — rather than as the
//! AWS-wide `N validation error(s) detected: Value '…' at '…' failed to satisfy
//! constraint: …` sentence. Each finding is worded for the CLASS of rule the value broke
//! ([`message::schema_finding`]): a string outside its length bounds quotes itself, its
//! length and the bound; a number outside its range states the bound and the value; a value
//! outside a regular expression or an enumerated set states the expression or the set. The
//! bound, the expression and the value set come from the MODEL and the value from the
//! request, so the report is built from the two things a caller can act on.
//!
//! One finding is about the VALIDATOR rather than about a value: a body property whose
//! declared type carries a JSON Schema `format` attribute the validator implements no check
//! for is reported with that attribute ([`message::format_attribute_not_supported`]) beside
//! the finding about its value, so a refusal of a numeric property reads as the two findings
//! its schema produced. Which attribute a property owes follows the MODEL — the type the
//! member is declared with ([`NumericFormat`]) — rather than a list of member names.
//!
//! The status (400) and the code (`ValidationException`, which this service's model declares
//! for every one of its operations) stay the framework's: only the words are this layer's,
//! so a client branching on the code reads the same code as before.
//!
//! Every sentence in the list, and the dispatch from a violation class to the sentence that
//! states it, is the SHARED one that layer uses
//! ([`::service_common::validation_message`]), so this service cannot word a finding
//! differently from another service handed the same violation. What this module owns is what
//! is this service's: which `format` attribute each of its members owes, and the envelope
//! the findings are listed in.
//!
//! # What this does NOT word
//!
//! A refusal a HANDLER words — an ARN naming nothing stored, a name a live resource already
//! holds, a `clientToken` offered for a second, different create — is not a schema finding:
//! the request satisfied every rule the schema declares and was refused on this service's
//! own state.
//!
//! A union body that sets other than exactly one of its alternatives is refused while it is
//! READ, before the validating walk runs, so its finding is stated where that body is
//! decoded (`crate::ops::types::create_input_source_types`) over the same shared sentence.
//!
//! ALL OR NOTHING. A refusal carrying one violation this layer states no finding for keeps
//! the framework's whole envelope rather than being reported half in one voice and half in
//! the other, which is why [`constraint_report`] answers `None` for it. A `@required`
//! member the request left out is such a violation: a JSON Schema states the properties an
//! object is missing as one finding naming them as the SCHEMA spells them, which the refusal
//! seam — handed the violations alone, without the operation whose input schema spells them
//! — cannot build.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use aws_smithy_schema::{Schema, ShapeType};
use aws_smithy_schema_server::constraints::path;
use service_common::validation::SubmittedEcho;
use service_common::validation_message as message;
use smithy_server_runtime::Violation;

/// The JSON Schema `format` attribute a numeric property is declared with, and where the
/// finding about it goes in the report.
///
/// The attribute follows the type the MODEL declares the member with, the way a JSON Schema
/// derived from that model declares it: a 32-bit integer property is an `int32`, a
/// double-precision one a `double`. Its POSITION follows the order the published schema
/// states that property's keywords in, which the two recorded reports of this service show
/// differing by class — a fractional property's `format` is stated BEFORE the finding about
/// its value, an integral property's AFTER it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NumericFormat {
    /// The attribute, as the schema spells it.
    attribute: &'static str,
    /// Whether the finding about it comes before the finding about the value.
    leading: bool,
}

/// The attribute a double-precision member owes, stated ahead of its value's finding.
const DOUBLE: NumericFormat = NumericFormat {
    attribute: "double",
    leading: true,
};

/// The attribute a single-precision member owes. No report of one is recorded; it takes the
/// position of the other fractional class.
const FLOAT: NumericFormat = NumericFormat {
    attribute: "float",
    leading: true,
};

/// The attribute a 32-bit integer member owes, stated after its value's finding.
const INT32: NumericFormat = NumericFormat {
    attribute: "int32",
    leading: false,
};

/// The attribute a 64-bit integer member owes. No report of one is recorded; it takes the
/// position of the other integral class.
const INT64: NumericFormat = NumericFormat {
    attribute: "int64",
    leading: false,
};

/// The shapes whose members a request of this service can carry in its BODY and which
/// declare a numeric member, each with the path prefix the validating walk reports that
/// shape's members under.
///
/// Every one of them is a target block of a resilience policy, bound at the member named
/// here by `CreatePolicy`, `UpdatePolicy` and `ImportPolicy` alike
/// (`smithy_query(query="operation", operation="CreatePolicy")`), so one prefix each places
/// every numeric body member a request can carry. Codegen emits a member schema WITHOUT its
/// target's own members, so a nested shape is named by its own schema under the member path
/// that binds it.
///
/// A value bound to the request URI (`@httpQuery maxResults`) is NOT here: the `format`
/// findings report on the schema of the request BODY, and a query parameter is no property
/// of it.
fn numeric_body_shapes() -> Vec<(&'static str, &'static Schema)> {
    vec![
        ("availabilitySlo", crate::ops::types::AVAILABILITY_SLO_SCHEMA),
        ("multiAz", crate::ops::types::MULTI_AZ_TARGETS_SCHEMA),
        ("multiRegion", crate::ops::types::MULTI_REGION_TARGETS_SCHEMA),
        (
            "dataRecovery",
            crate::ops::types::DATA_RECOVERY_TARGETS_SCHEMA,
        ),
    ]
}

/// The `format` attribute each numeric body member owes, by the validation path the walk
/// names that member at — read off the MODEL, so a member it adds to one of those shapes
/// appears here without a second list to maintain.
static NUMERIC_BODY_FORMATS: LazyLock<BTreeMap<String, NumericFormat>> =
    LazyLock::new(|| numeric_formats(&numeric_body_shapes()));

/// The numeric members of `shapes`, each with the `format` attribute its declared type
/// carries, keyed by the validation path the walk names it at.
fn numeric_formats(shapes: &[(&str, &'static Schema)]) -> BTreeMap<String, NumericFormat> {
    let mut formats = BTreeMap::new();
    for (prefix, shape) in shapes {
        for member in shape.members() {
            let Some(name) = member.member_name() else {
                continue;
            };
            let Some(format) = numeric_format(member.shape_type()) else {
                continue;
            };
            // The casing of a validation path segment belongs to the path, not to the
            // model, so it is spelled by the framework module that owns that spelling.
            let segment = path::member(name);
            let full = if prefix.is_empty() {
                segment
            } else {
                format!("{prefix}.{segment}")
            };
            formats.insert(full, format);
        }
    }
    formats
}

/// The `format` attribute a member declared with `shape_type` carries, or `None` for a type
/// that carries none.
fn numeric_format(shape_type: ShapeType) -> Option<NumericFormat> {
    match shape_type {
        ShapeType::Double => Some(DOUBLE),
        ShapeType::Float => Some(FLOAT),
        // A Smithy `byte`, `short` and `integer` are all 32-bit integers in a JSON Schema's
        // vocabulary, which has one integral format per width.
        ShapeType::Byte | ShapeType::Short | ShapeType::Integer => Some(INT32),
        ShapeType::Long => Some(INT64),
        _ => None,
    }
}

/// The whole report `violations` is stated as, or `None` to leave the refusal to the
/// framework's own envelope.
///
/// The findings are listed in the order the validating walk found the violations, each with
/// the `format` finding its member's declared type owes beside it ([`unchecked_format`]).
pub(super) fn constraint_report(violations: &[Violation]) -> Option<String> {
    if violations.is_empty() {
        return None;
    }
    let mut findings: Vec<String> = Vec::with_capacity(violations.len());
    for violation in violations {
        let format = unchecked_format(violation);
        if let Some(leading) = format.filter(|format| format.leading) {
            findings.push(message::format_attribute_not_supported(leading.attribute));
        }
        // The ONE dispatch from a violation class to the sentence this layer states about
        // it, so nothing here can word a finding differently from another service handed
        // the same violation — and a class that dispatch states nothing for leaves the
        // whole refusal to the framework's envelope.
        findings.push(message::schema_finding(violation, &submitted(violation))?);
        if let Some(trailing) = format.filter(|format| !format.leading) {
            findings.push(message::format_attribute_not_supported(trailing.attribute));
        }
    }
    Some(message::bracketed_findings(&findings))
}

/// How a finding can state the value `violation` is about: the rendering the framework
/// stored for it, or nothing at all for a member that was absent.
fn submitted(violation: &Violation) -> SubmittedEcho {
    match violation.value.as_deref() {
        Some(value) => SubmittedEcho::Rendered(value.to_string()),
        None => SubmittedEcho::Absent,
    }
}

/// The `format` the schema of the member `violation` is about declares and the validator
/// implements no check for, or `None` for a member whose type carries none.
fn unchecked_format(violation: &Violation) -> Option<NumericFormat> {
    NUMERIC_BODY_FORMATS.get(&violation.path).copied()
}

#[cfg(test)]
mod tests {
    //! The two halves of the report no request can drive on its own: which members owe a
    //! `format` finding, and that a class this layer states no finding for declines the
    //! whole report. Every class a request CAN reach is pinned over the router in
    //! `tests/all/constraint_report_wording.rs`.

    use super::*;
    use smithy_server_runtime::ViolationKind;

    /// A violation of `kind` at `path`, carrying the value the walk stored for it.
    fn violation(path: &str, kind: ViolationKind, value: Option<&str>) -> Violation {
        Violation {
            path: path.to_string(),
            kind,
            value: value.map(str::to_string),
            sensitive: false,
        }
    }

    /// RULE: the `format` attribute a member owes follows the TYPE this model declares it
    /// with, by the path the validating walk names it at — and a member whose type carries
    /// none, or one the request carries outside its body, owes none.
    #[test]
    fn the_format_attribute_follows_the_members_declared_type() {
        // (case, the violated member's path, the format it owes)
        let rows: [(&str, &str, Option<NumericFormat>); 7] = [
            (
                "the modelled double member",
                "availabilitySlo.target",
                Some(DOUBLE),
            ),
            ("a modelled integer member", "multiAz.rtoInMinutes", Some(INT32)),
            (
                "a second modelled integer member",
                "multiRegion.rpoInMinutes",
                Some(INT32),
            ),
            (
                "a third modelled integer member",
                "dataRecovery.timeBetweenBackupsInMinutes",
                Some(INT32),
            ),
            ("a string member", "name", None),
            ("a query parameter", "maxResults", None),
            (
                "a member whose last segment matches but whose path does not",
                "target",
                None,
            ),
        ];
        for (case, path, expected) in rows {
            let found = unchecked_format(&violation(path, ViolationKind::UniqueItems, None));
            assert_eq!(
                found, expected,
                "case {case:?}: expected {expected:?}, got {found:?}"
            );
        }
    }

    /// RULE: a refusal this layer has no finding for is not reported half in its voice —
    /// neither an empty violation set, nor an absent `@required` member, nor a violation
    /// whose value the walk withheld.
    #[test]
    fn what_this_layer_has_no_finding_for_is_left_to_the_framework() {
        let withheld = Violation {
            sensitive: true,
            ..violation("name", ViolationKind::MinLength(2), None)
        };
        // (case, the violations of one request)
        let rows: [(&str, Vec<Violation>); 3] = [
            ("a request that broke no rule", Vec::new()),
            (
                "a required member the request left out",
                vec![violation("name", ViolationKind::MissingRequiredMember, None)],
            ),
            ("a violation whose value is withheld", vec![withheld]),
        ];
        for (case, violations) in rows {
            let report = constraint_report(&violations);
            assert!(
                report.is_none(),
                "case {case:?}: expected no report, got {report:?}"
            );
        }
    }

    /// RULE: a numeric member's report holds both findings in the one bracketed list, with
    /// a fractional member's `format` finding ahead of the finding about its value and an
    /// integral member's after it.
    #[test]
    fn a_numeric_member_is_reported_as_its_two_findings() {
        // (case, the violation, the whole report)
        let rows: [(&str, Violation, &str); 2] = [
            (
                "a double above its range",
                violation(
                    "availabilitySlo.target",
                    ViolationKind::MaxValue("100".to_string()),
                    Some("100.5"),
                ),
                "[format attribute \"double\" not supported, numeric instance is greater \
                 than the required maximum (maximum: 100, found: 100.5)]",
            ),
            (
                "an integer below its range",
                violation(
                    "multiAz.rtoInMinutes",
                    ViolationKind::MinValue("0".to_string()),
                    Some("-1"),
                ),
                "[numeric instance is lower than the required minimum (minimum: 0, found: \
                 -1), format attribute \"int32\" not supported]",
            ),
        ];
        for (case, violation, expected) in rows {
            let report = constraint_report(&[violation])
                .unwrap_or_else(|| panic!("case {case:?}: must be reported"));
            assert_eq!(
                report, expected,
                "case {case:?}: expected {expected:?}, got {report:?}"
            );
        }
    }
}
