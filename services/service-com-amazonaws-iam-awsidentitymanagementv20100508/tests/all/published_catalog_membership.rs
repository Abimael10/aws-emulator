// SPDX-License-Identifier: CC0-1.0

//! Membership of AWS's own managed-policy catalog, read through `ListPolicies` with
//! `Scope=AWS` — WHICH policies a path prefix enumerates, and the publication data the
//! members of that page carry.
//!
//! ONE rule, asserted here: a policy in AWS's own namespace is published partition data, so
//! for every managed path prefix the enumeration of that prefix reports each policy AWS
//! publishes THERE, and reports it with that policy's own `PolicyId`, `DefaultVersionId`,
//! `CreateDate` and `UpdateDate`. A policy absent from the enumeration cannot be discovered
//! at all, which is what separates membership from the per-member data: the data of a member
//! can be stale, a missing member leaves a client with no candidate where AWS gives it one.
//!
//! What the sibling suites own, and this one deliberately does not repeat: the per-entry
//! invariants of the catalog (`tests/all/published_catalog_constants.rs`,
//! `tests/all/published_catalog_breadth.rs`) and the family-by-family enumeration
//! (`tests/all/published_policy_families.rs`). What is asserted here is MEMBERSHIP and the
//! published value of a member as the LIST page a client enumerates reports it.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: the path
//! prefix the family is published under, the entry inside that family, the member being
//! reported, and the kind of member (an id, a version, an instant).
//!
//! The awsQuery harness (`router`, `call`, `element`) is the one `tests/all/user_lifecycle.rs`
//! owns and the page walk is the one `tests/all/published_policy_families.rs` owns.

use ::std::collections::BTreeMap;

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state;

use crate::published_policy_families::walk_policies;
use crate::user_lifecycle::{call, element, error_code, router};

/// The managed path prefixes AWS publishes a policy under, beside the account root.
///
/// SOURCE: the AWS Managed Policy Reference, whose families are the account root, the path
/// reserved for a service-linked role's own policy, the job functions, the privileged root
/// tasks and the service roles.
const MANAGED_PREFIXES: &[&str] = &[
    "/",
    "/aws-service-role/",
    "/job-function/",
    "/root-task/",
    "/service-role/",
];

/// Policies AWS publishes that a client discovers by enumerating a prefix: `(path, name)`.
///
/// SOURCE: the AWS Managed Policy Reference, which files each of these under the path named
/// here. Written out by hand rather than read off the catalog, so a row that the catalog
/// stops carrying — or carries under another path — fails this suite. The rows span three
/// managed families and include entries no recorded trace of this project reads
/// (`AWSIoTRuleActions`, `AmazonEMRCleanupPolicy`, `AWSQuickSightListIAM`) beside the one a
/// recorded `ListPolicies` page is sharpest about (`AWSLambdaRole`).
const PUBLISHED_MEMBERSHIP: &[(&str, &str)] = &[
    ("/aws-service-role/", "AWSArtifactServiceRolePolicy"),
    ("/aws-service-role/", "AWSCloudFrontLogger"),
    ("/aws-service-role/", "AWSEC2FleetServiceRolePolicy"),
    (
        "/aws-service-role/",
        "AWSElasticLoadBalancingServiceRolePolicy",
    ),
    ("/aws-service-role/", "AWSLambdaServiceRolePolicy"),
    ("/aws-service-role/", "AccessAnalyzerServiceRolePolicy"),
    ("/aws-service-role/", "AmazonEMRCleanupPolicy"),
    ("/aws-service-role/", "AmazonFSxServiceRolePolicy"),
    ("/service-role/", "AWSBackupServiceRolePolicyForRestores"),
    ("/service-role/", "AWSCodeDeployRoleForLambda"),
    ("/service-role/", "AWSDataLifecycleManagerServiceRole"),
    ("/service-role/", "AWSFaultInjectionSimulatorEC2Access"),
    ("/service-role/", "AWSIoTRuleActions"),
    ("/service-role/", "AWSLambdaRole"),
    ("/service-role/", "AWSQuickSightListIAM"),
    ("/service-role/", "AmazonS3ObjectLambdaExecutionRolePolicy"),
    ("/service-role/", "ServerMigrationServiceLaunchRole"),
    ("/job-function/", "SupportUser"),
    ("/root-task/", "IAMDeleteRootUserCredentials"),
];

/// Published members of the catalog, written out literally: `(path, name, member, value)`.
///
/// SOURCE: the AWS Managed Policy Reference and the recorded AWS `ListPolicies` answers for
/// the catalog pages, which publish these ARNs' ids, default versions and instants. One row
/// per member, covering all three kinds of member (an id, a version, an instant) across four
/// families, read here through the LIST page rather than through `GetPolicy` — the page is
/// what a client enumerating the catalog sees.
const PUBLISHED_MEMBERS: &[(&str, &str, &str, &str)] = &[
    (
        "/aws-service-role/",
        "AmazonElasticsearchServiceRolePolicy",
        "PolicyId",
        "ANPAJFEWZPHXKLCVHEUIC",
    ),
    (
        "/aws-service-role/",
        "AWSEC2SpotServiceRolePolicy",
        "DefaultVersionId",
        "v4",
    ),
    (
        "/aws-service-role/",
        "AWSElasticBeanstalkServiceRolePolicy",
        "CreateDate",
        "2017-09-13T23:46:37Z",
    ),
    (
        "/job-function/",
        "ViewOnlyAccess",
        "PolicyId",
        "ANPAID22R6XPJATWOFDK6",
    ),
    (
        "/job-function/",
        "ViewOnlyAccess",
        "DefaultVersionId",
        "v46",
    ),
    (
        "/job-function/",
        "SupportUser",
        "PolicyId",
        "ANPAI3V4GSSN5SJY3P2RO",
    ),
    ("/job-function/", "SupportUser", "DefaultVersionId", "v11"),
    (
        "/job-function/",
        "SystemAdministrator",
        "DefaultVersionId",
        "v12",
    ),
    (
        "/root-task/",
        "IAMCreateRootUserPassword",
        "PolicyId",
        "ANPAZKAPJZG4OEGTN7YT5",
    ),
    (
        "/root-task/",
        "IAMDeleteRootUserCredentials",
        "PolicyId",
        "ANPAZKAPJZG4CX73XKORQ",
    ),
    (
        "/root-task/",
        "S3UnlockBucketPolicy",
        "CreateDate",
        "2024-11-06T21:55:56Z",
    ),
    (
        "/root-task/",
        "S3UnlockBucketPolicy",
        "UpdateDate",
        "2024-11-06T21:55:56Z",
    ),
    (
        "/service-role/",
        "AWSLambdaRole",
        "PolicyId",
        "ANPAJX4DPCRGTC4NFDUXI",
    ),
    ("/service-role/", "AWSLambdaRole", "DefaultVersionId", "v1"),
    (
        "/service-role/",
        "AWSLambdaRole",
        "CreateDate",
        "2015-02-06T18:41:28Z",
    ),
    (
        "/service-role/",
        "AWSLambdaRole",
        "UpdateDate",
        "2015-02-06T18:41:28Z",
    ),
    (
        "/service-role/",
        "AmazonEC2RoleforDataPipelineRole",
        "PolicyId",
        "ANPAJ3Z5I2WAJE5DN2J36",
    ),
    (
        "/service-role/",
        "AmazonEC2RoleforDataPipelineRole",
        "CreateDate",
        "2015-02-06T18:41:25Z",
    ),
    (
        "/service-role/",
        "AmazonElasticTranscoderRole",
        "PolicyId",
        "ANPAJNW3WMKVXFJ2KPIQ2",
    ),
    (
        "/service-role/",
        "AmazonElasticTranscoderRole",
        "UpdateDate",
        "2019-06-13T22:48:22Z",
    ),
    (
        "/service-role/",
        "RDSCloudHsmAuthorizationRole",
        "UpdateDate",
        "2019-09-26T22:14:29Z",
    ),
];

/// The ARN AWS publishes a managed policy under: `arn:aws:iam::aws:policy<path><name>`.
fn published_arn(path: &str, policy_name: &str) -> String {
    format!("arn:aws:iam::aws:policy{path}{policy_name}")
}

/// One member of every policy the `Scope=AWS` enumeration of `path` reports, keyed by ARN.
///
/// Walked ONCE per `(path, member)` and reused by every row of a table, with a page size small
/// enough that the walk really pages. Both walks run over the same request, so the pages come
/// back in the same order and the two member lists line up position by position.
async fn page_member_by_arn(
    router: &MultiServiceRouter,
    path: &str,
    member: &str,
) -> BTreeMap<String, String> {
    let case = format!("the Scope=AWS page of {path} read for <{member}>");
    let params = [("Scope", "AWS"), ("PathPrefix", path), ("MaxItems", "4")];
    let arns = walk_policies(router, &case, &params, "Arn").await;
    let values = walk_policies(router, &case, &params, member).await;
    assert_eq!(
        arns.len(),
        values.len(),
        "case {case:?}: every member of the page carries <{member}>, got {} ARNs and {} values",
        arns.len(),
        values.len()
    );
    let page: BTreeMap<String, String> = arns.into_iter().zip(values).collect();
    assert!(
        !page.is_empty(),
        "case {case:?}: the enumeration of a published family reports members"
    );
    page
}

/// RULE: the `Scope=AWS` enumeration of a managed path prefix reports every policy AWS
/// publishes under that prefix, so a client that discovers a policy by listing the catalog
/// finds it — and the record the listing reports is the one `GetPolicy` serves for the same
/// ARN.
#[tokio::test]
async fn every_published_policy_is_a_member_of_its_prefix_page() {
    let router = router();

    for prefix in MANAGED_PREFIXES {
        let rows: Vec<&(&str, &str)> = PUBLISHED_MEMBERSHIP
            .iter()
            .filter(|(path, _)| path == prefix)
            .collect();
        if rows.is_empty() {
            continue;
        }
        let listed = page_member_by_arn(&router, prefix, "PolicyId").await;

        for (path, policy_name) in rows {
            let arn = published_arn(path, policy_name);
            let case = format!("{policy_name} at {path}");
            let listed_policy_id = listed.get(&arn).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the Scope=AWS enumeration of {path:?} must report {arn:?}, \
                     got {:?}",
                    listed.keys().collect::<Vec<_>>()
                )
            });

            let (status, body) = call(&router, "GetPolicy", &[("PolicyArn", arn.as_str())]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: GetPolicy must serve a published policy, got {status}: {body}"
            );
            assert_eq!(
                element(&body, "Path"),
                *path,
                "case {case:?}: expected the published path, got {body}"
            );
            assert_eq!(
                element(&body, "PolicyName"),
                *policy_name,
                "case {case:?}: expected the published friendly name, got {body}"
            );
            assert_eq!(
                element(&body, "IsAttachable"),
                "true",
                "case {case:?}: AWS publishes every policy of its catalog as attachable, got \
                 {body}"
            );
            assert_eq!(
                &element(&body, "PolicyId"),
                listed_policy_id,
                "case {case:?}: the listing and the read must report ONE id for the policy, got \
                 {body}"
            );
            for reported in ["PolicyId", "DefaultVersionId", "CreateDate", "UpdateDate"] {
                assert!(
                    !element(&body, reported).is_empty(),
                    "case {case:?}: a published policy carries <{reported}>, got {body}"
                );
            }
        }
    }
}

/// RULE: a published member is the value AWS publishes for that ARN, reported by the page the
/// client enumerates — an id, a default version and an instant alike.
#[tokio::test]
async fn published_members_are_reported_by_the_prefix_page() {
    let router = router();

    let mut pages: BTreeMap<(&str, &str), BTreeMap<String, String>> = BTreeMap::new();
    for (path, policy_name, member, value) in PUBLISHED_MEMBERS {
        let arn = published_arn(path, policy_name);
        let case = format!("{arn} <{member}>");
        let key = (*path, *member);
        if let ::std::collections::btree_map::Entry::Vacant(vacant) = pages.entry(key) {
            let page = page_member_by_arn(&router, path, member).await;
            vacant.insert(page);
        }
        let reported = pages[&key]
            .get(&arn)
            .unwrap_or_else(|| panic!("case {case:?}: the page must report this policy"));
        assert_eq!(
            reported, value,
            "case {case:?}: expected the value AWS publishes, got {reported:?}"
        );
    }
}

/// RULE: a published policy's identity is its path AND its friendly name, so the name of a
/// policy the catalog carries, spelled under any other managed prefix, names NO policy — the
/// missing entity every read of a `PolicyArn` is refused as.
#[tokio::test]
async fn a_carried_name_under_another_prefix_names_no_policy() {
    let router = router();

    // Rows: a name the catalog carries, paired with each managed prefix that is NOT the one
    // AWS publishes it under.
    for (path, policy_name) in PUBLISHED_MEMBERSHIP {
        for other in MANAGED_PREFIXES.iter().filter(|other| *other != path) {
            let arn = published_arn(other, policy_name);
            let case = format!("{policy_name} spelled under {other}");
            assert!(
                state::aws_managed_policy(&arn).is_none(),
                "case {case:?}: the catalog publishes this name under {path:?} only"
            );
            let (status, body) = call(&router, "GetPolicy", &[("PolicyArn", arn.as_str())]).await;
            assert_eq!(
                status, 404,
                "case {case:?}: expected 404, got {status}: {body}"
            );
            assert_eq!(
                error_code(&body),
                "NoSuchEntity",
                "case {case:?}: expected the modelled missing entity, got {body}"
            );
        }
    }
}
