// SPDX-License-Identifier: CC0-1.0

//! The deployment configuration a deployment group runs by when the request that
//! wrote it named NONE: the one place that default is chosen.
//!
//! # The rule this module implements
//!
//! `deploymentConfigName` is optional on a creating request, and a group that
//! names none still runs under one — "It is used if a configuration isn't
//! specified for the deployment or deployment group"
//! (`smithy_query(query="doc",shape="CreateDeploymentGroupInput",member="deploymentConfigName")`)
//! — so the service picks one of the configurations it predefines and stores it,
//! which is what every read of the group then reports.
//!
//! WHICH one it picks is not one constant: it depends on the deployment the group
//! would run, and that is two things about the group — the compute platform of the
//! application it belongs to, whose configurations are the only ones it can run
//! (`CreateDeploymentConfigInput$computePlatform`), and whether its
//! `deploymentStyle` routes the production traffic behind a load balancer
//! (`DeploymentOption`: WITH_TRAFFIC_CONTROL | WITHOUT_TRAFFIC_CONTROL). A style
//! that does is the one a traffic-SHIFTING configuration describes: the service
//! moves the production traffic from the original environment to the replacement
//! one itself, so the default it picks for such a group is the platform's
//! time-based canary rather than its single cutover, and the group's deployments
//! hold the canary's share of the traffic for the canary's interval.
//!
//! So the default is a function of those two things, read from
//! [`PLATFORM_DEFAULTS`] — one row per `ComputePlatform`
//! (`smithy_query(query="shape",shape="ComputePlatform")`: Server | Lambda | ECS),
//! each naming the configuration the platform defaults to for each answer, and
//! every name in it one of the configurations this service ships
//! (`super::BUILTIN_DEPLOYMENT_CONFIGS`, which this module's own test holds it
//! to).
//!
//! The model documents one entry of that table — "CodeDeployDefault.OneAtATime is
//! the default deployment configuration", the EC2/on-premises platform's — and the
//! rest are what the service is observed to assign. Where no traffic-controlled
//! default is established for a platform, its row names the platform's all-at-once
//! configuration for both answers rather than a traffic shift nothing established:
//! the configuration decides the PACING of every deployment of the group, so a
//! guess there would give a client a deployment that holds traffic on two versions
//! for minutes it never asked for.

use crate::ops::types::DeploymentStyle;

use super::{
    DEPLOYMENT_OPTION_WITH_TRAFFIC_CONTROL, DEPLOYMENT_TYPE_BLUE_GREEN,
};

/// What one compute platform defaults to, for each answer to "does this group's
/// style route the production traffic behind a load balancer".
struct PlatformDefaults {
    /// The `ComputePlatform` value this row is about.
    compute_platform: &'static str,
    /// The configuration a group of this platform defaults to when its style does
    /// NOT have the service move the production traffic: one cutover.
    cutover: &'static str,
    /// The configuration a group of this platform defaults to when its style is
    /// the blue/green deployment whose traffic the service routes
    /// ([`routes_traffic_between_environments`]).
    traffic_shifting: &'static str,
}

/// The default configuration of each compute platform, per the rule in this
/// module's own documentation.
const PLATFORM_DEFAULTS: &[PlatformDefaults] = &[
    // The EC2/on-premises platform ships no traffic-shifting configuration at all
    // — its built-ins carry `minimumHealthyHosts` rather than a
    // `trafficRoutingConfig` — so both answers are the configuration the model
    // documents for it.
    PlatformDefaults {
        compute_platform: "Server",
        cutover: "CodeDeployDefault.OneAtATime",
        traffic_shifting: "CodeDeployDefault.OneAtATime",
    },
    PlatformDefaults {
        compute_platform: "Lambda",
        cutover: "CodeDeployDefault.LambdaAllAtOnce",
        traffic_shifting: "CodeDeployDefault.LambdaCanary10Percent5Minutes",
    },
    // No traffic-controlled default is established for the Amazon ECS platform, so
    // its row names its all-at-once configuration for both answers; see the note
    // in this module's documentation.
    PlatformDefaults {
        compute_platform: "ECS",
        cutover: "CodeDeployDefault.ECSAllAtOnce",
        traffic_shifting: "CodeDeployDefault.ECSAllAtOnce",
    },
];

/// The configuration a group of an unknown compute platform defaults to.
///
/// `ComputePlatform` is a modelled enum, so a platform outside
/// [`PLATFORM_DEFAULTS`] is one this service does not know; it answers the
/// configuration the model documents as the service's default, which is what a
/// group with no platform of its own runs by.
const UNKNOWN_PLATFORM_DEFAULT: &str = "CodeDeployDefault.OneAtATime";

/// The deployment configuration CodeDeploy applies when neither the request nor
/// the deployment group names one.
///
/// `compute_platform` is the platform of the application the group belongs to and
/// `deployment_style` the style the group would HAVE (materialised by
/// [`super::effective_deployment_style`], so the server-side default decides for a
/// request that named neither member) — the two things the default depends on.
#[must_use]
pub fn default_deployment_config_for(
    compute_platform: &str,
    deployment_style: Option<&DeploymentStyle>,
) -> &'static str {
    let Some(defaults) = PLATFORM_DEFAULTS
        .iter()
        .find(|row| row.compute_platform == compute_platform)
    else {
        return UNKNOWN_PLATFORM_DEFAULT;
    };
    if routes_traffic_between_environments(deployment_style) {
        return defaults.traffic_shifting;
    }
    defaults.cutover
}

/// Whether `deployment_style` is the one in which the service moves the production
/// traffic from the original environment to the replacement one: a blue/green
/// deployment ("whether to run an in-place deployment or a blue/green
/// deployment") whose traffic is routed behind a load balancer ("whether to route
/// deployment traffic behind a load balancer",
/// `smithy_query(query="shape",shape="DeploymentStyle")`).
///
/// Both members have to say so: an in-place deployment replaces the revision on
/// the targets a client already routes to, and a blue/green deployment without
/// traffic control leaves the reroute to the client, so in neither case is there a
/// traffic shift for a configuration to pace.
fn routes_traffic_between_environments(deployment_style: Option<&DeploymentStyle>) -> bool {
    let Some(style) = deployment_style else {
        return false;
    };
    let names =
        |member: Option<&String>, value: &str| member.is_some_and(|named| named.trim() == value);
    names(style.deployment_type.as_ref(), DEPLOYMENT_TYPE_BLUE_GREEN)
        && names(
            style.deployment_option.as_ref(),
            DEPLOYMENT_OPTION_WITH_TRAFFIC_CONTROL,
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::effective_deployment_style;

    /// The style a request's two members materialise to, which is what the
    /// default is chosen from.
    fn style(
        deployment_type: Option<&str>,
        deployment_option: Option<&str>,
    ) -> Option<DeploymentStyle> {
        effective_deployment_style(Some(DeploymentStyle {
            deployment_type: deployment_type.map(str::to_string),
            deployment_option: deployment_option.map(str::to_string),
        }))
    }

    /// Every configuration the table names is one this service SHIPS, so a group
    /// that named none is never stored under a configuration no read of the
    /// configuration store resolves.
    #[test]
    fn every_defaulted_configuration_is_one_the_service_ships() {
        let shipped = |name: &str| crate::state::is_predefined_deployment_config(name);
        for row in PLATFORM_DEFAULTS {
            for name in [row.cutover, row.traffic_shifting] {
                assert!(
                    shipped(name),
                    "case {:?}: {name:?} must be a configuration the service ships",
                    row.compute_platform
                );
            }
        }
        assert!(
            shipped(UNKNOWN_PLATFORM_DEFAULT),
            "case \"an unknown platform\": {UNKNOWN_PLATFORM_DEFAULT:?} must be a configuration \
             the service ships"
        );
    }

    /// RULE: the default depends on the compute platform AND on whether the
    /// group's style has the service move the production traffic — a
    /// traffic-controlled blue/green group of a platform with a time-based
    /// configuration defaults to it, every other group to its platform's single
    /// cutover.
    ///
    /// Rows span `ComputePlatform` (Server | Lambda | ECS) and a platform outside
    /// it, crossed with every pair of `DeploymentType` and `DeploymentOption`,
    /// plus the half-stated and omitted forms the server-side default completes.
    #[test]
    fn the_default_follows_the_platform_and_the_traffic_the_style_moves() {
        let rows: Vec<(&str, &str, Option<DeploymentStyle>, &str)> = vec![
            (
                "a Server group naming no style",
                "Server",
                effective_deployment_style(None),
                "CodeDeployDefault.OneAtATime",
            ),
            (
                "a Server group that is blue/green with traffic control",
                "Server",
                style(Some("BLUE_GREEN"), Some("WITH_TRAFFIC_CONTROL")),
                "CodeDeployDefault.OneAtATime",
            ),
            (
                "a Lambda group naming no style",
                "Lambda",
                effective_deployment_style(None),
                "CodeDeployDefault.LambdaAllAtOnce",
            ),
            (
                "a Lambda group that is in-place with traffic control",
                "Lambda",
                style(Some("IN_PLACE"), Some("WITH_TRAFFIC_CONTROL")),
                "CodeDeployDefault.LambdaAllAtOnce",
            ),
            (
                "a Lambda group that is blue/green without traffic control",
                "Lambda",
                style(Some("BLUE_GREEN"), Some("WITHOUT_TRAFFIC_CONTROL")),
                "CodeDeployDefault.LambdaAllAtOnce",
            ),
            (
                "a Lambda group naming only the blue/green type",
                "Lambda",
                style(Some("BLUE_GREEN"), None),
                "CodeDeployDefault.LambdaAllAtOnce",
            ),
            (
                "a Lambda group that is blue/green with traffic control",
                "Lambda",
                style(Some("BLUE_GREEN"), Some("WITH_TRAFFIC_CONTROL")),
                "CodeDeployDefault.LambdaCanary10Percent5Minutes",
            ),
            (
                "an ECS group that is blue/green with traffic control",
                "ECS",
                style(Some("BLUE_GREEN"), Some("WITH_TRAFFIC_CONTROL")),
                "CodeDeployDefault.ECSAllAtOnce",
            ),
            (
                "an ECS group naming no style",
                "ECS",
                effective_deployment_style(None),
                "CodeDeployDefault.ECSAllAtOnce",
            ),
            (
                "a group holding no style at all",
                "Lambda",
                None,
                "CodeDeployDefault.LambdaAllAtOnce",
            ),
            (
                "a platform this service does not know",
                "Mainframe",
                style(Some("BLUE_GREEN"), Some("WITH_TRAFFIC_CONTROL")),
                "CodeDeployDefault.OneAtATime",
            ),
        ];

        for (case, compute_platform, deployment_style, expected) in rows {
            let answered =
                default_deployment_config_for(compute_platform, deployment_style.as_ref());
            assert_eq!(
                answered, expected,
                "case {case:?}: expected {expected:?}, got {answered:?}"
            );
        }
    }
}
