// SPDX-License-Identifier: CC0-1.0

//! The REPLICA COUNT an add-on is configured with, from the document the catalog
//! publishes to the workload the cluster runs.
//!
//! The RULE under test: `DescribeAddonConfiguration` publishes the add-on's OWN
//! document — its keys declared where that add-on's document declares them, and a
//! key whose value the build SHIPS carrying that value as its JSON Schema
//! `default` — and an install's `configurationValues` is what the cluster runs, so
//! the replica count a client reads out of the document is the one its workload
//! runs until the client asks for another.
//!
//! The suites drive that over TABLES whose rows differ along what the rule
//! generalises over: the add-on (so the FORM its published document takes, the
//! root-declared one and the `$ref`-into-definitions one), the replica count an
//! install asks for (none, one, several, the published bound itself and one past
//! it, which the document refuses), and the form the install's own document is
//! written in (JSON and YAML, both of which this service reads).
//!
//! The clients are the shared ones: the wire harness in `tests/all/wire.rs` for the
//! control plane and the kubeconfig-built dialler in `tests/all/kubernetes_api.rs`
//! for the cluster's own API server.

use std::time::{Duration, Instant};

use service_com_amazonaws_eks_awswesleyfrontend::state::kubernetes_workloads::MAX_REPLICAS_PER_WORKLOAD;
use smithy_server_runtime::MultiServiceRouter;

use crate::addon_catalog_publication::{configuration_schema, schema_root};
use crate::addon_lifecycle::{create_addon, describe_addon};
use crate::addon_versions::{addons, describe_addon_versions, string_member, versions_of};
use crate::certificate_authority::REGION;
use crate::identity_provider_config_not_found::active_cluster_in;
use crate::kubernetes_api::{dial, Kubeconfig};
use crate::kubernetes_cluster_resources::{
    active_nodegroup, instance, published_cluster, register_nodes,
};
use crate::wire::{call, emulator, encode_label, error_code, read_until};

/// How long a polled condition is given before the case fails. Generous: the spans
/// waited out here are the service's own and nothing measures elapsed time.
const DEADLINE: Duration = Duration::from_secs(60);

/// How often a polled condition is looked at again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The namespace the add-on under test is installed into.
const KUBE_SYSTEM: &str = "kube-system";

/// The add-on whose chart exposes a replica count: the metrics server, whose
/// published document declares that count at its root.
const REPLICATED_ADDON: &str = "metrics-server";

/// The newest build the catalog publishes of one add-on.
async fn newest_build(router: &MultiServiceRouter, addon_name: &str) -> String {
    let answer =
        describe_addon_versions(router, &format!("addonName={}", encode_label(addon_name))).await;
    assert_eq!(
        answer.status, 200,
        "setup DescribeAddonVersions({addon_name}): expected HTTP 200, got {answer:?}"
    );
    let listed = addons(&answer);
    let addon = listed
        .first()
        .unwrap_or_else(|| panic!("setup {addon_name}: the catalog must publish it"));
    let builds = versions_of(addon);
    let build = builds
        .first()
        .unwrap_or_else(|| panic!("setup {addon_name}: expected a published build"));
    string_member(build, "addonVersion")
}

/// The keys the document a client validates against declares, read the way a
/// client reads them: the `properties` of the type the document's root stands for.
fn declared_properties(document: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    schema_root(document)["properties"]
        .as_object()
        .cloned()
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// RULE: the published document declares the add-on's keys where that add-on's own
// document declares them — at the ROOT for the add-ons whose documents are
// published root-declared, in `definitions` for the ones whose documents reference
// their own types — and a key whose value the build ships carries that value as
// its `default`.
// ---------------------------------------------------------------------------

/// Which shape an add-on's published configuration document takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocumentForm {
    /// A root `$ref` into the document's own `definitions`.
    Definitions,
    /// The add-on's keys declared at the document root.
    Root,
}

/// The form each add-on's published document takes, and one key it declares.
///
/// SOURCE: the document `aws eks describe-addon-configuration --addon-name <name>
/// --addon-version <build>` reports for the add-on. The rows carry both forms, and
/// more than one add-on of each: the form is the DOCUMENT's own, so two add-ons
/// whose documents declare the same dialect can take different forms (the metrics
/// server and `external-dns` are both published in the draft-07 dialect).
const PUBLISHED_FORMS: &[(&str, DocumentForm, &str)] = &[
    (REPLICATED_ADDON, DocumentForm::Root, "replicas"),
    ("aws-efs-csi-driver", DocumentForm::Root, "useFIPS"),
    ("external-dns", DocumentForm::Definitions, "resources"),
    ("vpc-cni", DocumentForm::Definitions, "eniConfig"),
    ("coredns", DocumentForm::Definitions, "corefile"),
];

#[tokio::test]
async fn the_published_document_declares_its_keys_in_the_form_the_addon_publishes() {
    let router = emulator();

    for (addon_name, form, key) in PUBLISHED_FORMS {
        let addon_version = newest_build(&router, addon_name).await;
        let case = format!("{addon_name}/{addon_version}");
        let document = configuration_schema(&router, addon_name, &addon_version).await;
        let reported = if document["$ref"].is_string() {
            DocumentForm::Definitions
        } else {
            DocumentForm::Root
        };
        assert_eq!(
            reported, *form,
            "case {case}: expected the form this add-on's document is published in, got \
             {document:?}"
        );
        if *form == DocumentForm::Root {
            assert!(
                document["definitions"].is_null(),
                "case {case}: a document that declares its keys at the root carries no \
                 definitions section, got {:?}",
                document["definitions"]
            );
            // A client reads the keys off the document's own `properties`, so a
            // root-declared document must carry them there rather than only in a
            // type it references.
            assert!(
                document["properties"][*key].is_object(),
                "case {case}: expected the key {key:?} among the document's own properties, \
                 got {:?}",
                document["properties"]
            );
        }
        assert!(
            declared_properties(&document).contains_key(*key),
            "case {case}: expected the add-on's own key {key:?} to be declared, got {document:?}"
        );
    }
}

#[tokio::test]
async fn the_replica_count_key_declares_the_count_the_build_ships() {
    let router = emulator();
    let addon_version = newest_build(&router, REPLICATED_ADDON).await;
    let case = format!("{REPLICATED_ADDON}/{addon_version}");
    let document = configuration_schema(&router, REPLICATED_ADDON, &addon_version).await;
    let properties = declared_properties(&document);

    // EXACTLY ONE of the keys the document declares is an integer carrying a
    // default: the replica count the build ships, which is what a client reads the
    // shipped count out of the document by.
    let candidates: Vec<String> = properties
        .iter()
        .filter(|(_, declared)| {
            declared["type"] == serde_json::json!("integer") && !declared["default"].is_null()
        })
        .map(|(key, _)| key.clone())
        .collect();
    assert_eq!(
        candidates.len(),
        1,
        "case {case}: expected exactly one integer key carrying a default among the declared \
         keys {:?}, got {candidates:?}",
        properties.keys().collect::<Vec<&String>>()
    );
    let declared = &properties[&candidates[0]];
    let shipped = declared["default"].as_u64().unwrap_or_else(|| {
        panic!("case {case}: the default of {:?} must be a count, got {declared:?}", candidates[0])
    });
    assert!(
        shipped >= 1,
        "case {case}: an installed add-on runs at least one replica, got {shipped}"
    );
    assert_eq!(
        candidates[0], "replicas",
        "case {case}: the key carrying the shipped count is the one an install asks for \
         replicas through, got {candidates:?}"
    );
    // And the document states the MOST an install may set it to, which is what an
    // install asking for more is refused against: the shipped count is within it.
    assert_eq!(
        declared["maximum"],
        serde_json::json!(MAX_REPLICAS_PER_WORKLOAD),
        "case {case}: expected the replica key to declare the most one workload runs, got \
         {declared:?}"
    );
    assert!(
        shipped <= MAX_REPLICAS_PER_WORKLOAD,
        "case {case}: the shipped count must be one an install may ask for, got {shipped}"
    );
}

// ---------------------------------------------------------------------------
// RULE: a `configurationValues` document is validated against the document the
// catalog publishes, so a replica count within the declared `maximum` is installed
// and one past it is refused, naming the path at fault and the bound.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_replica_count_past_the_published_maximum_is_refused() {
    let router = emulator();
    let cluster = "addon-replica-bound";
    active_cluster_in(&router, REGION, cluster).await;

    // The rows are the published bound itself and one past it, with a count well
    // inside it as the control: a bound is tested at its edge and beyond it. The
    // over-bound count is sent in BOTH forms an add-on configuration may be written
    // in, because the document is what the install is held to whichever form it
    // arrived in.
    let rows: &[(&str, String, u16)] = &[
        (
            "a count well inside the bound, in JSON",
            serde_json::json!({ "replicas": 2 }).to_string(),
            200,
        ),
        (
            "the published bound itself, in JSON",
            serde_json::json!({ "replicas": MAX_REPLICAS_PER_WORKLOAD }).to_string(),
            200,
        ),
        (
            "one past the published bound, in JSON",
            serde_json::json!({ "replicas": MAX_REPLICAS_PER_WORKLOAD + 1 }).to_string(),
            400,
        ),
        (
            "one past the published bound, in YAML",
            format!("replicas: {}\n", MAX_REPLICAS_PER_WORKLOAD + 1),
            400,
        ),
    ];
    for (case, values, expected) in rows {
        let request = serde_json::json!({
            "addonName": REPLICATED_ADDON,
            "configurationValues": values,
        });
        let answer = create_addon(&router, cluster, &request).await;
        assert_eq!(
            answer.status, *expected,
            "case {case:?}: expected HTTP {expected}, got {answer:?}"
        );
        if *expected == 400 {
            assert_eq!(
                error_code(&answer),
                "InvalidParameterException",
                "case {case:?}: expected the modelled refusal, got {answer:?}"
            );
            let message = answer.body["message"].as_str().unwrap_or_default();
            assert!(
                message.contains("$.replicas")
                    && message.contains(&MAX_REPLICAS_PER_WORKLOAD.to_string()),
                "case {case:?}: the refusal names the path at fault and the bound, got \
                 {message:?}"
            );
        } else {
            // The install that was accepted is removed again, so the next row
            // installs into a cluster that holds no add-on of this name.
            let deleted = call(
                &router,
                REGION,
                "DELETE",
                &format!(
                    "/clusters/{}/addons/{}",
                    encode_label(cluster),
                    encode_label(REPLICATED_ADDON)
                ),
                "",
            )
            .await;
            assert_eq!(
                deleted.status, 200,
                "case {case:?}: expected 200 from DeleteAddon, got {deleted:?}"
            );
            read_until(
                case,
                "the removed add-on left the cluster",
                || describe_addon(&router, cluster, REPLICATED_ADDON),
                |answer| answer.status == 404,
            )
            .await;
        }
    }
}

// ---------------------------------------------------------------------------
// RULE: an install's `configurationValues` is what the cluster runs, so the
// replica count a document asks for is the count the add-on's workload runs — and
// an install that asks for none runs the count the build ships, which is the
// `default` the published document declares.
// ---------------------------------------------------------------------------

/// The Deployment the add-on owns, read through the cluster's own API server.
async fn addon_deployment(
    config: &Kubeconfig,
    case: &str,
    workload: &str,
) -> serde_json::Value {
    let path = format!("/apis/apps/v1/namespaces/{KUBE_SYSTEM}/deployments/{workload}");
    let deadline = Instant::now() + DEADLINE;
    loop {
        let read = dial(config, case, "GET", &path, None).await;
        if read.status == 200 {
            return read.body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the installed add-on must run its workload, got {read:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// `DescribeAddon` until the add-on reports a status in which its resources have
/// been applied, or the deadline passes.
async fn poll_until_installed(router: &MultiServiceRouter, cluster: &str, addon: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = describe_addon(router, cluster, addon).await;
        let status = described.body["addon"]["status"].clone();
        if status == serde_json::json!("ACTIVE") || status == serde_json::json!("DEGRADED") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "add-on {addon:?}: the clock owes it an installed status, got {described:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Remove the add-on and wait until the workload it owned is gone, so the next row
/// installs into a cluster that holds neither.
async fn remove_addon(
    router: &MultiServiceRouter,
    config: &Kubeconfig,
    cluster: &str,
    addon: &str,
) {
    let deleted = call(
        router,
        REGION,
        "DELETE",
        &format!(
            "/clusters/{}/addons/{}",
            encode_label(cluster),
            encode_label(addon)
        ),
        "",
    )
    .await;
    assert_eq!(
        deleted.status, 200,
        "case {addon:?}: expected 200 from DeleteAddon, got {deleted:?}"
    );
    let path = format!("/apis/apps/v1/namespaces/{KUBE_SYSTEM}/deployments/{addon}");
    let deadline = Instant::now() + DEADLINE;
    loop {
        let read = dial(config, addon, "GET", &path, None).await;
        let described = describe_addon(router, cluster, addon).await;
        if read.status == 404 && described.status == 404 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {addon:?}: the removal must take the add-on and its workload away, got \
             {read:?} and {described:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn the_configured_replica_count_is_what_the_addons_workload_runs() {
    let cluster = "addon-replicas";
    let (router, engine, config) = published_cluster(cluster).await;
    active_nodegroup(&router, cluster, "workers").await;
    register_nodes(
        &engine,
        cluster,
        "workers",
        &[
            instance("i-0aaa1111aaa1111aa", "us-west-2a", "t3.medium", "10.0.1.11"),
            instance("i-0bbb2222bbb2222bb", "us-west-2b", "t3.medium", "10.0.2.12"),
        ],
    );

    // The count the published document declares as shipped, which is what an
    // install that asks for none runs.
    let addon_version = newest_build(&router, REPLICATED_ADDON).await;
    let document = configuration_schema(&router, REPLICATED_ADDON, &addon_version).await;
    let shipped = declared_properties(&document)["replicas"]["default"]
        .as_u64()
        .unwrap_or_else(|| panic!("setup: the document declares the shipped replica count"));

    // Rows: the `configurationValues` document the install carries, and the count
    // the workload must then run. They differ along the count (none named, one,
    // several) and along the FORM the document is written in — JSON and the YAML an
    // add-on configuration may equally be written in — while a count past what the
    // document allows is refused rather than installed
    // ([`a_replica_count_past_the_published_maximum_is_refused`]).
    let rows: &[(&str, Option<&str>, u64)] = &[
        ("no configuration values", None, shipped),
        ("a single replica, in JSON", Some("{\"replicas\": 1}"), 1),
        ("several replicas, in JSON", Some("{\"replicas\": 3}"), 3),
        ("several replicas, in YAML", Some("replicas: 2\n"), 2),
    ];

    for (case, values, expected) in rows {
        let mut request = serde_json::json!({ "addonName": REPLICATED_ADDON });
        if let Some(values) = values {
            request["configurationValues"] = serde_json::json!(values);
        }
        let created = create_addon(&router, cluster, &request).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected 200 from CreateAddon, got {created:?}"
        );
        poll_until_installed(&router, cluster, REPLICATED_ADDON).await;

        let workload = addon_deployment(&config, case, REPLICATED_ADDON).await;
        assert_eq!(
            workload["spec"]["replicas"],
            serde_json::json!(expected),
            "case {case:?}: expected the workload to run {expected} replicas, got {:?}",
            workload["spec"]
        );
        remove_addon(&router, &config, cluster, REPLICATED_ADDON).await;
    }
}
