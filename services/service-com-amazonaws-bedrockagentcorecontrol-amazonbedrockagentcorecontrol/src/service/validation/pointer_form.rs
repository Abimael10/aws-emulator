// SPDX-License-Identifier: CC0-1.0

//! The clause form the capacity-provider backend of this endpoint states a constraint
//! refusal in, as rule 6 of [`super`] states it.
//!
//! [`super`] owns the walk over the violations — which clauses a refusal states, in what
//! order, and the requirement every other backend of this endpoint words them with — and
//! this module owns the one thing that differs: the ENVELOPE, the SPELLING of the member
//! path, the absence of a value echo, and the way a `@range` or `@length` violation states
//! the WINDOW the model declares rather than the single bound the value fell outside.
//!
//! WHY A SEPARATE FORM: `bedrock-agentcore-control` is one endpoint in front of several
//! AgentCore services, one per resource family (see [`super::VALUE_LESS_FAMILIES`]), and the
//! `/capacity-providers` backend does not word a refusal the way the others do. Its recorded
//! refusal of an out-of-range `idleInstanceTimeout` states no violation count, addresses the
//! offending member as a JSON pointer rather than a dotted path, quotes no value, and names
//! BOTH modelled bounds — none of which the counted clause the other families state can
//! produce.
//!
//! A violation carries only the bound that failed
//! ([`ViolationKind::MaxValue`] holds the maximum alone), so the window a clause states is
//! read off the MODEL: [`WINDOWS`] is built once from the generated schemas of the shapes
//! this family's inputs reach.
//!
//! The sentences themselves are the Smithy framework's, so other services of this tree whose
//! backends state this form render the same ones; they are spelled here rather than shared
//! because the WINDOW half of them is this service's model, read off this family's own
//! schemas, and lifting the renderer into `services/common` would edit another service's
//! refusal path, which this change does not own.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use aws_smithy_schema::Schema;
use aws_smithy_schema_server::constraints::{
    self, LengthTrait, RangeTrait, ViolationKind,
};

use super::{constraint_trait, render_bound, CLAUSE_MARKER};

/// The first URI segment of every resource family whose backend states a constraint refusal
/// in this form.
///
/// Read off the operation's own modelled `@http` URI the way [`super::echo_of`] reads the
/// echo policy, so every operation of a listed family is worded alike and an operation added
/// to one later needs no change here.
///
/// Attested by the recorded refusal of a `CreateCapacityProvider` whose
/// `computeConfiguration.ec2Configuration.lifecycleConfiguration.idleInstanceTimeout` was
/// past its modelled maximum: `validation error detected. Value at
/// '/computeConfiguration/ec2Configuration/lifecycleConfiguration/idleInstanceTimeout'
/// failed to satisfy constraint: Member must be between 60 and 1209600, inclusive`.
pub(super) const FAMILIES: &[&str] = &["capacity-providers"];

/// The whole of the envelope this form states: no violation count, and a full stop rather
/// than the colon the counted envelope separates its clauses with.
const ENVELOPE: &str = "validation error detected. ";

/// This form's envelope over the clauses of one refusal, or `None` when there are none.
///
/// Several clauses are joined the way the counted envelope joins them
/// ([`super::envelope`]), which is the only part of a many-clause refusal this form's one
/// recorded sentence does not fix.
pub(super) fn envelope(clauses: &[String]) -> Option<String> {
    (!clauses.is_empty()).then(|| format!("{ENVELOPE}{}", clauses.join("; ")))
}

/// One clause of this form: the offending member as the JSON pointer that names it in the
/// request, and the requirement it failed — with no value of the caller's between them.
///
/// `path` is the dotted path the framework's walk reports the violation at, and the pointer
/// is the tree's one spelling of one ([`service_common::json_pointer`]) rather than a second
/// copy of that rule.
pub(super) fn clause(path: &str, requirement: &str) -> String {
    format!(
        "Value at '{}' {CLAUSE_MARKER}{requirement}",
        service_common::json_pointer::json_pointer(path)
    )
}

/// The requirement this form states for one violation at `path`, or `None` for a class
/// [`super`] has no wording for.
///
/// A `@range` or `@length` violation states the WINDOW the model declares at that member —
/// both bounds, as one inclusive interval — when the model declares both; everything else is
/// the requirement every other backend of this endpoint states, so one service does not word
/// the same constraint two ways.
pub(super) fn requirement(path: &str, kind: &ViolationKind) -> Option<String> {
    let windows = path.rsplit('.').next().and_then(|leaf| WINDOWS.get(leaf));
    match kind {
        ViolationKind::MinValue(_) | ViolationKind::MaxValue(_) => {
            value_requirement(windows.and_then(|windows| windows.range), kind)
        }
        ViolationKind::MinLength(_) | ViolationKind::MaxLength(_) => {
            length_requirement(windows.and_then(|windows| windows.length), kind)
        }
        other => super::requirement_of(other),
    }
}

/// A `@range` violation as this form states it: the declared interval when the model bounds
/// the member on both sides, and otherwise the one bound it declares.
///
/// The one-sided sentences drop the counted clause's `have value` infix, in the same phrase
/// family as the two-sided one the recording fixes (`Member must be between …, inclusive`
/// carries no such infix either) — which is how the other services of this tree that render
/// this form word them too. That is why this one does not delegate to
/// [`super::requirement_of`] the way [`length_requirement`] does: the counted form's `@length`
/// sentences already read the way this form states them, and its `@range` sentences do not.
fn value_requirement(range: Option<RangeTrait>, kind: &ViolationKind) -> Option<String> {
    if let Some(RangeTrait {
        min: Some(min),
        max: Some(max),
    }) = range
    {
        return Some(format!(
            "Member must be between {} and {}, inclusive",
            render_bound(min),
            render_bound(max)
        ));
    }
    match kind {
        ViolationKind::MinValue(min) => {
            Some(format!("Member must be greater than or equal to {min}"))
        }
        ViolationKind::MaxValue(max) => Some(format!("Member must be less than or equal to {max}")),
        _ => None,
    }
}

/// A `@length` violation as this form states it, on the same rule as [`value_requirement`]:
/// the declared interval, or the one bound the model declares.
fn length_requirement(length: Option<LengthTrait>, kind: &ViolationKind) -> Option<String> {
    if let Some(LengthTrait {
        min: Some(min),
        max: Some(max),
    }) = length
    {
        return Some(format!(
            "Member must have length between {min} and {max}, inclusive"
        ));
    }
    super::requirement_of(kind)
}

/// The `@range` and `@length` windows the model declares at one member.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Windows {
    range: Option<RangeTrait>,
    length: Option<LengthTrait>,
}

/// Every bounded member of the shapes a [`FAMILIES`] operation's input reaches, keyed by the
/// member path the framework's walk reports it under.
///
/// Built once off the generated schemas, so a bound that changes in the model changes here
/// with it rather than being restated. A member NAME two of those shapes declare with
/// DIFFERENT windows is left out: a violation's path leaf cannot tell them apart, and the
/// single bound that failed is a smaller statement, never a wrong one.
static WINDOWS: LazyLock<HashMap<String, Windows>> = LazyLock::new(|| {
    let mut declared: HashMap<String, Windows> = HashMap::new();
    let mut ambiguous: HashSet<String> = HashSet::new();
    for schema in shapes() {
        for member in schema.members() {
            let (Some(name), Some(traits)) = (member.member_name(), member.traits()) else {
                continue;
            };
            let windows = Windows {
                range: constraint_trait::<RangeTrait>(traits, &RangeTrait::TRAIT_ID).copied(),
                length: constraint_trait::<LengthTrait>(traits, &LengthTrait::TRAIT_ID).copied(),
            };
            if windows.range.is_none() && windows.length.is_none() {
                continue;
            }
            let path = constraints::path::member(name);
            match declared.get(&path) {
                Some(already) if *already != windows => {
                    ambiguous.insert(path);
                }
                _ => {
                    declared.insert(path, windows);
                }
            }
        }
    }
    declared.retain(|path, _| !ambiguous.contains(path));
    declared
});

/// Every shape a [`FAMILIES`] operation's input reaches, as the generated schema statics:
/// the operations' own input shapes, and the structures nested under them.
///
/// All of them rather than the ones a report happened to cite — a window is a property of
/// the model, not of the request that tripped it. A shape missing from this list is not a
/// wrong clause, only a clause that states the bound that failed rather than the whole
/// window.
///
/// ENUMERATED rather than walked: codegen attaches a member's own constraint traits to its
/// member schema, but a member schema carries no link to the schema of the shape it TARGETS
/// (only a collection member's element traits travel with it), so the closure cannot be
/// reached at runtime from the six input schemas. The list is therefore maintained the way
/// the sibling services maintain their `input_schemas()` lists, and
/// [`tests::every_bounded_member_of_the_family_keeps_its_window`] pins that every bounded
/// member of the shapes listed here reaches the catalog.
///
/// Source: `smithy_query(query="closure",
/// operation="CreateCapacityProvider,UpdateCapacityProvider,GetCapacityProvider,\
/// DeleteCapacityProvider,ListCapacityProviders,\
/// ListAgentRuntimeVersionsByCapacityProvider")`, whose structure shapes these are.
fn shapes() -> Vec<&'static Schema> {
    use crate::ops::types as t;
    vec![
        t::CREATE_CAPACITY_PROVIDER_INPUT_SCHEMA,
        t::UPDATE_CAPACITY_PROVIDER_INPUT_SCHEMA,
        t::GET_CAPACITY_PROVIDER_INPUT_SCHEMA,
        t::DELETE_CAPACITY_PROVIDER_INPUT_SCHEMA,
        t::LIST_CAPACITY_PROVIDERS_INPUT_SCHEMA,
        t::LIST_AGENT_RUNTIME_VERSIONS_BY_CAPACITY_PROVIDER_INPUT_SCHEMA,
        t::PERMISSIONS_CONFIGURATION_SCHEMA,
        t::EC2_CONFIGURATION_SCHEMA,
        t::VPC_CONFIGURATION_SCHEMA,
        t::INSTANCE_LIFECYCLE_CONFIGURATION_SCHEMA,
        t::ROOT_VOLUME_CONFIGURATION_SCHEMA,
        t::LAUNCH_PARAMETERS_SCHEMA,
        t::INSTANCE_REQUIREMENTS_SCHEMA,
        t::EPHEMERAL_BLOCK_DEVICE_MAPPING_SCHEMA,
        t::EPHEMERAL_EBS_VOLUME_CONFIGURATION_SCHEMA,
        t::EBS_VOLUME_CONFIGURATION_SCHEMA,
        t::LICENSE_SPECIFICATION_SCHEMA,
        t::CAPACITY_RESERVATION_SPECIFICATION_SCHEMA,
        t::CAPACITY_RESERVATION_TARGET_SCHEMA,
        t::UPDATED_DESCRIPTION_SCHEMA,
    ]
}

#[cfg(test)]
mod tests {
    //! The parts of this form that need no request: the window catalog this model declares,
    //! and the sentence each class of violation states. The end-to-end half — which
    //! operation is worded this way — is `tests/all/capacity_provider_refusal_wording.rs`.

    use super::*;

    /// The window a member declares is read off the model, both bounds of it, whether the
    /// value fell below the minimum or above the maximum.
    #[test]
    fn a_bounded_member_states_the_window_the_model_declares() {
        // (case, member path, the violation the walk found, the requirement stated)
        // Sources: `smithy_query(query="closure", operation="CreateCapacityProvider")` for
        // the `@range`s, `smithy_query(query="operation",
        // operation="ListCapacityProviders")` for `maxResults` and `nextToken`.
        let rows: [(&str, &str, ViolationKind, &str); 7] = [
            (
                "a nested member past its maximum",
                "computeConfiguration.ec2Configuration.lifecycleConfiguration.idleInstanceTimeout",
                ViolationKind::MaxValue("1209600".to_string()),
                "Member must be between 60 and 1209600, inclusive",
            ),
            (
                "the same member below its minimum: the same window",
                "computeConfiguration.ec2Configuration.lifecycleConfiguration.idleInstanceTimeout",
                ViolationKind::MinValue("60".to_string()),
                "Member must be between 60 and 1209600, inclusive",
            ),
            (
                "another member of the same shape",
                "computeConfiguration.ec2Configuration.lifecycleConfiguration.maxLifetime",
                ViolationKind::MaxValue("1209600".to_string()),
                "Member must be between 60 and 1209600, inclusive",
            ),
            (
                "a member whose window its target shape declares",
                "computeConfiguration.ec2Configuration.rootVolume.iops",
                ViolationKind::MaxValue("256000".to_string()),
                "Member must be between 100 and 256000, inclusive",
            ),
            (
                "a member the model bounds on one side only",
                "computeConfiguration.ec2Configuration.launchTemplateSource.launchParameters.\
                 ephemeralVolumes.1.member.ebs.ebsCardIndex",
                ViolationKind::MinValue("0".to_string()),
                "Member must be greater than or equal to 0",
            ),
            (
                "a top-level @range member of another operation of the family",
                "maxResults",
                ViolationKind::MinValue("1".to_string()),
                "Member must be between 1 and 100, inclusive",
            ),
            (
                "a @length window, stated the same way",
                "nextToken",
                ViolationKind::MaxLength(2048),
                "Member must have length between 1 and 2048, inclusive",
            ),
        ];

        for (case, path, kind, expected) in rows {
            let actual = requirement(path, &kind);
            assert_eq!(
                actual.as_deref(),
                Some(expected),
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// Every other class of violation states the sentence every backend of this endpoint
    /// states: only a bounded member's window is this form's own.
    #[test]
    fn every_other_class_states_the_shared_requirement() {
        let rows: [(&str, ViolationKind); 3] = [
            ("an absent required member", ViolationKind::MissingRequiredMember),
            (
                "a @pattern violation",
                ViolationKind::Pattern("[a-z]+".to_string()),
            ),
            ("a repeated element", ViolationKind::UniqueItems),
        ];

        for (case, kind) in rows {
            let actual = requirement("name", &kind);
            let shared = super::super::requirement_of(&kind);
            assert_eq!(
                actual, shared,
                "case {case:?}: expected the shared requirement {shared:?}, got {actual:?}"
            );
        }
    }

    /// The clause names the member as the pointer into the request body, and carries no
    /// value of the caller's.
    #[test]
    fn a_clause_addresses_the_member_as_a_json_pointer() {
        // (case, the dotted path the walk reports, the pointer the clause states)
        let rows: [(&str, &str, &str); 3] = [
            ("a top-level member", "maxResults", "/maxResults"),
            (
                "a nested member",
                "computeConfiguration.ec2Configuration.lifecycleConfiguration.idleInstanceTimeout",
                "/computeConfiguration/ec2Configuration/lifecycleConfiguration/\
                 idleInstanceTimeout",
            ),
            (
                "a member of a list element",
                "volumes.1.member.ebsConfiguration.iops",
                "/volumes/0/ebsConfiguration/iops",
            ),
        ];

        for (case, dotted, pointer) in rows {
            let actual = clause(dotted, "Member must not be null");
            let expected =
                format!("Value at '{pointer}' failed to satisfy constraint: Member must not be null");
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// Every bounded member of the family's shapes keeps its window: no two of them declare
    /// one member name with DIFFERENT bounds, which [`WINDOWS`] would drop as ambiguous.
    ///
    /// The catalog is keyed by the member path the walk reports, whose leaf is the member's
    /// own name, so this is what makes a window clause reachable for every bounded member of
    /// the family rather than only for the ones a suite happens to name. A model change that
    /// re-used a name with other bounds would fail here instead of silently degrading that
    /// member's clause to the one bound that failed.
    #[test]
    fn every_bounded_member_of_the_family_keeps_its_window() {
        let mut bounded: Vec<String> = Vec::new();
        for schema in shapes() {
            for member in schema.members() {
                let (Some(name), Some(traits)) = (member.member_name(), member.traits()) else {
                    continue;
                };
                let has_window = constraint_trait::<RangeTrait>(traits, &RangeTrait::TRAIT_ID)
                    .is_some()
                    || constraint_trait::<LengthTrait>(traits, &LengthTrait::TRAIT_ID).is_some();
                if has_window {
                    bounded.push(constraints::path::member(name));
                }
            }
        }
        assert!(
            !bounded.is_empty(),
            "case \"the family's shapes\": the listed shapes declare bounded members, got none"
        );
        let dropped: Vec<&String> = bounded
            .iter()
            .filter(|name| !WINDOWS.contains_key(*name))
            .collect();
        assert!(
            dropped.is_empty(),
            "case \"an ambiguous member name\": every bounded member of the family's shapes \
             keeps its window, yet {dropped:?} declare one name with different bounds"
        );
    }

    /// The envelope states no count, whichever of this form's refusals it closes over.
    #[test]
    fn the_envelope_states_no_count() {
        assert_eq!(envelope(&[]), None, "case \"no clause\": nothing to word");
        let one = envelope(&["first".to_string()]).expect("one clause words a refusal");
        assert_eq!(
            one, "validation error detected. first",
            "case \"one clause\": got {one:?}"
        );
        let two = envelope(&["first".to_string(), "second".to_string()])
            .expect("two clauses word a refusal");
        assert_eq!(
            two, "validation error detected. first; second",
            "case \"two clauses\": got {two:?}"
        );
        for (case, message) in [("one clause", &one), ("two clauses", &two)] {
            assert!(
                !message.contains("validation error detected:")
                    && !message.starts_with(char::is_numeric),
                "case {case:?}: this form prefixes no count, got {message:?}"
            );
        }
    }
}
