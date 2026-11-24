// SPDX-License-Identifier: CC0-1.0

//! The control-plane component configuration catalog: the per-Kubernetes-version
//! defaults a cluster that overrides nothing runs with, and the CONSTRAINTS
//! Amazon EKS publishes beside each of them.
//!
//! `ClusterVersionInformation$controlPlaneComponentConfig` is "The default control
//! plane component configuration and constraints for this Kubernetes version", and
//! a constraint in it is PUBLISHED reference data: the window — or the value set —
//! Amazon EKS enforces for that parameter on `CreateCluster` and
//! `UpdateClusterConfig`. It is therefore carried here as the published table,
//! parameter by parameter, and never derived from the upstream Kubernetes
//! component's own default or from an example a member's documentation happens to
//! name: a window reached that way is wider or narrower than the one the service
//! enforces, and a caller validating its configuration against it offers values
//! the service refuses, or withholds values it accepts.
//!
//! The defaults live here beside the constraints because a default is published
//! twice — in this catalog, and as what a cluster that overrode nothing reports
//! ([`reported_kube_api_server_config`] and the two beside it) — so the catalog
//! and the cluster slice read ONE constant each.

use super::*;

/// The default retention of Kubernetes events on a cluster's API server.
///
/// `KubeApiServerConfigRequest$eventTtl` is "The duration that Kubernetes events
/// are retained. Valid values are single-unit durations such as `30m` or `1h`",
/// and one hour is the kube-apiserver `--event-ttl` default, which Amazon EKS
/// publishes in the single-unit minutes spelling a cluster payload carries.
///
/// It is BOTH what the version catalog publishes as that parameter's default and
/// what a cluster that overrode nothing reports for it
/// ([`reported_kube_api_server_config`]), which is why there is one constant.
pub(super) const DEFAULT_EVENT_TTL: &str = "60m";
/// The shortest event retention Amazon EKS lets a cluster of a published
/// Kubernetes version be configured with — the floor of the window
/// `DurationConstraints` states beside [`DEFAULT_EVENT_TTL`].
///
/// Source: the per-version control-plane configuration limits Amazon EKS
/// publishes for the API server's event TTL. The bound is NOT the shortest
/// duration the member's documentation names as an example, and not the
/// kube-apiserver default either: the published floor is its own value, and a
/// client reads this table precisely to learn it.
pub(super) const EVENT_TTL_MIN: &str = "10m";
/// The longest event retention the same published window allows, which is the
/// retention a cluster runs with unasked ([`DEFAULT_EVENT_TTL`]): Amazon EKS
/// publishes no longer retention for these versions, so a caller may shorten the
/// retention from the default and not lengthen it.
pub(super) const EVENT_TTL_MAX: &str = DEFAULT_EVENT_TTL;

/// The default `NodePort` range, which is the Kubernetes default
/// (`--service-node-port-range=30000-32767`) that
/// `KubeApiServerConfigRequest$serviceNodePortRange` ("The port range for
/// NodePort services") overrides.
pub(super) const DEFAULT_SERVICE_NODE_PORT_MIN: i32 = 30_000;
/// See [`DEFAULT_SERVICE_NODE_PORT_MIN`].
pub(super) const DEFAULT_SERVICE_NODE_PORT_MAX: i32 = 32_767;

/// The default `NodePort` range as the modelled shape — the value both the
/// version catalog publishes and a cluster that overrode nothing reports (see
/// [`reported_kube_api_server_config`]).
pub(super) fn default_service_node_port_range() -> ServiceNodePortRange {
    ServiceNodePortRange {
        min_port: DEFAULT_SERVICE_NODE_PORT_MIN,
        max_port: DEFAULT_SERVICE_NODE_PORT_MAX,
    }
}
/// The lowest port number either end of a cluster's `NodePort` range may be set
/// to — the floor `PortRangeConstraints` states for `minPort` and for `maxPort`
/// alike, since both ends are chosen from one published window.
///
/// Source: the per-version control-plane configuration limits Amazon EKS
/// publishes for the service NodePort range. The window is MUCH narrower than
/// the unprivileged TCP/UDP port range a Linux host would allow, because the
/// ports under it are the ones Amazon EKS keeps for the components it runs on a
/// node; a client that took the host's range for the published one would offer a
/// range the service refuses.
pub(super) const SERVICE_NODE_PORT_FLOOR: i32 = 10_260;
/// The highest port number either end of a `NodePort` range may be set to, from
/// the same published window: the top of the Kubernetes NodePort space, which is
/// also where the default range ends ([`DEFAULT_SERVICE_NODE_PORT_MAX`]).
pub(super) const SERVICE_NODE_PORT_CEILING: i32 = 32_767;

/// The default number of terminated pods that may exist before the controller
/// manager starts deleting them — the kube-controller-manager
/// `--terminated-pod-gc-threshold` default, which
/// `PodGcControllerConfigRequest$terminatedPodGcThreshold` overrides.
pub(super) const DEFAULT_TERMINATED_POD_GC_THRESHOLD: i32 = 12_500;
/// The floor of the window the catalog publishes for
/// `terminatedPodGcThreshold`, which is that parameter's own default: Amazon EKS
/// publishes no OTHER threshold for a Kubernetes version, so the window it
/// states beside the default is the default alone.
///
/// Source: the per-version control-plane configuration limits Amazon EKS
/// publishes for the pod garbage collector. The published window is not the
/// shape's own bound either — `TerminatedPodGcThresholdValue` is `range
/// 1..1000000` (`smithy_query query="shape"
/// shape="PodGcControllerConfigRequest"`), which is what the member may CARRY on
/// the wire — nor a window derived from the component's upstream default.
pub(super) const TERMINATED_POD_GC_THRESHOLD_MIN: i32 = DEFAULT_TERMINATED_POD_GC_THRESHOLD;
/// See [`TERMINATED_POD_GC_THRESHOLD_MIN`]: the published window's two ends are
/// the same value, so there is one constant behind both.
pub(super) const TERMINATED_POD_GC_THRESHOLD_MAX: i32 = DEFAULT_TERMINATED_POD_GC_THRESHOLD;

/// The default interval between horizontal-pod-autoscaler syncs — the
/// kube-controller-manager `--horizontal-pod-autoscaler-sync-period` default,
/// spelled as the single-unit duration
/// `HorizontalPodAutoscalerControllerConfigRequest$horizontalPodAutoscalerSyncPeriod`
/// documents ("Valid values are single-unit durations such as `15s` or `1m`").
pub(super) const DEFAULT_HPA_SYNC_PERIOD: &str = "15s";
/// The floor of the window the catalog publishes for
/// `horizontalPodAutoscalerSyncPeriod`, which is that parameter's own default.
///
/// Source: the per-version control-plane configuration limits Amazon EKS
/// publishes for the horizontal pod autoscaler. As with the pod garbage
/// collector's threshold, the published window's ends are both the default:
/// the longer interval the member's documentation names as an example is an
/// example of the member's SPELLING, not a published bound.
pub(super) const HPA_SYNC_PERIOD_MIN: &str = DEFAULT_HPA_SYNC_PERIOD;
/// See [`HPA_SYNC_PERIOD_MIN`].
pub(super) const HPA_SYNC_PERIOD_MAX: &str = DEFAULT_HPA_SYNC_PERIOD;

/// The `ScoringStrategyType` members (`smithy_query query="shape"
/// shape="ScoringStrategyType"`), the first of which is the Kubernetes
/// `NodeResourcesFit` default.
pub(super) const SCORING_STRATEGY_TYPES: &[&str] = &["LeastAllocated", "MostAllocated"];
/// The resources the DEFAULT `NodeResourcesFit` scoring strategy weights, which
/// are the two the Kubernetes scheduler weights by default (each at weight
/// [`DEFAULT_RESOURCE_WEIGHT`]).
///
/// A subset of [`SCOREABLE_RESOURCES`]: what a cluster scores unasked is not
/// what it MAY be asked to score.
pub(super) const SCORED_RESOURCES: &[&str] = &["cpu", "memory"];
/// Every resource name a cluster's scoring strategy may weight — the
/// `allowedValues` the catalog publishes for `ResourceConstraints$name` ("The
/// allowed values for resource names").
///
/// Source: the schedulable resources Amazon EKS publishes for the
/// `NodeResourcesFit` plugin — the two core compute resources every node reports
/// (`ResourceWeight$name` is "The name of the resource (for example, `cpu` or
/// `memory`)"), and the ACCELERATOR extended resources a node advertises through
/// its device plugin: the NVIDIA GPU resource, and the Neuron device and core
/// resources of the AWS Inferentia and Trainium instance families. An
/// accelerator resource left out of this list tells a client configuring
/// GPU-aware or Neuron-aware scheduling that it cannot be scored at all, so the
/// published set names every one of them. The list is within
/// `AllowedValuesConstraint$allowedValues`' own `length 0..20`.
pub(super) const SCOREABLE_RESOURCES: &[&str] = &[
    "cpu",
    "memory",
    "nvidia.com/gpu",
    "aws.amazon.com/neuron",
    "aws.amazon.com/neuroncore",
];
/// See [`SCORED_RESOURCES`].
pub(super) const DEFAULT_RESOURCE_WEIGHT: i32 = 1;
/// The bounds on a resource weight. PUBLISHED by the model: `ResourceWeightValue`
/// is `range 1..100` (`smithy_query query="shape" shape="ResourceWeight"`).
pub(super) const RESOURCE_WEIGHT_MIN: i32 = 1;
/// See [`RESOURCE_WEIGHT_MIN`].
pub(super) const RESOURCE_WEIGHT_MAX: i32 = 100;

/// The default `NodeResourcesFit` scoring strategy as the modelled shape — the
/// value both the version catalog publishes and a cluster that overrode nothing
/// reports (see [`reported_kube_scheduler_config`]): the first
/// `ScoringStrategyType`, scoring the two resources the Kubernetes scheduler
/// weights by default.
pub(super) fn default_scoring_strategy() -> ScoringStrategy {
    ScoringStrategy {
        r#type: SCORING_STRATEGY_TYPES
            .first()
            .map(|strategy| (*strategy).to_string()),
        resources: Some(
            SCORED_RESOURCES
                .iter()
                .map(|name| ResourceWeight {
                    name: Some((*name).to_string()),
                    weight: Some(DEFAULT_RESOURCE_WEIGHT),
                })
                .collect(),
        ),
    }
}

/// The control-plane component configuration defaults and constraints this
/// service publishes.
///
/// `ClusterVersionInformation$controlPlaneComponentConfig` is "The default control
/// plane component configuration and constraints for this Kubernetes version":
/// for each component parameter a cluster may override — on `CreateCluster`'s
/// `kubeApiServerConfig`, `kubeSchedulerConfig` and
/// `kubeControllerManagerConfig` — the value a cluster that overrides nothing
/// runs with, and the window or value set an override may be chosen from. Every
/// one of those windows is the PUBLISHED one (see the constants above), which is
/// what makes this table the contract a client may validate a configuration
/// against before it sends it.
pub(super) fn control_plane_component_config() -> ControlPlaneConfigInfo {
    ControlPlaneConfigInfo {
        kube_api_server_config: Some(KubeApiServerVersionConfig {
            event_ttl: Some(DurationParameterConfig {
                default_value: Some(DEFAULT_EVENT_TTL.to_string()),
                constraints: Some(DurationConstraints {
                    min: Some(EVENT_TTL_MIN.to_string()),
                    max: Some(EVENT_TTL_MAX.to_string()),
                }),
            }),
            service_node_port_range: Some(PortRangeParameterConfig {
                default_value: Some(default_service_node_port_range()),
                constraints: Some(PortRangeConstraints {
                    min_port: Some(IntegerRangeConstraint {
                        min: Some(SERVICE_NODE_PORT_FLOOR),
                        max: Some(SERVICE_NODE_PORT_CEILING),
                    }),
                    max_port: Some(IntegerRangeConstraint {
                        min: Some(SERVICE_NODE_PORT_FLOOR),
                        max: Some(SERVICE_NODE_PORT_CEILING),
                    }),
                }),
            }),
        }),
        kube_scheduler_config: Some(KubeSchedulerVersionConfig {
            node_resources_fit: Some(NodeResourcesFitVersionConfig {
                scoring_strategy: Some(ScoringStrategyConfig {
                    default_value: Some(default_scoring_strategy()),
                    constraints: Some(ScoringStrategyConstraints {
                        scoring_strategy: Some(AllowedValuesConstraint {
                            allowed_values: Some(
                                SCORING_STRATEGY_TYPES
                                    .iter()
                                    .map(|strategy| (*strategy).to_string())
                                    .collect(),
                            ),
                        }),
                        resources: Some(ResourceConstraints {
                            // Every resource name a scoring strategy MAY weight,
                            // which is a wider set than the one the default
                            // strategy weights.
                            name: Some(AllowedValuesConstraint {
                                allowed_values: Some(
                                    SCOREABLE_RESOURCES
                                        .iter()
                                        .map(|name| (*name).to_string())
                                        .collect(),
                                ),
                            }),
                            weight: Some(IntegerRangeConstraint {
                                min: Some(RESOURCE_WEIGHT_MIN),
                                max: Some(RESOURCE_WEIGHT_MAX),
                            }),
                        }),
                    }),
                }),
            }),
        }),
        kube_controller_manager_config: Some(KubeControllerManagerVersionConfig {
            pod_gc_controller_config: Some(PodGcControllerVersionConfig {
                terminated_pod_gc_threshold: Some(IntegerParameterConfig {
                    default_value: Some(DEFAULT_TERMINATED_POD_GC_THRESHOLD),
                    constraints: Some(IntegerConstraints {
                        min: Some(TERMINATED_POD_GC_THRESHOLD_MIN),
                        max: Some(TERMINATED_POD_GC_THRESHOLD_MAX),
                    }),
                }),
            }),
            horizontal_pod_autoscaler_controller_config: Some(
                HorizontalPodAutoscalerControllerVersionConfig {
                    horizontal_pod_autoscaler_sync_period: Some(DurationParameterConfig {
                        default_value: Some(DEFAULT_HPA_SYNC_PERIOD.to_string()),
                        constraints: Some(DurationConstraints {
                            min: Some(HPA_SYNC_PERIOD_MIN.to_string()),
                            max: Some(HPA_SYNC_PERIOD_MAX.to_string()),
                        }),
                    }),
                },
            ),
        }),
    }
}
