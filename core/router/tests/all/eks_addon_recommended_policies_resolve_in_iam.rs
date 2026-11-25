// SPDX-License-Identifier: CC0-1.0

//! The managed policies `DescribeAddonConfiguration` recommends for an add-on's
//! Kubernetes service accounts, followed into IAM — the service that has to answer for
//! every ARN the add-on catalog hands an installer.
//!
//! RULE under test: `podIdentityConfiguration` names, per service account the build's
//! workloads call AWS as, exactly the AWS-managed policies Amazon Web Services
//! publishes as the recommendation for that add-on — no policy beside them, and the
//! CURRENT one where AWS has superseded an older policy of the same family — and each
//! ARN it names resolves in IAM: `GetPolicy` reports it at the path its ARN spells, as
//! attachable, and the version that read names serves a document granting the actions
//! the add-on's workloads make. An extra recommendation widens the IAM surface an
//! installer builds beyond what AWS asks for, and a superseded one points at a
//! different policy, at a different path, than the real service does.
//!
//! It lives here rather than in the EKS crate because only the assembled emulator can
//! answer the second half: the recommendation is Amazon EKS's to publish and the policy
//! behind it is IAM's to report, so the ARN is a claim about another service's catalog.
//! The recommendation sets alone are asserted crate-locally by
//! `services/service-com-amazonaws-eks-awswesleyfrontend/tests/all/addon_pod_identity_catalog.rs`,
//! and the readability of a published ARN by
//! `services/service-com-amazonaws-iam-awsidentitymanagementv20100508/tests/all/published_managed_policy_reads.rs`.
//!
//! The rows differ along what the rule generalises over: how many service accounts an
//! add-on runs as (none, one, two), the path the recommended policy is published under
//! (the account root and `/service-role/`), the AWS service the granted actions belong
//! to (CloudWatch and X-Ray, Amazon EC2, Amazon EFS), and whether the add-on is one the
//! issue cited. Each build is resolved the way a client does it — the account's default
//! Kubernetes release, then the build that release marks as default — so no row names a
//! recorded version string.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and
//! the requests driven — on a sized thread. The request harness is the test crate's
//! own: `eks_wire` for the EKS and IAM drivers, `query_wire` for the XML readers.

use std::collections::BTreeSet;
use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use smithy_server_runtime::MultiServiceRouter;

use crate::eks_wire::{eks, iam, Answer};
use crate::query_wire::element;

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The prefix of an ARN in AWS's own managed-policy namespace, which is where every
/// recommendation of the add-on catalog points.
const AWS_MANAGED_POLICY_PREFIX: &str = "arn:aws:iam::aws:policy/";

/// One service account of an add-on, with the policies recommended for it and the
/// actions those policies must grant.
struct Recommendation {
    /// The Kubernetes service account the add-on's workload runs as.
    service_account: &'static str,
    /// The ARNs recommended for it, in the order the catalog publishes them.
    policies: &'static [&'static str],
    /// Actions the add-on's workloads call AWS with, which the recommended policies
    /// must therefore grant between them.
    ///
    /// SOURCE: the AWS Managed Policy Reference entry for each policy named above,
    /// read for the actions the add-on's own components make — the external authority
    /// this expectation comes from, not the catalog or the IAM entry under test.
    granted: &'static [&'static str],
}

/// One add-on: the recommendations its default build publishes.
struct Row {
    case: &'static str,
    addon: &'static str,
    pod_identity: &'static [Recommendation],
}

const ROWS: &[Row] = &[
    Row {
        // Cited by the issue: the agent publishes metrics, logs AND traces, and the
        // ONE policy AWS publishes for a CloudWatch agent carries all three, so
        // nothing is recommended beside it.
        case: "one service account, one root-path policy carrying metrics, logs and traces",
        addon: "amazon-cloudwatch-observability",
        pod_identity: &[Recommendation {
            service_account: "cloudwatch-agent",
            policies: &["arn:aws:iam::aws:policy/CloudWatchAgentServerPolicy"],
            granted: &[
                "cloudwatch:PutMetricData",
                "logs:CreateLogGroup",
                "logs:PutLogEvents",
                "xray:PutTraceSegments",
            ],
        }],
    },
    Row {
        // Cited by the issue: the policy published for the driver now, at the account
        // root, rather than the superseded one under `/service-role/`.
        case: "one service account, the current root-path policy of a superseded family",
        addon: "aws-ebs-csi-driver",
        pod_identity: &[Recommendation {
            service_account: "ebs-csi-controller-sa",
            policies: &["arn:aws:iam::aws:policy/AmazonEBSCSIDriverPolicyV2"],
            granted: &[
                "ec2:AttachVolume",
                "ec2:CreateSnapshot",
                "ec2:CreateVolume",
                "ec2:DeleteSnapshot",
                "ec2:DeleteVolume",
                "ec2:DetachVolume",
            ],
        }],
    },
    Row {
        // Cited by no issue: TWO service accounts, both recommending one policy
        // published under `/service-role/`, so the row spans the other path depth.
        case: "two service accounts, both recommending a service-role-path policy",
        addon: "aws-efs-csi-driver",
        pod_identity: &[
            Recommendation {
                service_account: "efs-csi-controller-sa",
                policies: &["arn:aws:iam::aws:policy/service-role/AmazonEFSCSIDriverPolicy"],
                granted: &[
                    "elasticfilesystem:CreateAccessPoint",
                    "elasticfilesystem:DeleteAccessPoint",
                    "elasticfilesystem:DescribeAccessPoints",
                ],
            },
            Recommendation {
                service_account: "efs-csi-node-sa",
                policies: &["arn:aws:iam::aws:policy/service-role/AmazonEFSCSIDriverPolicy"],
                granted: &["elasticfilesystem:DescribeMountTargets"],
            },
        ],
    },
    Row {
        // Cited by no issue: the networking add-on Amazon EKS builds itself.
        case: "one service account, a root-path policy of network-interface actions",
        addon: "vpc-cni",
        pod_identity: &[Recommendation {
            service_account: "aws-node",
            policies: &["arn:aws:iam::aws:policy/AmazonEKS_CNI_Policy"],
            granted: &[
                "ec2:AssignPrivateIpAddresses",
                "ec2:CreateNetworkInterface",
                "ec2:DeleteNetworkInterface",
            ],
        }],
    },
    Row {
        // An add-on whose workloads call no AWS API of their own: the list is empty,
        // so there is no ARN for IAM to answer for at all.
        case: "an add-on that calls no AWS API, recommending nothing",
        addon: "coredns",
        pod_identity: &[],
    },
];

/// The account's default Kubernetes release — the version a cluster created without
/// one lands on, and so the release whose default add-on builds a client installs.
fn default_cluster_version(router: &MultiServiceRouter, runtime: &tokio::runtime::Runtime) -> String {
    let answer = eks(router, runtime, "GET", "/cluster-versions?defaultOnly=true", None);
    assert_eq!(
        answer.status, 200,
        "setup: DescribeClusterVersions(defaultOnly) must be served, got {answer:?}"
    );
    let rows = answer.body["clusterVersions"]
        .as_array()
        .unwrap_or_else(|| panic!("setup: expected a clusterVersions list, got {}", answer.text))
        .clone();
    let row = rows
        .first()
        .unwrap_or_else(|| panic!("setup: expected the default release's row, got {rows:?}"));
    row["clusterVersion"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a clusterVersion string, got {row:?}"))
        .to_string()
}

/// The build `DescribeAddonVersions` marks as the DEFAULT one for `release` — the
/// build `CreateAddon` installs when the caller names no version.
fn default_build(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    addon: &str,
    release: &str,
) -> String {
    let answer = eks(
        router,
        runtime,
        "GET",
        &format!("/addons/supported-versions?addonName={addon}&kubernetesVersion={release}"),
        None,
    );
    assert_eq!(
        answer.status, 200,
        "case {addon:?}: DescribeAddonVersions must be served, got {answer:?}"
    );
    let addons = answer.body["addons"]
        .as_array()
        .unwrap_or_else(|| panic!("case {addon:?}: expected an addons list, got {}", answer.text))
        .clone();
    let published = addons
        .iter()
        .find(|entry| entry["addonName"].as_str() == Some(addon))
        .unwrap_or_else(|| panic!("case {addon:?}: the catalog must publish this add-on, got {addons:?}"));
    let builds = published["addonVersions"]
        .as_array()
        .unwrap_or_else(|| panic!("case {addon:?}: expected an addonVersions list, got {published:?}"))
        .clone();
    let default = builds
        .iter()
        .find(|build| {
            build["compatibilities"]
                .as_array()
                .is_some_and(|compatibilities| {
                    compatibilities.iter().any(|compatibility| {
                        compatibility["clusterVersion"].as_str() == Some(release)
                            && compatibility["defaultVersion"].as_bool() == Some(true)
                    })
                })
        })
        .unwrap_or_else(|| {
            panic!("case {addon:?}: one build must be the default for {release}, got {builds:?}")
        });
    default["addonVersion"]
        .as_str()
        .unwrap_or_else(|| panic!("case {addon:?}: expected an addonVersion string, got {default:?}"))
        .to_string()
}

/// The `podIdentityConfiguration` one build publishes, as `(serviceAccount, policies)`
/// pairs in the order reported.
fn reported_pod_identity(answer: &Answer, case: &str) -> Vec<(String, Vec<String>)> {
    answer.body["podIdentityConfiguration"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a podIdentityConfiguration list, got {}", answer.text)
        })
        .iter()
        .map(|entry| {
            let account = entry["serviceAccount"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a serviceAccount, got {entry:?}"))
                .to_string();
            let policies = entry["recommendedManagedPolicies"]
                .as_array()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected a recommendedManagedPolicies list, got {entry:?}")
                })
                .iter()
                .map(|policy| {
                    policy
                        .as_str()
                        .unwrap_or_else(|| panic!("case {case:?}: expected a policy ARN, got {policy:?}"))
                        .to_string()
                })
                .collect();
            (account, policies)
        })
        .collect()
}

/// The path an AWS-managed policy's ARN spells: everything from the first `/` after
/// `:policy` through the last one.
fn path_of(arn: &str) -> &str {
    let tail = arn.split_once(":policy").expect("a managed-policy ARN").1;
    &tail[..tail.rfind('/').expect("a path separator") + 1]
}

/// The friendly name an AWS-managed policy's ARN spells.
fn name_of(arn: &str) -> &str {
    arn.rsplit('/').next().expect("a non-empty ARN")
}

/// Every action the statements of an IAM policy document allow, flattened over the
/// statements and over the string-or-list form each `Action` takes.
fn allowed_actions(document: &serde_json::Value, case: &str) -> BTreeSet<String> {
    let statements = document["Statement"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a policy document carries a Statement list, got {document}"))
        .clone();
    let mut actions: BTreeSet<String> = BTreeSet::new();
    for statement in &statements {
        if statement["Effect"].as_str() != Some("Allow") {
            continue;
        }
        match &statement["Action"] {
            serde_json::Value::String(action) => {
                actions.insert(action.clone());
            }
            serde_json::Value::Array(named) => {
                for action in named {
                    if let Some(action) = action.as_str() {
                        actions.insert(action.to_string());
                    }
                }
            }
            other => panic!("case {case:?}: a statement names the actions it grants, got {other}"),
        }
    }
    actions
}

/// Follow one recommended ARN into IAM: the policy must resolve where its ARN says it
/// lives, be attachable, and serve a document granting `granted` at the version the
/// read names.
fn assert_resolves_in_iam(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    arn: &str,
    granted: &[&str],
) {
    let answer = iam(router, runtime, "GetPolicy", &[("PolicyArn", arn)]);
    assert_eq!(
        answer.status, 200,
        "case {case:?}: IAM must report the policy {arn} the add-on catalog recommends, got \
         {answer:?}"
    );
    assert_eq!(
        element(&answer.text, "Arn"),
        arn,
        "case {case:?}: IAM must report the ARN asked for, got {}",
        answer.text
    );
    assert_eq!(
        element(&answer.text, "Path"),
        path_of(arn),
        "case {case:?}: the policy must be published at the path its ARN spells, got {}",
        answer.text
    );
    assert_eq!(
        element(&answer.text, "PolicyName"),
        name_of(arn),
        "case {case:?}: the policy must carry the friendly name its ARN spells, got {}",
        answer.text
    );
    assert_eq!(
        element(&answer.text, "IsAttachable"),
        "true",
        "case {case:?}: a policy recommended for a pod identity must be attachable, got {}",
        answer.text
    );

    let default_version = element(&answer.text, "DefaultVersionId");
    let version = iam(
        router,
        runtime,
        "GetPolicyVersion",
        &[("PolicyArn", arn), ("VersionId", default_version.as_str())],
    );
    assert_eq!(
        version.status, 200,
        "case {case:?}: IAM must serve {arn} at the version {default_version} it named, got \
         {version:?}"
    );
    // IAM reports a policy document percent-encoded, which a client decodes back to
    // plain JSON text.
    let decoded = ::smithy_server_runtime::percent_decode(&element(&version.text, "Document"));
    let document: serde_json::Value = serde_json::from_str(&decoded)
        .unwrap_or_else(|error| panic!("case {case:?}: the served document must be JSON ({error}): {decoded}"));
    let actions = allowed_actions(&document, case);
    for action in granted {
        assert!(
            actions.contains(*action),
            "case {case:?}: {arn} must grant {action}, got {actions:?}"
        );
    }
}

#[test]
fn every_recommended_addon_policy_is_the_published_one_and_resolves_in_iam() {
    std::thread::Builder::new()
        .name("eks-addon-recommended-policies".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
                .expect("a current-thread runtime");

            let release = default_cluster_version(&router, &runtime);
            let mut resolved: BTreeSet<String> = BTreeSet::new();

            for row in ROWS {
                let build = default_build(&router, &runtime, row.addon, &release);
                let case = format!("{} [{}/{build}]", row.case, row.addon);

                let answer = eks(
                    &router,
                    &runtime,
                    "GET",
                    &format!(
                        "/addons/configuration-schemas?addonName={}&addonVersion={build}",
                        row.addon
                    ),
                    None,
                );
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: DescribeAddonConfiguration must be served, got {answer:?} \
                     ({})",
                    answer.message()
                );

                let reported = reported_pod_identity(&answer, &case);
                let expected: Vec<(String, Vec<String>)> = row
                    .pod_identity
                    .iter()
                    .map(|recommendation| {
                        (
                            recommendation.service_account.to_string(),
                            recommendation
                                .policies
                                .iter()
                                .map(|arn| (*arn).to_string())
                                .collect(),
                        )
                    })
                    .collect();
                assert_eq!(
                    reported, expected,
                    "case {case:?}: expected the service accounts the build runs as, each with \
                     exactly the policies AWS publishes for it, got {reported:?}"
                );

                for recommendation in row.pod_identity {
                    for arn in recommendation.policies {
                        assert!(
                            arn.starts_with(AWS_MANAGED_POLICY_PREFIX)
                                && arn.len() > AWS_MANAGED_POLICY_PREFIX.len(),
                            "case {case:?}: a recommendation is an ARN in AWS's own \
                             managed-policy namespace, got {arn:?}"
                        );
                        assert_resolves_in_iam(
                            &router,
                            &runtime,
                            &format!("{case} / {}", recommendation.service_account),
                            arn,
                            recommendation.granted,
                        );
                        resolved.insert((*arn).to_string());
                    }
                }
            }

            // The table spans more than one policy and more than one published path,
            // so neither half of the rule is asserted on a single ARN.
            assert!(
                resolved.len() >= 4,
                "the table must follow several recommended policies into IAM, got {resolved:?}"
            );
            let paths: BTreeSet<&str> = resolved.iter().map(|arn| path_of(arn)).collect();
            assert_eq!(
                paths,
                BTreeSet::from(["/", "/service-role/"]),
                "the table must span both paths an attachable AWS-managed policy is published \
                 under, got {paths:?}"
            );
        })
        .expect("spawn the eks addon recommended policies thread")
        .join()
        .expect("the eks addon recommended policies thread");
}
