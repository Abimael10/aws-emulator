// SPDX-License-Identifier: CC0-1.0

//! Which SHARE of a job queue a submitted job lands in, and whether the request picked one
//! the queue can admit it into.
//!
//! A job queue can carry a scheduling policy, and a QUOTA-SHARE policy divides the queue's
//! capacity into named quota shares — each "a virtual queue with a configured compute
//! capacity" (`CreateQuotaShare`) created ON that queue. A job submitted to such a queue is
//! accounted against one of those shares, which is why `SubmitServiceJob$quotaShareName`
//! documents the requirement as conditional on the queue: "Don't specify this parameter if
//! the job queue doesn't have a quota share scheduling policy. If the job queue has a quota
//! share scheduling policy, then this parameter must be specified."
//!
//! So the two questions an admitting submit asks about its target queue live here, once:
//! whether the queue's attached policy is a quota-share one
//! ([`ComAmazonawsBatchAwsbatchv20160810State::queue_divides_capacity_by_quota_share`]) and
//! which share of that queue a name picks out
//! ([`ComAmazonawsBatchAwsbatchv20160810State::quota_share_on_queue`]) — the same lookup the
//! capacity arithmetic of a placed job does ([`super::queue_snapshot`]), so the share a
//! submit is admitted into is the share the scheduler later bounds it by.

use super::*;

impl ComAmazonawsBatchAwsbatchv20160810State {
    /// Whether the queue `queue_arn` divides its capacity into quota shares, i.e. whether
    /// the scheduling policy attached to it carries a `quotaSharePolicy`.
    ///
    /// A queue with no policy attached, and one whose policy carries a fair-share policy
    /// instead ([`SchedulingPolicy`] carries one or the other and never both), divides no
    /// capacity by quota share.
    pub fn queue_divides_capacity_by_quota_share(&self, queue_arn: &str) -> bool {
        self.job_queue(queue_arn)
            .and_then(|queue| queue.scheduling_policy_arn.as_deref())
            .and_then(|policy_arn| self.scheduling_policy(policy_arn))
            .is_some_and(|policy| policy.quota_share_policy.is_some())
    }

    /// The quota share called `name` on the queue `queue_arn`, or `None` when that queue
    /// carries no share under that name.
    ///
    /// A share's name is unique per QUEUE rather than per account — its identity is composed
    /// under the queue it hangs off ([`quota_share_arn`]) — so a share is only ever looked up
    /// as the pair.
    pub fn quota_share_on_queue(&self, queue_arn: &str, name: &str) -> Option<&QuotaShare> {
        self.quota_shares
            .values()
            .find(|share| share.job_queue_arn == queue_arn && share.name == name)
    }
}
