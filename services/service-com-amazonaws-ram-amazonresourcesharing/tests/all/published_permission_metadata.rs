// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the PER-ENTRY published metadata every read of the AWS
//! managed permission library reports.
//!
//! The rule the rows below pin: each managed permission in RAM's library carries its own
//! published metadata, and a read reports it from the entry rather than defaulting it —
//! the version Amazon Web Services publishes that permission at (`defaultVersion` true,
//! so a share that names no version attaches that number), the `resourceType` spelled the
//! way the owning service publishes it, which is the spelling the shareable-resource-type
//! catalog carries and a resource ARN of that type reduces to, and
//! `isResourceTypeDefault` true only for the permission RAM attaches when a share names
//! none for the type. Membership is published data too: a name the library publishes no
//! permission under is enumerated nowhere and resolves to nothing.
//!
//! The rows vary along what the rule generalises over: versions spanning the first one
//! and six revisions beyond it, so no single number could satisfy them; resource types
//! whose spelling cannot be derived from the permission's own name (a plural, a `v2`
//! type, a shortened one, and two types published by a service the permission's name does
//! not mention); default flags on families where the default is neither the first nor the
//! most permissive permission of the family; and names that are near-misses of published
//! ones.
//!
//! Source of the published facts named as literals here: the managed permission library
//! an unfiltered `ListPermissions` enumerates, whose resource-type spellings are the ones
//! `ListResourceTypes` publishes — each one the ARN resource code of the owning service's
//! resource (an MLflow App is `arn:aws:sagemaker:<region>:<account>:mlflow-app/<id>` and a
//! SageMaker Catalog `arn:aws:sagemaker:<region>:<account>:sagemaker-catalog/<name>`,
//! their models' `MlflowAppArn` and `ResourceCatalogArn` patterns).

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_ram_amazonresourcesharing::state::{
    aws_managed_permission_arn, AWS_MANAGED_PERMISSIONS,
};
use smithy_server_runtime::MultiServiceRouter;

// Every suite in this crate calls the service the one way: through `tests/all/harness.rs`.
use super::harness::*;

async fn list_permissions(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/listpermissions", Some(body)).await
}

async fn get_permission(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/getpermission", Some(body)).await
}

/// The permissions a read reported.
fn permissions_of(case: &str, body: &Value) -> Vec<Value> {
    body.get("permissions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: response carried no permissions, got {body:?}"))
}

/// A string member of a reported summary or detail.
fn field(case: &str, permission: &Value, member: &str) -> String {
    permission
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} member, got {permission:?}"))
        .to_string()
}

/// A boolean member of a reported summary or detail.
fn flag(case: &str, permission: &Value, member: &str) -> bool {
    permission
        .get(member)
        .and_then(Value::as_bool)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} member, got {permission:?}"))
}

/// Every summary an unfiltered enumeration reports, paged to the end, as
/// `(arn, summary)` pairs.
///
/// The page budget is the library's own size, so a read that never stops fails the case
/// instead of looping.
async fn whole_library(router: &MultiServiceRouter, case: &str) -> Vec<(String, Value)> {
    let mut reported: Vec<(String, Value)> = Vec::new();
    let mut token: Option<String> = None;
    for page in 0..=AWS_MANAGED_PERMISSIONS.len() {
        let mut body = json!({"maxResults": 100});
        if let Some(resume) = &token {
            body["nextToken"] = json!(resume);
        }
        let (status, answered) = list_permissions(router, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: page {page} expected HTTP 200, got {status} with {answered:?}"
        );
        for permission in permissions_of(case, &answered) {
            reported.push((field(case, &permission, "arn"), permission));
        }
        token = answered
            .get("nextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if token.is_none() {
            return reported;
        }
    }
    panic!("case {case:?}: the enumeration never reported its last page");
}

/// The one summary the library reports for `name`.
fn summary_for(case: &str, library: &[(String, Value)], name: &str) -> Value {
    let arn = aws_managed_permission_arn(name);
    let found: Vec<&Value> = library
        .iter()
        .filter(|(reported, _)| *reported == arn)
        .map(|(_, permission)| permission)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: the library publishes {arn} exactly once, got {} entries",
        found.len()
    );
    found[0].clone()
}

/// Every resource type `ListResourceTypes` publishes, paged to the end.
async fn shareable_resource_types(router: &MultiServiceRouter, case: &str) -> BTreeSet<String> {
    let mut published: BTreeSet<String> = BTreeSet::new();
    let mut token: Option<String> = None;
    // One page per entry at worst, so a read that never stops fails the case.
    for page in 0..=200 {
        let mut body = json!({"maxResults": 100});
        if let Some(resume) = &token {
            body["nextToken"] = json!(resume);
        }
        let (status, answered) = call(router, "POST", "/listresourcetypes", Some(&body)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: page {page} expected HTTP 200, got {status} with {answered:?}"
        );
        let entries = answered
            .get("resourceTypes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: response carried no resourceTypes, got {answered:?}")
            });
        for entry in entries {
            published.insert(field(case, &entry, "resourceType"));
        }
        token = answered
            .get("nextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if token.is_none() {
            return published;
        }
    }
    panic!("case {case:?}: the resource-type catalog never reported its last page");
}

#[tokio::test]
async fn every_entry_reports_the_version_the_library_publishes_it_at() {
    // One row per published version the rows span — the first version and six revisions
    // beyond it — so no single number satisfies the table and an entry that falls back to
    // a catalog-wide default fails its case. Rows include permissions this issue did not
    // cite.
    let published: &[(&str, &str)] = &[
        ("AWSRAMDefaultPermissionSubnet", "1"),
        ("AWSRAMPermissionSagemakerHubReadOnly", "1"),
        ("AWSRAMPermissionGlueTableReadWrite", "2"),
        ("AWSRAMPermissionSageMakerFeatureGroupAdmin", "2"),
        ("AWSRAMDefaultPermissionsSecurityGroup", "2"),
        ("AWSRAMPermissionBackupVaultReadOnly", "3"),
        ("AWSRAMPermissionIpamResourceDiscovery", "3"),
        ("AWSRAMPermissionGlueAllTablesReadWriteForCatalog", "3"),
        ("AWSRAMDefaultPermissionsIpamPool", "4"),
        ("AWSRAMPermissionAmazonDataZoneDomainFullAccessWithPortalAccess", "7"),
        ("AWSRAMPermissionsAmazonDatazoneDomainExtendedServiceWithPortalAccess", "8"),
        ("AWSRAMDefaultPermissionAmazonDataZoneDomain", "10"),
    ];

    let router = emulator();
    let case = "published versions";
    let library = whole_library(&router, case).await;
    for (name, version) in published {
        let summary = summary_for(name, &library, name);
        assert_eq!(
            field(name, &summary, "version"),
            *version,
            "case {name:?}: expected the version the library publishes, got {summary:?}"
        );
        assert!(
            flag(name, &summary, "defaultVersion"),
            "case {name:?}: the published version is the default one, got {summary:?}"
        );

        // A read of the permission in its own right reports the same number, and the
        // version before it is not one the permission carries.
        let arn = aws_managed_permission_arn(name);
        let (status, read) = get_permission(&router, &json!({"permissionArn": arn.clone()})).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected HTTP 200, got {status} with {read:?}"
        );
        let detail = read
            .get("permission")
            .cloned()
            .unwrap_or_else(|| panic!("case {name:?}: the read carried no permission, got {read:?}"));
        assert_eq!(
            field(name, &detail, "version"),
            *version,
            "case {name:?}: a read of the permission reports the published version, got {detail:?}"
        );
        let numeric: i32 = version
            .parse()
            .unwrap_or_else(|error| panic!("case {name:?}: {version:?} is not a number: {error}"));
        if numeric > 1 {
            let (status, refused) = get_permission(
                &router,
                &json!({"permissionArn": arn, "permissionVersion": numeric - 1}),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {name:?}: version {} is not published, so the read is refused, got \
                 {status} with {refused:?}",
                numeric - 1
            );
        }
    }

    // The version is per entry rather than one number for the whole library: the
    // enumeration reports several distinct ones.
    let distinct: BTreeSet<String> = library
        .iter()
        .map(|(_, permission)| field(case, permission, "version"))
        .collect();
    assert!(
        distinct.len() > 3,
        "case {case:?}: the library publishes permissions at several versions, got {distinct:?}"
    );
}

#[tokio::test]
async fn every_entry_reports_the_resource_type_its_owning_service_publishes() {
    // One row per published spelling that cannot be derived from the permission's own
    // name: a `v2` type, a shortened one, two plurals, and three types published by a
    // service the permission's name does not name.
    let published: &[(&str, &str)] = &[
        ("AWSRAMEventBridgeEventBusV2PublishOnly", "events:EventBusv2"),
        ("AWSRAMPermissionAccessGrantsControl", "s3:AccessGrants"),
        (
            "AWSRAMPermissionServiceCatalogAppRegistryApplicationAllowAssociation",
            "servicecatalog:Applications",
        ),
        (
            "AWSRAMPermissionServiceCatalogAppRegistryAttributeGroupReadOnly",
            "servicecatalog:AttributeGroups",
        ),
        ("AWSRAMDefaultPermissionServerManageability", "ec2:DedicatedHost"),
        ("AWSRAMPermissionMlflowDefaultPolicy", "sagemaker:MlflowApp"),
        (
            "AWSRAMPermissionSageMakerCatalogResourceSearch",
            "sagemaker:SagemakerCatalog",
        ),
    ];

    let router = emulator();
    let case = "published resource types";
    let library = whole_library(&router, case).await;
    let shareable = shareable_resource_types(&router, case).await;
    for (name, resource_type) in published {
        let summary = summary_for(name, &library, name);
        assert_eq!(
            field(name, &summary, "resourceType"),
            *resource_type,
            "case {name:?}: expected the resource type the owning service publishes, got \
             {summary:?}"
        );

        // The same spelling is one the shareable-resource-type catalog publishes, so a
        // client that reads a type there and scopes a read with it finds the permission.
        assert!(
            shareable.contains(*resource_type),
            "case {name:?}: {resource_type:?} must be a type this service publishes, got \
             {} published types",
            shareable.len()
        );
        let scoped_case = format!("{name} scoped to {resource_type}");
        let (status, scoped) =
            list_permissions(&router, &json!({"resourceType": resource_type})).await;
        assert_eq!(
            status, 200,
            "case {scoped_case:?}: expected HTTP 200, got {status} with {scoped:?}"
        );
        let arns: Vec<String> = permissions_of(&scoped_case, &scoped)
            .iter()
            .map(|permission| field(&scoped_case, permission, "arn"))
            .collect();
        assert!(
            arns.contains(&aws_managed_permission_arn(name)),
            "case {scoped_case:?}: a read scoped to the published type reports the permission, \
             got {arns:?}"
        );
    }
}

#[tokio::test]
async fn the_resource_type_default_flag_is_the_one_the_library_publishes_per_entry() {
    // Families whose default is neither the first-listed nor the most permissive
    // permission of the family: for each resource type, exactly the named permission
    // reports the flag and every other permission published for the type reports false.
    let published: &[(&str, &str, &[&str])] = &[
        (
            "events:EventBusv2",
            "AWSRAMEventBridgeEventBusV2SubscribeOnly",
            &[
                "AWSRAMEventBridgeEventBusV2EventSourceAccess",
                "AWSRAMEventBridgeEventBusV2FullAccess",
                "AWSRAMEventBridgeEventBusV2PublishOnly",
            ],
        ),
        (
            "s3:AccessGrants",
            "AWSRAMPermissionAccessGrantsData",
            &[
                "AWSRAMPermissionAccessGrantsControl",
                "AWSRAMPermissionAccessGrantsReadAccess",
            ],
        ),
        (
            "sagemaker:Hub",
            "AWSRAMPermissionSagemakerHubReadAndUse",
            &[
                "AWSRAMPermissionSagemakerHubFullAccess",
                "AWSRAMPermissionSagemakerHubReadOnly",
            ],
        ),
        (
            "servicecatalog:Applications",
            "AWSRAMPermissionServiceCatalogAppRegistryApplicationReadOnly",
            &["AWSRAMPermissionServiceCatalogAppRegistryApplicationAllowAssociation"],
        ),
        (
            "servicecatalog:AttributeGroups",
            "AWSRAMPermissionServiceCatalogAppRegistryAttributeGroupReadOnly",
            &["AWSRAMPermissionServiceCatalogAppRegistryAttributeGroupAllowAssociation"],
        ),
        (
            "network-firewall:StatefulRulegroup",
            "AWSRAMDefaultPermissionNetworkFirewallStatefulRuleGroup",
            &["AWSRAMDefaultPermissionNetworkFirewallStatefulRuleGroupWithDescribe"],
        ),
    ];

    let router = emulator();
    let case = "published resource-type defaults";
    let library = whole_library(&router, case).await;
    for (resource_type, default, others) in published {
        let summary = summary_for(default, &library, default);
        assert!(
            flag(default, &summary, "isResourceTypeDefault"),
            "case {default:?}: the library publishes it as the default of {resource_type:?}, \
             got {summary:?}"
        );
        for other in *others {
            let summary = summary_for(other, &library, other);
            assert!(
                !flag(other, &summary, "isResourceTypeDefault"),
                "case {other:?}: {default:?} is the default of {resource_type:?}, so this one \
                 reports false, got {summary:?}"
            );
        }

        // Read through the type rather than through the names: the permissions the scoped
        // read reports as the type's default are exactly the one the library publishes as
        // such.
        let scoped_case = format!("the default of {resource_type}");
        let (status, scoped) =
            list_permissions(&router, &json!({"resourceType": resource_type})).await;
        assert_eq!(
            status, 200,
            "case {scoped_case:?}: expected HTTP 200, got {status} with {scoped:?}"
        );
        let defaults: Vec<String> = permissions_of(&scoped_case, &scoped)
            .iter()
            .filter(|permission| flag(&scoped_case, permission, "isResourceTypeDefault"))
            .map(|permission| field(&scoped_case, permission, "name"))
            .collect();
        assert_eq!(
            defaults,
            vec![default.to_string()],
            "case {scoped_case:?}: expected the permission the library publishes as the type's \
             default, got {defaults:?}"
        );
    }
}

#[tokio::test]
async fn a_name_the_library_publishes_no_permission_under_resolves_nowhere() {
    // Near-misses of published names: one subordinate-CA issuance permission per
    // certificate template beyond the published one, and five legacy spellings of a
    // published permission's name. A name the library does not publish is enumerated
    // nowhere, so a client cannot discover an ARN that resolves to nothing.
    let unpublished = [
        "AWSRAMSubordinateCACertificatePathLen1IssuanceCertificateAuthority",
        "AWSRAMSubordinateCACertificatePathLen2IssuanceCertificateAuthority",
        "AWSRAMSubordinateCACertificatePathLen3IssuanceCertificateAuthority",
        "AWSRAMDefaultPermissionIpamPool",
        "AWSRAMDefaultPermissionNetworkFirewallFirewallPolicy",
        "AWSRAMDefaultPermissionOutpost",
        "AWSRAMDefaultPermissionSite",
        "AWSRAMDefaultPermissionStatefulRuleGroup",
        "AWSRAMDefaultPermissionVpcLatticeServiceNetwork",
    ];

    let router = emulator();
    let case = "unpublished names";
    let library = whole_library(&router, case).await;
    let enumerated: BTreeSet<String> = library.iter().map(|(arn, _)| arn.clone()).collect();
    for name in unpublished {
        let arn = aws_managed_permission_arn(name);
        assert!(
            !enumerated.contains(&arn),
            "case {name:?}: a name the library does not publish must not be enumerated, got \
             {} entries carrying it",
            enumerated.iter().filter(|entry| **entry == arn).count()
        );
        let (status, refused) = get_permission(&router, &json!({"permissionArn": arn.clone()})).await;
        assert_eq!(
            status, 400,
            "case {name:?}: expected HTTP 400, got {status} with {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "UnknownResourceException",
            "case {name:?}: expected UnknownResourceException, got {refused:?}"
        );
        assert_eq!(
            error_message(&refused),
            format!("Permission {arn} could not be found."),
            "case {name:?}: the refusal names the permission it was asked for, got {refused:?}"
        );
    }

    // The permissions the library does publish for the same resource types are still
    // enumerated, so the membership above is a correction rather than a gap.
    for name in [
        "AWSRAMSubordinateCACertificatePathLen0IssuanceCertificateAuthority",
        "AWSRAMDefaultPermissionsIpamPool",
        "AWSRAMDefaultPermissionNetworkFirewallPolicy",
        "AWSRAMDefaultPermissionOutpostsOutpost",
        "AWSRAMDefaultPermissionOutpostsSite",
        "AWSRAMDefaultPermissionNetworkFirewallStatefulRuleGroup",
        "AWSRAMPermissionVpcLatticeServiceNetworkReadWrite",
    ] {
        let arn = aws_managed_permission_arn(name);
        assert!(
            enumerated.contains(&arn),
            "case {name:?}: the library publishes it, so the enumeration must report {arn}"
        );
    }
}
