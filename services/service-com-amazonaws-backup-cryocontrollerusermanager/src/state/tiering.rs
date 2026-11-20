// SPDX-License-Identifier: CC0-1.0

//! The TIERING side of this service's state: the configuration
//! `CreateTieringConfiguration` stores, and what every call that names a
//! `TieringConfigurationName` resolves against.
//!
//! # The rules this module holds
//!
//! * a tiering configuration is addressed by NAME and the name is unique to the
//!   account: the first create of a name stores the one configuration that name
//!   has, and a create naming one the account already holds is refused as
//!   already existing ([`super::tiering_configuration_already_exists`]) — so
//!   however many creates of a name race, exactly one of them stores a
//!   configuration and every other is refused;
//! * a name the account holds no configuration under is the not-found condition
//!   ([`super::tiering_configuration_not_found`]), which the read and the delete
//!   both answer, so a delete-then-read reports the same refusal as a read of a
//!   name nothing ever created;
//! * the selections a create carries are stored verbatim and reported back by
//!   both reads through the two projections below, so the configuration a
//!   create stored and the one a read reports can never differ;
//! * an update rewrites the MUTABLE body of the configuration a name already
//!   addresses — the vault it applies to and its selections — and records the
//!   instant it did ([`TieringConfigurationRecord::apply_update`]); the name,
//!   the ARN and the creation instant are the resource's identity and an update
//!   never changes them;
//! * a configuration carries a LAST-UPDATED instant from the moment it is
//!   stored — the create's own instant until an update rewrites it — so both
//!   reads report one for every stored configuration;
//! * the selections a create or an update carries may only name the one type of
//!   resource tiering supports ([`super::TIERING_RESOURCE_TYPE`]), and a body
//!   naming any other is refused as a bad parameter value
//!   ([`unsupported_tiering_resource_type`]) before anything is stored.

use std::collections::BTreeMap;
use std::time::SystemTime;

use super::ComAmazonawsBackupCryocontrollerusermanagerState;
use crate::ops::types::{
    InvalidParameterValueException, ResourceSelection, TieringConfiguration,
    TieringConfigurationInputForUpdate, TieringConfigurationsListMember,
};

/// The most tiering configurations this emulator keeps.
///
/// An EMULATOR-CHOSEN ceiling, not an AWS quota, for the reason given on
/// [`super::MAX_VAULTS`]: the store is in memory and every request can add to
/// it. AWS caps the tiering configurations an account may hold too, and answers
/// the same modelled `LimitExceededException` for a create past it.
pub const MAX_TIERING_CONFIGURATIONS: usize = 500;

/// A tiering configuration: what `CreateTieringConfiguration` stores,
/// `GetTieringConfiguration` and `ListTieringConfigurations` report, and
/// `DeleteTieringConfiguration` removes.
#[derive(Debug, Clone)]
pub struct TieringConfigurationRecord {
    pub name: String,
    pub arn: String,
    /// The vault the configuration applies to, or
    /// [`super::VAULT_NAMES_WILDCARD`] for every vault of the account — "use
    /// `*` to apply to all backup vaults" (model doc for the member).
    pub backup_vault_name: String,
    /// The resource selections the create carried, stored as submitted: the
    /// resources, their type and the day count after which each transitions.
    pub resource_selection: Vec<ResourceSelection>,
    pub creator_request_id: Option<String>,
    pub creation_time: SystemTime,
    /// When this configuration last changed: the instant the create stored it
    /// until an update rewrites it.
    pub last_updated_time: SystemTime,
    pub tags: BTreeMap<String, String>,
}

impl TieringConfigurationRecord {
    /// The configuration as the single-resource READ reports it — the ONE
    /// projection of a stored configuration, so `GetTieringConfiguration` can
    /// report nothing the create did not store.
    pub fn described(&self) -> TieringConfiguration {
        TieringConfiguration {
            tiering_configuration_name: self.name.clone(),
            tiering_configuration_arn: Some(self.arn.clone()),
            backup_vault_name: self.backup_vault_name.clone(),
            resource_selection: self.resource_selection.clone(),
            creator_request_id: self.creator_request_id.clone(),
            creation_time: Some(super::modelled_timestamp(self.creation_time)),
            last_updated_time: Some(super::modelled_timestamp(self.last_updated_time)),
        }
    }

    /// Rewrite the parts of this configuration an update may change, and
    /// record the instant it changed them.
    ///
    /// The name "cannot be updated after it is created" (the update's model
    /// doc) and the ARN and the creation instant are derived from it, so the
    /// body an update carries — the vault the configuration applies to and its
    /// selections — is all this rewrites. Stored as submitted, exactly as a
    /// create stores its body, so the two reads report an updated
    /// configuration the same way they report a created one.
    pub fn apply_update(&mut self, body: TieringConfigurationInputForUpdate, now: SystemTime) {
        self.backup_vault_name = body.backup_vault_name;
        self.resource_selection = body.resource_selection;
        self.last_updated_time = now;
    }

    /// The same configuration as the LISTING reports it: the summary members
    /// the list shape declares, taken from the same stored record the read
    /// projects, so a listed configuration and the read of it agree.
    pub fn listed(&self) -> TieringConfigurationsListMember {
        TieringConfigurationsListMember {
            tiering_configuration_arn: Some(self.arn.clone()),
            tiering_configuration_name: Some(self.name.clone()),
            backup_vault_name: Some(self.backup_vault_name.clone()),
            creation_time: Some(super::modelled_timestamp(self.creation_time)),
            last_updated_time: Some(super::modelled_timestamp(self.last_updated_time)),
        }
    }
}

/// The modelled refusal for a tiering body whose selections name a type of
/// resource tiering does not support, or `None` when every selection names the
/// one it does ([`super::TIERING_RESOURCE_TYPE`]).
///
/// The ONE reading of a submitted body's resource types, shared by the create
/// and the update, so neither can store a configuration over resources the
/// other would refuse. The sentence belongs to the condition, so it names
/// neither the type submitted nor the configuration
/// ([`super::MESSAGE_TIERING_RESOURCE_TYPE_UNSUPPORTED`]).
pub fn unsupported_tiering_resource_type(
    resource_selection: &[ResourceSelection],
) -> Option<InvalidParameterValueException> {
    resource_selection
        .iter()
        .any(|selection| selection.resource_type != super::TIERING_RESOURCE_TYPE)
        .then(|| InvalidParameterValueException {
            code: Some(super::ERROR_CODE_TIERING_RESOURCE_TYPE_UNSUPPORTED.to_string()),
            message: Some(super::MESSAGE_TIERING_RESOURCE_TYPE_UNSUPPORTED.to_string()),
            ..Default::default()
        })
}

impl ComAmazonawsBackupCryocontrollerusermanagerState {
    /// Resolve the tiering configuration a `TieringConfigurationName`
    /// addresses, or the modelled refusal for one this account does not hold.
    ///
    /// The ONE resolution step the read and the delete share, so neither can
    /// answer a different class or a different sentence for the same condition.
    pub fn stored_tiering_configuration(
        &self,
        name: &str,
    ) -> Result<&TieringConfigurationRecord, crate::ops::types::ResourceNotFoundException> {
        self.tiering_configurations
            .get(name)
            .ok_or_else(super::tiering_configuration_not_found)
    }

    /// Resolve the tiering configuration a `TieringConfigurationName`
    /// addresses FOR WRITING, or the modelled refusal for one this account does
    /// not hold — the same resolution, and so the same refusal, the read and
    /// the delete answer.
    pub fn stored_tiering_configuration_mut(
        &mut self,
        name: &str,
    ) -> Result<&mut TieringConfigurationRecord, crate::ops::types::ResourceNotFoundException> {
        self.tiering_configurations
            .get_mut(name)
            .ok_or_else(super::tiering_configuration_not_found)
    }

    /// The configuration a repeated `CreatorRequestId` replays: "a unique
    /// string that identifies the request and allows failed requests to be
    /// retried without the risk of running the operation twice" (model doc for
    /// the member), so a retry carrying the token of a create that already
    /// stored a configuration answers with that configuration rather than
    /// storing a second one.
    pub fn tiering_configuration_by_creator_request_id(
        &self,
        creator_request_id: &str,
    ) -> Option<&TieringConfigurationRecord> {
        self.tiering_configurations.values().find(|configuration| {
            configuration.creator_request_id.as_deref() == Some(creator_request_id)
        })
    }
}
