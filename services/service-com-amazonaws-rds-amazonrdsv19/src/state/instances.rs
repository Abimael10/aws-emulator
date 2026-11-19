// SPDX-License-Identifier: CC0-1.0

use super::*;

/// What a master user password is reported AS wherever Amazon RDS reports that one is
/// pending: the member is `@sensitive` (`PendingModifiedValues$MasterUserPassword`), so
/// the credential itself is never published and the value stands in for it.
pub const MASKED_PENDING_MASTER_USER_PASSWORD: &str = "****";

/// The Availability Zone the standby of a Multi-AZ DB instance in `region` is placed in,
/// given the zone its primary runs in; `None` for an instance that is not a Multi-AZ
/// deployment, which has no standby at all.
///
/// Source: `DBInstance$SecondaryAvailabilityZone` — "the name of the secondary Availability
/// Zone for a DB instance with multi-AZ support" — reported for a Multi-AZ deployment,
/// whose standby is in a DIFFERENT zone of the same Region from its primary. WHICH of the
/// Region's other zones holds it is the service's own choice; this emulator takes the first
/// zone of the Region that is not the primary's ([`region_availability_zones`]), as it takes
/// the Region's first zone for a primary the request placed nowhere.
pub fn standby_availability_zone(region: &str, primary: &str, multi_az: bool) -> Option<String> {
    if !multi_az {
        return None;
    }
    region_availability_zones(region)
        .into_iter()
        .find(|zone| zone != primary)
}

/// A stored DB instance: everything the service persists about it.
///
/// The ARN, the endpoint DNS name and the hosted zone id are NOT stored as text:
/// they are functions of the stored region, identifier and minted endpoint host, and
/// [`DbInstanceRecord::to_wire`] computes them. That is what keeps them consistent
/// after `ModifyDBInstance` renames an instance — a stored ARN would still name the
/// old identifier.
#[derive(Debug, Clone)]
pub struct DbInstanceRecord {
    pub identifier: String,
    /// The region the instance was created in; the scope of its ARN and endpoint.
    pub region: String,
    pub dbi_resource_id: String,
    /// The server-assigned middle label of the endpoint DNS name, minted once.
    pub endpoint_host: String,
    pub db_instance_class: String,
    pub engine: String,
    pub engine_version: Option<String>,
    pub status: String,
    pub pending_status: Option<PendingStatus>,
    pub db_name: Option<String>,
    pub allocated_storage: Option<i32>,
    pub max_allocated_storage: Option<i32>,
    pub master_username: Option<String>,
    /// The master user's password, as the DIGESTS a login on the instance's engine
    /// endpoint is verified against — never the password itself, which is `@sensitive`,
    /// is reported by no operation and is not stored (see
    /// [`data_plane::wire_session::MasterPasswordHash`]). `None` for an instance created
    /// without one, which no engine session can then authenticate against.
    pub master_user_password: Option<data_plane::wire_session::MasterPasswordHash>,
    /// `MasterUserSecret`: the secret RDS manages for this instance's master user
    /// password, for an instance created or modified with `ManageMasterUserPassword`
    /// turned on; `None` for one whose password the caller supplied, which has no managed
    /// secret to report.
    ///
    /// Only an operation that CREATES the secret for this instance sets it, so what is
    /// named here is the instance's OWN secret: an instance that inherits another's master
    /// credential — a read replica, an instance restored from a snapshot — manages none
    /// and reports none.
    pub master_user_secret: Option<ManagedMasterSecret>,
    /// The port the instance's engine accepts connections on as the request configured it:
    /// what `Port` (and `DBPortNumber`) set, else the documented default of its engine
    /// family. Reported by `DbInstancePort` for a cluster member
    /// ([`DbInstanceRecord::db_instance_port`]).
    pub endpoint_port: i32,
    /// Where the instance's own engine listener accepts connections IN THIS PROCESS, for
    /// the engines whose wire this emulator speaks: the address the modelled `Endpoint`
    /// publishes, because that is the address and port a client can actually reach the
    /// engine on. `None` for an engine this emulator serves no data plane for, which keeps
    /// the DNS-form endpoint and the configured port.
    pub served_endpoint: Option<ServedEndpoint>,
    pub multi_az: bool,
    /// Whether the instance owes a Multi-AZ FAILOVER: a reboot was conducted "through a
    /// Multi-AZ failover" (`RebootDBInstanceMessage$ForceFailover`), so the standby is
    /// promoted as the instance comes back up and the two zones trade places
    /// ([`Self::promote_standby`]). `false` for an instance with no failover owed, which is
    /// every instance between reboots.
    pub pending_failover: bool,
    pub storage_type: String,
    pub storage_encrypted: bool,
    pub kms_key_id: Option<String>,
    pub iops: Option<i32>,
    pub license_model: Option<String>,
    pub availability_zone: String,
    /// `SecondaryAvailabilityZone` — "If present, specifies the name of the secondary
    /// Availability Zone for a DB instance with multi-AZ support": the zone this instance's
    /// STANDBY sits in.
    ///
    /// It is another zone of [`Self::region`] than [`Self::availability_zone`] — a Multi-AZ
    /// deployment keeps its standby in a different zone from its primary, which is what
    /// makes it one — and `None` exactly when [`Self::multi_az`] is false, since a Single-AZ
    /// deployment has no standby to name. [`Self::place_multi_az_standby`] is what keeps the
    /// two in step.
    pub secondary_availability_zone: Option<String>,
    pub backup_retention_period: i32,
    pub preferred_backup_window: Option<String>,
    pub preferred_maintenance_window: Option<String>,
    pub publicly_accessible: bool,
    pub auto_minor_version_upgrade: bool,
    pub copy_tags_to_snapshot: bool,
    pub deletion_protection: bool,
    pub db_subnet_group_name: Option<String>,
    /// The DB parameter group applied to the instance: the one the request named, or
    /// the default group of the engine version it was created with.
    pub db_parameter_group_name: Option<String>,
    /// The option group applied to the instance: the one the request named, or the
    /// default group of its engine and major engine version. It is a REFERENCE, which
    /// is what keeps a group an instance uses from being deleted under it
    /// (`DeleteOptionGroup`).
    pub option_group_name: Option<String>,
    pub vpc_security_group_ids: Vec<String>,
    /// The legacy EC2-Classic DB security groups the request named, which is normally
    /// none: "This setting applies to the legacy EC2-Classic platform, which is no
    /// longer used to create new DB instances."
    pub db_security_group_names: Vec<String>,
    pub db_cluster_identifier: Option<String>,
    /// `PromotionTier`: "the order of priority in which an Aurora Replica is promoted to
    /// the primary instance after a failure of the existing primary instance" — what the
    /// request set, else the documented default [`DEFAULT_PROMOTION_TIER`].
    ///
    /// Stored for every instance, because every request that creates or modifies one can
    /// name it, and reported for the instances it orders: the members of a DB cluster
    /// ([`Self::promotion_tier`]).
    pub promotion_tier: i32,
    /// `ReadReplicaSourceDBInstanceIdentifier` — "the identifier of the source DB instance
    /// if this DB instance is a read replica", as `CreateDBInstanceReadReplica` resolved it,
    /// and `None` for an instance that replicates nothing.
    pub read_replica_source_db_instance_identifier: Option<String>,
    /// `ReadReplicaSourceDBClusterIdentifier` — "the identifier of the source DB cluster if
    /// this DB instance is a read replica", for the OTHER source
    /// `CreateDBInstanceReadReplica` creates a replica from: a Multi-AZ DB cluster
    /// (`SourceDBClusterIdentifier`). `None` for an instance that replicates no cluster.
    ///
    /// At most one of the two source members is ever set, because a replica replicates ONE
    /// source. The cluster's half of the relationship is NOT stored on the cluster: it is
    /// read off the instances that name it, as everything a cluster reports about the
    /// records around it is ([`ClusterMembership`]).
    pub read_replica_source_db_cluster_identifier: Option<String>,
    /// `ReadReplicaDBInstanceIdentifiers` — "the identifiers of the read replicas associated
    /// with this DB instance", in the order the replicas were created.
    ///
    /// The SOURCE's side of the same relationship, recorded on the source when a replica is
    /// created and dropped from it when that replica is deleted
    /// ([`Self::detach_read_replica`]), so the list never names an instance this store no
    /// longer holds.
    pub read_replica_db_instance_identifiers: Vec<String>,
    pub domain: Option<String>,
    /// The CA the instance's server certificate is issued under.
    pub ca_certificate_identifier: String,
    pub monitoring_interval: i32,
    pub performance_insights_enabled: bool,
    /// `PerformanceInsightsKMSKeyId`: the key the instance's Performance Insights data is
    /// encrypted under, for an instance whose request named one.
    pub performance_insights_kms_key_id: Option<String>,
    /// `PerformanceInsightsRetentionPeriod`: "the number of days to retain Performance
    /// Insights data", as the request configured it — the documented default
    /// ([`DEFAULT_PERFORMANCE_INSIGHTS_RETENTION_PERIOD`]) for an instance that has
    /// Performance Insights on and named no period, and `None` for one that collects no
    /// Performance Insights data at all, which retains none.
    pub performance_insights_retention_period: Option<i32>,
    pub iam_database_authentication_enabled: bool,
    pub database_insights_mode: String,
    pub dedicated_log_volume: bool,
    pub customer_owned_ip_enabled: bool,
    pub network_type: String,
    pub backup_target: String,
    pub engine_lifecycle_support: String,
    /// The storage throughput provisioned for the instance, in MiBps.
    pub storage_throughput: i32,
    pub instance_create_time: ::aws_smithy_types::DateTime,
    /// When this instance's database engine last came UP: the create that provisioned it,
    /// or the most recent restart of the engine (`RebootDBInstance`, `StartDBInstance`).
    ///
    /// Separate from [`Self::instance_create_time`], which is when the INSTANCE was
    /// created and never moves. This is the engine's run, which is what the data plane's
    /// `pg_postmaster_start_time()` reports and what decides whether a parameter change
    /// that waits for a reboot is in force yet
    /// ([`data_plane::pg_server::running_parameters`]).
    pub engine_started_at: ::aws_smithy_types::DateTime,
    pub tags: Vec<Tag>,
    /// `AssociatedRoles`: the IAM roles associated with this instance, in the order they
    /// were associated — what `AddRoleToDBInstance` adds to and
    /// `RemoveRoleFromDBInstance` removes from, bounded by
    /// [`associated_roles::MAX_ASSOCIATED_ROLES`] entries.
    pub associated_roles: Vec<DBInstanceRole>,
    /// What a modification the caller did not ask to apply immediately staged for the next
    /// maintenance window: the settings the instance is NOT running under yet, which is
    /// what `PendingModifiedValues` reports ([`DeferredModification`]). `None` for an
    /// instance with nothing staged.
    pub deferred_modification: Option<DeferredModification>,
    /// `EnabledCloudwatchLogsExports`: "a list of log types that this DB instance is
    /// configured to export to CloudWatch Logs" — the types the instance was created with
    /// (`EnableCloudwatchLogsExports`) plus every type a modification enabled and minus every
    /// type one disabled (`CloudwatchLogsExportConfiguration`), in the order they were
    /// enabled. EMPTY for an instance exporting none, which exports nothing at all
    /// ([`data_plane::log_export`]).
    pub enabled_cloudwatch_logs_exports: Vec<String>,
    /// The instance's SERVER log: the lifecycle lines this service has recorded for it and
    /// the lines its engine wrote while serving sessions, which is what `DescribeDBLogFiles`
    /// lists and `DownloadDBLogFilePortion` serves. See [`engine_log`] for the bounds on it.
    pub engine_log: InstanceLog,
    /// The MySQL family's GENERAL QUERY LOG, a log file of its own: the statements the engine
    /// ran while `general_log` was on ([`data_plane::statement_log`]). EMPTY for an instance
    /// whose engine has written none, which has no such file at all.
    pub general_query_log: InstanceLog,
    /// The recovery points this instance's CONTINUOUS BACKUP holds, oldest first: the data it
    /// held at each instant a session committed a change, which is what
    /// `RestoreDBInstanceToPointInTime` restores an instant's data from
    /// ([`data_plane::continuous_backup`]). EMPTY for an instance with automated backups off,
    /// which keeps none, and for one nothing has written to yet.
    pub recovery_points: Vec<data_plane::continuous_backup::RecoveryPoint>,
}

impl DbInstanceRecord {
    /// Apply a change whose due instant has passed. Returns whether the record is now
    /// gone — an accepted delete that has run its course — so the caller drops it from
    /// the store.
    pub(super) fn settle(&mut self, now: Instant) -> bool {
        self.settle_deferred_modification();
        let due = self
            .pending_status
            .as_ref()
            .is_some_and(|pending| now >= pending.due_at);
        if !due {
            return false;
        }
        match self.pending_status.take().map(|pending| pending.change) {
            Some(PendingChange::Status(status)) => {
                // The instance's own log records the lifecycle events this service
                // performs on it, and a status the clock just made due is one of them.
                self.engine_log
                    .record(format!("DB instance status is now {status}"));
                self.status = status;
                // A reboot "conducted through a Multi-AZ failover" promotes the standby, and
                // the promotion is part of coming back up: the zones have traded places by
                // the time the instance reports itself available again, never while it is
                // still out.
                if self.pending_failover {
                    self.pending_failover = false;
                    self.promote_standby();
                }
            }
            Some(PendingChange::Removal) => return true,
            // Unreachable: the change was there a line ago, under this one `&mut self`.
            // Answered as "nothing was due" rather than with a panic in a request path.
            None => {}
        }
        false
    }

    /// Apply a staged modification the clock has made due: the instant `ApplyImmediately`
    /// asked for it "as soon as possible", or the maintenance window a deferred one waits
    /// for — "changes to the DB instance are applied during the next maintenance window" —
    /// after which the instance runs under the staged settings and has nothing pending.
    ///
    /// Nothing is applied before then: a modification staged for a window that has not come
    /// round stays pending however often the instance is read.
    fn settle_deferred_modification(&mut self) {
        let due = self
            .deferred_modification
            .as_ref()
            .is_some_and(|staged| {
                staged.is_due(self.preferred_maintenance_window.as_deref(), Instant::now())
            });
        if !due {
            return;
        }
        if let Some(staged) = self.deferred_modification.take() {
            self.engine_log.record("applying pending modified values");
            staged.apply(self);
        }
    }

    /// Apply whatever is staged NOW, whether or not its window has come round: what
    /// `ApplyImmediately` asks for ("the modifications in this request and any pending
    /// modifications are asynchronously applied as soon as possible"), and what the reboot
    /// a deferred change waits for does ("Some parameter changes ... are applied on the
    /// next call to RebootDBInstance").
    ///
    /// Returns whether anything was applied, so a caller can report the instance
    /// `modifying` for a change this call really made.
    pub fn apply_deferred_modification(&mut self) -> bool {
        match self.deferred_modification.take() {
            Some(staged) => {
                staged.apply(self);
                true
            }
            None => false,
        }
    }

    /// Place this instance's Multi-AZ standby, or take it away: a Multi-AZ deployment has a
    /// standby in another Availability Zone of its own Region and names that zone, and a
    /// Single-AZ deployment has no standby and names none.
    ///
    /// Call it wherever `MultiAZ` is set — a create, a restore, a replica, a modification —
    /// so the setting and the zone pair cannot drift apart. A standby already placed in a
    /// zone of this Region other than the primary's STAYS where it is, so a failover that
    /// moved the pair is not undone by the next request that sets `MultiAZ` again.
    pub fn place_multi_az_standby(&mut self) {
        let already_placed = self
            .secondary_availability_zone
            .as_deref()
            .is_some_and(|zone| {
                zone != self.availability_zone
                    && region_availability_zones(&self.region)
                        .iter()
                        .any(|known| known == zone)
            });
        if self.multi_az && already_placed {
            return;
        }
        self.secondary_availability_zone =
            standby_availability_zone(&self.region, &self.availability_zone, self.multi_az);
    }

    /// Turn Multi-AZ on or off, placing or removing the standby with it: the deployment a
    /// `MultiAZ` setting asks for is the one with a standby, so the two are set together.
    pub fn set_multi_az(&mut self, multi_az: bool) {
        self.multi_az = multi_az;
        self.place_multi_az_standby();
    }

    /// Promote the standby to primary, which is what a Multi-AZ failover DOES: the instance
    /// runs in the zone its standby was in, and its standby is in the zone the former
    /// primary was in.
    ///
    /// Returns whether anything was promoted — `false` for an instance with no standby,
    /// whose placement is left alone.
    pub fn promote_standby(&mut self) -> bool {
        let Some(standby) = self.secondary_availability_zone.take() else {
            return false;
        };
        self.secondary_availability_zone =
            Some(::std::mem::replace(&mut self.availability_zone, standby));
        true
    }

    /// Whether a delete has already been accepted for this instance, so it is on its
    /// way out and nothing further can be done to it.
    pub fn is_deleting(&self) -> bool {
        self.status == DB_INSTANCE_STATUS_DELETING
    }

    /// Accept a delete: the instance reports `deleting` from now on, and the clock
    /// owes its removal from the store [`STATUS_TRANSITION_DELAY`] from now, after
    /// which every read of the identifier is a `DBInstanceNotFoundFault`. Nothing
    /// sleeps: the due instant is stored and materialised by
    /// [`ComAmazonawsRdsAmazonrdsv19State::settle_due_transitions`].
    pub fn begin_deletion(&mut self) {
        self.status = DB_INSTANCE_STATUS_DELETING.to_string();
        self.pending_status = Some(PendingStatus::removal_in_a_moment());
    }

    /// Record that `identifier` replicates this instance: the source's half of the
    /// relationship `CreateDBInstanceReadReplica` creates.
    ///
    /// A replica is recorded once, so a create that is refused after this point cannot leave
    /// the identifier named twice.
    pub fn attach_read_replica(&mut self, identifier: &str) {
        if !self
            .read_replica_db_instance_identifiers
            .iter()
            .any(|held| held == identifier)
        {
            self.read_replica_db_instance_identifiers
                .push(identifier.to_string());
        }
    }

    /// Drop `identifier` from the replicas of this instance, for a replica that is being
    /// deleted: the source reports the replicas it HAS, so a deleted one is not one of them.
    pub fn detach_read_replica(&mut self, identifier: &str) {
        self.read_replica_db_instance_identifiers
            .retain(|held| held != identifier);
    }

    /// Stop replicating: an instance whose source DB instance is being deleted is promoted
    /// to a standalone DB instance, so it reports no source from then on.
    ///
    /// Both sources are cleared, because a promotion ends the replication whichever kind of
    /// resource it was from — a DB instance or a Multi-AZ DB cluster.
    pub fn promote_from_read_replica(&mut self) {
        self.read_replica_source_db_instance_identifier = None;
        self.read_replica_source_db_cluster_identifier = None;
    }

    /// Whether this instance REPLICATES another resource, which is what makes it a read
    /// replica: the source DB instance or the source Multi-AZ DB cluster
    /// `CreateDBInstanceReadReplica` resolved for it, and nothing once a promotion has
    /// cleared the relationship.
    pub fn is_read_replica(&self) -> bool {
        self.read_replica_source_db_instance_identifier.is_some()
            || self.read_replica_source_db_cluster_identifier.is_some()
    }

    /// The name of the log file a DB instance running `engine` writes, which follows from its
    /// engine; see [`engine_log::log_file_name`].
    pub fn log_file_name(&self) -> &'static str {
        engine_log::log_file_name(&self.engine)
    }

    /// The log FILES this instance has: its engine's server log, which every instance has,
    /// and the MySQL general query log where that engine has written one.
    ///
    /// A file the engine has written nothing to is not a file the instance has, which is why
    /// the general query log appears only once a statement has been logged to it
    /// ([`data_plane::statement_log`]): `DescribeDBLogFiles` lists "the DB log files for the
    /// DB instance", and `DownloadDBLogFilePortion` serves exactly those.
    pub fn log_files(&self) -> Vec<(&'static str, &InstanceLog)> {
        let mut files: Vec<(&'static str, &InstanceLog)> =
            vec![(self.log_file_name(), &self.engine_log)];
        if !self.general_query_log.entries.is_empty() {
            files.push((engine_log::MYSQL_GENERAL_LOG_FILE, &self.general_query_log));
        }
        files
    }

    /// `DBParameterGroupStatus$ParameterApplyStatus` for the group this instance names:
    /// whether the values that group holds are the ones its engine is RUNNING.
    ///
    /// `ModifyDBParameterGroup` documents the rule: "changes to dynamic parameters are
    /// applied immediately" while "changes to static parameters require a reboot ... before
    /// the change can take effect". So a group holding a change this instance's engine has
    /// not come up again since — one whose apply method or published apply type makes it
    /// pending ([`change_is_pending_for`]), set after the engine last started — leaves the
    /// membership `pending-reboot`, which is what tells a caller that a reboot is what applies
    /// it; every other membership is `in-sync`. Decided by the SAME comparison the engine data
    /// plane makes when it picks the value it is running
    /// ([`data_plane::pg_server::running_parameters`]), so the status a caller reads and the
    /// value a session reads cannot disagree.
    pub fn parameter_apply_status(
        &self,
        db_parameter_group: Option<&DbParameterGroupRecord>,
    ) -> &'static str {
        let pending = db_parameter_group.is_some_and(|group| {
            group.parameters.iter().any(|(name, setting)| {
                let spec = published_db_parameter(&group.family, name)
                    .map(|spec| published_spec_for_family(&group.family, spec));
                change_is_pending_for(spec.as_ref(), &setting.apply_method)
                    && setting.set_at.as_secs_f64() > self.engine_started_at.as_secs_f64()
            })
        });
        if pending {
            PARAMETER_APPLY_STATUS_PENDING_REBOOT
        } else {
            MEMBERSHIP_IN_SYNC
        }
    }

    /// The log file `named`, or `None` when this instance has no such file.
    pub fn log_file(&self, named: &str) -> Option<&InstanceLog> {
        self.log_files()
            .into_iter()
            .find(|(name, _)| *name == named)
            .map(|(_, log)| log)
    }

    /// `arn:aws:rds:<region>:<account>:db:<identifier>`, from the stored region and
    /// the instance's current identifier.
    pub fn arn(&self) -> String {
        db_instance_arn(&self.region, &self.identifier)
    }

    /// The DNS name this instance's endpoint resolves to:
    /// `<identifier>.<minted host>.<region>.<domain>`, with the domain of the control plane
    /// that owns the instance's engine — see [`endpoint_domain`].
    ///
    /// An instance whose engine this emulator SERVES publishes the host its own listener
    /// is bound to instead: that is the address a client connects to, and a name this
    /// machine does not resolve would be no endpoint at all.
    pub fn endpoint_address(&self) -> String {
        if let Some(served) = &self.served_endpoint {
            return served.host.clone();
        }
        format!(
            "{}.{}.{}.{}",
            self.identifier,
            self.endpoint_host,
            self.region,
            endpoint_domain(&self.engine)
        )
    }

    /// The port the instance's database engine listens on: the port its own engine listener
    /// was assigned where it HAS one, else the configured port.
    ///
    /// The core owns the port range the listener is bound in and takes no port from this
    /// service (`vendor/server/src/listeners.rs`), so the configured port
    /// ([`Self::endpoint_port`], which `Port` and `DBPortNumber` set) is what the instance
    /// records and the port the listener landed on is what it publishes — an engine endpoint
    /// whose port nothing answers on would be no endpoint at all. For an instance with no
    /// listener the two are the same value.
    ///
    /// An instance of an engine whose data plane this emulator serves as an API
    /// ([`engine_data_plane_is_an_api`]) has no engine listener of its own, so it publishes
    /// the port its engine listens on rather than the port this process is served on — the
    /// same reading its cluster follows ([`DbClusterRecord::published_port`]).
    pub fn published_port(&self) -> i32 {
        if engine_data_plane_is_an_api(&self.engine) {
            return self.endpoint_port;
        }
        self.served_endpoint
            .as_ref()
            .map_or(self.endpoint_port, |served| served.port)
    }

    /// When the server certificate issued at creation expires:
    /// [`SERVER_CERTIFICATE_VALIDITY`] after the instance was created. A function of
    /// the stored creation time rather than a stored instant, so it cannot drift from
    /// it.
    pub fn certificate_valid_till(&self) -> ::aws_smithy_types::DateTime {
        ::aws_smithy_types::DateTime::from_secs_f64(
            self.instance_create_time.as_secs_f64() + SERVER_CERTIFICATE_VALIDITY.as_secs_f64(),
        )
    }

    /// `StatusInfos`: "The status of a read replica. If the DB instance isn't a read
    /// replica, the value is blank."
    ///
    /// So an instance reports one entry exactly while it REPLICATES another resource — the
    /// relationship [`Self::is_read_replica`] reads, which `PromoteReadReplica` clears — and
    /// an instance that replicates nothing reports no member at all, which is the blank value
    /// the member documents. The entry's
    /// `StatusType` "is currently \"read replication\"", `Normal` "indicates whether the
    /// instance is operating normally (TRUE) or is in an error state (FALSE)" and `Status`
    /// is one of the values the member publishes for that status type
    /// ([`READ_REPLICATION_STATUS_REPLICATING`]).
    ///
    /// Nothing this service does stops or breaks the replication it set up — the replica's
    /// storage is loaded from the source's and the replica stays attached to it until a
    /// promotion or a delete ends the relationship — so a replica this store holds is
    /// replicating normally, and `Message` ("details of the error if there is an error for
    /// the instance ... if the instance isn't in an error state, this value is blank") is
    /// left out rather than carrying an error nothing reported.
    pub fn status_infos(&self) -> Option<Vec<DBInstanceStatusInfo>> {
        self.is_read_replica().then(|| {
            vec![DBInstanceStatusInfo {
                status_type: Some(READ_REPLICATION_STATUS_TYPE.to_string()),
                normal: Some(true),
                status: Some(READ_REPLICATION_STATUS_REPLICATING.to_string()),
                message: None,
            }]
        })
    }

    /// `PendingModifiedValues`: what this service has accepted for the instance and not
    /// finished applying.
    ///
    /// Two things are pending. A modification the caller did not ask to apply immediately
    /// is staged for the next maintenance window and is pending until that window comes
    /// round ([`DeferredModification`]) — the structure's own documentation, "changes that
    /// will be applied during the next maintenance window". And an instance being
    /// provisioned with a master credential has that credential pending until it is up,
    /// which is what `PendingModifiedValues$MasterUserPassword` ("the master credentials
    /// for the DB instance") reports, masked because the member is `@sensitive`
    /// ([`MASKED_PENDING_MASTER_USER_PASSWORD`]).
    ///
    /// `None` once nothing is pending — the structure "is returned only when there are
    /// pending changes" — so an EMPTY element never claims there are changes whose
    /// subelements this answer does not identify.
    pub fn pending_modified_values(&self) -> Option<PendingModifiedValues> {
        let staged = self
            .deferred_modification
            .as_ref()
            .filter(|staged| !staged.is_empty())
            .map(DeferredModification::to_wire);
        let password_pending =
            self.status == DB_INSTANCE_STATUS_CREATING && self.master_user_password.is_some();
        match (staged, password_pending) {
            (Some(staged), false) => Some(staged),
            (Some(staged), true) => Some(PendingModifiedValues {
                master_user_password: Some(MASKED_PENDING_MASTER_USER_PASSWORD.to_string()),
                ..staged
            }),
            (None, true) => Some(PendingModifiedValues {
                master_user_password: Some(MASKED_PENDING_MASTER_USER_PASSWORD.to_string()),
                ..PendingModifiedValues::default()
            }),
            (None, false) => None,
        }
    }

    /// `DbInstancePort`: "The port that the DB instance listens on. If the DB instance
    /// is part of a DB cluster, this can be a different port than the DB cluster port."
    ///
    /// The member reports the port of a CLUSTER MEMBER, which is why it is separate from
    /// `Endpoint.Port`: an instance that belongs to no DB cluster has no such port, and
    /// RDS reports zero for it. It reports the CONFIGURED port — the one `Port` and
    /// `DBPortNumber` set, which the record keeps — where `Endpoint.Port` reports the
    /// port the instance's engine listener was assigned ([`Self::published_port`]): the
    /// endpoint has to be the one a client can reach, and this member is what still says
    /// which port the request asked the instance to listen on.
    pub fn db_instance_port(&self) -> i32 {
        match self.db_cluster_identifier {
            Some(_) => self.endpoint_port,
            None => 0,
        }
    }

    /// `PromotionTier`: "the order of priority in which an Aurora Replica is promoted to
    /// the primary instance after a failure of the existing primary instance."
    ///
    /// The order is the one a DB CLUSTER promotes its members in, so it is reported for an
    /// instance that belongs to one and not for an instance that belongs to none — the same
    /// reading [`Self::db_instance_port`] takes of the port a cluster member listens on.
    /// A cluster member always reports one, the value the request set or the documented
    /// default ([`DEFAULT_PROMOTION_TIER`]), which is what the stored value holds.
    pub fn promotion_tier(&self) -> Option<i32> {
        self.db_cluster_identifier
            .as_ref()
            .map(|_| self.promotion_tier)
    }

    /// `LatestRestorableTime`: "the latest time to which a database in this DB instance
    /// can be restored with point-in-time restore."
    ///
    /// An instance has one exactly while it keeps continuous backups, which is what
    /// `BackupRetentionPeriod` decides: "To disable automated backups, set this value to
    /// 0", and a non-zero period is what makes point-in-time restore available at all. So
    /// `None` for an instance with automated backups off, and `None` while the instance is
    /// still being created, which has taken no backup yet — in both cases there is no
    /// instant the instance can be restored to, which is what reporting no member says.
    ///
    /// The value ADVANCES with the clock and never predates the instance, in the STEPS the
    /// published point advances in ([`RECOVERY_POINT_INTERVAL`]): a client reads this instant,
    /// works out the window it may restore in from it and then calls the restore, so the two
    /// calls have to read the same value. This emulator's continuous backup records a recovery
    /// point at every commit ([`data_plane::continuous_backup`]) and nothing changes the
    /// instance's data between two commits, so the data of every instant the published point
    /// covers is held. Computed from the clock rather than stored, so it cannot go stale
    /// between two describes.
    pub fn latest_restorable_time(&self) -> Option<::aws_smithy_types::DateTime> {
        if !self.keeps_continuous_backups() || self.status == DB_INSTANCE_STATUS_CREATING {
            return None;
        }
        let step = RECOVERY_POINT_INTERVAL.as_secs().max(1) as i64;
        let now = now_as_wire_timestamp().secs();
        let published = now - now.rem_euclid(step);
        Some(::aws_smithy_types::DateTime::from_secs(
            published.max(self.instance_create_time.secs()),
        ))
    }

    /// The window a point-in-time restore of this instance may target: "you can restore to
    /// any point in time before the time identified by the `LatestRestorableTime` property.
    /// You can restore to a point up to the number of days specified by the
    /// `BackupRetentionPeriod` property."
    ///
    /// The lower bound is the retention period measured back from the published recovery point
    /// ([`Self::latest_restorable_time`]) — the span the automated backups cover, which is
    /// what the refusal of an earlier instant attributes the bound to
    /// ([`restore_time_before_the_window_message`]). The upper bound is that published point
    /// itself: "you can restore to any point in time BEFORE the time identified by the
    /// LatestRestorableTime property", so an instant the point has not reached yet is outside
    /// the window however recent the data this service holds is
    /// ([`restore_time_after_the_window_message`]).
    ///
    /// `None` for an instance that keeps no continuous backup, which is the case
    /// `RestoreDBInstanceToPointInTime` answers `PointInTimeRestoreNotEnabled` for.
    pub fn restorable_window(
        &self,
    ) -> Option<(::aws_smithy_types::DateTime, ::aws_smithy_types::DateTime)> {
        let published = self.latest_restorable_time()?;
        let retention = f64::from(self.backup_retention_period) * 24.0 * 60.0 * 60.0;
        Some((
            ::aws_smithy_types::DateTime::from_secs_f64(published.as_secs_f64() - retention),
            published,
        ))
    }

    /// Project the stored record onto the modelled `DBInstance` output shape.
    ///
    /// Every member RDS reports for a DB instance it holds is projected here, from the
    /// stored record or from the documented default the record was created with; the
    /// nested `Endpoint`, `CertificateDetails`, `PendingModifiedValues` and membership
    /// lists are projected from the same record, so no two of them can disagree. A member
    /// whose value this service has no state for at all is left out rather than invented —
    /// a list it does have state for is reported EMPTY, which is what RDS reports for an
    /// instance that has none of those associations.
    ///
    /// `db_subnet_group` is the DB SUBNET GROUP record the instance's own
    /// `DBSubnetGroupName` resolves to, which the store looks up for this projection
    /// ([`ComAmazonawsRdsAmazonrdsv19State::instance_to_wire`], the one way a handler
    /// reaches it): a nested resource of another record is reported FROM that record, so
    /// the group an instance reports and the one `DescribeDBSubnetGroups` reports are one
    /// answer. `None` where the instance names a group this store no longer resolves, which
    /// reports no group at all rather than a name with nothing behind it.
    /// `db_parameter_group` is the DB PARAMETER GROUP record the instance's own
    /// `DBParameterGroupName` resolves to, read by the store for this projection
    /// ([`ComAmazonawsRdsAmazonrdsv19State::db_parameter_group_of`]): whether the values that
    /// group holds are the ones the engine is RUNNING is what the membership's
    /// `ParameterApplyStatus` reports ([`Self::parameter_apply_status`]), and that is a fact
    /// about the group and this instance's engine run together.
    pub fn to_wire(
        &self,
        db_subnet_group: Option<&DbSubnetGroupRecord>,
        db_parameter_group: Option<&DbParameterGroupRecord>,
    ) -> DBInstance {
        DBInstance {
            db_instance_identifier: Some(self.identifier.clone()),
            db_instance_class: Some(self.db_instance_class.clone()),
            engine: Some(self.engine.clone()),
            db_instance_status: Some(self.status.clone()),
            master_username: self.master_username.clone(),
            // `MasterUserSecret`: reported for an instance whose master user password RDS
            // manages, and only for one — an instance whose password the caller supplied
            // has no secret of RDS's own, which is what reporting nothing says.
            master_user_secret: self
                .master_user_secret
                .as_ref()
                .map(ManagedMasterSecret::to_wire),
            db_name: self.db_name.clone(),
            endpoint: Some(Endpoint {
                address: Some(self.endpoint_address()),
                port: Some(self.published_port()),
                hosted_zone_id: Some(hosted_zone_id(
                    &self.region,
                    endpoint_domain(&self.engine),
                )),
            }),
            allocated_storage: self.allocated_storage,
            instance_create_time: Some(self.instance_create_time),
            preferred_backup_window: self.preferred_backup_window.clone(),
            backup_retention_period: Some(self.backup_retention_period),
            latest_restorable_time: self.latest_restorable_time(),
            // The legacy EC2-Classic memberships: empty for every instance created
            // through the VPC platform, which is every instance this service holds
            // unless a request named one.
            db_security_groups: Some(
                self.db_security_group_names
                    .iter()
                    .map(|name| DBSecurityGroupMembership {
                        db_security_group_name: Some(name.clone()),
                        status: Some(MEMBERSHIP_ACTIVE.to_string()),
                    })
                    .collect(),
            ),
            vpc_security_groups: Some(vpc_security_group_memberships(
                &self.region,
                &self.vpc_security_group_ids,
            )),
            db_parameter_groups: Some(
                self.db_parameter_group_name
                    .iter()
                    .map(|name| DBParameterGroupStatus {
                        db_parameter_group_name: Some(name.clone()),
                        parameter_apply_status: Some(
                            self.parameter_apply_status(db_parameter_group).to_string(),
                        ),
                    })
                    .collect(),
            ),
            option_group_memberships: Some(
                self.option_group_name
                    .iter()
                    .map(|name| OptionGroupMembership {
                        option_group_name: Some(name.clone()),
                        status: Some(MEMBERSHIP_IN_SYNC.to_string()),
                    })
                    .collect(),
            ),
            availability_zone: Some(self.availability_zone.clone()),
            // `SecondaryAvailabilityZone`: "If present, specifies the name of the secondary
            // Availability Zone for a DB instance with multi-AZ support" — the zone the
            // instance's standby sits in, reported from the stored placement, and reported
            // not at all for a Single-AZ deployment, which has no standby to name.
            secondary_availability_zone: self.secondary_availability_zone.clone(),
            // The DB subnet group the instance was created in, as the GROUP's own record
            // reports it: its description, its VPC, its status and its `Subnets` — each
            // subnet with the Availability Zone the service that owns subnets answered
            // with — are properties of that record, and a client reads them here to tell
            // which subnets and zones the instance can be placed in. Projected through
            // the group's one projection, so this copy cannot drift from what
            // `DescribeDBSubnetGroups` reports for the same group.
            db_subnet_group: db_subnet_group.map(DbSubnetGroupRecord::to_wire),
            preferred_maintenance_window: self.preferred_maintenance_window.clone(),
            // `PendingModifiedValues` is "returned only when there are pending changes":
            // what this instance still has pending, and nothing at all once it has none
            // ([`Self::pending_modified_values`]).
            pending_modified_values: self.pending_modified_values(),
            upgrade_rollout_order: Some(DEFAULT_UPGRADE_ROLLOUT_ORDER.to_string()),
            multi_az: Some(self.multi_az),
            engine_version: self.engine_version.clone(),
            auto_minor_version_upgrade: Some(self.auto_minor_version_upgrade),
            // The replicas of this instance, in the order they were created, and the source
            // it replicates itself. Both are stored halves of one relationship
            // `CreateDBInstanceReadReplica` creates, so an instance with no replica reports
            // an EMPTY list rather than none, and one that replicates nothing reports no
            // source at all.
            read_replica_db_instance_identifiers: Some(
                self.read_replica_db_instance_identifiers.clone(),
            ),
            read_replica_source_db_instance_identifier: self
                .read_replica_source_db_instance_identifier
                .clone(),
            // The other source a replica may have: the Multi-AZ DB cluster it replicates.
            // Reported from the same stored relationship, so a replica of a cluster names
            // its cluster here exactly as a replica of an instance names its instance above.
            read_replica_source_db_cluster_identifier: self
                .read_replica_source_db_cluster_identifier
                .clone(),
            // `StatusInfos`: the replication status of a READ REPLICA, and blank for an
            // instance that is not one ([`Self::status_infos`]).
            status_infos: self.status_infos(),
            license_model: self.license_model.clone(),
            iops: self.iops,
            storage_throughput: Some(self.storage_throughput),
            publicly_accessible: Some(self.publicly_accessible),
            storage_type: Some(self.storage_type.clone()),
            db_instance_port: Some(self.db_instance_port()),
            // `PromotionTier`: the failover order of a cluster member
            // ([`Self::promotion_tier`]).
            promotion_tier: self.promotion_tier(),
            db_cluster_identifier: self.db_cluster_identifier.clone(),
            storage_encrypted: Some(self.storage_encrypted),
            kms_key_id: self.kms_key_id.clone(),
            dbi_resource_id: Some(self.dbi_resource_id.clone()),
            ca_certificate_identifier: Some(self.ca_certificate_identifier.clone()),
            certificate_details: Some(CertificateDetails {
                ca_identifier: Some(self.ca_certificate_identifier.clone()),
                valid_till: Some(self.certificate_valid_till()),
            }),
            domain_memberships: Some(
                self.domain
                    .iter()
                    .map(|domain| DomainMembership {
                        domain: Some(domain.clone()),
                        status: Some("joined".to_string()),
                        ..DomainMembership::default()
                    })
                    .collect(),
            ),
            copy_tags_to_snapshot: Some(self.copy_tags_to_snapshot),
            monitoring_interval: Some(self.monitoring_interval),
            db_instance_arn: Some(self.arn()),
            iam_database_authentication_enabled: Some(self.iam_database_authentication_enabled),
            database_insights_mode: Some(self.database_insights_mode.clone()),
            performance_insights_enabled: Some(self.performance_insights_enabled),
            // The Performance Insights configuration the instance runs under: the
            // retention period it keeps that data for, and the key it is encrypted under
            // where the request named one. Reported from the instance's own record, so
            // what a create or a modification set is what a later read answers.
            performance_insights_kms_key_id: self.performance_insights_kms_key_id.clone(),
            performance_insights_retention_period: self.performance_insights_retention_period,
            deletion_protection: Some(self.deletion_protection),
            // "A list of Amazon Web Services Identity and Access Management (IAM) roles
            // associated with the DB instance": the associations this instance carries, in
            // the order they were made, and EMPTY for an instance that carries none.
            associated_roles: Some(self.associated_roles.clone()),
            max_allocated_storage: self.max_allocated_storage,
            // `EnabledCloudwatchLogsExports`: "a list of log types that this DB instance is
            // configured to export to CloudWatch Logs" — the types the instance exports NOW,
            // and an EMPTY list for one that exports none, which is what RDS reports for an
            // instance with no export configured.
            enabled_cloudwatch_logs_exports: Some(self.enabled_cloudwatch_logs_exports.clone()),
            tag_list: Some(self.tags.clone()),
            customer_owned_ip_enabled: Some(self.customer_owned_ip_enabled),
            network_type: Some(self.network_type.clone()),
            activity_stream_status: Some(ACTIVITY_STREAM_STOPPED.to_string()),
            backup_target: Some(self.backup_target.clone()),
            dedicated_log_volume: Some(self.dedicated_log_volume),
            // Nothing this service does changes an instance's storage file system
            // configuration, so no upgrade of it is ever recommended.
            is_storage_config_upgrade_available: Some(false),
            engine_lifecycle_support: Some(self.engine_lifecycle_support.clone()),
            ..DBInstance::default()
        }
    }
}
