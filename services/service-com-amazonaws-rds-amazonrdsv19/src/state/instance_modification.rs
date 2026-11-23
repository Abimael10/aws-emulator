// SPDX-License-Identifier: CC0-1.0

//! A modification the caller did NOT ask to apply immediately: the settings it staged on
//! the DB instance, and the maintenance window they are applied in.
//!
//! `ApplyImmediately`: "If this parameter is disabled, changes to the DB instance are
//! applied during the next maintenance window." Until then the instance keeps the values it
//! was running under, and the staged ones are what `PendingModifiedValues` reports —
//! "changes that will be applied during the next maintenance window".
//!
//! WHICH settings those are is the `PendingModifiedValues` structure itself: a setting it
//! can report is one the service defers to the window, and one it cannot is applied as soon
//! as possible whatever `ApplyImmediately` says. That is why this record's fields are that
//! structure's members and not every member `ModifyDBInstance` takes.

use super::*;

/// The settings one or more modifications staged for the next maintenance window.
///
/// Every field is a member of the modelled `PendingModifiedValues`, so what this record
/// holds is exactly what the instance reports as pending. Successive modifications MERGE
/// into one record ([`Self::stage`]): "the modifications in this request and any pending
/// modifications" are applied together, so the last value staged for a setting is the one
/// that lands.
#[derive(Debug, Clone, Default)]
pub struct DeferredModification {
    /// `DBInstanceIdentifier`: the identifier a staged rename gives the instance.
    pub identifier: Option<String>,
    pub db_instance_class: Option<String>,
    pub engine: Option<String>,
    pub engine_version: Option<String>,
    pub allocated_storage: Option<i32>,
    pub backup_retention_period: Option<i32>,
    pub multi_az: Option<bool>,
    pub storage_type: Option<String>,
    pub iops: Option<i32>,
    pub storage_throughput: Option<i32>,
    pub license_model: Option<String>,
    /// `Port`, which `ModifyDBInstance` names `DBPortNumber`.
    pub port: Option<i32>,
    pub db_subnet_group_name: Option<String>,
    pub ca_certificate_identifier: Option<String>,
    pub iam_database_authentication_enabled: Option<bool>,
    pub dedicated_log_volume: Option<bool>,
    /// The master user password a staged modification installs, as the digests a login on
    /// the instance's engine endpoint is verified against — the credential itself is
    /// `@sensitive` and is never stored or reported
    /// ([`MASKED_PENDING_MASTER_USER_PASSWORD`] is what the member carries).
    pub master_user_password: Option<data_plane::wire_session::MasterPasswordHash>,
    /// `PendingCloudwatchLogsExports`: the log types a staged modification turns ON and OFF
    /// for export to CloudWatch Logs ([`PendingLogExports`]). `None` for a modification that
    /// named no `CloudwatchLogsExportConfiguration`.
    pub cloudwatch_logs_exports: Option<PendingLogExports>,
    /// The instant this service OWES these settings, for a modification the caller asked to
    /// apply as soon as possible: `ApplyImmediately` "specifies whether the modifications in
    /// this request and any pending modifications are asynchronously applied as soon as
    /// possible, regardless of the PreferredMaintenanceWindow setting" — asynchronously, so
    /// the instance keeps the settings it is running under until that instant, reports them
    /// as pending until then, and reports itself `modifying` for exactly as long.
    ///
    /// `None` for a modification that waits for the maintenance window, which is what
    /// `ApplyImmediately` disabled asks for.
    pub apply_at: Option<Instant>,
}

/// The log types a staged modification turns on and off for export to CloudWatch Logs:
/// `CloudwatchLogsExportConfiguration`'s `EnableLogTypes` and `DisableLogTypes`, which is
/// what `PendingCloudwatchLogsExports` reports while the change is pending.
#[derive(Debug, Clone, Default)]
pub struct PendingLogExports {
    /// `LogTypesToEnable`: "the log types that are being enabled".
    pub enable: Vec<String>,
    /// `LogTypesToDisable`: "the log types that are being disabled".
    pub disable: Vec<String>,
}

impl DeferredModification {
    /// Whether `window` has come round, or the instant an immediate modification was owed
    /// has passed, so these settings are owed NOW.
    ///
    /// Two things make a staged modification due. `ApplyImmediately` asks for it "as soon as
    /// possible", which is the instant [`Self::apply_at`] records; otherwise "changes to the
    /// DB instance are applied during the next maintenance window", and the window is open
    /// ([`maintenance_window::window_is_open`]).
    ///
    /// `false` for a resource whose window cannot be read as a schedule and that owes no
    /// instant, which leaves the settings pending until a request applies them rather than
    /// applying them at a time no window names.
    pub fn is_due(&self, window: Option<&str>, now: Instant) -> bool {
        if self.apply_at.is_some_and(|due_at| now >= due_at) {
            return true;
        }
        window.is_some_and(|window| {
            maintenance_window::window_is_open(window, SystemTime::now())
        })
    }

    /// Whether anything is staged at all. An instance with nothing staged reports no
    /// `PendingModifiedValues` element: the structure "is returned only when there are
    /// pending changes".
    pub fn is_empty(&self) -> bool {
        self.identifier.is_none()
            && self.db_instance_class.is_none()
            && self.engine.is_none()
            && self.engine_version.is_none()
            && self.allocated_storage.is_none()
            && self.backup_retention_period.is_none()
            && self.multi_az.is_none()
            && self.storage_type.is_none()
            && self.iops.is_none()
            && self.storage_throughput.is_none()
            && self.license_model.is_none()
            && self.port.is_none()
            && self.db_subnet_group_name.is_none()
            && self.ca_certificate_identifier.is_none()
            && self.iam_database_authentication_enabled.is_none()
            && self.dedicated_log_volume.is_none()
            && self.master_user_password.is_none()
            && self.cloudwatch_logs_exports.is_none()
    }

    /// Take `staged` into this record, each setting the newer request named replacing what
    /// an earlier one staged for it: "the modifications in this request and any pending
    /// modifications" are applied together, in the first maintenance window that comes
    /// round after either was staged.
    pub fn stage(&mut self, staged: DeferredModification) {
        let DeferredModification {
            identifier,
            db_instance_class,
            engine,
            engine_version,
            allocated_storage,
            backup_retention_period,
            multi_az,
            storage_type,
            iops,
            storage_throughput,
            license_model,
            port,
            db_subnet_group_name,
            ca_certificate_identifier,
            iam_database_authentication_enabled,
            dedicated_log_volume,
            master_user_password,
            cloudwatch_logs_exports,
            apply_at,
        } = staged;
        replace(&mut self.identifier, identifier);
        replace(&mut self.db_instance_class, db_instance_class);
        replace(&mut self.engine, engine);
        replace(&mut self.engine_version, engine_version);
        replace(&mut self.allocated_storage, allocated_storage);
        replace(
            &mut self.backup_retention_period,
            backup_retention_period,
        );
        replace(&mut self.multi_az, multi_az);
        replace(&mut self.storage_type, storage_type);
        replace(&mut self.iops, iops);
        replace(&mut self.storage_throughput, storage_throughput);
        replace(&mut self.license_model, license_model);
        replace(&mut self.port, port);
        replace(&mut self.db_subnet_group_name, db_subnet_group_name);
        replace(
            &mut self.ca_certificate_identifier,
            ca_certificate_identifier,
        );
        replace(
            &mut self.iam_database_authentication_enabled,
            iam_database_authentication_enabled,
        );
        replace(&mut self.dedicated_log_volume, dedicated_log_volume);
        replace(&mut self.master_user_password, master_user_password);
        // The log types the newer request names are MERGED into what an earlier one staged:
        // each type the later request enables is one it enables, whichever list an earlier
        // request put it in.
        if let Some(staged) = cloudwatch_logs_exports {
            let held = self
                .cloudwatch_logs_exports
                .get_or_insert_with(PendingLogExports::default);
            held.enable.retain(|held| !staged.disable.contains(held));
            held.disable.retain(|held| !staged.enable.contains(held));
            for log_type in staged.enable {
                if !held.enable.contains(&log_type) {
                    held.enable.push(log_type);
                }
            }
            for log_type in staged.disable {
                if !held.disable.contains(&log_type) {
                    held.disable.push(log_type);
                }
            }
            // What one request may name is bounded where it enters the handler; what
            // SUCCESSIVE requests accumulate here is bounded by the same ceiling, so a
            // modification that stays pending cannot grow the staged record past the number
            // of log types the instance can carry applied.
            held.enable.truncate(MAX_CLUSTER_LIST_ENTRIES);
            held.disable.truncate(MAX_CLUSTER_LIST_ENTRIES);
        }
        // WHEN the staged settings are owed is the LAST request's answer, not an earlier
        // one's: `ApplyImmediately` "specifies whether the modifications in this request AND
        // ANY PENDING MODIFICATIONS are asynchronously applied as soon as possible,
        // regardless of the PreferredMaintenanceWindow setting", so a request that asks for
        // them at once pulls what an earlier one staged forward with it, and a request that
        // does NOT leaves the whole staged record for the maintenance window — which is what
        // `ApplyImmediately` disabled asks for. Keeping an earlier request's instant here
        // would apply a deferred modification at once because an immediate one came before
        // it, and the instance would report nothing pending the moment it was staged.
        self.apply_at = apply_at;
    }

    /// Write every staged setting onto `record`, which is what applying the modification
    /// MEANS: from this point the instance runs under the staged values and reports them
    /// as its own rather than as pending.
    ///
    /// The identifier is written with the rest, so the caller re-keys the record where a
    /// rename was staged ([`ComAmazonawsRdsAmazonrdsv19State::settle_due_transitions`]).
    pub fn apply(self, record: &mut DbInstanceRecord) {
        if let Some(value) = self.identifier {
            record.identifier = value;
        }
        if let Some(value) = self.db_instance_class {
            record.db_instance_class = value;
        }
        if let Some(value) = self.engine {
            record.engine = value;
        }
        if let Some(value) = self.engine_version {
            record.engine_version = Some(value);
        }
        if let Some(value) = self.allocated_storage {
            record.allocated_storage = Some(value);
        }
        if let Some(value) = self.backup_retention_period {
            record.backup_retention_period = value;
        }
        if let Some(value) = self.multi_az {
            // The standby goes with the setting: turning Multi-AZ on gives the instance one
            // in another zone of its Region, and turning it off leaves it with none
            // ([`DbInstanceRecord::set_multi_az`]).
            record.set_multi_az(value);
        }
        if let Some(value) = self.storage_type {
            record.storage_type = value;
        }
        if let Some(value) = self.iops {
            record.iops = Some(value);
        }
        if let Some(value) = self.storage_throughput {
            record.storage_throughput = value;
        }
        if let Some(value) = self.license_model {
            record.license_model = Some(value);
        }
        if let Some(value) = self.port {
            record.endpoint_port = value;
        }
        if let Some(value) = self.db_subnet_group_name {
            record.db_subnet_group_name = Some(value);
        }
        if let Some(value) = self.ca_certificate_identifier {
            record.ca_certificate_identifier = value;
        }
        if let Some(value) = self.iam_database_authentication_enabled {
            record.iam_database_authentication_enabled = value;
        }
        if let Some(value) = self.dedicated_log_volume {
            record.dedicated_log_volume = value;
        }
        if let Some(value) = self.master_user_password {
            record.master_user_password = Some(value);
        }
        // "The log types that are being enabled" and "being disabled": the enabled set the
        // instance reports is the stored one plus what the modification enables, less what it
        // disables.
        if let Some(exports) = self.cloudwatch_logs_exports {
            for log_type in exports.enable {
                if !record.enabled_cloudwatch_logs_exports.contains(&log_type) {
                    record.enabled_cloudwatch_logs_exports.push(log_type);
                }
            }
            for log_type in exports.disable {
                record
                    .enabled_cloudwatch_logs_exports
                    .retain(|enabled| *enabled != log_type);
            }
            record
                .enabled_cloudwatch_logs_exports
                .truncate(MAX_CLUSTER_LIST_ENTRIES);
        }
    }

    /// Project the staged settings onto the modelled `PendingModifiedValues`.
    ///
    /// The master user password is `@sensitive`, so the member carries the stand-in every
    /// pending credential is reported as ([`MASKED_PENDING_MASTER_USER_PASSWORD`]) rather
    /// than anything derived from the credential itself.
    pub fn to_wire(&self) -> PendingModifiedValues {
        PendingModifiedValues {
            db_instance_identifier: self.identifier.clone(),
            db_instance_class: self.db_instance_class.clone(),
            engine: self.engine.clone(),
            engine_version: self.engine_version.clone(),
            allocated_storage: self.allocated_storage,
            backup_retention_period: self.backup_retention_period,
            multi_az: self.multi_az,
            storage_type: self.storage_type.clone(),
            iops: self.iops,
            storage_throughput: self.storage_throughput,
            license_model: self.license_model.clone(),
            port: self.port,
            db_subnet_group_name: self.db_subnet_group_name.clone(),
            ca_certificate_identifier: self.ca_certificate_identifier.clone(),
            iam_database_authentication_enabled: self.iam_database_authentication_enabled,
            dedicated_log_volume: self.dedicated_log_volume,
            master_user_password: self
                .master_user_password
                .as_ref()
                .map(|_| MASKED_PENDING_MASTER_USER_PASSWORD.to_string()),
            // `PendingCloudwatchLogsExports`: "the log types that are being enabled" and
            // "being disabled", reported while the change is pending and not at all for an
            // instance with no log-export change staged.
            pending_cloudwatch_logs_exports: self.cloudwatch_logs_exports.as_ref().map(
                |exports| PendingCloudwatchLogsExports {
                    log_types_to_enable: Some(exports.enable.clone()),
                    log_types_to_disable: Some(exports.disable.clone()),
                },
            ),
            ..PendingModifiedValues::default()
        }
    }
}

/// Keep what `staged` names, and leave what it does not: a modification that named nothing
/// for a setting stages nothing for it, so an earlier request's value stands.
fn replace<T>(held: &mut Option<T>, staged: Option<T>) {
    if staged.is_some() {
        *held = staged;
    }
}
