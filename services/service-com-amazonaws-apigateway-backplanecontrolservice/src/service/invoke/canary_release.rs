// SPDX-License-Identifier: CC0-1.0

//! WHICH release of an api one request to a stage is served from: the stage's own deployment,
//! or the CANARY the stage carries beside it.
//!
//! A canary is a second deployment a stage serves part of its traffic from:
//! `CanarySettings$deploymentId` is "the ID of the canary deployment",
//! `CanarySettings$percentTraffic` "the percent (0-100) of traffic diverted to a canary
//! deployment" and `CanarySettings$stageVariableOverrides` "the stage variable overrides used
//! for the canary release deployment" — "the values of these stage variables override any
//! values defined in the deployment stage". So a stage with a canary serves TWO
//! configurations at once, and which one answers a given request is decided per request, by
//! that share:
//!
//! * `100` diverts every request to the canary, `0` none of it, and a share between them that
//!   fraction of them — drawn independently for each request, because the share is of the
//!   TRAFFIC rather than a property of any one call, so nothing a caller sends decides which
//!   release answers it;
//! * a canary with no `deploymentId` of its own serves the deployment the STAGE serves (the
//!   reading [`crate::state::StageEntry::canary_settings`] records), so such a canary differs
//!   from the base release only in the variables it overrides;
//! * the overrides are applied ON TOP of the stage's own `variables`: a name the canary
//!   overrides takes the canary's value and a name it does not keep the stage's, which is what
//!   makes one canary deployment reachable with its own backend, its own release label and the
//!   stage's remaining configuration.
//!
//! Everything else about serving the request is unchanged: the release decides only the
//! DEPLOYMENT the call is routed and served out of and the `$stageVariables` map the
//! integration's configuration and the call's mapping templates are resolved against.

use super::*;

/// Which release of the api a request is served from — the deployment it is routed out of and
/// the stage variables in force for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ServedRelease {
    /// The id of the deployment this request is served out of: the canary's where the request
    /// was diverted to it, else the stage's own.
    pub(super) deployment_id: String,
    /// The stage variables in force for this request: the stage's own map, with the canary's
    /// `stageVariableOverrides` applied over it where the request was diverted to the canary.
    pub(super) stage_variables: BTreeMap<String, String>,
    /// Whether this request may be answered from — and have its answer held in — the
    /// STAGE's cache. True for the base release, and for a canary only where
    /// `CanarySettings$useStageCache` says so ("whether the canary deployment uses the stage
    /// cache or not"), so a canary that does not use it is answered by its own deployment
    /// rather than out of what the base release cached.
    pub(super) uses_stage_cache: bool,
}

/// The release `stage` serves THIS request from.
///
/// `diverted` is the share of traffic this draw fell in, in `0.0..100.0`: a request is served
/// by the canary when the draw is below the canary's own `percentTraffic`, so a share of `0`
/// diverts nothing and `100` diverts everything. Taken by the caller rather than drawn here so
/// the decision is one value, testable on its own.
pub(super) fn released(stage: &StageEntry, diverted: f64) -> ServedRelease {
    let base = ServedRelease {
        deployment_id: stage.deployment_id.clone(),
        stage_variables: stage.variables.clone().unwrap_or_default(),
        uses_stage_cache: true,
    };
    let Some(canary) = stage.canary_settings.as_ref() else {
        return base;
    };
    if diverted >= canary.percent_traffic {
        return base;
    }
    let mut stage_variables = base.stage_variables;
    for (name, value) in canary.stage_variable_overrides.iter().flatten() {
        stage_variables.insert(name.clone(), value.clone());
    }
    ServedRelease {
        // A canary carrying no deployment of its own serves the stage's.
        deployment_id: canary
            .deployment_id
            .clone()
            .unwrap_or_else(|| stage.deployment_id.clone()),
        stage_variables,
        uses_stage_cache: canary.use_stage_cache,
    }
}

/// The share of traffic one request falls in: a draw in `0.0..100.0`, taken independently for
/// each request, which [`released`] compares against the canary's `percentTraffic`.
pub(super) fn traffic_draw() -> f64 {
    ::rand::Rng::random_range(&mut ::rand::rng(), 0.0..100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::CanarySettings;

    /// A stage serving `deployment`, with `variables` as its own map and `canary` beside it.
    fn stage(
        deployment: &str,
        variables: &[(&str, &str)],
        canary: Option<CanarySettings>,
    ) -> StageEntry {
        StageEntry {
            stage_name: "live".to_string(),
            deployment_id: deployment.to_string(),
            variables: Some(
                variables
                    .iter()
                    .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                    .collect(),
            ),
            canary_settings: canary,
            ..StageEntry::default()
        }
    }

    /// A canary of `deployment` taking `percent_traffic`, overriding `overrides`, which uses
    /// the stage's cache as `uses_cache` says.
    fn canary(
        percent_traffic: f64,
        deployment: Option<&str>,
        overrides: &[(&str, &str)],
        uses_cache: bool,
    ) -> CanarySettings {
        CanarySettings {
            percent_traffic,
            deployment_id: deployment.map(str::to_string),
            stage_variable_overrides: Some(
                overrides
                    .iter()
                    .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                    .collect(),
            ),
            use_stage_cache: uses_cache,
        }
    }

    #[test]
    fn the_share_of_traffic_the_draw_falls_in_decides_which_release_serves_the_request() {
        // Rows differ along what the rule generalises over: whether the stage has a canary at
        // all, the share it takes (both published bounds and one between them), where the
        // draw falls relative to that share, whether the canary names a deployment of its own,
        // whether it overrides a name the stage defines or one it does not, and whether it uses
        // the stage's cache.
        /// One row: the case, the canary configured on the stage, the share draw for this
        /// request, the deployment it must be served out of, whether the stage cache may
        /// answer it, and the variables in force.
        type Row<'a> = (
            &'a str,
            Option<CanarySettings>,
            f64,
            &'a str,
            bool,
            &'a [(&'a str, &'a str)],
        );
        let cases: [Row<'_>; 9] = [
            (
                "a stage with no canary at all",
                None,
                0.0,
                "base-deployment",
                true,
                &[("release", "base"), ("region", "primary")],
            ),
            (
                "a canary taking no traffic",
                Some(canary(
                    0.0,
                    Some("canary-deployment"),
                    &[("release", "canary")],
                    false,
                )),
                0.0,
                "base-deployment",
                true,
                &[("release", "base"), ("region", "primary")],
            ),
            (
                "a canary taking all of the traffic",
                Some(canary(
                    100.0,
                    Some("canary-deployment"),
                    &[("release", "canary")],
                    false,
                )),
                99.999,
                "canary-deployment",
                false,
                &[("release", "canary"), ("region", "primary")],
            ),
            (
                "a canary taking all of the traffic that uses the stage cache",
                Some(canary(
                    100.0,
                    Some("canary-deployment"),
                    &[("release", "canary")],
                    true,
                )),
                0.0,
                "canary-deployment",
                true,
                &[("release", "canary"), ("region", "primary")],
            ),
            (
                "a draw below a partial share",
                Some(canary(
                    25.0,
                    Some("canary-deployment"),
                    &[("release", "canary")],
                    false,
                )),
                24.9,
                "canary-deployment",
                false,
                &[("release", "canary"), ("region", "primary")],
            ),
            (
                "a draw at a partial share",
                Some(canary(
                    25.0,
                    Some("canary-deployment"),
                    &[("release", "canary")],
                    false,
                )),
                25.0,
                "base-deployment",
                true,
                &[("release", "base"), ("region", "primary")],
            ),
            (
                "a canary with no deployment of its own",
                Some(canary(100.0, None, &[("release", "canary")], false)),
                0.0,
                "base-deployment",
                false,
                &[("release", "canary"), ("region", "primary")],
            ),
            (
                "a canary overriding a name the stage does not define",
                Some(canary(
                    100.0,
                    Some("canary-deployment"),
                    &[("extra", "only-canary")],
                    false,
                )),
                0.0,
                "canary-deployment",
                false,
                &[
                    ("extra", "only-canary"),
                    ("release", "base"),
                    ("region", "primary"),
                ],
            ),
            (
                "a canary overriding nothing",
                Some(canary(100.0, Some("canary-deployment"), &[], false)),
                0.0,
                "canary-deployment",
                false,
                &[("release", "base"), ("region", "primary")],
            ),
        ];

        for (case, settings, diverted, deployment, uses_stage_cache, variables) in cases {
            let expected: BTreeMap<String, String> = variables
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect();
            let served = released(
                &stage(
                    "base-deployment",
                    &[("release", "base"), ("region", "primary")],
                    settings,
                ),
                diverted,
            );
            assert_eq!(
                served.deployment_id, deployment,
                "case {case:?}: expected the request served out of {deployment:?}, got {:?}",
                served.deployment_id
            );
            assert_eq!(
                served.stage_variables, expected,
                "case {case:?}: expected the variables {expected:?}, got {:?}",
                served.stage_variables
            );
            assert_eq!(
                served.uses_stage_cache, uses_stage_cache,
                "case {case:?}: expected the stage cache to be usable: {uses_stage_cache}, got \
                 {}",
                served.uses_stage_cache
            );
        }
    }

    #[test]
    fn every_draw_falls_inside_the_published_share_window() {
        // `CanarySettings$percentTraffic` is "the percent (0-100) of traffic diverted to a
        // canary deployment", so the draw a request is placed by has to fall in that window
        // for the two bounds to divert nothing and everything.
        for _ in 0..256 {
            let drawn = traffic_draw();
            assert!(
                (0.0..100.0).contains(&drawn),
                "expected a draw within the published 0-100 share window, got {drawn}"
            );
        }
    }
}
