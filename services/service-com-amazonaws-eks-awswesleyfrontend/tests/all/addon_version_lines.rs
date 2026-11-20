// SPDX-License-Identifier: CC0-1.0

//! The build LINE the add-on catalogue publishes for the Kubernetes versions this
//! service's clusters run.
//!
//! The RULE under test: **the builds `DescribeAddonVersions` publishes for an add-on
//! are the releases that add-on's own project has published over the Kubernetes
//! versions Amazon EKS offers it on — so for every version a cluster of this service
//! can run, the add-on has an installable build, the catalogue's own default for that
//! version is one `CreateAddon` accepts, and the newest release line the project has
//! reached is offered on the versions it was released alongside rather than a
//! superseded one.** A catalogue whose line stops short of a cluster version offers
//! that cluster a line its own project has moved past, and a client discovering a
//! compatible version — the documented way to install a managed add-on — cannot
//! install what the cluster actually runs.
//!
//! The rows differ along what the rule generalises over: the add-on (one per owner
//! the catalogue publishes), and the Kubernetes version the listing is filtered to
//! (every version a cluster can be created at, read out of `DescribeClusterVersions`
//! rather than written here).
//!
//! One row is the Amazon CloudWatch Observability add-on's current major line, which
//! is the line the Container Insights install runs on: it is named because the
//! catalogue's data is what carries it, and the version it is offered for is the one
//! a cluster created without a `version` runs.

use std::collections::BTreeSet;

use service_com_amazonaws_eks_awswesleyfrontend::state::DEFAULT_KUBERNETES_VERSION;
use smithy_server_runtime::MultiServiceRouter;

use crate::addon_versions::{addons, describe_addon_versions, string_member};
use crate::cluster_lifecycle::REGION;
use crate::wire::{call, emulator, encode_label};

/// The add-ons the rows read, one per owner the catalogue publishes: an add-on Amazon
/// Web Services builds, an upstream project Amazon EKS curates, and the observability
/// add-on the Container Insights install needs.
const ADDONS: &[&str] = &[
    "amazon-cloudwatch-observability",
    "vpc-cni",
    "cert-manager",
];

/// The MAJOR version of the Amazon CloudWatch Observability line the Container
/// Insights install runs on.
///
/// SOURCE: the add-on's own published release line, which the catalogue carries
/// (`state::addon_catalog`'s `ADDON_CATALOG` row for the add-on). The assertion below
/// is that a cluster of the version this service creates by default can install a
/// build OF THAT LINE — not that it is the only one offered.
const CONTAINER_INSIGHTS_MAJOR: u32 = 5;

/// The major version a published `vX.Y.Z-eksbuild.N` names.
fn major_of(addon_version: &str) -> u32 {
    addon_version
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|major| major.parse().ok())
        .unwrap_or_else(|| panic!("a published build is versioned, got {addon_version:?}"))
}

/// The Kubernetes versions a cluster can be CREATED at, read out of the published
/// calendar: the versions whose support — standard or extended — has not ended.
///
/// A version whose extended support has ended is one a cluster cannot be created at
/// any more, so the add-on catalogue publishing no build for it is not a gap a client
/// can reach.
async fn creatable_versions(router: &MultiServiceRouter) -> Vec<String> {
    let answer = call(router, REGION, "GET", "/cluster-versions", "").await;
    assert_eq!(
        answer.status, 200,
        "setup: the version calendar must be readable, got {answer:?}"
    );
    let versions: Vec<String> = answer.body["clusterVersions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|version| {
            version["versionStatus"]
                .as_str()
                .is_some_and(|status| status != "UNSUPPORTED")
        })
        .filter_map(|version| {
            version["clusterVersion"]
                .as_str()
                .map(str::to_string)
        })
        .collect();
    assert!(
        versions.contains(&DEFAULT_KUBERNETES_VERSION.to_string()),
        "setup: the calendar must offer the version a create without one lands on, got \
         {versions:?}"
    );
    versions
}

/// The builds the catalogue offers for `addon_name` on `kubernetes_version`, with the
/// one it marks as that version's DEFAULT.
async fn offered(
    router: &MultiServiceRouter,
    addon_name: &str,
    kubernetes_version: &str,
) -> (Vec<String>, Vec<String>) {
    let answer = describe_addon_versions(
        router,
        &format!(
            "addonName={}&kubernetesVersion={}",
            encode_label(addon_name),
            encode_label(kubernetes_version)
        ),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {addon_name:?} on {kubernetes_version:?}: the catalogue must answer, got {answer:?}"
    );
    let reported = addons(&answer);
    assert_eq!(
        reported.len(),
        1,
        "case {addon_name:?} on {kubernetes_version:?}: one add-on was asked for, got {reported:?}"
    );
    let mut versions = Vec::new();
    let mut defaults = Vec::new();
    for build in reported[0]["addonVersions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let version = string_member(&build, "addonVersion");
        let compatibilities = build["compatibilities"].as_array().cloned().unwrap_or_default();
        assert!(
            compatibilities.iter().all(|entry| {
                entry["clusterVersion"] == serde_json::json!(kubernetes_version)
            }),
            "case {addon_name:?} on {kubernetes_version:?}: a filtered listing answers about \
             that version alone, got {build}"
        );
        if compatibilities
            .iter()
            .any(|entry| entry["defaultVersion"] == serde_json::json!(true))
        {
            defaults.push(version.clone());
        }
        versions.push(version);
    }
    (versions, defaults)
}

#[tokio::test]
async fn every_cluster_version_can_install_the_add_ons_the_catalogue_publishes() {
    let router = emulator();
    let versions = creatable_versions(&router).await;

    for addon_name in ADDONS {
        let mut newest_overall: BTreeSet<String> = BTreeSet::new();
        for kubernetes_version in &versions {
            let case = format!("{addon_name} on Kubernetes {kubernetes_version}");
            let (offered_versions, defaults) =
                offered(&router, addon_name, kubernetes_version).await;
            // A VERSION A CLUSTER CAN RUN HAS A BUILD: a cluster of it can install
            // the add-on at all.
            assert!(
                !offered_versions.is_empty(),
                "case {case:?}: the catalogue must offer a build, got {offered_versions:?}"
            );
            // EXACTLY ONE of them is the default for that version, which is the build
            // an install that names no version resolves to.
            assert_eq!(
                defaults.len(),
                1,
                "case {case:?}: one build is the default for a version, got {defaults:?}"
            );
            newest_overall.insert(defaults[0].clone());
        }
        // The default build of the NEWEST cluster version is the newest build the
        // catalogue publishes for the add-on at all: a cluster on a current version is
        // never limited to a line the catalogue has moved past.
        let unfiltered = describe_addon_versions(
            &router,
            &format!("addonName={}", encode_label(addon_name)),
        )
        .await;
        let published: Vec<String> = addons(&unfiltered)[0]["addonVersions"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|build| string_member(build, "addonVersion"))
            .collect();
        let newest = published
            .first()
            .unwrap_or_else(|| panic!("case {addon_name:?}: the add-on publishes builds"));
        assert!(
            newest_overall.contains(newest),
            "case {addon_name:?}: the newest build the catalogue publishes must be the default \
             of some cluster version, got {newest_overall:?} against {published:?}"
        );
    }

    // THE CONTAINER INSIGHTS LINE: a cluster of the version this service creates by
    // default can install a build of the Amazon CloudWatch Observability line that
    // install runs on, and the build it names is one `CreateAddon` accepts.
    let case = "the Container Insights line on the default cluster version";
    let (offered_versions, _) = offered(
        &router,
        "amazon-cloudwatch-observability",
        DEFAULT_KUBERNETES_VERSION,
    )
    .await;
    let classic = offered_versions
        .iter()
        .find(|version| major_of(version) == CONTAINER_INSIGHTS_MAJOR)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a v{CONTAINER_INSIGHTS_MAJOR}.x build for Kubernetes \
                 {DEFAULT_KUBERNETES_VERSION}, got {offered_versions:?}"
            )
        })
        .clone();

    // A VERSION THE CATALOGUE OFFERS IS ONE AN INSTALL TAKES: the cluster runs the
    // default version, so the build discovered above installs on it.
    let created = call(
        &router,
        REGION,
        "POST",
        "/clusters",
        &serde_json::json!({
            "name": "insights-host",
            "roleArn": format!(
                "arn:aws:iam::{}:role/eksClusterRole",
                engine::emulator_account_id()
            ),
            "resourcesVpcConfig": { "subnetIds": ["subnet-1111111111111111a", "subnet-1111111111111111b"] },
        })
        .to_string(),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the cluster must be created, got {created:?}"
    );
    assert_eq!(
        created.body["cluster"]["version"],
        serde_json::json!(DEFAULT_KUBERNETES_VERSION),
        "case {case:?}: a create naming no version lands on the default one, got {created:?}"
    );
    let installed = call(
        &router,
        REGION,
        "POST",
        "/clusters/insights-host/addons",
        &serde_json::json!({
            "addonName": "amazon-cloudwatch-observability",
            "addonVersion": classic,
        })
        .to_string(),
    )
    .await;
    assert_eq!(
        installed.status, 200,
        "case {case:?}: the version the catalogue offered must install, got {installed:?}"
    );
    assert_eq!(
        installed.body["addon"]["addonVersion"],
        serde_json::json!(classic),
        "case {case:?}: the install reports the build it was asked for, got {installed:?}"
    );
}
