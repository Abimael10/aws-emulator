// SPDX-License-Identifier: CC0-1.0

//! The AUTOMATED BACKUP of a DB instance: what `DescribeDBInstanceAutomatedBackups`
//! reports and `DeleteDBInstanceAutomatedBackup` removes.
//!
//! `DBInstanceAutomatedBackup` is "an automated backup of a DB instance ... the database
//! instance properties that existed at the time you deleted the source instance", and the
//! statuses the member publishes say WHICH instances have one: `active` "for current
//! instances", `creating` for one "waiting for the first automated snapshot to be
//! available" and `retained` "for deleted instances". So a DB instance has an automated
//! backup exactly while automated backups are being kept for it — which is what
//! `BackupRetentionPeriod` decides, "To disable automated backups, set this value to 0" —
//! and that backup OUTLIVES the instance when the delete asked to keep it
//! (`DeleteDBInstance$DeleteAutomatedBackups`).
//!
//! Hence two sources for one report, and one projection over both:
//!
//! * a LIVE instance that keeps continuous backups has an `active` backup (`creating`
//!   while the instance itself is still coming up and has taken none yet), projected from
//!   the instance's own record, so the two can never disagree about the instance's
//!   properties;
//! * a DELETED instance leaves a [`RetainedInstanceBackupRecord`] behind — the properties
//!   it had when the delete was accepted — which the store keeps until
//!   `DeleteDBInstanceAutomatedBackup` removes it.
//!
//! The retained records are bounded: at most
//! [`RETAINED_INSTANCE_BACKUP_QUOTA_PER_REGION`] per region, which is the quota
//! `DeleteDBInstance`'s own `DBInstanceAutomatedBackupQuotaExceededFault` reports — "The
//! quota for retained automated backups was exceeded ... the same as your DB instance
//! quota".

use super::*;

/// `active` — "automated backups for current instances", the status of the automated
/// backup of an instance this store still holds.
pub const DB_INSTANCE_AUTOMATED_BACKUP_STATUS_ACTIVE: &str = "active";

/// `creating` — "automated backups that are waiting for the first automated snapshot to
/// be available", the status while the source instance is itself still being created.
pub const DB_INSTANCE_AUTOMATED_BACKUP_STATUS_CREATING: &str = "creating";

/// `retained` — "automated backups for deleted instances", the status of a backup a
/// delete kept.
pub const DB_INSTANCE_AUTOMATED_BACKUP_STATUS_RETAINED: &str = "retained";

/// How many RETAINED automated backups one region may hold.
///
/// `DeleteDBInstance` models `DBInstanceAutomatedBackupQuotaExceededFault` — "The quota
/// for retained automated backups was exceeded. This prevents you from retaining any
/// additional automated backups. The retained automated backups quota is the same as your
/// DB instance quota" — so the bound is the region's DB instance quota
/// ([`DB_INSTANCE_QUOTA_PER_REGION`]), and reaching it is that fault rather than a store
/// that grows without end.
pub const RETAINED_INSTANCE_BACKUP_QUOTA_PER_REGION: usize = DB_INSTANCE_QUOTA_PER_REGION;

/// A RETAINED automated backup: the properties of a DB instance that has been deleted with
/// `DeleteAutomatedBackups` turned off, kept after the instance itself is gone.
///
/// Every member is copied from the instance at the instant its delete was accepted — "the
/// database instance properties that existed at the time you deleted the source instance" —
/// because the instance record is dropped from the store once the delete runs its course
/// and the backup must still report the instance it was taken of.
#[derive(Debug, Clone)]
pub struct RetainedInstanceBackupRecord {
    /// `DbiResourceId`: "the resource ID for the source DB instance, which can't be
    /// changed and which is unique to an Amazon Web Services Region" — what this record is
    /// keyed by, and what `DeleteDBInstanceAutomatedBackup` names it by.
    pub dbi_resource_id: String,
    /// `DBInstanceIdentifier`: "the identifier for the source DB instance".
    pub db_instance_identifier: String,
    /// The region the source instance was created in: the scope of the backup and of the
    /// ARNs it reports.
    pub region: String,
    /// When the delete that retained this backup was accepted: the LATEST instant the
    /// backup covers, since nothing was written to the instance after it.
    pub retained_at: ::aws_smithy_types::DateTime,
    /// `BackupRetentionPeriod`: "the retention period for the automated backups", as the
    /// source instance carried it. It is what the restore window reaches back over.
    pub backup_retention_period: i32,
    /// `PreferredBackupWindow`, `InstanceCreateTime`, `MasterUsername`, … — the rest of the
    /// source instance's properties this backup reports, each copied from its record.
    pub preferred_backup_window: Option<String>,
    pub instance_create_time: ::aws_smithy_types::DateTime,
    pub master_username: Option<String>,
    pub engine: String,
    pub engine_version: Option<String>,
    pub license_model: Option<String>,
    pub allocated_storage: Option<i32>,
    pub port: i32,
    pub availability_zone: String,
    pub iops: Option<i32>,
    pub storage_throughput: i32,
    pub option_group_name: Option<String>,
    pub storage_encrypted: bool,
    pub storage_type: String,
    pub kms_key_id: Option<String>,
    pub iam_database_authentication_enabled: bool,
    pub backup_target: String,
    pub dedicated_log_volume: bool,
    pub tags: Vec<Tag>,
}

/// WHICH automated backup a request names: `DbiResourceId`, "the identifier for the source
/// DB instance", or `DBInstanceAutomatedBackupsArn`, "the Amazon Resource Name (ARN) of the
/// automated backups" — the two members `DeleteDBInstanceAutomatedBackup` accepts, either of
/// which names the ONE automated backup of one instance.
///
/// A reference carries whichever member the request sent and is matched against BOTH of the
/// backup's own values ([`Self::names`]), so neither member has to be taken apart to resolve
/// it: the ARN is a function of the source instance's resource id
/// ([`instance_automated_backups_arn`]), and reversing that function at a lookup is how a
/// reference stops resolving when the function changes.
#[derive(Debug, Clone, Default)]
pub struct AutomatedBackupReference {
    /// What the request's `DbiResourceId` carried, where it carried one.
    pub dbi_resource_id: Option<String>,
    /// What its `DBInstanceAutomatedBackupsArn` carried, where it carried one.
    pub automated_backups_arn: Option<String>,
}

impl AutomatedBackupReference {
    /// Whether this reference's `DbiResourceId` names the source instance whose resource id
    /// is `dbi_resource_id`. "This parameter isn't case-sensitive."
    pub fn names_resource_id(&self, dbi_resource_id: &str) -> bool {
        self.dbi_resource_id
            .as_deref()
            .is_some_and(|named| named.eq_ignore_ascii_case(dbi_resource_id))
    }

    /// Whether this reference's `DBInstanceAutomatedBackupsArn` names the backup whose own
    /// ARN is `arn`. Resolved without regard to case, as every other name this service
    /// resolves against stored state is — the ONE rule for this member, so the report and
    /// the delete cannot disagree about which backup an ARN names.
    pub fn names_arn(&self, arn: &str) -> bool {
        self.automated_backups_arn
            .as_deref()
            .is_some_and(|named| named.eq_ignore_ascii_case(arn))
    }

    /// Whether this reference names the backup whose source instance's resource id is
    /// `dbi_resource_id` and whose own ARN is `arn` — by either member, since either names
    /// the one backup of one instance.
    pub fn names(&self, dbi_resource_id: &str, arn: &str) -> bool {
        self.names_resource_id(dbi_resource_id) || self.names_arn(arn)
    }

    /// What a refusal names the backup by: the SOURCE INSTANCE'S `DbiResourceId`, which is
    /// the id an automated backup is identified by — "the identifier for the source DB
    /// instance, which can't be changed and which is unique to an Amazon Web Services
    /// Region".
    ///
    /// A request that named the backup by `DBInstanceAutomatedBackupsArn` is answered with
    /// the resource id THAT ARN belongs to, read back out of it: the ARN is a function of the
    /// source instance's resource id ([`instance_automated_backups_arn`]), so the backup an
    /// ARN names has exactly one, whether or not this Region still holds a record of it.
    /// EMPTY for a request that named neither member, which is refused before any lookup.
    pub fn describe(&self) -> String {
        if let Some(resource_id) = self.dbi_resource_id.as_deref() {
            return resource_id.to_string();
        }
        match self.automated_backups_arn.as_deref() {
            Some(arn) => dbi_resource_id_in_automated_backups_arn(arn)
                .unwrap_or_else(|| arn.to_string()),
            None => String::new(),
        }
    }
}

impl RetainedInstanceBackupRecord {
    /// The properties of `instance` as of NOW: the instant a delete that keeps its automated
    /// backup was accepted, or the instant replication of that backup to another Region was
    /// enabled ([`ReplicatedInstanceBackupRecord`]) — the two calls that take a copy of an
    /// instance's backup properties, which is why there is one copy of them.
    pub fn of(instance: &DbInstanceRecord) -> Self {
        Self {
            dbi_resource_id: instance.dbi_resource_id.clone(),
            db_instance_identifier: instance.identifier.clone(),
            region: instance.region.clone(),
            retained_at: now_as_wire_timestamp(),
            backup_retention_period: instance.backup_retention_period,
            preferred_backup_window: instance.preferred_backup_window.clone(),
            instance_create_time: instance.instance_create_time,
            master_username: instance.master_username.clone(),
            engine: instance.engine.clone(),
            engine_version: instance.engine_version.clone(),
            license_model: instance.license_model.clone(),
            allocated_storage: instance.allocated_storage,
            port: instance.endpoint_port,
            availability_zone: instance.availability_zone.clone(),
            iops: instance.iops,
            storage_throughput: instance.storage_throughput,
            option_group_name: instance.option_group_name.clone(),
            storage_encrypted: instance.storage_encrypted,
            storage_type: instance.storage_type.clone(),
            kms_key_id: instance.kms_key_id.clone(),
            iam_database_authentication_enabled: instance.iam_database_authentication_enabled,
            backup_target: instance.backup_target.clone(),
            dedicated_log_volume: instance.dedicated_log_volume,
            tags: instance.tags.clone(),
        }
    }

    /// `DBInstanceAutomatedBackupsArn`: the ARN of THIS backup, from its source instance's
    /// resource id ([`instance_automated_backups_arn`]).
    pub fn automated_backups_arn(&self) -> String {
        instance_automated_backups_arn(&self.region, &self.dbi_resource_id)
    }

    /// Project the retained backup onto the modelled `DBInstanceAutomatedBackup` shape.
    ///
    /// `Status` is `retained` — the value the member documents for "automated backups for
    /// deleted instances" — and the restore window ENDS at the instant the delete was
    /// accepted: the instance wrote nothing after it, so no later instant can be restored
    /// to.
    pub fn to_wire(&self) -> DBInstanceAutomatedBackup {
        DBInstanceAutomatedBackup {
            db_instance_identifier: Some(self.db_instance_identifier.clone()),
            dbi_resource_id: Some(self.dbi_resource_id.clone()),
            region: Some(self.region.clone()),
            db_instance_arn: Some(db_instance_arn(&self.region, &self.db_instance_identifier)),
            db_instance_automated_backups_arn: Some(self.automated_backups_arn()),
            status: Some(DB_INSTANCE_AUTOMATED_BACKUP_STATUS_RETAINED.to_string()),
            restore_window: Some(restore_window(
                self.retained_at,
                self.instance_create_time,
                self.backup_retention_period,
            )),
            backup_retention_period: Some(self.backup_retention_period),
            preferred_backup_window: self.preferred_backup_window.clone(),
            instance_create_time: Some(self.instance_create_time),
            master_username: self.master_username.clone(),
            engine: Some(self.engine.clone()),
            engine_version: self.engine_version.clone(),
            license_model: self.license_model.clone(),
            allocated_storage: self.allocated_storage,
            port: Some(self.port),
            availability_zone: Some(self.availability_zone.clone()),
            vpc_id: Some(region_vpc_id(&self.region)),
            iops: self.iops,
            storage_throughput: Some(self.storage_throughput),
            option_group_name: self.option_group_name.clone(),
            encrypted: Some(self.storage_encrypted),
            storage_encryption_type: Some(
                storage_encryption_type(self.storage_encrypted, &self.engine).to_string(),
            ),
            storage_type: Some(self.storage_type.clone()),
            kms_key_id: self.kms_key_id.clone(),
            iam_database_authentication_enabled: Some(self.iam_database_authentication_enabled),
            backup_target: Some(self.backup_target.clone()),
            dedicated_log_volume: Some(self.dedicated_log_volume),
            tag_list: Some(self.tags.clone()),
            // The REPLICATED backups of another Region: the instance this backup was taken
            // of is GONE, so the replications of its backup are no longer in force and the
            // list is EMPTY rather than absent, which is what RDS reports for a backup
            // replicated nowhere. The key store a Transparent Data Encryption credential
            // lives in, an Amazon Web Services Backup recovery point and the time zone only a
            // Microsoft SQL Server instance carries: this service holds none of them, so none
            // is invented.
            db_instance_automated_backups_replications: Some(Vec::new()),
            tde_credential_arn: None,
            aws_backup_recovery_point_arn: None,
            timezone: None,
            multi_tenant: None,
            additional_storage_volumes: None,
        }
    }
}

impl DbInstanceRecord {
    /// Whether automated backups are being kept for this instance, so it HAS an automated
    /// backup to report: `BackupRetentionPeriod` decides it — "To disable automated
    /// backups, set this value to 0" — and an instance that is a MEMBER of a DB cluster
    /// keeps none of its own, because "the data is held in the cluster volume" and the
    /// cluster is what the backups are of — `DescribeDBClusterAutomatedBackups` is that
    /// side of it.
    ///
    /// An instance whose DELETE has been accepted holds none either: that call decided what
    /// becomes of its automated backups — dropped, or kept as a
    /// [`RetainedInstanceBackupRecord`] — so from then on the retained record is the one
    /// backup of that instance, and the instance it is OF reports none of its own while it
    /// reports `deleting`. Otherwise one backup would be reported twice for the span
    /// between the accepted delete and the instance's removal.
    pub fn has_automated_backup(&self) -> bool {
        self.backup_retention_period > 0
            && self.db_cluster_identifier.is_none()
            && !self.is_deleting()
    }

    /// The status of this instance's automated backup: `creating` while the instance is
    /// still being provisioned and "waiting for the first automated snapshot to be
    /// available", and `active` — "automated backups for current instances" — once it is
    /// up.
    pub fn automated_backup_status(&self) -> &'static str {
        match self.status == DB_INSTANCE_STATUS_CREATING {
            true => DB_INSTANCE_AUTOMATED_BACKUP_STATUS_CREATING,
            false => DB_INSTANCE_AUTOMATED_BACKUP_STATUS_ACTIVE,
        }
    }

    /// `DBInstanceAutomatedBackupsArn` for this instance's automated backup.
    pub fn automated_backups_arn(&self) -> String {
        instance_automated_backups_arn(&self.region, &self.dbi_resource_id)
    }

    /// Project this instance's automated backup onto the modelled
    /// `DBInstanceAutomatedBackup` shape.
    ///
    /// Every member is read from the instance the backup is OF, so a describe of the
    /// instance and a describe of its automated backup cannot disagree, and the restore
    /// window is the window a point-in-time restore of the instance may target
    /// ([`Self::restorable_window`]) — the same window, read once.
    pub fn automated_backup_to_wire(&self) -> DBInstanceAutomatedBackup {
        let window = self.restorable_window().map_or_else(
            // An instance still coming up has taken no backup yet, so there is no instant
            // it can be restored to: the window reaches from the instant it was created to
            // that same instant rather than claiming coverage nothing holds.
            || RestoreWindow {
                earliest_time: Some(self.instance_create_time),
                latest_time: Some(self.instance_create_time),
            },
            |(earliest, latest)| RestoreWindow {
                earliest_time: Some(earliest),
                latest_time: Some(latest),
            },
        );
        DBInstanceAutomatedBackup {
            db_instance_identifier: Some(self.identifier.clone()),
            dbi_resource_id: Some(self.dbi_resource_id.clone()),
            region: Some(self.region.clone()),
            db_instance_arn: Some(self.arn()),
            db_instance_automated_backups_arn: Some(self.automated_backups_arn()),
            status: Some(self.automated_backup_status().to_string()),
            restore_window: Some(window),
            backup_retention_period: Some(self.backup_retention_period),
            preferred_backup_window: self.preferred_backup_window.clone(),
            instance_create_time: Some(self.instance_create_time),
            master_username: self.master_username.clone(),
            engine: Some(self.engine.clone()),
            engine_version: self.engine_version.clone(),
            license_model: self.license_model.clone(),
            allocated_storage: self.allocated_storage,
            port: Some(self.endpoint_port),
            availability_zone: Some(self.availability_zone.clone()),
            vpc_id: Some(region_vpc_id(&self.region)),
            iops: self.iops,
            storage_throughput: Some(self.storage_throughput),
            option_group_name: self.option_group_name.clone(),
            encrypted: Some(self.storage_encrypted),
            storage_encryption_type: Some(
                storage_encryption_type(self.storage_encrypted, &self.engine).to_string(),
            ),
            storage_type: Some(self.storage_type.clone()),
            kms_key_id: self.kms_key_id.clone(),
            iam_database_authentication_enabled: Some(self.iam_database_authentication_enabled),
            backup_target: Some(self.backup_target.clone()),
            dedicated_log_volume: Some(self.dedicated_log_volume),
            tag_list: Some(self.tags.clone()),
            // As above: the members this service holds no state for are left out rather than
            // invented. The replication list is EMPTY here because the replications of this
            // backup are records of OTHER Regions, which this projection cannot reach — the
            // store's own projection fills it
            // ([`ComAmazonawsRdsAmazonrdsv19State::instance_automated_backup_to_wire`]).
            db_instance_automated_backups_replications: Some(Vec::new()),
            tde_credential_arn: None,
            aws_backup_recovery_point_arn: None,
            timezone: None,
            multi_tenant: None,
            additional_storage_volumes: None,
        }
    }
}

/// `arn:aws:rds:<region>:<account>:auto-backup:ab-<source instance's resource id>` — the
/// ARN form `DBInstanceAutomatedBackupsArn` publishes
/// (`arn:aws:rds:us-east-1:123456789012:auto-backup:ab-L2IJCEXJP7XQ7HOJ4SIEXAMPLE`).
///
/// Derived from the SOURCE instance's `DbiResourceId`, which "can't be changed and ... is
/// unique to an Amazon Web Services Region": one instance has one automated backup, so the
/// ARN is a function of that id rather than a second id minted beside it, and it still
/// names the same backup after the instance is deleted. The `db-` the instance's own
/// resource id begins with is dropped, so the id reads under the `ab-` prefix its own
/// member publishes.
pub fn instance_automated_backups_arn(region: &str, dbi_resource_id: &str) -> String {
    let id = dbi_resource_id
        .strip_prefix(DBI_RESOURCE_ID_PREFIX)
        .unwrap_or(dbi_resource_id);
    emulator_server::emulator_arn(
        ARN_NAMESPACE,
        region,
        &format!("{AUTOMATED_BACKUP_RESOURCE}:{AUTOMATED_BACKUP_ID_PREFIX}{id}"),
    )
}

/// The `db-` an instance's resource id begins with, which the automated backup's own
/// `ab-` id is read against ([`instance_automated_backups_arn`]).
const DBI_RESOURCE_ID_PREFIX: &str = "db-";

/// The resource type of an automated backup's ARN, and the prefix of the id in it.
const AUTOMATED_BACKUP_RESOURCE: &str = "auto-backup";
const AUTOMATED_BACKUP_ID_PREFIX: &str = "ab-";

/// The segment an automated backup's ARN carries its own id behind, which is what the id is
/// read back out of it after ([`dbi_resource_id_in_automated_backups_arn`]).
const AUTOMATED_BACKUP_ARN_SEGMENT: &str = ":auto-backup:";

/// The SOURCE INSTANCE's `DbiResourceId` an automated-backups ARN names, read back out of
/// the ARN — the inverse of [`instance_automated_backups_arn`] for every id that function is
/// given, which is a minted one and so always carries the [`DBI_RESOURCE_ID_PREFIX`] this
/// puts back.
///
/// `None` for a value of any other form, which names no automated backup of this service at
/// all. This is what lets a refusal name the backup by the id it is identified by even when
/// the request named it by its ARN and no record of it is left
/// ([`AutomatedBackupReference::describe`]).
pub fn dbi_resource_id_in_automated_backups_arn(arn: &str) -> Option<String> {
    let (_, resource) = arn.rsplit_once(AUTOMATED_BACKUP_ARN_SEGMENT)?;
    let id = resource.strip_prefix(AUTOMATED_BACKUP_ID_PREFIX)?;
    if id.is_empty() {
        return None;
    }
    Some(format!("{DBI_RESOURCE_ID_PREFIX}{id}"))
}

/// `RestoreWindow`: "the earliest and latest time a DB instance can be restored to".
///
/// The window ends at `latest` — the published point the backup covers to — and reaches
/// `backup_retention_period` days back from it, never before `earliest_possible`: the
/// earliest instant the backup holds any data from, which for an instance's own backup is
/// when that instance was created and for one replicated to another Region is when the
/// replication began ([`ReplicatedInstanceBackupRecord::restore_window`]).
pub(super) fn restore_window(
    latest: ::aws_smithy_types::DateTime,
    earliest_possible: ::aws_smithy_types::DateTime,
    backup_retention_period: i32,
) -> RestoreWindow {
    let retention = f64::from(backup_retention_period) * 24.0 * 60.0 * 60.0;
    let earliest = (latest.as_secs_f64() - retention).max(earliest_possible.as_secs_f64());
    RestoreWindow {
        earliest_time: Some(::aws_smithy_types::DateTime::from_secs_f64(earliest)),
        latest_time: Some(latest),
    }
}

impl ComAmazonawsRdsAmazonrdsv19State {
    /// The RETAINED automated backups of `region`, in key order — the order
    /// `DescribeDBInstanceAutomatedBackups` pages them in.
    ///
    /// Reads one region's ordered range, bounded by
    /// [`RETAINED_INSTANCE_BACKUP_QUOTA_PER_REGION`].
    pub fn retained_instance_backups_in(&self, region: &str) -> Vec<&RetainedInstanceBackupRecord> {
        range_in(&self.db_instance_automated_backups, region)
    }

    /// Whether this store has ROOM to retain the automated backup of the instance in
    /// `region` whose resource id is `dbi_resource_id`
    /// ([`RETAINED_INSTANCE_BACKUP_QUOTA_PER_REGION`] per region). A region already holding
    /// that instance's backup has room for it, since keeping it again replaces that one
    /// record rather than adding another.
    ///
    /// Read BEFORE the delete is accepted, so a delete the quota refuses changes nothing:
    /// the refusal is the modelled `DBInstanceAutomatedBackupQuotaExceededFault`.
    pub fn has_room_for_retained_instance_backup(
        &self,
        region: &str,
        dbi_resource_id: &str,
    ) -> bool {
        self.db_instance_automated_backups
            .contains_key(&(region.to_string(), dbi_resource_id.to_string()))
            || self.retained_instance_backups_in(region).len()
                < RETAINED_INSTANCE_BACKUP_QUOTA_PER_REGION
    }

    /// Keep `backup` after the instance it was taken of is gone: what `DeleteDBInstance`
    /// with `DeleteAutomatedBackups` turned off does.
    ///
    /// Call it with [`Self::has_room_for_retained_instance_backup`] already answered, which
    /// is where the region's quota is enforced.
    pub fn retain_instance_automated_backup(&mut self, backup: RetainedInstanceBackupRecord) {
        self.db_instance_automated_backups.insert(
            (backup.region.clone(), backup.dbi_resource_id.clone()),
            backup,
        );
    }

    /// The retained automated backup of `region` that `reference` names, by either of the
    /// members that may name one ([`AutomatedBackupReference`]).
    pub fn retained_instance_backup_named(
        &self,
        region: &str,
        reference: &AutomatedBackupReference,
    ) -> Option<&RetainedInstanceBackupRecord> {
        self.retained_instance_backups_in(region)
            .into_iter()
            .find(|backup| {
                reference.names(&backup.dbi_resource_id, &backup.automated_backups_arn())
            })
    }

    /// The DB instance of `region` whose own `active` automated backup `reference` names —
    /// the backups that go with a LIVE instance, which `DeleteDBInstance` is what removes.
    ///
    /// Reads one region's ordered range, bounded by that region's DB instance quota.
    pub fn instance_with_automated_backup_named(
        &self,
        region: &str,
        reference: &AutomatedBackupReference,
    ) -> Option<&DbInstanceRecord> {
        self.instances_in(region).into_iter().find(|instance| {
            instance.has_automated_backup()
                && reference.names(&instance.dbi_resource_id, &instance.automated_backups_arn())
        })
    }

    /// Remove that backup, answering the record removed: what
    /// `DeleteDBInstanceAutomatedBackup` does, after which the backup reads as gone.
    pub fn remove_retained_instance_backup(
        &mut self,
        region: &str,
        dbi_resource_id: &str,
    ) -> Option<RetainedInstanceBackupRecord> {
        let key = self
            .retained_instance_backups_in(region)
            .into_iter()
            .find(|backup| backup.dbi_resource_id.eq_ignore_ascii_case(dbi_resource_id))
            .map(|backup| (region.to_string(), backup.dbi_resource_id.clone()))?;
        self.db_instance_automated_backups.remove(&key)
    }
}
