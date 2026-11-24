// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for what the read-only EKS catalogs PUBLISH about an add-on
//! and a Kubernetes version, and for the refusal a catalog read earns when it
//! leaves out a parameter the operation requires — all driven over the wire
//! through the same `MultiServiceRouter` the production binary mounts.
//!
//! Four rules are asserted here, each over a TABLE whose rows differ along what
//! the rule generalises over rather than over one recorded request:
//!
//! * a request whose METHOD AND PATH name an operation of this API is that
//!   operation's request: a query-string parameter the operation requires and the
//!   request omits is refused with the modelled `InvalidParameterException` (400)
//!   in the sentence this API states for that parameter, never with the
//!   not-implemented answer a request naming no operation gets;
//! * every build the catalog publishes for an add-on has a published
//!   CONFIGURATION SCHEMA — including the builds of the earlier release lines
//!   Amazon EKS still publishes for the older cluster versions — so a
//!   `(addonName, addonVersion)` pair read out of the catalog resolves, and only a
//!   pair the catalog does not publish is refused;
//! * an add-on's `type` and `publisher` are the ADD-ON's own published values, so
//!   an add-on another Amazon Web Services service team builds carries that
//!   service's publisher and its own category, and the `types` / `publishers`
//!   filters narrow the catalog to exactly the add-ons carrying them;
//! * each calendar row publishes its own support lifecycle, and the
//!   control-plane constraints a row publishes are the window a cluster of that
//!   version may be CONFIGURED in rather than the bound the model lets a member
//!   carry.
//!
//! The request harness is the crate's own (`wire.rs`) and the catalog readers are
//! the ones `addon_versions.rs` owns.

use std::collections::BTreeSet;

use service_com_amazonaws_eks_awswesleyfrontend::state::{
    ADDON_CATALOG_OLDEST_CLUSTER_VERSION, ADDON_PUBLISHER_EKS, ADDON_PUBLISHER_SAGEMAKER,
    ADDON_TYPE_ML_OPERATOR,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::addon_versions::{
    addons, compatibilities_of, describe_addon_configuration, describe_addon_versions,
    string_member, versions_of, whole_catalog,
};
use crate::cluster_lifecycle::REGION;
use crate::wire::{call, emulator, error_code, Answer};

/// The catalog entry for one published add-on name.
async fn catalog_entry(router: &MultiServiceRouter, addon_name: &str) -> serde_json::Value {
    let answer = describe_addon_versions(router, &format!("addonName={addon_name}")).await;
    assert_eq!(
        answer.status, 200,
        "setup DescribeAddonVersions {addon_name:?}: expected HTTP 200, got {answer:?}"
    );
    let listed = addons(&answer);
    assert_eq!(
        listed.len(),
        1,
        "setup {addon_name:?}: a published name answers exactly that add-on, got {:?}",
        answer.body["addons"]
    );
    listed[0].clone()
}

/// Every build one published add-on reports, newest first.
fn published_builds(addon: &serde_json::Value) -> Vec<String> {
    versions_of(addon)
        .iter()
        .map(|build| string_member(build, "addonVersion"))
        .collect()
}

/// The whole published version calendar, newest row first — the out-of-support
/// rows included, which is what `includeAll` widens the listing to.
async fn published_calendar(router: &MultiServiceRouter) -> Vec<serde_json::Value> {
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
    answer.body["clusterVersions"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a clusterVersions list, got {:?}", answer.body))
        .clone()
}

/// The first instant (UTC) of a published calendar day `YYYY-MM-DD`, in epoch
/// seconds — what a row's three date members carry on the wire.
///
/// Parsed by `aws-smithy-types`, the parser the wire layer itself uses, rather
/// than by calendar arithmetic written here.
fn published_day(day: &str) -> f64 {
    let text = format!("{day}T00:00:00Z");
    let parsed = aws_smithy_types::DateTime::from_str(
        &text,
        aws_smithy_types::date_time::Format::DateTime,
    )
    .unwrap_or_else(|error| panic!("{text} is not an RFC-3339 instant: {error}"));
    parsed.secs() as f64
}

/// One date member of a calendar row, in epoch seconds.
fn epoch_secs(value: &serde_json::Value) -> f64 {
    value
        .as_f64()
        .unwrap_or_else(|| panic!("expected an epoch-seconds timestamp, got {value:?}"))
}

/// Seconds in one UTC day, for stating a span between two published days.
const DAY: f64 = 24.0 * 60.0 * 60.0;

// ---------------------------------------------------------------------------
// RULE: a request whose method and path name an operation of this API IS that
// operation's request. A query-string parameter the operation requires and the
// request omits is refused with the modelled `InvalidParameterException` (400) in
// the sentence Amazon EKS states for that parameter — never with the
// not-implemented answer, which says the operation does not exist.
// ---------------------------------------------------------------------------

/// The sentence Amazon EKS states for each add-on parameter a catalog read
/// requires, and the member it is about.
///
/// SOURCE: the refusal the service answers a `DescribeAddonConfiguration` that
/// leaves the parameter out with. Both members of that operation are `@required`
/// `@httpQuery` (`smithy_query(query="operation",
/// operation="DescribeAddonConfiguration")`), so each of them has such a
/// sentence; the parameter is named in WORDS rather than by its member spelling.
const REQUIRED_PARAMETER_SENTENCE: &[(&str, &str)] = &[
    ("addonName", "The addon name parameter is required."),
    ("addonVersion", "The addon version parameter is required."),
];

/// The sentence for one member of [`REQUIRED_PARAMETER_SENTENCE`].
fn required_sentence(member: &str) -> &'static str {
    REQUIRED_PARAMETER_SENTENCE
        .iter()
        .find(|(named, _)| *named == member)
        .map(|(_, sentence)| *sentence)
        .unwrap_or_else(|| panic!("no published sentence for {member}"))
}

#[tokio::test]
async fn an_omitted_required_query_parameter_is_refused_as_an_invalid_parameter() {
    let router = emulator();

    // The pair every row is built around is read out of the catalog itself, so
    // the rows keep holding as the published catalog grows.
    let addon = catalog_entry(&router, "vpc-cni").await;
    let addon_version = published_builds(&addon)
        .first()
        .cloned()
        .expect("a published build of vpc-cni");

    // Rows: each way a required parameter can be left out — neither supplied, one
    // of the two supplied, and the other one supplied — so the refusal is not
    // asserted of one recorded query string.
    for (case, query, member) in [
        ("neither parameter", String::new(), "addonName"),
        ("the name alone", "addonName=vpc-cni".to_string(), "addonVersion"),
        (
            "the version alone",
            format!("addonVersion={addon_version}"),
            "addonName",
        ),
        (
            "the name alone, beside a parameter the operation does not take",
            "unknown=1&addonName=vpc-cni".to_string(),
            "addonVersion",
        ),
    ] {
        let uri = if query.is_empty() {
            "/addons/configuration-schemas".to_string()
        } else {
            format!("/addons/configuration-schemas?{query}")
        };
        let answer = call(&router, REGION, "GET", &uri, "").await;
        assert_eq!(
            answer.status, 400,
            "case {case:?} ({uri}): expected HTTP 400, got {answer:?}"
        );
        assert_eq!(
            error_code(&answer),
            "InvalidParameterException",
            "case {case:?}: expected the modelled parameter refusal, got {answer:?}"
        );
        assert_eq!(
            answer.message(),
            required_sentence(member),
            "case {case:?}: expected the sentence for {member}, got {:?}",
            answer.message()
        );
    }

    // The other side of the rule: a request that supplies the parameters is
    // served, an operation that requires none is served with no query string at
    // all, and a path this API binds no operation to is still answered as the
    // unimplemented operation it is.
    let served = describe_addon_configuration(&router, "vpc-cni", &addon_version).await;
    assert_eq!(
        served.status, 200,
        "case both parameters supplied: expected HTTP 200, got {served:?}"
    );
    let unfiltered = describe_addon_versions(&router, "").await;
    assert_eq!(
        unfiltered.status, 200,
        "case an operation that requires no query parameter: expected HTTP 200, got {unfiltered:?}"
    );
    let unbound = call(&router, REGION, "GET", "/addons/not-an-operation", "").await;
    assert_eq!(
        unbound.status, 501,
        "case a path this API binds nothing to: expected the unimplemented answer, got {unbound:?}"
    );
    assert_eq!(
        error_code(&unbound),
        "NotImplemented",
        "case a path this API binds nothing to: expected NotImplemented, got {unbound:?}"
    );
}

// ---------------------------------------------------------------------------
// RULE: every build the catalog PUBLISHES for an add-on has a published
// configuration schema — the builds of an earlier release line Amazon EKS still
// publishes for the older cluster versions included — so a
// `(addonName, addonVersion)` pair read out of the catalog resolves, and only a
// pair the catalog publishes no build of is refused.
// ---------------------------------------------------------------------------

/// The add-ons this case walks.
///
/// SOURCE: the catalog's own rows (`crate::state::addon_catalog`'s
/// `ADDON_CATALOG`). They span the kinds of published history the rule has to
/// hold for: a driver whose published line crosses three major versions and
/// reaches the oldest cluster version the calendar carries, two add-ons with a
/// single-major history, one published by another Amazon Web Services service
/// team, and a vendor's own build sold through AWS Marketplace.
const ADDONS_WITH_PUBLISHED_BUILDS: &[&str] = &[
    "aws-efs-csi-driver",
    "vpc-cni",
    "cert-manager",
    "amazon-sagemaker-hyperpod-taskgovernance",
    "accuknox_kubearmor",
];

#[tokio::test]
async fn every_published_build_resolves_its_configuration_schema() {
    let router = emulator();

    for addon_name in ADDONS_WITH_PUBLISHED_BUILDS {
        let addon = catalog_entry(&router, addon_name).await;
        let builds = published_builds(&addon);
        assert!(
            !builds.is_empty(),
            "case {addon_name:?}: a published add-on has builds, got {builds:?}"
        );
        for addon_version in &builds {
            let case = format!("{addon_name}/{addon_version}");
            let answer = describe_addon_configuration(&router, addon_name, addon_version).await;
            assert_eq!(
                answer.status, 200,
                "case {case}: a build the catalog publishes has a published schema, got {answer:?}"
            );
            assert_eq!(
                string_member(&answer.body, "addonName"),
                *addon_name,
                "case {case}: expected the add-on the request named, got {:?}",
                answer.body["addonName"]
            );
            assert_eq!(
                string_member(&answer.body, "addonVersion"),
                *addon_version,
                "case {case}: expected the build the request named, got {:?}",
                answer.body["addonVersion"]
            );
            let document: serde_json::Value =
                serde_json::from_str(&string_member(&answer.body, "configurationSchema"))
                    .unwrap_or_else(|error| {
                        panic!("case {case}: expected a JSON Schema document, got {error}")
                    });
            assert!(
                document.get("$schema").is_some(),
                "case {case}: a published schema names its dialect, got {document:?}"
            );
            assert!(
                answer.body["podIdentityConfiguration"].is_array(),
                "case {case}: expected the service accounts the add-on runs as, got {:?}",
                answer.body["podIdentityConfiguration"]
            );
        }

        // A build the catalog publishes NO version of is the only miss: the pair
        // names a schema that does not exist.
        let answer = describe_addon_configuration(&router, addon_name, "v0.0.0-eksbuild.0").await;
        assert_eq!(
            answer.status, 404,
            "case {addon_name:?}/an unpublished build: expected HTTP 404, got {answer:?}"
        );
        assert_eq!(
            error_code(&answer),
            "ResourceNotFoundException",
            "case {addon_name:?}/an unpublished build: expected the not-found refusal, got \
             {answer:?}"
        );
    }

    // An add-on Amazon EKS has published builds of since the oldest row the
    // calendar carries publishes a build for that row — the earlier release
    // line — and that build resolves its schema like every other.
    let addon = catalog_entry(&router, "aws-efs-csi-driver").await;
    let oldest_supported: BTreeSet<String> = versions_of(&addon)
        .iter()
        .flat_map(compatibilities_of)
        .map(|entry| string_member(&entry, "clusterVersion"))
        .collect();
    assert!(
        oldest_supported.contains(ADDON_CATALOG_OLDEST_CLUSTER_VERSION),
        "case aws-efs-csi-driver: expected a build published for the oldest cluster version the \
         calendar carries ({ADDON_CATALOG_OLDEST_CLUSTER_VERSION}), got {oldest_supported:?}"
    );
    let for_the_oldest = describe_addon_versions(
        &router,
        &format!(
            "addonName=aws-efs-csi-driver&kubernetesVersion={ADDON_CATALOG_OLDEST_CLUSTER_VERSION}"
        ),
    )
    .await;
    assert_eq!(
        for_the_oldest.status, 200,
        "case the oldest cluster version: expected HTTP 200, got {for_the_oldest:?}"
    );
    let offered = addons(&for_the_oldest);
    assert_eq!(
        offered.len(),
        1,
        "case the oldest cluster version: expected the add-on to be offered on it, got {:?}",
        for_the_oldest.body["addons"]
    );
    for addon_version in published_builds(&offered[0]) {
        let answer =
            describe_addon_configuration(&router, "aws-efs-csi-driver", &addon_version).await;
        assert_eq!(
            answer.status, 200,
            "case aws-efs-csi-driver/{addon_version} on the oldest cluster version: expected a \
             published schema, got {answer:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE: `AddonInfo$type` and `$publisher` are the ADD-ON's own published values —
// an add-on another Amazon Web Services service team builds for its own service
// is published under that service's name and its own category — and the `types`
// and `publishers` filters narrow the catalog to exactly the add-ons carrying
// them.
// ---------------------------------------------------------------------------

/// The category and the publisher the catalog publishes for one add-on, as
/// `(addonName, type, publisher)`.
///
/// SOURCE: the publisher and category columns of the add-on tables the Amazon EKS
/// User Guide publishes — the add-ons Amazon EKS builds and curates, the ones
/// another Amazon Web Services service team builds for its own service (the
/// Amazon SageMaker HyperPod operators), and the independent-software-vendor
/// listings. The rows span all three kinds and more than one category per kind.
const EXPECTED_TAXONOMY: &[(&str, &str, &str)] = &[
    (
        "amazon-sagemaker-hyperpod-taskgovernance",
        ADDON_TYPE_ML_OPERATOR,
        ADDON_PUBLISHER_SAGEMAKER,
    ),
    (
        "amazon-sagemaker-hyperpod-training-operator",
        ADDON_TYPE_ML_OPERATOR,
        ADDON_PUBLISHER_SAGEMAKER,
    ),
    ("vpc-cni", "networking", ADDON_PUBLISHER_EKS),
    ("aws-efs-csi-driver", "storage", ADDON_PUBLISHER_EKS),
    ("cert-manager", "security", ADDON_PUBLISHER_EKS),
    ("accuknox_kubearmor", "security", "AccuKnox"),
    ("kubecost_kubecost", "cost-management", "Kubecost"),
];

#[tokio::test]
async fn an_addons_category_and_publisher_are_its_own_and_narrow_the_catalog() {
    let router = emulator();

    for (addon_name, addon_type, publisher) in EXPECTED_TAXONOMY {
        let addon = catalog_entry(&router, addon_name).await;
        for (member, expected) in [("type", *addon_type), ("publisher", *publisher)] {
            assert_eq!(
                string_member(&addon, member),
                expected,
                "case {addon_name:?}: expected the published {member} {expected:?}, got {:?}",
                addon[member]
            );
        }
    }

    // The filters read those same members, so each value the catalog publishes
    // narrows it to exactly the add-ons carrying that value — and a value nothing
    // is published under narrows it to nothing.
    let catalog = addons(&whole_catalog(&router).await);
    let named_by = |member: &str, value: &str| -> BTreeSet<String> {
        catalog
            .iter()
            .filter(|addon| string_member(addon, member) == value)
            .map(|addon| string_member(addon, "addonName"))
            .collect()
    };
    for (member, filter, value) in [
        ("publisher", "publishers", ADDON_PUBLISHER_SAGEMAKER),
        ("publisher", "publishers", ADDON_PUBLISHER_EKS),
        ("type", "types", ADDON_TYPE_ML_OPERATOR),
        ("type", "types", "storage"),
        ("publisher", "publishers", "not-a-publisher"),
        ("type", "types", "not-a-category"),
    ] {
        let expected = named_by(member, value);
        let answer = describe_addon_versions(&router, &format!("{filter}={value}")).await;
        assert_eq!(
            answer.status, 200,
            "case {filter}={value:?}: expected HTTP 200, got {answer:?}"
        );
        let reported: BTreeSet<String> = addons(&answer)
            .iter()
            .map(|addon| string_member(addon, "addonName"))
            .collect();
        assert_eq!(
            reported, expected,
            "case {filter}={value:?}: expected exactly the add-ons published with that {member}, \
             got {reported:?}"
        );
        if value.starts_with("not-a-") {
            assert!(
                reported.is_empty(),
                "case {filter}={value:?}: nothing is published under it, got {reported:?}"
            );
        } else {
            assert!(
                !reported.is_empty() && reported.len() < catalog.len(),
                "case {filter}={value:?}: a published value narrows the catalog to a strict \
                 non-empty subset, got {reported:?} of {} add-ons",
                catalog.len()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// RULE: each calendar row publishes its OWN support lifecycle — the day Amazon
// EKS released that Kubernetes version, the day standard support for it ends and
// the day extended support ends a year after that — so a client computing an
// upgrade deadline from a row reads that version's published days.
// ---------------------------------------------------------------------------

/// Calendar rows this case names, as `(version, release, end of standard
/// support, end of extended support)`.
///
/// SOURCE: the "Amazon EKS Kubernetes release calendar" of the Amazon EKS User
/// Guide, which lists those three days per minor version. The rows are versions
/// the other calendar suite does not name, so the rule is asserted beyond the
/// rows that suite already pins.
const NAMED_CALENDAR_ROWS: &[(&str, &str, &str, &str)] = &[
    ("1.33", "2025-05-29", "2026-07-29", "2027-07-29"),
    ("1.36", "2026-06-01", "2027-08-01", "2028-08-01"),
];

#[tokio::test]
async fn every_calendar_row_publishes_its_own_support_lifecycle() {
    let router = emulator();
    let calendar = published_calendar(&router).await;
    assert!(
        calendar.len() > 1,
        "setup: expected the published calendar, got {calendar:?}"
    );

    for row in &calendar {
        let case = string_member(row, "clusterVersion");
        let release = epoch_secs(&row["releaseDate"]);
        let standard = epoch_secs(&row["endOfStandardSupportDate"]);
        let extended = epoch_secs(&row["endOfExtendedSupportDate"]);
        assert!(
            release < standard && standard < extended,
            "case {case}: expected release < end of standard support < end of extended support, \
             got {release}, {standard} and {extended}"
        );
        // Standard support runs for more than a year from the release, and
        // extended support for exactly one further year — the policy the published
        // calendar follows, stated in days so a leap year is allowed for.
        assert!(
            standard - release > 365.0 * DAY,
            "case {case}: expected standard support to run past a year from the release, got \
             {} day(s)",
            (standard - release) / DAY
        );
        let extended_days = (extended - standard) / DAY;
        assert!(
            (365.0..=366.0).contains(&extended_days),
            "case {case}: expected extended support to run one year past standard support, got \
             {extended_days} day(s)"
        );
    }

    for (version, release, standard, extended) in NAMED_CALENDAR_ROWS {
        let row = calendar
            .iter()
            .find(|row| string_member(row, "clusterVersion") == *version)
            .unwrap_or_else(|| panic!("case {version}: the calendar must publish it"));
        for (member, expected) in [
            ("releaseDate", published_day(release)),
            ("endOfStandardSupportDate", published_day(standard)),
            ("endOfExtendedSupportDate", published_day(extended)),
        ] {
            assert_eq!(
                epoch_secs(&row[member]),
                expected,
                "case {version}: expected the published {member}, got {:?}",
                row[member]
            );
        }
    }
}

// ---------------------------------------------------------------------------
// RULE: the constraints a row publishes for a control-plane component parameter
// are the window a cluster of that version may be CONFIGURED in — narrower than
// the bound the model lets the member carry — and a scaling tier publishes the
// same window for the parameter it runs with, so a client that validates its
// configuration against the advertised constraints does not accept a value the
// service refuses.
// ---------------------------------------------------------------------------

/// The bound the model lets `terminatedPodGcThreshold` CARRY:
/// `TerminatedPodGcThresholdValue` is `@range {min: 1, max: 1000000}`
/// (`smithy_query(query="shape", shape="PodGcControllerConfigRequest")`).
const MODELLED_THRESHOLD_RANGE: (i64, i64) = (1, 1_000_000);

/// The published `terminatedPodGcThreshold` parameter of one component
/// configuration.
fn pod_gc_threshold(config: &serde_json::Value) -> &serde_json::Value {
    &config["kubeControllerManagerConfig"]["podGcControllerConfig"]["terminatedPodGcThreshold"]
}

/// One parameter's `(defaultValue, constraints.min, constraints.max)`.
fn window(case: &str, parameter: &serde_json::Value) -> (i64, i64, i64) {
    let read = |path: &[&str]| -> i64 {
        let mut value = parameter;
        for step in path {
            value = &value[*step];
        }
        value.as_i64().unwrap_or_else(|| {
            panic!("case {case}: expected an integer at {path:?}, got {parameter:?}")
        })
    };
    (
        read(&["defaultValue"]),
        read(&["constraints", "min"]),
        read(&["constraints", "max"]),
    )
}

#[tokio::test]
async fn the_published_parameter_window_is_the_one_a_cluster_may_be_configured_in() {
    let router = emulator();

    for row in published_calendar(&router).await {
        let version = string_member(&row, "clusterVersion");
        let published = window(
            &version,
            pod_gc_threshold(&row["controlPlaneComponentConfig"]),
        );
        let (default_value, min, max) = published;
        // A published window runs forwards and holds the parameter's default. Its
        // two ends may be the SAME value: a parameter Amazon EKS publishes no
        // alternative value for for a Kubernetes version is published as that one
        // value. WHICH ends the table states is published data, asserted in
        // `control_plane_component_constraints.rs`.
        assert!(
            min <= max,
            "case {version}: expected a window that runs forwards, got {min}..{max}"
        );
        assert!(
            min <= default_value && default_value <= max,
            "case {version}: the default must sit inside the published window, got \
             {default_value} in {min}..{max}"
        );
        // Narrower than what the member may carry at the wire: the model's range
        // is what the shape accepts, this window is what the service supports.
        assert!(
            min > MODELLED_THRESHOLD_RANGE.0 && max < MODELLED_THRESHOLD_RANGE.1,
            "case {version}: expected the published window to be narrower than the modelled \
             range {MODELLED_THRESHOLD_RANGE:?}, got {min}..{max}"
        );

        // Every scaling tier of the row publishes, for the parameter a control
        // plane on it runs with, the window that control plane may be configured
        // in. Only that parameter is asserted here: WHICH components a tier
        // overrides is the per-tier catalog content, not this rule.
        let tiers = row["controlPlaneScalingTiers"]
            .as_array()
            .unwrap_or_else(|| panic!("case {version}: expected the scaling tiers, got {row:?}"))
            .clone();
        assert!(
            !tiers.is_empty(),
            "case {version}: expected the provisioned scaling tiers, got {row:?}"
        );
        for tier in &tiers {
            let case = format!("{version}/{}", string_member(tier, "tierName"));
            let overrides = window(
                &case,
                pod_gc_threshold(&tier["controlPlaneComponentConfigOverrides"]),
            );
            assert_eq!(
                overrides, published,
                "case {case}: a tier publishes the window a control plane on it may be \
                 configured in for that parameter, got {overrides:?} against {published:?}"
            );
        }
    }
}

/// The answer of a read that must stay a read: asserting the catalog does not
/// move under a caller is what makes these reads reporting operations.
#[tokio::test]
async fn a_catalog_read_does_not_change_what_a_later_caller_sees() {
    let router = emulator();
    let first: Answer = whole_catalog(&router).await;
    let configuration = {
        let addon = catalog_entry(&router, "aws-efs-csi-driver").await;
        let addon_version = published_builds(&addon)
            .first()
            .cloned()
            .expect("a published build");
        describe_addon_configuration(&router, "aws-efs-csi-driver", &addon_version).await
    };
    assert_eq!(
        configuration.status, 200,
        "setup DescribeAddonConfiguration: expected HTTP 200, got {configuration:?}"
    );
    // And a refused read changes nothing either.
    let refused = call(
        &router,
        REGION,
        "GET",
        "/addons/configuration-schemas?addonName=aws-efs-csi-driver",
        "",
    )
    .await;
    assert_eq!(
        refused.status, 400,
        "setup the refused read: expected HTTP 400, got {refused:?}"
    );
    let second = whole_catalog(&router).await;
    assert_eq!(
        second.body, first.body,
        "case the published catalog: a read must not change what a later caller sees"
    );
}
