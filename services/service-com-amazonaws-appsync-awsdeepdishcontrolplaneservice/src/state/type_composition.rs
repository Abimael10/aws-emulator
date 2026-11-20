// SPDX-License-Identifier: CC0-1.0

//! How the type systems of several source APIs COMBINE into the one a Merged API holds.
//!
//! "When linked to a merged API, the source API's schema, data sources, and resolvers will
//! be combined with the other associated source APIs" (`SourceApiAssociation`): a Merged
//! API's schema is the composition of EVERY source API's schema, so a type name more than
//! one source API declares is held once, carrying what all of them declare under it. The
//! root operation types are the names this is normally observed on — every source API
//! declares its own fields on `Query` — but the rule is about the name, not about which
//! name it is, so it holds for any type two sources both declare.
//!
//! What cannot be combined is a DISAGREEMENT: one name declared as two different kinds, or
//! one member of it declared twice with two different definitions. There is no composed
//! type that satisfies both declarations, so the merge that carries the second one is not
//! performed at all — the association reports `MERGE_FAILED` with the conflict as its
//! detail (`crate::state::merge_failed_detail`) and the Merged API keeps the schema it
//! held. Two sources declaring the same member IDENTICALLY say the same thing, so that is
//! no disagreement: the composed type carries that member once.

use std::collections::BTreeMap;

use super::{
    DirectiveUse, EnumValueDefinition, FieldDefinition, InputValueDefinition, TypeDefinition,
    ENUM_KIND, INPUT_OBJECT_KIND, INTERFACE_KIND, OBJECT_KIND,
};

/// Why two declarations of one type name cannot be combined into one type.
///
/// Carried out of the composition rather than worded there, so the sentence the detail of
/// a failed merge states is built where every other status detail is
/// (`crate::state::merge_failed_detail`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeConflict {
    /// The name is declared as two different `__TypeKind`s, which no one type can be.
    Kind {
        type_name: String,
        held: &'static str,
        incoming: &'static str,
    },
    /// The name is declared by both with this member, and the two definitions of that
    /// member differ, so the composed type would have to carry it twice.
    Member {
        type_name: String,
        member: String,
    },
    /// Both declarations write this directive on the name, with different arguments, so
    /// the composed type would have to carry one directive twice.
    Directive {
        type_name: String,
        directive: String,
    },
}

impl TypeConflict {
    /// What the two declarations disagree about, as the sentence a failed merge's detail
    /// states it: the type name, and the member or the kinds that cannot be reconciled.
    pub fn conflict(&self) -> String {
        match self {
            Self::Kind {
                type_name,
                held,
                incoming,
            } => format!(
                "the type {type_name} is declared as {incoming} by this source API and as \
                 {held} by another"
            ),
            Self::Member { type_name, member } => format!(
                "the member {member} of the type {type_name} is declared differently by this \
                 source API and by another"
            ),
            Self::Directive {
                type_name,
                directive,
            } => format!(
                "the directive @{directive} on the type {type_name} is written with different \
                 arguments by this source API and by another"
            ),
        }
    }
}

/// Combine the declaration `incoming` into the declaration `held` of the same type name,
/// or report why the two cannot be combined.
///
/// Everything a declaration states about the name is carried over: the members its kind
/// holds (an object's and an interface's fields, an input object's input fields, an enum's
/// values, a union's members), the interfaces it implements and the directives written on
/// it. What both declare identically is held once; what one declares and the other does
/// not is added, in the order the declarations write it, so the composition of a type a
/// single source API declares IS that declaration. A directive both write with DIFFERENT
/// arguments is a disagreement like any other: a type carrying one directive twice is not
/// a type GraphQL holds, so there is nothing to compose.
pub fn combine_type_definitions(
    held: &TypeDefinition,
    incoming: &TypeDefinition,
) -> Result<TypeDefinition, TypeConflict> {
    if held.kind != incoming.kind {
        return Err(TypeConflict::Kind {
            type_name: held.name.clone(),
            held: held.kind,
            incoming: incoming.kind,
        });
    }

    let mut combined = held.clone();
    // A description one of the two states describes the composed type as well; the one
    // already held is kept, so composing does not depend on which merge ran last.
    if combined.description.is_none() {
        combined.description = incoming.description.clone();
    }
    combine_names(&mut combined.interfaces, &incoming.interfaces);
    combine_names(&mut combined.members, &incoming.members);
    combined.directives = combine_members(
        &held.directives,
        &incoming.directives,
        |directive: &DirectiveUse| directive.name.clone(),
        |directive| TypeConflict::Directive {
            type_name: held.name.clone(),
            directive,
        },
    )?;

    // Only the members the kind can hold are composed, so a kind with no body at all
    // (a scalar) composes to its declaration alone.
    match held.kind {
        OBJECT_KIND | INTERFACE_KIND => {
            combined.fields = combine_members(
                &held.fields,
                &incoming.fields,
                |field: &FieldDefinition| field.name.clone(),
                member_conflict(&held.name),
            )?;
        }
        INPUT_OBJECT_KIND => {
            combined.input_fields = combine_members(
                &held.input_fields,
                &incoming.input_fields,
                |input: &InputValueDefinition| input.name.clone(),
                member_conflict(&held.name),
            )?;
        }
        ENUM_KIND => {
            combined.enum_values = combine_members(
                &held.enum_values,
                &incoming.enum_values,
                |value: &EnumValueDefinition| value.name.clone(),
                member_conflict(&held.name),
            )?;
        }
        // A union's members and a scalar's declaration are composed above: neither kind
        // holds a member whose own definition could disagree.
        _ => {}
    }
    Ok(combined)
}

/// The conflict a member of the type `type_name` that the two declare differently is
/// reported as, for whichever of the member kinds that type holds.
fn member_conflict(type_name: &str) -> impl Fn(String) -> TypeConflict + '_ {
    move |member| TypeConflict::Member {
        type_name: type_name.to_string(),
        member,
    }
}

/// Add every name of `incoming` that `held` does not already carry, keeping the order
/// both wrote theirs in.
///
/// The names a declaration lists are a SET in GraphQL — the interfaces a type implements,
/// the members a union is one of — so a name both declarations list is listed once.
fn combine_names(held: &mut Vec<String>, incoming: &[String]) {
    for name in incoming {
        if !held.contains(name) {
            held.push(name.clone());
        }
    }
}

/// Combine the members `incoming` of one type into its members `held`, or report the
/// member the two declare differently as `conflict_of` words it.
///
/// One member name is one member of the composed type: declared identically by both, it
/// is carried once; declared by one alone, it is added; declared differently by the two,
/// there is no composed type that carries both definitions of it.
///
/// The members already held are indexed by name first, so composing two declarations costs
/// one lookup per incoming member rather than a scan of everything held — a document may
/// declare as many members as its parse bound allows
/// ([`super::DEFINITION_TOKEN_LIMIT`]), and a merge runs while the store is locked.
fn combine_members<M: Clone + PartialEq>(
    held: &[M],
    incoming: &[M],
    name_of: impl Fn(&M) -> String,
    conflict_of: impl Fn(String) -> TypeConflict,
) -> Result<Vec<M>, TypeConflict> {
    let mut combined = held.to_vec();
    let mut positions: BTreeMap<String, usize> = combined
        .iter()
        .enumerate()
        .map(|(position, member)| (name_of(member), position))
        .collect();
    for member in incoming {
        let name = name_of(member);
        match positions
            .get(&name)
            .and_then(|position| combined.get(*position))
        {
            Some(held) if held == member => {}
            Some(_) => return Err(conflict_of(name)),
            _ => {
                positions.insert(name, combined.len());
                combined.push(member.clone());
            }
        }
    }
    Ok(combined)
}
