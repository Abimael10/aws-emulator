// SPDX-License-Identifier: CC0-1.0

//! What the resource-policy ARN resolution SAYS: the sentence a request naming
//! no policy holder is refused with, and the line an undecided resolution
//! leaves for an operator.
//!
//! The resolution itself lives beside the ARN classification it extends, in
//! [`super::resolve_policy_resource`], so one file holds the whole decision
//! about which ARN names a policy holder — and the only thing split out here is
//! the wording, which the write, the read and the delete all answer with.
//!
//! This file also holds the two other things that are about the DOCUMENT and
//! the resource TYPE rather than about a single operation: which resource type
//! carries a resource policy at all ([`POLICY_RESOURCE_TYPE`]), and the
//! canonical form of a policy document ([`canonical_resource_policy`]).

/// Why a resource-policy request's `ResourceArn` names no resource a policy can
/// be attached to, naming the ARN the request carried.
///
/// The ARN is well formed and names a service whose resources carry an Amazon
/// Redshift resource policy — the value is refused because the resource it
/// identifies is not there, which is a fact about the parameter and not about
/// the policy.
pub fn no_policy_resource_message(resource_arn: &str) -> String {
    format!("Invalid value for ResourceArn: '{resource_arn}'. The resource it names does not exist.")
}

/// Record that the resource a resource-policy request names could not be
/// resolved, so a request admitted because nothing could be asked does not read
/// like one admitted because the resource is really there.
///
/// Visible at default verbosity, for the reason the serverless registration
/// lookup logs the same condition: the degraded path says so rather than passing
/// quietly.
pub(super) fn policy_resource_undecided(resource_arn: &str, reason: &str) {
    ::tracing::warn!(
        resource_arn = %resource_arn,
        reason = %reason,
        "the resource a resource-policy request names could not be resolved"
    );
}

/// The ONE resource type an Amazon Redshift resource policy is attached to: a
/// namespace, the resource that holds the data a policy shares.
///
/// A namespace is the resource type both arms of
/// [`super::POLICY_RESOURCE_SERVICES`] publish under this name — the
/// provisioned control plane as `namespace:<uuid>` ([`super::namespace_arn`])
/// and Redshift Serverless as `namespace/<id>` — and no other type of either
/// service carries a resource policy.
pub const POLICY_RESOURCE_TYPE: &str = "namespace";

/// The resource TYPE an ARN names: the first component of its resource portion,
/// whether that portion is written `<type>:<id>` or `<type>/<id>`.
///
/// Both separators are in use across AWS, and Amazon Redshift itself publishes
/// ARNs in both forms, so the type is read up to whichever comes first. A value
/// whose resource portion is missing names no type and reads as empty.
pub fn arn_resource_type(resource_arn: &str) -> &str {
    let portion = resource_arn.split(':').nth(5).unwrap_or_default();
    portion.split(['/', ':']).next().unwrap_or(portion)
}

/// Why a resource-policy request naming a resource of a type the service's ARN
/// resolver knows, but which carries no resource policy, is refused — naming the
/// type it carried.
///
/// A resource policy is attached to a namespace ([`POLICY_RESOURCE_TYPE`]), so a
/// request naming any other type of this service's resources — a cluster, a
/// snapshot, a parameter group — is refused on the strength of the TYPE alone,
/// before the resource it names is resolved and before any policy is attached,
/// read or detached. The sentence's only variable part is the type read from the
/// request's own `ResourceArn` ([`arn_resource_type`]).
///
/// This is the SECOND of the two refusals an unsupported `ResourceArn` earns:
/// the resource-policy rule proper, which only a type the shared ARN resolver
/// resolves a resource for reaches ([`is_resolvable_arn_resource_type`]).
pub fn unsupported_policy_resource_type_message(resource_type: &str) -> String {
    format!(
        "Resource type {resource_type} isn't supported for the resource policy. Choose another resource type."
    )
}

/// Whether `resource_type` is a resource type the service's shared ARN resolver
/// names a resource of.
///
/// The resolver an operation taking a resource ARN validates that ARN through is
/// the one the tagging surface resolves a `ResourceName` with, so the types it
/// knows are the types a tag attaches to — the catalog behind
/// [`super::resource_type_of_arn_segment`], whose segments are the ARN forms the
/// Cluster Management Guide publishes — together with the namespace a resource
/// policy itself attaches to ([`POLICY_RESOURCE_TYPE`]). The lookup is that ONE
/// segment lookup rather than a second walk of the catalog, so the two cannot
/// drift when a type is added. A segment outside it is one the resolver cannot
/// tell apart from a misspelled ARN — which is what
/// [`unresolvable_arn_resource_type_message`] says.
pub fn is_resolvable_arn_resource_type(resource_type: &str) -> bool {
    resource_type == POLICY_RESOURCE_TYPE
        || super::resource_type_of_arn_segment(resource_type).is_some()
}

/// Why a request carrying an ARN whose resource type the shared ARN resolver
/// resolves nothing for is refused, naming the type segment it read.
///
/// This is the FIRST of the two refusals an unsupported `ResourceArn` earns, and
/// it comes from the resolver rather than from the operation: a segment naming
/// none of the resource types this service resolves an ARN for
/// ([`is_resolvable_arn_resource_type`]) is as likely a malformed ARN as a
/// resource of some other kind, so the refusal states that resources of this
/// kind take none of the resource-level metadata the resolver attaches and
/// points the caller back at the published ARN forms. The one variable part is
/// the type segment read from the request's own ARN ([`arn_resource_type`]), and
/// the sentence says nothing about whether such a resource exists.
pub fn unresolvable_arn_resource_type_message(resource_type: &str) -> String {
    format!(
        "Tagging is not supported for this type of resource: '{resource_type}' \
         (the ARN is potentially malformed, please check the ARN documentation \
         for more information)"
    )
}

/// The policy-document members whose value is either one value or a LIST of
/// values, and which a canonical document therefore writes as the one value
/// when the list holds exactly one.
///
/// Source: the IAM JSON policy grammar, whose `Principal`, `Action` and
/// `Resource` elements (and their `Not…` counterparts) each take "a value or a
/// list of values" — the same reading
/// [`service_common::iam_policy::canonical_document`] renders by. No other
/// element of the grammar has that form.
const SINGLE_OR_LIST_POLICY_MEMBERS: [&str; 6] = [
    "Action",
    "NotAction",
    "Resource",
    "NotResource",
    "Principal",
    "NotPrincipal",
];

/// The policy-document members whose value is a map of CALLER-CHOSEN keys, which
/// a canonical document leaves exactly as it was written.
///
/// A condition block's keys and values are the caller's own vocabulary, not
/// policy-grammar elements, so nothing inside one is rewritten even where a key
/// happens to spell a grammar element.
const OPAQUE_POLICY_MEMBERS: [&str; 1] = ["Condition"];

/// The members a `Principal` map is written with, each of which takes one value
/// or a list of values in the same way the enclosing element does.
///
/// Source: the IAM JSON policy grammar's `Principal` element, written as
/// `{"<type>": "<identifier>"}` or `{"<type>": ["<identifier>", …]}`.
const PRINCIPAL_MEMBERS: [&str; 2] = ["Principal", "NotPrincipal"];

/// The canonical form of one resource-policy document, or `None` when the
/// document is already canonical (or is no JSON document at all).
///
/// The service reports the policy it holds in ITS canonical form rather than
/// the characters a write happened to send: a `Principal`, `Action` or
/// `Resource` written as a one-element list is reported as the single value that
/// list holds, while a list of several values stays a list. Everything else —
/// every other member, the order they were written in, every condition key and
/// value, and every non-ASCII character in any of them — is the document's own
/// and is carried through unchanged, so canonicalizing twice changes nothing.
///
/// The one CLAUSE this shares with the policy rendering
/// [`service_common::iam_policy::canonical_document`] states for the services
/// whose reads show it is that collapse; the rest of that rendering — IAM's own
/// indentation and key separators — is not applied here, because no read of this
/// service reports a layout other than the one the document was written in.
///
/// `None` says there was nothing to canonicalize, which is what lets a document
/// already in canonical form be reported as the very characters it arrived as.
///
/// The walk descends only as deep as the document nests, which the parser's own
/// nesting limit bounds and the length check a write makes before it gets here
/// ([`super::check_resource_policy`]) bounds again — so a document a caller
/// sends cannot drive it without end.
pub fn canonical_resource_policy(policy: &str) -> Option<String> {
    let mut document: serde_json::Value = serde_json::from_str(policy).ok()?;
    if !canonicalize_policy_value(&mut document) {
        return None;
    }
    serde_json::to_string(&document).ok()
}

/// Rewrite every single-or-list policy member of `value` and of the documents
/// nested in it to its canonical form, and report whether anything changed.
fn canonicalize_policy_value(value: &mut serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(members) => {
            let mut changed = false;
            for (name, member) in members.iter_mut() {
                if OPAQUE_POLICY_MEMBERS.contains(&name.as_str()) {
                    continue;
                }
                if !SINGLE_OR_LIST_POLICY_MEMBERS.contains(&name.as_str()) {
                    changed |= canonicalize_policy_value(member);
                    continue;
                }
                changed |= collapse_single_element_list(member);
                // A `Principal` map's members carry identifiers the same way
                // the element itself does, so each of them collapses too.
                if PRINCIPAL_MEMBERS.contains(&name.as_str()) {
                    if let serde_json::Value::Object(by_type) = member {
                        for (_, identifiers) in by_type.iter_mut() {
                            changed |= collapse_single_element_list(identifiers);
                        }
                    }
                }
            }
            changed
        }
        serde_json::Value::Array(items) => items
            .iter_mut()
            .map(canonicalize_policy_value)
            .fold(false, |changed, item| changed | item),
        _ => false,
    }
}

/// Rewrite a one-element list of one value as that value, and report whether it
/// did.
///
/// Only a list of exactly one VALUE collapses: a list of several values is what
/// the document means, and a one-element list holding a structure is not a value
/// a policy element is written as.
fn collapse_single_element_list(value: &mut serde_json::Value) -> bool {
    let serde_json::Value::Array(items) = value else {
        return false;
    };
    let [only] = items.as_slice() else {
        return false;
    };
    if !only.is_string() {
        return false;
    }
    *value = only.clone();
    true
}
