// SPDX-License-Identifier: CC0-1.0

//! Which owner provisions `AWS::AutoScaling::ScalingPolicy` — and the guard that keeps it ONE.
//!
//! The type's arms live in the catalogue, in
//! [`crate::state::provisioner::auto_scaling_policies`]: the scaling policy is put through
//! Amazon EC2 Auto Scaling's own `PutScalingPolicy`, read back through its `DescribePolicies`
//! and removed by its `DeletePolicy`, and [`super::owner_for`] reaches those arms through its
//! closing `provisioner_for(resource_type).map(Owner::Builtin)`.
//!
//! This module holds no arm of its own, deliberately. It is the one place that states where the
//! type is provisioned, beside the guard that fails if a SECOND owner is ever introduced for it:
//! a per-type arm added here — or in any other module reached earlier by that walk — would
//! SHADOW the catalogue's, leaving the arms a client's deployment really wants dead in the tree
//! while a second copy answered. Nothing in a diff shows that; this test does.

#[cfg(test)]
mod tests {
    use super::super::{owner_for, Owner};
    use crate::state::provisioner::{auto_scaling_policies, Provisioner};

    /// RULE: exactly ONE owner provisions `AWS::AutoScaling::ScalingPolicy`, and it is the
    /// catalogue's own arm — so the policy a stack declares is put by the module that reads it
    /// back from Amazon EC2 Auto Scaling, and no second implementation can answer instead.
    #[test]
    fn the_scaling_policy_type_has_one_owner_and_it_is_the_catalogues_arm() {
        let resolved = owner_for(auto_scaling_policies::AUTOSCALING_SCALING_POLICY);
        assert_eq!(
            resolved,
            Some(Owner::Builtin(Provisioner::AutoScalingScalingPolicy)),
            "case {:?}: expected the catalogue's own arm to provision the type, got {resolved:?}",
            auto_scaling_policies::AUTOSCALING_SCALING_POLICY
        );
    }
}
