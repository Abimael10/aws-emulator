// SPDX-License-Identifier: CC0-1.0

//! The GLOBAL DATABASE a DB cluster joins: the record this store keeps of one, the endpoint
//! and status it publishes, and the sentences the faults about it are worded as.
//!
//! An Amazon Aurora global database is "a single database that spans multiple Amazon Web
//! Services Regions" with one primary cluster and its secondaries, so it is a resource of its
//! own rather than a member of any one cluster's record — which is why it is held and settled
//! here, beside the clusters that are its members
//! ([`ComAmazonawsRdsAmazonrdsv19State::global_clusters`]).

use super::*;

/// The message a `GlobalClusterAlreadyExistsFault` carries for `identifier`.
///
/// Amazon RDS words this one as a sentence ABOUT the global cluster the request named: the
/// resource type, then the requested identifier unquoted, then "already exists" with no
/// terminal period — so the identifier sits INSIDE the sentence rather than after a colon.
/// The identifier is the only variable part, and it is the one the request carried.
pub fn global_cluster_already_exists_message(identifier: &str) -> String {
    format!("Global cluster {identifier} already exists")
}

/// The message an `InvalidGlobalClusterStateFault` carries for a global database that still
/// holds member clusters.
///
/// Amazon RDS words this family's state refusals as a sentence about the global cluster,
/// naming it by its ARN: the fixed text is "Global Cluster " and " is not empty", and the
/// ARN of the global database the request named is the only variable part. Its primary and
/// secondary clusters have to be detached or deleted first.
pub fn global_cluster_not_empty_message(arn: &str) -> String {
    format!("Global Cluster {arn} is not empty")
}

/// The message an `InvalidGlobalClusterStateFault` carries for a global database whose
/// current status is not one the requested operation can be performed in.
///
/// The same sentence shape as [`global_cluster_not_empty_message`] — the global cluster
/// named by its ARN — carrying the status it is in, which is what the DB cluster and DB
/// instance state faults of this service name as the reason too. Both parts come from the
/// stored record.
pub fn global_cluster_not_available_message(arn: &str, status: &str) -> String {
    format!("Global Cluster {arn} isn't available for the requested operation with status {status}")
}

/// A stored Aurora global database cluster: the record `DescribeGlobalClusters` reports.
///
/// Account-scoped rather than regional — see
/// [`ComAmazonawsRdsAmazonrdsv19State::global_clusters`].
#[derive(Debug, Clone)]
pub struct GlobalClusterRecord {
    /// The caller's `GlobalClusterIdentifier`, and this record's key.
    pub identifier: String,
    /// `GlobalClusterResourceId`: the immutable, server-minted id.
    pub resource_id: String,
    /// `Status`.
    pub status: String,
    /// `Engine`.
    pub engine: Option<String>,
    /// `EngineVersion`.
    pub engine_version: Option<String>,
    /// `EngineLifecycleSupport`: the lifecycle type the global cluster is enrolled in —
    /// the value the request named, or [`DEFAULT_ENGINE_LIFECYCLE_SUPPORT`].
    pub engine_lifecycle_support: String,
    /// `DatabaseName`.
    pub database_name: Option<String>,
    /// `StorageEncrypted`.
    pub storage_encrypted: bool,
    /// `DeletionProtection`.
    pub deletion_protection: bool,
    /// The ARNs of the member DB clusters, the PRIMARY (writer) first: that order is what
    /// [`Self::to_wire`] reports as `GlobalClusterMember$IsWriter`, and the regions in
    /// these ARNs are what the documented `region` filter of `DescribeGlobalClusters`
    /// matches on.
    pub member_cluster_arns: Vec<String>,
    /// The DNS label AWS assigns the global database's own writer endpoint between the
    /// identifier and the `global` suffix: a server-minted token, unique per global
    /// cluster — see [`Self::endpoint`].
    pub endpoint_host: String,
    /// The status this record owes, and when; see [`PendingStatus`]. A global cluster is
    /// `available` from its create onwards and only a modify puts it in a transient status,
    /// so this is `None` for a global cluster nothing has changed.
    pub pending_status: Option<PendingStatus>,
    /// `TagList`: the tags `CreateGlobalCluster$Tags` assigned to the global cluster.
    pub tags: Vec<Tag>,
}

impl GlobalClusterRecord {
    /// `arn:aws:rds::<account>:global-cluster:<identifier>`. A global cluster spans
    /// regions, so — as AWS does — the ARN's region segment is EMPTY.
    pub fn arn(&self) -> String {
        emulator_server::emulator_arn(
            ARN_NAMESPACE,
            "",
            &format!("global-cluster:{}", self.identifier),
        )
    }

    /// Whether any member cluster of this global cluster lives in `region`, which is what
    /// the one documented filter of `DescribeGlobalClusters` selects on: "the only
    /// supported filter is `region` … any global cluster with at least one member (primary
    /// or secondary) in the specified Amazon Web Services Regions".
    pub fn has_member_in(&self, region: &str) -> bool {
        self.member_cluster_arns
            .iter()
            .any(|arn| arn_region(arn) == Some(region))
    }

    /// Whether any member cluster of this global cluster is the DB cluster `named`, which is
    /// what the `db-cluster-id` filter of `DescribeGlobalClusters` selects on: the member
    /// clusters are held by ARN, and such a value "accepts DB cluster identifiers and DB
    /// cluster Amazon Resource Names (ARNs)", so both forms are folded to the identifier a DB
    /// cluster is keyed by ([`db_cluster_identifier_of`]).
    ///
    /// A `DBClusterIdentifier` "isn't case-sensitive", so the comparison is not either.
    pub fn has_member_cluster(&self, named: &str) -> bool {
        let wanted = db_cluster_identifier_of(named);
        self.member_cluster_arns
            .iter()
            .any(|arn| db_cluster_identifier_of(arn).eq_ignore_ascii_case(wanted))
    }

    /// The global database's own writer endpoint, `<identifier>.global-<minted
    /// host>.global.rds.amazonaws.com`.
    ///
    /// `GlobalCluster$Endpoint` is "the writer endpoint for the new global database
    /// cluster. This endpoint always points to the writer DB instance in the current
    /// primary cluster" — the Aurora global database writer endpoint, whose published form
    /// is the global cluster's identifier, a server-assigned label and the
    /// `global.rds.amazonaws.com` suffix (Amazon Aurora User Guide, "Using the writer
    /// endpoint of an Amazon Aurora global database"). It is a function of the stored
    /// identifier and minted host rather than stored text, so it cannot drift from the
    /// record; the label itself is server-assigned, as the DB instance endpoint's is.
    ///
    /// A global database AWS assigns one the moment it exists, member or no member: the name
    /// is the global database's own and the primary it points at is whichever cluster joins
    /// it, so it is reported from the create onwards.
    pub fn endpoint(&self) -> String {
        format!(
            "{}.global-{}.global.rds.amazonaws.com",
            self.identifier, self.endpoint_host
        )
    }

    /// `StorageEncryptionType`: "the type of encryption used to protect data at rest IN
    /// the global database cluster".
    ///
    /// Aurora storage is encrypted at rest either way, so the member reports WHICH key
    /// class protects it rather than whether anything does: `sse-kms` — "encrypted using a
    /// customer managed KMS key or Amazon Web Services managed KMS key" — when
    /// `StorageEncrypted` asked for KMS encryption (`CreateGlobalCluster` names no key of
    /// its own, so the key is the Amazon Web Services managed one RDS encrypts with by
    /// default), and otherwise `sse-rds`, "encrypted using an Amazon Web Services owned
    /// KMS key", the key the service applies itself. The third published value, `none`, is
    /// for a global database whose data is not encrypted at all, which this service
    /// creates none of.
    pub fn storage_encryption_type(&self) -> &'static str {
        if self.storage_encrypted {
            STORAGE_ENCRYPTION_TYPE_SSE_KMS
        } else {
            STORAGE_ENCRYPTION_TYPE_SSE_RDS
        }
    }

    /// Accept a modify: the global cluster reports [`GLOBAL_CLUSTER_STATUS_MODIFYING`]
    /// while the change is applied, and the clock owes it
    /// [`GLOBAL_CLUSTER_STATUS_AVAILABLE`] [`STATUS_TRANSITION_DELAY`] later — so a caller
    /// reading straight after the modify sees the transient status the real service
    /// reports, and the requests that status refuses are refused.
    pub fn begin_modifying(&mut self) {
        self.status = GLOBAL_CLUSTER_STATUS_MODIFYING.to_string();
        self.pending_status = Some(PendingStatus::in_a_moment(GLOBAL_CLUSTER_STATUS_AVAILABLE));
    }

    /// Whether this global cluster is in the one status another request can be taken in.
    ///
    /// `InvalidGlobalClusterStateFault` is "the global cluster is in an invalid state and
    /// can't perform the requested operation": a global database with a change still being
    /// applied is in exactly that state, which is what makes two modifies of one global
    /// database serialise and a delete during one refused.
    pub fn is_available(&self) -> bool {
        self.status == GLOBAL_CLUSTER_STATUS_AVAILABLE
    }

    /// Apply a change whose due instant has passed. Returns whether the record is now
    /// gone, exactly as [`DbInstanceRecord::settle`] does.
    ///
    /// `pub(super)` because the store that holds these records settles them
    /// ([`ComAmazonawsRdsAmazonrdsv19State::settle_due_transitions`]), and it lives in the
    /// module above this one.
    pub(super) fn settle(&mut self, now: Instant) -> bool {
        let due = self
            .pending_status
            .as_ref()
            .is_some_and(|pending| now >= pending.due_at);
        if !due {
            return false;
        }
        match self.pending_status.take().map(|pending| pending.change) {
            Some(PendingChange::Status(status)) => self.status = status,
            Some(PendingChange::Removal) => return true,
            // Unreachable: the change was there a line ago, under this one `&mut self`.
            // Answered as "nothing was due" rather than with a panic in a request path.
            None => {}
        }
        false
    }

    /// Project the stored record onto the modelled `GlobalCluster` output shape.
    ///
    /// Three of this model's members belong to this model ALONE: neither sibling control
    /// plane's `GlobalCluster` publishes an `Endpoint`, an `EngineLifecycleSupport` or a
    /// `StorageEncryptionType` (`com.amazonaws.neptune#GlobalCluster` and
    /// `com.amazonaws.docdb#GlobalCluster` both carry the identifier, the resource id, the
    /// ARN, the status, the engine and its version, the database name, the two booleans,
    /// the member list, the failover state and the tags, and nothing else). A global
    /// database of one of their engines is a resource of THAT control plane, so it reports
    /// the members its own API has and not this model's extra three.
    pub fn to_wire(&self) -> GlobalCluster {
        let own_control_plane =
            !is_sibling_control_plane_engine(self.engine.as_deref().unwrap_or_default());
        GlobalCluster {
            global_cluster_identifier: Some(self.identifier.clone()),
            global_cluster_resource_id: Some(self.resource_id.clone()),
            global_cluster_arn: Some(self.arn()),
            status: Some(self.status.clone()),
            engine: self.engine.clone(),
            engine_version: self.engine_version.clone(),
            engine_lifecycle_support: own_control_plane
                .then(|| self.engine_lifecycle_support.clone()),
            database_name: self.database_name.clone(),
            storage_encrypted: Some(self.storage_encrypted),
            // "The type of encryption used to protect data at rest in the global database
            // cluster" — see [`Self::storage_encryption_type`].
            storage_encryption_type: own_control_plane
                .then(|| self.storage_encryption_type().to_string()),
            // The global database's own writer endpoint — see [`Self::endpoint`].
            endpoint: own_control_plane.then(|| self.endpoint()),
            deletion_protection: Some(self.deletion_protection),
            global_cluster_members: Some(
                self.member_cluster_arns
                    .iter()
                    .enumerate()
                    .map(|(position, arn)| GlobalClusterMember {
                        db_cluster_arn: Some(arn.clone()),
                        // The primary is stored first; every other member is a secondary.
                        is_writer: Some(position == 0),
                        readers: Some(Vec::new()),
                        ..GlobalClusterMember::default()
                    })
                    .collect(),
            ),
            tag_list: Some(self.tags.clone()),
            // `FailoverState` "is empty unless the SwitchoverGlobalCluster or
            // FailoverGlobalCluster operation was called on this global cluster", and
            // neither is implemented in this crate, so nothing can have set one.
            ..GlobalCluster::default()
        }
    }
}
