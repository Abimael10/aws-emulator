// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the PUBLISHED BUILD HISTORY `DescribeAddonVersions`
//! reports, and for the `kubernetesVersion` filter that selects out of it, driven
//! over the wire through the same `MultiServiceRouter` the production binary
//! mounts.
//!
//! The RULE under test: **an add-on's published builds, and the Kubernetes
//! versions each of them supports, are that ADD-ON's own published history — each
//! build is offered for the Kubernetes versions Amazon EKS still offered when that
//! build was published, and an add-on whose publisher has stopped publishing is
//! still listed with the builds it has while being offered for no Kubernetes
//! version released since. The request's `kubernetesVersion` SELECTS rows out of
//! that history: it keeps the add-ons that have a build supporting the version
//! asked for, and each kept add-on's builds that support it.**
//!
//! So a catalog that derived a build per Kubernetes version for every add-on would
//! offer a client builds its publisher never published, and offer a stale listing
//! on a cluster version it was never published for — which is what a client
//! pinning, upgrading or rolling back an add-on reads the catalog to avoid.
//!
//! The rows differ along what the rule generalises over: every Kubernetes version
//! the calendar publishes (in support and out of it), and every add-on the catalog
//! publishes — the ones Amazon EKS builds, the upstream projects it curates, the
//! AWS Marketplace listings and the vendor rebuilds — including add-ons no
//! recorded request named. Nothing here writes a version, a date or a build out:
//! the calendar comes from `DescribeClusterVersions` and the builds from
//! `DescribeAddonVersions`, so these cases keep asserting the rule as the
//! published tables grow.

use std::collections::{BTreeMap, BTreeSet};

use service_com_amazonaws_eks_awswesleyfrontend::state::addon_not_supported_message;
use smithy_server_runtime::MultiServiceRouter;

use crate::addon_lifecycle::create_addon;
use crate::addon_versions::{
    active_cluster, addons, compatibilities_of, describe_addon_configuration,
    describe_addon_versions, string_member, versions_of, whole_catalog,
};
use crate::cluster_versions::version_key;
use crate::wire::{call, emulator, encode_label, error_code};

/// The region every request in this suite is signed for.
const REGION: &str = "us-west-2";

/// One published calendar row, as the catalog publishes it: the Kubernetes
/// version, the day Amazon EKS released it and the day its extended support ends.
#[derive(Debug, Clone)]
struct CalendarRow {
    /// `ClusterVersionInformation$clusterVersion`.
    cluster_version: String,
    /// `ClusterVersionInformation$releaseDate`, in epoch seconds as reported.
    released: f64,
    /// `ClusterVersionInformation$endOfExtendedSupportDate`, likewise — the day
    /// after which Amazon EKS offers the version no more.
    end_of_extended_support: f64,
}

/// The WHOLE published calendar, newest version first — out-of-support rows
/// included, because a published build still names them.
async fn published_calendar(router: &MultiServiceRouter) -> Vec<CalendarRow> {
    let answer = call(
        router,
        REGION,
        "GET",
        "/cluster-versions?maxResults=100&includeAll=true",
        "",
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "setup DescribeClusterVersions: expected HTTP 200, got {answer:?}"
    );
    let rows: Vec<CalendarRow> = answer.body["clusterVersions"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a clusterVersions list, got {:?}", answer.body))
        .iter()
        .map(|row| {
            let cluster_version = string_member(row, "clusterVersion");
            let day = |member: &str| {
                row[member].as_f64().unwrap_or_else(|| {
                    panic!("setup {cluster_version}: expected a {member}, got {row:?}")
                })
            };
            CalendarRow {
                released: day("releaseDate"),
                end_of_extended_support: day("endOfExtendedSupportDate"),
                cluster_version,
            }
        })
        .collect();
    assert!(
        rows.len() > 1,
        "setup DescribeClusterVersions: expected the published calendar, got {rows:?}"
    );
    rows
}

/// The Kubernetes minor versions one reported build names as compatible, in the
/// order reported.
fn supported_minors(build: &serde_json::Value) -> Vec<String> {
    compatibilities_of(build)
        .iter()
        .map(|entry| string_member(entry, "clusterVersion"))
        .collect()
}

/// One add-on's published build history, as [`published_history`] reports it:
/// each build's version with the Kubernetes versions it supports, newest build
/// first.
type PublishedBuilds = Vec<(String, Vec<String>)>;

/// Every add-on the catalog publishes, with its builds newest first as
/// `(the build's version, the Kubernetes versions it supports)`.
async fn published_history(router: &MultiServiceRouter) -> BTreeMap<String, PublishedBuilds> {
    addons(&whole_catalog(router).await)
        .iter()
        .map(|addon| {
            let builds = versions_of(addon)
                .iter()
                .map(|build| (string_member(build, "addonVersion"), supported_minors(build)))
                .collect();
            (string_member(addon, "addonName"), builds)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// RULE: a build is published for the Kubernetes versions Amazon EKS still
// OFFERED when the build was published — so no build names a version whose
// extended support had already ended on the day the newest version it supports
// was released, and the window of a newer build never reaches further back than
// the window of an older one.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_published_build_is_offered_for_the_versions_still_offered_when_it_was_published() {
    let router = emulator();
    let calendar = published_calendar(&router).await;
    let released: BTreeMap<&str, &CalendarRow> = calendar
        .iter()
        .map(|row| (row.cluster_version.as_str(), row))
        .collect();
    let history = published_history(&router).await;
    assert!(
        history.len() > 1,
        "setup: the catalog must publish add-ons, got {history:?}"
    );

    let mut windows: BTreeSet<Vec<String>> = BTreeSet::new();
    for (addon_name, builds) in &history {
        let mut newer_floor: Option<(u32, u32)> = None;
        for (addon_version, supported) in builds {
            let case = format!("{addon_name}/{addon_version}");
            assert!(
                !supported.is_empty(),
                "case {case}: a published build names the Kubernetes versions it supports, got \
                 {supported:?}"
            );
            let newest = &supported[0];
            let published_at = released
                .get(newest.as_str())
                .map(|row| row.released)
                .unwrap_or_else(|| panic!("case {case}: the calendar publishes {newest}"));
            for minor in supported {
                let row = released
                    .get(minor.as_str())
                    .unwrap_or_else(|| panic!("case {case}: the calendar publishes {minor}"));
                // A version nobody can still be running is not one a new build is
                // published for.
                assert!(
                    row.end_of_extended_support > published_at,
                    "case {case}: a build published alongside {newest} must not name {minor}, \
                     whose extended support ended first, got {supported:?}"
                );
                // Nor is a version released after the build was published.
                assert!(
                    row.released <= published_at,
                    "case {case}: a build published alongside {newest} must not name {minor}, \
                     released later, got {supported:?}"
                );
            }
            // The window moves FORWARD along the calendar with the add-on's own
            // line: builds are reported newest first, so each one that follows is
            // older and reaches no further forward than the build before it — the
            // oldest version in its window is at or below that build's.
            let floor = version_key(
                supported
                    .last()
                    .unwrap_or_else(|| panic!("case {case}: a build names a version")),
            );
            if let Some(newer_floor) = newer_floor {
                assert!(
                    floor <= newer_floor,
                    "case {case}: an older build's window must not start later than a newer \
                     build's, got {supported:?}"
                );
            }
            newer_floor = Some(floor);
            windows.insert(supported.clone());
        }
    }
    // The windows are not one list repeated: an add-on's builds are published over
    // the versions of their own time, so the catalog carries several.
    assert!(
        windows.len() > 1,
        "case the published windows: a catalog that names one window for every build cannot \
         answer which versions a build was published for, got {windows:?}"
    );
}

// ---------------------------------------------------------------------------
// RULE: `kubernetesVersion` SELECTS rows out of the published history — the
// add-ons that have a build supporting the version asked for, each with the
// builds of its own that support it, and each build reporting that version's
// compatibility alone.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_kubernetes_version_filter_selects_the_published_builds_for_that_version() {
    let router = emulator();
    let calendar = published_calendar(&router).await;
    let history = published_history(&router).await;

    // One row per published Kubernetes version, the out-of-support ones included:
    // a client reading the catalog for an older cluster asks about that cluster's
    // version, whatever the calendar now says about it.
    for row in &calendar {
        let minor = row.cluster_version.as_str();
        let answer =
            describe_addon_versions(&router, &format!("kubernetesVersion={minor}&maxResults=100"))
                .await;
        assert_eq!(
            answer.status, 200,
            "case kubernetesVersion {minor}: expected HTTP 200, got {answer:?}"
        );

        // What the published history says the answer must be.
        let expected: BTreeMap<String, Vec<String>> = history
            .iter()
            .filter_map(|(addon_name, builds)| {
                let selected: Vec<String> = builds
                    .iter()
                    .filter(|(_, supported)| supported.iter().any(|version| version == minor))
                    .map(|(addon_version, _)| addon_version.clone())
                    .collect();
                (!selected.is_empty()).then(|| (addon_name.clone(), selected))
            })
            .collect();

        let reported: BTreeMap<String, Vec<String>> = addons(&answer)
            .iter()
            .map(|addon| {
                let builds = versions_of(addon)
                    .iter()
                    .map(|build| {
                        let case = format!("{minor}/{}", string_member(build, "addonVersion"));
                        // A filtered listing answers about the version asked for
                        // alone, so a kept build reports that compatibility only.
                        assert_eq!(
                            supported_minors(build),
                            vec![minor.to_string()],
                            "case {case}: a filtered listing reports the asked-for version's \
                             compatibility alone, got {build:?}"
                        );
                        string_member(build, "addonVersion")
                    })
                    .collect();
                (string_member(addon, "addonName"), builds)
            })
            .collect();

        assert_eq!(
            reported, expected,
            "case kubernetesVersion {minor}: expected exactly the published builds that support \
             it, got {reported:?}"
        );
    }

    // The filter NARROWS: the newest published version is offered fewer add-ons
    // than the whole catalog publishes, because a published line that has ended
    // does not reach it.
    let newest = calendar
        .first()
        .map(|row| row.cluster_version.clone())
        .unwrap_or_else(|| panic!("setup: the calendar publishes a version"));
    let offered = addons(
        &describe_addon_versions(
            &router,
            &format!("kubernetesVersion={newest}&maxResults=100"),
        )
        .await,
    )
    .len();
    assert!(
        offered < history.len(),
        "case kubernetesVersion {newest}: the catalog publishes {} add-ons, of which an ended \
         published line reaches no newer version, got {offered} offered",
        history.len()
    );
}

// ---------------------------------------------------------------------------
// RULE: an add-on whose publisher has stopped publishing keeps the builds it
// published — it is still part of the catalog, with its own window — and is
// offered for no Kubernetes version released since, so an install of it on a
// cluster of such a version is refused the way an unpublished name is.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_addon_whose_published_line_ended_is_listed_but_not_offered_for_a_newer_version() {
    let router = emulator();
    let calendar = published_calendar(&router).await;
    let newest = calendar
        .first()
        .map(|row| row.cluster_version.clone())
        .unwrap_or_else(|| panic!("setup: the calendar publishes a version"));
    let history = published_history(&router).await;

    // The add-ons whose published line stops before the newest published
    // Kubernetes version: the class this case is about, read out of the catalog
    // rather than named here.
    let ended: Vec<(&String, &PublishedBuilds)> = history
        .iter()
        .filter(|(_, builds)| {
            builds
                .iter()
                .all(|(_, supported)| !supported.contains(&newest))
        })
        .collect();
    assert!(
        !ended.is_empty(),
        "setup: the catalog must publish an add-on whose published line has ended, got \
         {history:?}"
    );

    active_cluster(&router, "ended-lines", &newest).await;
    for (addon_name, builds) in &ended {
        let case = addon_name.as_str();
        // It is still published, with its own builds, and the configuration of
        // each of them is still readable: a client that installed it earlier can
        // still read what it installed.
        let named = describe_addon_versions(
            &router,
            &format!("addonName={}", encode_label(addon_name)),
        )
        .await;
        let listed = addons(&named);
        assert_eq!(
            listed.len(),
            1,
            "case {case}: an add-on whose line has ended is still published, got {:?}",
            named.body["addons"]
        );
        let reported: Vec<String> = versions_of(&listed[0])
            .iter()
            .map(|build| string_member(build, "addonVersion"))
            .collect();
        let published: Vec<String> = builds
            .iter()
            .map(|(addon_version, _)| addon_version.clone())
            .collect();
        assert_eq!(
            reported, published,
            "case {case}: the builds it published are the ones reported, got {reported:?}"
        );
        let configuration = describe_addon_configuration(&router, addon_name, &published[0]).await;
        assert_eq!(
            configuration.status, 200,
            "case {case}: the configuration of a published build stays readable, got \
             {configuration:?}"
        );

        // And it is offered to a cluster of the newest version under no name at
        // all, which is the sentence an unpublished name is refused with.
        let refused = create_addon(
            &router,
            "ended-lines",
            &serde_json::json!({ "addonName": addon_name }),
        )
        .await;
        assert_eq!(
            refused.status, 400,
            "case {case}: an add-on with no build for the cluster's version is refused, got \
             {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "InvalidParameterException",
            "case {case}: expected InvalidParameterException, got {refused:?}"
        );
        assert_eq!(
            refused.body["message"],
            serde_json::json!(addon_not_supported_message(addon_name, &newest)),
            "case {case}: expected the unsupported-add-on sentence, got {:?}",
            refused.body["message"]
        );

        // The versions it WAS published for still offer it, so the entry a client
        // reads for an older cluster is the published one.
        for minor in &builds[0].1 {
            let offered = describe_addon_versions(
                &router,
                &format!(
                    "addonName={}&kubernetesVersion={}",
                    encode_label(addon_name),
                    encode_label(minor)
                ),
            )
            .await;
            assert_eq!(
                addons(&offered).len(),
                1,
                "case {case} on {minor}: a version the add-on was published for still offers \
                 it, got {:?}",
                offered.body["addons"]
            );
        }
    }
}
