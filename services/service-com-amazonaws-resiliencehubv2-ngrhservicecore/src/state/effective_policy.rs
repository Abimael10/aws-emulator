// SPDX-License-Identifier: CC0-1.0

//! The derivation behind `Service$effectivePolicyValues`.
//!
//! A service is held to the resilience targets of EVERY policy that reaches it, not just
//! the one its own `policyArn` names: an `associatedSystems` entry's `userJourneyIds` are
//! "the list of user journey identifiers that associate this system with the service", and
//! a [`UserJourney`](super::UserJourneyRecord) carries its own `policyArn` — so the
//! journeys a service is associated through contribute their policies' targets too.
//!
//! Where two contributing policies set the SAME target, the service is held to the
//! STRICTER of the two, and the entry names the policy that supplied the value it reports:
//! the HIGHEST availability SLO, the LOWEST RTO, RPO and time between backups, and the most
//! resilient disaster recovery approach (see
//! [`DISASTER_RECOVERY_APPROACHES_STRICTEST_FIRST`]). A target no contributing policy sets
//! derives nothing, so its entry stays absent, and a service nothing reaches derives the
//! empty object.
//!
//! Inheritance derives VALUES, not a pointer: the service's own `policyArn` member is the
//! caller's input and is left exactly as stored, and
//! [`Policy$associatedServiceCount`](super::ComAmazonawsResiliencehubv2NgrhservicecoreState::associated_service_count)
//! keeps counting only the services whose own `policyArn` names the policy.
//!
//! Everything here is resolved at PROJECTION time from the policies, systems and journeys
//! under the same lock, so editing a policy, repointing a journey or narrowing an
//! association shows up in the next read of the service without the service being written
//! to. The fold visits at most one policy per association entry and journey id, and the
//! model bounds both lists at twenty entries (`AssociatedSystemList` and
//! `UserJourneyIdList`, each `@length max 20`), so what one read costs is bounded by the
//! stored association rather than by anything a caller can grow.

use crate::ops::types::{DisasterRecoverySource, EffectivePolicyValues, SloSource, TargetSource};

use super::policies::{arn_account, POLICY_VALUE_SOURCE_CROSS_ACCOUNT, POLICY_VALUE_SOURCE_SELF};
use super::{ComAmazonawsResiliencehubv2NgrhservicecoreState, ServiceRecord};

/// The disaster recovery approaches, most resilient FIRST.
///
/// SOURCE: the model's `MultiAzDisasterRecoveryApproach` and
/// `MultiRegionDisasterRecoveryApproach` enums
/// (`smithy_query(query="shape", shape="MultiAzDisasterRecoveryApproach")`), which both
/// declare their five values as a ladder running from the approach that keeps a second
/// copy serving traffic down to the one that restores from a backup. The order is what
/// decides which of two contributing policies' approaches a service is held to: the one
/// nearer the front.
pub(super) const DISASTER_RECOVERY_APPROACHES_STRICTEST_FIRST: [&str; 5] = [
    "ACTIVE_ACTIVE",
    "HOT_STANDBY",
    "WARM_STANDBY",
    "PILOT_LIGHT",
    "BACKUP_AND_RESTORE",
];

impl ComAmazonawsResiliencehubv2NgrhservicecoreState {
    /// The `EffectivePolicyValues` in force for `service`.
    ///
    /// Folded over every policy that reaches the service — its own, then the policy of
    /// every user journey its `associatedSystems` entries name — keeping for each target
    /// the strictest value offered and the name of the policy that offered it. A service
    /// with nothing to derive gets the empty object.
    pub fn effective_policy_values(&self, service: &ServiceRecord) -> EffectivePolicyValues {
        let mut effective = EffectivePolicyValues::default();
        for policy_arn in self.contributing_policy_arns(service) {
            let Some(record) = self.policies.get(policy_arn) else {
                // A pointer that no longer resolves to a stored policy contributes
                // nothing, the way a service's own dangling `policyArn` does.
                continue;
            };
            // Which account the value came from, as `PolicyValueSource` reports it: the
            // account that owns the contributing policy, against the service's own.
            let source = if arn_account(policy_arn) == Some(service.account_id.as_str()) {
                POLICY_VALUE_SOURCE_SELF
            } else {
                POLICY_VALUE_SOURCE_CROSS_ACCOUNT
            };
            keep_strictest_targets(&mut effective, record.effective_policy_values(source));
        }
        effective
    }

    /// Every policy whose targets reach `service`, in contribution order: the service's own
    /// `policyArn` first, then the `policyArn` of each user journey its `associatedSystems`
    /// entries name, in the order the entries and their ids were stored.
    ///
    /// Each ARN appears once — a policy attached to the service AND named by one of its
    /// journeys offers the same values either way — and an entry naming a system this store
    /// does not hold, or an id naming no journey of that system, contributes nothing.
    fn contributing_policy_arns<'a>(&'a self, service: &'a ServiceRecord) -> Vec<&'a str> {
        let mut arns: Vec<&str> = Vec::new();
        if let Some(own) = service.policy_arn.as_deref() {
            arns.push(own);
        }
        for entry in &service.associated_systems {
            let Some(system) = self.systems.get(&entry.system_arn) else {
                continue;
            };
            for journey_id in entry.user_journey_ids.iter().flatten() {
                let Some(journey) = system.user_journeys.get(journey_id) else {
                    continue;
                };
                let Some(policy_arn) = journey.policy_arn.as_deref() else {
                    continue;
                };
                if !arns.contains(&policy_arn) {
                    arns.push(policy_arn);
                }
            }
        }
        arns
    }
}

/// Fold one contributing policy's projection into the values derived so far, keeping the
/// stricter of the two for every target either of them sets.
fn keep_strictest_targets(into: &mut EffectivePolicyValues, offered: EffectivePolicyValues) {
    keep_strictest(&mut into.availability_slo, offered.availability_slo, slo_rank);
    keep_strictest(&mut into.multi_az_rto, offered.multi_az_rto, target_rank);
    keep_strictest(&mut into.multi_az_rpo, offered.multi_az_rpo, target_rank);
    keep_strictest(&mut into.multi_region_rto, offered.multi_region_rto, target_rank);
    keep_strictest(&mut into.multi_region_rpo, offered.multi_region_rpo, target_rank);
    keep_strictest(
        &mut into.data_recovery_time_between_backups,
        offered.data_recovery_time_between_backups,
        target_rank,
    );
    keep_strictest(
        &mut into.multi_az_dr_approach,
        offered.multi_az_dr_approach,
        approach_rank,
    );
    keep_strictest(
        &mut into.multi_region_dr_approach,
        offered.multi_region_dr_approach,
        approach_rank,
    );
}

/// Keep whichever of the held and the `offered` entry is STRICTER, by `rank`: a lower rank
/// is the stricter requirement.
///
/// An entry nothing has offered yet is replaced by the first one offered, so the policies
/// are folded in contribution order and the first to offer a target supplies it until a
/// stricter one does — which is also what settles a tie, since an equal value is not
/// stricter than the one already held and leaves the earlier policy's name in place.
fn keep_strictest<S, V: PartialOrd>(
    current: &mut Option<S>,
    offered: Option<S>,
    rank: impl Fn(&S) -> Option<V>,
) {
    let Some(offered) = offered else {
        return;
    };
    let replace = match current.as_ref() {
        None => true,
        Some(held) => match (rank(&offered), rank(held)) {
            (Some(offered), Some(held)) => offered < held,
            // An entry carrying no value ranks nowhere: one that carries a value is the
            // stricter of the two, and one that does not never displaces it.
            (Some(_), None) => true,
            _ => false,
        },
    };
    if replace {
        *current = Some(offered);
    }
}

/// The rank of an availability SLO: the HIGHER percentage is the stricter target, so the
/// ranking is the negated value.
fn slo_rank(slo: &SloSource) -> Option<f64> {
    slo.value.map(|value| -value)
}

/// The rank of an RTO, RPO or time-between-backups target: the LOWER number of minutes is
/// the stricter target, so the value ranks as itself.
fn target_rank(target: &TargetSource) -> Option<i32> {
    target.value
}

/// The rank of a disaster recovery approach: its position in
/// [`DISASTER_RECOVERY_APPROACHES_STRICTEST_FIRST`]. A value outside that ladder ranks
/// behind every value on it, so a modelled approach is never displaced by an unrecognised
/// one.
fn approach_rank(approach: &DisasterRecoverySource) -> Option<usize> {
    let value = approach.value.as_deref()?;
    Some(
        DISASTER_RECOVERY_APPROACHES_STRICTEST_FIRST
            .iter()
            .position(|known| *known == value)
            .unwrap_or(DISASTER_RECOVERY_APPROACHES_STRICTEST_FIRST.len()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ops::types::{
        MULTI_AZ_DISASTER_RECOVERY_APPROACH_VALUES, MULTI_REGION_DISASTER_RECOVERY_APPROACH_VALUES,
    };

    /// RULE: the ladder ranks exactly the approaches the model accepts, so a value a policy
    /// can carry always has a position of its own rather than being ranked behind the whole
    /// ladder — which is what a value from outside the model gets.
    #[test]
    fn the_ladder_ranks_every_modelled_approach() {
        for values in [
            MULTI_AZ_DISASTER_RECOVERY_APPROACH_VALUES,
            MULTI_REGION_DISASTER_RECOVERY_APPROACH_VALUES,
        ] {
            assert_eq!(
                values.len(),
                DISASTER_RECOVERY_APPROACHES_STRICTEST_FIRST.len(),
                "the ladder ranks {} approaches, the model accepts {}: {values:?}",
                DISASTER_RECOVERY_APPROACHES_STRICTEST_FIRST.len(),
                values.len()
            );
            for value in values {
                let ranked = DISASTER_RECOVERY_APPROACHES_STRICTEST_FIRST.contains(value);
                assert!(
                    ranked,
                    "case {value:?}: the model accepts it, so the ladder has to rank it, \
                     and it holds {DISASTER_RECOVERY_APPROACHES_STRICTEST_FIRST:?}"
                );
            }
        }
        // And a value from outside the model ranks behind every value on the ladder, so it
        // never displaces one.
        let outside = DisasterRecoverySource {
            value: Some("NOT_A_MODELLED_APPROACH".to_string()),
            policy_name: None,
            source: None,
        };
        let rank = approach_rank(&outside);
        assert_eq!(
            rank,
            Some(DISASTER_RECOVERY_APPROACHES_STRICTEST_FIRST.len()),
            "an unmodelled approach ranks behind the ladder, got {rank:?}"
        );
    }
}
