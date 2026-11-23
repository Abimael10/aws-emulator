// SPDX-License-Identifier: CC0-1.0

//! What the Kubernetes API server a cluster publishes answers `/version` with.
//!
//! The RULE under test: `/version` answers with the Kubernetes version object of
//! the BUILD serving the cluster, and that build is the one the control plane says
//! the cluster runs. `gitVersion` is "the semantic version of the binary" — the
//! member every Kubernetes client parses to decide what the server supports — so on
//! Amazon EKS it is the patch-level EKS build string
//! `v<major>.<minor>.<patch>-eks-<build>`: the leading `v`, the dots and the
//! `-eks-` marker are fixed, the major and minor are the cluster's own version (the
//! one `DescribeCluster` reports), and the patch and the build id come from the
//! control plane — the patch level `DescribeClusterVersions` publishes for that
//! version. The document carries the rest of the build's identity beside it, and a
//! `platform` that is the operating-system/architecture pair of the server
//! answering.
//!
//! The suites drive that over a TABLE whose rows differ along what the rule
//! generalises over: the Kubernetes version the cluster was created at — several
//! rows of the published calendar, including one the issue behind this suite never
//! cited — and a cluster created at no version at all, which runs the default.
//!
//! The client is the raw, kubeconfig-built one `kubernetes_api.rs` dials with.

use crate::certificate_authority::REGION;
use crate::kubernetes_api::{dial, kubeconfig_of, poll_until_active, ApiAnswer, Kubeconfig};
use crate::wire::{call, emulator_with_engine, Answer};
use smithy_server_runtime::MultiServiceRouter;

/// The name Go — and therefore every Kubernetes client that parses `platform` —
/// gives the architecture this test process runs on: the `GOARCH` spelling of it,
/// which is what the pair's second half has to be to mean anything to a client.
fn go_architecture() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// Create the cluster `name`, at `version` when a row names one, and read the
/// kubeconfig members back the way a client does.
async fn cluster_at_version(
    router: &MultiServiceRouter,
    name: &str,
    version: Option<&str>,
) -> Kubeconfig {
    let mut body = serde_json::json!({
        "name": name,
        "roleArn": "arn:aws:iam::000000000000:role/eksClusterRole",
        "resourcesVpcConfig": { "subnetIds": ["subnet-aaaa1111", "subnet-bbbb2222"] },
    });
    if let Some(version) = version {
        body["version"] = serde_json::json!(version);
    }
    let created = call(router, REGION, "POST", "/clusters", &body.to_string()).await;
    assert_eq!(
        created.status, 200,
        "setup {name:?}: expected 200 from CreateCluster, got {created:?}"
    );
    let described = poll_until_active(router, REGION, name).await;
    kubeconfig_of(name, &described)
}

/// The calendar rows `DescribeClusterVersions` publishes, which is where the patch
/// level of a Kubernetes version comes from.
async fn published_calendar(router: &MultiServiceRouter) -> Vec<serde_json::Value> {
    let answer: Answer = call(router, REGION, "GET", "/cluster-versions", "").await;
    assert_eq!(
        answer.status, 200,
        "setup: expected 200 from DescribeClusterVersions, got {answer:?}"
    );
    let rows = answer.body["clusterVersions"]
        .as_array()
        .unwrap_or_else(|| panic!("setup: expected a clusterVersions list, got {answer:?}"))
        .clone();
    assert!(
        !rows.is_empty(),
        "setup: the calendar must publish a version a cluster can be created at, got {answer:?}"
    );
    rows
}

/// One row of the version table: the cluster version a case creates at (and `None`
/// for the cluster that names none), with the patch level and release day the
/// calendar publishes for the version it ends up running.
struct VersionCase {
    label: String,
    created_at_version: Option<String>,
    expected_version: String,
    expected_patch: String,
    released_at: Option<f64>,
}

/// The table: the OLDEST and the NEWEST version the calendar offers, the DEFAULT
/// one, and a cluster created at no version at all — so the rule is driven over
/// versions the issue behind this suite never cited as well as the one it did.
async fn version_cases(router: &MultiServiceRouter) -> Vec<VersionCase> {
    let rows = published_calendar(router).await;
    let member = |row: &serde_json::Value, name: &str| -> String {
        row[name]
            .as_str()
            .unwrap_or_else(|| panic!("setup: expected a {name} on {row:?}"))
            .to_string()
    };
    let default_row = rows
        .iter()
        .find(|row| row["defaultVersion"] == serde_json::json!(true))
        .unwrap_or_else(|| panic!("setup: one row is the default version, got {rows:?}"));
    let mut cases: Vec<VersionCase> = Vec::new();
    for (label, row) in [
        ("the newest version the calendar offers", rows.first()),
        ("the oldest version the calendar offers", rows.last()),
        ("the default version", Some(default_row)),
    ] {
        let Some(row) = row else { continue };
        let version = member(row, "clusterVersion");
        cases.push(VersionCase {
            label: format!("{label} ({version})"),
            created_at_version: Some(version.clone()),
            expected_patch: member(row, "kubernetesPatchVersion"),
            released_at: row["releaseDate"].as_f64(),
            expected_version: version,
        });
    }
    let default_version = member(default_row, "clusterVersion");
    cases.push(VersionCase {
        label: "a cluster created at no version at all".to_string(),
        created_at_version: None,
        expected_patch: member(default_row, "kubernetesPatchVersion"),
        released_at: default_row["releaseDate"].as_f64(),
        expected_version: default_version,
    });
    cases
}

/// `GET /version` on the cluster's published address.
async fn served_version(config: &Kubeconfig, case: &str) -> ApiAnswer {
    let answered = dial(config, case, "GET", "/version", None).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: the published address must serve /version, got {answered:?}"
    );
    answered
}

/// The string member `name` of a served version document.
fn member(case: &str, document: &serde_json::Value, name: &str) -> String {
    document[name]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the version member {name:?}, got {document:?}")
        })
        .to_string()
}

/// RULE: `/version` reports the patch-level EKS build of the Kubernetes version the
/// control plane says the cluster runs.
#[tokio::test]
async fn the_served_version_is_the_patch_level_eks_build_of_the_cluster_version() {
    let (router, _engine) = emulator_with_engine();
    let cases = version_cases(&router).await;

    for (index, case) in cases.iter().enumerate() {
        let label = case.label.as_str();
        let cluster = format!("served-version-{index}");
        let config =
            cluster_at_version(&router, &cluster, case.created_at_version.as_deref()).await;
        let document = served_version(&config, label).await.body;

        // The two components of the cluster's own version, as the document reports
        // them.
        let mut expected = case.expected_version.split('.');
        let (major, minor) = (
            expected.next().unwrap_or_default(),
            expected.next().unwrap_or_default(),
        );
        assert_eq!(
            member(label, &document, "major"),
            major,
            "case {label:?}: expected the cluster's own major version, got {document:?}"
        );
        assert_eq!(
            member(label, &document, "minor"),
            minor,
            "case {label:?}: expected the cluster's own minor version, got {document:?}"
        );

        // `gitVersion` is the patch-level build string: the patch version the
        // control plane publishes for this version, with the `-eks-<build>` suffix
        // every Amazon EKS build of Kubernetes carries.
        let git_version = member(label, &document, "gitVersion");
        let build = git_version
            .strip_prefix(&format!("v{}-eks-", case.expected_patch))
            .unwrap_or_else(|| {
                panic!(
                    "case {label:?}: expected v{}-eks-<build>, got {git_version:?}",
                    case.expected_patch
                )
            });
        assert!(
            !build.is_empty() && build.chars().all(|digit| digit.is_ascii_hexdigit()),
            "case {label:?}: the build id is a hexadecimal object name, got {git_version:?}"
        );
        // The patch level is a THREE-component version, which is what a client
        // parsing `gitVersion` as a semantic version needs.
        assert_eq!(
            case.expected_patch.split('.').count(),
            3,
            "case {label:?}: the control plane publishes a patch-level version, got {:?}",
            case.expected_patch
        );
        assert!(
            git_version.starts_with(&format!("v{}.{}.", major, minor)),
            "case {label:?}: the served build is a {major}.{minor} patch release, got \
             {git_version:?}"
        );

        // The rest of the build's identity, and the platform the server answering
        // runs on.
        let commit = member(label, &document, "gitCommit");
        assert!(
            commit.len() == 40 && commit.chars().all(|digit| digit.is_ascii_hexdigit()),
            "case {label:?}: expected a 40-digit git object name, got {commit:?}"
        );
        assert_eq!(
            member(label, &document, "gitTreeState"),
            "clean",
            "case {label:?}: a published build is made from a clean tree, got {document:?}"
        );
        let platform = member(label, &document, "platform");
        assert_eq!(
            platform,
            format!("{}/{}", std::env::consts::OS, go_architecture()),
            "case {label:?}: expected the OS/arch pair of the serving process, got {platform:?}"
        );

        // The build date is the day Amazon EKS released the version, as the
        // calendar publishes it.
        if let Some(released_at) = case.released_at {
            let expected_day = aws_smithy_types::DateTime::from_secs_f64(released_at)
                .fmt(aws_smithy_types::date_time::Format::DateTime)
                .unwrap_or_else(|error| panic!("case {label:?}: the release day formats: {error}"));
            assert_eq!(
                member(label, &document, "buildDate"),
                expected_day,
                "case {label:?}: expected the release day of {}, got {document:?}",
                case.expected_version
            );
        }
    }
}

/// RULE: the served build is the CLUSTER's: two reads of one cluster report the
/// same document, and two clusters on different versions report different builds.
#[tokio::test]
async fn the_served_build_is_stable_per_cluster_and_differs_between_versions() {
    let (router, _engine) = emulator_with_engine();
    let rows = published_calendar(&router).await;
    let versions: Vec<String> = rows
        .iter()
        .filter_map(|row| row["clusterVersion"].as_str().map(str::to_string))
        .collect();
    assert!(
        versions.len() >= 2,
        "setup: the calendar offers more than one version, got {versions:?}"
    );

    let mut served: Vec<(String, serde_json::Value)> = Vec::new();
    for (index, version) in versions.iter().take(3).enumerate() {
        let cluster = format!("stable-version-{index}");
        let config = cluster_at_version(&router, &cluster, Some(version)).await;
        let first = served_version(&config, version).await.body;
        let again = served_version(&config, version).await.body;
        assert_eq!(
            first, again,
            "case {version:?}: every read of one cluster reports one version document, got \
             {first:?} then {again:?}"
        );
        served.push((version.clone(), first));
    }

    for (version, document) in &served {
        for (other_version, other) in &served {
            if version == other_version {
                continue;
            }
            assert_ne!(
                document["gitVersion"], other["gitVersion"],
                "case {version:?} against {other_version:?}: two Kubernetes versions are two \
                 builds, got {:?} and {:?}",
                document["gitVersion"], other["gitVersion"]
            );
        }
    }
}
