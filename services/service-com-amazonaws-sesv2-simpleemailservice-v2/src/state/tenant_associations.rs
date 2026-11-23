// SPDX-License-Identifier: CC0-1.0

//! The hold a tenant's resource associations put on the resource they name: a resource a
//! tenant still holds an association to cannot be deleted, so the delete of one is
//! refused until every association naming it is removed.
//!
//! Split out of the state module root so the one rule every delete of an associable
//! resource reads lives in one place, with the sentence it is refused with; every item is
//! re-exported from [`crate::state`], so a handler reaches it as `crate::state::…`.

use super::{identity_key, ses_resource_of, ComAmazonawsSesv2SimpleemailserviceV2State};

/// How this service words `BadRequestException` for a delete of a resource a tenant
/// still holds an association to.
///
/// Stated once for every operation that deletes a resource a tenant may hold
/// ([`super::TENANT_RESOURCE_TYPES`]), so the refusal reads the same whichever kind was
/// addressed. The fixed text is `Cannot delete <` + `> because it has tenant
/// associations. Remove all tenant associations and try again.`; the one variable part is
/// the ARN of the resource the call addressed, which identifies the resource whatever
/// spelling the request named it by — the same shape the identity surface already words a
/// delete held back by the certificates an identity signs with.
pub fn resource_has_tenant_associations(resource_arn: &str) -> String {
    format!(
        "Cannot delete <{resource_arn}> because it has tenant associations. Remove all \
         tenant associations and try again."
    )
}

/// The refusal a delete of the resource `resource_arn` names has to answer while a tenant
/// of the account still holds an association to it, and `None` when none does.
///
/// `CreateTenantResourceAssociation` binds a resource to a tenant, and the tenant goes on
/// using it, so the resource cannot be taken out from under it: the delete is refused
/// until the caller removes the associations (`DeleteTenantResourceAssociation`) or
/// deletes the tenants holding them, after which the same delete is accepted. Only a row
/// a LIVE tenant holds counts: `DeleteTenant` removes "its associations with resources",
/// leaving the row as the resource's own record of an association whose tenant is gone
/// ([`super::TenantResourceAssociationRecord::belongs_to`]), and such a row holds nothing
/// back.
///
/// Reporting only: the association table is read and nothing is written, so a refused
/// delete leaves every later caller seeing exactly what it saw.
pub fn tenant_associations_blocking_delete(
    store: &ComAmazonawsSesv2SimpleemailserviceV2State,
    resource_arn: &str,
) -> Option<String> {
    store
        .tenant_resource_associations
        .values()
        .filter(|record| {
            store
                .tenants
                .get(&record.tenant_name)
                .is_some_and(|tenant| record.belongs_to(tenant))
        })
        .any(|record| names_one_resource(&record.resource_arn, resource_arn))
        .then(|| resource_has_tenant_associations(resource_arn))
}

/// Whether two caller- or service-supplied ARNs address ONE resource of this service.
///
/// An association stores the `ResourceArn` as the request spelled it, and a delete mints
/// the ARN of the resource it is about, so the two are compared the way this service
/// resolves an ARN to a record ([`ses_resource_of`]): the resource type picks the kind and
/// the id picks the record within it, with an identity's id compared through
/// [`identity_key`] so a domain written in another case still names the one identity. A
/// value that is no resource ARN of this service names no resource at all, so it matches
/// nothing.
///
/// The kind and the id are the WHOLE comparison because they are what this service keys a
/// resource by — one table per kind, keyed by the name the resource is held under — so two
/// ARNs differing only in the Region or account they spell still name that one record, and
/// an association made against either spelling holds it.
fn names_one_resource(left: &str, right: &str) -> bool {
    let (Some((left_type, left_id)), Some((right_type, right_id))) =
        (ses_resource_of(left), ses_resource_of(right))
    else {
        return false;
    };
    left_type == right_type
        && resource_id_key(left_type, left_id) == resource_id_key(right_type, right_id)
}

/// The id an ARN addresses a resource of `resource_type` by, as the key that type's table
/// holds it under: an email identity resolves through [`identity_key`], every other kind
/// is keyed by the id exactly as it is written.
fn resource_id_key(resource_type: &str, resource_id: &str) -> String {
    if resource_type == "identity" {
        return identity_key(resource_id);
    }
    resource_id.to_string()
}
