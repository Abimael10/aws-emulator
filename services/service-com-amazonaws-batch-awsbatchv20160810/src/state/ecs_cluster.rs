// SPDX-License-Identifier: CC0-1.0

//! The Amazon ECS CLUSTER that backs a compute environment, held where Amazon ECS holds it.
//!
//! # The rule
//!
//! A compute environment AWS Batch orchestrates with Amazon ECS runs its jobs as tasks of an
//! Amazon ECS cluster, and that cluster is the SERVICE's: `ComputeEnvironmentDetail$ecsClusterArn`
//! is "the Amazon Resource Name (ARN) of the underlying Amazon ECS cluster that the compute
//! environment uses", and `DescribeComputeEnvironments`' own documentation points a caller at
//! it for an environment whose capacity the caller registers itself ("if you're using an
//! unmanaged compute environment, you can use the DescribeComputeEnvironment operation to
//! determine the `ecsClusterArn` that you launch your Amazon ECS container instances into").
//! So the ARN is reported for EVERY environment this service orchestrates with Amazon ECS —
//! whether Batch manages its capacity or the caller does — and for none it orchestrates with
//! Amazon EKS, where `eksClusterArn` of the environment's `eksConfiguration` is what names
//! the cluster instead ([`ComputeEnvironment::orchestrated_with_ecs`]).
//!
//! # Naming the cluster is not holding it
//!
//! The cluster is Amazon ECS's resource, so it is CREATED there, through that service's own
//! `CreateCluster` over this emulator's in-process callback — the same hop the log stream a
//! placement names is created over ([`super::job_logs`]) — and what this store keeps is the ARN
//! that service answered with. A client that asks Amazon ECS for the cluster therefore finds
//! it, which is the whole point of reporting the ARN: it is how a caller reaches the tasks of
//! the environment's jobs. Nothing is reported for a cluster nothing holds — an emulator with
//! no Amazon ECS mounted leaves the member absent rather than naming a cluster that is
//! nowhere.
//!
//! The environment's deletion takes the cluster with it: "when you delete a compute
//! environment, the underlying Amazon ECS cluster is deleted as well", so the removal the
//! clock materialises queues a `DeleteCluster` for the same cluster.
//!
//! # How it reaches Amazon ECS
//!
//! The work is QUEUED under this store's lock, where the environment is created and where its
//! removal falls due, and drained by the caller once that lock is released
//! ([`ComAmazonawsBatchAwsbatchv20160810State::take_pending_ecs_clusters`],
//! [`settle_backing_clusters`]) — the shape every cross-service delivery in this crate
//! follows, and for the same reason: no in-process request is made while a guard is held. The
//! queue is a ring bounded by [`MAX_PENDING_ECS_CLUSTERS`], which the compute environment cap
//! ([`limits::MAX_COMPUTE_ENVIRONMENTS`]) already bounds the demand on, and one drain makes at
//! most [`MAX_CLUSTER_CALLS_PER_DRAIN`] in-process calls.

use super::*;

/// The prefix AWS Batch names the cluster of a compute environment with: Batch's own
/// `AWSBatch-<compute environment name>-<environment id>`, which is why a client that lists
/// Amazon ECS clusters can tell the ones Batch provisioned from its own.
const CLUSTER_NAME_PREFIX: &str = "AWSBatch";

/// How many cluster calls this store holds undelivered.
///
/// This store's OWN bound, not a limit AWS publishes. A compute environment queues one
/// creation and one deletion in its life and the environments are capped
/// ([`limits::MAX_COMPUTE_ENVIRONMENTS`]), so the queue cannot outgrow twice that; it is a
/// ring, so the oldest undelivered call is dropped before a new one is queued rather than
/// letting the queue grow.
const MAX_PENDING_ECS_CLUSTERS: usize = 2 * limits::MAX_COMPUTE_ENVIRONMENTS;

/// How many of them ONE drain delivers, under [`MAX_PUBLISHED_PER_DRAIN`]'s rule: what a drain
/// leaves behind keeps the clock pump awake
/// ([`ComAmazonawsBatchAwsbatchv20160810State::next_job_due_or_release_pump`]), so nothing is
/// dropped — it is delivered a tick later.
const MAX_CLUSTER_CALLS_PER_DRAIN: usize = 25;

/// The cluster of the compute environment `name`, as AWS Batch names one:
/// `AWSBatch-<compute environment name>-<the environment's own id>`.
///
/// The id is the environment's `uuid`, the identifier this service already mints once per
/// environment ([`ComputeEnvironment::uuid`]), so the cluster name is derived from the
/// environment rather than minted a second time.
pub fn backing_cluster_name(name: &str, uuid: &str) -> String {
    format!("{CLUSTER_NAME_PREFIX}-{name}-{uuid}")
}

/// What ONE compute environment's cluster still owes Amazon ECS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClusterCall {
    /// Create the cluster backing the compute environment of this name, and record the ARN
    /// Amazon ECS answers with on that environment.
    Create,
    /// Delete the cluster that backed an environment whose removal has fallen due.
    Delete,
}

/// ONE call this store owes Amazon ECS for a compute environment's backing cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingClusterCall {
    /// What is owed.
    pub call: ClusterCall,
    /// The compute environment the cluster backs, which is where a created cluster's ARN is
    /// recorded.
    pub compute_environment_name: String,
    /// The cluster itself ([`backing_cluster_name`]).
    pub cluster_name: String,
    /// The Region whose Amazon ECS holds it — the Region of the request that created the
    /// environment.
    pub region: String,
}

impl ComputeEnvironment {
    /// Whether Amazon ECS is what this environment's jobs run as tasks of.
    ///
    /// `ComputeEnvironmentDetail$containerOrchestrationType` is "the orchestration type of the
    /// compute environment" and an environment created with an `eksConfiguration` is the EKS
    /// one, whose cluster is named by that configuration's own `eksClusterArn`; every other
    /// environment — managed or unmanaged — is orchestrated with Amazon ECS and so is backed
    /// by a cluster of that service. ONE reading, so the `ecsClusterArn` a reader reports and
    /// the cluster the creation provisions cannot disagree.
    pub fn orchestrated_with_ecs(&self) -> bool {
        self.eks_configuration.is_none()
    }
}

impl ComAmazonawsBatchAwsbatchv20160810State {
    /// Queue the creation of the cluster backing the compute environment `name`, and answer
    /// the cluster's name.
    ///
    /// Called from the ONE place a compute environment is created, so a cluster is queued once
    /// per environment and never once per read. A ring bounded by
    /// [`MAX_PENDING_ECS_CLUSTERS`].
    pub fn queue_backing_cluster(&mut self, name: &str, uuid: &str, region: &str) -> String {
        let cluster_name = backing_cluster_name(name, uuid);
        self.queue_cluster_call(PendingClusterCall {
            call: ClusterCall::Create,
            compute_environment_name: name.to_string(),
            cluster_name: cluster_name.clone(),
            region: region.to_string(),
        });
        cluster_name
    }

    /// Queue the deletion of the cluster that backed `environment`, whose removal has just
    /// fallen due. Nothing is queued for an environment that never had one.
    pub(super) fn queue_backing_cluster_deletion(&mut self, environment: &ComputeEnvironment) {
        let Some(cluster_name) = environment.ecs_cluster_name.clone() else {
            return;
        };
        let region = arn_region(&environment.arn)
            .unwrap_or(DEFAULT_REGION)
            .to_string();
        self.queue_cluster_call(PendingClusterCall {
            call: ClusterCall::Delete,
            compute_environment_name: environment.name.clone(),
            cluster_name,
            region,
        });
    }

    /// Put one call on the ring, dropping the oldest undelivered one at the bound.
    fn queue_cluster_call(&mut self, call: PendingClusterCall) {
        while self.pending_ecs_clusters.len() >= MAX_PENDING_ECS_CLUSTERS {
            self.pending_ecs_clusters.pop_front();
        }
        self.pending_ecs_clusters.push_back(call);
    }

    /// Take at most [`MAX_CLUSTER_CALLS_PER_DRAIN`] of the calls not yet delivered, oldest
    /// first.
    ///
    /// Taking the records out under the lock is what stops two concurrent requests from making
    /// the same call twice.
    pub fn take_pending_ecs_clusters(&mut self) -> Vec<PendingClusterCall> {
        let taking = MAX_CLUSTER_CALLS_PER_DRAIN.min(self.pending_ecs_clusters.len());
        self.pending_ecs_clusters.drain(..taking).collect()
    }

    /// Whether this store still owes Amazon ECS a call, which is what keeps the clock pump
    /// awake until every backing cluster exists.
    pub(super) fn has_pending_ecs_clusters(&self) -> bool {
        !self.pending_ecs_clusters.is_empty()
    }

    /// Record the ARN Amazon ECS answered with for the cluster backing `name`.
    ///
    /// Written only while that environment still holds the cluster this call created, so a
    /// reader reports an ARN only for a cluster Amazon ECS really holds.
    fn record_backing_cluster(&mut self, name: &str, cluster_name: &str, cluster_arn: String) {
        if let Some(environment) = self.compute_environments.get_mut(name) {
            if environment.ecs_cluster_name.as_deref() == Some(cluster_name) {
                environment.ecs_cluster_arn = Some(cluster_arn);
            }
        }
    }
}

/// Make every cluster call this store has queued, for an operation of the compute environment
/// plane.
///
/// The seam a handler calls once it has released this store's lock: the environments' clusters
/// are Amazon ECS's resources, so provisioning one and deleting one are in-process requests to
/// that service, made with no guard held. The job plane reaches the same drain through its own
/// publisher ([`publish_job_state_changes`]), so there is ONE place a cluster call is made.
pub async fn settle_compute_environment_clusters<Eng: Send + Sync + 'static>(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    engine: &Eng,
) {
    let Some(state) = service_state(engine) else {
        return;
    };
    settle_backing_clusters(ctx.callback(), &state).await;
}

/// Make every cluster call this store has queued and nothing has made yet.
///
/// Called by the publisher once this store's lock is released, so no guard is held across the
/// in-process request. BEST EFFORT, as every cross-service delivery in this tree is: an
/// emulator with no Amazon ECS mounted to ask, and a cluster that service refuses, are
/// reported in tracing and the operation's own result stands — with no ARN recorded, so
/// nothing claims a cluster that is not there.
pub(super) async fn settle_backing_clusters(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    state: &::engine::ServiceState<ComAmazonawsBatchAwsbatchv20160810State>,
) {
    // The guard is taken and dropped inside this statement: the records are moved out here so
    // no call is made twice, and no lock is held across an await.
    let calls = state.lock().take_pending_ecs_clusters();
    for pending in calls {
        let client = ecs_client(callback, &pending.region);
        match pending.call {
            ClusterCall::Create => {
                match client
                    .create_cluster()
                    .cluster_name(&pending.cluster_name)
                    .send()
                    .await
                {
                    Ok(answered) => {
                        if let Some(arn) = answered
                            .cluster()
                            .and_then(|cluster| cluster.cluster_arn())
                            .map(str::to_string)
                        {
                            state.lock().record_backing_cluster(
                                &pending.compute_environment_name,
                                &pending.cluster_name,
                                arn,
                            );
                        }
                    }
                    Err(error) => {
                        ::tracing::warn!(
                            cluster = %pending.cluster_name,
                            error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                            "the Amazon ECS cluster backing a Batch compute environment could \
                             not be created, so no cluster ARN is reported for it"
                        );
                    }
                }
            }
            ClusterCall::Delete => {
                if let Err(error) = client.delete_cluster().cluster(&pending.cluster_name).send().await
                {
                    ::tracing::warn!(
                        cluster = %pending.cluster_name,
                        error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                        "the Amazon ECS cluster of a deleted Batch compute environment could \
                         not be deleted"
                    );
                }
            }
        }
    }
}

/// An Amazon ECS client over the in-process callback into this same emulator.
///
/// The callback is the framework's documented seam for calling another emulated service: it
/// routes in process, so no socket is opened and this crate names no other service's crate for
/// it. The configuration is the one every callback client in this workspace uses (see the
/// CloudWatch Logs client beside it in [`super::job_logs`]): nothing to retry, time out or
/// watch for a stalled stream, because the transport is in process, and the credentials the
/// callback hands over are the emulator's own.
pub(super) fn ecs_client(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    region: &str,
) -> ::aws_sdk_ecs::Client {
    let config = ::aws_sdk_ecs::Config::builder()
        .behavior_version(::aws_sdk_ecs::config::BehaviorVersion::latest())
        .region(::aws_sdk_ecs::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://ecs.{region}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(::aws_sdk_ecs::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ecs::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ecs::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_ecs::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_ecs::Client::from_conf(config)
}
