// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the DEFAULT BUILD of a published add-on — the build
//! `DescribeAddonVersions` flags `defaultVersion` and the one `CreateAddon`
//! installs when the caller names no `addonVersion` — driven over the wire
//! through the same `MultiServiceRouter` the production binary mounts.
//!
//! The RULE under test: **`AddonInfo$addonVersions` is the add-on's own published
//! build table, one `AddonVersionInfo` per published build, and
//! `Compatibility$defaultVersion` marks, on each compatibility row, whether that
//! build is the DEFAULT build for the Kubernetes version the row names — the
//! newest published build of the add-on that supports that version. So every
//! Kubernetes version an add-on publishes a build for has exactly ONE default
//! build, an add-on whose publisher stopped publishing before the version Amazon
//! EKS creates clusters at by default still has a default build for the versions
//! its last builds support, and an add-on whose newest build dropped an older
//! version defaults, on that version, to the newest build that kept it.**
//!
//! So the flag answers "which build would an install on a cluster of THIS version
//! be given" — the question `CreateAddon` resolves when the caller names no
//! `addonVersion` — and a listing filtered to one cluster version reports the
//! same flag for that version as the unfiltered listing does.
//!
//! The rows differ along what the rule generalises over: every add-on the catalog
//! publishes (the ones Amazon EKS builds, the upstream projects it curates, the
//! AWS Marketplace listings and the vendor rebuilds), add-ons with one published
//! build and with several, add-ons whose published line reaches the default
//! Kubernetes version and add-ons whose line ended before it, and every
//! Kubernetes version the published calendar carries. Nothing here writes a
//! version, a build or a date out: the calendar comes from
//! `DescribeClusterVersions` and the builds from `DescribeAddonVersions`, so these
//! cases keep asserting the rule as the published tables grow.

use std::collections::{BTreeMap, BTreeSet};

use smithy_server_runtime::MultiServiceRouter;

use crate::addon_versions::{
    addons, compatibilities_of, describe_addon_versions, string_member, versions_of, whole_catalog,
};
use crate::wire::{call, emulator, encode_label};

/// The region every request in this suite is signed for.
const REGION: &str = "us-west-2";

/// The Kubernetes version Amazon EKS creates a cluster at when the caller names
/// none, read out of the published calendar rather than written here: the row
/// `DescribeClusterVersions` flags `defaultVersion`.
async fn default_cluster_version(router: &MultiServiceRouter) -> String {
    let answer = call(router, REGION, "GET", "/cluster-versions?defaultOnly=true", "").await;
    assert_eq!(
        answer.status, 200,
        "setup DescribeClusterVersions: expected HTTP 200, got {answer:?}"
    );
    let rows = answer.body["clusterVersions"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a clusterVersions list, got {:?}", answer.body))
        .clone();
    assert_eq!(
        rows.len(),
        1,
        "setup defaultOnly: expected exactly the default row, got {rows:?}"
    );
    string_member(&rows[0], "clusterVersion")
}

/// Every Kubernetes version the published calendar carries, newest first — the
/// out-of-support rows included, since a published build still names them.
async fn published_calendar(router: &MultiServiceRouter) -> Vec<String> {
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
    let versions: Vec<String> = answer.body["clusterVersions"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a clusterVersions list, got {:?}", answer.body))
        .iter()
        .map(|row| string_member(row, "clusterVersion"))
        .collect();
    assert!(
        versions.len() > 1,
        "setup DescribeClusterVersions: expected the published calendar, got {versions:?}"
    );
    versions
}

/// The Kubernetes versions one reported build names as compatible, in the order
/// reported.
fn supported_minors(build: &serde_json::Value) -> Vec<String> {
    compatibilities_of(build)
        .iter()
        .map(|entry| string_member(entry, "clusterVersion"))
        .collect()
}

/// The builds of one reported add-on that are flagged `defaultVersion` for one
/// Kubernetes version — the version a row names, so a build is counted only for
/// the rows it carries.
fn flagged_for(addon: &serde_json::Value, minor: &str) -> Vec<String> {
    versions_of(addon)
        .iter()
        .filter(|build| {
            compatibilities_of(build).iter().any(|entry| {
                string_member(entry, "clusterVersion") == minor
                    && entry["defaultVersion"].as_bool() == Some(true)
            })
        })
        .map(|build| string_member(build, "addonVersion"))
        .collect()
}

// ---------------------------------------------------------------------------
// RULE: for every Kubernetes version an add-on publishes a build for, exactly ONE
// of its builds is flagged on that version's row — the newest published build
// that supports it — and no row of any other build is, so an add-on whose
// published line ended before the default Kubernetes version still has a default
// build for the versions it does publish builds for.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_default_build_for_a_cluster_version_is_the_newest_published_for_it() {
    let router = emulator();
    let default_version = default_cluster_version(&router).await;
    let catalog = addons(&whole_catalog(&router).await);
    assert!(
        catalog.len() > 1,
        "setup: the catalog must publish add-ons, got {catalog:?}"
    );

    // Both classes must be exercised, or the case says nothing about either: the
    // add-ons still offered for the default Kubernetes version, and the ones
    // whose published line ended before it — which have a default build of their
    // own for the versions they do publish builds for.
    let mut offered_for_default = 0_usize;
    let mut ended_before_default = 0_usize;
    for addon in &catalog {
        let addon_name = string_member(addon, "addonName");
        let builds = versions_of(addon);
        assert!(
            !builds.is_empty(),
            "case {addon_name}: a published add-on reports its builds, got {addon:?}"
        );
        // Every Kubernetes version any build of the add-on supports, and the
        // builds are reported newest first, so the FIRST build that supports one
        // is that version's default build.
        let mut published_for: BTreeSet<String> = BTreeSet::new();
        for build in &builds {
            published_for.extend(supported_minors(build));
        }
        for minor in &published_for {
            let expected = builds
                .iter()
                .find(|build| supported_minors(build).iter().any(|named| named == minor))
                .map(|build| string_member(build, "addonVersion"))
                .into_iter()
                .collect::<Vec<String>>();
            assert_eq!(
                flagged_for(addon, minor),
                expected,
                "case {addon_name} on {minor}: expected the newest build published for that \
                 version flagged and no other, got {:?}",
                addon["addonVersions"]
            );
        }
        if published_for.contains(&default_version) {
            offered_for_default += 1;
        } else {
            ended_before_default += 1;
        }
    }
    assert!(
        offered_for_default > 1,
        "setup: the catalog must publish add-ons offered for {default_version}, got \
         {offered_for_default}"
    );
    assert!(
        ended_before_default > 0,
        "setup: the catalog must publish an add-on whose published line ended before \
         {default_version}, got {ended_before_default}"
    );
}

// ---------------------------------------------------------------------------
// RULE: the flag is decided against every published build of the add-on, not
// against the builds one answer carries — so a listing filtered to a cluster
// version reports, for that version, the same flag the unfiltered listing does.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_filtered_listing_reports_the_same_default_build_as_the_whole_catalog() {
    let router = emulator();
    let calendar = published_calendar(&router).await;
    let catalog = addons(&whole_catalog(&router).await);
    // What the unfiltered catalog says: per add-on and Kubernetes version, the
    // build flagged as that version's default.
    let unfiltered: BTreeMap<String, serde_json::Value> = catalog
        .iter()
        .map(|addon| (string_member(addon, "addonName"), addon.clone()))
        .collect();

    for minor in &calendar {
        let answer =
            describe_addon_versions(&router, &format!("kubernetesVersion={minor}&maxResults=100"))
                .await;
        assert_eq!(
            answer.status, 200,
            "case kubernetesVersion {minor}: expected HTTP 200, got {answer:?}"
        );
        for addon in addons(&answer) {
            let addon_name = string_member(&addon, "addonName");
            let case = format!("{addon_name} on {minor}");
            let published = unfiltered
                .get(&addon_name)
                .unwrap_or_else(|| panic!("case {case}: the whole catalog must publish it"));
            assert_eq!(
                flagged_for(&addon, minor),
                flagged_for(published, minor),
                "case {case}: expected the catalog's own default build for that version, got \
                 {:?}",
                addon["addonVersions"]
            );
            assert_eq!(
                flagged_for(&addon, minor).len(),
                1,
                "case {case}: a listing filtered to a version the add-on publishes a build for \
                 flags that version's default build, got {:?}",
                addon["addonVersions"]
            );
        }
    }
}

// ---------------------------------------------------------------------------
// RULE: `addonVersions` is the add-on's published build table — the builds it
// publishes and no others, newest first, each with its own compatibility run —
// so two add-ons publish different histories and an add-on with several builds
// reports each of them once.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_addons_builds_are_its_own_published_table() {
    let router = emulator();
    let catalog = addons(&whole_catalog(&router).await);
    let mut histories: BTreeSet<Vec<String>> = BTreeSet::new();
    let mut with_several_builds = 0_usize;

    for addon in &catalog {
        let addon_name = string_member(addon, "addonName");
        let builds: Vec<String> = versions_of(addon)
            .iter()
            .map(|build| string_member(build, "addonVersion"))
            .collect();
        let distinct: BTreeSet<&String> = builds.iter().collect();
        assert_eq!(
            distinct.len(),
            builds.len(),
            "case {addon_name}: a published build is reported once, got {builds:?}"
        );
        if builds.len() > 1 {
            with_several_builds += 1;
        }
        histories.insert(builds);

        // Each build names its own compatibility run, and the add-on's builds do
        // not all name the same single version: the history is a table, not one
        // row repeated.
        for build in versions_of(addon) {
            let case = format!("{addon_name}/{}", string_member(&build, "addonVersion"));
            let supported = supported_minors(&build);
            assert!(
                !supported.is_empty(),
                "case {case}: a published build names the Kubernetes versions it supports, got \
                 {supported:?}"
            );
        }
    }
    assert!(
        with_several_builds > 1,
        "setup: the catalog must publish add-ons with several builds, got {with_several_builds}"
    );
    assert!(
        histories.len() > 1,
        "case the published histories: a catalog that publishes one history for every add-on \
         cannot answer which builds an add-on has, got {histories:?}"
    );
}

// ---------------------------------------------------------------------------
// RULE: a name-filtered listing of an add-on whose published builds the catalog
// CITES reports exactly those builds, newest first, each over its own cited run
// of Kubernetes versions — including an add-on whose line ended, which keeps the
// builds it published.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_cited_build_tables_builds_are_served_newest_first_over_their_own_run() {
    let router = emulator();
    let calendar = published_calendar(&router).await;
    let catalog = addons(&whole_catalog(&router).await);

    for addon in &catalog {
        let addon_name = string_member(addon, "addonName");
        let named = describe_addon_versions(
            &router,
            &format!("addonName={}", encode_label(&addon_name)),
        )
        .await;
        assert_eq!(
            named.status, 200,
            "case {addon_name}: expected HTTP 200, got {named:?}"
        );
        let listed = addons(&named);
        assert_eq!(
            listed.len(),
            1,
            "case {addon_name}: a name-filtered listing reports that add-on alone, got {:?}",
            named.body["addons"]
        );
        // The name-filtered answer is the same record the whole catalog carries:
        // the filter selects rows, it does not compose them.
        assert_eq!(
            listed[0], *addon,
            "case {addon_name}: expected the catalog's own record, got {:?}",
            listed[0]
        );

        for build in versions_of(&listed[0]) {
            let version = string_member(&build, "addonVersion");
            let case = format!("{addon_name}/{version}");
            let supported = supported_minors(&build);
            // One unbroken run of published calendar rows, newest first.
            let expected: Vec<String> = calendar
                .iter()
                .skip_while(|minor| **minor != supported[0])
                .take(supported.len())
                .cloned()
                .collect();
            assert_eq!(
                supported, expected,
                "case {case}: expected one unbroken run of published rows newest first from {}, \
                 got {supported:?}",
                supported[0]
            );
        }
    }
}
