// SPDX-License-Identifier: CC0-1.0

//! The work a cluster's BRING-UP owes its nodes, performed as it falls due rather than when
//! somebody next reports the cluster.
//!
//! # The rule this module implements
//!
//! Bringing a launched cluster up is the SERVICE's own work, not its reader's. "A list of
//! bootstrap actions to run before Hadoop starts on the cluster nodes"
//! (`RunJobFlow$BootstrapActions` doc) is run on the nodes once their capacity is up
//! ([`super::bootstrap_run`]), and "the location in Amazon S3 to write the log files of the job
//! flow. If a value is not provided, logs are not created" (`RunJobFlow$LogUri` doc) says the
//! files those nodes produce ARE created where a launch provided one ([`super::node_logs`]) —
//! so both happen for a cluster nobody calls about again, and a client that waits on the object
//! store rather than on Amazon EMR still sees them.
//!
//! Each of the two is already held in exactly one place; what this module adds is WHO drives
//! them when no request does. A cluster can be launched by a caller that then watches something
//! else entirely — the object store under its `LogUri`, an EventBridge rule, or a
//! CloudFormation stack whose `AWS::EMR::Cluster` it declared — so a bring-up performed only by
//! the next operation that reports the cluster is one that never happens at all. The pump that
//! already carries a cluster's lifecycle by the clock ([`super::cluster_events`]) carries this
//! with it, and because that pump stands down only once nothing is owed
//! ([`ComAmazonawsEmrElasticmapreduceState::owes_node_work`]), the files of a run it answered
//! for are written before it does.
//!
//! It changes WHEN nothing: both calls apply only what the stored due instants have already
//! made due, which is exactly what a reporting operation applies, and each performs one
//! action's run and one node's files ONCE however many callers arrive afterwards. Nothing
//! sleeps for a span here and no host process is started: the run is the bounded session
//! [`super::bootstrap_run`] documents, and the write is an Amazon S3 call made with this
//! service's store released.

use super::*;

/// How many clusters ONE pass performs this work for, leaving the rest to the next pass.
///
/// CHOSEN: the ceiling on what one wake-up of the clock spends on it, well above the clusters a
/// realistic account brings up at once, and far below the bounded cluster map
/// ([`MAX_CLUSTERS`]).
pub const MAX_CLUSTERS_PER_NODE_WORK_PASS: usize = 32;

impl ComAmazonawsEmrElasticmapreduceState {
    /// The clusters that still owe the work above, at most
    /// [`MAX_CLUSTERS_PER_NODE_WORK_PASS`] of them.
    #[must_use]
    pub fn clusters_owing_node_work(&self) -> Vec<String> {
        self.clusters
            .values()
            .filter(|cluster| owes_node_work(cluster))
            .take(MAX_CLUSTERS_PER_NODE_WORK_PASS)
            .map(|cluster| cluster.id.clone())
            .collect()
    }

    /// Whether ANY cluster this store holds still owes it, which is what keeps the clock's pump
    /// awake until the bring-up it is carrying has really happened.
    #[must_use]
    pub fn owes_node_work(&self) -> bool {
        self.clusters.values().any(owes_node_work)
    }
}

/// Whether this cluster still owes the work its own nodes do.
///
/// Three things, each the state of a record rather than a guess: a cluster in its bootstrap
/// phase whose actions have not all answered on all of its nodes
/// ([`ClusterRecord::bootstrap_actions_have_run`]) owes those runs; and a cluster whose launch
/// named a log location owes the files of each answered run and of each node that has come up
/// until they have been written. A cluster whose launch named NO location owes none of the
/// files — with nowhere to write them "logs are not created" — and one whose bootstrap phase is
/// over owes no further run.
fn owes_node_work(cluster: &ClusterRecord) -> bool {
    if cluster.status.state == cluster_state::BOOTSTRAPPING && !cluster.bootstrap_actions_have_run()
    {
        return true;
    }
    if cluster
        .log_uri
        .as_deref()
        .and_then(LogDestination::parse)
        .is_none()
    {
        return false;
    }
    if cluster.bootstrap_runs.iter().any(|run| !run.log_delivered) {
        return true;
    }
    cluster
        .ec2_instances
        .iter()
        .any(|instance| instance.ready_date_time.is_some() && !instance.node_log_delivered)
}

/// Perform the bring-up work every cluster of this store already owes: each bootstrap action
/// whose turn it is on each node, and then the log files those nodes have to push.
///
/// Called with NO store guard held, by the clock's own pump and by nothing else: each call it
/// makes takes the store only to read what is owed and to record what came of it, and the
/// Amazon S3 and IAM calls in between are made with it released. The runs are performed BEFORE
/// the files are taken, so a run this pass answered for leaves its files in this same pass
/// instead of waiting for another one.
pub async fn settle_due_node_work<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    now: SystemTime,
) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let owed = state.lock().clusters_owing_node_work();
    for cluster_id in owed {
        run_due_bootstrap_actions(engine, ctx, region, &cluster_id, now).await;
        deliver_due_node_logs(engine, ctx, region, &cluster_id, now).await;
    }
}
