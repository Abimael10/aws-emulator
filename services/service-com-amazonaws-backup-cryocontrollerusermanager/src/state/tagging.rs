// SPDX-License-Identifier: CC0-1.0

//! The TAGS this service's resources carry, and the one resolution step the
//! tagging calls share.
//!
//! # The rule this module implements
//!
//! AWS Backup's tagging calls address a resource by ARN rather than by the name
//! or id the resource's own operations use, so a tag written through
//! `TagResource` has to land on the SAME stored record the resource's creator
//! wrote and its reads report — a vault's tags are the vault's, a recovery
//! point's are the point's. That resolution is stated once here
//! ([`ComAmazonawsBackupCryocontrollerusermanagerState::tagged_resource_tags`]
//! and its `_mut` twin) over every record this service stores that carries both
//! an ARN and tags, so a tag a create stored and a tag a tagging call stored are
//! one set, and an ARN that names no such resource is one not-found condition
//! ([`super::tagged_resource_not_found`]) however it fails to name one.
//!
//! A deleted backup plan is deliberately not one of them: the account no longer
//! holds it, so its tags are not there to read or write
//! ([`super::BackupPlanRecord::deletion_date`]).
//!
//! # Reading the tags of an ARN this account does not hold
//!
//! A tag READ is authorized against the resource its ARN names, so when that
//! ARN names a BACKUP VAULT the read is a vault-scoped read reached through the
//! ARN instead of through `BackupVaultName` — and a vault-scoped call decides
//! ACCESS before EXISTENCE
//! ([`ComAmazonawsBackupCryocontrollerusermanagerState::visible_vault`]): a
//! vault this account does not hold in that Region, because it was never
//! created or because it has been deleted, or a vault of another account, is
//! refused for lack of privileges and nothing is disclosed about whether it
//! exists. [`ComAmazonawsBackupCryocontrollerusermanagerState::tag_read_tags`]
//! states that decision for the read, so the two ways of addressing one vault
//! cannot answer differently. Every other ARN the resolution above fails to
//! place keeps the modelled not-found refusal its own resource's reads state.

use std::collections::BTreeMap;

use super::ComAmazonawsBackupCryocontrollerusermanagerState;
use crate::ops::types::ResourceNotFoundException;

/// The most tags this emulator keeps on ONE resource.
///
/// An EMULATOR-CHOSEN ceiling, for the reason given on [`super::MAX_VAULTS`]:
/// the keys are the caller's, so without a bound one resource's tag set would
/// grow with the request count. AWS caps the tags a resource may carry too, and
/// answers the same modelled `LimitExceededException` past it.
pub const MAX_TAGS_PER_RESOURCE: usize = 50;

/// The resource field a backup vault's ARN carries, as
/// [`super::backup_vault_arn`] mints it
/// (`arn:<partition>:backup:<region>:<account>:backup-vault:<name>`) and as the
/// model's own doc gives it for every member that carries a vault ARN.
const VAULT_RESOURCE_PREFIX: &str = "backup-vault:";

/// Why an ARN-addressed tag READ has no tags to report for the ARN it was given.
///
/// Two classes, because the ARN says which resource the read is authorized
/// against: an ARN naming a backup vault is judged by the decision every
/// vault-scoped call shares
/// ([`ComAmazonawsBackupCryocontrollerusermanagerState::visible_vault`]), which
/// reports only that the caller lacks privileges, while any other ARN this
/// service cannot place is the modelled not-found condition
/// ([`super::tagged_resource_not_found`]).
#[derive(Debug)]
pub enum UnresolvedTagRead {
    /// The ARN names a backup vault outside the caller's visibility, so the
    /// refusal discloses nothing about whether that vault exists.
    Denied(crate::scaffolded::AccessDeniedException),
    /// The ARN names no taggable resource of this account and is not a vault's,
    /// so the read is the not-found refusal the model declares.
    NotFound(ResourceNotFoundException),
}

/// Whether `resource_arn` names a BACKUP VAULT: this service's own ARN namespace
/// and the resource field [`super::backup_vault_arn`] mints for a vault.
///
/// Deliberately looser about the Region and the name than
/// [`super::backup_vault_arn_parts`], which only reads an ARN whose name a vault
/// of this account could have been stored under
/// ([`super::is_modelled_vault_name`]): a vault-scoped call states ONE decision
/// for a name the service could never have stored and for one it could
/// (`DescribeBackupVault` takes both through
/// [`ComAmazonawsBackupCryocontrollerusermanagerState::visible_vault`]), so a
/// tag read must not report an unstorable name as merely absent either.
fn names_a_backup_vault(resource_arn: &str) -> bool {
    let mut fields = resource_arn.splitn(6, ':');
    let (
        Some("arn"),
        Some(partition),
        Some(super::ARN_NAMESPACE),
        Some(_region),
        Some(_account),
        Some(resource),
    ) = (
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
    )
    else {
        return false;
    };
    !partition.is_empty() && resource.starts_with(VAULT_RESOURCE_PREFIX)
}

impl ComAmazonawsBackupCryocontrollerusermanagerState {
    /// The tags of the resource `resource_arn` names, or the modelled refusal
    /// for an ARN that names none of this account's taggable resources.
    pub fn tagged_resource_tags(
        &self,
        resource_arn: &str,
    ) -> Result<&BTreeMap<String, String>, ResourceNotFoundException> {
        if let Some(vault) = self
            .vaults
            .values()
            .find(|vault| vault.arn == resource_arn)
        {
            return Ok(&vault.tags);
        }
        if let Some(point) = self
            .recovery_points
            .values()
            .find(|point| point.arn == resource_arn)
        {
            return Ok(&point.tags);
        }
        if let Some(plan) = self
            .backup_plans
            .values()
            .find(|plan| plan.arn == resource_arn && plan.deletion_date.is_none())
        {
            return Ok(&plan.tags);
        }
        if let Some(framework) = self
            .frameworks
            .values()
            .find(|framework| framework.arn == resource_arn)
        {
            return Ok(&framework.tags);
        }
        if let Some(hold) = self
            .legal_holds
            .values()
            .find(|hold| hold.arn == resource_arn)
        {
            return Ok(&hold.tags);
        }
        if let Some(configuration) = self
            .tiering_configurations
            .values()
            .find(|configuration| configuration.arn == resource_arn)
        {
            return Ok(&configuration.tags);
        }
        if let Some(plan) = self
            .restore_testing_plans
            .values()
            .find(|plan| plan.arn == resource_arn)
        {
            return Ok(&plan.tags);
        }
        if let Some(plan) = self
            .report_plans
            .values()
            .find(|plan| plan.arn == resource_arn)
        {
            return Ok(&plan.tags);
        }
        Err(super::tagged_resource_not_found())
    }

    /// The tags of the resource `resource_arn` names for a tag READ, which
    /// decides ACCESS before EXISTENCE for the vault an ARN names.
    ///
    /// The resolution is the one every tagging call shares
    /// ([`Self::tagged_resource_tags`]); what this step adds is WHICH refusal an
    /// ARN it cannot place is answered with. An ARN naming a backup vault
    /// ([`names_a_backup_vault`]) is a vault-scoped read addressed by ARN, so an
    /// ARN whose vault this account does not hold — never created, deleted, in
    /// another Region, or another account's — gets the refusal every
    /// vault-scoped call states for a vault outside the caller's visibility
    /// ([`Self::visible_vault`]), which discloses nothing about whether that
    /// vault exists. Any other unplaceable ARN keeps the modelled not-found
    /// refusal ([`super::tagged_resource_not_found`]).
    pub fn tag_read_tags(
        &self,
        resource_arn: &str,
    ) -> Result<&BTreeMap<String, String>, UnresolvedTagRead> {
        match self.tagged_resource_tags(resource_arn) {
            Ok(tags) => Ok(tags),
            Err(not_found) => Err(if names_a_backup_vault(resource_arn) {
                UnresolvedTagRead::Denied(
                    crate::scaffolded::AccessDeniedException::insufficient_privileges(),
                )
            } else {
                UnresolvedTagRead::NotFound(not_found)
            }),
        }
    }

    /// The SAME resolution for a call that WRITES the tags it resolves, so the
    /// write and the read can never disagree about which record an ARN names.
    pub fn tagged_resource_tags_mut(

        &mut self,
        resource_arn: &str,
    ) -> Result<&mut BTreeMap<String, String>, ResourceNotFoundException> {
        if let Some(vault) = self
            .vaults
            .values_mut()
            .find(|vault| vault.arn == resource_arn)
        {
            return Ok(&mut vault.tags);
        }
        if let Some(point) = self
            .recovery_points
            .values_mut()
            .find(|point| point.arn == resource_arn)
        {
            return Ok(&mut point.tags);
        }
        if let Some(plan) = self
            .backup_plans
            .values_mut()
            .find(|plan| plan.arn == resource_arn && plan.deletion_date.is_none())
        {
            return Ok(&mut plan.tags);
        }
        if let Some(framework) = self
            .frameworks
            .values_mut()
            .find(|framework| framework.arn == resource_arn)
        {
            return Ok(&mut framework.tags);
        }
        if let Some(hold) = self
            .legal_holds
            .values_mut()
            .find(|hold| hold.arn == resource_arn)
        {
            return Ok(&mut hold.tags);
        }
        if let Some(configuration) = self
            .tiering_configurations
            .values_mut()
            .find(|configuration| configuration.arn == resource_arn)
        {
            return Ok(&mut configuration.tags);
        }
        if let Some(plan) = self
            .restore_testing_plans
            .values_mut()
            .find(|plan| plan.arn == resource_arn)
        {
            return Ok(&mut plan.tags);
        }
        if let Some(plan) = self
            .report_plans
            .values_mut()
            .find(|plan| plan.arn == resource_arn)
        {
            return Ok(&mut plan.tags);
        }
        Err(super::tagged_resource_not_found())
    }
}
