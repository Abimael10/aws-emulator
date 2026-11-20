// SPDX-License-Identifier: CC0-1.0

//! The TARGET of the automatic-scaling policy pair: the instance group the request names,
//! and the cluster that scopes it.
//!
//! One concern, split out of [`super`]: `PutAutoScalingPolicy` and
//! `RemoveAutoScalingPolicy` carry the same pair of members — "the ID of a cluster. The
//! instance group to which the automatic scaling policy is applied is within this cluster"
//! and "the ID of the instance group to which the scaling policy is applied" (their member
//! docs, Smithy model) — so the pair is resolved ONCE here and neither operation can judge
//! the two ids in a different order or word one refusal differently.
//!
//! Neither operation models a client error, so every sentence below is recorded behavior,
//! as is the ORDER the two ids are judged in. An instance group id names ONE group of ONE
//! cluster, and that reference is what Amazon EMR resolves first:
//!
//! * an `InstanceGroupId` no cluster of this account carries a group under is refused in
//!   terms of the INSTANCE GROUP ([`unknown_instance_group_message`]), whatever the
//!   request's `ClusterId` resolves to — including a `ClusterId` that resolves nothing
//!   either, where the group is still the value named;
//! * a group id that DOES name a group of this account, sent with a `ClusterId` the
//!   account holds no cluster for, is refused in terms of the CLUSTER
//!   ([`unknown_cluster_message`]) — the sentence every cluster-scoped operation of this
//!   service answers an unresolved cluster id with;
//! * a cluster that resolved while carrying no group under that id is refused in terms of
//!   the group again, because the group the call names is no part of the cluster it names.

use super::*;

/// The instance group an automatic-scaling policy call acts on, beside the ARN of the
/// cluster that scopes it — the member the attach answers next to the policy it persisted.
#[derive(Debug)]
pub struct AutoScalingTarget<'a> {
    pub cluster_arn: String,
    pub group: &'a mut InstanceGroupRecord,
}

impl ComAmazonawsEmrElasticmapreduceState {
    /// Whether any cluster of this account carries an instance group under
    /// `instance_group_id`.
    ///
    /// An instance group id is minted per group across every cluster of the account
    /// (`next_instance_group_seq`), so at most one group ever answers to one id and this
    /// says whether the reference names a group at all.
    ///
    /// The reference is read WITHOUT advancing a cluster's clock, because no transition a
    /// cluster owes adds a group to its record or takes one off — a group is created with
    /// the cluster and reported for the rest of its life, whatever state either has moved
    /// to — so what a clock-owed move would change is a group's STATE and never whether
    /// this id names one. The cluster the request named is advanced where it is resolved
    /// ([`Self::auto_scaling_target_mut`]), which is where a state-dependent decision is
    /// taken.
    pub fn holds_instance_group(&self, instance_group_id: &str) -> bool {
        self.clusters.values().any(|cluster| {
            cluster
                .groups
                .iter()
                .any(|group| group.id == instance_group_id)
        })
    }

    /// The instance group `instance_group_id` of the cluster `cluster_id`, with every
    /// transition the clock already made due applied — or the sentence the call is refused
    /// with, naming whichever of the two ids did not resolve (see this module's docs).
    pub fn auto_scaling_target_mut(
        &mut self,
        cluster_id: &str,
        instance_group_id: &str,
        now: SystemTime,
    ) -> Result<AutoScalingTarget<'_>, String> {
        if !self.holds_instance_group(instance_group_id) {
            return Err(unknown_instance_group_message(instance_group_id));
        }
        let Some(cluster) = self.cluster_mut(cluster_id, now) else {
            return Err(unknown_cluster_message(cluster_id));
        };
        let cluster_arn = cluster.arn.clone();
        match cluster.group_mut(instance_group_id) {
            Some(group) => Ok(AutoScalingTarget { cluster_arn, group }),
            None => Err(unknown_instance_group_message(instance_group_id)),
        }
    }
}
