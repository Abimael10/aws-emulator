// SPDX-License-Identifier: CC0-1.0

//! The resource ATTRIBUTES a declaration carries, as the deployment reads them.
//!
//! A `Resources` entry carries, beside its `Properties`, the resource attributes
//! CloudFormation defines on it — the model's `ResourceAttribute` enum lists `CreationPolicy`,
//! `DeletionPolicy`, `Metadata`, `Properties`, `Tags`, `UpdatePolicy` and
//! `UpdateReplacePolicy` — and the two POLICY attributes this service acts on say what becomes
//! of the physical resource: `DeletionPolicy` "when its stack is deleted", and
//! `UpdateReplacePolicy` for the old physical resource a replacement leaves behind.
//!
//! The rule this module implements: such an attribute may be written as an INTRINSIC
//! FUNCTION, and it is then resolved against the stack's conditions, parameters and pseudo
//! parameters exactly as a property is — through the one expression evaluator this crate has
//! ([`super::text`]) — so `{"Fn::If": [<condition>, "Retain", "Delete"]}` resolves per stack
//! and the policy the teardown reads is the one the condition chose. Resolution is part of
//! the PROCESSED stage, the document CloudFormation deploys, so
//! `GetTemplate(TemplateStage=Processed)` serves the resolved literal and the walk that
//! deletes the resource and the document a client reads cannot disagree about it.
//!
//! Only these two attributes are resolved here: they are the ones whose value this service
//! acts on, and each takes one of the three policy words rather than a structure. An
//! attribute whose expression cannot be resolved at all is left exactly as it was written,
//! which is what every other unevaluable expression of a template gets.

use super::*;

/// The resource attribute that says "what to do with the resource when its stack is
/// deleted".
pub(crate) const DELETION_POLICY: &str = "DeletionPolicy";

/// The resource attribute that says how CloudFormation handles the OLD physical resource of
/// a replacement.
pub(crate) const UPDATE_REPLACE_POLICY: &str = "UpdateReplacePolicy";

/// The attributes resolved as expressions: the two whose value this service acts on.
const RESOLVED_ATTRIBUTES: &[&str] = &[DELETION_POLICY, UPDATE_REPLACE_POLICY];

/// The text one resource attribute of `declared` carries, with every intrinsic function in
/// it evaluated against `scope`.
///
/// A value the declaration spells as TEXT is that text; one it spells as an expression is
/// what the expression resolves to. `None` for a declaration that carries no such attribute
/// — the documented default then applies — and for one whose expression this service cannot
/// resolve, which names no policy either.
pub(crate) fn resolved_attribute(
    declared: &DeclaredResource,
    attribute: &str,
    scope: &Scope<'_>,
    budget: &mut Budget,
) -> Option<String> {
    match declared.declaration.get(attribute)? {
        Value::String(stated) => Some(stated.clone()),
        expression => text(expression, scope, 0, budget),
    }
}

/// `body` with every resource attribute that is written as an intrinsic function replaced by
/// the value it resolves to, or `None` when it carries none — in which case the stage this
/// template serves is unchanged, down to the bytes the body was submitted in.
///
/// The document is read and re-serialised by the same reader and writer the transform stage
/// uses ([`super::process_with_parameters`]), so a template written in either spelling
/// resolves alike and the stage is served as JSON, in key order. A document whose resolution
/// would weigh more than that stage's own ceiling is left alone rather than materialised.
pub(crate) fn resolved_resource_attributes(
    body: &str,
    scope: &Scope<'_>,
    budget: &mut Budget,
) -> Option<String> {
    // A body that spells neither attribute carries none to resolve, so it is not read at all:
    // the stage such a template serves is the one it already had, and every deployment goes
    // through here.
    if !RESOLVED_ATTRIBUTES
        .iter()
        .any(|attribute| body.contains(attribute))
    {
        return None;
    }
    let Some(Value::Object(mut root)) = document(body) else {
        return None;
    };
    let mut resolved_any = false;
    let resources = root.get_mut(RESOURCES_SECTION)?.as_object_mut()?;
    for declaration in resources.values_mut() {
        let Some(members) = declaration.as_object_mut() else {
            continue;
        };
        for attribute in RESOLVED_ATTRIBUTES {
            let Some(written) = members.get(*attribute) else {
                continue;
            };
            if written.is_string() {
                continue;
            }
            let Some(resolved) = text(written, scope, 0, budget) else {
                continue;
            };
            members.insert((*attribute).to_string(), Value::String(resolved));
            resolved_any = true;
        }
    }
    if !resolved_any {
        return None;
    }
    ::serde_json::to_string(&in_key_order(Value::Object(root)))
        .ok()
        .filter(|resolved| resolved.len() <= MAX_EXPANDED_BYTES)
}
