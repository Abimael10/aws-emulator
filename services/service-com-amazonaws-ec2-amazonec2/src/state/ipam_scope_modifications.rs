// SPDX-License-Identifier: CC0-1.0

//! What one `ModifyIpamScope` request changes about a stored IPAM scope: the one reading of
//! the external authority configuration a request names (shared with the create, so the two
//! cannot admit a different configuration), the ONE definition of what applying a
//! modification leaves behind, and the one writer that applies it.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every other
//! primitive there.

use super::*;
use crate::ops::types::{ExternalAuthorityConfiguration, IpamScopeExternalAuthorityConfiguration};

/// `requested` as the external authority configuration a scope stores, or the refusal the
/// request earns: `None` when the request named none at all, which links the scope to no
/// external system.
///
/// "The configuration that links an Amazon VPC IPAM scope to an external authority system. It
/// specifies the type of external system AND the external resource identifier that identifies
/// your account or instance in that system" (`ExternalAuthorityConfiguration`): a
/// configuration naming only one of the two links the scope to nothing, so it is refused
/// rather than stored half-written. The type itself targets the modelled
/// `IpamScopeExternalAuthorityType` enum, so a spelling outside it is refused before a handler
/// runs; the identifier is caller-chosen text the scope keeps for the process's lifetime and
/// the model publishes no `@length` for it, so it is held to the bound this crate gives a
/// caller-chosen text member of that kind ([`MAX_TAG_VALUE_CHARS`]).
///
/// The ONE reader of such a configuration, so `CreateIpamScope` and `ModifyIpamScope` cannot
/// admit a different one or word the same refusal differently.
pub fn requested_ipam_scope_external_authority<E>(
    requested: Option<&ExternalAuthorityConfiguration>,
) -> Result<Option<IpamScopeExternalAuthorityConfiguration>, Scaffolded<E>> {
    let Some(requested) = requested else {
        return Ok(None);
    };
    let authority_type = requested
        .r#type
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            missing_parameter("An external authority configuration must have a type")
        })?;
    let identifier = requested
        .external_resource_identifier
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            missing_parameter(
                "An external authority configuration must have an external resource identifier",
            )
        })?;
    if identifier.chars().count() > MAX_TAG_VALUE_CHARS {
        return Err(invalid_parameter_value(format!(
            "externalResourceIdentifier accepts at most {max} characters",
            max = MAX_TAG_VALUE_CHARS,
        )));
    }
    Ok(Some(IpamScopeExternalAuthorityConfiguration {
        r#type: Some(authority_type.to_string()),
        external_resource_identifier: Some(identifier.to_string()),
    }))
}

/// What one `ModifyIpamScope` asks to be changed about a stored IPAM scope.
///
/// A member left `None` here is one the REQUEST did not carry, which leaves the stored value
/// exactly as it was rather than returning it to the default a create would have given it —
/// the same contract [`IpamPoolModification`] has for a pool. The external authority is a set
/// value and a removal flag rather than one value, because "Remove the external authority
/// configuration. true to remove" (`ModifyIpamScopeRequest$RemoveExternalAuthorityConfiguration`)
/// asks for the link to be taken AWAY rather than set.
#[derive(Debug, Clone, Default)]
pub struct IpamScopeModification {
    /// "The ID of the scope you want to modify" (`ModifyIpamScopeRequest$IpamScopeId`).
    pub ipam_scope_id: String,
    /// "The description of the scope you want to modify"
    /// (`ModifyIpamScopeRequest$Description`), or `None` when the request named none.
    pub description: Option<String>,
    /// The external authority configuration the request names, already read by
    /// [`requested_ipam_scope_external_authority`], or `None` to leave the stored link as it
    /// is.
    pub external_authority_configuration: Option<IpamScopeExternalAuthorityConfiguration>,
    /// "Remove the external authority configuration. true to remove"
    /// (`ModifyIpamScopeRequest$RemoveExternalAuthorityConfiguration`): the one member that
    /// asks for a link to be taken away rather than set.
    pub remove_external_authority_configuration: bool,
}

impl IpamScopeModification {
    /// The IPAM scope this modification leaves behind, from the one it is applied to.
    ///
    /// The ONE definition of what a modification DOES to a scope, so the figure a handler
    /// charges to the store's byte budget is exactly the one the write stores. Every member
    /// the request did not carry is copied across unchanged, and every member the scope's
    /// creation derived — its id, its ARN, its owner, the IPAM and Region it belongs to, its
    /// type, its pool count, whether it is a default scope and its state — is carried over
    /// untouched, since none of them follows from this request.
    pub fn applied_to(&self, scope: &IpamScope) -> IpamScope {
        let mut applied = scope.clone();
        if self.description.is_some() {
            applied.description = self.description.clone();
        }
        // The removal leaves the scope linked to no external system at all; a request carrying
        // both members asks for two incompatible things of one link and is refused by the
        // handler before this is reached.
        if self.remove_external_authority_configuration {
            applied.external_authority_configuration = None;
        } else if self.external_authority_configuration.is_some() {
            applied.external_authority_configuration = self.external_authority_configuration.clone();
        }
        applied
    }
}

/// The caller-chosen bytes of a stored IPAM scope that a `ModifyIpamScope` can change: its
/// description and the external resource identifier it is linked by — exactly the share
/// [`ComAmazonawsEc2Amazonec2State::insert_created_ipam_scope`] charges a create for them.
///
/// The ONE definition of that charge, on the terms [`ipam_pool_modifiable_bytes`] states for a
/// pool, so what [`ComAmazonawsEc2Amazonec2State::modify_ipam_scope`] gives back for the
/// record it replaces is the same number a later modification of the replacement gives back
/// for it. The scope's tags, its client token and the fixed per-record overhead are
/// deliberately NOT counted: no modification changes them, so counting them would charge and
/// refund the same figure twice.
pub fn ipam_scope_modifiable_bytes(scope: &IpamScope) -> usize {
    scope.description.as_deref().unwrap_or_default().len()
        + scope
            .external_authority_configuration
            .as_ref()
            .and_then(|configuration| configuration.external_resource_identifier.as_deref())
            .unwrap_or_default()
            .len()
}

impl ComAmazonawsEc2Amazonec2State {
    /// Apply `modification` to the IPAM scope it names and answer the scope as the store then
    /// holds it — `None` when the account holds no such scope.
    ///
    /// The ONE writer of a modified scope, the counterpart of [`Self::modify_ipam_pool`] for
    /// the container a pool lives in: what the record becomes is
    /// [`IpamScopeModification::applied_to`]'s answer, so a handler cannot check one scope and
    /// store another, and the store's byte budget gives back what the replaced record held
    /// ([`ipam_scope_modifiable_bytes`]) before charging what the new one holds, so the budget
    /// tracks exactly the bytes the store keeps.
    ///
    /// The scope's STATE is left as it stands: a modification is not a transition, so a scope
    /// the clock still owes its `create-complete` move keeps that move — the same reading
    /// [`Self::modify_ipam_pool`] has of a pool.
    pub fn modify_ipam_scope(
        &mut self,
        modification: &IpamScopeModification,
    ) -> Option<IpamScope> {
        let current = self.ipam_scopes.get(&modification.ipam_scope_id)?.clone();
        let applied = modification.applied_to(&current);
        self.release_stored_bytes(ipam_scope_modifiable_bytes(&current));
        self.add_stored_bytes(ipam_scope_modifiable_bytes(&applied));
        self.ipam_scopes
            .insert(modification.ipam_scope_id.clone(), applied.clone());
        Some(applied)
    }
}
