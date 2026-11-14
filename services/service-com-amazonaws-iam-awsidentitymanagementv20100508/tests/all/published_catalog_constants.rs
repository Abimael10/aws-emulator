// SPDX-License-Identifier: CC0-1.0

//! The publication data a policy AWS OWNS is reported from: its `PolicyId`, the version it
//! is published at, and the instants it was published and last revised.
//!
//! ONE rule, asserted here: a policy in AWS's own namespace (`arn:aws:iam::aws:policy/…`) is
//! not account state, so everything a reader reports about it is a CONSTANT of the published
//! catalog — the same value for the same ARN in every account, in every Region and in every
//! emulator instance, and never a value minted when the catalog was first read. The account's
//! own policy store is the opposite: a policy this account creates is minted now, with an id
//! out of the account's own id space.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: the family
//! the policy is published under (all five paths), the entry inside that family (including
//! entries no recorded trace reads), the member being reported, the Region the request is
//! signed for, the emulator instance serving it, and the page size an enumeration is walked
//! at.
//!
//! The awsQuery harness (`router`, `call`, `element`, `elements`) is the one
//! `tests/all/user_lifecycle.rs` owns and every suite in this crate shares.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state;

use crate::published_policy_families::walk_policies;
use crate::user_lifecycle::{
    call, call_signed_in_region, document_element, element, error_code, router,
};

/// A document the account's own `CreatePolicy` is driven with, for the rows that separate a
/// minted record from a published one.
const ACCOUNT_DOCUMENT: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"}]}"#;

/// Regions a request is signed for. A published catalog entry is owned by AWS rather than by
/// a Region's copy of the account, so the same ARN reports the same data in each of them.
const REGIONS: &[&str] = &["us-east-1", "eu-west-1", "ap-southeast-2"];

/// Page sizes an enumeration of a family is walked at, plus the walk that names no page size.
const PAGE_SIZES: &[Option<&str>] = &[None, Some("1"), Some("3"), Some("20")];

/// Catalog entries spanning every family AWS publishes, with the entries no recorded trace
/// reads named beside the ones it does.
///
/// SOURCE: the AWS Managed Policy Reference, which files each of these under the path named
/// here. Written out rather than read off the catalog, so an entry that moved or disappeared
/// fails this suite.
const ENTRIES: &[(&str, &str)] = &[
    // Named by no recorded trace: the account-wide and job-function families, and a
    // service-linked-role policy.
    ("/", "AdministratorAccess"),
    ("/", "CloudWatchAgentServerPolicy"),
    ("/aws-service-role/", "AmazonGuardDutyServiceRolePolicy"),
    ("/job-function/", "DatabaseAdministrator"),
    // The root-task family, published in one AWS change.
    ("/root-task/", "IAMAuditRootUserCredentials"),
    ("/root-task/", "IAMCreateRootUserPassword"),
    ("/root-task/", "IAMDeleteRootUserCredentials"),
    ("/root-task/", "S3UnlockBucketPolicy"),
    ("/root-task/", "SQSUnlockQueuePolicy"),
    // Service-role entries published decades apart, so the rows differ in their version and
    // in both instants.
    ("/service-role/", "AWSCloudHSMRole"),
    ("/service-role/", "AWSLambdaKinesisExecutionRole"),
    ("/service-role/", "AmazonEFSCSIDriverPolicy"),
    ("/service-role/", "AmazonRDSEnhancedMonitoringRole"),
];

/// The policies AWS publishes under `/service-role/` that this catalog carries — the family a
/// client enumerates to find the grant a service's own role is published with.
///
/// SOURCE: the AWS Managed Policy Reference's `/service-role/` family. AWS's own family is
/// larger than any emulator carries, so this is the set that must be ENUMERABLE here: a name
/// on it that the enumeration does not report is a policy a client cannot discover.
const SERVICE_ROLE_FAMILY: &[&str] = &[
    "AWSBatchServiceRole",
    "AWSCloudHSMRole",
    "AWSCodeDeployRole",
    "AWSConfigRulesExecutionRole",
    "AWSGlueServiceRole",
    "AWSLambdaBasicExecutionRole",
    "AWSLambdaDynamoDBExecutionRole",
    "AWSLambdaKinesisExecutionRole",
    "AWSLambdaVPCAccessExecutionRole",
    "AmazonDMSVPCManagementRole",
    "AmazonEBSCSIDriverPolicy",
    "AmazonEC2ContainerServiceRole",
    "AmazonEC2RoleforSSM",
    "AmazonECSTaskExecutionRolePolicy",
    "AmazonEFSCSIDriverPolicy",
    "AmazonElasticMapReduceRole",
    "AmazonElasticMapReduceforEC2Role",
    "AmazonElasticTranscoderRole",
    "AmazonMacieServiceRole",
    "AmazonRDSEnhancedMonitoringRole",
    "AutoScalingNotificationAccessRole",
    "CloudWatchEventsInvocationAccess",
];

/// Members of the published catalog written out literally, so a seed regression is caught
/// here and not only against the seed itself.
///
/// SOURCE: the recorded AWS `ListPolicies` answers for the catalog pages, which publish
/// these ARNs' ids, default versions and instants. One row per `(path, name, member,
/// value)`, covering both families this suite is about and both the id and the instant
/// kinds of member.
const PUBLISHED_MEMBERS: &[(&str, &str, &str, &str)] = &[
    (
        "/root-task/",
        "IAMAuditRootUserCredentials",
        "PolicyId",
        "ANPAZKAPJZG4BDSCREYVS",
    ),
    (
        "/root-task/",
        "S3UnlockBucketPolicy",
        "PolicyId",
        "ANPAZKAPJZG4OALTIPZOF",
    ),
    (
        "/root-task/",
        "SQSUnlockQueuePolicy",
        "PolicyId",
        "ANPAZKAPJZG4HPZI4FDUV",
    ),
    (
        "/root-task/",
        "SQSUnlockQueuePolicy",
        "CreateDate",
        "2024-11-06T21:51:02Z",
    ),
    (
        "/root-task/",
        "SQSUnlockQueuePolicy",
        "UpdateDate",
        "2024-11-06T21:51:02Z",
    ),
    (
        "/service-role/",
        "AmazonElasticMapReduceforEC2Role",
        "PolicyId",
        "ANPAIGALS5RCDLZLB3PGS",
    ),
    (
        "/service-role/",
        "AmazonElasticMapReduceforEC2Role",
        "DefaultVersionId",
        "v3",
    ),
    (
        "/service-role/",
        "AmazonElasticMapReduceforEC2Role",
        "CreateDate",
        "2015-02-06T18:41:21Z",
    ),
    (
        "/service-role/",
        "AmazonElasticMapReduceforEC2Role",
        "UpdateDate",
        "2017-08-11T23:57:30Z",
    ),
    (
        "/service-role/",
        "AutoScalingNotificationAccessRole",
        "PolicyId",
        "ANPAIO2VMUPGDC5PZVXVA",
    ),
    (
        "/service-role/",
        "AWSCloudHSMRole",
        "PolicyId",
        "ANPAI7QIUU4GC66SF26WE",
    ),
    (
        "/service-role/",
        "AmazonElasticMapReduceRole",
        "PolicyId",
        "ANPAIDI2BQT2LKXZG36TW",
    ),
    (
        "/service-role/",
        "AmazonElasticMapReduceRole",
        "UpdateDate",
        "2020-06-24T22:24:20Z",
    ),
    (
        "/service-role/",
        "AmazonEFSCSIDriverPolicy",
        "DefaultVersionId",
        "v1",
    ),
];

/// The ARN AWS publishes a managed policy under: `arn:aws:iam::aws:policy<path><name>`.
fn published_arn(path: &str, policy_name: &str) -> String {
    format!("arn:aws:iam::aws:policy{path}{policy_name}")
}

/// The four members of one `Policy` this suite is about, read off an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PublicationData {
    policy_id: String,
    default_version_id: String,
    create_date: String,
    update_date: String,
}

impl PublicationData {
    /// The data an XML answer carrying ONE policy reports.
    fn of(body: &str) -> Self {
        Self {
            policy_id: element(body, "PolicyId"),
            default_version_id: element(body, "DefaultVersionId"),
            create_date: element(body, "CreateDate"),
            update_date: element(body, "UpdateDate"),
        }
    }
}

/// What `GetPolicy` reports for `arn`, signed for `region`, which must resolve.
async fn read_policy(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    arn: &str,
) -> PublicationData {
    let (status, body) = call_signed_in_region(
        router,
        None,
        region,
        "GetPolicy",
        &[("PolicyArn", arn)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetPolicy must serve a published policy, got {status}: {body}"
    );
    PublicationData::of(&body)
}

/// Every ARN `ListPolicies` reports for `params`, walked over ALL its pages.
///
/// The page walk itself is the one `tests/all/published_policy_families.rs` owns, read for
/// the `Arn` member of each page.
async fn walk_arns(router: &MultiServiceRouter, case: &str, params: &[(&str, &str)]) -> Vec<String> {
    walk_policies(router, case, params, "Arn").await
}

/// The catalog entry behind `arn`, which the emulator must carry for every row here.
fn entry_of(case: &str, arn: &str) -> &'static state::AwsManagedPolicy {
    state::aws_managed_policy(arn)
        .unwrap_or_else(|| panic!("case {case:?}: the catalog must publish this policy"))
}

/// RULE: the publication data of a policy AWS owns is a catalog constant — the same value for
/// the same ARN whichever Region the request is signed for and whichever emulator instance
/// serves it — and it is the data of THAT policy, not a value stamped on the catalog.
///
/// The expected record here is the catalog's own entry, so what this run has force over is
/// the INVARIANTS: one value per ARN across Regions and across instances, one id per policy,
/// and ids, versions and instants that differ between entries. The published values
/// themselves are pinned literally by
/// [`published_members_are_the_values_the_catalog_publishes`].
#[tokio::test]
async fn published_policy_data_is_one_constant_per_arn() {
    // Two emulator instances over two engines of their own: the second one stands for a
    // client reading the same ARN from another build of the emulator.
    let first = router();
    let second = router();

    let mut seen: Vec<PublicationData> = Vec::new();
    for (path, policy_name) in ENTRIES {
        let arn = published_arn(path, policy_name);
        let entry = entry_of(&arn, &arn);
        let expected = PublicationData {
            policy_id: entry.policy_id.to_string(),
            default_version_id: entry.default_version_id.to_string(),
            create_date: entry.published.to_string(),
            update_date: entry.revised.to_string(),
        };

        for region in REGIONS {
            for (instance, router) in [("first", &first), ("second", &second)] {
                let case = format!("{arn} in {region} on the {instance} instance");
                let read = read_policy(router, &case, region, &arn).await;
                assert_eq!(
                    read, expected,
                    "case {case:?}: expected the data AWS publishes for this policy, got \
                     {read:?}"
                );
            }
        }

        // The same record the enumeration of the policy's own family reports.
        let listed = walk_arns(
            &first,
            &arn,
            &[("Scope", "AWS"), ("PathPrefix", path), ("MaxItems", "5")],
        )
        .await;
        assert!(
            listed.contains(&arn),
            "case {arn:?}: the family's enumeration must report this policy, got {listed:?}"
        );

        // No two published policies share an id, and the version a policy reports names a
        // version the reader serves.
        assert!(
            !seen
                .iter()
                .any(|other| other.policy_id == expected.policy_id),
            "case {arn:?}: a PolicyId identifies ONE policy, but {:?} repeats",
            expected.policy_id
        );
        assert!(
            expected.create_date <= expected.update_date,
            "case {arn:?}: a policy is revised no earlier than it is published, got {:?} and \
             {:?}",
            expected.create_date,
            expected.update_date
        );
        seen.push(expected);
    }

    // The rows are not one template's values: ids, versions and instants differ across the
    // catalog a client keys, pins and sorts by.
    for (member, values) in [
        (
            "PolicyId",
            seen.iter()
                .map(|data| data.policy_id.clone())
                .collect::<::std::collections::BTreeSet<String>>(),
        ),
        (
            "DefaultVersionId",
            seen.iter()
                .map(|data| data.default_version_id.clone())
                .collect(),
        ),
        (
            "CreateDate",
            seen.iter().map(|data| data.create_date.clone()).collect(),
        ),
        (
            "UpdateDate",
            seen.iter().map(|data| data.update_date.clone()).collect(),
        ),
    ] {
        assert!(
            values.len() > 1,
            "case {member:?}: expected each policy's own value, got one shared value across \
             {} policies",
            seen.len()
        );
    }
}

/// RULE: a published policy's member is the value AWS publishes for that ARN, asserted here
/// against values written out from the recorded answers rather than against the seed, so a
/// seed that drifts from the published catalog fails.
#[tokio::test]
async fn published_members_are_the_values_the_catalog_publishes() {
    let router = router();

    for (path, policy_name, member, value) in PUBLISHED_MEMBERS {
        let arn = published_arn(path, policy_name);
        let case = format!("{arn} <{member}>");
        let read = read_policy(&router, &case, REGIONS[0], &arn).await;
        let reported = match *member {
            "PolicyId" => &read.policy_id,
            "DefaultVersionId" => &read.default_version_id,
            "CreateDate" => &read.create_date,
            "UpdateDate" => &read.update_date,
            other => panic!("case {case:?}: this suite reports no member {other:?}"),
        };
        assert_eq!(
            reported, value,
            "case {case:?}: expected the value AWS publishes, got {reported:?}"
        );
    }
}

/// RULE: a published policy's data is CATALOG data, so it is not minted the way the account's
/// own policy store mints a record — neither its id nor its instants come out of the minter a
/// `CreatePolicy` goes through.
#[tokio::test]
async fn published_data_is_not_minted_like_an_account_policy() {
    let router = router();

    let (status, body) = call(
        &router,
        "CreatePolicy",
        &[
            ("PolicyName", "catalog-constants-account-policy"),
            ("PolicyDocument", ACCOUNT_DOCUMENT),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreatePolicy must succeed, got {status}: {body}"
    );
    let minted = PublicationData::of(&body);

    for (path, policy_name) in ENTRIES {
        let arn = published_arn(path, policy_name);
        let case = arn.clone();
        let published = read_policy(&router, &case, REGIONS[0], &arn).await;
        assert_ne!(
            published.policy_id, minted.policy_id,
            "case {case:?}: the account's minter must not hand out a published policy's id"
        );
        assert_ne!(
            published.create_date, minted.create_date,
            "case {case:?}: a published policy carries the instant AWS published it, not the \
             instant this account's store was written, got {:?}",
            published.create_date
        );
        assert_ne!(
            published.update_date, minted.update_date,
            "case {case:?}: a published policy carries the instant AWS last revised it, got \
             {:?}",
            published.update_date
        );
    }
}

/// RULE: every policy the published `/service-role/` family carries is discoverable by
/// enumerating that path — at any page size — and readable by the ARN the enumeration reports,
/// down to the document of the version it names.
#[tokio::test]
async fn the_service_role_family_is_enumerable_and_readable_whole() {
    let router = router();

    let expected: Vec<String> = SERVICE_ROLE_FAMILY
        .iter()
        .map(|policy_name| published_arn("/service-role/", policy_name))
        .collect();
    let mut sorted = expected.clone();
    sorted.sort();
    assert_eq!(
        expected, sorted,
        "the family is written out in the ascending ARN order an enumeration walks"
    );

    for size in PAGE_SIZES {
        let case = match size {
            Some(size) => format!("/service-role/ at MaxItems={size}"),
            None => "/service-role/ with no MaxItems".to_string(),
        };
        let mut params: Vec<(&str, &str)> =
            vec![("Scope", "AWS"), ("PathPrefix", "/service-role/")];
        if let Some(size) = size {
            params.push(("MaxItems", size));
        }
        let reported = walk_arns(&router, &case, &params).await;
        // Every name the family publishes is reported — a gap is what makes a policy
        // undiscoverable — and the walk is one ascending enumeration, so a later addition to
        // the family is not a failure here while a missing member is.
        for arn in &expected {
            assert!(
                reported.contains(arn),
                "case {case:?}: expected the enumeration to report {arn:?}, got {reported:?}"
            );
        }
        let mut ascending = reported.clone();
        ascending.sort();
        ascending.dedup();
        assert_eq!(
            reported, ascending,
            "case {case:?}: expected one ascending enumeration, each policy once"
        );
        for arn in &reported {
            assert!(
                arn.starts_with("arn:aws:iam::aws:policy/service-role/"),
                "case {case:?}: every member selected by the prefix is published under it, \
                 got {arn:?}"
            );
        }
    }

    for policy_name in SERVICE_ROLE_FAMILY {
        let arn = published_arn("/service-role/", policy_name);
        let case = arn.clone();
        let entry = entry_of(&case, &arn);
        let read = read_policy(&router, &case, REGIONS[0], &arn).await;
        assert_eq!(
            read.policy_id, entry.policy_id,
            "case {case:?}: expected this policy's published id, got {:?}",
            read.policy_id
        );

        // The version the policy reports is the version a reader serves, with its document.
        let (status, body) = call(
            &router,
            "GetPolicyVersion",
            &[
                ("PolicyArn", arn.as_str()),
                ("VersionId", read.default_version_id.as_str()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetPolicyVersion must serve {:?}, got {status}: {body}",
            read.default_version_id
        );
        assert_eq!(
            element(&body, "VersionId"),
            read.default_version_id,
            "case {case:?}: expected the version asked for, got {body}"
        );
        assert!(
            state::is_policy_document(&document_element(&body, "Document")),
            "case {case:?}: expected the policy's document, got {body}"
        );

        // The path is part of the published identifier: the same friendly name under the
        // account root names no policy at all.
        let elsewhere = published_arn("/", policy_name);
        let (status, body) = call(&router, "GetPolicy", &[("PolicyArn", elsewhere.as_str())]).await;
        assert_eq!(
            status, 404,
            "case {elsewhere:?}: a name under a path AWS does not publish it at names nothing, \
             got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "NoSuchEntity",
            "case {elsewhere:?}: the refusal must be NoSuchEntity, got {body}"
        );
    }
}
