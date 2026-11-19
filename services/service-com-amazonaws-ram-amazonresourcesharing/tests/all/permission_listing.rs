// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `ListPermissions` and `ListPermissionVersions`.
//!
//! The rule the rows below pin: a list of managed permissions reports every permission
//! the store holds that the Region the request was signed for sees — the customer
//! managed permissions created in that Region plus the AWS managed catalog, whose ARNs
//! name no Region — each reported once at its DEFAULT version, in permission ARN order,
//! one page of at most `maxResults` entries at a time with a `nextToken` only while a
//! further page exists, keeping the permissions whose `permissionType` the filter asks
//! for (`ALL`, and a request naming no filter, keep both kinds) and, when a
//! `resourceType` is named, the ones applying to that type matched without regard to
//! case or to the separators inside the resource code.
//!
//! A list of the versions of ONE permission pins the same rule over that permission's
//! versions: every version ever written for it, once each, in version order, with the
//! default flag on the version a read naming none answers with, paged the same way; a
//! request naming no permission this store holds is refused with the modelled error
//! that words that refusal.
//!
//! So the rows vary along what the rule generalises over: the filter takes each of its
//! modelled values and none at all; the resource type takes a type with two permissions,
//! one with a single permission, a hyphenated service code, two spellings of one type,
//! a shareable type no permission applies to and a type nothing shares; the page size
//! takes both published bounds, values between them and one past each; a permission
//! is read from the Region that created it and from another one; and a permission's
//! version count runs from the first version up to the published maximum of 5, over a
//! customer managed permission and over the catalog Amazon Web Services publishes.

use serde_json::{json, Value};
use service_com_amazonaws_ram_amazonresourcesharing::state::{
    aws_managed_permission_arn, encode_arn_cursor, encode_permission_version_cursor,
    permission_arn, resource_type_matches, AWS_MANAGED_PERMISSIONS, FIRST_PERMISSION_VERSION,
    PERMISSION_FEATURE_SET_STANDARD, PERMISSION_STATUS_ATTACHABLE, PERMISSION_TYPE_AWS_MANAGED,
    PERMISSION_TYPE_CUSTOMER_MANAGED, RESOURCE_TYPES,
};
use smithy_server_runtime::MultiServiceRouter;

// Every suite in this crate calls the service the one way: through `tests/all/harness.rs`.
use super::harness::*;

/// A Region other than the one the harness signs for, so a case can read what one
/// Region holds from another.
const OTHER_REGION: &str = "eu-west-1";

async fn list_permissions(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/listpermissions", Some(body)).await
}

async fn create_permission(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/createpermission", Some(body)).await
}

async fn create_permission_version(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/createpermissionversion", Some(body)).await
}

async fn get_permission(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/getpermission", Some(body)).await
}

/// The permissions a list reported.
fn permissions_of(case: &str, body: &Value) -> Vec<Value> {
    body.get("permissions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: response carried no permissions, got {body:?}"))
}

/// The ARNs a list reported, in the order it reported them.
fn arns_of(case: &str, body: &Value) -> Vec<String> {
    permissions_of(case, body)
        .iter()
        .map(|permission| {
            permission
                .get("arn")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a reported permission carried no arn, got {permission:?}")
                })
                .to_string()
        })
        .collect()
}

/// The `nextToken` a list reported, if any.
fn token_of(body: &Value) -> Option<String> {
    body.get("nextToken")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// A string member of a reported permission.
fn field(case: &str, permission: &Value, member: &str) -> String {
    permission
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} member, got {permission:?}"))
        .to_string()
}

/// A boolean member of a reported permission.
fn flag(case: &str, permission: &Value, member: &str) -> bool {
    permission
        .get(member)
        .and_then(Value::as_bool)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} member, got {permission:?}"))
}

/// Every AWS managed permission's ARN, in the order a page reports them.
fn catalog_arns() -> Vec<String> {
    let mut arns: Vec<String> = AWS_MANAGED_PERMISSIONS
        .iter()
        .map(|entry| aws_managed_permission_arn(entry.name))
        .collect();
    arns.sort();
    arns
}

/// A list that must succeed, with everything it reported on one page.
async fn list_ok(router: &MultiServiceRouter, case: &str, body: &Value) -> Value {
    let (status, reported) = list_permissions(router, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {reported:?}"
    );
    reported
}

/// Create a customer managed permission that must succeed, and answer its ARN.
async fn create_ok(
    router: &MultiServiceRouter,
    name: &str,
    resource_type: &str,
    action: &str,
) -> String {
    let (status, created) = create_permission(
        router,
        &json!({
            "name": name,
            "resourceType": resource_type,
            "policyTemplate": json!({"Effect": "ALLOW", "Action": [action]}).to_string(),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {name:?}: creating the permission expected 200, got {status} with {created:?}"
    );
    created
        .get("permission")
        .and_then(|permission| permission.get("arn"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {name:?}: the create reported no permission ARN, got {created:?}"))
        .to_string()
}

#[tokio::test]
async fn every_permission_the_store_holds_is_listed_once_at_its_default_version() {
    let router = emulator();
    let reported = list_ok(&router, "whole store", &json!({})).await;
    let case = "whole store";

    assert_eq!(
        arns_of(case, &reported),
        catalog_arns(),
        "case {case:?}: a fresh store lists the AWS managed catalog, once each in ARN order"
    );
    assert_eq!(
        token_of(&reported),
        None,
        "case {case:?}: one page held every permission, so no nextToken is reported"
    );

    // Each reported summary is the catalog entry at its default version: what the
    // store holds, not a value this read invented.
    for permission in permissions_of(case, &reported) {
        let arn = field(case, &permission, "arn");
        let entry = AWS_MANAGED_PERMISSIONS
            .iter()
            .find(|entry| aws_managed_permission_arn(entry.name) == arn)
            .unwrap_or_else(|| panic!("case {case:?}: {arn:?} is not a catalog ARN"));
        assert_eq!(
            field(case, &permission, "name"),
            entry.name,
            "case {:?}: expected the catalog entry's name, got {permission:?}",
            entry.name
        );
        assert_eq!(
            field(case, &permission, "resourceType"),
            entry.resource_type,
            "case {:?}: expected the catalog entry's resourceType, got {permission:?}",
            entry.name
        );
        assert_eq!(
            field(case, &permission, "permissionType"),
            PERMISSION_TYPE_AWS_MANAGED,
            "case {:?}: a catalog entry is AWS_MANAGED, got {permission:?}",
            entry.name
        );
        assert_eq!(
            field(case, &permission, "featureSet"),
            PERMISSION_FEATURE_SET_STANDARD,
            "case {:?}: expected the STANDARD feature set, got {permission:?}",
            entry.name
        );
        assert_eq!(
            field(case, &permission, "status"),
            PERMISSION_STATUS_ATTACHABLE,
            "case {:?}: a catalog entry can be attached to a share, got {permission:?}",
            entry.name
        );
        assert_eq!(
            field(case, &permission, "version"),
            entry.version.to_string(),
            "case {:?}: expected the version the catalog publishes the permission at, got \
             {permission:?}",
            entry.name
        );
        assert!(
            flag(case, &permission, "defaultVersion"),
            "case {:?}: a list reports each permission at its default version, got {permission:?}",
            entry.name
        );
        assert_eq!(
            flag(case, &permission, "isResourceTypeDefault"),
            entry.is_resource_type_default,
            "case {:?}: expected the catalog's resource-type default flag, got {permission:?}",
            entry.name
        );
    }
}

#[tokio::test]
async fn the_permission_type_filter_keeps_one_kind_of_permission_or_both() {
    let router = emulator();
    // Two created permissions, so the customer managed population is more than one
    // entry and spans more than one resource type.
    let project = create_ok(
        &router,
        "ListFilterProject",
        "codebuild:Project",
        "codebuild:BatchGetProjects",
    )
    .await;
    let site = create_ok(&router, "ListFilterSite", "outposts:Site", "outposts:GetSite").await;
    let mut created = vec![project, site];
    created.sort();
    let catalog = catalog_arns();
    let mut both = catalog.clone();
    both.extend(created.clone());
    both.sort();

    // The rows: each modelled `PermissionTypeFilter` value, and no filter at all.
    let rows: Vec<(Option<&str>, &Vec<String>)> = vec![
        (None, &both),
        (Some("ALL"), &both),
        (Some(PERMISSION_TYPE_AWS_MANAGED), &catalog),
        (Some(PERMISSION_TYPE_CUSTOMER_MANAGED), &created),
    ];
    for (filter, expected) in rows {
        let case = format!("permissionType {filter:?}");
        let mut body = json!({});
        if let Some(value) = filter {
            body["permissionType"] = json!(value);
        }
        let reported = list_ok(&router, &case, &body).await;
        assert_eq!(
            &arns_of(&case, &reported),
            expected,
            "case {case:?}: expected exactly the permissions of that kind, got {reported:?}"
        );
        // Every entry a filtered page reports carries the type that was asked for.
        if let Some(value) = filter.filter(|value| *value != "ALL") {
            for permission in permissions_of(&case, &reported) {
                assert_eq!(
                    field(&case, &permission, "permissionType"),
                    value,
                    "case {case:?}: a filtered page reports only that type, got {permission:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn the_resource_type_filter_selects_the_permissions_of_one_type() {
    let router = emulator();
    // One created permission for a type the catalog also publishes permissions for, so
    // the filter is shown to select over both populations.
    let glue = create_ok(
        &router,
        "ListFilterGlueDatabase",
        "glue:Database",
        "glue:GetDatabase",
    )
    .await;

    // The rows name resource types from this service's own catalogs: the display names
    // `RESOURCE_TYPES` records and the types `AWS_MANAGED_PERMISSIONS` entries apply
    // to. `ec2:prefix-list` is the spelling an `ec2` prefix-list ARN carries for
    // `ec2:PrefixList`, and `GLUE:DATABASE` the same type shouted: the member "is not
    // case sensitive" (`ListPermissions$resourceType`).
    for wanted in [
        "glue:Database",
        "GLUE:DATABASE",
        "ec2:PrefixList",
        "ec2:prefix-list",
        "outposts:Site",
        "license-manager:LicenseConfiguration",
        "apigateway:Domainnames",
        // A type this service shares that no permission in the store applies to, and a
        // well-formed type nothing shares at all: both select nothing.
        "rds:Cluster",
        "ec2:NoSuchResourceType",
    ] {
        let case = format!("resourceType {wanted:?}");
        let mut expected: Vec<String> = AWS_MANAGED_PERMISSIONS
            .iter()
            .filter(|entry| resource_type_matches(entry.resource_type, wanted))
            .map(|entry| aws_managed_permission_arn(entry.name))
            .collect();
        if resource_type_matches("glue:Database", wanted) {
            expected.push(glue.clone());
        }
        expected.sort();

        let reported = list_ok(&router, &case, &json!({"resourceType": wanted})).await;
        assert_eq!(
            arns_of(&case, &reported),
            expected,
            "case {case:?}: expected the permissions applying to that resource type, got \
             {reported:?}"
        );
        for permission in permissions_of(&case, &reported) {
            let reported_type = field(&case, &permission, "resourceType");
            assert!(
                resource_type_matches(&reported_type, wanted),
                "case {case:?}: a filtered page reports only that resource type, got \
                 {reported_type:?}"
            );
        }
    }

    // A resource type that takes no customer managed permission is still listed: it is
    // shared through the permissions Amazon Web Services manages for it.
    let restricted = RESOURCE_TYPES
        .iter()
        .find(|entry| !entry.customer_managed_permissions)
        .expect("the resource-type catalog records a type that takes no customer managed permission");
    let case = format!("restricted {:?}", restricted.resource_type);
    let reported = list_ok(
        &router,
        &case,
        &json!({"resourceType": restricted.resource_type}),
    )
    .await;
    for permission in permissions_of(&case, &reported) {
        assert_eq!(
            field(&case, &permission, "permissionType"),
            PERMISSION_TYPE_AWS_MANAGED,
            "case {case:?}: that resource type is shared only through AWS managed permissions, \
             got {permission:?}"
        );
    }
}

#[tokio::test]
async fn a_page_runs_to_max_results_and_its_token_resumes_after_it() {
    let router = emulator();
    let created = create_ok(
        &router,
        "ListPagingPermission",
        "codebuild:Project",
        "codebuild:StartBuild",
    )
    .await;
    let mut all = catalog_arns();
    all.push(created);
    all.sort();

    // The published bounds of `maxResults` (1..500) and sizes between them: the whole
    // store comes back once, in one order, however it is cut into pages.
    for size in [1usize, 2, 7, all.len(), 500] {
        let case = format!("maxResults {size}");
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let mut body = json!({"maxResults": size});
            if let Some(resume) = &token {
                body["nextToken"] = json!(resume);
            }
            let reported = list_ok(&router, &case, &body).await;
            let page = arns_of(&case, &reported);
            assert!(
                page.len() <= size,
                "case {case:?}: a page reported {} entries, more than the {size} asked for",
                page.len()
            );
            seen.extend(page);
            token = token_of(&reported);
            pages += 1;
            assert!(
                pages <= all.len() + 1,
                "case {case:?}: paging did not finish after {pages} pages, got {seen:?}"
            );
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            seen, all,
            "case {case:?}: paging reported every permission exactly once, in ARN order"
        );
    }

    // One past each published bound is refused by the modelled range, not clamped.
    for size in [0, 501] {
        let case = format!("maxResults {size}");
        let (status, refused) = list_permissions(&router, &json!({"maxResults": size})).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a page size outside 1..500 is refused, got {status} with {refused:?}"
        );
    }
}

#[tokio::test]
async fn a_next_token_this_read_did_not_issue_is_refused() {
    let router = emulator();
    // A token is opaque, but it is one this read issued: anything else cannot name
    // where a page resumes.
    for token in [
        "not-a-token".to_string(),
        String::new(),
        encode_arn_cursor(&format!(
            "arn:aws:ram:{REGION}:{ACCOUNT}:resource-share/11111111-2222-3333-4444-555555555555"
        )),
        encode_arn_cursor("not-an-arn-at-all"),
    ] {
        let case = format!("nextToken {token:?}");
        let (status, refused) = list_permissions(&router, &json!({"nextToken": token})).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "InvalidNextTokenException",
            "case {case:?}: expected InvalidNextTokenException, got {refused:?}"
        );
    }

    // The token a page DID issue resumes the read.
    let first = list_ok(&router, "first page", &json!({"maxResults": 1})).await;
    let resume = token_of(&first).expect("a first page of one of many carries a nextToken");
    let second = list_ok(
        &router,
        "second page",
        &json!({"maxResults": 1, "nextToken": resume}),
    )
    .await;
    assert_eq!(
        arns_of("second page", &second),
        catalog_arns()[1..2].to_vec(),
        "case \"second page\": the token resumed at the entry after the first page"
    );
}

#[tokio::test]
async fn a_permission_is_listed_in_the_region_that_created_it_and_the_catalog_in_every_region() {
    let router = emulator();
    let created = create_ok(
        &router,
        "RegionScopedPermission",
        "codebuild:Project",
        "codebuild:BatchGetProjects",
    )
    .await;
    assert_eq!(
        created,
        permission_arn(REGION, "RegionScopedPermission"),
        "case \"created ARN\": a created permission's ARN carries the Region it was created in"
    );

    // The Region that created it lists it...
    let here = list_ok(
        &router,
        "same region",
        &json!({"permissionType": PERMISSION_TYPE_CUSTOMER_MANAGED}),
    )
    .await;
    assert_eq!(
        arns_of("same region", &here),
        vec![created],
        "case \"same region\": the Region that created the permission lists it"
    );

    // ...and another Region does not, while the catalog, which is Amazon Web Services'
    // own and names no Region, is listed from both.
    for (case, body, expected) in [
        (
            "other region, customer managed",
            json!({"permissionType": PERMISSION_TYPE_CUSTOMER_MANAGED}),
            Vec::new(),
        ),
        (
            "other region, aws managed",
            json!({"permissionType": PERMISSION_TYPE_AWS_MANAGED}),
            catalog_arns(),
        ),
        ("other region, every kind", json!({}), catalog_arns()),
    ] {
        let (status, reported) =
            call_in_region(&router, OTHER_REGION, "POST", "/listpermissions", Some(&body)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {reported:?}"
        );
        assert_eq!(
            arns_of(case, &reported),
            expected,
            "case {case:?}: expected what that Region sees, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn a_list_reports_the_default_version_a_new_version_moved_to() {
    let router = emulator();
    let arn = create_ok(
        &router,
        "VersionedListedPermission",
        "codebuild:Project",
        "codebuild:BatchGetProjects",
    )
    .await;
    let case = "new default version";
    let (status, added) = create_permission_version(
        &router,
        &json!({
            "permissionArn": arn.clone(),
            "policyTemplate": json!({"Effect": "ALLOW", "Action": ["codebuild:StartBuild"]})
                .to_string(),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: creating a second version expected 200, got {status} with {added:?}"
    );

    let reported = list_ok(
        &router,
        case,
        &json!({"permissionType": PERMISSION_TYPE_CUSTOMER_MANAGED}),
    )
    .await;
    let permissions = permissions_of(case, &reported);
    assert_eq!(
        permissions.len(),
        1,
        "case {case:?}: a permission is reported once however many versions it carries, got \
         {reported:?}"
    );
    assert_eq!(
        field(case, &permissions[0], "version"),
        (FIRST_PERMISSION_VERSION + 1).to_string(),
        "case {case:?}: expected the version the second create made the default, got {reported:?}"
    );
    assert!(
        flag(case, &permissions[0], "defaultVersion"),
        "case {case:?}: the reported version is the default one, got {reported:?}"
    );
}

#[tokio::test]
async fn a_list_reports_the_same_permissions_to_a_later_caller() {
    let router = emulator();
    let arn = create_ok(
        &router,
        "RepeatedlyListedPermission",
        "glue:Database",
        "glue:GetDatabase",
    )
    .await;
    // A read writes nothing: the page does not drift between calls, and neither does
    // what a read of one permission reports afterwards.
    let before = get_permission(&router, &json!({"permissionArn": arn.clone()})).await;
    let first = list_ok(&router, "repeat read", &json!({})).await;
    let second = list_ok(&router, "repeat read", &json!({})).await;
    assert_eq!(
        first, second,
        "case \"repeat read\": a list reported something else the second time, got {second:?}"
    );
    let after = get_permission(&router, &json!({"permissionArn": arn})).await;
    assert_eq!(
        before, after,
        "case \"repeat read\": listing changed what a read of one permission reports, got {after:?}"
    );
}

async fn list_permission_versions(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/listpermissionversions", Some(body)).await
}

/// The version numbers a list of versions reported, in the order it reported them.
fn versions_of(case: &str, body: &Value) -> Vec<String> {
    permissions_of(case, body)
        .iter()
        .map(|permission| field(case, permission, "version"))
        .collect()
}

/// A list of versions that must succeed.
async fn list_versions_ok(router: &MultiServiceRouter, case: &str, body: &Value) -> Value {
    let (status, reported) = list_permission_versions(router, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {reported:?}"
    );
    reported
}

/// Write `count` further versions of the permission at `arn`, each with its own
/// template, and answer the version numbers the permission then carries.
async fn add_versions(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    actions: &[&str],
) -> Vec<String> {
    for action in actions {
        let (status, written) = create_permission_version(
            router,
            &json!({
                "permissionArn": arn,
                "policyTemplate": json!({"Effect": "ALLOW", "Action": [action]}).to_string(),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: writing the version granting {action:?} expected 200, got {status} \
             with {written:?}"
        );
    }
    (FIRST_PERMISSION_VERSION..=FIRST_PERMISSION_VERSION + actions.len() as i32)
        .map(|version| version.to_string())
        .collect()
}

#[tokio::test]
async fn every_version_a_permission_carries_is_listed_in_version_order() {
    let router = emulator();
    // The rule over a VARYING number of versions: a permission at its first version,
    // and one at each further version up to the published maximum of 5, each reported
    // once, in version order, with the default flag on the version a read that names
    // none answers with.
    let later_actions = [
        "codebuild:StartBuild",
        "codebuild:ListBuildsForProject",
        "codebuild:BatchGetBuilds",
        "codebuild:ListProjects",
    ];
    for count in 0..=later_actions.len() {
        let case = format!("{} version(s) beyond the first", count);
        let name = format!("VersionsListed{count}");
        let arn = create_ok(
            &router,
            &name,
            "codebuild:Project",
            "codebuild:BatchGetProjects",
        )
        .await;
        let expected = add_versions(&router, &case, &arn, &later_actions[..count]).await;

        let reported = list_versions_ok(&router, &case, &json!({"permissionArn": arn})).await;
        assert_eq!(
            versions_of(&case, &reported),
            expected,
            "case {case:?}: expected every version in version order, got {reported:?}"
        );
        assert_eq!(
            token_of(&reported),
            None,
            "case {case:?}: one page holds every version, so it carries no nextToken, got \
             {reported:?}"
        );
        for permission in permissions_of(&case, &reported) {
            let version = field(&case, &permission, "version");
            assert_eq!(
                (
                    field(&case, &permission, "arn"),
                    field(&case, &permission, "name"),
                    field(&case, &permission, "resourceType"),
                    field(&case, &permission, "permissionType"),
                    field(&case, &permission, "featureSet"),
                    field(&case, &permission, "status"),
                ),
                (
                    arn.clone(),
                    name.clone(),
                    "codebuild:Project".to_string(),
                    PERMISSION_TYPE_CUSTOMER_MANAGED.to_string(),
                    PERMISSION_FEATURE_SET_STANDARD.to_string(),
                    PERMISSION_STATUS_ATTACHABLE.to_string(),
                ),
                "case {case:?}: version {version} reported another permission, got {permission:?}"
            );
            assert_eq!(
                flag(&case, &permission, "defaultVersion"),
                version == *expected.last().expect("a permission carries a version"),
                "case {case:?}: only the latest version is the default one, got {permission:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_page_of_versions_runs_to_max_results_and_its_token_resumes_after_it() {
    let router = emulator();
    let arn = create_ok(
        &router,
        "VersionPagingPermission",
        "glue:Database",
        "glue:GetDatabase",
    )
    .await;
    let all = add_versions(
        &router,
        "setup",
        &arn,
        &["glue:GetTable", "glue:GetTables", "glue:GetPartitions"],
    )
    .await;

    // The published bounds of `maxResults` (1..500) and sizes between them: every
    // version comes back once, in one order, however it is cut into pages.
    for size in [1usize, 2, all.len(), 500] {
        let case = format!("maxResults {size}");
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let mut body = json!({"permissionArn": arn.clone(), "maxResults": size});
            if let Some(resume) = &token {
                body["nextToken"] = json!(resume);
            }
            let reported = list_versions_ok(&router, &case, &body).await;
            let page = versions_of(&case, &reported);
            assert!(
                page.len() <= size,
                "case {case:?}: a page reported {} versions, more than the {size} asked for",
                page.len()
            );
            seen.extend(page);
            token = token_of(&reported);
            pages += 1;
            assert!(
                pages <= all.len() + 1,
                "case {case:?}: paging did not finish after {pages} pages, got {seen:?}"
            );
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            seen, all,
            "case {case:?}: paging reported every version exactly once, in version order"
        );
    }

    // One past each published bound is refused by the modelled range, not clamped.
    for size in [0, 501] {
        let case = format!("maxResults {size}");
        let (status, refused) = list_permission_versions(
            &router,
            &json!({"permissionArn": arn.clone(), "maxResults": size}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a page size outside 1..500 is refused, got {status} with {refused:?}"
        );
    }
}

#[tokio::test]
async fn the_versions_of_an_aws_managed_permission_are_the_one_it_publishes() {
    let router = emulator();
    // The catalog is read the same way a created permission is, from every Region: an
    // AWS managed permission is published at one version, which is its default — whether
    // that is the first version or the revision the library has since published.
    for entry in AWS_MANAGED_PERMISSIONS {
        let case = entry.name;
        let arn = aws_managed_permission_arn(entry.name);
        let reported = list_versions_ok(&router, case, &json!({"permissionArn": arn.clone()})).await;
        assert_eq!(
            versions_of(case, &reported),
            vec![entry.version.to_string()],
            "case {case:?}: expected the one published version, got {reported:?}"
        );
        let permission = permissions_of(case, &reported)
            .first()
            .cloned()
            .expect("the page reported the published version");
        assert_eq!(
            (
                field(case, &permission, "arn"),
                field(case, &permission, "permissionType"),
                flag(case, &permission, "defaultVersion"),
                flag(case, &permission, "isResourceTypeDefault"),
            ),
            (
                arn,
                PERMISSION_TYPE_AWS_MANAGED.to_string(),
                true,
                entry.is_resource_type_default,
            ),
            "case {case:?}: the published version reported something else, got {permission:?}"
        );
    }
}

#[tokio::test]
async fn a_list_of_versions_refuses_a_request_that_names_no_permission() {
    let router = emulator();
    let arn = create_ok(
        &router,
        "VersionRefusalPermission",
        "outposts:Site",
        "outposts:GetSite",
    )
    .await;

    // WHY a request names no versions to list: the ARN is not a managed permission ARN
    // at all, it is one nothing was stored under, or the token does not name where a
    // page of versions resumes.
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "a value that is not an ARN",
            json!({"permissionArn": "permission/Nope"}),
            "MalformedArnException",
        ),
        (
            "an ARN of another resource of this service",
            json!({
                "permissionArn":
                    format!("arn:aws:ram:{REGION}:{ACCOUNT}:resource-share/11111111-2222-3333-4444-555555555555"),
            }),
            "MalformedArnException",
        ),
        (
            "a permission ARN nothing was stored under",
            json!({"permissionArn": permission_arn(REGION, "NeverCreated")}),
            "UnknownResourceException",
        ),
        (
            "a nextToken this read did not issue",
            json!({"permissionArn": arn.clone(), "nextToken": "not-a-token"}),
            "InvalidNextTokenException",
        ),
        (
            "an empty nextToken",
            json!({"permissionArn": arn.clone(), "nextToken": ""}),
            "InvalidNextTokenException",
        ),
        (
            "a nextToken naming an ARN rather than a version",
            json!({"permissionArn": arn.clone(), "nextToken": encode_arn_cursor(&arn)}),
            "InvalidNextTokenException",
        ),
        (
            "a nextToken naming a version below the first one",
            json!({
                "permissionArn": arn.clone(),
                "nextToken": encode_permission_version_cursor(FIRST_PERMISSION_VERSION - 1),
            }),
            "InvalidNextTokenException",
        ),
    ];
    for (case, body, code) in cases {
        let (status, refused) = list_permission_versions(&router, &body).await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, code.to_string()),
            "case {case:?}: expected 400 {code}, got {status} with {refused:?}"
        );
    }
}
