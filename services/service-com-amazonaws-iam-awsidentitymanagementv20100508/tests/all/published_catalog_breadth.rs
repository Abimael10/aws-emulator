// SPDX-License-Identifier: CC0-1.0

//! How WIDE the built-in AWS-managed policy catalog is, family by family: the policies a
//! client enumerating `arn:aws:iam::aws:policy/<family>/` discovers, and the publication
//! data each of them is reported from.
//!
//! ONE rule, asserted here: `ListPolicies` with `Scope=AWS` and a `PathPrefix` enumerates
//! the policies AWS publishes under that prefix, each reported from ITS OWN published data —
//! its opaque id, the version it is currently published at, the instants it was published
//! and last revised, and the attachability AWS publishes for it — so a policy AWS publishes
//! under the prefix is discoverable there whatever page size the walk uses, is readable by
//! the ARN the enumeration reports, down to the document of the version it names, and is
//! attachable where AWS does not reserve it. A policy the catalog leaves out is a policy no
//! client can discover, read or attach, which is why the breadth of each family is asserted
//! and not only the handful of entries one recorded trace reads.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: the family
//! the policy is published under, the entry inside that family (policies of one service and
//! of fifteen, job functions published a decade apart, service roles published at `v1` and
//! at `v38`), the member being reported, and the page size the enumeration is walked at.
//!
//! The awsQuery harness (`router`, `call`, `element`, `elements`) is the one
//! `tests/all/user_lifecycle.rs` owns and every suite in this crate shares; the page walk is
//! the one `tests/all/published_policy_families.rs` owns.

use ::std::collections::BTreeSet;

use smithy_server_runtime::MultiServiceRouter;

use crate::published_policy_families::walk_policies;
use crate::user_lifecycle::{call, document_element, element, elements, error_code, router};

/// A trust policy every role here needs to exist at all. It is a resource-based policy, so
/// it puts no service in the role's reach.
const TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// Page sizes the enumeration of a family is walked at, plus the walk that names no page
/// size and is served the family at the service's own default.
const PAGE_SIZES: &[Option<&str>] = &[None, Some("5"), Some("100")];

/// Policies AWS publishes under the path reserved for the policies of service-linked roles,
/// written out as `(path, policy_name)`.
///
/// SOURCE: the AWS Managed Policy Reference's `/aws-service-role/` family, cross-read
/// against the `ListPolicies` answer a client gets for that prefix. The rows include the
/// per-namespace Application Auto Scaling policies, which are also the ARNs this emulator's
/// own Application Auto Scaling control plane reports as the permissions policy of the
/// service-linked role it creates — an ARN this emulator hands a caller must be one its own
/// API can describe.
const SERVICE_LINKED_ROLE_FAMILY: &[&str] = &[
    "APIGatewayServiceRolePolicy",
    "AWSAppConfigServiceRolePolicy",
    "AWSAppMeshServiceRolePolicy",
    "AWSAppSyncServiceRolePolicy",
    // AWS spells this one with a capital `S` in "AutoScaling", unlike the fourteen beside it.
    "AWSApplicationAutoScalingCustomResourcePolicy",
    "AWSApplicationAutoscalingAppStreamFleetPolicy",
    "AWSApplicationAutoscalingCassandraTablePolicy",
    "AWSApplicationAutoscalingComprehendEndpointPolicy",
    "AWSApplicationAutoscalingDynamoDBTablePolicy",
    "AWSApplicationAutoscalingEC2SpotFleetRequestPolicy",
    "AWSApplicationAutoscalingECSServicePolicy",
    "AWSApplicationAutoscalingEMRInstanceGroupPolicy",
    "AWSApplicationAutoscalingElastiCacheRGPolicy",
    "AWSApplicationAutoscalingKafkaClusterPolicy",
    "AWSApplicationAutoscalingLambdaConcurrencyPolicy",
    "AWSApplicationAutoscalingNeptuneClusterPolicy",
    "AWSApplicationAutoscalingRDSClusterPolicy",
    "AWSApplicationAutoscalingSageMakerEndpointPolicy",
    "AWSApplicationAutoscalingWorkSpacesPoolPolicy",
    "AWSApplicationMigrationServiceRolePolicy",
    "AWSAuditManagerServiceRolePolicy",
    "AWSAutoScalingPlansEC2AutoScalingPolicy",
    "AWSElasticBeanstalkServiceRolePolicy",
    "AmazonElasticsearchServiceRolePolicy",
];

/// Policies AWS publishes under the path reserved for JOB FUNCTIONS.
///
/// SOURCE: the AWS Managed Policy Reference's `/job-function/` family, cross-read against
/// the `ListPolicies` answer for that prefix: the six long-standing job functions, the
/// console-administrator job, and the per-resource administrator jobs AWS files here for
/// Oracle Database@AWS.
const JOB_FUNCTION_FAMILY: &[&str] = &[
    "AWSManagementConsoleAdministratorAccess",
    "AmazonODBAutonomousDatabaseAdmin",
    "AmazonODBAutonomousVmClusterAdmin",
    "AmazonODBExadataInfrastructureAdmin",
    "AmazonODBExadataVmClusterAdmin",
    "AmazonODBExascaleStorageVaultAdmin",
    "AmazonODBExascaleVmClusterAdmin",
    "AmazonODBNetworkAdmin",
    "Billing",
    "DataScientist",
    "DatabaseAdministrator",
    "NetworkAdministrator",
    "SupportUser",
    "SystemAdministrator",
    "ViewOnlyAccess",
];

/// Policies AWS publishes under the path it files the policies of a SERVICE's own role
/// under.
///
/// SOURCE: the AWS Managed Policy Reference's `/service-role/` family, cross-read against
/// the `ListPolicies` answer for that prefix — the grants a client provisioning a service
/// role attaches by their well-known ARN.
const SERVICE_ROLE_FAMILY: &[&str] = &[
    "AWSTransferLoggingAccess",
    "AWS_ConfigRole",
    "AmazonAPIGatewayPushToCloudWatchLogs",
    "AmazonAppStreamServiceAccess",
    "AmazonDMSCloudWatchLogsRole",
    "AmazonDMSRedshiftS3Role",
    "AmazonEC2ContainerServiceAutoscaleRole",
    "AmazonEC2ContainerServiceEventsRole",
    "AmazonEC2ContainerServiceforEC2Role",
    "AmazonEC2RoleforAWSCodeDeploy",
    "AmazonEC2RoleforDataPipelineRole",
    "AmazonEC2SpotFleetTaggingRole",
    "AmazonEMRServicePolicy_v2",
    "AmazonElasticMapReduceEditorsRole",
    "AmazonElasticMapReduceforAutoScalingRole",
    "AmazonElasticMapReduceforEC2Role",
    "AmazonRDSDirectoryServiceAccess",
    "AmazonRekognitionServiceRole",
    "AmazonSNSRole",
    "AmazonSSMAutomationRole",
    "AmazonSSMMaintenanceWindowRole",
    "AmazonTextractServiceRole",
    "AutoScalingNotificationAccessRole",
    "CloudWatchEventsBuiltInTargetExecutionAccess",
    "CloudWatchEventsInvocationAccess",
    "ComprehendDataAccessRolePolicy",
    "RDSCloudHsmAuthorizationRole",
    "VMImportExportRoleForAWSConnector",
];

/// The three families this suite is about, each with the path AWS files it under.
fn families() -> Vec<(&'static str, &'static [&'static str])> {
    vec![
        ("/aws-service-role/", SERVICE_LINKED_ROLE_FAMILY),
        ("/job-function/", JOB_FUNCTION_FAMILY),
        ("/service-role/", SERVICE_ROLE_FAMILY),
    ]
}

/// The ARN AWS publishes a managed policy under: `arn:aws:iam::aws:policy<path><name>`.
fn published_arn(path: &str, policy_name: &str) -> String {
    format!("arn:aws:iam::aws:policy{path}{policy_name}")
}

/// The publication data one `Policy` is reported with.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    policy_name: String,
    path: String,
    policy_id: String,
    default_version_id: String,
    create_date: String,
    update_date: String,
    is_attachable: String,
}

/// What `GetPolicy` reports for `arn`, which must resolve.
async fn read_policy(router: &MultiServiceRouter, case: &str, arn: &str) -> Reported {
    let (status, body) = call(router, "GetPolicy", &[("PolicyArn", arn)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetPolicy must serve a published policy, got {status}: {body}"
    );
    Reported {
        policy_name: element(&body, "PolicyName"),
        path: element(&body, "Path"),
        policy_id: element(&body, "PolicyId"),
        default_version_id: element(&body, "DefaultVersionId"),
        create_date: element(&body, "CreateDate"),
        update_date: element(&body, "UpdateDate"),
        is_attachable: element(&body, "IsAttachable"),
    }
}

/// The id the earlier catalog derived from a policy's path and name — the shape a reported id
/// must NOT have, because a published id belongs to the policy rather than being read off
/// its name.
fn derived_from_the_name(path: &str, policy_name: &str) -> String {
    format!(
        "ANPA_{}_{}",
        path.trim_matches('/').replace('-', "").to_ascii_uppercase(),
        policy_name.to_ascii_uppercase()
    )
}

/// RULE: enumerating a family by its `PathPrefix` reports every policy AWS publishes under
/// that prefix — at any page size, as one ascending enumeration with each policy once — so a
/// client that resolves a managed policy by listing the catalog finds it.
#[tokio::test]
async fn every_published_family_member_is_discoverable_at_any_page_size() {
    let router = router();

    for (path, family) in families() {
        let expected: Vec<String> = family
            .iter()
            .map(|policy_name| published_arn(path, policy_name))
            .collect();

        for size in PAGE_SIZES {
            let case = match size {
                Some(size) => format!("the family at {path} walked {size} per page"),
                None => format!("the family at {path} walked at the default page size"),
            };
            let mut params: Vec<(&str, &str)> = vec![("Scope", "AWS"), ("PathPrefix", path)];
            if let Some(size) = size {
                params.push(("MaxItems", size));
            }
            let reported = walk_policies(&router, &case, &params, "Arn").await;

            for arn in &expected {
                assert!(
                    reported.contains(arn),
                    "case {case:?}: expected the enumeration to report {arn:?}, got \
                     {reported:?}"
                );
            }
            // One ascending enumeration, each policy once — a later addition to the family
            // is not a failure here, a missing member or a repeated page is.
            let mut ascending = reported.clone();
            ascending.sort();
            ascending.dedup();
            assert_eq!(
                reported, ascending,
                "case {case:?}: expected one ascending enumeration, each policy once"
            );
            for arn in &reported {
                assert!(
                    arn.starts_with(&format!("arn:aws:iam::aws:policy{path}")),
                    "case {case:?}: every member selected by the prefix is published under \
                     it, got {arn:?}"
                );
            }
        }
    }
}

/// RULE: each member of a family is reported from ITS OWN published data — an opaque id that
/// is not read off the policy's name, the version it is currently published at, its own two
/// instants and the attachability AWS publishes — and the version it names is the one a
/// reader is served, with that policy's document.
#[tokio::test]
async fn every_published_family_member_carries_its_own_publication_data() {
    let router = router();

    let mut ids: BTreeSet<String> = BTreeSet::new();
    let mut versions: BTreeSet<String> = BTreeSet::new();
    let mut created: BTreeSet<String> = BTreeSet::new();
    let mut updated: BTreeSet<String> = BTreeSet::new();

    for (path, family) in families() {
        for policy_name in family {
            let arn = published_arn(path, policy_name);
            let case = arn.clone();
            let read = read_policy(&router, &case, &arn).await;

            assert_eq!(
                read.policy_name, *policy_name,
                "case {case:?}: expected the published friendly name, got {:?}",
                read.policy_name
            );
            assert_eq!(
                read.path, path,
                "case {case:?}: expected the path AWS publishes it under, got {:?}",
                read.path
            );
            // AWS spells a policy's unique id as `ANPA` and 17 further uppercase
            // alphanumerics, and it identifies the policy rather than spelling its name.
            assert_eq!(
                read.policy_id.len(),
                21,
                "case {case:?}: expected ANPA plus 17 characters, got {:?}",
                read.policy_id
            );
            assert!(
                read.policy_id
                    .strip_prefix("ANPA")
                    .is_some_and(|suffix| suffix
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())),
                "case {case:?}: expected ANPA plus uppercase alphanumerics, got {:?}",
                read.policy_id
            );
            assert_ne!(
                read.policy_id,
                derived_from_the_name(path, policy_name),
                "case {case:?}: a published id is the policy's own, not one computed from \
                 its path and name"
            );
            assert!(
                !read
                    .policy_id
                    .contains(&policy_name.to_ascii_uppercase()),
                "case {case:?}: a published id does not spell the policy's name, got {:?}",
                read.policy_id
            );
            assert!(
                ids.insert(read.policy_id.clone()),
                "case {case:?}: a PolicyId identifies ONE policy, but {:?} repeats",
                read.policy_id
            );

            // The default version is the one this policy is published at, which for a
            // revised policy is past v1.
            let numbered = read
                .default_version_id
                .strip_prefix('v')
                .and_then(|digits| digits.parse::<u32>().ok());
            assert!(
                numbered.is_some_and(|number| number >= 1),
                "case {case:?}: expected a policyVersionIdType of the form v<number>, got \
                 {:?}",
                read.default_version_id
            );
            assert_eq!(
                read.is_attachable, "true",
                "case {case:?}: AWS publishes this policy as one that can be attached, got \
                 {:?}",
                read.is_attachable
            );
            assert!(
                !read.create_date.is_empty() && read.create_date <= read.update_date,
                "case {case:?}: a policy is revised no earlier than it is published, got \
                 {:?} and {:?}",
                read.create_date,
                read.update_date
            );

            // The version it reports is the version a reader is served, with its document.
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
            assert!(
                service_com_amazonaws_iam_awsidentitymanagementv20100508::state::is_policy_document(
                    &document_element(&body, "Document")
                ),
                "case {case:?}: expected the policy's document, got {body}"
            );

            versions.insert(read.default_version_id.clone());
            created.insert(read.create_date.clone());
            updated.insert(read.update_date.clone());
        }
    }

    // The data is per entry, not one template's values: a client keying the catalog by id,
    // pinning the version it read, or sorting by the dates sees policies that differ.
    for (member, values) in [
        ("DefaultVersionId", &versions),
        ("CreateDate", &created),
        ("UpdateDate", &updated),
    ] {
        assert!(
            values.len() > 1,
            "case {member:?}: expected each policy's own value, got the single value {:?} \
             across {} policies",
            values,
            ids.len()
        );
    }
    assert!(
        versions.iter().any(|version| version != "v1"),
        "case \"DefaultVersionId\": AWS revises its managed policies, so the families must \
         carry a policy published past v1, got {versions:?}"
    );
}

/// RULE: a discovered policy is a resource a client can USE — a policy AWS does not reserve
/// for a service-linked role attaches to an identity by the ARN the enumeration reported and
/// is read back as that identity's attachment, while one AWS files under the path it
/// reserves is refused the attach.
#[tokio::test]
async fn a_discovered_policy_attaches_unless_aws_reserves_it() {
    let router = router();
    let role = "CatalogBreadthRole";
    let (status, body) = call(
        &router,
        "CreateRole",
        &[
            ("RoleName", role),
            ("AssumeRolePolicyDocument", TRUST_POLICY),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture CreateRole must succeed, got {status}: {body}"
    );

    // Rows spanning the two outcomes and three families: a service role's grant and a job
    // function attach, a policy AWS reserves for the role it was written for does not.
    let rows: &[(&str, &str, bool)] = &[
        ("/service-role/", "AmazonAPIGatewayPushToCloudWatchLogs", true),
        ("/service-role/", "AmazonEC2ContainerServiceforEC2Role", true),
        ("/job-function/", "AmazonODBNetworkAdmin", true),
        ("/job-function/", "AWSManagementConsoleAdministratorAccess", true),
        (
            "/aws-service-role/",
            "AWSApplicationAutoscalingDynamoDBTablePolicy",
            false,
        ),
        ("/aws-service-role/", "APIGatewayServiceRolePolicy", false),
    ];

    let mut attached: Vec<String> = Vec::new();
    for (path, policy_name, attaches) in rows {
        let arn = published_arn(path, policy_name);
        let case = arn.clone();
        let (status, body) = call(
            &router,
            "AttachRolePolicy",
            &[("RoleName", role), ("PolicyArn", arn.as_str())],
        )
        .await;
        if *attaches {
            assert_eq!(
                status, 200,
                "case {case:?}: a published policy AWS does not reserve must attach, got \
                 {status}: {body}"
            );
            attached.push(arn);
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: AWS reserves this policy for the service-linked role it was \
                 written for, so the attach must be refused, got {status}: {body}"
            );
            assert_eq!(
                error_code(&body),
                "PolicyNotAttachable",
                "case {case:?}: expected the refusal AWS words for a reserved policy, got \
                 {body}"
            );
        }
    }

    let (status, body) = call(
        &router,
        "ListAttachedRolePolicies",
        &[("RoleName", role)],
    )
    .await;
    assert_eq!(
        status, 200,
        "ListAttachedRolePolicies must succeed, got {status}: {body}"
    );
    let mut reported = elements(&body, "PolicyArn");
    reported.sort();
    let mut expected = attached.clone();
    expected.sort();
    assert_eq!(
        reported, expected,
        "the role must carry exactly the policies whose attach was served, got {reported:?}"
    );
}
