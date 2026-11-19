// SPDX-License-Identifier: CC0-1.0

//! What replication of ONE table to each of its destinations reports: the state
//! of each destination, the update last replicated to it, and why one could not
//! be replicated to at all — and the replica each destination holds, which is
//! what replication leaves behind there.
//!
//! Setting a replication configuration provisions a table at the destination:
//! the operation that sets one takes "s3tables:CreateTable permission for the
//! destination" and "s3tables:CreateNamespace permission for the destination"
//! (`PutTableReplication`), so a destination table bucket of a configuration
//! this store holds really does hold the replica — and the state a read reports
//! for that destination is a fact about that record rather than a claim about
//! work nothing performed.
//!
//! Nothing a READ does here is written: a destination's state is DERIVED from
//! records this store already holds — the replication configuration that covers
//! the table, the table itself, the destination table bucket and the replica in
//! it — plus the clock, exactly as a maintenance job's status is
//! ([`super::maintenance_jobs`]). So two reads of a table nobody reconfigured
//! answer the same, and a read never moves a destination's state.

use std::time::{Duration, SystemTime};

use super::{
    ComAmazonawsS3tablesS3tablebucketsState, ReplicationConfiguration, Table,
    BUCKET_NOT_FOUND_MESSAGE,
};

/// How long after the table it covers last changed — or after the configuration
/// was set, whichever is later — replication of that state to a destination
/// completes.
///
/// CHOSEN, not an AWS-published figure: S3 Tables publishes no replication
/// latency. What matters is that the span is NOT zero — a caller that reads the
/// status straight after setting the configuration still sees the replication
/// that has not completed yet — and that it is short enough to wait for rather
/// than poll for hours.
pub const REPLICATION_COMPLETION_DELAY: Duration = Duration::from_secs(3);

/// The service that manages a table replication created at a destination
/// (`GetTable$managedByService`): S3 Tables' own replication principal, which
/// the operation that sets a configuration names as the one a replication role
/// is passed to ("roles to be passed to replication.s3tables.amazonaws.com",
/// `PutTableReplication`). A replica is a table S3 Tables creates on the
/// account's behalf rather than one a caller created, so it is held under that
/// principal — which is also what keeps a caller's own table of the same name
/// from ever being taken over by one.
pub const REPLICATION_SERVICE_PRINCIPAL: &str = "replication.s3tables.amazonaws.com";

/// `ReplicationStatus` — the state of replication to ONE destination. Values
/// from `com.amazonaws.s3tables#ReplicationStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicationState {
    /// The table's current state has not been replicated to the destination
    /// yet.
    Pending,
    /// The table's current state has been replicated to the destination, which
    /// holds the replica.
    Completed,
    /// Replication to the destination cannot proceed.
    Failed,
}

impl ReplicationState {
    /// Every modeled value, for exhaustiveness checks.
    pub const ALL: &'static [Self] = &[Self::Pending, Self::Completed, Self::Failed];

    /// The value as it goes on the wire.
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// The most recent update replication copied to a destination: the table
/// metadata that was replicated, and the instant that replication completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicatedUpdate {
    /// The location of the metadata file that was replicated — the source
    /// table's own, because that is the document replication copies.
    pub metadata_location: String,
    /// When the replication of that metadata completed.
    pub timestamp: SystemTime,
}

/// The state of replication of one table to ONE destination table bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicationDestinationStatus {
    /// The destination table bucket, as the configuration's rule names it.
    pub destination_table_bucket_arn: String,
    /// The replica's own ARN, reported only when the destination table bucket
    /// really holds a table under the source table's namespace and name — so
    /// the ARN this reports is one a read of that table answers for, never a
    /// claim about a table nothing holds.
    pub destination_table_arn: Option<String>,
    pub state: ReplicationState,
    /// `None` until a replication to this destination has completed, so the
    /// member is left out rather than claiming an update nothing copied.
    pub last_successful_update: Option<ReplicatedUpdate>,
    /// Why replication to this destination cannot proceed, set for exactly the
    /// destinations in [`ReplicationState::Failed`].
    pub failure_message: Option<&'static str>,
}

impl ComAmazonawsS3tablesS3tablebucketsState {
    /// The replication configuration that replicates `table`, or `None` when
    /// nothing does.
    ///
    /// A table's OWN configuration is what replicates it once a put sets one,
    /// because table-level replication is defined "independently of
    /// bucket-level replication, providing granular control over which tables
    /// are replicated and where" (`PutTableReplication`); a table that carries
    /// none is replicated by the configuration of the table bucket holding it,
    /// whose rules "define which tables to replicate and where to replicate
    /// them" (`TableBucketReplicationConfiguration`) and therefore cover the
    /// tables of that bucket.
    pub fn replicating_configuration<'a>(
        &'a self,
        table: &'a Table,
    ) -> Option<&'a ReplicationConfiguration> {
        if let Some(own) = table.replication.as_ref() {
            return Some(own);
        }
        let location = self.tables_by_arn.get(&table.table_arn)?;
        self.table_buckets
            .get(&location.table_bucket_arn)?
            .replication
            .as_ref()
    }

    /// Provision the replica of the table `source_table_arn` at every
    /// destination table bucket `destinations` names, answering the ARN of each
    /// replica that stands at a destination afterwards.
    ///
    /// This is what the write that SETS a replication configuration leaves at
    /// the destinations: the replica lives under the source table's own
    /// namespace and name, in a namespace created there when the destination
    /// holds none of that name — which is why setting a configuration takes
    /// permission to create a namespace and a table at the destination
    /// (`PutTableReplication`).
    ///
    /// IDEMPOTENT, and it never takes anything over: a destination table bucket
    /// this store does not hold takes no replica (a configuration may name one
    /// of another account), a name a CALLER's own table stands under is left
    /// exactly as it is, and a destination already holding the replica is
    /// answered with the ARN it holds. The work is bounded by the destinations
    /// the model allows one configuration (`@length(min: 1, max: 5)`), and each
    /// destination's own bounds ([`super::NAMESPACES_PER_TABLE_BUCKET`],
    /// [`super::TABLES_PER_NAMESPACE`]) refuse a replica past them.
    pub fn provision_replicas(
        &mut self,
        source_table_arn: &str,
        destinations: &[String],
        now: SystemTime,
    ) -> Vec<String> {
        let Some(location) = self.tables_by_arn.get(source_table_arn).cloned() else {
            return Vec::new();
        };
        let mut provisioned = Vec::with_capacity(destinations.len());
        for destination in destinations {
            // A table is not its own replica, so a destination naming the
            // bucket the source itself lives in takes none.
            if destination == &location.table_bucket_arn {
                continue;
            }
            if !self.table_buckets.contains_key(destination) {
                continue;
            }
            if self
                .ensure_namespace(destination, &location.namespace, now)
                .is_err()
            {
                continue;
            }
            if let Ok(replica) = self.create_service_managed_table(
                destination,
                &location.namespace,
                &location.name,
                REPLICATION_SERVICE_PRINCIPAL,
                now,
            ) {
                provisioned.push(replica);
            }
        }
        provisioned
    }

    /// Every table of the table bucket `table_bucket_arn`, by its own ARN.
    ///
    /// What a configuration set on the BUCKET covers: its rules "define which
    /// tables to replicate and where to replicate them"
    /// (`TableBucketReplicationConfiguration`), so the tables the bucket holds
    /// are the tables it replicates. The list is as long as the bucket's
    /// resident tables, which this store's own bounds cap
    /// ([`super::NAMESPACES_PER_TABLE_BUCKET`], [`super::TABLES_PER_NAMESPACE`]).
    pub fn table_arns_in_bucket(&self, table_bucket_arn: &str) -> Vec<String> {
        self.table_buckets
            .get(table_bucket_arn)
            .into_iter()
            .flat_map(|bucket| bucket.namespaces.values())
            .flat_map(|namespace| namespace.tables.values())
            .map(|table| table.table_arn.clone())
            .collect()
    }

    /// The state of replication of `table` to each destination that replicates
    /// it, as of `at`, or `None` when no configuration replicates the table at
    /// all — a table with no destination has no status to report.
    ///
    /// One entry per destination the covering configuration's rules name, in
    /// the order those rules hold them, so the report is as wide as the
    /// configuration and in one stable order. A destination is pending until
    /// the destination table bucket holds the replica and the replication of
    /// the table's current state to it is due, completed once both hold, and
    /// failed when the store holds no such destination table bucket at all.
    pub fn table_replication_status(
        &self,
        table: &Table,
        at: SystemTime,
    ) -> Option<Vec<ReplicationDestinationStatus>> {
        let configuration = self.replicating_configuration(table)?;
        let location = self.tables_by_arn.get(&table.table_arn)?;
        // Replication copies the table AS IT STANDS, so what is owed is the
        // replication of its current state: the span runs from the later of the
        // instant the configuration was set and the instant the table last
        // changed. `checked_add`, so a clock far enough in the future that the
        // span does not fit cannot panic a read.
        let since = configuration.configured_at.max(table.modified_at);
        let completes_at = since
            .checked_add(REPLICATION_COMPLETION_DELAY)
            .unwrap_or(since);
        Some(
            configuration
                .rules
                .iter()
                .flat_map(|rule| rule.destinations.iter())
                .map(|destination| {
                    // A destination table bucket this store does not hold
                    // cannot be written to, so replication to it never
                    // completes: that is the modeled failed state, and the
                    // failure it states is that class of fact about the
                    // destination.
                    let Some(bucket) = self.table_buckets.get(destination) else {
                        return ReplicationDestinationStatus {
                            destination_table_bucket_arn: destination.clone(),
                            destination_table_arn: None,
                            state: ReplicationState::Failed,
                            last_successful_update: None,
                            failure_message: Some(BUCKET_NOT_FOUND_MESSAGE),
                        };
                    };
                    let replica = bucket
                        .namespaces
                        .get(&location.namespace)
                        .and_then(|namespace| namespace.tables.get(&location.name))
                        // The REPLICA, not merely a table standing under the
                        // same name: a table a caller created at the
                        // destination is its own, so it is never reported as
                        // what replication left there.
                        .filter(|held| {
                            held.managed_by_service.as_deref() == Some(REPLICATION_SERVICE_PRINCIPAL)
                        })
                        .map(|held| held.table_arn.clone());
                    // A replication is reported completed only when BOTH hold:
                    // the destination holds the replica, which is what a
                    // completed replication leaves behind there, and the
                    // replication of the table's current state to it is due. So
                    // a destination nothing has been replicated to is reported
                    // as still pending rather than as work this emulator never
                    // performed.
                    let completed = replica.is_some() && at >= completes_at;
                    ReplicationDestinationStatus {
                        destination_table_bucket_arn: destination.clone(),
                        destination_table_arn: replica,
                        state: if completed {
                            ReplicationState::Completed
                        } else {
                            ReplicationState::Pending
                        },
                        // The update a completed replication copied is the
                        // table's metadata document, so a table that has no
                        // metadata file yet has no replicated update to report
                        // even once the replication is due. The instant is the
                        // one the replication completed at, which does not move
                        // because somebody asked again.
                        last_successful_update: completed
                            .then(|| table.metadata_location.clone())
                            .flatten()
                            .map(|metadata_location| ReplicatedUpdate {
                                metadata_location,
                                timestamp: completes_at,
                            }),
                        // Nothing refuses a destination this store holds, so
                        // such a destination carries no failure to state.
                        failure_message: None,
                    }
                })
                .collect(),
        )
    }
}
