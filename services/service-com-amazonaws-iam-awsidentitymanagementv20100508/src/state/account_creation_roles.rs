// SPDX-License-Identifier: CC0-1.0

//! The service-linked roles an account HOLDS from the moment it is created.
//!
//! An Amazon Web Services account does not come into existence with an empty set of IAM
//! roles: AWS itself creates the service-linked roles its account-wide tooling needs
//! before the account has made a single request, so the first role enumeration an account
//! ever answers already reports them. Two services create theirs that way — AWS Support
//! ("AWS Support creates the AWSServiceRoleForSupport role when you create your Amazon Web
//! Services account", "Using service-linked roles for AWS Support", AWS Support User Guide)
//! and Trusted Advisor ("AWSServiceRoleForTrustedAdvisor … is created when you create your
//! Amazon Web Services account", "Using service-linked roles for Trusted Advisor", AWS
//! Support User Guide) — which is why `iam list-roles` in a brand-new account reports those
//! two roles and nothing else.
//!
//! They are held as the account's OWN records, in the same collections a created role lands
//! in, so every reader of the account's roles — the listings, the account summary, the
//! authorization-details report, a role's attached policies — reports them from the one
//! place the account's roles live rather than from a second derivation beside it. Each
//! record is the one [`service_provisioned_role`] derives for that name, so a role AWS
//! provisioned reads the same whether the account holds it or a service keeps it
//! provisioned: a stable id keyed to the catalog row and the fixed `CreateDate` a record
//! nothing in this account created carries.

#[allow(unused_imports)]
use super::*;

/// The service principals whose service-linked role AWS creates WITH the account, each a
/// row of [`PUBLISHED_SERVICE_LINKED_ROLE_NAMES`] — the catalog that names the role AWS
/// publishes for the principal and, through [`service_linked_role_policy`], the policy that
/// service controls it through.
///
/// Source: the "Using service-linked roles" page of each of those two services' own user
/// guide, which says the role is created when the account is created rather than on first
/// use of the service. A service whose role is created when its own first resource is
/// created is NOT here: an account that has never used that service holds no record of its
/// role, and a client reading such a name back is answered by the record the owning service
/// keeps provisioned ([`service_provisioned_role`]).
pub const ACCOUNT_CREATION_SERVICE_LINKED_ROLE_PRINCIPALS: &[&str] =
    &["support.amazonaws.com", "trustedadvisor.amazonaws.com"];

/// The names AWS publishes for the roles it creates with the account
/// ([`ACCOUNT_CREATION_SERVICE_LINKED_ROLE_PRINCIPALS`]), in the order the account
/// enumerates its roles: ascending by the case-folded name, which is the key its role store
/// is ordered on.
///
/// The one derivation of that set, so a reader of the account's roles and anything checking
/// what it reports resolve the same names from the same catalogs.
pub fn account_creation_role_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = ACCOUNT_CREATION_SERVICE_LINKED_ROLE_PRINCIPALS
        .iter()
        .filter_map(|principal| published_service_linked_role_name(principal))
        .collect();
    names.sort_by_key(|name| name_key(name));
    names
}

/// The roles AWS provisions WITH the account, as the account holds them: the record of each
/// principal in [`ACCOUNT_CREATION_SERVICE_LINKED_ROLE_PRINCIPALS`], under the name AWS
/// publishes for it, carrying the policy that service controls the role through.
///
/// Called once, by [`ComAmazonawsIamAwsidentitymanagementv20100508State::default`], on the
/// account with nothing in it; the `RoleId` of each record comes from the catalog row rather
/// than from this store's counter, so holding them leaves the id a created role is minted
/// next exactly where it was.
pub(crate) fn hold_account_creation_roles(
    account: &mut ComAmazonawsIamAwsidentitymanagementv20100508State,
) {
    for service_principal in ACCOUNT_CREATION_SERVICE_LINKED_ROLE_PRINCIPALS {
        // Both the role and its policy are resolved against the published catalogs rather
        // than spelled out again here, so a row of either one cannot drift from the record
        // a read of that role reports.
        let Some(published_name) = published_service_linked_role_name(service_principal) else {
            continue;
        };
        let Some(role) = service_provisioned_role(published_name) else {
            continue;
        };
        account.roles.insert(name_key(published_name), role);
        // "The service controls the attached policies" (`CreateServiceLinkedRole`): the role
        // AWS created for the service carries the policy AWS files for that service, the
        // same attachment a role created for the principal comes into existence with
        // ([`insert_service_linked_role`]).
        if let Some(policy) = service_linked_role_policy(service_principal) {
            account
                .role_attached_policies
                .entry(name_key(published_name))
                .or_default()
                .insert(policy.arn());
        }
    }
}
