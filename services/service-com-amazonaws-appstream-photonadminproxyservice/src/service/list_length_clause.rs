// SPDX-License-Identifier: CC0-1.0

//! How a LIST of modelled STRUCTURES is quoted in the clause refusing it for its modelled
//! `@length`.
//!
//! A constraint clause names the value that was rejected, the member path it sat at and the
//! requirement it failed. A `@length` on a list is checked ON THE MEMBER, before the
//! validating walk descends into the elements, so the only rendering the framework has for
//! the rejected value there is a SUMMARY of its size — `<n> items`
//! (`vendor/smithy-schema-server/src/constraints.rs`) — which is not a value any caller
//! sent and not a rendering AWS emits. AWS quotes the LIST: the elements in order,
//! comma-separated, in brackets.
//!
//! What one element renders as is a property of the layer that prints it. The validation
//! layer in front of this service prints a modelled structure as its CLASS and object
//! identity ([`printed_element`]) rather than over the members it holds, so a clause about a
//! list of modelled structures carries one rendering per element and nothing of any
//! element's own content: the list's LENGTH and the shape its elements are is the whole of
//! it. Which shape that is, per list member, is [`ELEMENT_SHAPES`].
//!
//! Two kinds of list are stated here, and they are the two this seam can state from the
//! violation alone:
//!
//! * an EMPTY list (the `@length` minimum of a list a request carried no element in)
//!   renders as `[]` whatever its elements would have been;
//! * a list of modelled STRUCTURES renders as one printed element per element the request
//!   carried, because that rendering is the element's class and identity and not its value.
//!
//! A non-empty list of SCALARS renders as the elements' own text, which no violation
//! carries: it is read from the REQUEST instead, one layer up
//! ([`super::refusal_wording`], whose per-operation wrapper collects it through
//! `service_common::validation::submitted_lists`), and this layer answers `None` for it.
//!
//! Everything else about the refusal stays where it already was: the requirement, the member
//! path, the clause around them, the envelope over the clauses and the status come from the
//! framework and from [`super::refusal_wording`].

use ::smithy_server_runtime::{Violation, ViolationKind};

/// The package the layer in front of this service prints a modelled shape's class from.
///
/// It is AppStream's own internal name for the service (the model's own service shape is
/// `PhotonAdminProxyService`), not the `com.amazonaws.appstream` namespace the public model
/// declares its shapes in.
const PRINTED_CLASS_PACKAGE: &str = "com.amazonaws.photon";

/// The greatest number of elements one clause quotes.
///
/// A list a clause quotes is one the MODEL bounds, so the elements a clause ever has to
/// state are the declared maximum plus the few past it that make the list over its bound;
/// this ceiling is far above every such maximum this service declares
/// (`tests::every_bounded_structure_list_is_stated` asserts that against the model), so
/// every list the bound is about is quoted in full. Past it the framework's own count
/// summary stands, which is the fail-safe direction and what keeps a request from sizing
/// the message it is refused with: a request may carry any number of elements, and each one
/// renders to more text than it occupied.
const MAX_QUOTED_ELEMENTS: usize = 256;

/// Every list member of this service whose elements are modelled STRUCTURES and whose list
/// shape declares a `@length` MAXIMUM — the only lists a clause can be asked to quote with
/// elements in it — as `(member, element shape)`.
///
/// The member is spelled the way a violation path spells one segment (lowerCamel,
/// [`::aws_smithy_schema_server::constraints::path`]) and the shape is the simple name of
/// the shape one element is, which is what the printed class names.
///
/// Derived from the Smithy model that ships beside this crate rather than from the members
/// one recording exercises: `tests::every_bounded_structure_list_is_stated` rebuilds this
/// table from that model and fails when a member is missing from it, carries the wrong
/// element shape, or names a member the model does not declare.
const ELEMENT_SHAPES: &[(&str, &str)] = &[
    ("accessEndpoints", "AccessEndpoint"),
    (
        "appBlockBuilderAppBlockAssociations",
        "AppBlockBuilderAppBlockAssociation",
    ),
    ("appCatalogConfig", "ApplicationConfig"),
    ("applicationFleetAssociations", "ApplicationFleetAssociation"),
    ("userStackAssociations", "UserStackAssociation"),
];

/// The list `violation` is about, as a clause quotes it, or `None` when `violation` is not a
/// list-length violation this layer can state the list of.
///
/// The summary the walk stored is the only signal the violation carries about the shape that
/// broke the bound (`service_common::validation_message::list_item_count` is the shared
/// reading of it), so a string member whose own value happens to read as one is the accepted
/// collision that reading already documents.
pub(super) fn quoted_list(violation: &Violation) -> Option<String> {
    if !matches!(
        violation.kind,
        ViolationKind::MinLength(_) | ViolationKind::MaxLength(_)
    ) {
        return None;
    }
    let elements =
        ::service_common::validation_message::list_item_count(violation.value.as_deref()?)?;
    if elements == 0 {
        return Some(::service_common::validation::rendered_list(&[]));
    }
    if elements > MAX_QUOTED_ELEMENTS {
        return None;
    }
    let shape = element_shape(&violation.path)?;
    let printed: Vec<String> = (1..=elements)
        .map(|position| printed_element(shape, &violation.path, position))
        .collect();
    let printed: Vec<&str> = printed.iter().map(String::as_str).collect();
    Some(::service_common::validation::rendered_list(&printed))
}

/// The shape the elements of the list member at `path` are, when they are modelled
/// structures this layer knows the printed class of ([`ELEMENT_SHAPES`]).
///
/// Looked up by the LAST segment of the path, which is the member the clause is about: a
/// list declared on a nested structure is reported under the member chain that reaches it,
/// and the member is what names the element shape.
fn element_shape(path: &str) -> Option<&'static str> {
    let member = path.rsplit('.').next()?;
    ELEMENT_SHAPES
        .iter()
        .find(|(declared, _)| *declared == member)
        .map(|(_, shape)| *shape)
}

/// One element of a list of modelled structures, as the layer in front of this service
/// prints it: the element's class, then the identity of the object the layer held it in.
///
/// The printed form carries nothing of the element's own value — a modelled shape that
/// states no rendering of itself prints as its class and its object identity — so the
/// element's content is not recoverable from it and is not part of it. The identity is a
/// handle on an object of ONE run: it is stated here from the member and the position the
/// element sat at, so one request is answered the same way twice and nothing of the
/// request's own values reaches it.
fn printed_element(shape: &str, path: &str, position: usize) -> String {
    let identity =
        ::service_common::validation_message::string_hash(&format!("{path}.{position}"));
    format!("{PRINTED_CLASS_PACKAGE}.{shape}@{identity:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This service's Smithy model, as it ships beside this crate. Read at test time rather
    /// than embedded: it is megabytes of JSON that no non-test build needs.
    const MODEL_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/smithy-model/com-amazonaws-appstream-photonadminproxyservice.json"
    );

    /// One violation, as the validating walk records one.
    fn violation(path: &str, kind: ViolationKind, value: Option<&str>) -> Violation {
        Violation {
            path: path.to_owned(),
            kind,
            value: value.map(str::to_owned),
            sensitive: false,
        }
    }

    /// The simple name of a Smithy shape id (`com.amazonaws.appstream#AccessEndpoint` ->
    /// `AccessEndpoint`).
    fn simple_name(shape_id: &str) -> &str {
        shape_id.rsplit('#').next().unwrap_or(shape_id)
    }

    /// Every `(member, element shape, declared maximum)` the MODEL declares a bounded
    /// structure list at: a structure member whose target is a list with a `@length` maximum
    /// whose own member targets a structure.
    fn bounded_structure_lists(
        model: &serde_json::Value,
    ) -> std::collections::BTreeSet<(String, String, i64)> {
        let shapes = model["shapes"]
            .as_object()
            .expect("the model declares shapes");
        let mut stated = std::collections::BTreeSet::new();
        for shape in shapes.values() {
            if shape["type"] != "structure" {
                continue;
            }
            let Some(members) = shape["members"].as_object() else {
                continue;
            };
            for (member, declared) in members {
                let Some(target) = declared["target"].as_str() else {
                    continue;
                };
                let Some(list) = shapes.get(target) else {
                    continue;
                };
                if list["type"] != "list" {
                    continue;
                }
                let Some(maximum) = list["traits"]["smithy.api#length"]["max"].as_i64() else {
                    continue;
                };
                let Some(element) = list["member"]["target"].as_str() else {
                    continue;
                };
                if shapes
                    .get(element)
                    .is_none_or(|element| element["type"] != "structure")
                {
                    continue;
                }
                stated.insert((
                    ::aws_smithy_schema_server::constraints::path::member(member),
                    simple_name(element).to_owned(),
                    maximum,
                ));
            }
        }
        stated
    }

    /// RULE applied to the whole model: every bounded list of modelled structures this
    /// service declares has its element shape stated, nothing else is, and the ceiling one
    /// clause quotes up to is above every maximum those lists declare — so a clause about a
    /// list the model bounds quotes it in full.
    ///
    /// The pairs come from the MODEL rather than from a second hand list, so a member added
    /// to the model fails here instead of silently answering a clause with the framework's
    /// count summary in it.
    #[test]
    fn every_bounded_structure_list_is_stated() {
        let model = std::fs::read_to_string(MODEL_PATH)
            .expect("the model that generated this crate ships beside it");
        let model: serde_json::Value =
            serde_json::from_str(&model).expect("the model that generated this crate is JSON");
        let declared = bounded_structure_lists(&model);
        let stated: std::collections::BTreeSet<(String, String)> = ELEMENT_SHAPES
            .iter()
            .map(|(member, shape)| ((*member).to_owned(), (*shape).to_owned()))
            .collect();
        let members: std::collections::BTreeSet<(String, String)> = declared
            .iter()
            .map(|(member, shape, _)| (member.clone(), shape.clone()))
            .collect();
        assert_eq!(
            stated, members,
            "case \"the model's bounded structure lists\": expected {members:?}, got \
             {stated:?}"
        );
        for (member, shape, maximum) in &declared {
            assert!(
                i64::try_from(MAX_QUOTED_ELEMENTS).is_ok_and(|ceiling| ceiling > *maximum),
                "case {member:?}: the ceiling {MAX_QUOTED_ELEMENTS} must be above the \
                 declared maximum {maximum} of a list of {shape}"
            );
        }
    }

    /// RULE: a list-length violation this layer can state quotes the list the request
    /// carried — `[]` for an empty one and one printed element per element otherwise — and
    /// every other violation is left for the layer above to state.
    ///
    /// The rows differ along what the rule generalises over: the bound that was broken
    /// (minimum, maximum), the element count, a count past the ceiling one clause quotes up
    /// to, a list member this layer states no element shape for, and the constraint class of
    /// a clause that is not a list's length at all.
    #[test]
    fn a_list_length_clause_quotes_the_list_and_nothing_else_changes() {
        let past_the_ceiling = format!("{} items", MAX_QUOTED_ELEMENTS + 1);
        let rows = [
            (
                "a bounded structure list one element over its maximum",
                violation("accessEndpoints", ViolationKind::MaxLength(4), Some("5 items")),
                Some(5usize),
            ),
            (
                "the same list far over its maximum",
                violation("accessEndpoints", ViolationKind::MaxLength(4), Some("9 items")),
                Some(9),
            ),
            (
                "a list the request carried no element in",
                violation("userSettings", ViolationKind::MinLength(1), Some("0 items")),
                Some(0),
            ),
            (
                "a member outside its pattern",
                violation(
                    "name",
                    ViolationKind::Pattern("^[a-z]+$".to_owned()),
                    Some("NOPE"),
                ),
                None,
            ),
            (
                "a string past its length maximum",
                violation("description", ViolationKind::MaxLength(256), Some("xx")),
                None,
            ),
            (
                "a map over its entry maximum",
                violation("tags", ViolationKind::MaxLength(50), Some("51 entries")),
                None,
            ),
            (
                "a list past the ceiling one clause quotes",
                violation(
                    "accessEndpoints",
                    ViolationKind::MaxLength(4),
                    Some(past_the_ceiling.as_str()),
                ),
                None,
            ),
            (
                "a list of scalars, whose elements this layer cannot state",
                violation("embedHostDomains", ViolationKind::MaxLength(20), Some("21 items")),
                None,
            ),
        ];
        for (case, offence, quoted) in rows {
            let stated = quoted_list(&offence);
            let Some(elements) = quoted else {
                assert!(
                    stated.is_none(),
                    "case {case:?}: expected no quoted list, got {stated:?}"
                );
                continue;
            };
            let value = stated
                .unwrap_or_else(|| panic!("case {case:?}: a list is quoted"));
            let inner = value
                .strip_prefix('[')
                .and_then(|value| value.strip_suffix(']'))
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a quoted list is bracketed, got {value:?}")
                });
            let printed: Vec<&str> = if inner.is_empty() {
                Vec::new()
            } else {
                inner.split(", ").collect()
            };
            assert_eq!(
                printed.len(),
                elements,
                "case {case:?}: expected {elements} printed elements, got {value:?}"
            );
            for element in printed {
                assert!(
                    element.starts_with(&format!("{PRINTED_CLASS_PACKAGE}.")),
                    "case {case:?}: expected a printed class, got {element:?}"
                );
            }
        }
    }
}
