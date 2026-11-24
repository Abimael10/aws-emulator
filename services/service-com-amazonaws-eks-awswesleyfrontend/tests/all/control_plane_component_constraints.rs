// SPDX-License-Identifier: CC0-1.0

//! RULE: the constraints `DescribeClusterVersions` publishes beside each
//! control-plane component parameter are the PUBLISHED Amazon EKS configuration
//! limits for that parameter — the window, or the value set, the service enforces
//! when a cluster of that Kubernetes version is configured — not a window derived
//! from the upstream Kubernetes component's own default, nor from a duration a
//! member's documentation names as an example of its spelling, nor from what the
//! host's port space would allow.
//!
//! `ClusterVersionInformation$controlPlaneComponentConfig` is "The default control
//! plane component configuration and constraints for this Kubernetes version", so
//! this table is the contract a client reads to decide which parameter values it
//! may send. A published window that is WIDER than the enforced one has the client
//! accept a value the service refuses; a NARROWER one has it withhold a value the
//! service supports; and a value missing from a published `allowedValues` set tells
//! the client that value cannot be used at all.
//!
//! Each assertion below runs over a TABLE: every parameter the catalog publishes
//! (a duration, an integer, a port range, a value set), every row of the shipped
//! calendar, and the three request forms that select rows (the whole calendar, one
//! named version, the default version alone) — the published table is the same
//! whichever way a row is reached. `cluster_version_catalog.rs` covers the
//! structural invariants of the same members (a default sits inside its own
//! window); this suite covers WHICH bounds are published.

use crate::cluster_lifecycle::REGION;
use crate::wire::{call, emulator, Answer};
use smithy_server_runtime::MultiServiceRouter;

/// The published window of each DURATION parameter, as `(the member path under
/// `controlPlaneComponentConfig`, defaultValue, constraints.min,
/// constraints.max)`.
///
/// Source: the per-version control-plane configuration limits Amazon EKS
/// publishes for the API server's event TTL and for the controller manager's
/// horizontal-pod-autoscaler sync period. Both are single-unit durations
/// (`smithy_query(query="shape", shape="KubeApiServerConfigRequest")` and
/// `…shape="HorizontalPodAutoscalerControllerConfigRequest"`), and the sync
/// period is published at ONE value: its window's two ends are the same
/// duration, which is how a parameter Amazon EKS offers no alternative value for
/// is published.
const PUBLISHED_DURATIONS: &[(&[&str], &str, &str, &str)] = &[
    (&["kubeApiServerConfig", "eventTtl"], "60m", "10m", "60m"),
    (
        &[
            "kubeControllerManagerConfig",
            "horizontalPodAutoscalerControllerConfig",
            "horizontalPodAutoscalerSyncPeriod",
        ],
        "15s",
        "15s",
        "15s",
    ),
];

/// The published window of each INTEGER parameter, in the same form.
///
/// Source: the per-version control-plane configuration limits Amazon EKS
/// publishes for the pod garbage collector's terminated-pod threshold. It too is
/// published at one value, and it is NOT the range the model lets the member
/// carry (`TerminatedPodGcThresholdValue` is `range 1..1000000`,
/// `smithy_query(query="shape", shape="PodGcControllerConfigRequest")`).
const PUBLISHED_INTEGERS: &[(&[&str], i64, i64, i64)] = &[(
    &[
        "kubeControllerManagerConfig",
        "podGcControllerConfig",
        "terminatedPodGcThreshold",
    ],
    12_500,
    12_500,
    12_500,
)];

/// The published `serviceNodePortRange`: the default range a cluster runs with,
/// and the window EITHER end of a range may be chosen from.
///
/// Source: the default is the Kubernetes `--service-node-port-range` default;
/// the window is the per-version NodePort window Amazon EKS publishes, which is
/// far narrower than the unprivileged port space of a host
/// ([`HOST_PORT_WINDOW`]) because the ports below it are the ones Amazon EKS
/// keeps for the components it runs on a node.
const PUBLISHED_NODE_PORT_DEFAULT: (i64, i64) = (30_000, 32_767);
/// See [`PUBLISHED_NODE_PORT_DEFAULT`]: the window `minPort` and `maxPort` are
/// each constrained to.
const PUBLISHED_NODE_PORT_WINDOW: (i64, i64) = (10_260, 32_767);
/// The unprivileged TCP/UDP port space of a host, which is NOT what this
/// parameter's published window is: a client that took it for the published one
/// would offer a NodePort range the service refuses.
const HOST_PORT_WINDOW: (i64, i64) = (1_024, 65_535);

/// The scoring-strategy types the catalog publishes, which are the modelled
/// enum's members (`smithy_query(query="shape", shape="ScoringStrategyType")`).
const PUBLISHED_SCORING_STRATEGIES: &[&str] = &["LeastAllocated", "MostAllocated"];

/// Every resource name a cluster's `NodeResourcesFit` scoring strategy may
/// weight — the `allowedValues` published for `ResourceConstraints$name` ("The
/// allowed values for resource names").
///
/// Source: the schedulable resources Amazon EKS publishes for that plugin — the
/// two core compute resources every node reports (`ResourceWeight$name` is "The
/// name of the resource (for example, `cpu` or `memory`)") and the accelerator
/// extended resources a node advertises through its device plugin: the NVIDIA
/// GPU resource and the Neuron device and core resources of the AWS Inferentia
/// and Trainium instance families.
const PUBLISHED_SCOREABLE_RESOURCES: &[&str] = &[
    "cpu",
    "memory",
    "nvidia.com/gpu",
    "aws.amazon.com/neuron",
    "aws.amazon.com/neuroncore",
];

/// Resource names the catalog publishes NO entry for, so a client reading the
/// published set learns it cannot weight them: another vendor's GPU resource,
/// and a node resource the plugin does not score.
const UNPUBLISHED_RESOURCES: &[&str] = &["amd.com/gpu", "ephemeral-storage"];

/// The published weight range, from the model: `ResourceWeightValue` is `range
/// 1..100` (`smithy_query(query="shape", shape="ResourceWeight")`).
const PUBLISHED_WEIGHT_RANGE: (i64, i64) = (1, 100);

async fn describe_versions(router: &MultiServiceRouter, query: &str) -> Answer {
    let uri = if query.is_empty() {
        "/cluster-versions".to_string()
    } else {
        format!("/cluster-versions?{query}")
    };
    let answer = call(router, REGION, "GET", &uri, "").await;
    assert_eq!(
        answer.status, 200,
        "setup {query:?}: expected HTTP 200 from the catalog, got {} ({:?})",
        answer.status, answer.body
    );
    answer
}

fn rows(answer: &Answer) -> Vec<serde_json::Value> {
    answer.body["clusterVersions"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a clusterVersions list, got {:?}", answer.body))
        .clone()
}

/// A row's `clusterVersion`, which names the case an assertion failed on.
fn version_of(row: &serde_json::Value) -> String {
    row["clusterVersion"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a clusterVersion string, got {row:?}"))
        .to_string()
}

/// The value at a member path inside one row's component-configuration table.
fn at<'a>(config: &'a serde_json::Value, path: &[&str]) -> &'a serde_json::Value {
    let mut cursor = config;
    for step in path {
        cursor = &cursor[*step];
    }
    cursor
}

/// Every `(case, controlPlaneComponentConfig)` the catalog serves, over the three
/// request forms that select rows: the whole calendar, each single version named
/// on its own, and the default version alone. The published table does not depend
/// on which of them a client asks for.
async fn published_tables(router: &MultiServiceRouter) -> Vec<(String, serde_json::Value)> {
    let calendar = rows(&describe_versions(router, "includeAll=true").await);
    assert!(
        !calendar.is_empty(),
        "setup: the catalog must publish at least one version"
    );

    let mut tables: Vec<(String, serde_json::Value)> = Vec::new();
    for row in &calendar {
        let version = version_of(row);
        tables.push((
            format!("includeAll/{version}"),
            row["controlPlaneComponentConfig"].clone(),
        ));

        let named = rows(
            &describe_versions(
                router,
                &format!("includeAll=true&clusterVersions={version}"),
            )
            .await,
        );
        assert_eq!(
            named.len(),
            1,
            "setup clusterVersions={version}: expected the one named row, got {named:?}"
        );
        tables.push((
            format!("clusterVersions={version}"),
            named[0]["controlPlaneComponentConfig"].clone(),
        ));
    }

    let default_only = rows(&describe_versions(router, "defaultOnly=true").await);
    assert!(
        !default_only.is_empty(),
        "setup defaultOnly=true: expected the default version's row"
    );
    for row in &default_only {
        tables.push((
            format!("defaultOnly/{}", version_of(row)),
            row["controlPlaneComponentConfig"].clone(),
        ));
    }
    tables
}

#[tokio::test]
async fn every_published_row_states_the_published_duration_and_integer_windows() {
    let router = emulator();

    for (case, config) in published_tables(&router).await {
        for (path, default_value, min, max) in PUBLISHED_DURATIONS {
            let parameter = at(&config, path);
            let actual = (
                parameter["defaultValue"].as_str(),
                parameter["constraints"]["min"].as_str(),
                parameter["constraints"]["max"].as_str(),
            );
            assert_eq!(
                actual,
                (Some(*default_value), Some(*min), Some(*max)),
                "case {case}/{}: expected the published default and window \
                 ({default_value:?}, {min:?}..{max:?}), got {actual:?}",
                path.join("."),
            );
        }

        for (path, default_value, min, max) in PUBLISHED_INTEGERS {
            let parameter = at(&config, path);
            let actual = (
                parameter["defaultValue"].as_i64(),
                parameter["constraints"]["min"].as_i64(),
                parameter["constraints"]["max"].as_i64(),
            );
            assert_eq!(
                actual,
                (Some(*default_value), Some(*min), Some(*max)),
                "case {case}/{}: expected the published default and window \
                 ({default_value}, {min}..{max}), got {actual:?}",
                path.join("."),
            );
        }
    }
}

#[tokio::test]
async fn every_published_row_states_the_published_node_port_window() {
    let router = emulator();

    for (case, config) in published_tables(&router).await {
        let ports = at(&config, &["kubeApiServerConfig", "serviceNodePortRange"]);
        let default_range = (
            ports["defaultValue"]["minPort"].as_i64(),
            ports["defaultValue"]["maxPort"].as_i64(),
        );
        assert_eq!(
            default_range,
            (
                Some(PUBLISHED_NODE_PORT_DEFAULT.0),
                Some(PUBLISHED_NODE_PORT_DEFAULT.1)
            ),
            "case {case}/serviceNodePortRange: expected the published default range \
             {PUBLISHED_NODE_PORT_DEFAULT:?}, got {default_range:?}"
        );

        // BOTH ends of a range are chosen from one published window, so both
        // constraints are asserted, and each is probed at its own bounds and one
        // port past them.
        for member in ["minPort", "maxPort"] {
            let window = (
                ports["constraints"][member]["min"].as_i64(),
                ports["constraints"][member]["max"].as_i64(),
            );
            assert_eq!(
                window,
                (
                    Some(PUBLISHED_NODE_PORT_WINDOW.0),
                    Some(PUBLISHED_NODE_PORT_WINDOW.1)
                ),
                "case {case}/{member}: expected the published window \
                 {PUBLISHED_NODE_PORT_WINDOW:?}, got {window:?}"
            );
            let (floor, ceiling) = (
                window.0.unwrap_or_default(),
                window.1.unwrap_or_default(),
            );
            for (port, inside) in [
                (PUBLISHED_NODE_PORT_WINDOW.0 - 1, false),
                (PUBLISHED_NODE_PORT_WINDOW.0, true),
                (PUBLISHED_NODE_PORT_WINDOW.1, true),
                (PUBLISHED_NODE_PORT_WINDOW.1 + 1, false),
                // The host's unprivileged port space is not this window: its own
                // two ends fall OUTSIDE the published one.
                (HOST_PORT_WINDOW.0, false),
                (HOST_PORT_WINDOW.1, false),
            ] {
                assert_eq!(
                    (floor..=ceiling).contains(&port),
                    inside,
                    "case {case}/{member}: expected port {port} to be {} the published window \
                     {floor}..{ceiling}",
                    if inside { "inside" } else { "outside" }
                );
            }
        }
    }
}

#[tokio::test]
async fn every_published_row_states_every_scoreable_resource_name() {
    let router = emulator();

    for (case, config) in published_tables(&router).await {
        let scoring = at(
            &config,
            &["kubeSchedulerConfig", "nodeResourcesFit", "scoringStrategy"],
        );
        let values = |path: &[&str]| -> Vec<String> {
            at(scoring, path)
                .as_array()
                .unwrap_or_else(|| {
                    panic!("case {case}: expected allowedValues at {path:?}, got {scoring:?}")
                })
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect()
        };

        let strategies = values(&["constraints", "scoringStrategy", "allowedValues"]);
        assert_eq!(
            strategies, PUBLISHED_SCORING_STRATEGIES,
            "case {case}/scoringStrategy: expected the published strategy types, got \
             {strategies:?}"
        );

        let resources = values(&["constraints", "resources", "name", "allowedValues"]);
        assert_eq!(
            resources, PUBLISHED_SCOREABLE_RESOURCES,
            "case {case}/resources.name: expected every published scoreable resource, got \
             {resources:?}"
        );
        // Named one by one as well, so a truncated set says WHICH resource a
        // client configuring accelerator-aware scheduling would be told it cannot
        // weight.
        for published in PUBLISHED_SCOREABLE_RESOURCES {
            assert!(
                resources.iter().any(|name| name == published),
                "case {case}/resources.name: {published:?} must be published as a scoreable \
                 resource, got {resources:?}"
            );
        }
        for unpublished in UNPUBLISHED_RESOURCES {
            assert!(
                !resources.iter().any(|name| name == unpublished),
                "case {case}/resources.name: {unpublished:?} is not a published scoreable \
                 resource, got {resources:?}"
            );
        }

        // The default strategy weights a SUBSET of the published names, at weights
        // inside the published range.
        let weight_range = (
            at(scoring, &["constraints", "resources", "weight", "min"]).as_i64(),
            at(scoring, &["constraints", "resources", "weight", "max"]).as_i64(),
        );
        assert_eq!(
            weight_range,
            (
                Some(PUBLISHED_WEIGHT_RANGE.0),
                Some(PUBLISHED_WEIGHT_RANGE.1)
            ),
            "case {case}/resources.weight: expected the published weight range \
             {PUBLISHED_WEIGHT_RANGE:?}, got {weight_range:?}"
        );
        let weighted = at(scoring, &["defaultValue", "resources"])
            .as_array()
            .unwrap_or_else(|| {
                panic!("case {case}: expected the default resource weights, got {scoring:?}")
            })
            .clone();
        assert!(
            !weighted.is_empty(),
            "case {case}/defaultValue: expected at least one weighted resource, got {scoring:?}"
        );
        for resource in &weighted {
            let name = resource["name"].as_str().unwrap_or_default().to_string();
            let weight = resource["weight"].as_i64();
            assert!(
                resources.contains(&name),
                "case {case}/defaultValue/{name}: a weighted resource must be one the published \
                 set allows ({resources:?})"
            );
            assert!(
                weight.is_some_and(|weight| {
                    (PUBLISHED_WEIGHT_RANGE.0..=PUBLISHED_WEIGHT_RANGE.1).contains(&weight)
                }),
                "case {case}/defaultValue/{name}: the weight must sit inside the published range \
                 {PUBLISHED_WEIGHT_RANGE:?}, got {weight:?}"
            );
        }
    }
}
