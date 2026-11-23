// SPDX-License-Identifier: CC0-1.0

//! A managed permission Resource Access Manager publishes for a resource type it shares is
//! reported by the share-scoped read of every share that permission governs.
//!
//! The rule under test: **RAM governs a resource share by the managed permissions it publishes
//! for the resource types the share shares, so each of those permissions resolves by ARN in the
//! service, and `ListResourceSharePermissions` reports one `ResourceSharePermissionSummary` per
//! permission currently governing the share** — carrying that permission's ARN, the resource
//! type it applies to, the attached version as a string, whether that version is the
//! permission's default, the permission's feature set and the status of its association with
//! THAT share. A share comes to be governed by a permission in three ways, and the read answers
//! the same for each: the create request named it in `permissionArns`,
//! `AssociateResourceSharePermission` attached it afterwards ("Adds or replaces the RAM
//! permission for a resource type included in a resource share"), or it is the published
//! default of a resource type the share shares and the share names none of its own.
//!
//! So the rows run over what the rule generalises over:
//!
//! * WHICH resource type the share shares, and so which published permission governs it: the
//!   Image Builder component the recording named is ONE row beside its container and image
//!   recipes, an Image Builder image, a managed prefix list and a license configuration —
//!   whose service code carries a hyphen — so the rule is driven on types the issue did not
//!   cite;
//! * HOW the share came to be governed by it: named at create, attached afterwards, or derived
//!   from what the share shares;
//! * WHETHER the permission is its resource type's default or one of the further permissions
//!   the library publishes for that type;
//! * and, over the WHOLE published catalog, that every default it publishes round-trips from
//!   the documented discovery call (`ListPermissions` scoped to a resource type) through an
//!   attach to the share-scoped read.
//!
//! This suite lives here, beside the other suites of the assembled emulator, because the owning
//! crate still roots its integration tests at a hand-declared `tests/all.rs`, which a fix may
//! not edit; the router's `tests/all/` picks a new suite up with nothing to declare. Every call
//! goes through a router with the owning service mounted on one shared engine, exactly as the
//! production router mounts it, so each assertion exercises the real wire path.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_ram_amazonresourcesharing::state::{
    aws_managed_permission_arn, AwsManagedPermission, AWS_MANAGED_PERMISSIONS,
    PERMISSION_FEATURE_SET_STANDARD, SHARE_PERMISSION_STATUS_ASSOCIATED,
};
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 header naming `ram` in `us-west-2`: the routing key this service claims on, and the
/// source of the Region the ARNs it mints carry.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-west-2/ram/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=deadbeef";

/// The Region every request below signs for.
const REGION: &str = "us-west-2";

/// The account every resource ARN below names: the emulator's own.
const ACCOUNT: &str = "000000000000";

/// A fresh emulator: one router with Resource Access Manager mounted over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ram_amazonresourcesharing::EmulatedService::new(engine),
    );
    router
}

/// Send one restJson1 request and return its status and JSON body.
async fn call(router: &MultiServiceRouter, uri: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", format!("ram.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", AUTHORIZATION)
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body is serializable"),
        ))
        .expect("a valid request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Send one request that must answer 200, and return its body.
async fn ok(router: &MultiServiceRouter, uri: &str, body: &Value, case: &str) -> Value {
    let (status, parsed) = call(router, uri, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {uri}, got {status} with {parsed:?}"
    );
    parsed
}

/// Create a resource share and answer its ARN.
async fn share(router: &MultiServiceRouter, case: &str, request: &Value) -> String {
    let created = ok(router, "/createresourceshare", request, case).await;
    created
        .get("resourceShare")
        .and_then(|share| share.get("resourceShareArn"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: create reported no share ARN, got {created:?}"))
        .to_string()
}

/// The permissions a read reported, whichever read reported them.
fn permissions_of(case: &str, body: &Value) -> Vec<Value> {
    body.get("permissions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: response carried no permissions, got {body:?}"))
}

/// The permissions the share `share_arn` is governed by, as the share-scoped read reports them.
async fn share_permissions(router: &MultiServiceRouter, case: &str, share_arn: &str) -> Vec<Value> {
    let listed = ok(
        router,
        "/listresourcesharepermissions",
        &json!({"resourceShareArn": share_arn}),
        case,
    )
    .await;
    permissions_of(case, &listed)
}

/// A string member of a reported permission.
fn field(case: &str, permission: &Value, member: &str) -> String {
    permission
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} member, got {permission:?}"))
        .to_string()
}

/// The catalog entry the library publishes under `name`.
///
/// The row below names the permission; its resource type and the version it is published at are
/// read off the catalog rather than spelled a second time, so a row cannot disagree with it.
fn published(name: &str) -> &'static AwsManagedPermission {
    AWS_MANAGED_PERMISSIONS
        .iter()
        .find(|entry| entry.name == name)
        .unwrap_or_else(|| panic!("the catalog publishes a managed permission named {name:?}"))
}

/// Assert that `reported` is the summary of the published permission `entry` as a read THROUGH A
/// SHARE reports it.
fn assert_governs(case: &str, reported: &Value, entry: &AwsManagedPermission) {
    assert_eq!(
        field(case, reported, "arn"),
        aws_managed_permission_arn(entry.name),
        "case {case:?}: expected the attached permission's ARN, got {reported:?}"
    );
    assert_eq!(
        field(case, reported, "resourceType"),
        entry.resource_type,
        "case {case:?}: expected the resource type the permission applies to, got {reported:?}"
    );
    assert_eq!(
        field(case, reported, "version"),
        entry.version.to_string(),
        "case {case:?}: expected the attached version as a string, got {reported:?}"
    );
    assert_eq!(
        reported.get("defaultVersion").and_then(Value::as_bool),
        Some(true),
        "case {case:?}: the attached version is the permission's default one, got {reported:?}"
    );
    assert_eq!(
        field(case, reported, "featureSet"),
        PERMISSION_FEATURE_SET_STANDARD,
        "case {case:?}: expected the permission's feature set, got {reported:?}"
    );
    assert_eq!(
        field(case, reported, "status"),
        SHARE_PERMISSION_STATUS_ASSOCIATED,
        "case {case:?}: a permission governing the share reports its association with it, got \
         {reported:?}"
    );
}

/// The one permission a share must be governed by, as the share-scoped read reports it.
fn only_permission(case: &str, reported: &[Value]) -> Value {
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: expected exactly one permission, got {reported:?}"
    );
    reported[0].clone()
}

/// One row: a resource type the service shares, a permission the library publishes for it, and
/// the ARN of a resource of that type.
struct GovernedType {
    case: &'static str,
    /// The published permission's name; its resource type and version come from the catalog.
    permission: &'static str,
    /// A resource of that type, spelled the way the owning service spells it in an ARN.
    resource_arn: &'static str,
}

/// The rows, spanning the resource types and the two kinds of published permission; the resource
/// type of each is whatever the catalog publishes the named permission for.
const GOVERNED_TYPES: &[GovernedType] = &[
    GovernedType {
        case: "an Image Builder component, the type the recording shared",
        permission: "AWSRAMDefaultPermissionImageBuilderComponent",
        resource_arn: "arn:aws:imagebuilder:us-west-2:000000000000:component/hardening/1.0.0/1",
    },
    GovernedType {
        case: "an Image Builder container recipe, whose resource code is hyphenated in its ARN",
        permission: "AWSRAMDefaultPermissionImageBuilderContainerRecipe",
        resource_arn: "arn:aws:imagebuilder:us-west-2:000000000000:container-recipe/app/1.0.0",
    },
    GovernedType {
        case: "an Image Builder image recipe",
        permission: "AWSRAMDefaultPermissionImageBuilderImageRecipe",
        resource_arn: "arn:aws:imagebuilder:us-west-2:000000000000:image-recipe/app/1.0.0",
    },
    GovernedType {
        case: "an Image Builder image",
        permission: "AWSRAMDefaultPermissionImageBuilderImage",
        resource_arn: "arn:aws:imagebuilder:us-west-2:000000000000:image/app/1.0.0/1",
    },
    GovernedType {
        case: "a managed prefix list",
        permission: "AWSRAMDefaultPermissionPrefixList",
        resource_arn: "arn:aws:ec2:us-west-2:000000000000:prefix-list/pl-0a1b2c3d4e5f60718",
    },
    GovernedType {
        case: "a license configuration, whose service code carries a hyphen",
        permission: "AWSRAMDefaultPermissionLicenseConfiguration",
        resource_arn:
            "arn:aws:license-manager:us-west-2:000000000000:license-configuration/lic-0a1b2c3d",
    },
    GovernedType {
        case: "a Glue database governed by a published permission that is not its type's default",
        permission: "AWSRAMPermissionGlueDatabaseReadWrite",
        resource_arn: "arn:aws:glue:us-west-2:000000000000:database/analytics",
    },
];

#[tokio::test]
async fn a_permission_named_at_create_governs_the_share_and_is_reported_by_the_share_read() {
    let router = emulator();
    for (index, row) in GOVERNED_TYPES.iter().enumerate() {
        let entry = published(row.permission);
        let share_arn = share(
            &router,
            row.case,
            &json!({
                "name": format!("named-at-create-{index}"),
                "resourceArns": [row.resource_arn],
                "permissionArns": [aws_managed_permission_arn(entry.name)],
            }),
        )
        .await;
        let reported = share_permissions(&router, row.case, &share_arn).await;
        assert_governs(row.case, &only_permission(row.case, &reported), entry);
    }
}

#[tokio::test]
async fn a_permission_attached_after_the_create_governs_the_share_and_is_reported() {
    let router = emulator();
    for (index, row) in GOVERNED_TYPES.iter().enumerate() {
        let entry = published(row.permission);
        let share_arn = share(
            &router,
            row.case,
            &json!({
                "name": format!("attached-afterwards-{index}"),
                "resourceArns": [row.resource_arn],
            }),
        )
        .await;
        let attached = ok(
            &router,
            "/associateresourcesharepermission",
            &json!({
                "resourceShareArn": share_arn,
                "permissionArn": aws_managed_permission_arn(entry.name),
                "replace": true,
            }),
            row.case,
        )
        .await;
        assert_eq!(
            attached.get("returnValue").and_then(Value::as_bool),
            Some(true),
            "case {:?}: the attach reports that it succeeded, got {attached:?}",
            row.case
        );

        let reported = share_permissions(&router, row.case, &share_arn).await;
        assert_governs(row.case, &only_permission(row.case, &reported), entry);
    }
}

#[tokio::test]
async fn the_default_permission_of_a_shared_type_governs_a_share_that_names_none() {
    let router = emulator();
    for (index, row) in GOVERNED_TYPES
        .iter()
        .filter(|row| published(row.permission).is_resource_type_default)
        .enumerate()
    {
        let entry = published(row.permission);
        let share_arn = share(
            &router,
            row.case,
            &json!({
                "name": format!("derived-default-{index}"),
                "resourceArns": [row.resource_arn],
            }),
        )
        .await;
        let reported = share_permissions(&router, row.case, &share_arn).await;
        assert_governs(row.case, &only_permission(row.case, &reported), entry);
    }
}

#[tokio::test]
async fn a_permission_is_discoverable_by_its_resource_type_before_it_is_attached() {
    // The documented way to find the ARN to pass to `CreateResourceShare(permissionArns=…)` or
    // `AssociateResourceSharePermission` is a `ListPermissions` scoped to the resource type, so
    // every permission a share can be governed by is reported by that read too — the read the
    // rows below resolve their ARN through.
    let router = emulator();
    for row in GOVERNED_TYPES {
        let entry = published(row.permission);
        let listed = ok(
            &router,
            "/listpermissions",
            &json!({"resourceType": entry.resource_type}),
            row.case,
        )
        .await;
        let arns: Vec<String> = permissions_of(row.case, &listed)
            .iter()
            .map(|permission| field(row.case, permission, "arn"))
            .collect();
        assert!(
            arns.contains(&aws_managed_permission_arn(entry.name)),
            "case {:?}: a read scoped to {:?} reports the permission published for it, got \
             {arns:?}",
            row.case,
            entry.resource_type
        );
    }
}

#[tokio::test]
async fn every_published_default_round_trips_from_discovery_to_the_share_scoped_read() {
    // The whole published catalog, not only the rows above: for every resource type the catalog
    // publishes a default permission for, that permission is discoverable by its resource type,
    // attachable to a share, and reported by the share-scoped read.
    let router = emulator();
    let defaults: Vec<&AwsManagedPermission> = AWS_MANAGED_PERMISSIONS
        .iter()
        .filter(|entry| entry.is_resource_type_default)
        .collect();
    assert!(
        defaults.len() > GOVERNED_TYPES.len(),
        "the catalog publishes a default for more resource types than the rows above name, got {}",
        defaults.len()
    );

    for (index, entry) in defaults.iter().enumerate() {
        let case = entry.name;
        let arn = aws_managed_permission_arn(entry.name);
        let listed = ok(
            &router,
            "/listpermissions",
            &json!({"resourceType": entry.resource_type}),
            case,
        )
        .await;
        let arns: Vec<String> = permissions_of(case, &listed)
            .iter()
            .map(|permission| field(case, permission, "arn"))
            .collect();
        assert!(
            arns.contains(&arn),
            "case {case:?}: a read scoped to {:?} reports the permission published for it, got \
             {arns:?}",
            entry.resource_type
        );

        // A resource of that type: the service code is the ARN's service field and the resource
        // code one of the segments of its resource field, which is how `Resource$type` is read
        // back off it.
        let (service, code) = entry
            .resource_type
            .split_once(':')
            .unwrap_or_else(|| panic!("case {case:?}: {:?} names no resource type", entry.resource_type));
        let resource_arn = format!(
            "arn:aws:{service}:{REGION}:{ACCOUNT}:{}/shared-{index}",
            code.to_ascii_lowercase()
        );
        let share_arn = share(
            &router,
            case,
            &json!({
                "name": format!("published-default-{index}"),
                "resourceArns": [resource_arn],
                "permissionArns": [arn.clone()],
            }),
        )
        .await;
        let reported = share_permissions(&router, case, &share_arn).await;
        assert_governs(case, &only_permission(case, &reported), entry);
    }
}
