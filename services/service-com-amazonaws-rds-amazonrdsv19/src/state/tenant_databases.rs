// SPDX-License-Identifier: CC0-1.0

//! The tenant database records `CreateTenantDatabase`, `DescribeTenantDatabases`,
//! `ModifyTenantDatabase` and `DeleteTenantDatabase` share.
//!
//! A tenant database is a pluggable database INSIDE a DB instance running one of the
//! Oracle container database (CDB) engines, so every record here names the DB instance it
//! lives in and is reached only through a DB instance this service already holds.

use std::time::Instant;

use super::{
    ComAmazonawsRdsAmazonrdsv19State, ManagedMasterSecret, PendingChange, PendingStatus,
    RegionalKey, ARN_NAMESPACE,
};
use crate::ops::types::{Tag, TenantDatabase};

/// How many tenant databases one DB instance holds. Amazon RDS publishes a per-instance
/// limit for the Oracle multi-tenant configuration; the cap here is this emulator's, and
/// it bounds the store the same way the per-region quotas in `src/state.rs` do.
pub const TENANT_DATABASE_QUOTA_PER_INSTANCE: usize = 10;

/// The status a just-created tenant database reports while this service brings it up.
pub const TENANT_DATABASE_STATUS_CREATING: &str = "creating";

/// The status a tenant database reports once it is usable.
pub const TENANT_DATABASE_STATUS_AVAILABLE: &str = "available";

/// The status a tenant database reports once a delete has been accepted for it.
pub const TENANT_DATABASE_STATUS_DELETING: &str = "deleting";

/// The ARN resource type of a tenant database.
const ARN_RESOURCE_TYPE: &str = "tenant-database";

/// Whether a DB instance running `engine` uses the MULTI-TENANT configuration, which is
/// what decides whether it can hold tenant databases at all.
///
/// `CreateDBInstance$MultiTenant`: "Specifies whether to use the multi-tenant
/// configuration or the single-tenant configuration (default). This parameter only applies
/// to RDS for Oracle container database (CDB) engines." The CDB engines are the ones whose
/// `Engine` value ends in `-cdb` (`oracle-ee-cdb`, `oracle-se2-cdb` and their `custom-`
/// forms), so the architecture follows from the engine.
pub fn is_multi_tenant_engine(engine: &str) -> bool {
    engine.to_ascii_lowercase().ends_with("-cdb")
}

/// The engine family every DB instance that holds tenant databases runs
/// ([`is_multi_tenant_engine`]): the Oracle CDB engines.
///
/// Named so that a rule published PER ENGINE — the `MasterUsername` bound
/// ([`super::master_username_len`]), for one — can be read for a tenant database's own master
/// user before the DB instance holding it is resolved, which is the order an input constraint
/// is checked in.
pub const MULTI_TENANT_ENGINE: &str = "oracle";

/// What a request addressed at a TENANT DATABASE of a DB instance that does not use the
/// multi-tenant configuration is refused with.
///
/// Tenant databases "only appl[y] to RDS for Oracle container database (CDB) engines"
/// ([`is_multi_tenant_engine`]), so a single-tenant instance holds none at all and the
/// `TenantDBName` the request carried resolves nothing in it. The sentence is the one the
/// model publishes for that outcome — `TenantDatabaseNotFoundFault`: "The specified tenant
/// database wasn't found in the DB instance."
/// (`smithy_query(query="shape",shape="TenantDatabaseNotFoundFault")`) — so the refusal
/// reports WHAT the request could not resolve rather than explaining the instance's
/// architecture. It has no variable part: neither the instance, nor the engine it runs, nor
/// the tenant name the request carried is in it.
pub const TENANT_DATABASE_NOT_FOUND_IN_INSTANCE: &str =
    "The specified tenant database wasn't found in the DB instance.";

/// The second half of a tenant database's store key: the DB instance it lives in and its
/// own name, so one region's tenant databases are ordered by instance and then by name and
/// a scan of one instance's tenants is an ordered range.
pub fn tenant_database_key(db_instance_identifier: &str, tenant_db_name: &str) -> String {
    format!("{db_instance_identifier}/{tenant_db_name}")
}

/// The immutable resource id reported as `TenantDatabaseResourceId`, of the same minted
/// alphabet as the other server-assigned ids this service hands out.
pub fn mint_tenant_database_resource_id(mint: u64) -> String {
    format!("tdb-{}", super::minted_token("tenant-database", mint, 26))
}

/// The name of the secret RDS manages for a TENANT DATABASE's master user password: the
/// tenant database is addressed by its DB instance and its own name, so the secret of one
/// is named for that pair — under the same reserved prefix and in the same form every
/// secret RDS manages takes, a UUID derived from the resource whose credential it holds;
/// see [`super::cluster_master_user_secret_name`] for the form these names share.
pub fn tenant_database_master_user_secret_name(
    dbi_resource_id: &str,
    tenant_db_name: &str,
) -> String {
    format!(
        "{}tenantdb-{}",
        super::MANAGED_SECRET_NAME_PREFIX,
        super::managed_secret_uuid(&format!("{dbi_resource_id}/{tenant_db_name}"))
    )
}

/// A persisted tenant database.
#[derive(Debug, Clone)]
pub struct TenantDatabaseRecord {
    /// The region whose credential scope the create was signed with.
    pub region: String,
    /// The `DBInstanceIdentifier` of the instance this tenant database lives in.
    pub db_instance_identifier: String,
    /// `TenantDBName`.
    pub tenant_db_name: String,
    /// The minted `TenantDatabaseResourceId`.
    pub resource_id: String,
    /// The `DbiResourceId` of the instance this tenant database lives in, which is what
    /// `TenantDatabase$DbiResourceId` reports.
    pub dbi_resource_id: String,
    /// `MasterUsername`.
    pub master_username: String,
    /// `MasterUserSecret`: the secret RDS manages for this tenant database's master user
    /// password, for one created with `ManageMasterUserPassword` turned on; `None` for one
    /// whose password the caller supplied, which has no managed secret to report.
    pub master_user_secret: Option<ManagedMasterSecret>,
    /// `CharacterSetName`.
    pub character_set_name: Option<String>,
    /// `NcharCharacterSetName`.
    pub nchar_character_set_name: Option<String>,
    /// `Status`.
    pub status: String,
    /// The status change the clock owes this record, if any.
    pub pending_status: Option<PendingStatus>,
    /// `TenantDatabaseCreateTime`.
    pub create_time: ::aws_smithy_types::DateTime,
    /// `TagList`.
    pub tags: Vec<Tag>,
}

impl TenantDatabaseRecord {
    /// Apply a change whose due instant has passed. Returns whether the record is now
    /// gone, so the caller drops it from the store.
    pub(super) fn settle(&mut self, now: Instant) -> bool {
        if self.pending_status.as_ref().is_some_and(|p| now >= p.due_at) {
            let pending = self.pending_status.take().expect("checked just above");
            match pending.change {
                PendingChange::Status(status) => self.status = status,
                PendingChange::Removal => return true,
            }
        }
        false
    }

    /// `TenantDatabaseARN`, from the stored region and the minted resource id — the id
    /// rather than the name, because the name can be changed and the ARN cannot.
    pub fn arn(&self) -> String {
        emulator_server::emulator_arn(
            ARN_NAMESPACE,
            &self.region,
            &format!("{ARN_RESOURCE_TYPE}:{}", self.resource_id),
        )
    }

    /// Whether a delete has already been accepted for this tenant database.
    pub fn is_deleting(&self) -> bool {
        self.status == TENANT_DATABASE_STATUS_DELETING
    }

    /// Accept a delete: the tenant database reports `deleting` from now on and the clock
    /// owes its removal from the store. Nothing sleeps — the due instant is stored.
    pub fn begin_deletion(&mut self) {
        self.status = TENANT_DATABASE_STATUS_DELETING.to_string();
        self.pending_status = Some(PendingStatus::removal_in_a_moment());
    }

    /// Project the stored record onto the modelled `TenantDatabase` shape.
    ///
    /// `DeletionProtection` is "whether deletion protection is enabled for the DB
    /// INSTANCE" — a property of the instance this tenant database lives in, so it is
    /// passed in from that record rather than stored a second time here.
    ///
    /// `PendingModifiedValues` is what the call that CHANGED the tenant database answers
    /// with — a modification is applied as part of that call
    /// ([`TenantDatabaseModification`]) — so a tenant database this service REPORTS carries
    /// none. `MasterUserSecret` is reported for a tenant database
    /// whose master user password RDS manages, and only for one — one whose password the
    /// caller supplied has no secret of RDS's own.
    pub fn to_wire(&self, deletion_protection: bool) -> TenantDatabase {
        TenantDatabase {
            tenant_database_create_time: Some(self.create_time),
            db_instance_identifier: Some(self.db_instance_identifier.clone()),
            tenant_db_name: Some(self.tenant_db_name.clone()),
            status: Some(self.status.clone()),
            master_username: Some(self.master_username.clone()),
            dbi_resource_id: Some(self.dbi_resource_id.clone()),
            tenant_database_resource_id: Some(self.resource_id.clone()),
            tenant_database_arn: Some(self.arn()),
            character_set_name: self.character_set_name.clone(),
            nchar_character_set_name: self.nchar_character_set_name.clone(),
            deletion_protection: Some(deletion_protection),
            pending_modified_values: None,
            master_user_secret: self
                .master_user_secret
                .as_ref()
                .map(ManagedMasterSecret::to_wire),
            tag_list: Some(self.tags.clone()),
        }
    }
}

impl ComAmazonawsRdsAmazonrdsv19State {
    /// Materialise what the clock owes one region's TENANT DATABASES: the status transitions
    /// and the removals every record of this service settles
    /// ([`TenantDatabaseRecord::settle`]).
    ///
    /// Called from [`Self::settle_due_transitions`], and only from there, so a tenant
    /// database settles exactly where every other record family does; it is written here
    /// beside the record it walks.
    pub(super) fn settle_tenant_databases(&mut self, now: std::time::Instant, region: &str) {
        // A record whose pending change is its REMOVAL leaves the store below: the keys are
        // collected first because the range borrows the map mutably.
        let mut gone: Vec<RegionalKey> = Vec::new();
        for (key, tenant) in self
            .tenant_databases
            .range_mut((region.to_string(), String::new())..)
        {
            if key.0 != region {
                break;
            }
            if tenant.settle(now) {
                gone.push(key.clone());
            }
        }
        for key in gone {
            self.tenant_databases.remove(&key);
        }
    }

    /// The store key of every tenant database of the DB instance `key` names: a tenant
    /// database is keyed by its instance and then its own name ([`tenant_database_key`]), so
    /// one instance's tenants are an ordered range.
    fn tenant_databases_range(&self, key: &RegionalKey) -> Vec<RegionalKey> {
        let prefix = tenant_database_key(&key.1, "");
        self.tenant_databases
            .range((key.0.clone(), prefix.clone())..)
            .take_while(|((region, stored), _)| *region == key.0 && stored.starts_with(&prefix))
            .map(|(stored_key, _)| stored_key.clone())
            .collect()
    }

    /// Drop every tenant database of the DB instance `key` names.
    ///
    /// A tenant database is a pluggable database INSIDE a DB instance, reached only through
    /// that instance, so none of them outlives the instance that held it: the moment the
    /// instance leaves the store, so do its tenant databases, and no report answers with one
    /// whose DB instance nothing can resolve.
    pub(crate) fn drop_tenant_databases_of(&mut self, key: &RegionalKey) {
        for stored_key in self.tenant_databases_range(key) {
            self.tenant_databases.remove(&stored_key);
        }
    }

    /// Move every tenant database of the DB instance renamed from `from` to `to`.
    ///
    /// Each of them is ADDRESSED by the instance that holds it — the identifier is half its
    /// store key and the `DBInstanceIdentifier` it reports — so a renamed instance carries
    /// its tenant databases over to the identifier it is now called by.
    pub(crate) fn move_tenant_databases_of(&mut self, from: &RegionalKey, to: &RegionalKey) {
        for stored_key in self.tenant_databases_range(from) {
            let Some(mut record) = self.tenant_databases.remove(&stored_key) else {
                continue;
            };
            record.db_instance_identifier = to.1.clone();
            record.region = to.0.clone();
            let moved_key = (
                to.0.clone(),
                tenant_database_key(&to.1, &record.tenant_db_name),
            );
            self.tenant_databases.insert(moved_key, record);
        }
    }
}
