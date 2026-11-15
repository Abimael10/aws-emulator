// SPDX-License-Identifier: CC0-1.0

//! What the boundary records about the INPUT SHAPE each request is read against, for
//! the two things this service's refusal states that the violations alone cannot say.
//!
//! 1. **The `@pattern` the walk declares but cannot apply.** The requirement is
//!    applied by [`super::unapplied_patterns`], over the expressions recorded here.
//! 2. **The constraint set a collection's ELEMENTS declare.** A refusal carries only
//!    the constraint that FAILED, and a clause stated at a collection member lists
//!    the element shape's WHOLE set, so the set has to be read off the model
//!    ([`element_set`]).
//!
//! Both are read off the member schemas of the input shape, so both are bounded the
//! same way by what codegen puts on one: a member schema carries its own constraints
//! but not the members of the shape it targets, so what is recorded here belongs to
//! the input shape's OWN members. A member nested inside a structure element keeps
//! the framework's own clause, which is what AWS states for one, and the reference
//! form of such a member is stated by the operation that reads it.
//!
//! # The invariant the recording rests on
//!
//! The two sides are ONE poll apart with nothing awaited between them: the
//! framework's sequence reads the request through
//! [`ServerProtocol::deserialize_input`](aws_smithy_schema_server::ServerProtocol::deserialize_input),
//! walks the value it built against the model's rules, and asks the service to word
//! any violation it found, all without yielding
//! (`smithy_server_runtime::run_operation_http`). Every modelled request goes through
//! that read, which REPLACES whatever this thread recorded for the request before it,
//! so what is read back belongs to the request being refused. Nothing is quietly
//! wrong if that ever stops holding: an expression is applied only to a value the
//! SAME violation carries, and a member path the recorded shape does not declare adds
//! no clause and groups none.

use std::cell::RefCell;

use ::aws_smithy_schema::{Schema, ShapeId, TraitMap};
use ::aws_smithy_schema_server::constraints::{
    path, ElementConstraintsTrait, EnumTrait, LengthTrait, PatternTrait, RangeTrait,
};
use ::service_common::validation_message::{
    element_set_requirement, hashed_enum_order, requirement,
};
use ::smithy_server_runtime::{Violation, ViolationKind};

/// What one input shape declares that a refusal of it needs.
#[derive(Debug, Clone, Default)]
struct Declared {
    /// `(the member path a clause spells, the `@pattern` the member declares)`, one
    /// per member of the shape carrying one.
    patterns: Vec<(String, &'static str)>,
    /// `(the member path a clause spells, the requirement a clause stated at that
    /// collection member nests)`, one per member whose ELEMENTS carry constraints.
    element_sets: Vec<(String, String)>,
}

thread_local! {
    /// What the input shape this thread is reading declares.
    ///
    /// Recorded for every modelled request and read only while wording that request's
    /// refusal (see the module docs).
    static DECLARED: RefCell<Declared> = RefCell::new(Declared::default());
}

/// Record what `input_schema` declares, as the input shape this thread is now
/// reading.
pub(super) fn reading_input(input_schema: &Schema) {
    let mut declared = Declared::default();
    for member in input_schema.members() {
        let Some(name) = member.member_name() else {
            continue;
        };
        let member_path = path::member(name);
        let Some(traits) = member.traits() else {
            continue;
        };
        if let Some(pattern) = constraint::<PatternTrait>(traits, &PatternTrait::TRAIT_ID) {
            declared.patterns.push((member_path.clone(), pattern.regex));
        }
        if let Some(element) =
            constraint::<ElementConstraintsTrait>(traits, &ElementConstraintsTrait::TRAIT_ID)
        {
            if let Some(stated) = stated_element_set(element.inner()) {
                declared.element_sets.push((member_path, stated));
            }
        }
    }
    DECLARED.with(|slot| *slot.borrow_mut() = declared);
}

/// One constraint trait off a trait map, downcast to the type carrying its bounds.
fn constraint<'map, T: 'static>(traits: &'map TraitMap, id: &ShapeId) -> Option<&'map T> {
    traits.get(id)?.as_any().downcast_ref::<T>()
}

/// The requirement a clause stated at a collection member nests: every constraint the
/// ELEMENT shape declares, as this service words one, inside the shared home's
/// `Member must satisfy constraint: [ … ]`.
///
/// The constraints are read off the element trait map here rather than through
/// `service_common::validation_message::element_constraint_set`, the shared reading of
/// them, for one reason: that entry point takes `&'static Schema`, which the borrow a
/// protocol seam is handed is not. The ORDER is that reading's — a `@length` maximum,
/// then its minimum, then the numeric bounds the same way, then the expression, then
/// an enumerated value set, with a `@length` that declares no minimum stating `0`, the
/// smallest a Smithy `@length` can bound — and every requirement SENTENCE, the
/// bracketed set included, is the shared home's, so this states nothing in words of
/// its own.
///
/// `None` when the elements carry no constraint at all, which leaves such a clause the
/// framework's own.
fn stated_element_set(elements: &TraitMap) -> Option<String> {
    let mut declared: Vec<ViolationKind> = Vec::new();
    if let Some(length) = constraint::<LengthTrait>(elements, &LengthTrait::TRAIT_ID) {
        if let Some(max) = length.max {
            declared.push(ViolationKind::MaxLength(max));
        }
        declared.push(ViolationKind::MinLength(length.min.unwrap_or(0)));
    }
    if let Some(range) = constraint::<RangeTrait>(elements, &RangeTrait::TRAIT_ID) {
        if let Some(max) = range.max {
            declared.push(ViolationKind::MaxValue(bound(max)));
        }
        if let Some(min) = range.min {
            declared.push(ViolationKind::MinValue(bound(min)));
        }
    }
    if let Some(pattern) = constraint::<PatternTrait>(elements, &PatternTrait::TRAIT_ID) {
        declared.push(ViolationKind::Pattern(
            super::validation::published_expression(pattern.regex),
        ));
    }
    if let Some(values) = constraint::<EnumTrait>(elements, &EnumTrait::TRAIT_ID) {
        declared.push(ViolationKind::EnumValue(
            hashed_enum_order(values.values)
                .into_iter()
                .map(str::to_string)
                .collect(),
        ));
    }
    if declared.is_empty() {
        return None;
    }
    let requirements: Vec<String> = declared.into_iter().map(requirement).collect();
    Some(element_set_requirement(&requirements))
}

/// A `@range` bound as a requirement displays it: an integral bound without a trailing
/// `.0`, a fractional one with its decimals — the rendering the framework's own clause
/// carries, so a bound read off the model and one read off a violation agree.
fn bound(declared: f64) -> String {
    if declared.fract() == 0.0 && declared.is_finite() {
        format!("{declared:.0}")
    } else {
        format!("{declared}")
    }
}

/// The requirement a clause stated at the collection member `collection` nests, or
/// `None` when the input shape being read declares no constrained elements for it.
pub(super) fn element_set(collection: &str) -> Option<String> {
    DECLARED.with(|slot| {
        slot.borrow()
            .element_sets
            .iter()
            .find(|(member, _)| member == collection)
            .map(|(_, stated)| stated.clone())
    })
}

/// `violations` with the requirement the walk could not apply added to it, over the
/// expressions the input shape being read declares
/// ([`super::unapplied_patterns::applied`]).
pub(super) fn with_unapplied_patterns(violations: &[Violation]) -> Vec<Violation> {
    let declared = DECLARED.with(|slot| slot.borrow().patterns.clone());
    super::unapplied_patterns::applied(violations, &declared)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the set a clause nests is every constraint the element shape declares,
    /// read off the input shape being read. Driven over this model's own
    /// `ListAssetModels` input, whose `assetModelTypes` is a list of an enumerated
    /// type (`smithy_query(query="operation", operation="ListAssetModels")`), and over
    /// an input that declares no collection of constrained scalars at all.
    #[test]
    fn a_collections_element_set_is_read_off_the_input_shape() {
        reading_input(crate::ops::types::LIST_ASSET_MODELS_INPUT_SCHEMA);
        let stated = element_set("assetModelTypes");
        assert_eq!(
            stated.as_deref(),
            Some(
                "Member must satisfy constraint: [Member must satisfy enum value set: \
                 [COMPONENT_MODEL, INTERFACE, ASSET_MODEL]]"
            ),
            "the element set of a list of an enumerated type, got {stated:?}"
        );
        let plain = element_set("maxResults");
        assert_eq!(
            plain, None,
            "a member that is no collection of constrained scalars nests no set, got {plain:?}"
        );

        reading_input(crate::ops::types::DESCRIBE_ASSET_MODEL_INPUT_SCHEMA);
        let forgotten = element_set("assetModelTypes");
        assert_eq!(
            forgotten, None,
            "the shape being read is the one just recorded, got {forgotten:?}"
        );
    }

    /// RULE: the expressions recorded are those of the input shape's own members, and
    /// the one just read replaces the one before it.
    #[test]
    fn the_expressions_recorded_are_the_shapes_own() {
        reading_input(crate::ops::types::ASSOCIATE_ASSETS_INPUT_SCHEMA);
        let declared = DECLARED.with(|slot| slot.borrow().patterns.clone());
        for member in ["assetId", "hierarchyId", "childAssetId", "clientToken"] {
            assert!(
                declared.iter().any(|(path, _)| path == member),
                "the input shape declares an expression on {member:?}, got {declared:?}"
            );
        }
        reading_input(crate::ops::types::LIST_ASSET_MODELS_INPUT_SCHEMA);
        let after = DECLARED.with(|slot| slot.borrow().patterns.clone());
        assert!(
            !after.iter().any(|(path, _)| path == "hierarchyId"),
            "the shape being read is the one just recorded, got {after:?}"
        );
    }
}
