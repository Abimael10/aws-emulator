// SPDX-License-Identifier: CC0-1.0

//! The access control the SERVICE provisions, as opposed to the access control a
//! customer creates.
//!
//! `AccessProfileType` and `AccessGrantType` each declare SERVICE_MANAGED — "Provisioned
//! and owned by the service. Cannot be deleted by customers." — beside CUSTOMER_MANAGED,
//! "Created and owned by a customer". So the model states that rows of both kinds live in
//! the same collections and that the two kinds answer a customer call differently. This
//! module is the one place that says WHICH rows the service provisions, and what a call
//! that would change one of them is answered with:
//!
//! * creating a space provisions that space's own access control — the access profile the
//!   space's asynchronous workflows run under, the grant that makes the account's root
//!   principal the space's administrator, and the grant that lets the space's agent
//!   principal assume that profile — into the SAME collections `ListAccessProfiles`,
//!   `ListAccessGrants`, `GetAccessProfile` and `GetAccessGrant` read, so a brand-new
//!   space reports the access control it was provisioned with rather than nothing;
//! * a customer call that would update or delete one of those rows is refused with the
//!   modelled `ConflictException` (HTTP 409), and the row is left exactly as it was;
//! * deleting the space releases them, because they were provisioned WITH the space and
//!   nothing can address them once it is gone.
//!
//! The names of the provisioned rows are fixed text; everything else about them is read
//! from the space they belong to — its id, its domain, its region and its owning account
//! — so two spaces never report the same ids, ARNs or principals.
//!
//! BOUNDED: one profile and two grants per live space, which [`super::SPACE_QUOTA`]
//! bounds, and they are released with the space.

use std::collections::HashMap;

use crate::ops::types::{
    AccessGrant, AccessGrantPrincipal, AccessProfile, ResourceScope, ScopedActions, Space,
};

/// `AccessProfileType`'s value for a profile the service provisions and owns. Its model
/// doc: "Provisioned and owned by the service. Cannot be deleted by customers."
pub const ACCESS_PROFILE_TYPE_SERVICE_MANAGED: &str = "SERVICE_MANAGED";

/// `AccessGrantType`'s value for a grant the service provisions and owns, documented with
/// the same sentence as the profile type's.
pub const ACCESS_GRANT_TYPE_SERVICE_MANAGED: &str = "SERVICE_MANAGED";

/// The name of the access profile a space's asynchronous workflows run under, which space
/// creation provisions. Fixed text: it names a role in the space rather than anything the
/// request carried.
pub const ASYNC_WORKFLOWS_PROFILE_NAME: &str = "DefaultAccessProfileForAsyncWorkflows";

/// The name of the grant that makes the account that owns a space its administrator,
/// which space creation provisions. Fixed text, for the same reason.
pub const SPACE_ADMIN_GRANT_NAME: &str = "OmniDefaults-Space-Admin";

/// The name of the grant that lets a space's agent principal assume
/// [`ASYNC_WORKFLOWS_PROFILE_NAME`], which space creation provisions. Fixed text, for the
/// same reason.
pub const AGENT_ASSUME_GRANT_NAME: &str = "OmniDefaults-AsyncAccessProfile-AgentCanAssume";

/// What a space's agent principal id begins with, before the space's own id.
pub const AGENT_PRINCIPAL_ID_PREFIX: &str = "agent-";

/// `AccessGrantPermission`'s value for the space administrator — its model doc: "Can
/// manage grants within the space but cannot create spaces."
pub const ACCESS_GRANT_PERMISSION_SPACE_ADMIN: &str = "SPACE_ADMIN";

/// `AccessGrantPrincipalType`'s value for the account root principal — its model doc: "An
/// AWS account root principal."
pub const ACCESS_GRANT_PRINCIPAL_TYPE_IAM_ROOT: &str = "IAM_ROOT";

/// `AccessGrantPrincipalType`'s value for an agent workload — its model doc: "An agent
/// workload principal." It is the principal a space's asynchronous workflows run as.
pub const ACCESS_GRANT_PRINCIPAL_TYPE_AGENT: &str = "AGENT";

/// How a refusal to delete a row the service owns names the conflict — the value this
/// crate's other "cannot be deleted" conflict already carried, kept so the member does
/// not change wording with this fix. `ConflictException.conflictType`'s model doc is
/// "The type of conflict that caused the request to fail. Not always present."; the
/// model fixes no value set for it, so this is the crate's own vocabulary.
pub const CONFLICT_TYPE_NOT_DELETABLE: &str = "NOT_DELETABLE";

/// How a refusal to update a row the service owns names the conflict, in the same
/// vocabulary.
pub const CONFLICT_TYPE_NOT_UPDATABLE: &str = "NOT_UPDATABLE";

/// The refusal an `UpdateAccessProfile` of a service-provisioned profile earns.
///
/// Wholly fixed text: the condition is the profile's own ownership, which no part of the
/// request varies, so the sentence names neither the profile nor the space.
pub const SERVICE_MANAGED_PROFILE_NOT_UPDATABLE: &str =
    "A system-managed access profile cannot be updated";

/// The refusal a `DeleteAccessProfile` of a service-provisioned profile earns, wholly
/// fixed text for the same reason.
pub const SERVICE_MANAGED_PROFILE_NOT_DELETABLE: &str =
    "A system-managed access profile cannot be deleted";

/// The refusal a `DeleteAccessGrant` of a service-provisioned grant earns, wholly fixed
/// text for the same reason. `DeleteAccessGrant`'s own model doc states the rule: "A
/// service-managed grant cannot be deleted."
pub const SERVICE_MANAGED_GRANT_NOT_DELETABLE: &str = "A system-managed grant cannot be deleted";

/// The agent principal of the space `space_id` — the principal a space's asynchronous
/// workflows run as, which the provisioned grant authorizes.
///
/// Derived from the space's own id, so the principal of one space authorizes nothing in
/// another. It satisfies `PrincipalId`'s `^[a-zA-Z0-9][a-zA-Z0-9:/_+=,.@-]*$`: the prefix
/// is alphanumeric and a `SpaceId` is hex digits and hyphens.
pub fn agent_principal_id(space_id: &str) -> String {
    format!("{AGENT_PRINCIPAL_ID_PREFIX}{space_id}")
}

/// The principal a service-provisioned row reports as its `createdBy`.
///
/// `AccessGrant.createdBy` is `@required` and reads "The principal that created the
/// grant"; nobody called to create these rows, so the principal is this service itself,
/// named by its own service principal — this service's `awsService.endpointPrefix`
/// (`smithy_query(query="service_info")`) under the AWS service-principal domain.
pub fn service_principal() -> String {
    format!("{}.amazonaws.com", super::ENDPOINT_PREFIX)
}

/// Whether `profile` is one the service provisioned and owns.
///
/// Read from the record's own `profileType` rather than from its name, so a row is
/// classified by who manages it.
pub fn is_service_managed_profile(profile: &AccessProfile) -> bool {
    profile.profile_type.as_deref() == Some(ACCESS_PROFILE_TYPE_SERVICE_MANAGED)
}

/// Whether `grant` is one the service provisioned and owns, read from its own
/// `grantType` for the same reason.
pub fn is_service_managed_grant(grant: &AccessGrant) -> bool {
    grant.grant_type == ACCESS_GRANT_TYPE_SERVICE_MANAGED
}

/// The `ConflictException` a call that would change a service-provisioned row earns, with
/// `message` as the caller reads it.
///
/// One constructor for all three refusals: the conflict is the same one — the row is the
/// service's — so the envelope around the message is the same too, naming the row the
/// request addressed, its resource type and which change was refused.
pub fn service_managed_conflict(
    conflict_type: &str,
    message: &str,
    resource_id: &str,
    resource_type: &str,
) -> crate::ops::types::ConflictException {
    crate::ops::types::ConflictException {
        message: message.to_string(),
        conflict_type: Some(conflict_type.to_string()),
        resource_id: Some(resource_id.to_string()),
        resource_type: Some(resource_type.to_string()),
        error_code: None,
    }
}

impl super::ComAmazonawsCloudwatchomniCloudwatchomnifrontendState {
    /// Provision `space`'s own access control, as creating the space does.
    ///
    /// Writes the one SERVICE_MANAGED access profile and the two SERVICE_MANAGED grants
    /// into the same collections every reader of a profile or a grant reads, so the rows
    /// are the store's rather than a second copy: each carries a minted id and the ARN,
    /// region, account, space and domain of the space it belongs to.
    ///
    /// The agent grant is a CUSTOM one — `AccessGrantPermission` CUSTOM is "Access limited
    /// to an explicit list of actions, specified in scopedActions" — whose one group names
    /// the action assuming an access profile is and scopes it to the provisioned profile's
    /// ARN alone. That is also what records the grant against the profile, so the profile
    /// the agent may assume is the one the grant names and no other profile of the space
    /// becomes assumable through it.
    pub fn provision_space_access_control(&mut self, space: &Space, domain_id: &str) {
        let region = space.region.as_str();
        let at = super::now();
        let owner = service_principal();

        let profile_id = super::mint_profile_id();
        let profile_arn = super::access_profile_arn(region, &profile_id);
        let admin_grant_id = super::mint_grant_id();
        let agent_grant_id = super::mint_grant_id();

        let admin_grant = AccessGrant {
            grant_id: admin_grant_id.clone(),
            grant_arn: super::access_grant_arn(region, &admin_grant_id),
            name: Some(SPACE_ADMIN_GRANT_NAME.to_string()),
            account_id: space.owner_account_id.clone(),
            domain_id: domain_id.to_string(),
            space_id: space.space_id.clone(),
            // The administrator of a new space is the account that owns it: the grant
            // outlives whichever identity called, so it names the account root rather
            // than that caller.
            principal: super::reported_principal(AccessGrantPrincipal {
                principal_type: ACCESS_GRANT_PRINCIPAL_TYPE_IAM_ROOT.to_string(),
                principal_id: Some(space.owner_account_id.clone()),
                principal_attributes: None,
            }),
            permission: ACCESS_GRANT_PERMISSION_SPACE_ADMIN.to_string(),
            grant_type: ACCESS_GRANT_TYPE_SERVICE_MANAGED.to_string(),
            created_by: owner.clone(),
            created_at: at,
            updated_at: at,
            scoped_actions: None,
        };

        let agent_grant = AccessGrant {
            grant_id: agent_grant_id.clone(),
            grant_arn: super::access_grant_arn(region, &agent_grant_id),
            name: Some(AGENT_ASSUME_GRANT_NAME.to_string()),
            account_id: space.owner_account_id.clone(),
            domain_id: domain_id.to_string(),
            space_id: space.space_id.clone(),
            principal: super::reported_principal(AccessGrantPrincipal {
                principal_type: ACCESS_GRANT_PRINCIPAL_TYPE_AGENT.to_string(),
                principal_id: Some(agent_principal_id(&space.space_id)),
                principal_attributes: None,
            }),
            permission: super::ACCESS_GRANT_PERMISSION_CUSTOM.to_string(),
            grant_type: ACCESS_GRANT_TYPE_SERVICE_MANAGED.to_string(),
            created_by: owner.clone(),
            created_at: at,
            updated_at: at,
            scoped_actions: Some(vec![ScopedActions {
                actions: vec![super::ASSUME_ACCESS_PROFILE_ACTION.to_string()],
                resources: Some(vec![ResourceScope {
                    resource_type: super::ACCESS_PROFILE_RESOURCE_TYPE.to_string(),
                    resource_arns: Some(vec![profile_arn.clone()]),
                    tags: None,
                    signal_types: None,
                    row_scope_groups: None,
                }]),
                context_conditions: None,
            }]),
        };

        let profile_seq = self.next_seq();
        self.access_profiles.insert(
            profile_id.clone(),
            super::StoredAccessProfile {
                seq: profile_seq,
                profile: AccessProfile {
                    arn: profile_arn,
                    space_id: space.space_id.clone(),
                    name: ASYNC_WORKFLOWS_PROFILE_NAME.to_string(),
                    // The recorded profile carries no description, and the service does
                    // not invent one for it.
                    description: None,
                    created_at: at,
                    updated_at: at,
                    // Reported per caller, as for every profile.
                    assume_status: None,
                    profile_type: Some(ACCESS_PROFILE_TYPE_SERVICE_MANAGED.to_string()),
                    profile_id: profile_id.clone(),
                },
                created_by: owner,
                // The agent grant is the trust recorded against this profile.
                assume_grant_ids: vec![agent_grant_id.clone()],
                tags: HashMap::new(),
            },
        );
        for grant in [admin_grant, agent_grant] {
            let seq = self.next_seq();
            self.access_grants.insert(
                grant.grant_id.clone(),
                super::StoredAccessGrant {
                    seq,
                    grant,
                    tags: HashMap::new(),
                },
            );
        }
    }

    /// Release the access control the space `space_id` was provisioned with, as deleting
    /// the space does.
    ///
    /// Only the rows the SERVICE provisioned go: a customer-created profile or grant has
    /// its own lifecycle operations and is not this store's to remove. A released grant is
    /// also no longer recorded against any profile, the same way deleting a grant through
    /// the API withdraws the trust it recorded.
    pub fn release_space_access_control(&mut self, space_id: &str) {
        let released_grants: ::std::collections::HashSet<String> = self
            .access_grants
            .values()
            .filter(|stored| {
                stored.grant.space_id == space_id && is_service_managed_grant(&stored.grant)
            })
            .map(|stored| stored.grant.grant_id.clone())
            .collect();
        let released_profiles: Vec<String> = self
            .access_profiles
            .values()
            .filter(|stored| {
                stored.profile.space_id == space_id && is_service_managed_profile(&stored.profile)
            })
            .map(|stored| stored.profile.profile_id.clone())
            .collect();

        for grant_id in &released_grants {
            self.access_grants.remove(grant_id);
        }
        for profile_id in &released_profiles {
            self.access_profiles.remove(profile_id);
        }
        for profile in self.access_profiles.values_mut() {
            profile
                .assume_grant_ids
                .retain(|grant_id| !released_grants.contains(grant_id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_principal_id_is_the_prefix_and_the_space_s_own_id() {
        // Rows span the `SpaceId` class the id is built from: the model's own pattern
        // (`^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`) at its
        // lowest and highest digits, plus one mixed value.
        for space_id in [
            "00000000-0000-0000-0000-000000000000",
            "ffffffff-ffff-ffff-ffff-ffffffffffff",
            "8a4f1b3c-0e2d-4a5b-9c7d-1f2e3a4b5c6d",
        ] {
            let actual = agent_principal_id(space_id);
            let expected = format!("agent-{space_id}");
            assert_eq!(
                actual, expected,
                "case {space_id:?}: expected {expected:?}, got {actual:?}"
            );
            // `PrincipalId`'s pattern: an alphanumeric first character, then the class
            // its remaining characters are drawn from.
            assert!(
                actual
                    .chars()
                    .next()
                    .is_some_and(|first| first.is_ascii_alphanumeric()),
                "case {space_id:?}: {actual:?} must start with an alphanumeric character"
            );
        }
    }
}
