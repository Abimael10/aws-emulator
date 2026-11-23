// SPDX-License-Identifier: CC0-1.0

//! The provisioned control-plane scaling tiers the Kubernetes version catalog
//! publishes, and the control-plane component configuration each tier reports.

use super::*;

/// The provisioned control-plane scaling tiers this service publishes, as
/// `(tierName, apiRequestConcurrency, podSchedulingRatePerSecond,
/// clusterDatabaseSizeGb)`.
///
/// `ClusterVersionInformation$controlPlaneScalingTiers` is "The available
/// provisioned control plane scaling tiers and their capabilities for this
/// Kubernetes version", bounded by the model at 0..10 entries. The tier NAMES are
/// the members of `ProvisionedControlPlaneTier`
/// (`smithy_query query="shape" shape="ProvisionedControlPlaneTier"`:
/// `standard | tier-xl | tier-2xl | tier-4xl | tier-8xl`) — the same enum
/// `ControlPlaneScalingConfig$tier` accepts, so a caller can feed a tier name
/// this catalog publishes straight back into `CreateCluster`. `standard` is the
/// tier a cluster runs on when it asks for NO provisioned control plane, so it is
/// not one of the provisioned tiers published here; the four provisioned members
/// are published in the enum's own order, which is ascending capability.
///
/// The capabilities are the EKS provisioned-control-plane tier table: `tier-xl`
/// serves 2,000 concurrent API requests, schedules 167 pods a second and holds a
/// 16 GB cluster database, and each tier above it doubles all three. A caller
/// sizes a control plane from these three numbers, which is why a tier is
/// published with them rather than as a bare name.
pub(super) const CONTROL_PLANE_SCALING_TIERS: &[(&str, i32, i32, i32)] = &[
    ("tier-xl", 2_000, 167, 16),
    ("tier-2xl", 4_000, 334, 32),
    ("tier-4xl", 8_000, 668, 64),
    ("tier-8xl", 16_000, 1_336, 128),
];

/// The provisioned control-plane scaling tiers, as the modelled shape.
///
/// Each tier reports the component configuration a control plane on it runs with
/// as its `controlPlaneComponentConfigOverrides` ("The control plane component
/// configuration overrides specific to this scaling tier"). This service
/// publishes ONE set of component defaults and constraints
/// ([`control_plane_component_config`]), so every tier reports that same set
/// rather than a per-tier value this emulator would have to invent: a caller
/// reading a tier learns the window it may configure a cluster on that tier in
/// without having to merge the tier against the version.
pub(super) fn control_plane_scaling_tiers() -> Vec<ControlPlaneScalingTierInfo> {
    CONTROL_PLANE_SCALING_TIERS
        .iter()
        .map(
            |(tier_name, api_request_concurrency, pod_scheduling_rate, database_size_gb)| {
                ControlPlaneScalingTierInfo {
                    tier_name: Some((*tier_name).to_string()),
                    api_request_concurrency: Some(*api_request_concurrency),
                    pod_scheduling_rate_per_second: Some(*pod_scheduling_rate),
                    cluster_database_size_gb: Some(*database_size_gb),
                    control_plane_component_config_overrides: Some(
                        control_plane_component_config(),
                    ),
                }
            },
        )
        .collect()
}
