// SPDX-License-Identifier: CC0-1.0

//! The WAVES an EC2/on-premises deployment takes its targets out of service in:
//! the one place a resolved deployment configuration's `minimumHealthyHosts`
//! bound is applied, and the one place a single target's `TargetStatus` is read
//! off the deployment's progress through those waves.
//!
//! `minimumHealthyHosts` is "the minimum number or percentage of instances that
//! should be healthy at any time during a deployment"
//! (`smithy_query(query="doc",shape="DeploymentConfigInfo",member="minimumHealthyHosts")`),
//! stated either way by `MinimumHealthyHostsType`
//! (`smithy_query(query="shape",shape="MinimumHealthyHostsType")`:
//! HOST_COUNT | FLEET_PERCENT) — "the minimum number of healthy instances as an
//! absolute value", or as a percentage of the fleet, which CodeDeploy "converts
//! […] to the equivalent number of instances and rounds up fractional
//! instances". It is therefore a bound on how many of a deployment's targets may
//! be out of service AT ONE INSTANT, and the model states the arithmetic itself:
//! "In an example of nine instances, if a HOST_COUNT of six is specified, deploy
//! to up to three instances at a time".
//!
//! So a deployment over a fleet of N targets under a bound of H healthy hosts
//! deploys to at most N - H of them at once — never fewer than one, or no
//! deployment could make progress at all — and works through the fleet in waves
//! of that size, one wave at a time. Which is what a client reads: at every
//! instant the targets of the deployment partition into the ones a past wave
//! finished, the ones the current wave is deploying to, and the ones still ahead
//! of it (`super::deployment_summary::deployment_overview` counts exactly that
//! partition), and a deployment bounded to a fraction of its fleet is held open
//! for one span per wave, so it takes longer than the same revision deployed to
//! the whole fleet at once.
//!
//! A deployment whose configuration states no bound — the Lambda and Amazon ECS
//! built-ins carry none, `minimumHealthyHosts` being the EC2/on-premises member —
//! deploys to everything it resolved in ONE wave, which is the progression every
//! reader of such a deployment saw before this module existed.

use crate::ops::types::MinimumHealthyHosts;

use super::{
    Deployment, DeploymentTargetSnapshot, DEPLOYMENT_STATUS_FAILED, DEPLOYMENT_STATUS_STOPPED,
    DEPLOYMENT_STATUS_SUCCEEDED, DEPLOYMENT_WAVE_MILLIS, TARGET_STATUS_FAILED,
    TARGET_STATUS_IN_PROGRESS, TARGET_STATUS_PENDING, TARGET_STATUS_SKIPPED,
    TARGET_STATUS_SUCCEEDED,
};

/// The `MinimumHealthyHostsType` values the model carries
/// (`smithy_query(query="shape",shape="MinimumHealthyHostsType")`).
const HOST_COUNT: &str = "HOST_COUNT";
/// See [`HOST_COUNT`].
const FLEET_PERCENT: &str = "FLEET_PERCENT";

/// How many of a fleet of `fleet` targets a deployment under `minimum_healthy_hosts`
/// may take out of service at one instant.
///
/// The fleet size less the healthy hosts the configuration requires, and never
/// less than one target: a bound that would leave nothing deployable still has
/// to deploy — CodeDeploy's own `CodeDeployDefault.OneAtATime` is
/// `FLEET_PERCENT` 99, which on any fleet rounds up to "every host healthy" and
/// means one host at a time. A configuration stating no bound (or a type this
/// service does not know) bounds nothing, so the whole fleet goes at once.
#[must_use]
pub fn concurrency(minimum_healthy_hosts: Option<&MinimumHealthyHosts>, fleet: usize) -> usize {
    if fleet == 0 {
        return 0;
    }
    let Some(bound) = minimum_healthy_hosts else {
        return fleet;
    };
    let healthy = match bound.r#type.as_deref() {
        Some(HOST_COUNT) => usize::try_from(bound.value.max(0)).unwrap_or(fleet).min(fleet),
        Some(FLEET_PERCENT) => percent_of(fleet, bound.value),
        _ => return fleet,
    };
    fleet.saturating_sub(healthy).max(1)
}

/// `percent` of a fleet of `fleet` targets as a number of targets, "rounded up"
/// as the member documents and clamped to the percentages a percentage can be.
fn percent_of(fleet: usize, percent: i32) -> usize {
    let percent = usize::try_from(percent.clamp(0, 100)).unwrap_or(0);
    (fleet * percent).div_ceil(100)
}

/// Put each of `targets` in the wave that deploys to it, `concurrency` targets to
/// a wave, in resolution order.
pub fn assign_waves(targets: &mut [DeploymentTargetSnapshot], concurrency: usize) {
    let per_wave = concurrency.max(1);
    for (index, target) in targets.iter_mut().enumerate() {
        target.wave = index / per_wave;
    }
}

/// How many waves `targets` are deployed to in: none at all for a deployment that
/// resolved no target, and otherwise one more than the last wave an assigned
/// target belongs to.
#[must_use]
pub fn wave_count(targets: &[DeploymentTargetSnapshot]) -> usize {
    targets.iter().map(|target| target.wave + 1).max().unwrap_or(0)
}

/// The instant a deployment that started at `starts_at_ms` has worked through
/// `waves` waves of its targets.
///
/// A deployment that paces its fleet is held open LONGER than one that takes the
/// whole fleet out of service at once — a wave starts only once the wave before it
/// is finished — which is the window a client reads as `createTime` to
/// `completeTime`. How long exactly is not something CodeDeploy documents (it is
/// however long the agent takes on each host), so the window is CHOSEN, as the
/// span before a deployment starts is: one span for a deployment with a single
/// wave, and [`MAX_RUN_SPANS`] for one that paces, which is also what bounds the
/// window of a deployment over a large fleet ([`waves_finished_at`] spreads its
/// waves evenly across it).
#[must_use]
pub fn waves_complete_at_ms(starts_at_ms: i64, waves: usize) -> i64 {
    starts_at_ms.saturating_add(run_millis(waves))
}

/// How many spans a deployment pacing `waves` waves of targets is held open for:
/// one per wave, up to [`MAX_RUN_SPANS`].
fn run_millis(waves: usize) -> i64 {
    let spans = i64::try_from(waves.clamp(1, MAX_RUN_SPANS)).unwrap_or(1);
    DEPLOYMENT_WAVE_MILLIS.saturating_mul(spans)
}

/// The most spans a deployment is held open for, whatever its fleet size.
///
/// THIS EMULATOR's own bound, not an AWS one: without it a deployment of a
/// hundred hosts one at a time would be held open for a hundred spans, and
/// nothing a client of an emulator does needs to wait that out. What the bound
/// changes is only HOW LONG each wave is out of service, never the order of the
/// waves or how many targets are in one at an instant — the concurrency
/// `minimumHealthyHosts` bounds is what a client observes, and it holds at every
/// instant whatever the window.
pub const MAX_RUN_SPANS: usize = 2;

/// How many waves of `deployment`'s targets are finished at `now_ms`: its waves
/// spread evenly across the window it is held open for ([`run_millis`]), counted
/// from the instant the clock owes the deployment its start.
///
/// Read off that owed instant rather than off any instant of a reader's own, so
/// two readers of one deployment at one instant cannot see it on two different
/// waves.
#[must_use]
pub fn waves_finished_at(deployment: &Deployment, now_ms: i64) -> usize {
    let waves = wave_count(&deployment.targets);
    if waves == 0 {
        return 0;
    }
    let elapsed = now_ms.saturating_sub(deployment.starts_at_ms);
    if elapsed <= 0 {
        return 0;
    }
    let counted = i64::try_from(waves).unwrap_or(i64::MAX);
    let finished = elapsed.saturating_mul(counted) / run_millis(waves);
    usize::try_from(finished).unwrap_or(usize::MAX).min(waves)
}

/// The `TargetStatus` one target of `deployment` carries
/// (`smithy_query(query="shape",shape="TargetStatus")`: Pending | InProgress |
/// Succeeded | Failed | Skipped | Unknown | Ready).
///
/// A deployment that reached an outcome gives every one of its targets that
/// outcome — it `Succeeded` or `Failed` with the deployment, and one a caller
/// STOPPED leaves them `Skipped`, the revision never having been finished on them.
/// A deployment still in flight is as far through its waves as the clock has taken
/// it ([`waves_finished_at`], materialised onto the record by
/// [`super::ComAmazonawsCodedeployCodedeploy20141006State::advance_clock`]): a
/// target a finished wave deployed to `Succeeded`, the current wave's targets are
/// `InProgress`, and the ones in a wave the deployment has not started are
/// `Pending`. A deployment the clock has not started yet has `Pending` targets,
/// which no listing reaches: an operation over the targets of a deployment that
/// has not started is refused with `DeploymentNotStartedException` (see
/// [`Deployment::is_started`]).
#[must_use]
pub fn target_status(
    deployment: &Deployment,
    target: &DeploymentTargetSnapshot,
) -> &'static str {
    match deployment.status.as_str() {
        DEPLOYMENT_STATUS_SUCCEEDED => TARGET_STATUS_SUCCEEDED,
        DEPLOYMENT_STATUS_FAILED => TARGET_STATUS_FAILED,
        DEPLOYMENT_STATUS_STOPPED => TARGET_STATUS_SKIPPED,
        _ if deployment.is_started() => match target.wave.cmp(&deployment.target_waves_finished) {
            ::std::cmp::Ordering::Less => TARGET_STATUS_SUCCEEDED,
            ::std::cmp::Ordering::Equal => TARGET_STATUS_IN_PROGRESS,
            ::std::cmp::Ordering::Greater => TARGET_STATUS_PENDING,
        },
        _ => TARGET_STATUS_PENDING,
    }
}

#[cfg(test)]
mod tests {
    //! What the bound means, over the forms a configuration states it in. How a
    //! deployment then paces its targets — the concurrency a client observes, the
    //! partition its summary reports and the window it is held open for — is
    //! asserted end to end, through the operations a client calls, in
    //! `core/router/tests/all/codedeploy_minimum_healthy_hosts_paces_instance_targets.rs`.

    use super::*;

    /// A `minimumHealthyHosts` of `kind` and `value`.
    fn bound(kind: &str, value: i32) -> MinimumHealthyHosts {
        MinimumHealthyHosts {
            r#type: Some(kind.to_string()),
            value,
        }
    }

    /// RULE: the targets deployed to at once are the fleet less the healthy hosts
    /// the configuration requires — a percentage rounded UP to whole hosts — and
    /// at least one; a configuration that states no bound deploys to the whole
    /// fleet at once.
    #[test]
    fn the_targets_deployed_to_at_once_are_the_fleet_less_the_healthy_hosts() {
        // Rows differ along the type the bound is stated in, its value (each
        // published bound of a percentage and one past it included), and the size
        // of the fleet it is applied to.
        let cases: &[(&str, Option<MinimumHealthyHosts>, usize, usize)] = &[
            ("no bound at all, over a fleet", None, 4, 4),
            ("no bound at all, over no fleet", None, 0, 0),
            // The model's own example: "In an example of nine instances, if a
            // HOST_COUNT of six is specified, deploy to up to three instances at
            // a time".
            (
                "the model's nine instances under a HOST_COUNT of six",
                Some(bound(HOST_COUNT, 6)),
                9,
                3,
            ),
            // And its second example: "If a FLEET_PERCENT of 40 is specified,
            // deploy to up to five instances at a time" — of nine, 40% rounds up
            // to four healthy hosts.
            (
                "the model's nine instances under a FLEET_PERCENT of 40",
                Some(bound(FLEET_PERCENT, 40)),
                9,
                5,
            ),
            (
                "a HOST_COUNT of none, which is every host at once",
                Some(bound(HOST_COUNT, 0)),
                4,
                4,
            ),
            (
                "a HOST_COUNT one below the fleet",
                Some(bound(HOST_COUNT, 1)),
                2,
                1,
            ),
            (
                "a HOST_COUNT of the whole fleet, which still deploys to one",
                Some(bound(HOST_COUNT, 2)),
                2,
                1,
            ),
            (
                "a HOST_COUNT above the fleet",
                Some(bound(HOST_COUNT, 9)),
                2,
                1,
            ),
            (
                "a HOST_COUNT below the published minimum",
                Some(bound(HOST_COUNT, -1)),
                3,
                3,
            ),
            (
                "FLEET_PERCENT 0, the lowest percentage",
                Some(bound(FLEET_PERCENT, 0)),
                4,
                4,
            ),
            (
                "FLEET_PERCENT 100, the highest percentage",
                Some(bound(FLEET_PERCENT, 100)),
                4,
                1,
            ),
            (
                "FLEET_PERCENT past the highest percentage",
                Some(bound(FLEET_PERCENT, 150)),
                4,
                1,
            ),
            (
                "FLEET_PERCENT below the lowest percentage",
                Some(bound(FLEET_PERCENT, -10)),
                4,
                4,
            ),
            (
                "FLEET_PERCENT 50 over an even fleet",
                Some(bound(FLEET_PERCENT, 50)),
                4,
                2,
            ),
            (
                "FLEET_PERCENT 50 over an odd fleet, rounded up",
                Some(bound(FLEET_PERCENT, 50)),
                3,
                1,
            ),
            (
                "FLEET_PERCENT 75 over a fleet of four",
                Some(bound(FLEET_PERCENT, 75)),
                4,
                1,
            ),
            (
                "FLEET_PERCENT 25 over a fleet of eight",
                Some(bound(FLEET_PERCENT, 25)),
                8,
                6,
            ),
            // The EC2/on-premises built-in configurations, from the catalogue
            // this store seeds.
            (
                "CodeDeployDefault.OneAtATime over a fleet of three",
                Some(bound(FLEET_PERCENT, 99)),
                3,
                1,
            ),
            (
                "CodeDeployDefault.HalfAtATime over a fleet of four",
                Some(bound(FLEET_PERCENT, 50)),
                4,
                2,
            ),
            (
                "CodeDeployDefault.AllAtOnce over a fleet of four",
                Some(bound(HOST_COUNT, 0)),
                4,
                4,
            ),
            (
                "a bound over a fleet of nothing",
                Some(bound(HOST_COUNT, 1)),
                0,
                0,
            ),
            (
                "a type outside the model's enum, which bounds nothing",
                Some(bound("FLEET_FRACTION", 1)),
                3,
                3,
            ),
        ];

        for (case, bound, fleet, expected) in cases {
            let got = concurrency(bound.as_ref(), *fleet);
            assert_eq!(
                got, *expected,
                "case {case:?}: expected {expected} target(s) out of service at once, got {got}"
            );
        }
    }

    /// One resolved target, as a deployment's snapshot holds it.
    fn target() -> DeploymentTargetSnapshot {
        DeploymentTargetSnapshot {
            target_id: None,
            target_type: super::super::DEPLOYMENT_TARGET_TYPE_INSTANCE.to_string(),
            wave: 0,
            ..DeploymentTargetSnapshot::default()
        }
    }

    /// RULE: the waves are the targets in resolution order, cut into runs of the
    /// concurrency the bound allows, and the deployment runs one span per wave.
    #[test]
    fn the_waves_are_runs_of_the_concurrency_the_bound_allows() {
        // Rows differ along the fleet size and the concurrency, so the number of
        // waves follows from the division: exact, with a remainder, one wave, and
        // a fleet of nothing.
        let cases: &[(&str, usize, usize, &[usize], usize)] = &[
            ("one at a time over three", 3, 1, &[0, 1, 2], 3),
            ("two at a time over four", 4, 2, &[0, 0, 1, 1], 2),
            ("two at a time over five", 5, 2, &[0, 0, 1, 1, 2], 3),
            ("all at once over three", 3, 3, &[0, 0, 0], 1),
            ("more at once than there are", 2, 9, &[0, 0], 1),
            ("no target at all", 0, 0, &[], 0),
        ];

        for (case, fleet, concurrency, expected, waves) in cases {
            let mut targets: Vec<DeploymentTargetSnapshot> =
                (0..*fleet).map(|_| target()).collect();
            assign_waves(&mut targets, *concurrency);
            let got: Vec<usize> = targets.iter().map(|target| target.wave).collect();
            assert_eq!(
                got.as_slice(),
                *expected,
                "case {case:?}: expected the waves {expected:?}, got {got:?}"
            );
            let counted = wave_count(&targets);
            assert_eq!(
                counted, *waves,
                "case {case:?}: expected {waves} wave(s), got {counted}"
            );
            // The window the deployment is held open for: one span for a single
            // wave, and the bounded window a paced deployment gets for more.
            let spans = i64::try_from((*waves).clamp(1, MAX_RUN_SPANS)).unwrap();
            let expected_end = 1_000 + spans * DEPLOYMENT_WAVE_MILLIS;
            let end = waves_complete_at_ms(1_000, counted);
            assert_eq!(
                end, expected_end,
                "case {case:?}: expected the last wave to end at {expected_end}, got {end}"
            );
        }
    }
}
