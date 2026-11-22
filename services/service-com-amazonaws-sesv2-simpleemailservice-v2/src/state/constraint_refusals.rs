// SPDX-License-Identifier: CC0-1.0

//! How this service words the refusal of a value outside a modelled constraint.
//!
//! One wording, whether the value was refused at the request BOUNDARY by the framework's
//! constraint walk ([`boundary_refusal_message`], which `crate::service` states on the
//! `Service::override_refusal` seam) or by a HANDLER reading the member itself
//! ([`modelled_enum_value`]), so the two cannot drift.
//!
//! Split out of the state module root so this one concern lives in its own file; every
//! item is re-exported from [`crate::state`], so a caller still reaches it as
//! `crate::state::…`.

use super::*;

/// `value` when it is one of `allowed`, and the refusal naming `member` otherwise.
///
/// The wording an operation lifts into its own input error, so every member of this
/// service refuses an unmodelled value the same way — and a member SES words the
/// refusal of itself ([`SERVICE_WORDED_ENUM_MEMBERS`]) gets that sentence here too,
/// so a handler's own read of the member and the request boundary's read of it cannot
/// answer differently.
pub fn modelled_enum_value(
    member: &str,
    value: String,
    allowed: &[&str],
) -> Result<String, String> {
    if allowed.contains(&value.as_str()) {
        return Ok(value);
    }
    if let Some(sentence) = service_worded_enum_refusal(member) {
        return Err(sentence.to_string());
    }
    // The values are listed in the order this service publishes for the enum, the same
    // order the request boundary's own report of the value set states
    // ([`published_enum_value_list`]).
    Err(format!(
        "{member} {value} is not one of {}.",
        published_enum_value_list(allowed)
    ))
}

/// The members whose refusal for a value outside their own modelled value set SES
/// states as its OWN sentence, rather than as a report of the constraint that failed.
///
/// A constraint report names the member and the requirement it broke, which is what
/// SES answers for most members. For the members below it answers a semantic sentence
/// about what the member MEANS instead: fixed text with no variable part, naming
/// neither the offending value nor a member path. Stated once here, keyed by the
/// member's own name, so the sentence does not depend on which operation carried the
/// member, where in a collection the offending entry sat, or whether the value was
/// refused on the request boundary or by a handler reading the member itself.
const SERVICE_WORDED_ENUM_MEMBERS: [(&str, &str); 1] =
    [("DimensionValueSource", "invalid CloudWatch dimension source")];

/// SES's own sentence for a value outside `member`'s modelled value set, for a member
/// it words the refusal of itself; `None` for every other member, which is refused
/// with the constraint report.
///
/// `member` may be a dotted PATH to the member, as the constraint walk spells one, or
/// the member's own name, as a handler names the one it read: the last segment is what
/// is matched, and the match ignores case because a path spells a member in lowerCamel
/// while the model names it in UpperCamel.
fn service_worded_enum_refusal(member: &str) -> Option<&'static str> {
    let name = member.rsplit('.').next().unwrap_or(member);
    SERVICE_WORDED_ENUM_MEMBERS
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, sentence)| *sentence)
}

/// SES's own sentence for `violation`, when it is a value outside the value set of a
/// member SES words the refusal of itself ([`service_worded_enum_refusal`]).
fn service_worded_violation(violation: &Violation) -> Option<&'static str> {
    if !matches!(violation.kind, ViolationKind::EnumValue(_)) {
        return None;
    }
    service_worded_enum_refusal(&violation.path)
}

/// The text separating the subject of a validation clause from the requirement it
/// states, as the framework renders one.
const CONSTRAINT_SEPARATOR: &str = " failed to satisfy constraint: ";

/// The step a member path takes from a collection into one of its elements, as the
/// constraint walk spells it: the element's position within the collection, then this.
const COLLECTION_STEP: &str = ".member";

/// The collection member a violation reported at `path` belongs to, when the path names a
/// position INSIDE a collection rather than a member of its own.
///
/// The walk spells a step from a collection into one of its elements as the element's
/// position followed by the collection step (`suppressedReasons.2.member` for the second
/// entry of a list), and a member reached THROUGH an element carries its own name after
/// that step (`queries.1.member.namespace`) — so a path ENDING in the step is the one that
/// names an element itself, and this answers the member the collection sits at, with the
/// position dropped.
fn collection_member(path: &str) -> Option<&str> {
    let (collection, _position) = path.strip_suffix(COLLECTION_STEP)?.rsplit_once('.')?;
    (!collection.is_empty()).then_some(collection)
}

/// The message this service answers a refusal the constraint walk made at the request
/// boundary with: one clause per violation, in the envelope AWS counts them in.
///
/// Every clause is the framework's own, with ONE difference: a value refused for its
/// FORM — outside a modelled enum's value set, or outside a modelled `@pattern` — or for
/// being LONGER than a modelled `@length` maximum is stated WITHOUT the value. SES names
/// the member and the value set, the pattern or the maximum that member had to satisfy
/// and withholds what was sent. A too-SHORT value, a range violation and an absent member
/// quote the value the way the framework renders it, which is how AWS's own validation
/// renders those.
///
/// `None` when there is nothing to state, which keeps the framework's own wording.
pub fn boundary_refusal_message(violations: &[Violation]) -> Option<String> {
    // A member whose refusal for a value outside its own value set SES words ITSELF
    // answers that sentence alone: it is a semantic refusal about what the member
    // means, not a report of the walk, so it carries neither the envelope the walk
    // counts clauses in nor any other clause.
    if let Some(sentence) = violations.iter().find_map(service_worded_violation) {
        return Some(sentence.to_string());
    }
    let clauses: Vec<String> = violations.iter().map(boundary_clause).collect();
    match clauses.len() {
        0 => None,
        1 => Some(format!("1 validation error detected: {}", clauses[0])),
        n => Some(format!(
            "{n} validation errors detected: {}",
            clauses.join("; ")
        )),
    }
}

/// One clause of [`boundary_refusal_message`].
///
/// The requirement half is the framework's own words — the clause is cut at the
/// separator rather than rebuilt — so a constraint class the framework learns to check
/// cannot drift from this wording. Two things about a clause are decided here:
///
/// * WHERE it is reported. A constraint the ELEMENTS of a collection carry is reported
///   against the collection's own member, with the element's requirement nested inside
///   the collection's clause (`Member must satisfy constraint: [<requirement>]`) and the
///   element's position within the collection not named at all. A member reached THROUGH
///   an element is reported at its own position, element and all
///   ([`collection_member`] is what separates the two). The violation is RESTATED at that
///   member and then rendered, so the clause stays the walk's own words.
/// * Whether the SUBJECT states the value. A value refused for its FORM — outside a
///   modelled enum's value set, or outside a modelled `@pattern` — or for being LONGER
///   than a modelled `@length` maximum is stated WITHOUT the value: SES names the member
///   and the value set, the pattern or the maximum that member had to satisfy and
///   withholds what was sent. A too-SHORT value, a range violation and an absent member
///   quote the value the way the framework renders it, which is how AWS's own validation
///   renders those.
///
/// The one other thing the walk does not decide is the ORDER a value set is listed in,
/// which belongs to the enum rather than to the walk that read the model's members
/// ([`in_published_enum_value_order`]) — applied to the violation before it is
/// rendered, so the sentence around the list stays the framework's whatever shape it
/// takes.
fn boundary_clause(violation: &Violation) -> String {
    let violation = in_published_enum_value_order(violation);
    let element_of = collection_member(&violation.path).map(str::to_string);
    let violation = &match &element_of {
        Some(collection) => Violation {
            path: collection.clone(),
            ..violation
        },
        None => violation,
    };
    let rendered = violation.message();
    let Some((subject, requirement)) = rendered.split_once(CONSTRAINT_SEPARATOR) else {
        return rendered;
    };
    let subject = if matches!(
        violation.kind,
        ViolationKind::EnumValue(_) | ViolationKind::Pattern(_) | ViolationKind::MaxLength(_)
    ) {
        format!("Value at '{}'", violation.path)
    } else {
        subject.to_string()
    };
    let requirement = match element_of {
        Some(_) => format!("Member must satisfy constraint: [{requirement}]"),
        None => requirement.to_string(),
    };
    format!("{subject}{CONSTRAINT_SEPARATOR}{requirement}")
}
