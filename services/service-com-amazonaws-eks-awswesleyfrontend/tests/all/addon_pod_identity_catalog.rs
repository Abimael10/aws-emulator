// SPDX-License-Identifier: CC0-1.0

//! The pod-identity configuration the add-on catalog publishes, read over the wire
//! through `DescribeAddonConfiguration` on the same `MultiServiceRouter` the
//! production binary mounts.
//!
//! The RULE under test: `DescribeAddonConfiguration` resolves one `(addonName,
//! addonVersion)` pair, so the `podIdentityConfiguration` it answers with is THAT
//! BUILD's — it names every Kubernetes service account that build's own workloads
//! call AWS as and supports EKS Pod Identity for, each entry with the AWS-managed
//! policies Amazon EKS recommends for that service account, and reports an EMPTY
//! list for a build whose add-on supports pod identity for none of its workloads —
//! an add-on that calls no AWS API of its own, or a build of a release line older
//! than the one its add-on gained pod identity support in. Pod-identity support is
//! never withdrawn, so an older build's entries are a subset of a newer build's.
//!
//! Three suites assert it:
//!
//! * a TABLE of the recommendations the catalog publishes at each add-on's newest
//!   build, whose rows differ along what the rule generalises over — how many
//!   service accounts an add-on runs as (none, one, two), how many policies are
//!   recommended for one of them (one, two), and the path depth of the policy ARNs
//!   (the account root and `/service-role/`);
//! * the version-keyed half of the rule, over the whole published build line of the
//!   one add-on whose published history reaches back before the releases its
//!   components gained pod identity support in;
//! * an invariant over the WHOLE catalog, every add-on the emulator publishes being a
//!   row: the entries of one build name distinct service accounts, every recommended
//!   policy is an ARN in AWS's own managed-policy namespace, and an older build of an
//!   add-on publishes a subset of what a newer build does, with the same policies.
//!
//! The harness is the crate's own (`wire.rs`), and the catalog readers are the ones
//! `addon_versions.rs` owns.

use std::collections::BTreeSet;

use smithy_server_runtime::MultiServiceRouter;

use crate::addon_catalog_publication::{resolved, schema_root};
use crate::addon_versions::{
    addons, describe_addon_configuration, string_member, versions_of, whole_catalog,
};
use crate::wire::emulator;

/// The prefix of an ARN in AWS's own managed-policy namespace.
const AWS_MANAGED_POLICY_PREFIX: &str = "arn:aws:iam::aws:policy/";

/// What one published add-on's `podIdentityConfiguration` must report at its NEWEST
/// build: the service accounts it runs as, in the order the catalog publishes them,
/// each with the managed policies recommended for it.
#[derive(Debug)]
struct Expected {
    addon: &'static str,
    pod_identity: &'static [(&'static str, &'static [&'static str])],
}

/// The recommendations the catalog publishes.
///
/// SOURCE: the "Amazon EKS add-ons" section of the Amazon EKS User Guide, which
/// lists for each add-on supporting EKS Pod Identity the Kubernetes service
/// accounts its workloads run as and the AWS-managed policy recommended for each —
/// the external authority this expectation is taken from, not the catalog under
/// test. Rows are written in add-on name order; the add-ons with no entry at all
/// are the tail of the table.
const EXPECTED: &[Expected] = &[
    Expected {
        // One service account, TWO recommended policies: the agent publishes both
        // metrics and traces.
        addon: "amazon-cloudwatch-observability",
        pod_identity: &[(
            "cloudwatch-agent",
            &[
                "arn:aws:iam::aws:policy/CloudWatchAgentServerPolicy",
                "arn:aws:iam::aws:policy/AWSXrayWriteOnlyAccess",
            ],
        )],
    },
    Expected {
        // An operator that calls AWS for the work it runs, so it carries the
        // managed policy Amazon Web Services publishes for it.
        addon: "amazon-sagemaker-hyperpod-training-operator",
        pod_identity: &[(
            "hp-training-controller-manager",
            &["arn:aws:iam::aws:policy/AmazonSageMakerHyperPodTrainingOperatorAccess"],
        )],
    },
    Expected {
        // The EBS driver calls AWS from its controller only, so one entry.
        addon: "aws-ebs-csi-driver",
        pod_identity: &[(
            "ebs-csi-controller-sa",
            &["arn:aws:iam::aws:policy/service-role/AmazonEBSCSIDriverPolicy"],
        )],
    },
    Expected {
        // The EFS driver calls Amazon EFS from BOTH components, so both of its
        // service accounts are named.
        addon: "aws-efs-csi-driver",
        pod_identity: &[
            (
                "efs-csi-controller-sa",
                &["arn:aws:iam::aws:policy/service-role/AmazonEFSCSIDriverPolicy"],
            ),
            (
                "efs-csi-node-sa",
                &["arn:aws:iam::aws:policy/service-role/AmazonEFSCSIDriverPolicy"],
            ),
        ],
    },
    Expected {
        addon: "aws-network-flow-monitoring-agent",
        pod_identity: &[(
            "aws-network-flow-monitor-agent",
            &["arn:aws:iam::aws:policy/CloudWatchNetworkFlowMonitorAgentPublishPolicy"],
        )],
    },
    Expected {
        addon: "external-dns",
        pod_identity: &[(
            "external-dns",
            &["arn:aws:iam::aws:policy/AmazonRoute53FullAccess"],
        )],
    },
    Expected {
        addon: "vpc-cni",
        pod_identity: &[("aws-node", &["arn:aws:iam::aws:policy/AmazonEKS_CNI_Policy"])],
    },
    // Add-ons whose workloads call no AWS API of their own: the list is empty, not
    // missing, and the published schema declares no service account default.
    Expected {
        addon: "coredns",
        pod_identity: &[],
    },
    Expected {
        addon: "eks-pod-identity-agent",
        pod_identity: &[],
    },
    Expected {
        addon: "kube-proxy",
        pod_identity: &[],
    },
];

/// One add-on's reported `podIdentityConfiguration`, as `(serviceAccount, policies)`
/// pairs in the order reported.
fn reported_pod_identity(
    body: &serde_json::Value,
    case: &str,
) -> Vec<(String, Vec<String>)> {
    body["podIdentityConfiguration"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: expected a podIdentityConfiguration list, got {body:?}"))
        .iter()
        .map(|entry| {
            let policies = entry["recommendedManagedPolicies"]
                .as_array()
                .unwrap_or_else(|| {
                    panic!("case {case}: expected a recommendedManagedPolicies list, got {entry:?}")
                })
                .iter()
                .map(|policy| {
                    policy
                        .as_str()
                        .unwrap_or_else(|| {
                            panic!("case {case}: expected a policy ARN string, got {policy:?}")
                        })
                        .to_string()
                })
                .collect();
            (string_member(entry, "serviceAccount"), policies)
        })
        .collect()
}

/// The newest build the catalog publishes for `addon_name` — the build whose
/// pod-identity configuration names every service account the add-on has gained pod
/// identity support for.
async fn newest_build(router: &MultiServiceRouter, addon_name: &str) -> String {
    let builds = published_builds(router, addon_name).await;
    builds
        .first()
        .unwrap_or_else(|| panic!("the catalog must publish a build of {addon_name}"))
        .clone()
}

/// Every build the catalog publishes for `addon_name`, newest first — the order
/// `DescribeAddonVersions` publishes them in.
async fn published_builds(router: &MultiServiceRouter, addon_name: &str) -> Vec<String> {
    let catalog = addons(&whole_catalog(router).await);
    let addon = catalog
        .iter()
        .find(|addon| string_member(addon, "addonName") == addon_name)
        .unwrap_or_else(|| panic!("the catalog must publish {addon_name}"));
    versions_of(addon)
        .iter()
        .map(|build| string_member(build, "addonVersion"))
        .collect()
}

/// The `(major, minor, patch)` release a `vX.Y.Z-eksbuild.N` add-on version names,
/// which is what orders one add-on's builds.
fn release_of(addon_version: &str) -> (u32, u32, u32) {
    let numbers: Vec<u32> = addon_version
        .trim_start_matches('v')
        .split('-')
        .next()
        .unwrap_or_default()
        .split('.')
        .filter_map(|part| part.parse().ok())
        .collect();
    match numbers.as_slice() {
        [major, minor, patch] => (*major, *minor, *patch),
        other => panic!("an add-on version is vX.Y.Z-eksbuild.N, got {addon_version:?} ({other:?})"),
    }
}

/// The `serviceAccount.name` default the published configuration schema declares, or
/// `None` when the schema declares no service account at all.
///
/// The document is a `$ref` into its own `definitions`, so both the root type and
/// the service-account type are read through the references that name them.
fn schema_service_account_default(body: &serde_json::Value, case: &str) -> Option<String> {
    let schema: serde_json::Value =
        serde_json::from_str(&string_member(body, "configurationSchema"))
            .unwrap_or_else(|error| panic!("case {case}: configurationSchema must be JSON: {error}"));
    let root = schema_root(&schema);
    let declared = &root["properties"]["serviceAccount"];
    if declared.is_null() {
        return None;
    }
    resolved(&schema, declared)["properties"]["name"]["default"]
        .as_str()
        .map(str::to_string)
}

#[tokio::test]
async fn every_service_account_an_addon_runs_as_is_published_with_its_policies() {
    let router = emulator();

    for expected in EXPECTED {
        // The newest build, which is the one that names every service account the
        // add-on has gained pod identity support for; the builds that gained one
        // later are the next suite's subject.
        let build = newest_build(&router, expected.addon).await;
        let case = format!("{}/{build}", expected.addon);
        let answer = describe_addon_configuration(&router, expected.addon, &build).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected HTTP 200, got {answer:?}"
        );

        let reported = reported_pod_identity(&answer.body, &case);
        let reported_accounts: Vec<&str> = reported
            .iter()
            .map(|(account, _)| account.as_str())
            .collect();
        let expected_accounts: Vec<&str> = expected
            .pod_identity
            .iter()
            .map(|(account, _)| *account)
            .collect();
        assert_eq!(
            reported_accounts, expected_accounts,
            "case {case}: expected the service accounts the add-on runs as, got \
             {reported_accounts:?}"
        );
        for ((account, policies), (_, expected_policies)) in
            reported.iter().zip(expected.pod_identity)
        {
            let policies: Vec<&str> = policies.iter().map(String::as_str).collect();
            assert_eq!(
                policies.as_slice(),
                *expected_policies,
                "case {case}/{account}: expected the policies recommended for it, got \
                 {policies:?}"
            );
        }

        // The schema's service account default is the add-on's FIRST published
        // entry, and an add-on that runs as none declares no default: a second
        // entry adds a recommendation, it does not change the install default.
        let default = schema_service_account_default(&answer.body, &case);
        assert_eq!(
            default.as_deref(),
            expected_accounts.first().copied(),
            "case {case}: expected the first published service account as the schema \
             default, got {default:?}"
        );
    }
}

/// One service account an add-on runs as, with the add-on release it is published
/// from: the minimum build of that add-on which can be given a pod identity for
/// that component.
type SupportedFrom = (&'static str, (u32, u32, u32));

/// The add-on whose published build line reaches back before the releases its
/// components gained EKS Pod Identity support in.
///
/// SOURCE: the minimum add-on version the Amazon EKS User Guide publishes for each
/// component of that add-on that supports EKS Pod Identity — the EFS CSI driver's
/// controller Deployment from the driver's 2.0.7 release and its node DaemonSet
/// from the 2.1 line — which is the external authority this expectation is taken
/// from, not the catalog under test. A build older than both publishes nothing at
/// all: pod identity is not something that build can be installed with.
const POD_IDENTITY_ADDON: &str = "aws-efs-csi-driver";

/// The releases that add-on's service accounts are published from, in the order
/// the catalog publishes the accounts. See [`POD_IDENTITY_ADDON`].
const POD_IDENTITY_FROM: &[SupportedFrom] = &[
    ("efs-csi-controller-sa", (2, 0, 7)),
    ("efs-csi-node-sa", (2, 1, 0)),
];

#[tokio::test]
async fn a_build_publishes_the_service_accounts_it_supports_pod_identity_for() {
    let router = emulator();
    let (addon, supported) = (POD_IDENTITY_ADDON, POD_IDENTITY_FROM);

    // Every build of the published line is a row, so the rows span every side of
    // those releases and both published bounds — the oldest build the add-on has
    // and its newest.
    let builds = published_builds(&router, addon).await;
    assert!(
        builds.len() > 1,
        "setup: the catalog must publish several builds of {addon}, got {builds:?}"
    );
    let mut published: BTreeSet<usize> = BTreeSet::new();
    for build in &builds {
        let case = format!("{addon}/{build}");
        let answer = describe_addon_configuration(&router, addon, build).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected HTTP 200, got {answer:?}"
        );
        let reported = reported_pod_identity(&answer.body, &case);
        let accounts: Vec<&str> = reported
            .iter()
            .map(|(account, _)| account.as_str())
            .collect();
        let expected: Vec<&str> = supported
            .iter()
            .filter(|(_, first_build)| release_of(build) >= *first_build)
            .map(|(account, _)| *account)
            .collect();
        published.insert(expected.len());
        assert_eq!(
            accounts, expected,
            "case {case}: a build publishes the service accounts it supports pod \
             identity for, got {accounts:?}"
        );
    }
    assert_eq!(
        published,
        (0..=supported.len()).collect::<BTreeSet<usize>>(),
        "setup: the published line must cross every release {supported:?} an account is \
         published from, so some of its builds publish none, some all"
    );
}

#[tokio::test]
async fn the_pod_identity_configuration_of_an_older_build_is_a_subset() {
    let router = emulator();
    let catalog = addons(&whole_catalog(&router).await);
    assert!(
        !catalog.is_empty(),
        "setup: the catalog must publish add-ons, got {catalog:?}"
    );

    for addon in &catalog {
        let addon_name = string_member(addon, "addonName");
        // The builds come newest first, so each one that follows is older than the
        // one before it and may publish no more than it does.
        let mut newer: Option<Vec<(String, Vec<String>)>> = None;
        for build in versions_of(addon) {
            let addon_version = string_member(&build, "addonVersion");
            let case = format!("{addon_name}/{addon_version}");
            let answer = describe_addon_configuration(&router, &addon_name, &addon_version).await;
            assert_eq!(
                answer.status, 200,
                "case {case}: expected HTTP 200, got {answer:?}"
            );
            let reported = reported_pod_identity(&answer.body, &case);

            let accounts: BTreeSet<&str> = reported
                .iter()
                .map(|(account, _)| account.as_str())
                .collect();
            assert_eq!(
                accounts.len(),
                reported.len(),
                "case {case}: one service account is named at most once, got {reported:?}"
            );
            for (account, policies) in &reported {
                assert!(
                    !policies.is_empty(),
                    "case {case}/{account}: a published service account carries the policies \
                     recommended for it, got {policies:?}"
                );
                for policy in policies {
                    assert!(
                        policy.starts_with(AWS_MANAGED_POLICY_PREFIX)
                            && policy.len() > AWS_MANAGED_POLICY_PREFIX.len(),
                        "case {case}/{account}: a recommended policy is an AWS-managed policy \
                         ARN, got {policy:?}"
                    );
                }
            }

            if let Some(newer) = &newer {
                // Pod identity support is never withdrawn, so this older build
                // publishes a subset of the newer one's entries, and an entry it
                // does publish recommends exactly what the newer build does.
                for entry in &reported {
                    assert!(
                        newer.contains(entry),
                        "case {case}: an older build publishes a subset of what a newer \
                         build does, got {entry:?} against {newer:?}"
                    );
                }
            }
            newer = Some(reported);
        }
    }
}
