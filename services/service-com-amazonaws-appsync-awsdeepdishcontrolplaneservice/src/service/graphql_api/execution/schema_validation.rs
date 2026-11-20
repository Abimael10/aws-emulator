// SPDX-License-Identifier: CC0-1.0

//! The DOCUMENT-VALIDATION stage of the GraphQL data plane's request pipeline: what a
//! request's selection is checked against the schema the API serves for, before any field of
//! it is dispatched.
//!
//! AppSync validates the incoming document against the API's ACTIVE schema first, so a
//! selection the type it is selected from does not declare is a failure of the REQUEST — one
//! GraphQL validation error per such selection, and no `data` at all — rather than a field
//! that resolves to nothing. This module is that reading of the rule; the executor beside it
//! ([`super`]) runs only what this stage admits.
//!
//! What it costs is bounded by the reading that produced the selection: a document deeper
//! than [`MAX_SELECTION_DEPTH`] or wider than [`MAX_SELECTED_NODES`] is refused before it is
//! expanded, so this walk recurses at most that deep, visits at most that many selections,
//! and reports at most that many errors. It is synchronous and takes the service's own store
//! for the length of that bounded walk, so no lock is held across an await.

use super::*;

/// The validation errors one request's document carries against the API's ACTIVE schema:
/// one entry per selected field the type it is selected from does not declare.
///
/// A selection the schema does not declare is reported as GraphQL's own validation error,
/// naming the class of the failure, the field and the type that does not declare it, and the
/// query path it sits at. Empty for an API whose schema declares nothing about the root type
/// the operation selects on: there is no type system to validate the document against.
///
/// The walk descends while the type a field resolves to is an OBJECT type the schema
/// declares. A selection on an INTERFACE or a UNION type is written under a type condition
/// (`... on T`), which the reading splices into the parent's own selections, so the type that
/// declares those fields is no longer the one this walk holds and they are left for the
/// execution rather than reported against the wrong type. The fields GraphQL's INTROSPECTION
/// SYSTEM adds to every schema belong to no type the schema declares, so they are not
/// validated here either.
pub(super) fn schema_validation_errors(
    context: &PlaneContext,
    root_type: &str,
    fields: &[SelectedField],
) -> Vec<Json> {
    let store = context.state.lock();
    let Some(types) = store
        .graphql_apis
        .get(&context.api_id)
        .map(|api| &api.types)
        .filter(|types| types.contains_key(root_type))
    else {
        return Vec::new();
    };
    let mut errors = Vec::new();
    undefined_fields(types, root_type, fields, &mut Vec::new(), &mut errors);
    errors
}

/// Report every field of `fields` that `type_name` does not declare, and walk the
/// sub-selection of each one it does against the type that field resolves to.
///
/// `at` is the query path the walk stands at, which each error names the selection by. The
/// descent is bounded by the reading that produced `fields`: a selection set deeper than
/// [`MAX_SELECTION_DEPTH`] is refused before it is expanded, so this recursion is bounded
/// by the same depth, and the number of errors by the nodes one document may expand to.
pub(super) fn undefined_fields(
    types: &BTreeMap<String, crate::state::TypeRecord>,
    type_name: &str,
    fields: &[SelectedField],
    at: &mut Vec<String>,
    errors: &mut Vec<Json>,
) {
    let Some(record) = types.get(type_name) else {
        return;
    };
    if record.parsed.kind != crate::state::OBJECT_KIND {
        return;
    }
    for field in fields {
        if crate::service::graphql_api::introspection::is_introspection_field(&field.name) {
            continue;
        }
        at.push(field.name.clone());
        match record
            .parsed
            .fields
            .iter()
            .find(|declared| declared.name == field.name)
        {
            None => errors.push(json!({
                "message": undefined_field_message(&field.name, type_name, at),
            })),
            Some(declared) if !field.selections.is_empty() => {
                let child = declared
                    .value_type
                    .as_ref()
                    .map(named_type)
                    .unwrap_or_default();
                undefined_fields(types, &child, &field.selections, at, errors);
            }
            Some(_) => {}
        }
        at.pop();
    }
}

/// The sentence a validation failure of the `FieldUndefined` class states: the field, the
/// type that does not declare it, and the query path the selection sits at.
pub(super) fn undefined_field_message(field_name: &str, type_name: &str, at: &[String]) -> String {
    format!(
        "{VALIDATION_ERROR} {FIELD_UNDEFINED}: Field '{field_name}' in type '{type_name}' \
         is undefined @ '{}'",
        at.join(QUERY_PATH_SEPARATOR)
    )
}

/// How a GraphQL validation failure is stated: the fixed opening of the sentence, the name
/// of the class of failure it is, and the separator the query path it was found at is
/// written with.
pub(super) const VALIDATION_ERROR: &str = "Validation error of type";
pub(super) const FIELD_UNDEFINED: &str = "FieldUndefined";
pub(super) const QUERY_PATH_SEPARATOR: &str = "/";
