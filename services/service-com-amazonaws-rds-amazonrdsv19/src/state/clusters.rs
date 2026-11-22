// SPDX-License-Identifier: CC0-1.0

use super::*;

/// What a DB cluster reports about the records AROUND it.
///
/// A cluster's members and read replicas are other records — the DB instances that name
/// it and the clusters that replicate from it — so they are READ from those records when
/// the cluster is projected, never copied onto the cluster record: a copy would still
/// claim a member after the instance was deleted. Built by
/// [`ComAmazonawsRdsAmazonrdsv19State::cluster_to_wire`], which is the only way a handler
/// projects a cluster.
#[derive(Debug, Default)]
pub struct ClusterMembership {
    /// `DBClusterMembers`: the DB instances that make up the cluster, writer first.
    pub members: Vec<DBClusterMember>,
    /// `ReadReplicaIdentifiers`: the clusters whose replication source is this one.
    pub read_replica_identifiers: Vec<String>,
    /// `MultiAZ`: whether the member instances sit in more than one Availability Zone.
    pub multi_az: bool,
    /// The immutable resource id of the global database cluster the cluster belongs to,
    /// read from that global cluster's own record — `None` for a cluster that belongs to
    /// none. Read rather than copied onto the cluster, for the same reason its members are:
    /// a copy would still name a global cluster the cluster was removed from.
    pub global_cluster_resource_id: Option<String>,
    /// Whether a backup of the cluster is in flight — a snapshot OF it is still being taken
    /// — which is the `backing-up` status the cluster reports while it lasts; see
    /// [`DbClusterRecord::reported_status`].
    pub backing_up: bool,
    /// `CustomEndpoints`: "the custom endpoints associated with the DB cluster", as the
    /// addresses they publish.
    ///
    /// Read from the endpoint records that NAME this cluster, as the members above are read
    /// off the instances: a copy on the cluster would still report an endpoint a
    /// `DeleteDBClusterEndpoint` took away. Empty for a cluster nobody created one for,
    /// which is what RDS reports for a cluster that has none.
    pub custom_endpoints: Vec<String>,
    /// `LimitlessDatabase`: "the details for Aurora Limitless Database" — the state of the
    /// Limitless database of a cluster that operates as one, and `None` for a cluster that
    /// does not ([`DbClusterRecord::limitless_database`]).
    ///
    /// Read from the DB SHARD GROUPS that name this cluster, as the members above are read
    /// off the instances: whether anything runs on the Limitless database is a fact about
    /// those records, so a copy on the cluster would still claim one a delete took away.
    pub limitless_database: Option<LimitlessDatabase>,
}

/// A stored DB cluster: everything the service persists about it.
///
/// As with a DB instance record, the ARN, the two cluster endpoints and the hosted zone
/// id are NOT stored as text: each is a function of the stored region, identifier and
/// minted endpoint host, so none of them can be left naming an identifier the cluster no
/// longer has. The cluster's members are not stored either — see [`ClusterMembership`].
#[derive(Debug, Clone)]
pub struct DbClusterRecord {
    /// `DBClusterIdentifier`, stored lowercased: "this parameter is stored as a lowercase
    /// string".
    pub identifier: String,
    /// The region the cluster was created in; the scope of its ARN and endpoints.
    pub region: String,
    /// `DbClusterResourceId`: the immutable, server-minted id.
    pub resource_id: String,
    /// The server-assigned middle label of the cluster endpoint DNS names, minted once.
    pub endpoint_host: String,
    /// The address a client's connection to this cluster is ANSWERED at: the cluster's own
    /// engine listener for an engine whose native wire this emulator serves
    /// ([`data_plane::cluster_endpoint::cluster_engine_wire`]), or this emulator's own
    /// endpoint for an engine whose data plane it serves as an API instead
    /// ([`crate::state::api_plane_endpoint`]); `None` for a cluster whose data plane this
    /// emulator does not serve at all.
    ///
    /// The two endpoint members are read off this address differently: the WRITER endpoint
    /// IS this host, because that is where a session on this cluster is answered
    /// ([`Self::endpoint`]), while the READER endpoint keeps the name the cluster mints from
    /// its identifier ([`Self::reader_endpoint`]), which is the only thing that tells a read
    /// path from a write path. This address also decides the zone a minted name is announced
    /// in ([`Self::endpoint_dns_suffix`]) and the `Port` published beside both
    /// ([`Self::published_port`]). A cluster whose plane is served as an API is addressed at
    /// this host for every one of its endpoints — the two members and each CUSTOM endpoint in
    /// front of it — because that plane answers HTTP there and nowhere else.
    pub served_endpoint: Option<ServedEndpoint>,
    /// `Status`.
    pub status: String,
    /// The transition the clock owes this cluster, if any.
    pub pending_status: Option<PendingStatus>,
    /// The instant the clock makes — or made — this cluster READY for the first time, as
    /// [`DbInstanceRecord::ready_at`] records it for a DB instance: what a resource
    /// provisioned FROM this cluster (a member, a cluster snapshot) charges its own span
    /// from ([`provisioning_to`]).
    pub ready_at: Instant,
    /// `Engine`.
    pub engine: String,
    /// `EngineVersion`: the version the cluster RUNS, resolved against the engine-version
    /// catalogue rather than the string the request carried.
    pub engine_version: Option<String>,
    /// `EngineMode`.
    pub engine_mode: String,
    /// `DatabaseName`.
    pub database_name: Option<String>,
    /// `MasterUsername`. The password is never stored: the member is `@sensitive` and RDS
    /// reports nothing carrying it.
    pub master_username: Option<String>,
    /// `MasterUserSecret`: the secret RDS manages for this cluster's master user
    /// password, for a cluster created or modified with `ManageMasterUserPassword` turned
    /// on; `None` for one whose password the caller supplied, which has no managed secret
    /// to report.
    pub master_user_secret: Option<ManagedMasterSecret>,
    /// The master user's password, kept only as the DIGESTS a login on the cluster's own
    /// engine endpoint can be verified against — the password the request carried, or the
    /// one RDS generated into the managed secret. The password itself is `@sensitive` and
    /// is never stored or reported; see [`data_plane::wire_session::MasterPasswordHash`].
    pub master_user_password: Option<data_plane::wire_session::MasterPasswordHash>,
    /// `Port`: the port the instances in the cluster accept connections on.
    pub port: i32,
    /// `AvailabilityZones`: where instances in the cluster can be created.
    pub availability_zones: Vec<String>,
    /// `BackupRetentionPeriod`.
    pub backup_retention_period: i32,
    /// `PreferredBackupWindow`.
    pub preferred_backup_window: String,
    /// `PreferredMaintenanceWindow`.
    pub preferred_maintenance_window: String,
    /// `DBClusterParameterGroup`: the group the request named, or the default group of the
    /// engine version the cluster was created with; `None` where this service publishes no
    /// version for the engine at all, so there is no family to name a default group after.
    pub db_cluster_parameter_group_name: Option<String>,
    /// `DBSubnetGroup`: the subnet group the cluster was created in, resolved in the store
    /// before the cluster was stored.
    pub db_subnet_group_name: Option<String>,
    /// The option group the request named, reported as the cluster's one option group
    /// membership.
    pub option_group_name: Option<String>,
    /// `CharacterSetName`.
    pub character_set_name: Option<String>,
    /// `VpcSecurityGroups`: the EC2 VPC security group ids the request named.
    pub vpc_security_group_ids: Vec<String>,
    /// `StorageEncrypted`.
    pub storage_encrypted: bool,
    /// `KmsKeyId`.
    pub kms_key_id: Option<String>,
    /// The storage type the cluster was created with, or `None` for an Aurora cluster on
    /// the default [`AURORA_DEFAULT_STORAGE_TYPE`], which the member documents as not
    /// reported.
    pub storage_type: Option<String>,
    /// `Iops`.
    pub iops: Option<i32>,
    /// `AllocatedStorage`.
    pub allocated_storage: Option<i32>,
    /// `DBClusterInstanceClass`, for a Multi-AZ DB cluster.
    pub db_cluster_instance_class: Option<String>,
    /// `PubliclyAccessible`, for a Multi-AZ DB cluster; `None` for an Aurora cluster,
    /// whose member is "only for Multi-AZ DB clusters".
    pub publicly_accessible: Option<bool>,
    /// `AutoMinorVersionUpgrade`.
    pub auto_minor_version_upgrade: bool,
    /// `DeletionProtection`.
    pub deletion_protection: bool,
    /// `CopyTagsToSnapshot`.
    pub copy_tags_to_snapshot: bool,
    /// `HttpEndpointEnabled`: whether the RDS Data API endpoint is on for this cluster.
    pub http_endpoint_enabled: bool,
    /// `IAMDatabaseAuthenticationEnabled`.
    pub iam_database_authentication_enabled: bool,
    /// `BacktrackWindow`, in seconds; zero disables backtracking.
    pub backtrack_window: i64,
    /// `EnabledCloudwatchLogsExports`.
    pub enabled_cloudwatch_logs_exports: Vec<String>,
    /// `NetworkType`, for an Aurora cluster; `None` for a Multi-AZ DB cluster, whose
    /// member is "only for Aurora DB clusters".
    pub network_type: Option<String>,
    /// `MonitoringInterval`.
    pub monitoring_interval: i32,
    /// `MonitoringRoleArn`.
    pub monitoring_role_arn: Option<String>,
    /// `DatabaseInsightsMode`.
    pub database_insights_mode: String,
    /// `PerformanceInsightsEnabled`: whether Performance Insights is on for this cluster,
    /// as a request configured it. `None` for a cluster no request ever configured
    /// Performance Insights on: the member is a `BooleanOptional` the model gives no
    /// default, so a cluster that carries no setting for it reports none rather than a
    /// value nothing asked for.
    pub performance_insights_enabled: Option<bool>,
    /// `PerformanceInsightsKMSKeyId`.
    pub performance_insights_kms_key_id: Option<String>,
    /// `PerformanceInsightsRetentionPeriod`.
    pub performance_insights_retention_period: Option<i32>,
    /// `ServerlessV2ScalingConfiguration`: the Aurora Serverless v2 capacity range the
    /// request named, resolved against the capacity range its engine version publishes.
    pub serverless_v2_scaling: Option<ServerlessV2ScalingConfigurationInfo>,
    /// `ScalingConfigurationInfo`: the Aurora Serverless v1 scaling configuration the
    /// request named, for a cluster in the `serverless` engine mode.
    pub scaling_configuration: Option<ScalingConfigurationInfo>,
    /// `AssociatedRoles`: the IAM roles the request associated with the cluster.
    pub associated_roles: Vec<DBClusterRole>,
    /// `Domain` and `DomainIAMRoleName`, reported as the cluster's one domain membership.
    pub domain: Option<String>,
    /// See [`Self::domain`].
    pub domain_iam_role_name: Option<String>,
    /// `GlobalClusterIdentifier`: the global database this cluster is a member of, resolved
    /// in the store before the cluster was stored.
    pub global_cluster_identifier: Option<String>,
    /// `ReplicationSourceIdentifier`: the ARN of the cluster or instance this cluster
    /// replicates from, resolved in the store before the cluster was stored.
    pub replication_source_arn: Option<String>,
    /// The CA the cluster's server certificate is issued under.
    pub ca_certificate_identifier: String,
    /// `EngineLifecycleSupport`.
    pub engine_lifecycle_support: String,
    /// `ClusterScalabilityType`.
    pub cluster_scalability_type: String,
    /// `DBSystemId`: "reserved for future use".
    pub db_system_id: Option<String>,
    /// Whether `EnableLocalWriteForwarding` turned in-cluster write forwarding on.
    pub local_write_forwarding_enabled: bool,
    /// Whether `EnableGlobalWriteForwarding` asked for write forwarding to a global
    /// database's primary.
    pub global_write_forwarding_requested: bool,
    /// `VPCNetworkingEnabled`: whether the cluster uses VPC-based networking. True for
    /// every cluster placed in the account's VPC, which is every cluster a create makes;
    /// `RestoreDBClusterFromSnapshot$EnableVPCNetworking` is the one member that can turn it
    /// off, for a cluster restored without a VPC network interface.
    pub vpc_networking_enabled: bool,
    /// `InternetAccessGatewayEnabled`: whether clients reach the cluster over the internet
    /// through an internet access gateway, which
    /// `RestoreDBClusterFromSnapshot$EnableInternetAccessGateway` is the one member that
    /// asks for; false for a cluster that is reached inside its VPC.
    pub internet_access_gateway_enabled: bool,
    /// `ClusterCreateTime`.
    pub cluster_create_time: ::aws_smithy_types::DateTime,
    /// The member instance the cluster currently WRITES through — the one
    /// `DBClusterMembers` reports `IsClusterWriter` for.
    ///
    /// `None` while nothing has moved it, in which case the writer is the member that was
    /// created first (a cluster's first instance is its primary and every later one is a
    /// replica), so a cluster nobody failed over needs no stored choice. `FailoverDBCluster`
    /// sets it, which is the whole observable effect of a failover: "failover for a DB
    /// cluster promotes one of the Aurora Replicas (read-only instances) in the DB cluster
    /// to be the primary DB instance (the cluster writer)". It names the member, not an
    /// index, so an instance leaving the cluster cannot leave it pointing at another one.
    pub writer_identifier: Option<String>,
    /// `TagList`.
    pub tags: Vec<Tag>,
    /// The CONSISTENT POINTS this cluster can be backtracked to, oldest first: each is the
    /// instant and a copy of the data the cluster held at it.
    ///
    /// Backtracking "rewinds the DB cluster to the time you specify", which it can only do
    /// to a point it kept the data of — "If the specified time isn't a consistent time for
    /// the DB cluster, Aurora automatically chooses the nearest possible consistent time for
    /// the DB cluster", and these are this service's consistent times. A point is recorded
    /// when the cluster's storage is opened with backtracking enabled and after each
    /// backtrack; points older than [`Self::backtrack_window`] are dropped, because a
    /// cluster "can't be backtracked" beyond its window, and the cluster keeps at most
    /// [`MAX_BACKTRACK_POINTS_PER_CLUSTER`] of them, because each one holds a copy of the
    /// data.
    ///
    /// EMPTY for a cluster created with backtracking disabled (`BacktrackWindow` zero, the
    /// documented default), which keeps none, and for an engine this emulator serves no data
    /// plane for, which has no data to keep.
    pub backtrack_points: Vec<BacktrackPoint>,
    /// The backtracks PERFORMED on this cluster, oldest first: what
    /// `DescribeDBClusterBacktracks` "returns information about backtracks for a DB cluster"
    /// from.
    ///
    /// One record per accepted `BacktrackDBCluster`, so the identifier that call answered
    /// resolves to the backtrack it performed for as long as the cluster holds it. Bounded by
    /// [`MAX_BACKTRACKS_PER_CLUSTER`] ([`Self::record_backtrack`]), and dropped with the
    /// cluster, which is the only thing that can be backtracked.
    pub backtracks: Vec<BacktrackRecord>,
}

/// Emulator resource guard, NOT an AWS quota: how many performed backtracks one DB cluster
/// keeps for `DescribeDBClusterBacktracks` to report.
///
/// A caller may backtrack a cluster as often as it likes, so the list is the caller's to grow
/// and has to be capped, like every other caller-sized list this store keeps. Once it is
/// full the OLDEST record is dropped, so the report holds the most recent backtracks —
/// generous beside the largest page any report of this front door serves
/// ([`MAX_RECORDS_RANGE`]), so a caller can still page through what it kept.
pub const MAX_BACKTRACKS_PER_CLUSTER: usize = 100;

/// Emulator resource guard, NOT an AWS quota: how many CONSISTENT POINTS one DB cluster
/// keeps ([`DbClusterRecord::backtrack_points`]).
///
/// How many points fall inside a cluster's backtrack window is driven by how often a caller
/// asks for a backtrack, and each point holds a COPY of the cluster's data, so the count has
/// to be capped as well as the age: without it a caller could make the store hold a copy per
/// request. Once it is full the OLDEST point is dropped, so what the cluster keeps is the
/// most recent times it can be rewound to, and the `EarliestBacktrackTime` the cluster
/// reports moves up with them. Generous beside the points one cluster accumulates in normal
/// use — one when its storage opens and one per backtrack performed on it.
pub const MAX_BACKTRACK_POINTS_PER_CLUSTER: usize = 16;

/// One backtrack PERFORMED on a DB cluster, as `DBClusterBacktrack` reports it.
#[derive(Debug, Clone)]
pub struct BacktrackRecord {
    /// `BacktrackIdentifier`: "a valid universally unique identifier (UUID)", minted for the
    /// call that performed the backtrack and unique across this store
    /// ([`mint_uuid_token`]).
    pub identifier: String,
    /// `BacktrackRequestCreationTime`: when the backtrack was asked for.
    pub requested_at: ::aws_smithy_types::DateTime,
    /// `BacktrackedFrom`: the time the cluster was rewound FROM.
    pub backtracked_from: ::aws_smithy_types::DateTime,
    /// `BacktrackTo`: the consistent time the cluster was rewound TO.
    pub backtrack_to: ::aws_smithy_types::DateTime,
    /// `Status`: one of the values that member publishes (`applying`, `completed`, `failed`,
    /// `pending`).
    pub status: String,
}

impl BacktrackRecord {
    /// This backtrack as the modelled `DBClusterBacktrack`, under the identifier the cluster
    /// it was performed on is reported by — which a rename moves, so the report always names
    /// the cluster as it stands now.
    pub fn to_wire(&self, cluster_identifier: &str) -> BacktrackDBClusterOutput {
        BacktrackDBClusterOutput {
            db_cluster_identifier: Some(cluster_identifier.to_string()),
            backtrack_identifier: Some(self.identifier.clone()),
            backtrack_to: Some(self.backtrack_to),
            backtracked_from: Some(self.backtracked_from),
            backtrack_request_creation_time: Some(self.requested_at),
            status: Some(self.status.clone()),
        }
    }
}

/// How precisely two instants are compared when a backtrack picks the point it rewinds to:
/// a millisecond, which is finer than any interval between two points this service records
/// and coarse enough that a timestamp carried out to a client and back — through the wire
/// format's own rounding — still names the point it was read from.
const BACKTRACK_POINT_RESOLUTION: f64 = 0.001;

/// One consistent point a DB cluster can be backtracked to: when the cluster held this
/// data, and the data itself.
#[derive(Clone)]
pub struct BacktrackPoint {
    /// The instant the cluster held this copy, which is what `BacktrackTo` reports for a
    /// backtrack that rewound to it.
    pub at: ::aws_smithy_types::DateTime,
    /// The copy itself, taken by the engine the way a snapshot's is
    /// ([`data_plane::capture_endpoint_storage`]).
    pub storage: ::std::sync::Arc<data_plane::sql_engine::InstanceDatabase>,
}

impl std::fmt::Debug for BacktrackPoint {
    /// The engine's database handle carries no `Debug`, and the DATA is not something a
    /// debug line should carry anyway, so a point shows the instant it holds and the
    /// presence of the copy.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BacktrackPoint")
            .field("at", &self.at)
            .finish_non_exhaustive()
    }
}

impl DbClusterRecord {
    /// Apply every change whose due instant has passed. Returns whether the record is now
    /// gone, so the caller drops it from the store — the same contract as
    /// [`DbInstanceRecord::settle`].
    ///
    /// A delete that takes a final snapshot is a progression of two steps, and both are
    /// applied in one pass when both came due: each step's span runs from the instant the
    /// step before it was due, so a read that comes after the whole progression reports where
    /// the clock has taken it rather than one step per read.
    pub(super) fn settle(&mut self, now: Instant) -> bool {
        while self.pending_status.as_ref().is_some_and(|p| now >= p.due_at) {
            let pending = self.pending_status.take().expect("checked just above");
            match pending.change {
                PendingChange::Status(status) => {
                    self.status = status;
                    // A cluster that reaches `deleting` on the clock — the end of the final
                    // snapshot a delete asked for — still owes the removal that delete
                    // accepted, so the clock takes it from here.
                    if self.status == DB_CLUSTER_STATUS_DELETING {
                        self.pending_status =
                            Some(PendingStatus::removal_in_a_moment_after(pending.due_at));
                    }
                }
                PendingChange::Removal => return true,
            }
        }
        false
    }

    /// `arn:aws:rds:<region>:<account>:cluster:<identifier>`, from the stored region and
    /// the cluster's current identifier.
    pub fn arn(&self) -> String {
        db_cluster_arn(&self.region, &self.identifier)
    }

    /// Whether this cluster keeps a backtrack window at all: "To disable backtracking, set
    /// this value to 0", so a positive window is backtracking enabled.
    pub fn backtracking_enabled(&self) -> bool {
        self.backtrack_window > 0
    }

    /// `EarliestBacktrackTime`: the oldest consistent point the cluster CAN be backtracked
    /// to, and `None` for a cluster that has no backtracking available — one that kept no
    /// point ([`Self::backtrack_points`]) and one whose window is zero, since "to disable
    /// backtracking, set this value to 0" leaves no time the cluster can be rewound to at
    /// all. So a cluster a modification turned backtracking off for reports no earliest
    /// time, the same as one created with it off, rather than offering an instant the
    /// backtrack then refuses.
    pub fn earliest_backtrack_time(&self) -> Option<::aws_smithy_types::DateTime> {
        if !self.backtracking_enabled() {
            return None;
        }
        self.backtrack_points.first().map(|point| point.at)
    }

    /// Keep `storage` as the consistent point the cluster holds AS OF NOW, and drop the
    /// points that have fallen out of the backtrack window or past the count the cluster
    /// keeps.
    ///
    /// A window of zero keeps nothing: backtracking is off, so there is no time the cluster
    /// can be backtracked to.
    pub fn record_backtrack_point(
        &mut self,
        storage: ::std::sync::Arc<data_plane::sql_engine::InstanceDatabase>,
    ) {
        if !self.backtracking_enabled() {
            self.backtrack_points.clear();
            return;
        }
        let now = now_as_wire_timestamp();
        self.backtrack_points.push(BacktrackPoint { at: now, storage });
        // "The target backtrack window, in seconds": a point older than the window is one
        // the cluster can no longer be backtracked to, so it is dropped rather than kept
        // and offered. The oldest point is kept while it is the ONLY one, because a cluster
        // with backtracking on reports an earliest backtrack time.
        let cutoff = now.as_secs_f64() - self.backtrack_window as f64;
        while self.backtrack_points.len() > 1
            && self
                .backtrack_points
                .first()
                .is_some_and(|point| point.at.as_secs_f64() < cutoff)
        {
            self.backtrack_points.remove(0);
        }
        // And the COUNT bound: how many points fall inside the window is driven by how often
        // a caller asks for a backtrack, and each one holds a copy of the cluster's data, so
        // the oldest are dropped once the cluster keeps
        // [`MAX_BACKTRACK_POINTS_PER_CLUSTER`] of them. The newest points survive, which are
        // the times a caller can still ask to be rewound to.
        while self.backtrack_points.len() > MAX_BACKTRACK_POINTS_PER_CLUSTER {
            self.backtrack_points.remove(0);
        }
    }

    /// Keep `backtrack` as a backtrack performed on this cluster, dropping the oldest record
    /// once the cluster holds [`MAX_BACKTRACKS_PER_CLUSTER`] of them.
    pub fn record_backtrack(&mut self, backtrack: BacktrackRecord) {
        self.backtracks.push(backtrack);
        while self.backtracks.len() > MAX_BACKTRACKS_PER_CLUSTER {
            self.backtracks.remove(0);
        }
    }

    /// The consistent point a backtrack to `target` rewinds the cluster to: the latest
    /// point at or before it, which is the "nearest possible consistent time for the DB
    /// cluster" Aurora chooses when the time asked for is not itself one.
    ///
    /// `None` when every point the cluster kept is LATER than the time asked for, which is
    /// a time before its earliest backtrack time — the case the request's
    /// `UseEarliestTimeOnPointInTimeUnavailable` decides the answer to.
    ///
    /// The comparison allows the wire timestamp's own resolution
    /// ([`BACKTRACK_POINT_RESOLUTION`]), so a caller that asks to be rewound to the
    /// `EarliestBacktrackTime` the cluster reported is rewound to that point rather than
    /// refused for the rounding of the value it was given.
    pub fn backtrack_point_at(&self, target: ::aws_smithy_types::DateTime) -> Option<&BacktrackPoint> {
        let target = target.as_secs_f64() + BACKTRACK_POINT_RESOLUTION;
        self.backtrack_points
            .iter()
            .rfind(|point| point.at.as_secs_f64() <= target)
    }

    /// The cluster endpoint: "the connection endpoint for the primary instance of the DB
    /// cluster".
    ///
    /// A cluster whose data plane this emulator SERVES publishes the HOST of the listener that
    /// plane is answered on ([`Self::served_endpoint`]) — the same reading a DB instance's
    /// `Endpoint$Address` ([`DbInstanceRecord::endpoint_address`]) and a DB proxy's `Endpoint`
    /// ([`DbProxyRecord::endpoint`]) follow. This member and `Port` are the ONE pair a client
    /// composes the address it dials from, and the port beside it is that listener's
    /// ([`Self::published_port`]), so the host has to be the same listener's too: a name in a
    /// zone this emulator neither serves nor owns is no connection endpoint, whatever a
    /// resolver does with it, and a session can be opened on no other address.
    ///
    /// A cluster this emulator serves NO data plane for keeps the published AWS form,
    /// `<identifier>.cluster-<server-assigned label>.<region>.<suffix>` (Amazon Aurora User
    /// Guide, "Amazon Aurora connection management") — the name the CLUSTER mints from its own
    /// identifier, over the suffix it shares with [`Self::reader_endpoint`]; see
    /// [`cluster_endpoint_names`]. There is no local plane for such a cluster to lead a client
    /// to, so the name is all it can publish.
    ///
    /// A HOST and nothing else, either way: the cluster has one `Port` member for both of its
    /// endpoints, so neither carries a port of its own.
    pub fn endpoint(&self) -> String {
        if let Some(served) = &self.served_endpoint {
            return served.host.clone();
        }
        self.endpoint_name(CLUSTER_WRITER_LABEL)
    }

    /// One of the cluster's two endpoint names: its identifier, the `role` label
    /// ([`CLUSTER_WRITER_LABEL`] or [`CLUSTER_READER_LABEL`]) with the server-assigned label
    /// behind it, the region, and the suffix both endpoints share
    /// ([`Self::endpoint_dns_suffix`]).
    ///
    /// A cluster whose data plane is an AWS API this emulator serves at its OWN endpoint
    /// publishes the host that endpoint is reached at instead
    /// ([`crate::state::api_plane_endpoint`]): a graph cluster's plane answers HTTP there,
    /// and the front door admits a request only when its `Host` names the interface the core
    /// bound, so that host is the only address that reaches it — see
    /// [`cluster_endpoint_names`] for why this engine class is exempt from the name pair.
    fn endpoint_name(&self, role: &str) -> String {
        if let Some(served) = &self.served_endpoint {
            if engine_data_plane_is_an_api(&self.engine) {
                return served.host.clone();
            }
        }
        cluster_endpoint_host(
            &self.identifier,
            role,
            &self.endpoint_host,
            &self.region,
            &self.endpoint_dns_suffix(),
        )
    }

    /// The DNS suffix BOTH of this cluster's endpoint names sit in.
    ///
    /// A cluster whose engine wire this emulator serves is reached on a listener of its own,
    /// so its names sit in the zone that listener's address names
    /// ([`served_endpoint_suffix`]) — the loopback zone for the interface the core binds on —
    /// and a cluster this emulator serves no plane for keeps the published AWS form, under the
    /// endpoint domain of the control plane that owns its engine ([`endpoint_domain`]).
    fn endpoint_dns_suffix(&self) -> String {
        match &self.served_endpoint {
            Some(served) => served_endpoint_suffix(endpoint_domain(&self.engine), &served.host),
            None => endpoint_domain(&self.engine).to_string(),
        }
    }

    /// The host a CUSTOM endpoint of this cluster publishes: `named`, the cluster's
    /// `cluster-custom-` label and the cluster's own label, region and suffix — one name in
    /// front of the same data plane the cluster's two endpoints name
    /// ([`DbClusterEndpointRecord::to_wire`]).
    ///
    /// Composed here, from the cluster's own fields, so a custom endpoint cannot sit in
    /// another zone than the cluster it fronts.
    ///
    /// A cluster whose data plane is an AWS API this emulator serves at its own endpoint is
    /// read as [`Self::endpoint_name`] reads it, for the same reason: that plane answers on
    /// this process's address and nowhere else, so a custom endpoint in front of it is
    /// reached there too.
    pub fn custom_endpoint_host(&self, named: &str) -> String {
        if let Some(served) = &self.served_endpoint {
            if engine_data_plane_is_an_api(&self.engine) {
                return served.host.clone();
            }
        }
        cluster_endpoint_host(
            named,
            CLUSTER_CUSTOM_LABEL,
            &self.endpoint_host,
            &self.region,
            &self.endpoint_dns_suffix(),
        )
    }

    /// The port the cluster's database engine listens on: the port of the address this
    /// emulator answers the cluster's data plane at where it serves one, else the configured
    /// port.
    ///
    /// The core owns the port range a listener is bound in and takes no port from this
    /// service (`vendor/server/src/listeners.rs`), so for an engine whose native wire this
    /// emulator speaks the configured port (what `Port` set, which the record keeps) is what
    /// the cluster records and the port the listener landed on is what it publishes — an
    /// engine endpoint whose port nothing answers on would be no endpoint at all.
    ///
    /// The same holds for an engine whose data plane this emulator serves as an API
    /// ([`engine_data_plane_is_an_api`]): that plane answers on THIS process's endpoint, so
    /// the cluster publishes the port it is reached on there
    /// ([`crate::state::api_plane_endpoint`]). `Endpoint` carries no port of its own, so
    /// `Endpoint` + `Port` is the one pair a client dials, and the engine's own default
    /// beside a host this process serves would name a port nothing answers on.
    ///
    /// A cluster whose plane this emulator serves NEITHER way publishes the port it was
    /// created with — the engine's published default where the request named none — beside
    /// the AWS DNS name it also keeps.
    pub fn published_port(&self) -> i32 {
        self.served_endpoint
            .as_ref()
            .map_or(self.port, |served| served.port)
    }

    /// The reader endpoint, which "load-balances connections across the Aurora Replicas
    /// that are available in a DB cluster": the name the cluster mints under the published
    /// `cluster-ro-` label, over the suffix [`Self::endpoint_dns_suffix`] answers.
    ///
    /// A HOST, exactly as [`Self::endpoint`] is: the model gives a DB cluster ONE port
    /// member for both of its endpoints (`DBCluster$Port`, "the port that the database
    /// engine is listening on"), so neither endpoint member carries a port of its own and
    /// neither is ever written as a `host:port` authority — a client composes the address it
    /// dials from the endpoint and that one `Port`.
    ///
    /// The CLUSTER mints the name, not any reader instance: it is therefore non-empty,
    /// distinct from the writer endpoint and of this form whatever the cluster's membership
    /// is — a cluster with no Aurora Replica at all is still reached at it.
    ///
    /// It is the one endpoint member of a SERVED cluster that is not the address of the
    /// listener behind it ([`Self::endpoint`] is): the read-only plane is a listener of its
    /// own ([`data_plane::DbEngineEndpoint::reader`]) with a port of its own, and the one
    /// `Port` the model gives the pair is the WRITER listener's, so an address literal here
    /// would name the writer's plane and send a read path's writes through it. The
    /// `cluster-ro-` label is what tells the two paths apart; publishing the read-only
    /// listener's own address needs a resource to be dialable at a port of its own, which the
    /// core does not offer (`vendor/server/src/listeners.rs` binds every listener on the one
    /// operator-chosen interface at an OS-assigned port).
    pub fn reader_endpoint(&self) -> String {
        self.endpoint_name(CLUSTER_READER_LABEL)
    }

    /// When the server certificate issued at creation expires:
    /// [`SERVER_CERTIFICATE_VALIDITY`] after the cluster was created, a function of the
    /// stored creation time so the two cannot drift.
    pub fn certificate_valid_till(&self) -> ::aws_smithy_types::DateTime {
        ::aws_smithy_types::DateTime::from_secs_f64(
            self.cluster_create_time.as_secs_f64() + SERVER_CERTIFICATE_VALIDITY.as_secs_f64(),
        )
    }

    /// Whether a delete has already been accepted for this cluster, so it is on its way
    /// out and nothing further can be done to it.
    ///
    /// A delete is accepted at once, whatever the cluster is DOING while it runs: one whose
    /// request asked for a final DB cluster snapshot is backing that snapshot up and reaches
    /// `deleting` after it ([`Self::begin_deletion_after_final_snapshot`]), so the question
    /// is whether the record carries the delete — its status or the change the clock owes it
    /// — and not only whether it reports `deleting` now.
    pub fn is_deleting(&self) -> bool {
        if self.status == DB_CLUSTER_STATUS_DELETING {
            return true;
        }
        match self.pending_status.as_ref().map(|pending| &pending.change) {
            Some(PendingChange::Removal) => true,
            Some(PendingChange::Status(status)) => status == DB_CLUSTER_STATUS_DELETING,
            None => false,
        }
    }

    /// Whether this cluster OPERATES as an Aurora Limitless Database: `ClusterScalabilityType`
    /// "when set to `limitless`, the cluster operates as an Aurora Limitless Database"
    /// ([`CLUSTER_SCALABILITY_TYPE_LIMITLESS`]), and a cluster created in the standard mode
    /// runs none.
    ///
    /// The value is matched WITHOUT case, as every other published name this service resolves
    /// is.
    pub fn runs_limitless_database(&self) -> bool {
        self.cluster_scalability_type
            .eq_ignore_ascii_case(CLUSTER_SCALABILITY_TYPE_LIMITLESS)
    }

    /// `LimitlessDatabase`: "the details for Aurora Limitless Database", for a cluster that
    /// operates as one ([`Self::runs_limitless_database`]) and for no other — a cluster that
    /// runs no Limitless database has no such details, which is what reporting nothing says.
    ///
    /// `Status` follows what RUNS on the Limitless database: a DB shard group of this cluster
    /// is where the Limitless database's compute lives, so the status is
    /// [`LIMITLESS_DATABASE_STATUS_ACTIVE`] exactly while one of this cluster's shard groups
    /// is available and [`LIMITLESS_DATABASE_STATUS_NOT_IN_USE`] while none is — a cluster
    /// whose shard group is still being provisioned, was deleted, or was never created has
    /// nothing using it. `shard_group_is_available` is that answer, read from the shard-group
    /// records by the store ([`ClusterMembership::limitless_database`]).
    ///
    /// `MinRequiredACU` is left out: Amazon RDS publishes no rule for the minimum capacity it
    /// reports, and a number chosen here would be a capacity requirement AWS never published.
    pub fn limitless_database(&self, shard_group_is_available: bool) -> Option<LimitlessDatabase> {
        self.runs_limitless_database().then(|| LimitlessDatabase {
            status: Some(
                if shard_group_is_available {
                    LIMITLESS_DATABASE_STATUS_ACTIVE
                } else {
                    LIMITLESS_DATABASE_STATUS_NOT_IN_USE
                }
                .to_string(),
            ),
            min_required_acu: None,
        })
    }

    /// Whether this cluster is in a state a new Aurora Limitless Database DB shard group
    /// can be provisioned on: its status is one of
    /// [`SHARD_GROUP_CLUSTER_STATUSES`] — the shard group goes into the cluster, so the
    /// cluster has to be up or coming up — and every other status refuses the create
    /// ([`shard_group_cluster_state_message`]). A cluster whose delete has been accepted is
    /// in none of them, whichever status the delete left it reporting
    /// ([`Self::is_deleting`]).
    pub fn can_take_a_shard_group(&self) -> bool {
        SHARD_GROUP_CLUSTER_STATUSES.contains(&self.status.as_str()) && !self.is_deleting()
    }

    /// Accept a delete: the cluster reports `deleting` from now on, and the clock owes
    /// its removal from the store [`STATUS_TRANSITION_DELAY`] from now, after which every
    /// read of the identifier is a `DBClusterNotFoundFault`. Nothing sleeps: the due
    /// instant is stored and materialised by
    /// [`ComAmazonawsRdsAmazonrdsv19State::settle_due_transitions`], exactly as
    /// [`DbInstanceRecord::begin_deletion`] does for a DB instance.
    pub fn begin_deletion(&mut self) {
        self.status = DB_CLUSTER_STATUS_DELETING.to_string();
        self.pending_status = Some(PendingStatus::removal_in_a_moment());
    }

    /// Accept a delete that takes a FINAL DB cluster snapshot first: "RDS creates a DB
    /// cluster snapshot before it deletes the DB cluster", so the cluster is backing up
    /// while that snapshot is taken, reaches [`DB_CLUSTER_STATUS_DELETING`] once it is made
    /// ([`Self::settle`]) and leaves the store after that. The delete is accepted from this
    /// call on either way ([`Self::is_deleting`]).
    pub fn begin_deletion_after_final_snapshot(&mut self) {
        self.status = DB_CLUSTER_STATUS_BACKING_UP.to_string();
        self.pending_status = Some(PendingStatus::in_a_moment(DB_CLUSTER_STATUS_DELETING));
    }

    /// `Status`: the status the cluster REPORTS, which is the stored one plus what the
    /// records around it say the cluster is doing.
    ///
    /// A cluster that is up reports `backing-up` while a backup of it is in flight ("the DB
    /// cluster is currently being backed up", the published cluster-status list) — the
    /// snapshot being taken is the work, so the status follows the snapshot record rather
    /// than being written onto the cluster when one is asked for. A cluster in any other
    /// stored status is doing THAT, and reports it: a cluster still being created, stopped,
    /// modifying or on its way out is not described by the backup running inside it.
    pub fn reported_status(&self, membership: &ClusterMembership) -> String {
        if membership.backing_up && self.status == DB_CLUSTER_STATUS_AVAILABLE {
            return DB_CLUSTER_STATUS_BACKING_UP.to_string();
        }
        self.status.clone()
    }

    /// Whether this cluster has an automated backup at all: "The number of days for which
    /// automated backups are retained" (`BackupRetentionPeriod`), so a cluster that
    /// retains none has none to describe.
    pub fn has_automated_backup(&self) -> bool {
        self.backup_retention_period > 0
    }

    /// `DBClusterAutomatedBackupsArn`. The resource type is `cluster-auto-backup`, the one
    /// AWS uses for a cluster's automated backup under this service's `rds` ARN namespace,
    /// and it carries the cluster's immutable resource id rather than its identifier,
    /// which can be renamed.
    pub fn automated_backup_arn(&self) -> String {
        emulator_server::emulator_arn(
            ARN_NAMESPACE,
            &self.region,
            &format!("cluster-auto-backup:{}", self.resource_id),
        )
    }

    /// The VPC this cluster sits in: the one its DB subnet group's subnets belong to, which
    /// is the region's VPC — see [`region_vpc_id`] for why one region resolves to one VPC.
    pub fn vpc_id(&self) -> String {
        region_vpc_id(&self.region)
    }

    /// The window a point-in-time restore of this cluster can target, as of now: "the
    /// earliest and latest time a DB cluster can be restored to" is at most
    /// `BackupRetentionPeriod` days back and never before the cluster existed.
    fn backup_window(&self) -> (::aws_smithy_types::DateTime, ::aws_smithy_types::DateTime) {
        let now = now_as_wire_timestamp();
        let retention = f64::from(self.backup_retention_period) * 24.0 * 60.0 * 60.0;
        let earliest = (now.as_secs_f64() - retention).max(self.cluster_create_time.as_secs_f64());
        (
            ::aws_smithy_types::DateTime::from_secs_f64(earliest),
            now,
        )
    }

    /// `EarliestRestorableTime` and `LatestRestorableTime`: the window
    /// [`Self::backup_window`] computes, for a cluster that HAS an automated backup to
    /// restore from.
    ///
    /// `None` for a cluster that retains no automated backup at all
    /// ([`Self::has_automated_backup`]), and for one that is still being created, which has
    /// not taken its first backup yet — in both cases there is no point in time the cluster
    /// can be restored to, which is what reporting neither member says.
    pub fn restore_window(
        &self,
    ) -> Option<(::aws_smithy_types::DateTime, ::aws_smithy_types::DateTime)> {
        (self.has_automated_backup() && self.status != DB_CLUSTER_STATUS_CREATING)
            .then(|| self.backup_window())
    }

    /// Project this cluster's automated backup onto the modelled
    /// `DBClusterAutomatedBackup` shape.
    ///
    /// Every member is read from the cluster the backup is OF, so the two cannot disagree,
    /// and the restore window is computed from the clock and the retention period the
    /// cluster carries now: "the earliest and latest time a DB cluster can be restored to"
    /// is at most `BackupRetentionPeriod` days back, and never before the cluster existed.
    /// `Status` is `active` while that cluster is still here — the member documents
    /// `retained` for the backups of a cluster that has been deleted.
    pub fn automated_backup_to_wire(&self) -> DBClusterAutomatedBackup {
        let (earliest, latest) = self.backup_window();
        DBClusterAutomatedBackup {
            engine: Some(self.engine.clone()),
            vpc_id: Some(self.vpc_id()),
            db_cluster_automated_backups_arn: Some(self.automated_backup_arn()),
            db_cluster_identifier: Some(self.identifier.clone()),
            restore_window: Some(RestoreWindow {
                earliest_time: Some(earliest),
                latest_time: Some(latest),
            }),
            master_username: self.master_username.clone(),
            db_cluster_resource_id: Some(self.resource_id.clone()),
            region: Some(self.region.clone()),
            license_model: cluster_license_model(&self.engine),
            status: Some(DB_CLUSTER_AUTOMATED_BACKUP_STATUS_ACTIVE.to_string()),
            iam_database_authentication_enabled: Some(self.iam_database_authentication_enabled),
            cluster_create_time: Some(self.cluster_create_time),
            storage_encrypted: Some(self.storage_encrypted),
            storage_encryption_type: Some(self.storage_encryption_type().to_string()),
            allocated_storage: self.reported_allocated_storage(),
            engine_version: self.engine_version.clone(),
            db_cluster_arn: Some(self.arn()),
            backup_retention_period: Some(self.backup_retention_period),
            preferred_backup_window: Some(self.preferred_backup_window.clone()),
            engine_mode: Some(self.engine_mode.clone()),
            availability_zones: Some(self.availability_zones.clone()),
            port: Some(self.port),
            kms_key_id: self.kms_key_id.clone(),
            storage_type: self.storage_type.clone(),
            iops: self.iops,
            // The throughput of a cluster's storage is "not configurable" and this service
            // stores none for a cluster, and no automated backup here was taken by Amazon
            // Web Services Backup, so neither member is invented.
            storage_throughput: None,
            aws_backup_recovery_point_arn: None,
            tag_list: Some(self.tags.clone()),
        }
    }

    /// `AllocatedStorage`: "`AllocatedStorage` specifies the allocated storage size in
    /// gibibytes (GiB). For Aurora, `AllocatedStorage` can vary because Aurora DB cluster
    /// storage size adjusts as needed."
    ///
    /// A Multi-AZ DB cluster is created with the storage its request asked for ("the amount
    /// of storage in gibibytes (GiB) to allocate to each DB instance in the Multi-AZ DB
    /// cluster … required to create a Multi-AZ DB cluster"), and reports that. An
    /// Aurora-style cluster — the Aurora engines and the two sibling control planes' —
    /// takes no such parameter at all because its storage is not fixed, and the size it
    /// reports is the one unit of the member it still has to answer with:
    /// [`ELASTIC_CLUSTER_ALLOCATED_STORAGE`]. Reporting nothing would say the cluster has no
    /// storage, which is not what a cluster whose storage grows on its own has.
    pub fn reported_allocated_storage(&self) -> Option<i32> {
        self.allocated_storage
            .or_else(|| (!is_multi_az_db_cluster(&self.engine)).then_some(
                ELASTIC_CLUSTER_ALLOCATED_STORAGE,
            ))
    }

    /// `StorageEncryptionType`: "the type of encryption used to protect data at rest in
    /// the DB cluster" — the one reading in [`storage_encryption_type`], which the
    /// snapshots of this cluster report too.
    pub fn storage_encryption_type(&self) -> &'static str {
        storage_encryption_type(self.storage_encrypted, &self.engine)
    }

    /// Project the stored record onto the modelled `DBCluster` output shape.
    ///
    /// Every member RDS reports for a cluster it holds is projected from the stored record
    /// or from the records `membership` was read out of; a member this service has no
    /// state for at all (the restorable-time window, the progress percentage, the clone
    /// group, the backtrack change-record count) is LEFT OUT rather than invented, while a
    /// list it does have state for is reported empty, which is what RDS reports for a
    /// cluster that has none of those associations.
    pub fn to_wire(&self, membership: &ClusterMembership) -> DBCluster {
        // One reading of the restore window, so the two members it fills name the two ends
        // of the SAME window rather than two instants a moment apart.
        let restorable = self.restore_window();
        DBCluster {
            db_cluster_identifier: Some(self.identifier.clone()),
            db_cluster_arn: Some(self.arn()),
            db_cluster_resource_id: Some(self.resource_id.clone()),
            status: Some(self.reported_status(membership)),
            engine: Some(self.engine.clone()),
            engine_version: self.engine_version.clone(),
            engine_mode: Some(self.engine_mode.clone()),
            database_name: self.database_name.clone(),
            master_username: self.master_username.clone(),
            // `MasterUserSecret`: reported for a cluster whose master user password RDS
            // manages, and only for one — a cluster whose password the caller supplied
            // has no secret of RDS's own, which is what reporting nothing says.
            master_user_secret: self
                .master_user_secret
                .as_ref()
                .map(ManagedMasterSecret::to_wire),
            port: Some(self.published_port()),
            endpoint: Some(self.endpoint()),
            reader_endpoint: Some(self.reader_endpoint()),
            // "The custom endpoints associated with the DB cluster": the endpoints created
            // for this cluster, read off those records and reported as the addresses they
            // publish.
            custom_endpoints: Some(membership.custom_endpoints.clone()),
            hosted_zone_id: Some(hosted_zone_id(&self.region, endpoint_domain(&self.engine))),
            availability_zones: Some(self.availability_zones.clone()),
            // "The earliest / latest time to which a database can be restored with
            // point-in-time restore": the window the cluster's automated backup covers,
            // computed from the same record as the backup itself ([`Self::restore_window`])
            // so the two cannot disagree. A cluster still being created has no backup to
            // restore from yet, and one that retains no automated backup never has, so
            // neither reports a window.
            earliest_restorable_time: restorable.map(|(earliest, _)| earliest),
            latest_restorable_time: restorable.map(|(_, latest)| latest),
            // The two settings express configuration turns on, as this cluster was created
            // or restored with them: a cluster placed in its VPC is VPC-based and has no
            // internet access gateway in front of it, and a cluster restored without the VPC
            // network interface reports the gateway it reaches clients through instead. Both
            // are reported rather than left for a client to guess.
            vpc_networking_enabled: Some(self.vpc_networking_enabled),
            internet_access_gateway_enabled: Some(self.internet_access_gateway_enabled),
            backup_retention_period: Some(self.backup_retention_period),
            preferred_backup_window: Some(self.preferred_backup_window.clone()),
            preferred_maintenance_window: Some(self.preferred_maintenance_window.clone()),
            db_cluster_parameter_group: self.db_cluster_parameter_group_name.clone(),
            db_subnet_group: self.db_subnet_group_name.clone(),
            character_set_name: self.character_set_name.clone(),
            db_cluster_option_group_memberships: Some(
                self.option_group_name
                    .iter()
                    .map(|name| DBClusterOptionGroupStatus {
                        db_cluster_option_group_name: Some(name.clone()),
                        status: Some(MEMBERSHIP_IN_SYNC.to_string()),
                    })
                    .collect(),
            ),
            vpc_security_groups: Some(vpc_security_group_memberships(
                &self.region,
                &self.vpc_security_group_ids,
            )),
            storage_encrypted: Some(self.storage_encrypted),
            storage_encryption_type: Some(self.storage_encryption_type().to_string()),
            kms_key_id: self.kms_key_id.clone(),
            storage_type: self.storage_type.clone(),
            iops: self.iops,
            allocated_storage: self.reported_allocated_storage(),
            db_cluster_instance_class: self.db_cluster_instance_class.clone(),
            publicly_accessible: self.publicly_accessible,
            auto_minor_version_upgrade: Some(self.auto_minor_version_upgrade),
            deletion_protection: Some(self.deletion_protection),
            copy_tags_to_snapshot: Some(self.copy_tags_to_snapshot),
            http_endpoint_enabled: Some(self.http_endpoint_enabled),
            iam_database_authentication_enabled: Some(self.iam_database_authentication_enabled),
            backtrack_window: Some(self.backtrack_window),
            // `EarliestBacktrackTime`: "the earliest time to which a database can be
            // backtracked" — the oldest consistent point this service kept for the cluster,
            // and nothing at all for a cluster with backtracking off, which kept none and
            // can be backtracked to no time ([`Self::backtrack_points`]).
            earliest_backtrack_time: self.earliest_backtrack_time(),
            enabled_cloudwatch_logs_exports: Some(self.enabled_cloudwatch_logs_exports.clone()),
            network_type: self.network_type.clone(),
            monitoring_interval: Some(self.monitoring_interval),
            monitoring_role_arn: self.monitoring_role_arn.clone(),
            database_insights_mode: Some(self.database_insights_mode.clone()),
            // `PerformanceInsightsEnabled`, `PerformanceInsightsKMSKeyId` and
            // `PerformanceInsightsRetentionPeriod` are optional members the model gives no
            // default, so each is reported only once a request has configured it and is
            // left out of the envelope for a cluster that carries no Performance Insights
            // setting at all — a member with a value nothing asked for would read as
            // configuration the request never made.
            performance_insights_enabled: self.performance_insights_enabled,
            performance_insights_kms_key_id: self.performance_insights_kms_key_id.clone(),
            performance_insights_retention_period: self.performance_insights_retention_period,
            serverless_v2_scaling_configuration: self.serverless_v2_scaling.clone(),
            // "The version of the Aurora Serverless V2 platform used by the DB cluster":
            // reported for a cluster that HAS Serverless v2 capacity configured, and for
            // one only — a cluster with no such capacity runs on no Serverless v2 platform,
            // which is what reporting nothing says. The version itself is the one this
            // service publishes ([`DEFAULT_SERVERLESS_V2_PLATFORM_VERSION`]).
            serverless_v2_platform_version: self
                .serverless_v2_scaling
                .as_ref()
                .map(|_| DEFAULT_SERVERLESS_V2_PLATFORM_VERSION.to_string()),
            scaling_configuration_info: self.scaling_configuration.clone(),
            associated_roles: Some(self.associated_roles.clone()),
            domain_memberships: Some(
                self.domain
                    .iter()
                    .map(|domain| DomainMembership {
                        domain: Some(domain.clone()),
                        status: Some("joined".to_string()),
                        iam_role_name: self.domain_iam_role_name.clone(),
                        ..DomainMembership::default()
                    })
                    .collect(),
            ),
            global_cluster_identifier: self.global_cluster_identifier.clone(),
            // `GlobalClusterResourceId`: the immutable id of the global database cluster
            // this one is a member of, resolved through that membership
            // ([`ClusterMembership::global_cluster_resource_id`]) so the name and the id
            // the envelope carries always describe the same global cluster.
            global_cluster_resource_id: membership.global_cluster_resource_id.clone(),
            global_write_forwarding_requested: Some(self.global_write_forwarding_requested),
            local_write_forwarding_status: Some(
                if self.local_write_forwarding_enabled {
                    LOCAL_WRITE_FORWARDING_ENABLED
                } else {
                    LOCAL_WRITE_FORWARDING_DISABLED
                }
                .to_string(),
            ),
            replication_source_identifier: self.replication_source_arn.clone(),
            read_replica_identifiers: Some(membership.read_replica_identifiers.clone()),
            db_cluster_members: Some(membership.members.clone()),
            multi_az: Some(membership.multi_az),
            certificate_details: Some(CertificateDetails {
                ca_identifier: Some(self.ca_certificate_identifier.clone()),
                valid_till: Some(self.certificate_valid_till()),
            }),
            cluster_create_time: Some(self.cluster_create_time),
            cluster_scalability_type: Some(self.cluster_scalability_type.clone()),
            // `LimitlessDatabase`: the state of the Limitless database of a cluster that
            // operates as one, read from the shard-group records that name it
            // ([`ClusterMembership::limitless_database`]).
            limitless_database: membership.limitless_database.clone(),
            engine_lifecycle_support: Some(self.engine_lifecycle_support.clone()),
            db_system_id: self.db_system_id.clone(),
            upgrade_rollout_order: Some(DEFAULT_UPGRADE_ROLLOUT_ORDER.to_string()),
            // `StartActivityStream` is still a stub, so no cluster this service holds has
            // a database activity stream running. The recorded cluster envelope of this
            // front door carries the status under a member name of its own as well, for a
            // cluster of EVERY engine it serves, so the one status is reported under both
            // names.
            activity_stream_status: Some(ACTIVITY_STREAM_STOPPED.to_string()),
            db_activity_stream_status: Some(ACTIVITY_STREAM_STOPPED.to_string()),
            // The two capability flags the recorded envelope carries on every cluster this
            // front door reports, whatever its engine: neither capability is turned on for
            // a cluster created here, which is what reporting them `false` says.
            docdb_analytics_enabled: Some(DEFAULT_DOCDB_ANALYTICS_ENABLED),
            custom_data_group_present: Some(DEFAULT_CUSTOM_DATA_GROUP_PRESENT),
            // "Indicates whether the DB cluster is a clone of a DB cluster owned by a
            // different Amazon Web Services account": nothing here clones one.
            cross_account_clone: Some(false),
            tag_list: Some(self.tags.clone()),
            // `PendingModifiedValues` is "returned only when there are pending changes".
            // Every change this service accepts for a cluster is written to the stored
            // record at once, so the only change still in flight is the one the CREATE is
            // applying: a cluster being provisioned with a master credential has that
            // credential pending until it is up, masked because the member is
            // `@sensitive` — the same reading the DB instance envelope follows
            // ([`DbInstanceRecord::pending_modified_values`]).
            pending_modified_values: (self.status == DB_CLUSTER_STATUS_CREATING
                && self.master_user_password.is_some())
            .then(|| ClusterPendingModifiedValues {
                master_user_password: Some(MASKED_PENDING_MASTER_USER_PASSWORD.to_string()),
                ..ClusterPendingModifiedValues::default()
            }),
            ..DBCluster::default()
        }
    }
}
