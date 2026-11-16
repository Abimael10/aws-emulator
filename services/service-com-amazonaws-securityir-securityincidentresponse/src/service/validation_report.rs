// SPDX-License-Identifier: CC0-1.0

//! How this service states a request BODY it refused for a rule its request schema
//! declares, before any handler ran.
//!
//! # The rule
//!
//! Security Incident Response validates a request's JSON body against the operation's
//! modelled input as a SCHEMA, at a layer in front of its handlers, and reports what that
//! validation found as the validator's own REPORT rather than as the AWS-wide
//! `N validation error(s) detected: Value '…' at '…' failed to satisfy constraint: …`
//! sentence. The report is a bracketed, comma-separated list of findings
//! ([`::service_common::validation_message::bracketed_findings`]) holding, in the order the
//! operation declares its members:
//!
//! * one finding per rule the body broke, worded as that layer words the class of rule
//!   ([`::service_common::validation_message::schema_finding`]) — the value quoted where the
//!   class quotes it, taken from the request, and the bound, the expression or the
//!   admissible value set taken from the model for the member being validated;
//! * then one finding per JSON Schema `format` attribute THAT operation's request schema
//!   declares and the validator implements no check for ([`unsupported_format`]). Those
//!   report on the validator rather than on a value, so they refuse nothing on their own and
//!   are stated after the findings that do.
//!
//! What is RECORDED of that shape is one refusal of a `CreateCase` body: the bracketed list,
//! the finding for a value outside an enumerated type, and the two `format` findings
//! `password` and `double` after it. The rest follows from it: which sentence each other
//! class takes is the shared dispatch's, and the `format` findings are read off the refused
//! operation's own schema by the rule those two attributes state — a `@sensitive` string
//! property is declared as a password, a timestamp property (epoch seconds on this protocol)
//! as a double. So an operation whose schema declares neither owes neither, which no
//! recording shows yet; a recording that shows otherwise, or one that shows an attribute for
//! a type not named here, is what should correct [`unsupported_format`]. It reads the
//! PROPERTIES of the request object — the input schema's own members — and not the
//! properties of an object nested inside one, which no recorded report covers either.
//!
//! The status (400) and the code (`ValidationException`, which this service's model
//! declares for every operation) are the framework's own: only the words are this layer's,
//! and a client branching on the code reads the same code as before.
//!
//! Every sentence in the list, and the dispatch from a violation class to the sentence that
//! states it, is the SHARED one that layer uses
//! ([`::service_common::validation_message`]), so this service cannot word a finding
//! differently from another service handed the same violation. What this module owns is what
//! is this service's: the `format` findings its schemas declare, and the envelope they are
//! listed in.
//!
//! # What this does NOT word
//!
//! A value the request carries OUTSIDE its body — a `@httpLabel` path segment — is not a
//! body finding: it is refused behind the door, in the framework's own envelope, which
//! `crate::service`'s refusal seam keeps. WHICH half a member travels in is a property of
//! each operation's input schema, which is why this wording is attached per operation
//! through the shared front door ([`::service_common::front_door`]) rather than written in
//! that seam, where the violations arrive alone.
//!
//! A refusal a HANDLER words — a `caseId` nothing is stored under, a `nextToken` this
//! service never issued — is not a schema finding either: the request satisfied every rule
//! the schema declares and was refused on this service's own state.
//!
//! ALL OR NOTHING. A refusal carrying one violation this layer states no finding for keeps
//! the framework's whole envelope rather than being reported half in one voice and half in
//! the other, which is why [`body_findings`] answers `None` for it.

use ::aws_smithy_schema::{Schema, ShapeType};
use ::aws_smithy_schema_server::constraints::path;
use ::service_common::validation::SubmittedEcho;
use ::service_common::validation_message as message;
use ::smithy_server_runtime::{Operation, Refused, Violation, ViolationKind};

/// The wire error code of `com.amazonaws.securityir#ValidationException` — the shape name,
/// which is what an SDK matches a restJson1 error on, and the code the framework's own
/// refusal already names, so the chosen message travels with the code a client was already
/// branching on.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The JSON Schema `format` attribute a `@sensitive` string property is declared with.
const PASSWORD_FORMAT: &str = "password";

/// The JSON Schema `format` attribute a timestamp property is declared with: this protocol
/// carries a `Timestamp` member as epoch seconds, a JSON number.
const DOUBLE_FORMAT: &str = "double";

/// The findings this layer states for the violations of ONE request's body, as the
/// operation `Op` validates it, or `None` to leave the request to the layers behind the
/// door.
///
/// Monomorphised per operation and handed to that operation's own door, which is what puts
/// `Op`'s input schema — the schema the body is validated against, and therefore the one the
/// `format` findings are read off — in reach of a wording the refusal seam could not give
/// it.
pub(super) fn body_findings_for<Op: Operation>(
    refused: &[(Violation, SubmittedEcho)],
) -> Option<Refused> {
    body_findings(Op::INPUT_SCHEMA, refused)
}

/// The report `refused` is stated as, for a body validated against `input_schema`, or `None`
/// when there is nothing to report or one violation has no finding in this layer's voice.
fn body_findings(
    input_schema: &Schema,
    refused: &[(Violation, SubmittedEcho)],
) -> Option<Refused> {
    if refused.is_empty() {
        return None;
    }
    let mut findings = broken_rule_findings(input_schema, refused)?;
    findings.extend(
        unsupported_format_attributes(input_schema)
            .into_iter()
            .map(message::format_attribute_not_supported),
    );
    Some(Refused {
        code: VALIDATION_EXCEPTION.to_string(),
        message: message::bracketed_findings(&findings),
    })
}

/// One finding per rule the body broke, in the order the validating walk found them, or
/// `None` when any one of them is a class this layer states no finding for.
///
/// The absent members are the one exception to "one finding per violation": a JSON Schema
/// states the properties an object is missing as a single finding listing all of them, so
/// they are reported together, where the first of them was found.
fn broken_rule_findings(
    input_schema: &Schema,
    refused: &[(Violation, SubmittedEcho)],
) -> Option<Vec<String>> {
    let absent: Vec<&str> = refused
        .iter()
        .filter(|(violation, _)| {
            matches!(violation.kind, ViolationKind::MissingRequiredMember)
        })
        .map(|(violation, _)| property_name(input_schema, &violation.path))
        .collect();
    let mut findings = Vec::with_capacity(refused.len());
    let mut absent_stated = false;
    for (violation, echo) in refused {
        if matches!(violation.kind, ViolationKind::MissingRequiredMember) {
            if !absent_stated {
                absent_stated = true;
                findings.push(message::missing_required_properties(&absent));
            }
            continue;
        }
        // Every other class takes the ONE dispatch from a violation class to the sentence
        // this layer states about it, so nothing here can word a finding differently from
        // another service handed the same violation — and a class that dispatch states
        // nothing for leaves the whole refusal to the framework's envelope.
        findings.push(message::schema_finding(violation, echo)?);
    }
    Some(findings)
}

/// The property a schema finding names a member by: the name that member has in the object
/// it was read from — its `@jsonName` where the model declares one, else the member name as
/// the walk spells a path segment.
///
/// A nested path (an element of a list, an entry of a map, a member of a nested structure)
/// names no top-level member of `input_schema`, so it is named by its last step, which is
/// the property inside that object.
fn property_name<'paths>(input_schema: &'paths Schema, violation_path: &'paths str) -> &'paths str {
    for member in input_schema.members() {
        let Some(name) = member.member_name() else {
            continue;
        };
        if path::member(name) == violation_path {
            return member.json_name().map_or(name, |json| json.value());
        }
    }
    violation_path.rsplit('.').next().unwrap_or(violation_path)
}

/// Every JSON Schema `format` attribute `input_schema` declares that the validator
/// implements no check for, each stated once, in the order the operation declares its
/// members.
///
/// Read off the operation's OWN request schema, so the findings follow the model: an
/// operation whose input declares none — one taking only a token and a page size — is
/// refused with the findings about its values and nothing else.
fn unsupported_format_attributes(input_schema: &Schema) -> Vec<&'static str> {
    let mut attributes: Vec<&'static str> = Vec::new();
    for member in input_schema.members().iter().copied() {
        if let Some(attribute) = unsupported_format(member) {
            if !attributes.contains(&attribute) {
                attributes.push(attribute);
            }
        }
    }
    attributes
}

/// The `format` attribute `member`'s declared type carries and the validator implements no
/// check for, or `None` for a type that carries none.
///
/// Read off the member's own schema, so the finding follows the MODEL rather than a member
/// name: a string the model marks `@sensitive` is declared as a password, and a `Timestamp`
/// — which this protocol carries as epoch seconds — as a number with a double format.
fn unsupported_format(member: &Schema) -> Option<&'static str> {
    match member.shape_type() {
        ShapeType::String if is_sensitive(member) => Some(PASSWORD_FORMAT),
        ShapeType::Timestamp => Some(DOUBLE_FORMAT),
        _ => None,
    }
}

/// Whether the model marks `member` `@sensitive`.
fn is_sensitive(member: &Schema) -> bool {
    member.traits().is_some_and(|traits| {
        traits
            .get(&::aws_smithy_schema::traits::SensitiveTrait::TRAIT_ID)
            .is_some()
    })
}

#[cfg(test)]
mod tests {
    //! The two halves of the report no request can drive through the wire on its own: which
    //! `format` findings an operation's schema owes, and how SEVERAL absent properties are
    //! listed. Every class of broken rule a request CAN reach is pinned over the router in
    //! `tests/all/validation_report.rs`.

    use super::*;
    use ::engine::InMemoryEngine;

    /// A violation of `kind` at `path`, with the value the walk stored for it.
    fn violation(path: &str, kind: ViolationKind, value: Option<&str>) -> Violation {
        Violation {
            path: path.to_string(),
            kind,
            value: value.map(str::to_string),
            sensitive: false,
        }
    }

    /// The input schema of one operation, which is what the findings are read off.
    fn input_schema<Op: Operation>() -> &'static Schema {
        Op::INPUT_SCHEMA
    }

    /// RULE: the `format` findings an operation owes are the attributes ITS OWN request
    /// schema declares, each once, in member order — so an operation whose input declares
    /// none owes none.
    #[test]
    fn the_format_findings_follow_each_operations_own_schema() {
        // (case, the operation's input schema, the attributes its schema declares)
        let rows: [(&str, &'static Schema, &[&str]); 4] = [
            (
                // `title` and `description` are `@sensitive` strings and
                // `reportedIncidentStartDate` a timestamp, so both attributes are owed —
                // once each, though two members carry the first.
                "an input declaring sensitive strings and a timestamp",
                input_schema::<crate::ops::create_case::CreateCase<InMemoryEngine>>(),
                &["password", "double"],
            ),
            (
                // `nextToken` is a plain string and `maxResults` an integer.
                "an input declaring neither",
                input_schema::<crate::ops::list_cases::ListCases<InMemoryEngine>>(),
                &[],
            ),
            (
                // One `@sensitive` string member (`body`) and no timestamp.
                "an input declaring a sensitive string alone",
                input_schema::<crate::ops::create_case_comment::CreateCaseComment<InMemoryEngine>>(),
                &["password"],
            ),
            (
                // `caseId` alone, a plain string carried in the URI.
                "an input declaring one plain string",
                input_schema::<crate::ops::get_case::GetCase<InMemoryEngine>>(),
                &[],
            ),
        ];

        for (case, schema, expected) in rows {
            let found = unsupported_format_attributes(schema);
            assert_eq!(
                found, expected,
                "case {case:?}: expected {expected:?}, got {found:?}"
            );
        }
    }

    /// RULE: an object missing several of the properties its schema requires is reported as
    /// ONE finding listing all of them, where the first was found — a JSON Schema states
    /// `required` about the object, not about each property.
    #[test]
    fn several_absent_properties_are_one_finding() {
        let schema = input_schema::<crate::ops::create_case::CreateCase<InMemoryEngine>>();
        let refused = vec![
            (
                violation("resolverType", ViolationKind::MissingRequiredMember, None),
                SubmittedEcho::Absent,
            ),
            (
                violation("engagementType", ViolationKind::MissingRequiredMember, None),
                SubmittedEcho::Absent,
            ),
        ];
        let worded = body_findings(schema, &refused)
            .expect("case \"two absent properties\": must be reported");
        let expected = concat!(
            r#"[object has missing required properties (["resolverType","engagementType"]), "#,
            r#"format attribute "password" not supported, "#,
            r#"format attribute "double" not supported]"#
        );
        assert_eq!(
            worded.message, expected,
            "case \"two absent properties\": expected {expected:?}, got {:?}",
            worded.message
        );
        assert_eq!(
            worded.code, VALIDATION_EXCEPTION,
            "case \"two absent properties\": expected {VALIDATION_EXCEPTION:?}, got {:?}",
            worded.code
        );
    }

    /// RULE: a refusal this layer has no finding for is not reported half in its voice —
    /// neither an empty violation set, nor a violation whose value the walk withheld.
    #[test]
    fn what_this_layer_has_no_finding_for_is_left_to_the_framework() {
        let schema = input_schema::<crate::ops::create_case::CreateCase<InMemoryEngine>>();
        let withheld = Violation {
            sensitive: true,
            ..violation("title", ViolationKind::MinLength(1), None)
        };
        // (case, the violations of one body)
        let rows: [(&str, Vec<(Violation, SubmittedEcho)>); 2] = [
            ("a body that broke no rule", Vec::new()),
            (
                "a violation whose value is withheld",
                vec![(withheld, SubmittedEcho::Rendered(String::new()))],
            ),
        ];
        for (case, refused) in rows {
            let worded = body_findings(schema, &refused);
            assert!(
                worded.is_none(),
                "case {case:?}: expected no report, got {worded:?}"
            );
        }
    }
}
