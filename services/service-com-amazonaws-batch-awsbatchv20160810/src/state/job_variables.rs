// SPDX-License-Identifier: CC0-1.0

//! The variables AWS Batch itself SETS in the container of a job it runs.
//!
//! # The rule this module implements
//!
//! A job's container sees more variables than its definition declares:
//! `ContainerProperties$environment` is "the environment variables to pass to a container" and
//! its own documentation reserves a prefix for the service — "environment variables cannot
//! start with `AWS_BATCH`. This naming convention is reserved for variables that Batch sets"
//! ([`RESERVED_VARIABLE_PREFIX`]). The AWS Batch User Guide names the ones it sets for a
//! single-node container job, and each is a fact of the job this store already holds: the job
//! it is running ([`JOB_ID_VARIABLE`]), which attempt of it is running
//! ([`JOB_ATTEMPT_VARIABLE`]), the queue it was submitted to ([`JQ_NAME_VARIABLE`]), the
//! compute environment its capacity came from ([`CE_NAME_VARIABLE`]) and — for a CHILD of an
//! array job — the index of the shard it owns ([`JOB_ARRAY_INDEX_VARIABLE`], "the array job
//! index ... zero-based").
//!
//! Because the prefix is the service's, these are set OVER anything a request declared under
//! one of these names: the value the container reads for its own index is the index this store
//! holds for it, whatever a definition carrying a reserved name asked for.
//!
//! That last one is what makes an array job useful at all: every child runs the ONE program
//! its definition declares, so the only thing that can tell one child's work from another's is
//! the index its container reads. A child whose index were absent would do the same work as
//! every other child, or none at all.
//!
//! # Where they go, and where they do NOT
//!
//! They are set where the container RUNS — on the Amazon ECS task definition a placed job's
//! container runs as ([`super::ecs_task`]) and in the session that performs the program of a
//! job placed on capacity no task runs for ([`super::container_run`]) — and nowhere else. They
//! are NOT written onto the job's stored container, so `DescribeJobs` keeps reporting the
//! `environment` the request carried: AWS Batch answers that member with what the job
//! definition and the overrides declared, and these are the service's own additions to the
//! container's process.
//!
//! Every value is read off the job's own record at the instant the container is configured, so
//! two reads of a job answer the same and nothing here depends on when it ran.

use super::*;

/// The prefix `ContainerProperties$environment` reserves for the variables AWS Batch sets.
pub const RESERVED_VARIABLE_PREFIX: &str = "AWS_BATCH";

/// `AWS_BATCH_JOB_ID`: the id of the job the container is running — for a child of an array
/// job, the child's own id.
pub const JOB_ID_VARIABLE: &str = "AWS_BATCH_JOB_ID";

/// `AWS_BATCH_JOB_ATTEMPT`: which attempt of the job is running, counted from 1, as
/// `retryStrategy` counts them (`RetryStrategy$attempts` is "the number of times to move a job
/// to the RUNNABLE status").
pub const JOB_ATTEMPT_VARIABLE: &str = "AWS_BATCH_JOB_ATTEMPT";

/// `AWS_BATCH_JOB_ARRAY_INDEX`: the index of the array child the container is running, which
/// `ArrayPropertiesDetail$index` ("the job index within the array that's associated with this
/// job") reports for the same job. Set for a child of an array job and for nothing else: a job
/// that is not one has no index to read.
pub const JOB_ARRAY_INDEX_VARIABLE: &str = "AWS_BATCH_JOB_ARRAY_INDEX";

/// `AWS_BATCH_JQ_NAME`: the name of the job queue the job was submitted to.
pub const JQ_NAME_VARIABLE: &str = "AWS_BATCH_JQ_NAME";

/// `AWS_BATCH_CE_NAME`: the name of the compute environment whose capacity the job was placed
/// on, and absent for a job no environment of this store has taken.
pub const CE_NAME_VARIABLE: &str = "AWS_BATCH_CE_NAME";

impl ComAmazonawsBatchAwsbatchv20160810State {
    /// The variables AWS Batch sets in the container of the job `id`, in the order above, and
    /// none at all for a job this store does not hold.
    ///
    /// Each one is read off the job's own record: the attempt is the one the container is
    /// about to run or is running (the last recorded attempt while it is still open, and the
    /// next one otherwise, which is what the placement is starting), the queue is the one the
    /// job carries, and the compute environment is the one its queue would place it on
    /// ([`Self::placement_environment`]) — the environment whose capacity runs it.
    ///
    /// Costs one map lookup per fact, and nothing is written.
    pub(super) fn reserved_job_variables(&self, id: &str) -> Vec<(String, String)> {
        let Some(job) = self.jobs.get(id) else {
            return Vec::new();
        };
        let attempt = match job.attempts.last() {
            Some(open) if open.ended_at.is_none() => job.attempts.len(),
            _ => job.attempts.len().saturating_add(1),
        };
        let mut set: Vec<(String, String)> = vec![
            (JOB_ID_VARIABLE.to_string(), job.job_id.clone()),
            (JOB_ATTEMPT_VARIABLE.to_string(), attempt.to_string()),
        ];
        if let Some(index) = job.array_index {
            set.push((JOB_ARRAY_INDEX_VARIABLE.to_string(), index.to_string()));
        }
        if let Some(queue) = self.job_queue(&job.job_queue_arn) {
            set.push((JQ_NAME_VARIABLE.to_string(), queue.name.clone()));
        }
        if let Some(environment) = self.placement_environment(id) {
            set.push((CE_NAME_VARIABLE.to_string(), environment.name.clone()));
        }
        set
    }
}
