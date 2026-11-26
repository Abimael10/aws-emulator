// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the VERSION RECORDS the add-on catalog publishes, driven
//! over the wire through the same `MultiServiceRouter` the production binary
//! mounts.
//!
//! The RULE under test is: every member of a reported `AddonVersionInfo` comes
//! from the catalog entry of the add-on it belongs to — the compute types that
//! add-on is published for, and one `compatibilities` entry per Kubernetes minor
//! version that build supports, newest minor first, with `defaultVersion`
//! marking the build Amazon EKS installs when a caller names none (the newest
//! published build) — rather than being synthesized from the Kubernetes version
//! ladder; and `DescribeAddonConfiguration` publishes each add-on's own
//! configuration document, in the form and dialect that add-on publishes it in.
//!
//! So every suite asserts over a TABLE whose rows differ along what the rule
//! generalises over (which add-on, which build of it, how wide its published
//! window is, which document form it publishes), and the whole-catalog cases walk
//! every add-on the catalog publishes rather than the few a recorded request
//! named. The Kubernetes versions each case uses are read from the published
//! calendar, so the suites keep asserting the rule as the published tables grow.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use service_com_amazonaws_eks_awswesleyfrontend::state::{
    ADDON_COMPUTE_TYPES_EC2, ADDON_COMPUTE_TYPES_EC2_AUTO, ADDON_COMPUTE_TYPES_EC2_HYBRID_AUTO,
    ADDON_COMPUTE_TYPES_EVERY_COMPUTE, ADDON_COMPUTE_TYPE_EC2, ADDON_SCHEMA_DIALECT_2019_09,
    ADDON_SCHEMA_DIALECT_DRAFT_06,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::addon_catalog_publication::{configuration_schema, resolved, schema_root};
use crate::addon_lifecycle::{
    create_addon, default_cluster_version, describe_addon, published_calendar_rows,
};
use crate::addon_versions::{
    addons, compatibilities_of, describe_addon_versions, string_member, versions_of, whole_catalog,
};
use crate::cluster_lifecycle::{create, minimal_request, REGION};
use crate::cluster_versions::version_key;
use crate::wire::{call, emulator, encode_label, error_code};

/// How long a clock-owed cluster move is given before the suite calls it a
/// failure. Generous on purpose: what is asserted is the state a call reaches,
/// never how fast the host gets there.
const POLL_DEADLINE: Duration = Duration::from_secs(30);

/// How often the poll loop looks again. See [`POLL_DEADLINE`].
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The status a cluster carries once the clock has finished creating it.
const CLUSTER_ACTIVE: &str = "ACTIVE";

/// The compute types `AddonVersionInfo$computeTypes` may name: the kinds of
/// compute an Amazon EKS cluster runs pods on.
///
/// SOURCE: the catalog's own compute-type column (`crate::state`'s
/// `ADDON_COMPUTE_TYPE_*`), which is what the published combinations are built
/// out of.
const PUBLISHED_COMPUTE_TYPES: &[&str] = &["ec2", "hybrid", "fargate", "auto"];

/// The compute types the catalog publishes one add-on's builds for.
///
/// SOURCE: the `compute_types` column of that add-on's catalog row
/// (`crate::state::addon_catalog`'s `ADDON_CATALOG`). The rows span the
/// combinations the catalog publishes and the kinds of add-on that carry them: a
/// Deployment-only add-on offered on every kind of compute, one with a node
/// component (so not on Fargate), and a vendor's own build offered on EC2 nodes
/// alone — including add-ons no recorded request named (`coredns`, `kube-proxy`,
/// `datadog_operator`). An add-on whose own published builds were offered on
/// DIFFERENT compute is not a row here: `computeTypes` is a member of
/// `AddonVersionInfo`, so such an add-on's compute is asserted per build
/// ([`EXPECTED_BUILD_COMPUTE_TYPES`]).
const EXPECTED_COMPUTE_TYPES: &[(&str, &[&str])] = &[
    ("adot", ADDON_COMPUTE_TYPES_EVERY_COMPUTE),
    ("coredns", ADDON_COMPUTE_TYPES_EVERY_COMPUTE),
    ("vpc-cni", ADDON_COMPUTE_TYPES_EC2_AUTO),
    ("kube-proxy", ADDON_COMPUTE_TYPES_EC2_HYBRID_AUTO),
    ("accuknox_kubearmor", ADDON_COMPUTE_TYPES_EC2),
    ("datadog_operator", ADDON_COMPUTE_TYPES_EC2),
];

/// The compute types the catalog publishes ONE BUILD of one add-on for, where
/// that add-on's builds were not all published for the same compute.
///
/// SOURCE: the `compute_types` column of that BUILD's row in the add-on's
/// published build table (`crate::state::addon_catalog`'s
/// `ADDON_PUBLISHED_BUILDS`). `AddonVersionInfo$computeTypes` is "the compute
/// type of the add-on VERSION", so a project that gained support for a kind of
/// compute in one of its release lines is published for it from that line on
/// while its earlier builds keep the compute they were published for: the EFS
/// CSI driver's `1.5` line runs on the EC2 instances of a node group alone, its
/// current line on the nodes EKS Auto Mode provisions as well.
const EXPECTED_BUILD_COMPUTE_TYPES: &[(&str, &str, &[&str])] = &[
    (
        "aws-efs-csi-driver",
        "v1.5.9-eksbuild.1",
        ADDON_COMPUTE_TYPES_EC2,
    ),
    (
        "aws-efs-csi-driver",
        "v1.5.8-eksbuild.1",
        ADDON_COMPUTE_TYPES_EC2,
    ),
    (
        "aws-efs-csi-driver",
        "v3.5.2-eksbuild.1",
        ADDON_COMPUTE_TYPES_EC2_AUTO,
    ),
];

/// The Kubernetes versions `DescribeClusterVersions` publishes, in the order the
/// catalog reports them (newest first) — the calendar the add-on catalog's
/// compatibilities are rows of, out-of-support rows included, since a published
/// build still names them.
async fn published_calendar_minors(router: &MultiServiceRouter) -> Vec<String> {
    let versions: Vec<String> = published_calendar_rows(router)
        .await
        .iter()
        .map(|row| string_member(row, "clusterVersion"))
        .collect();
    assert!(
        versions.len() > 1,
        "setup DescribeClusterVersions: expected a published calendar, got {versions:?}"
    );
    versions
}

/// A created cluster on the calendar's default version, read back once the clock
/// has carried it to `ACTIVE` — the precondition an add-on install needs.
///
/// Local to this suite rather than one of the other suites' same-named helpers
/// because those wait out the activation SPAN; this one waits for the CONDITION,
/// looking again until the cluster reports `ACTIVE` or a generous deadline
/// passes, so what it asserts is the state the cluster reaches and not how fast
/// the host gets there.
async fn active_cluster(router: &MultiServiceRouter, case: &str, name: &str) {
    let created = create(router, &minimal_request(name)).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: setup CreateCluster expected 200, got {created:?}"
    );
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let answer = call(
            router,
            REGION,
            "GET",
            &format!("/clusters/{}", encode_label(name)),
            "",
        )
        .await;
        if answer.status == 200 && answer.body["cluster"]["status"] == serde_json::json!(CLUSTER_ACTIVE)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the cluster never reached {CLUSTER_ACTIVE}, last read {answer:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One published calendar row's days: when Amazon EKS released that Kubernetes
/// version, and when its extended support ends.
#[derive(Debug, Clone, Copy)]
struct SupportWindow {
    /// `ClusterVersionInformation$releaseDate`, in epoch seconds as reported.
    released: f64,
    /// `ClusterVersionInformation$endOfExtendedSupportDate`, likewise.
    end_of_extended_support: f64,
}

/// Every published calendar row's days, by Kubernetes minor version — read out of
/// `DescribeClusterVersions`, so the window a build is published over is checked
/// against the published calendar rather than against dates written here.
async fn published_support_window(router: &MultiServiceRouter) -> BTreeMap<String, SupportWindow> {
    published_calendar_rows(router)
        .await
        .iter()
        .map(|row| {
            let minor = string_member(row, "clusterVersion");
            let day = |member: &str| {
                row[member].as_f64().unwrap_or_else(|| {
                    panic!("setup {minor}: expected a {member}, got {row:?}")
                })
            };
            (
                minor.clone(),
                SupportWindow {
                    released: day("releaseDate"),
                    end_of_extended_support: day("endOfExtendedSupportDate"),
                },
            )
        })
        .collect()
}

/// The `computeTypes` one reported build names.
fn compute_types_of(build: &serde_json::Value) -> Vec<String> {
    build["computeTypes"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a computeTypes list, got {build:?}"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("expected a compute type string, got {value:?}"))
                .to_string()
        })
        .collect()
}

/// The Kubernetes minor versions one reported build names as compatible, in the
/// order reported.
fn supported_minors(build: &serde_json::Value) -> Vec<String> {
    compatibilities_of(build)
        .iter()
        .map(|entry| string_member(entry, "clusterVersion"))
        .collect()
}

/// Each compatibility's `defaultVersion` flag, in the order reported.
fn default_flags(build: &serde_json::Value) -> Vec<bool> {
    compatibilities_of(build)
        .iter()
        .map(|entry| entry["defaultVersion"].as_bool().unwrap_or(false))
        .collect()
}

// ---------------------------------------------------------------------------
// RULE: every build of an add-on reports the compute types THAT BUILD is
// published for, which the catalog carries per add-on and, where an add-on's own
// builds differ, per build — so a caller can narrow the catalog to the compute
// its cluster runs.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_published_build_reports_the_addons_own_compute_types() {
    let router = emulator();
    let catalog = addons(&whole_catalog(&router).await);

    for (addon_name, expected) in EXPECTED_COMPUTE_TYPES {
        let addon = catalog
            .iter()
            .find(|addon| string_member(addon, "addonName") == *addon_name)
            .unwrap_or_else(|| panic!("case {addon_name:?}: the catalog must publish this add-on"));
        for build in versions_of(addon) {
            let case = format!("{addon_name}/{}", string_member(&build, "addonVersion"));
            let reported = compute_types_of(&build);
            assert_eq!(
                reported,
                expected.iter().map(|value| value.to_string()).collect::<Vec<_>>(),
                "case {case}: expected the compute types the catalog publishes the add-on for, \
                 got {reported:?}"
            );
        }
    }

    // And where an add-on's own builds were published for different compute, each
    // build reports its own: the member belongs to the VERSION.
    for (addon_name, addon_version, expected) in EXPECTED_BUILD_COMPUTE_TYPES {
        let addon = catalog
            .iter()
            .find(|addon| string_member(addon, "addonName") == *addon_name)
            .unwrap_or_else(|| panic!("case {addon_name:?}: the catalog must publish this add-on"));
        let build = versions_of(addon)
            .into_iter()
            .find(|build| string_member(build, "addonVersion") == *addon_version)
            .unwrap_or_else(|| {
                panic!("case {addon_name}/{addon_version}: the catalog must publish this build")
            });
        let reported = compute_types_of(&build);
        assert_eq!(
            reported,
            expected.iter().map(|value| value.to_string()).collect::<Vec<_>>(),
            "case {addon_name}/{addon_version}: expected the compute types the catalog publishes \
             that BUILD for, got {reported:?}"
        );
    }

    // Across the WHOLE catalog: every build names compute, every value is one of
    // the published kinds, and every add-on runs on the EC2 instances of a node
    // group.
    let mut published_sets: BTreeSet<Vec<String>> = BTreeSet::new();
    for addon in &catalog {
        let addon_name = string_member(addon, "addonName");
        for build in versions_of(addon) {
            let case = format!("{addon_name}/{}", string_member(&build, "addonVersion"));
            let reported = compute_types_of(&build);
            assert!(
                !reported.is_empty(),
                "case {case}: a published build names the compute it runs on, got {reported:?}"
            );
            for compute_type in &reported {
                assert!(
                    PUBLISHED_COMPUTE_TYPES.contains(&compute_type.as_str()),
                    "case {case}: expected one of the published compute types \
                     {PUBLISHED_COMPUTE_TYPES:?}, got {compute_type:?}"
                );
            }
            assert!(
                reported.iter().any(|value| value == ADDON_COMPUTE_TYPE_EC2),
                "case {case}: every add-on is published for the EC2 instances of a node group, \
                 got {reported:?}"
            );
            published_sets.insert(reported);
        }
    }
    assert!(
        published_sets.len() > 1,
        "case the published compute types: a catalog that names the same compute for every \
         add-on cannot answer which compute an add-on runs on, got {published_sets:?}"
    );
}

// ---------------------------------------------------------------------------
// RULE: a build's `compatibilities` carry ONE entry per Kubernetes minor version
// that build supports, newest first — an unbroken run of published calendar rows
// starting at the newest version the build is published for and never reaching
// past the oldest version the add-on is offered for. A version whose own support
// has since ended is still named by a build published for it.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_builds_compatibilities_are_one_entry_per_supported_minor_newest_first() {
    let router = emulator();
    let calendar = published_calendar_minors(&router).await;
    let support = published_support_window(&router).await;
    let catalog = addons(&whole_catalog(&router).await);

    for addon in &catalog {
        let addon_name = string_member(addon, "addonName");
        let builds = versions_of(addon);
        // The oldest Kubernetes version this add-on is offered for at all: the
        // floor every one of its builds' windows stops at.
        let oldest_offered = builds
            .iter()
            .flat_map(supported_minors)
            .min_by_key(|minor| version_key(minor))
            .unwrap_or_else(|| panic!("case {addon_name:?}: a build names a compatible version"));
        for build in &builds {
            let case = format!("{addon_name}/{}", string_member(build, "addonVersion"));
            let reported = supported_minors(build);
            assert!(
                !reported.is_empty(),
                "case {case}: a published build names the Kubernetes versions it supports, got \
                 {reported:?}"
            );
            for minor in &reported {
                assert!(
                    calendar.contains(minor),
                    "case {case}: a compatibility names a version the calendar publishes, got \
                     {minor:?} against {calendar:?}"
                );
            }
            // Newest minor first, each named once: the order and the granularity
            // the catalog publishes a build's compatibilities in.
            let mut descending = reported.clone();
            descending.sort_by_key(|minor| version_key(minor));
            descending.reverse();
            assert_eq!(
                reported, descending,
                "case {case}: expected the supported minors newest first, got {reported:?}"
            );
            let distinct: BTreeSet<&String> = reported.iter().collect();
            assert_eq!(
                distinct.len(),
                reported.len(),
                "case {case}: a Kubernetes version is named once, got {reported:?}"
            );
            // The window itself: one unbroken run of published calendar rows,
            // newest first, starting at the newest version the build is published
            // for and reaching back no further than the oldest version the add-on
            // is offered for at all. WHERE a run stops is the BUILD's own — a
            // build the catalog publishes one by one carries both ends of its run,
            // while a build derived along the add-on's release line stops where
            // the support calendar stopped offering a version — so what every
            // build is held to here is the run itself.
            let newest_supported = &reported[0];
            let published_at = support
                .get(newest_supported)
                .map(|row| row.released)
                .unwrap_or_else(|| panic!("case {case}: the calendar publishes {newest_supported}"));
            let expected: Vec<String> = calendar
                .iter()
                .skip_while(|minor| *minor != newest_supported)
                .take(reported.len())
                .cloned()
                .collect();
            assert_eq!(
                reported, expected,
                "case {case}: expected one unbroken run of published rows newest first from \
                 {newest_supported}, got {reported:?}"
            );
            let floor = reported
                .last()
                .unwrap_or_else(|| panic!("case {case}: a build names a version"));
            assert!(
                version_key(floor) >= version_key(oldest_offered.as_str()),
                "case {case}: a build's window must not reach past {oldest_offered}, the oldest \
                 version the add-on is offered for, got {reported:?}"
            );
            // A version whose own support has ended is not excluded from a
            // window — what a build supports is the build's published span — but
            // the version it was published alongside was still offered then, so no
            // window is made up of ended versions alone.
            let ended = reported
                .iter()
                .filter(|minor| {
                    support
                        .get(*minor)
                        .is_some_and(|row| row.end_of_extended_support <= published_at)
                })
                .count();
            assert!(
                ended < reported.len(),
                "case {case}: a build names the version it was published alongside, which was \
                 still offered then, got {reported:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// RULE: `defaultVersion` marks, on each compatibility row, whether that build is
// the DEFAULT build for the Kubernetes version the row names — the newest
// published build of the add-on that supports it — so every version an add-on
// publishes a build for has exactly one default build, and an install on a
// cluster of that version that names no version is given that same build.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_default_build_is_the_newest_build_published_for_each_cluster_version() {
    let router = emulator();
    let catalog = addons(&whole_catalog(&router).await);

    for addon in &catalog {
        let addon_name = string_member(addon, "addonName");
        let builds = versions_of(addon);
        // The builds are reported newest first, so the first one that supports a
        // Kubernetes version is that version's default build.
        let default_for = |minor: &str| {
            builds
                .iter()
                .find(|build| supported_minors(build).iter().any(|named| named == minor))
                .map(|build| string_member(build, "addonVersion"))
        };
        for build in &builds {
            let version = string_member(build, "addonVersion");
            let minors = supported_minors(build);
            let flags = default_flags(build);
            let expected: Vec<bool> = minors
                .iter()
                .map(|minor| default_for(minor).as_ref() == Some(&version))
                .collect();
            assert_eq!(
                flags, expected,
                "case {addon_name}/{version}: expected defaultVersion {expected:?} over \
                 {minors:?} — the newest build published for each of them — got {flags:?}"
            );
        }
    }

    // Read one version at a time, which is how a client resolves a build for its
    // own cluster: the filtered listing marks the same build the whole catalog
    // marks, and marks at most one.
    let calendar = published_calendar_minors(&router).await;
    for minor in &calendar {
        let answer =
            describe_addon_versions(&router, &format!("kubernetesVersion={minor}&maxResults=100"))
                .await;
        assert_eq!(
            answer.status, 200,
            "case kubernetesVersion {minor}: expected HTTP 200, got {answer:?}"
        );
        for addon in addons(&answer) {
            let case = format!("{}/{minor}", string_member(&addon, "addonName"));
            let builds = versions_of(&addon);
            let defaults: Vec<String> = builds
                .iter()
                .filter(|build| default_flags(build).iter().any(|flag| *flag))
                .map(|build| string_member(build, "addonVersion"))
                .collect();
            assert_eq!(
                defaults,
                vec![string_member(&builds[0], "addonVersion")],
                "case {case}: the newest build published for a Kubernetes version is that \
                 version's default build, got {defaults:?}"
            );
        }
    }

    // The round trip: an install that names no version takes the build the
    // catalog marks, and reads back at it.
    let version = default_cluster_version(&router).await;
    active_cluster(&router, "default build", "catalog-default").await;
    for addon_name in ["vpc-cni", "datadog_operator"] {
        let answer =
            describe_addon_versions(&router, &format!("addonName={addon_name}&kubernetesVersion={version}"))
                .await;
        let listed = addons(&answer);
        assert_eq!(
            listed.len(),
            1,
            "case {addon_name:?}: expected one AddonInfo for the cluster's version, got \
             {:?}",
            answer.body["addons"]
        );
        let builds = versions_of(&listed[0]);
        let marked = string_member(&builds[0], "addonVersion");
        let created = create_addon(
            &router,
            "catalog-default",
            &serde_json::json!({ "addonName": addon_name }),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {addon_name:?}: expected HTTP 200 from CreateAddon, got {created:?}"
        );
        let described = describe_addon(&router, "catalog-default", addon_name).await;
        assert_eq!(
            described.status, 200,
            "case {addon_name:?}: the installed add-on must read back, got {described:?}"
        );
        assert_eq!(
            described.addon()["addonVersion"],
            serde_json::json!(marked),
            "case {addon_name:?}: expected the build the catalog marks as the default {marked:?}, \
             got {:?}",
            described.addon()["addonVersion"]
        );
    }
}

// ---------------------------------------------------------------------------
// RULE: `DescribeAddonConfiguration` publishes the add-on's OWN configuration
// document — the keys that add-on takes, in the form and the dialect that
// add-on's published document is written in — and the install of a build is held
// to the same document.
// ---------------------------------------------------------------------------

/// Which shape an add-on's published configuration document takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocumentForm {
    /// A root `$ref` into the document's own `definitions`.
    Definitions,
    /// The add-on's keys declared at the document root.
    Root,
}

/// The dialect and form each add-on's published document is written in, with one
/// key that document declares.
///
/// SOURCE: the document `aws eks describe-addon-configuration --addon-name
/// <name> --addon-version <build>` reports: the longest-published documents are
/// a root `$ref` into their own `definitions`, and the ones reissued in the
/// 2019-09 dialect declare the add-on's keys at the root. `coredns` is an add-on
/// no recorded request of the reported divergence named.
const EXPECTED_DOCUMENTS: &[(&str, &str, DocumentForm, &str, &str)] = &[
    (
        "vpc-cni",
        ADDON_SCHEMA_DIALECT_DRAFT_06,
        DocumentForm::Definitions,
        "eniConfig",
        "object",
    ),
    (
        "coredns",
        ADDON_SCHEMA_DIALECT_DRAFT_06,
        DocumentForm::Definitions,
        "corefile",
        "string",
    ),
    (
        "aws-efs-csi-driver",
        ADDON_SCHEMA_DIALECT_2019_09,
        DocumentForm::Root,
        "useFIPS",
        "boolean",
    ),
];

#[tokio::test]
async fn the_published_configuration_document_takes_the_addons_own_form() {
    let router = emulator();
    let catalog = addons(&whole_catalog(&router).await);

    for (addon_name, dialect, form, key, key_type) in EXPECTED_DOCUMENTS {
        let addon = catalog
            .iter()
            .find(|addon| string_member(addon, "addonName") == *addon_name)
            .unwrap_or_else(|| panic!("case {addon_name:?}: the catalog must publish this add-on"));
        // Both ends of the add-on's published history: the document is the
        // add-on's, so every build of it publishes the same form.
        let builds = versions_of(addon);
        for build in [&builds[0], &builds[builds.len() - 1]] {
            let addon_version = string_member(build, "addonVersion");
            let case = format!("{addon_name}/{addon_version}");
            let document = configuration_schema(&router, addon_name, &addon_version).await;
            assert_eq!(
                document["$schema"].as_str(),
                Some(*dialect),
                "case {case}: expected the dialect the add-on's document declares, got {:?}",
                document["$schema"]
            );
            let reported = if document["$ref"].is_string() {
                DocumentForm::Definitions
            } else {
                DocumentForm::Root
            };
            assert_eq!(
                reported, *form,
                "case {case}: expected the form the add-on's document is published in, got \
                 {document:?}"
            );
            if *form == DocumentForm::Root {
                assert!(
                    document["definitions"].is_null(),
                    "case {case}: a document that declares its keys at the root carries no \
                     definitions section, got {:?}",
                    document["definitions"]
                );
                assert!(
                    document["description"].as_str().is_some_and(|text| text.contains(addon_name)),
                    "case {case}: expected the description of what the document configures, got \
                     {:?}",
                    document["description"]
                );
            }
            let root = schema_root(&document);
            assert_eq!(
                root["additionalProperties"],
                serde_json::json!(false),
                "case {case}: a key the document does not declare is not a key the add-on takes, \
                 got {:?}",
                root["additionalProperties"]
            );
            let declared = &root["properties"][*key];
            assert!(
                !declared.is_null(),
                "case {case}: expected the add-on's own key {key:?} among the document's \
                 properties, got {:?}",
                root["properties"]
            );
            assert_eq!(
                resolved(&document, declared)["type"].as_str(),
                Some(*key_type),
                "case {case}: expected {key:?} declared as {key_type:?}, got {declared:?}"
            );
        }
    }

    // The EFS CSI driver's components are published with the keys of each
    // component, the access-point switch among them — a key declared inside the
    // component it belongs to rather than at the root.
    let builds = versions_of(
        catalog
            .iter()
            .find(|addon| string_member(addon, "addonName") == "aws-efs-csi-driver")
            .unwrap_or_else(|| panic!("the catalog must publish aws-efs-csi-driver")),
    );
    let addon_version = string_member(&builds[0], "addonVersion");
    let document = configuration_schema(&router, "aws-efs-csi-driver", &addon_version).await;
    for (component, key) in [
        ("controller", "deleteAccessPointRootDir"),
        ("controller", "logLevel"),
        ("node", "logLevel"),
    ] {
        let case = format!("aws-efs-csi-driver/{addon_version}/{component}.{key}");
        let declared = resolved(&document, &schema_root(&document)["properties"][component]);
        assert!(
            !declared["properties"][key].is_null(),
            "case {case}: expected the component's own key, got {declared:?}"
        );
    }
}

#[tokio::test]
async fn an_install_is_held_to_the_document_the_catalog_publishes() {
    let router = emulator();
    active_cluster(&router, "held to the document", "schema-install").await;
    let version = default_cluster_version(&router).await;

    // Rows differ in the add-on — so in the FORM its published document takes,
    // the `$ref`-into-definitions one and the root-declared one — and, within an
    // add-on, in how the document stands against the published one: a key it does
    // not declare, a key whose declared type the value is not, the same two INSIDE
    // an object-valued key whose own keys the document declares, and the keys the
    // document does declare (at its root, inside such an object, and inside an
    // object the document declares no key of, which takes the chart's own values).
    // The refused rows of an add-on come first, so the accepted one is the install
    // the cluster keeps.
    #[allow(clippy::type_complexity)]
    let addons_under_test: &[(&str, &[(&str, fn() -> serde_json::Value, u16)])] = &[
        (
            "vpc-cni",
            &[
                (
                    "a key the document does not declare",
                    || serde_json::json!({ "notAKeyTheChartTakes": true }),
                    400,
                ),
                (
                    "a key set to a value of another type",
                    || serde_json::json!({ "branchENICooldown": "not a number" }),
                    400,
                ),
                (
                    "the add-on's own keys",
                    || {
                        serde_json::json!({
                            "enableNetworkPolicy": "true",
                            "env": { "WARM_IP_TARGET": "5" },
                            "eniConfig": { "create": true, "region": REGION, "subnets": {} },
                        })
                    },
                    200,
                ),
            ],
        ),
        (
            "aws-efs-csi-driver",
            &[
                (
                    "a key the document does not declare",
                    || serde_json::json!({ "notAKeyTheChartTakes": true }),
                    400,
                ),
                (
                    "a key set to a value of another type",
                    || serde_json::json!({ "useFIPS": "yes" }),
                    400,
                ),
                (
                    "a key the component object does not declare",
                    || {
                        serde_json::json!({
                            "controller": { "notAKeyTheComponentTakes": true }
                        })
                    },
                    400,
                ),
                (
                    "a declared key of the component set to another type",
                    || serde_json::json!({ "controller": { "logLevel": "verbose" } }),
                    400,
                ),
                (
                    "the add-on's own keys",
                    || {
                        serde_json::json!({
                            "useFIPS": true,
                            "node": { "replicaCount": 2, "logLevel": 4 },
                            "controller": {
                                "deleteAccessPointRootDir": true,
                                "logLevel": 4,
                                // An object the published document declares no key
                                // of: the chart's own values go inside it, so it
                                // is not closed.
                                "nodeSelector": { "topology.kubernetes.io/zone": "us-west-2a" },
                            },
                        })
                    },
                    200,
                ),
            ],
        ),
    ];

    for (addon_name, cases) in addons_under_test {
        let answer = describe_addon_versions(
            &router,
            &format!("addonName={addon_name}&kubernetesVersion={version}"),
        )
        .await;
        let addon_version = string_member(&versions_of(&addons(&answer)[0])[0], "addonVersion");
        for (case, values, expected) in *cases {
            let case = format!("{addon_name}: {case}");
            let values = values();
            let request = serde_json::json!({
                "addonName": addon_name,
                "addonVersion": addon_version,
                "configurationValues": values.to_string(),
            });
            let answer = create_addon(&router, "schema-install", &request).await;
            assert_eq!(
                answer.status, *expected,
                "case {case:?}: expected HTTP {expected}, got {answer:?}"
            );
            if *expected == 200 {
                let described = describe_addon(&router, "schema-install", addon_name).await;
                assert_eq!(
                    described.addon()["configurationValues"],
                    serde_json::json!(values.to_string()),
                    "case {case:?}: the stored document must read back as supplied, got \
                     {described:?}"
                );
                continue;
            }
            assert_eq!(
                error_code(&answer),
                "InvalidParameterException",
                "case {case:?}: a document the published schema refuses is an invalid parameter, \
                 got {answer:?}"
            );
        }
    }
}
