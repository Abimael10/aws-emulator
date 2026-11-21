// SPDX-License-Identifier: CC0-1.0

//! The AUTOMATED DB snapshots this service takes of a DB instance that keeps automated
//! backups, which is what `DescribeDBSnapshots` reports under `SnapshotType` `automated`.
//!
//! `BackupRetentionPeriod` is what turns automated backups on — "to disable automated
//! backups, set this value to 0" — and an automated backup of a DB instance IS a snapshot
//! the service took without being asked: `DBSnapshot$SnapshotType` publishes `automated`
//! beside `manual`, and `DescribeDBSnapshots$SnapshotType` documents the value as "return
//! all DB snapshots that have been automatically taken by Amazon RDS for my Amazon Web
//! Services account".
//!
//! WHEN the first one exists is documented by the automated backup's own status
//! ([`DB_INSTANCE_AUTOMATED_BACKUP_STATUS_CREATING`] is "automated backups that are waiting
//! for the first automated snapshot to be available", and
//! [`DB_INSTANCE_AUTOMATED_BACKUP_STATUS_ACTIVE`] is the backup of a current instance): an
//! instance whose automated backup is no longer `creating` HAS its first automated snapshot,
//! and that snapshot is `available`. This service reports that backup as `active` from the
//! moment the instance leaves `creating` ([`DbInstanceRecord::automated_backup_status`]), so
//! the snapshot is materialised at exactly that instant — a change the CLOCK made due, which
//! is why it is written from [`ComAmazonawsRdsAmazonrdsv19State::settle_due_transitions`]
//! and not by any one caller.
//!
//! What it HOLDS is the instance's own continuous backup: the latest recovery point, which is
//! a copy of the instance's databases taken by the engine
//! ([`data_plane::continuous_backup`]). So an automated snapshot is restorable exactly as a
//! manual one is, out of the same kind of copy — never an empty database.
//!
//! One instance has one automated snapshot here, recorded in
//! [`ComAmazonawsRdsAmazonrdsv19State::automated_db_snapshots`] under the instance's
//! immutable `DbiResourceId`, so the records are bounded by the region's DB instance quota
//! and a rename of the instance cannot orphan one. It is taken ONCE — nothing retakes it,
//! and so no read can mint a second one — and it is dropped when the instance stops keeping
//! automated backups at all (its retention period set to 0, its delete accepted, the
//! instance gone), which is what "disable automated backups" does to the snapshots they
//! took.

use super::*;

/// The `SnapshotType` of a snapshot THIS SERVICE took of an instance that keeps automated
/// backups, from the values [`SNAPSHOT_TYPES`] lists: "automated - return all DB snapshots
/// that have been automatically taken by Amazon RDS".
pub const SNAPSHOT_TYPE_AUTOMATED: &str = "automated";

/// The form the identifier of an automated snapshot carries: `rds:` and the minute it was
/// taken, which is how Amazon RDS names the snapshots it takes itself
/// (`CopyDBSnapshot$SourceDBSnapshotIdentifier` gives one as an example: "you might specify
/// rds:mysql-instance1-snapshot-20130805").
const AUTOMATED_SNAPSHOT_MINUTE_FORM: &str = "%Y-%m-%d-%H-%M";

/// `DBSnapshotIdentifier` of the automated snapshot of `db_instance_identifier` taken at
/// `at`: `rds:<instance>-<YYYY-MM-DD-HH-MM>`.
///
/// The `rds:` prefix is what tells an automated snapshot's name from a manual one — no
/// caller can create an identifier carrying a colon ([`check_identifier`]) — and the minute
/// is the instant the snapshot was taken, so two snapshots of one instance cannot share a
/// name.
#[must_use]
pub fn automated_snapshot_identifier(
    db_instance_identifier: &str,
    at: ::aws_smithy_types::DateTime,
) -> String {
    let minute = ::chrono::DateTime::from_timestamp(at.secs(), 0)
        .map(|utc| utc.format(AUTOMATED_SNAPSHOT_MINUTE_FORM).to_string())
        .unwrap_or_default();
    format!("rds:{db_instance_identifier}-{minute}")
}

impl DbSnapshotRecord {
    /// The AUTOMATED snapshot of `instance`: the instance's settings and the data its
    /// continuous backup holds, as a snapshot of type [`SNAPSHOT_TYPE_AUTOMATED`].
    ///
    /// It is `available` the moment it exists, because it is published only once the backup
    /// it holds has been taken (see this module's documentation): there is no instant at
    /// which a caller could read it as still being made. `storage` is the copy the
    /// instance's own continuous backup recorded, and the tags are the instance's when it
    /// asked for them to be copied — `CopyTagsToSnapshot` is "specifies whether to copy tags
    /// from the DB instance to snapshots of the DB instance ... by default, tags aren't
    /// copied", which holds for the snapshots the service takes as much as for the ones a
    /// caller asks for.
    pub fn automated_of(
        instance: &DbInstanceRecord,
        snapshot_identifier: &str,
        storage: Option<::std::sync::Arc<data_plane::sql_engine::InstanceDatabase>>,
        tenant_databases: Vec<SnapshotTenantDatabaseRecord>,
    ) -> Self {
        let tags = match instance.copy_tags_to_snapshot {
            true => instance.tags.clone(),
            false => Vec::new(),
        };
        Self {
            snapshot_type: SNAPSHOT_TYPE_AUTOMATED.to_string(),
            status: SNAPSHOT_STATUS_AVAILABLE.to_string(),
            pending_status: None,
            ..Self::from_instance(
                instance,
                snapshot_identifier,
                tags,
                storage,
                tenant_databases,
            )
        }
    }

    /// Whether this snapshot is one the SERVICE took rather than one a caller asked for.
    #[must_use]
    pub fn is_automated(&self) -> bool {
        self.snapshot_type == SNAPSHOT_TYPE_AUTOMATED
    }
}

impl ComAmazonawsRdsAmazonrdsv19State {
    /// The manual DB snapshots of `region` — the ones a CALLER asked for, which is what the
    /// published manual-snapshot quota bounds.
    ///
    /// The snapshots this service took itself are not counted: they are bounded by the
    /// region's DB instance quota instead (one per instance), so an account that keeps
    /// automated backups does not lose manual snapshots to them.
    pub fn manual_snapshots_in(&self, region: &str) -> usize {
        self.snapshots_in(region)
            .into_iter()
            .filter(|record| !record.is_automated())
            .count()
    }

    /// Materialise the automated snapshots the clock owes the DB instances of `region`, and
    /// drop the ones whose instance no longer keeps automated backups.
    ///
    /// Called from [`Self::settle_due_transitions`] with the instance statuses already
    /// settled, so the one question this asks of an instance — whether its automated backup
    /// is `active`, i.e. whether its first automated snapshot has been taken — is answered
    /// against the status the clock has already made due. Nothing here depends on who
    /// called: an instance that came up while no request was in flight has its snapshot the
    /// moment the next one arrives, with the instant the backup was taken recorded on the
    /// record.
    ///
    /// Reads one region's ordered ranges, bounded by that region's DB instance quota.
    pub(super) fn settle_automated_db_snapshots(&mut self, region: &str) {
        // The instances that owe their first automated snapshot, and the entries whose
        // instance keeps no automated backups any more: its retention period set to 0, its
        // delete accepted, or the instance itself gone from this store.
        let mut owed: Vec<String> = Vec::new();
        let mut dropped: Vec<String> = Vec::new();
        for instance in self.instances_in(region) {
            let has = self
                .automated_db_snapshots
                .contains_key(&(region.to_string(), instance.dbi_resource_id.clone()));
            let keeps = instance.has_automated_backup()
                && instance.automated_backup_status()
                    == DB_INSTANCE_AUTOMATED_BACKUP_STATUS_ACTIVE;
            match (keeps, has) {
                (true, false) => owed.push(instance.identifier.clone()),
                (false, true) => dropped.push(instance.dbi_resource_id.clone()),
                _ => {}
            }
        }
        dropped.extend(
            self.automated_db_snapshots
                .range((region.to_string(), String::new())..)
                .take_while(|((stored, _), _)| stored == region)
                .map(|((_, resource_id), _)| resource_id)
                .filter(|resource_id| {
                    self.instance_by_resource_id(region, resource_id).is_none()
                })
                .cloned()
                .collect::<Vec<String>>(),
        );
        for resource_id in dropped {
            // The entry holds the identifier of the snapshot it took, so the snapshot goes
            // with the record of it rather than being looked for by any other name.
            if let Some(identifier) = self
                .automated_db_snapshots
                .remove(&(region.to_string(), resource_id))
            {
                self.db_snapshots.remove(&(region.to_string(), identifier));
            }
        }
        for identifier in owed {
            let key = (region.to_string(), identifier);
            let Some(instance) = self.db_instances.get(&key) else {
                continue;
            };
            let at = now_as_wire_timestamp();
            let snapshot_identifier = automated_snapshot_identifier(&key.1, at);
            // The data the instance's continuous backup holds: a copy taken by the engine
            // itself, so this snapshot restores the rows the instance had rather than an
            // empty database. An instance whose engine this emulator serves no data plane
            // for has recorded no point and the snapshot holds no copy, exactly as a manual
            // snapshot of it holds none.
            let storage = instance
                .latest_recovery_point()
                .map(|point| ::std::sync::Arc::clone(&point.storage));
            let resource_id = instance.dbi_resource_id.clone();
            let snapshot = DbSnapshotRecord::automated_of(
                instance,
                &snapshot_identifier,
                storage,
                self.capture_tenant_databases(region, &key.1),
            );
            self.db_snapshots.insert(
                (region.to_string(), snapshot_identifier.clone()),
                snapshot,
            );
            // Nothing is recorded in the region's event log: the log carries the changes
            // CALLERS made ([`Self::record_event`]), and this snapshot is the one the
            // instance's own backup window took — the lifecycle events published for it
            // belong to the backup of the DB INSTANCE, which this service does not publish.
            self.automated_db_snapshots
                .insert((region.to_string(), resource_id), snapshot_identifier);
        }
    }
}
