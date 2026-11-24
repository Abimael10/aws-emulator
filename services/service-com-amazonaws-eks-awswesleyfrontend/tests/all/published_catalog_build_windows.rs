// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the published catalog's BUILD WINDOWS and for the keys a
//! published configuration document declares, driven over the wire through the
//! same `MultiServiceRouter` the production binary mounts.
//!
//! Three rules are under test:
//!
//! 1. **A build the catalog publishes one by one names every Kubernetes version
//!    Amazon EKS publishes THAT BUILD for**, from the oldest through the newest,
//!    as one unbroken run of published calendar rows — a version whose own
//!    support has since ended included, because a cluster still running it
//!    installs that build and reads this listing to find it. Only the NEWEST
//!    published build is the default for each of those versions, and the add-on
//!    drops out of a listing filtered to a version outside its window.
//! 2. **A published configuration document declares the add-on's OWN keys**: an
//!    add-on whose chart configures its components separately declares the
//!    components and nothing else at its root, with each component's placement
//!    inside that component, while an add-on whose chart exposes the placement
//!    keys at its root declares them there beside the two keys the install takes.
//! 3. **`required` is a keyword only a build that cannot be installed without a
//!    property declares** — the same property set that makes
//!    `AddonVersionInfo$requiresConfiguration` true — so a build that requires
//!    nothing publishes a document with no `required` keyword at all.
//!
//! Each suite asserts over a TABLE whose rows differ along what the rule
//! generalises over: the length of the published window and where it sits on the
//! calendar, how many builds share one release line, whether the add-on is one
//! Amazon EKS publishes or one sold through AWS Marketplace, the dialect and
//! shape its document is published in, and whether it requires configuration at
//! all. Every window is checked at BOTH its published ends and one calendar row
//! past each of them, and the Kubernetes versions themselves are read out of
//! `DescribeClusterVersions` rather than written here.

use std::collections::BTreeSet;

use service_com_amazonaws_eks_awswesleyfrontend::state::ADDON_ANY_PLATFORM_VERSION;
use smithy_server_runtime::MultiServiceRouter;

use crate::addon_catalog_publication::{configuration_schema, resolved, schema_root};
use crate::addon_versions::{
    addon_names, addons, compatibilities_of, describe_addon_versions, string_member, version_key,
    versions_of, whole_catalog,
};
use crate::cluster_lifecycle::REGION;
use crate::wire::{call, emulator, encode_label};

/// The add-ons whose own published build table the catalog carries build by
/// build, as `(addonName, the OLDEST Kubernetes version those builds are
/// published for, the NEWEST one)`.
///
/// Source: the build table each add-on's own published listing carries, which is
/// what `aws eks describe-addon-versions --addon-name <name>` reports — AccuKnox
/// published one build of KubeArmor over the five Kubernetes versions of its time
/// and none since; the ADOT operator publishes a short line whose builds are each
/// offered for every Kubernetes version the operator supports, two of them of one
/// release (which is what the second `-eksbuild` build counts); and NetApp
/// publishes its Trident operator over the ten Kubernetes versions the operator
/// supports, up to the newest version it has published a build for.
///
/// The rows differ in how long the window is, where on the calendar it sits
/// (one ending well before the newest published version, one reaching it), how
/// many builds share it, and whether the add-on is a Marketplace listing or one
/// Amazon EKS publishes itself.
const CITED_BUILD_WINDOWS: &[(&str, &str, &str)] = &[
    ("accuknox_kubearmor", "1.23", "1.27"),
    ("adot", "1.31", "1.37"),
    ("netapp_trident-operator", "1.27", "1.36"),
];

/// The add-on whose published document declares the add-on's own keys ALONE,
/// because its chart configures the driver's two components separately, and the
/// keys that document declares at its root.
///
/// Source: the document `aws eks describe-addon-configuration --addon-name
/// aws-efs-csi-driver --addon-version <build>` reports: the two components and
/// the one switch that belongs to the driver as a whole.
const OWN_KEYS_ONLY_ADDON: &str = "aws-efs-csi-driver";
/// See [`OWN_KEYS_ONLY_ADDON`].
const OWN_KEYS_ONLY_ROOT_KEYS: &[&str] = &["controller", "node", "useFIPS"];
/// The key the driver's documents declare at the ROOT until the release that
/// gathered the controller's values under `controller`, from which they declare it
/// inside that component instead. See [`OWN_KEYS_ONLY_ADDON`].
const MOVED_ROOT_KEY: &str = "deleteAccessPointRootDir";
/// The driver's own release the key moved in. See [`MOVED_ROOT_KEY`].
const MOVED_IN: (u32, u32, u32) = (2, 0, 0);

/// The keys an add-on whose chart exposes the Kubernetes placement values at the
/// ROOT of its document declares there, and the add-on read as that contrast row.
///
/// Source: the same `describe-addon-configuration` documents — the add-ons Amazon
/// EKS publishes with one set of placement values for the whole chart declare them
/// at the root, which is why the rule above is a property of the ADD-ON and not a
/// blanket removal.
const ROOT_PLACEMENT_ADDON: &str = "coredns";
/// See [`ROOT_PLACEMENT_ADDON`].
const ROOT_PLACEMENT_KEYS: &[&str] = &["nodeSelector", "podAnnotations", "tolerations"];
/// The add-on read as the contrast row for the two keys the INSTALL takes, which
/// the documents that declare them carry beside the chart's own keys.
const INSTALL_KEYS_ADDON: &str = "aws-ebs-csi-driver";
/// See [`INSTALL_KEYS_ADDON`].
const INSTALL_KEYS: &[&str] = &["namespace", "serviceAccount"];

/// What each add-on's published document requires, as `(addonName, the keys a
/// `configurationValues` document must set)` — EMPTY for an add-on that can be
/// installed with no configuration at all.
///
/// Source: the configuration each add-on's own documentation states as mandatory —
/// a Marketplace add-on that cannot reach its vendor's service without an account
/// id, a token or an API key, and a storage operator that must be told whose
/// storage it is managing. The rows differ in the dialect and shape the document
/// is published in (the `$ref`-into-definitions form and the root-declared one),
/// in whether the add-on is a Marketplace listing, and in how many properties it
/// requires.
const REQUIRED_PROPERTIES: &[(&str, &[&str])] = &[
    ("netapp_trident-operator", &["cloudProvider"]),
    ("datadog_operator", &["apiKey"]),
    ("akuity_akuity-agent", &["agentId", "agentToken"]),
    ("aws-efs-csi-driver", &[]),
    ("coredns", &[]),
    ("vpc-cni", &[]),
];

/// The WHOLE published Kubernetes version calendar, newest version first — the
/// out-of-support rows included, because a published build still names them.
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
    let rows: Vec<String> = answer.body["clusterVersions"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a clusterVersions list, got {:?}", answer.body))
        .iter()
        .map(|row| string_member(row, "clusterVersion"))
        .collect();
    assert!(
        rows.len() > 1,
        "setup DescribeClusterVersions: expected the published calendar, got {rows:?}"
    );
    rows
}

/// Where `cluster_version` sits in the published calendar, newest first.
fn calendar_position(calendar: &[String], cluster_version: &str) -> usize {
    calendar
        .iter()
        .position(|row| row == cluster_version)
        .unwrap_or_else(|| {
            panic!("setup: the calendar must publish {cluster_version}, got {calendar:?}")
        })
}

/// The one `AddonInfo` the listing answers for `addon_name`, with every build of
/// it.
async fn published_addon(router: &MultiServiceRouter, addon_name: &str) -> serde_json::Value {
    let answer = describe_addon_versions(
        router,
        &format!("addonName={}&maxResults=100", encode_label(addon_name)),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "setup DescribeAddonVersions({addon_name}): expected HTTP 200, got {answer:?}"
    );
    let listed = addons(&answer);
    assert_eq!(
        listed.len(),
        1,
        "setup DescribeAddonVersions({addon_name}): expected exactly the named add-on, got {:?}",
        answer.body["addons"]
    );
    listed[0].clone()
}

/// The Kubernetes versions one reported build names, in the order reported.
fn supported_minors(build: &serde_json::Value) -> Vec<String> {
    compatibilities_of(build)
        .iter()
        .map(|entry| string_member(entry, "clusterVersion"))
        .collect()
}

/// The add-on names a listing filtered to one Kubernetes version answers.
async fn offered_for(router: &MultiServiceRouter, minor: &str) -> Vec<String> {
    let answer = describe_addon_versions(
        router,
        &format!("kubernetesVersion={}&maxResults=100", encode_label(minor)),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case kubernetesVersion {minor}: expected HTTP 200, got {answer:?}"
    );
    addon_names(&answer)
}

/// The keys a published document declares at its root.
fn root_keys(document: &serde_json::Value) -> BTreeSet<String> {
    schema_root(document)["properties"]
        .as_object()
        .unwrap_or_else(|| panic!("expected a properties object, got {document:?}"))
        .keys()
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// RULE 1: a build the catalog publishes one by one names every Kubernetes
// version Amazon EKS publishes that build for — an unbroken run of calendar rows
// from the oldest to the newest, whether or not a version's own support has since
// ended — with only the newest build default for each of them, and the add-on is
// offered for no version outside that run.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_cited_build_names_every_kubernetes_version_it_is_published_for() {
    let router = emulator();
    let calendar = published_calendar(&router).await;

    for (addon_name, oldest, newest) in CITED_BUILD_WINDOWS {
        let case = format!("{addon_name} ({oldest}..{newest})");
        let newest_at = calendar_position(&calendar, newest);
        let oldest_at = calendar_position(&calendar, oldest);
        assert!(
            newest_at <= oldest_at,
            "case {case}: the window's ends must be in calendar order, got {calendar:?}"
        );
        // The run the published window covers, newest first — read off the
        // calendar rather than written out here.
        let window: Vec<String> = calendar[newest_at..=oldest_at].to_vec();

        let builds = versions_of(&published_addon(&router, addon_name).await);
        assert!(
            !builds.is_empty(),
            "case {case}: the catalog must publish builds of it, got none"
        );
        let keys: Vec<(u32, u32, u32, u32)> = builds
            .iter()
            .map(|build| version_key(&string_member(build, "addonVersion")))
            .collect();
        assert!(
            keys.windows(2).all(|pair| pair[0] > pair[1]),
            "case {case}: the published builds are reported newest first, got {keys:?}"
        );

        for (position, build) in builds.iter().enumerate() {
            let addon_version = string_member(build, "addonVersion");
            let build_case = format!("{case}/{addon_version}");
            assert_eq!(
                supported_minors(build),
                window,
                "case {build_case}: expected the published window as one unbroken run newest \
                 first, got {build:?}"
            );
            for entry in compatibilities_of(build) {
                let minor = string_member(&entry, "clusterVersion");
                assert_eq!(
                    entry["platformVersions"],
                    serde_json::json!([ADDON_ANY_PLATFORM_VERSION]),
                    "case {build_case} on {minor}: expected every platform version of it, got {:?}",
                    entry["platformVersions"]
                );
                // Exactly the newest published build is the default for each
                // version it supports; a superseded build is the default for
                // none.
                assert_eq!(
                    entry["defaultVersion"],
                    serde_json::json!(position == 0),
                    "case {build_case} on {minor}: expected defaultVersion {}, got {:?}",
                    position == 0,
                    entry["defaultVersion"]
                );
            }
        }

        // The window is checked at BOTH its published ends and one calendar row
        // past each of them: the add-on is offered for the versions it publishes
        // builds for and for no other.
        for bound in [oldest, newest] {
            assert!(
                offered_for(&router, bound)
                    .await
                    .iter()
                    .any(|name| name == addon_name),
                "case {case}: a version at the window's end offers the add-on, got none for \
                 {bound}"
            );
        }
        for past in [
            newest_at.checked_sub(1).map(|row| calendar[row].clone()),
            calendar.get(oldest_at + 1).cloned(),
        ]
        .into_iter()
        .flatten()
        {
            assert!(
                !offered_for(&router, &past)
                    .await
                    .iter()
                    .any(|name| name == addon_name),
                "case {case}: a version past the window's end must not offer the add-on, got it \
                 for {past}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// RULE 2: a published document declares the ADD-ON's own keys — the components
// alone for a chart that configures its components separately, the placement keys
// and the install's own keys at the root for a chart that exposes them there.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_published_document_declares_the_keys_the_addons_own_chart_exposes() {
    let router = emulator();

    // EVERY build of the component-scoped add-on, so the moved key is read on
    // both sides of the release that moved it.
    let builds = versions_of(&published_addon(&router, OWN_KEYS_ONLY_ADDON).await);
    assert!(
        builds.len() > 1,
        "setup {OWN_KEYS_ONLY_ADDON}: expected a published history of several builds, got \
         {builds:?}"
    );
    for build in &builds {
        let addon_version = string_member(build, "addonVersion");
        let case = format!("{OWN_KEYS_ONLY_ADDON}/{addon_version}");
        let (major, minor, patch, _) = version_key(&addon_version);
        let document = configuration_schema(&router, OWN_KEYS_ONLY_ADDON, &addon_version).await;

        let mut expected: BTreeSet<String> = OWN_KEYS_ONLY_ROOT_KEYS
            .iter()
            .map(|key| (*key).to_string())
            .collect();
        if (major, minor, patch) < MOVED_IN {
            expected.insert(MOVED_ROOT_KEY.to_string());
        }
        assert_eq!(
            root_keys(&document),
            expected,
            "case {case}: expected the add-on's own keys alone, got {:?}",
            schema_root(&document)["properties"]
        );
        // The component's own placement keys are declared INSIDE the component,
        // which is what makes the root's key set the chart's own.
        let component = resolved(&document, &schema_root(&document)["properties"]["controller"]);
        for key in ROOT_PLACEMENT_KEYS {
            assert!(
                !component["properties"][*key].is_null(),
                "case {case}: expected {key} inside the component, got {component:?}"
            );
        }
    }

    // The contrast rows: an add-on whose chart exposes the placement values at
    // the root declares them there, and the documents that carry the install's own
    // two keys still carry them.
    let placement_builds = versions_of(&published_addon(&router, ROOT_PLACEMENT_ADDON).await);
    let placement_version = string_member(
        placement_builds
            .first()
            .unwrap_or_else(|| panic!("setup {ROOT_PLACEMENT_ADDON}: expected a build")),
        "addonVersion",
    );
    let placement = configuration_schema(&router, ROOT_PLACEMENT_ADDON, &placement_version).await;
    for key in ROOT_PLACEMENT_KEYS {
        assert!(
            root_keys(&placement).contains(*key),
            "case {ROOT_PLACEMENT_ADDON}/{placement_version}: expected the chart's root key \
             {key}, got {:?}",
            schema_root(&placement)["properties"]
        );
    }
    let install_builds = versions_of(&published_addon(&router, INSTALL_KEYS_ADDON).await);
    let install_version = string_member(
        install_builds
            .first()
            .unwrap_or_else(|| panic!("setup {INSTALL_KEYS_ADDON}: expected a build")),
        "addonVersion",
    );
    let install = configuration_schema(&router, INSTALL_KEYS_ADDON, &install_version).await;
    for key in INSTALL_KEYS {
        assert!(
            root_keys(&install).contains(*key),
            "case {INSTALL_KEYS_ADDON}/{install_version}: expected the install's key {key}, got \
             {:?}",
            schema_root(&install)["properties"]
        );
    }
}

// ---------------------------------------------------------------------------
// RULE 3: `required` is declared by the builds that cannot be installed without
// a property — the same property set `requiresConfiguration` reports — and by no
// other, so a document a client loads into a validator requires nothing the
// add-on does not.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn only_a_build_that_requires_configuration_declares_required() {
    let router = emulator();
    let catalog = addons(&whole_catalog(&router).await);

    for (addon_name, required) in REQUIRED_PROPERTIES {
        let addon = catalog
            .iter()
            .find(|addon| string_member(addon, "addonName") == *addon_name)
            .unwrap_or_else(|| panic!("setup: the catalog must publish {addon_name}"));
        let build = versions_of(addon)
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("setup {addon_name}: expected a published build"));
        let addon_version = string_member(&build, "addonVersion");
        let case = format!("{addon_name}/{addon_version}");
        let document = configuration_schema(&router, addon_name, &addon_version).await;
        let declared = schema_root(&document).get("required").cloned();

        assert_eq!(
            build["requiresConfiguration"],
            serde_json::json!(!required.is_empty()),
            "case {case}: expected requiresConfiguration {}, got {:?}",
            !required.is_empty(),
            build["requiresConfiguration"]
        );
        if required.is_empty() {
            assert_eq!(
                declared, None,
                "case {case}: a build that requires nothing declares no required keyword, got \
                 {declared:?}"
            );
            continue;
        }
        let listed: Vec<String> = declared
            .as_ref()
            .and_then(|declared| declared.as_array())
            .unwrap_or_else(|| {
                panic!("case {case}: expected a required list, got {declared:?}")
            })
            .iter()
            .map(|entry| {
                entry
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case}: expected a property name, got {entry}"))
                    .to_string()
            })
            .collect();
        for property in *required {
            assert!(
                listed.iter().any(|entry| entry == property),
                "case {case}: expected {property} among the required properties, got {listed:?}"
            );
            assert!(
                !schema_root(&document)["properties"][*property].is_null(),
                "case {case}: a required property is one the document declares, got {:?}",
                schema_root(&document)["properties"]
            );
        }
    }
}
