// SPDX-License-Identifier: CC0-1.0

//! Which actions a managed permission's policy template may grant for the resource
//! type that permission applies to.
//!
//! `CreatePermission$policyTemplate` and `CreatePermissionVersion$policyTemplate` both
//! document the same rule for the statement's `Action` element: it "specifies the
//! actions that are allowed by this customer managed permission. The list must contain
//! only actions that are supported by the specified resource type." So a template is a
//! policy a managed permission may carry only when EVERY action it names is one this
//! service supports for that resource type; one that is not is refused with
//! `InvalidPolicyException` ([`super::PolicyTemplateRejection::InvalidPolicy`]) worded
//! with [`action_not_allowed_message`].
//!
//! What the supported set is, for a resource type: the permissions this service
//! publishes for its shareable resource types are the record of what it supports for
//! each of them ([`super::AWS_MANAGED_PERMISSIONS`]), so an action any permission
//! published for the type grants is supported for it, whatever that action does to the
//! resource. Past the catalog, the rule the catalog's own entries follow decides: a
//! managed permission grants a principal the USE of a resource another account shared
//! with it — a read of it, a use of it, or the creation, change or removal of something
//! BESIDE it (a table in a shared database, an attachment to a shared transit gateway,
//! an association with a shared service network) — while control over the shared
//! resource itself stays with the account that owns it. So an action that creates,
//! deletes or rewrites a resource of the very type being shared is not supported unless
//! a published permission grants it, and neither is an action of some other service's
//! namespace, which a resource type of this service cannot support at all.
//!
//! A resource type whose published permissions DO grant a write over the shared resource
//! itself therefore has those actions supported through the catalog: extending
//! [`super::AWS_MANAGED_PERMISSIONS`] with a permission is what records that, rather
//! than widening the rule below.

use super::{resource_type_matches, resource_type_service, AWS_MANAGED_PERMISSIONS};

/// The verbs that name control over a resource rather than use of it: an action of one
/// of these verbs applied to the shared resource type's OWN resource code creates,
/// removes or rewrites a resource of that type, which a managed permission grants only
/// where the published catalog says so (see the module documentation).
///
/// Compared without regard to case, and only against a whole action name — an action
/// whose verb is one of these but whose object is something else (`glue:DeleteTable`
/// for a shared `glue:Database`, `ec2:CreateTransitGatewayVpcAttachment` for a shared
/// `ec2:TransitGateway`) names a resource beside the shared one, which the published
/// permissions do grant.
const RESOURCE_CONTROL_VERBS: &[&str] = &["Create", "Delete", "Update", "Modify", "Put"];

/// The sentence this service words a refusal of an action a resource type does not
/// support with.
///
/// Fixed text around one value: the resource type the permission applies to, taken
/// from the request that named it or from the permission a new version is being
/// written for. The code the error class carries on the wire
/// (`InvalidPolicyException`'s `awsQueryError` code is `InvalidPolicy`) opens the
/// sentence, the way [`super::INVALID_POLICY_FORMAT_MESSAGE`] does.
pub fn action_not_allowed_message(resource_type: &str) -> String {
    format!(
        "InvalidPolicy: Input policy containing action that is not allowed for resource type \
         {resource_type}"
    )
}

/// Whether a managed permission for `resource_type` may grant `action`.
///
/// `action` is one entry of a policy statement's `Action` element, in the
/// `<service-code>:<action>` form an IAM policy statement names an action with.
pub fn action_supported_for_resource_type(resource_type: &str, action: &str) -> bool {
    let Some(service) = resource_type_service(resource_type) else {
        return false;
    };
    // An action of another service's namespace is not one a resource type of this
    // service's catalog supports, and neither is a value that names no action at all.
    let Some((namespace, operation)) = action.split_once(':') else {
        return false;
    };
    if operation.is_empty() || !namespace.eq_ignore_ascii_case(service) {
        return false;
    }
    // A wildcard names a set of actions rather than one operation, so there is no one
    // action name to hold against the resource type; it stays with the namespace rule
    // above.
    if operation.contains('*') || operation.contains('?') {
        return true;
    }
    // The published catalog comes first: an action a permission published for this
    // resource type grants is one the type supports, however that action acts on the
    // resource.
    if published_for_resource_type(resource_type, action) {
        return true;
    }
    let resource_code = &resource_type[service.len() + 1..];
    !controls_resource_type(operation, resource_code)
}

/// Whether a permission this service publishes for `resource_type` grants `action`.
///
/// The type is matched the way [`resource_type_matches`] matches one, so the spelling a
/// request wrote finds the entry the catalog publishes under the display spelling of
/// the same type; an action name is matched without regard to case, the way the rule
/// below matches one.
fn published_for_resource_type(resource_type: &str, action: &str) -> bool {
    AWS_MANAGED_PERMISSIONS.iter().any(|entry| {
        resource_type_matches(entry.resource_type, resource_type)
            && entry
                .actions
                .iter()
                .any(|published| published.eq_ignore_ascii_case(action))
    })
}

/// Whether `operation` names control over a resource of the type whose resource code is
/// `resource_code`: one of [`RESOURCE_CONTROL_VERBS`] applied to that code itself,
/// named in the singular or the plural.
fn controls_resource_type(operation: &str, resource_code: &str) -> bool {
    RESOURCE_CONTROL_VERBS.iter().any(|verb| {
        operation
            .get(..verb.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(verb))
            && operation
                .get(verb.len()..)
                .is_some_and(|object| names_resource_code(object, resource_code))
    })
}

/// Whether `object`, the part of an action name after its verb, names the resource code
/// `resource_code` — as the code itself, or as its plural, because a service names the
/// action over several resources of a type in the plural (`ec2:DescribeSubnets` for
/// `ec2:Subnet`).
fn names_resource_code(object: &str, resource_code: &str) -> bool {
    [
        object,
        object.strip_suffix("es").unwrap_or(object),
        object.strip_suffix('s').unwrap_or(object),
    ]
    .iter()
    .any(|candidate| candidate.eq_ignore_ascii_case(resource_code))
}
