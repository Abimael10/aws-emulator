// SPDX-License-Identifier: CC0-1.0

//! Sharing a private certificate authority: the resource type this service shares, and the
//! managed permissions Amazon Web Services publishes for it.
//!
//! The rule the rows below pin: a resource ARN is classified as shareable from the catalog
//! of resource types AWS Resource Access Manager publishes (`RESOURCE_TYPES`, the catalog
//! `ListResourceTypes` reports), which carries one entry per `(service, resource)` pair that
//! documentation names — Amazon Web Services Private CA's certificate authority among them —
//! and the managed-permission library publishes, for each such type, every permission Amazon
//! Web Services maintains for it, exactly one of which is that type's default. A type the
//! catalog does not carry is still refused by KIND, with the fixed text that refusal carries.
//!
//! Why these rows: a certificate authority is shared precisely so another principal — a
//! service principal, for an Amazon Web Services service that brokers certificates — may have
//! certificates issued from it, so a share of one that this service refuses is a CA no such
//! service can use.
//!
//! The rows vary along what the rule generalises over:
//!
//! * the FORM of the certificate authority's ARN: the Region the request signs for and
//!   another one, the owning account and another account, and an id segment that is not a
//!   UUID — the classification is about the resource TYPE, not about the id;
//! * WHICH published permission the share names: the type's default, each issuance permission
//!   of the family, and none at all, where the share is governed by the type's default;
//! * WHO the share names as its principal: an account id and an Amazon Web Services service
//!   principal;
//! * WHICH spelling a read filters the permission library by: the display spelling the
//!   catalog publishes, the hyphenated spelling a resource ARN carries, and that spelling
//!   shouted;
//! * WHICH ARNs are still refused by kind: another service's certificate, and a resource code
//!   of the private CA service that names no shareable type;
//! * WHICH types the catalog READ publishes: every entry of it, paged and filtered by Region
//!   scope, with a resource ARN of each of several of them accepted by a share — the read and
//!   the classifier are one catalog.

use serde_json::{json, Value};
use service_com_amazonaws_ram_amazonresourcesharing::ops::types::RESOURCE_REGION_SCOPE_VALUES;
use service_com_amazonaws_ram_amazonresourcesharing::state::{
    aws_managed_permission_arn, resource_type_matches, AWS_MANAGED_PERMISSIONS,
    ASSOCIATION_STATUS_ASSOCIATED, ASSOCIATION_STATUS_ASSOCIATING, FIRST_PERMISSION_VERSION,
    NON_SHAREABLE_RESOURCE_TYPE_MESSAGE, PERMISSION_TYPE_AWS_MANAGED,
    RESOURCE_REGION_SCOPE_FILTER_ALL, RESOURCE_REGION_SCOPE_GLOBAL,
    RESOURCE_REGION_SCOPE_REGIONAL, RESOURCE_TYPES,
};
use smithy_server_runtime::MultiServiceRouter;

// Every suite in this crate calls the service the one way: through `tests/all/harness.rs`.
use super::harness::*;

/// The resource type a private certificate authority's ARN names, as `RESOURCE_TYPES`
/// publishes it.
const CERTIFICATE_AUTHORITY_TYPE: &str = "acm-pca:CertificateAuthority";

/// The permission that type's family publishes as its default — the one this service attaches
/// to a share of a certificate authority that names no permission of its own.
const DEFAULT_CA_PERMISSION: &str = "AWSRAMDefaultPermissionCertificateAuthority";

/// The action a share of a certificate authority exists to grant: every published issuance
/// permission of the family allows it, and it is what a service brokering certificates from
/// the shared CA performs.
const ISSUE_CERTIFICATE_ACTION: &str = "acm-pca:IssueCertificate";

/// The permission the family publishes for REVOKING a certificate of the shared CA rather
/// than issuing one, and the action it allows in the issuance's place.
const REVOKE_CA_PERMISSION: &str = "AWSRAMRevokeCertificateCertificateAuthority";
const REVOKE_CERTIFICATE_ACTION: &str = "acm-pca:RevokeCertificate";

/// An Amazon Web Services service principal, which is what a share with a service that brokers
/// certificates from the CA names as its principal.
const SERVICE_PRINCIPAL: &str = "pca-connector-scep.amazonaws.com";

/// A Region other than the one the harness signs for.
const OTHER_REGION: &str = "eu-west-1";

/// An account other than the one that owns the shares.
const OTHER_ACCOUNT: &str = "111122223333";

async fn list_share_permissions(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/listresourcesharepermissions", Some(body)).await
}

async fn list_permissions(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/listpermissions", Some(body)).await
}

async fn get_permission(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/getpermission", Some(body)).await
}

async fn get_policies(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/getresourcepolicies", Some(body)).await
}

async fn list_resource_types(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/listresourcetypes", Some(body)).await
}

/// A string member of a reported object.
fn field(case: &str, value: &Value, member: &str) -> String {
    value
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} member, got {value:?}"))
        .to_string()
}

/// The permissions a read of a share's permission set reported, by ARN, sorted.
fn permission_arns(case: &str, body: &Value) -> Vec<String> {
    let mut arns: Vec<String> = body
        .get("permissions")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: response carried no permissions, got {body:?}"))
        .iter()
        .map(|permission| field(case, permission, "arn"))
        .collect();
    arns.sort();
    arns
}

/// The permissions a read of a share's permission set reported, as objects.
fn permissions(case: &str, body: &Value) -> Vec<Value> {
    body.get("permissions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: response carried no permissions, got {body:?}"))
}

/// Every permission the library publishes for the certificate-authority type, read off the
/// catalog rather than spelled again here.
fn certificate_authority_family() -> Vec<&'static str> {
    AWS_MANAGED_PERMISSIONS
        .iter()
        .filter(|entry| resource_type_matches(entry.resource_type, CERTIFICATE_AUTHORITY_TYPE))
        .map(|entry| entry.name)
        .collect()
}

/// One row of the sharing table: the ARN of the certificate authority shared, who with, and
/// which published permission the share names.
struct Row {
    case: &'static str,
    certificate_authority: String,
    principal: &'static str,
    /// The published permission the create names, or `None` where it names none and the
    /// share is governed by the type's default.
    permission: Option<&'static str>,
}

fn rows() -> Vec<Row> {
    let family = certificate_authority_family();
    vec![
        Row {
            case: "a CA in the request's Region shared with a service principal",
            certificate_authority: format!(
                "arn:aws:acm-pca:{REGION}:{ACCOUNT}:certificate-authority/\
                 1a2b3c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d"
            ),
            principal: SERVICE_PRINCIPAL,
            permission: Some(
                "AWSRAMBlankEndEntityCertificateAPICSRPassthroughIssuanceCertificateAuthority",
            ),
        },
        Row {
            case: "the same type shared naming no permission at all",
            certificate_authority: format!(
                "arn:aws:acm-pca:{REGION}:{ACCOUNT}:certificate-authority/\
                 2b3c4d5e-6f7a-4b8c-9d0e-1f2a3b4c5d6e"
            ),
            principal: SERVICE_PRINCIPAL,
            permission: None,
        },
        Row {
            case: "a CA in another Region",
            certificate_authority: format!(
                "arn:aws:acm-pca:{OTHER_REGION}:{ACCOUNT}:certificate-authority/\
                 3c4d5e6f-7a8b-4c9d-8e1f-2a3b4c5d6e7f"
            ),
            principal: OTHER_ACCOUNT,
            permission: Some(DEFAULT_CA_PERMISSION),
        },
        Row {
            case: "a CA owned by another account",
            certificate_authority: format!(
                "arn:aws:acm-pca:{REGION}:{OTHER_ACCOUNT}:certificate-authority/\
                 4d5e6f7a-8b9c-4d8e-9f2a-3b4c5d6e7f80"
            ),
            principal: SERVICE_PRINCIPAL,
            permission: Some(
                "AWSRAMBlankEndEntityCertificateAPIPassthroughIssuanceCertificateAuthority",
            ),
        },
        Row {
            case: "a CA whose id segment is not a UUID",
            certificate_authority: format!(
                "arn:aws:acm-pca:{REGION}:{ACCOUNT}:certificate-authority/device-enrolment"
            ),
            principal: SERVICE_PRINCIPAL,
            // The last entry of the published family, whichever it is, so the table reaches
            // one the rows above do not name.
            permission: family.last().copied(),
        },
    ]
}

#[tokio::test]
async fn a_private_certificate_authority_is_shared_and_reads_back() {
    let router = emulator();
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let name = format!("ca-share-{index}");
        let mut request = json!({
            "name": name,
            "resourceArns": [row.certificate_authority.clone()],
            "principals": [row.principal],
        });
        if let Some(permission) = row.permission {
            request["permissionArns"] = json!([aws_managed_permission_arn(permission)]);
        }

        let (status, created) = create(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: sharing a certificate authority expected 200, got {status} with \
             {created:?}"
        );
        let share = created
            .get("resourceShare")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: create reported no share, got {created:?}"));
        let share_arn = arn_of(case, &share);
        assert_share_arn(case, &share_arn);

        // Round trip: the share the create minted is the share a read reports.
        let (status, read) = get_shares(
            &router,
            &json!({"resourceOwner": "SELF", "resourceShareArns": [share_arn.clone()]}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: reading the share back expected 200, got {status} with {read:?}"
        );
        assert_eq!(
            only_share(case, &read),
            share,
            "case {case:?}: the read-back must be the record the create reported"
        );

        // The resource and the principal the create named are the entities it associated.
        for (association_type, entity) in [
            ("RESOURCE", row.certificate_authority.as_str()),
            ("PRINCIPAL", row.principal),
        ] {
            let (status, found) = get_associations(
                &router,
                &json!({"associationType": association_type, "resourceShareArns": [share_arn]}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}/{association_type}: expected 200, got {status} with {found:?}"
            );
            let reported = associations_of(case, &found);
            let entities: Vec<String> = reported
                .iter()
                .map(|association| field(case, association, "associatedEntity"))
                .collect();
            assert_eq!(
                entities,
                vec![entity.to_string()],
                "case {case:?}/{association_type}: expected the entity the create named, got \
                 {reported:?}"
            );
            let status_reported = field(case, &reported[0], "status");
            assert!(
                [
                    ASSOCIATION_STATUS_ASSOCIATING,
                    ASSOCIATION_STATUS_ASSOCIATED
                ]
                .contains(&status_reported.as_str()),
                "case {case:?}/{association_type}: expected an association that is coming up or \
                 up, got {status_reported:?}"
            );
        }

        // The share is governed by the permission it named, or — where it named none — by the
        // default the library publishes for the type.
        let governing = row.permission.unwrap_or(DEFAULT_CA_PERMISSION);
        let (status, listed) =
            list_share_permissions(&router, &json!({"resourceShareArn": share_arn})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: reading the share's permissions expected 200, got {status} with \
             {listed:?}"
        );
        assert_eq!(
            permission_arns(case, &listed),
            vec![aws_managed_permission_arn(governing)],
            "case {case:?}: expected the share to be governed by {governing:?}, got {listed:?}"
        );
        let permission = permissions(case, &listed)
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no permission reported in {listed:?}"));
        assert_eq!(
            field(case, &permission, "resourceType"),
            CERTIFICATE_AUTHORITY_TYPE,
            "case {case:?}: expected the certificate-authority resource type, got {permission:?}"
        );
        assert_eq!(
            field(case, &permission, "permissionType"),
            PERMISSION_TYPE_AWS_MANAGED,
            "case {case:?}: a published permission is Amazon Web Services' own, got \
             {permission:?}"
        );
        assert_eq!(
            field(case, &permission, "version"),
            FIRST_PERMISSION_VERSION.to_string(),
            "case {case:?}: expected the version the library publishes, got {permission:?}"
        );
    }
}

#[tokio::test]
async fn the_resource_type_catalog_publishes_the_certificate_authority_type() {
    let entry = RESOURCE_TYPES
        .iter()
        .find(|entry| entry.resource_type == CERTIFICATE_AUTHORITY_TYPE)
        .unwrap_or_else(|| {
            panic!(
                "case {CERTIFICATE_AUTHORITY_TYPE:?}: the catalog must publish the type, got \
                 {:?}",
                RESOURCE_TYPES
                    .iter()
                    .map(|entry| entry.resource_type)
                    .collect::<Vec<_>>()
            )
        });
    // A type shared only through the permissions Amazon Web Services manages for it takes no
    // customer managed permission.
    assert!(
        !entry.customer_managed_permissions,
        "case {CERTIFICATE_AUTHORITY_TYPE:?}: expected a type shared through AWS managed \
         permissions alone, got {entry:?}"
    );
}

#[tokio::test]
async fn every_published_certificate_authority_permission_reads_back() {
    let router = emulator();
    let family = certificate_authority_family();
    assert!(
        family.len() > 1,
        "case {CERTIFICATE_AUTHORITY_TYPE:?}: the library publishes a family of permissions for \
         the type, got {family:?}"
    );
    let defaults: Vec<&str> = AWS_MANAGED_PERMISSIONS
        .iter()
        .filter(|entry| {
            entry.is_resource_type_default
                && resource_type_matches(entry.resource_type, CERTIFICATE_AUTHORITY_TYPE)
        })
        .map(|entry| entry.name)
        .collect();
    assert_eq!(
        defaults,
        vec![DEFAULT_CA_PERMISSION],
        "case {CERTIFICATE_AUTHORITY_TYPE:?}: expected exactly that type's default, got \
         {defaults:?}"
    );

    for name in family {
        let arn = aws_managed_permission_arn(name);
        let (status, read) = get_permission(&router, &json!({"permissionArn": arn})).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected HTTP 200, got {status} with {read:?}"
        );
        let permission = read
            .get("permission")
            .cloned()
            .unwrap_or_else(|| panic!("case {name:?}: no permission reported in {read:?}"));
        assert_eq!(
            field(name, &permission, "name"),
            name,
            "case {name:?}: expected the published name, got {permission:?}"
        );
        assert_eq!(
            field(name, &permission, "resourceType"),
            CERTIFICATE_AUTHORITY_TYPE,
            "case {name:?}: expected the certificate-authority resource type, got {permission:?}"
        );
        assert_eq!(
            field(name, &permission, "permissionType"),
            PERMISSION_TYPE_AWS_MANAGED,
            "case {name:?}: expected AWS_MANAGED, got {permission:?}"
        );
        // A share of a certificate authority exists to let the receiving principal act on
        // the certificates of that CA: every issuance permission of the family allows the
        // issuance, and the one published for revocation allows the revocation.
        let document = field(name, &permission, "permission");
        let granted = if name == REVOKE_CA_PERMISSION {
            REVOKE_CERTIFICATE_ACTION
        } else {
            ISSUE_CERTIFICATE_ACTION
        };
        assert!(
            document.contains(granted),
            "case {name:?}: expected {granted:?} among the actions, got \
             {document:?}"
        );
    }
}

#[tokio::test]
async fn the_permission_library_filters_on_every_spelling_of_the_type() {
    let router = emulator();
    let mut expected: Vec<String> = certificate_authority_family()
        .into_iter()
        .map(aws_managed_permission_arn)
        .collect();
    expected.sort();

    // The display spelling the catalog publishes, the spelling a certificate authority's own
    // ARN carries, and that spelling shouted: `ListPermissions$resourceType` "is not case
    // sensitive".
    for spelling in [
        "acm-pca:CertificateAuthority",
        "acm-pca:certificate-authority",
        "ACM-PCA:CERTIFICATE-AUTHORITY",
    ] {
        let case = format!("resourceType {spelling:?}");
        let (status, listed) = list_permissions(&router, &json!({"resourceType": spelling})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {listed:?}"
        );
        assert_eq!(
            permission_arns(&case, &listed),
            expected,
            "case {case:?}: expected the permissions published for that type, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn the_library_publishes_exactly_the_permissions_aws_publishes_for_the_type() {
    let router = emulator();

    // What the catalog's MEMBERSHIP must be, named here rather than read off the catalog:
    // a permission the library publishes is enumerated and resolves, and a name it
    // publishes no permission under is neither. Source: the managed permissions the RAM
    // permission library publishes for `acm-pca:CertificateAuthority` — the type's default
    // permission, the two blank end-entity issuance permissions, one subordinate-CA
    // issuance permission per published certificate template (the path lengths zero
    // through three), and the revocation permission — which is what a `ListPermissions`
    // scoped to that type enumerates in any account.
    let mut published: Vec<String> = [
        "AWSRAMBlankEndEntityCertificateAPICSRPassthroughIssuanceCertificateAuthority",
        "AWSRAMBlankEndEntityCertificateAPIPassthroughIssuanceCertificateAuthority",
        "AWSRAMDefaultPermissionCertificateAuthority",
        "AWSRAMRevokeCertificateCertificateAuthority",
        "AWSRAMSubordinateCACertificatePathLen0IssuanceCertificateAuthority",
        "AWSRAMSubordinateCACertificatePathLen1IssuanceCertificateAuthority",
        "AWSRAMSubordinateCACertificatePathLen2IssuanceCertificateAuthority",
        "AWSRAMSubordinateCACertificatePathLen3IssuanceCertificateAuthority",
    ]
    .into_iter()
    .map(aws_managed_permission_arn)
    .collect();
    published.sort();

    let case = "the permissions published for a private certificate authority";
    let (status, listed) = list_permissions(
        &router,
        &json!({"resourceType": CERTIFICATE_AUTHORITY_TYPE}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {listed:?}"
    );
    assert_eq!(
        permission_arns(case, &listed),
        published,
        "case {case:?}: the library must publish exactly those permissions for the type, got \
         {listed:?}"
    );

    // Names that look like members of the family and are not permissions: a path length
    // beyond the published templates, a template the library publishes no permission for,
    // and the family's name shouted. A name the library does not publish is enumerated
    // nowhere and resolves to nothing, so a client cannot attach an ARN that does not
    // exist.
    for name in [
        "AWSRAMSubordinateCACertificatePathLen9IssuanceCertificateAuthority",
        "AWSRAMRootCACertificateIssuanceCertificateAuthority",
        "awsramdefaultpermissioncertificateauthority",
    ] {
        let arn = aws_managed_permission_arn(name);
        assert!(
            !permission_arns(case, &listed).contains(&arn),
            "case {name:?}: a permission the library does not publish must not be enumerated, \
             got {listed:?}"
        );
        let (status, refused) = get_permission(&router, &json!({"permissionArn": arn})).await;
        assert_eq!(
            status, 400,
            "case {name:?}: expected HTTP 400, got {status} with {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "UnknownResourceException",
            "case {name:?}: expected UnknownResourceException, got {refused:?}"
        );
    }
}

#[tokio::test]
async fn an_arn_of_a_type_the_catalog_does_not_publish_is_still_refused_by_kind() {
    let router = emulator();
    for (case, arn) in [
        (
            "another service's certificate",
            format!("arn:aws:acm:{REGION}:{ACCOUNT}:certificate/0a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d"),
        ),
        (
            "a resource code of the private CA service that names no shareable type",
            format!("arn:aws:acm-pca:{REGION}:{ACCOUNT}:certificate/0a1b2c3d4e5f6a7b"),
        ),
    ] {
        let (status, body) = get_policies(&router, &json!({"resourceArns": [arn]})).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "MalformedArnException",
            "case {case:?}: expected the ARN-kind refusal, got {body:?}"
        );
        assert_eq!(
            error_message(&body),
            NON_SHAREABLE_RESOURCE_TYPE_MESSAGE,
            "case {case:?}: expected the fixed non-shareable text, got {:?}",
            error_message(&body)
        );
    }
}

// ---------------------------------------------------------------------------
// The read side of the same catalog: the resource types this service publishes.
//
// The rule the suites below pin: the resource types this service reports are the one
// catalog its ARN classifier consults, each reported with the display name the catalog
// publishes, the service code half of that name as the service whose resources they
// are, and the Region scope of that service's resources — so a type a client is told
// this service can share is a type a share may carry, and a type it is not told about
// is one no share is accepted for.
// ---------------------------------------------------------------------------

/// One row of the catalog table: a published resource type, an ARN of a resource of
/// that type spelled the way the OWNING service spells it, and the Region scope of
/// such resources.
///
/// The ARN forms are each service's own, and the scope follows `ResourceRegionScope`:
/// `GLOBAL` is a resource reachable "from any Amazon Web Services Region", which such a
/// service's ARN names none of, and `REGIONAL` one reachable only in the Region it
/// exists in, which its ARN carries.
struct CatalogRow {
    case: &'static str,
    resource_type: &'static str,
    arn: String,
    region_scope: &'static str,
}

fn catalog_rows() -> Vec<CatalogRow> {
    vec![
        CatalogRow {
            case: "a private certificate authority",
            resource_type: "acm-pca:CertificateAuthority",
            arn: format!(
                "arn:aws:acm-pca:{REGION}:{ACCOUNT}:certificate-authority/\
                 5e6f7a8b-9c0d-4e1f-8a2b-3c4d5e6f7a8b"
            ),
            region_scope: RESOURCE_REGION_SCOPE_REGIONAL,
        },
        CatalogRow {
            case: "a resource code an ARN spells the way the type publishes it",
            resource_type: "ec2:Subnet",
            arn: format!("arn:aws:ec2:{REGION}:{ACCOUNT}:subnet/subnet-0a1b2c3d4e5f6a7b8"),
            region_scope: RESOURCE_REGION_SCOPE_REGIONAL,
        },
        CatalogRow {
            case: "a resource code an ARN hyphenates",
            resource_type: "ec2:PrefixList",
            arn: format!("arn:aws:ec2:{REGION}:{ACCOUNT}:prefix-list/pl-0a1b2c3d4e5f6a7b8"),
            region_scope: RESOURCE_REGION_SCOPE_REGIONAL,
        },
        CatalogRow {
            case: "a resource field that is the resource code alone",
            resource_type: "glue:Catalog",
            arn: format!("arn:aws:glue:{REGION}:{ACCOUNT}:catalog"),
            region_scope: RESOURCE_REGION_SCOPE_REGIONAL,
        },
        CatalogRow {
            case: "a resource code nested under another resource",
            resource_type: "s3-outposts:Bucket",
            arn: format!(
                "arn:aws:s3-outposts:{REGION}:{ACCOUNT}:outpost/op-0a1b2c3d4e5f6a7b8/bucket/\
                 device-enrolment"
            ),
            region_scope: RESOURCE_REGION_SCOPE_REGIONAL,
        },
        CatalogRow {
            case: "a resource of a service that names no Region",
            resource_type: "networkmanager:CoreNetwork",
            arn: format!("arn:aws:networkmanager::{ACCOUNT}:core-network/core-network-0a1b2c3d4"),
            region_scope: RESOURCE_REGION_SCOPE_GLOBAL,
        },
        CatalogRow {
            case: "a Region-less resource of a service whose other resources are Regional",
            resource_type: "ec2:Ipam",
            arn: format!("arn:aws:ec2::{ACCOUNT}:ipam/ipam-0a1b2c3d4e5f6a7b8"),
            region_scope: RESOURCE_REGION_SCOPE_GLOBAL,
        },
    ]
}

/// The entries one `ListResourceTypes` page reported.
fn reported_types(case: &str, body: &Value) -> Vec<Value> {
    body.get("resourceTypes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: response carried no resourceTypes, got {body:?}"))
}

/// Every entry a read reports, collected page by page, so one walk serves whatever page
/// size and scope filter the case asks for.
async fn walk_catalog(
    router: &MultiServiceRouter,
    case: &str,
    max_results: Option<usize>,
    scope: Option<&str>,
) -> Vec<Value> {
    let mut collected = Vec::new();
    let mut next_token: Option<String> = None;
    // One page per entry at worst, plus the last page that carries no token: a walk
    // that asks for more pages than that is not converging.
    let page_budget = RESOURCE_TYPES.len() + 2;
    for _ in 0..page_budget {
        let mut request = json!({});
        if let Some(size) = max_results {
            request["maxResults"] = json!(size);
        }
        if let Some(scope) = scope {
            request["resourceRegionScope"] = json!(scope);
        }
        if let Some(token) = &next_token {
            request["nextToken"] = json!(token);
        }
        let (status, body) = list_resource_types(router, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: listing the catalog expected 200, got {status} with {body:?}"
        );
        let page = reported_types(case, &body);
        if let Some(size) = max_results {
            assert!(
                page.len() <= size,
                "case {case:?}: a page of at most {size} entries was asked for, got {}",
                page.len()
            );
        }
        collected.extend(page);
        next_token = body
            .get("nextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if next_token.is_none() {
            return collected;
        }
    }
    panic!("case {case:?}: the catalog still reported a nextToken after {page_budget} pages");
}

/// The `resourceType` of each reported entry, in the order they were reported.
fn type_names(case: &str, entries: &[Value]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| field(case, entry, "resourceType"))
        .collect()
}

#[tokio::test]
async fn the_catalog_read_publishes_every_type_this_service_shares() {
    let router = emulator();
    let case = "the whole catalog";
    let reported = walk_catalog(&router, case, None, None).await;
    let published: Vec<String> = RESOURCE_TYPES
        .iter()
        .map(|entry| entry.resource_type.to_string())
        .collect();
    assert_eq!(
        type_names(case, &reported),
        published,
        "case {case:?}: expected every resource type this service shares"
    );

    for entry in &reported {
        let name = field(case, entry, "resourceType");
        // The service whose resources these are is the service code half of the
        // display name, which is also the service field an ARN of one carries.
        let expected_service = name
            .split_once(':')
            .map(|(service, _)| service.to_string())
            .unwrap_or_else(|| panic!("case {name:?}: expected a <service>:<resource> name"));
        assert_eq!(
            field(&name, entry, "serviceName"),
            expected_service,
            "case {name:?}: expected the service the resources belong to, got {entry:?}"
        );
        let scope = field(&name, entry, "resourceRegionScope");
        assert!(
            RESOURCE_REGION_SCOPE_VALUES.contains(&scope.as_str()),
            "case {name:?}: expected one of {RESOURCE_REGION_SCOPE_VALUES:?}, got {scope:?}"
        );
    }
}

#[tokio::test]
async fn every_published_type_is_a_type_a_share_may_carry() {
    let router = emulator();
    let reported = walk_catalog(&router, "the whole catalog", None, None).await;

    for (index, row) in catalog_rows().into_iter().enumerate() {
        let case = row.case;
        let entry = reported
            .iter()
            .find(|entry| field(case, entry, "resourceType") == row.resource_type)
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the read must publish {:?}, got {:?}",
                    row.resource_type,
                    type_names(case, &reported)
                )
            });
        assert_eq!(
            field(case, &entry, "resourceRegionScope"),
            row.region_scope,
            "case {case:?}: expected the scope of that service's resources, got {entry:?}"
        );

        // The read and the classifier are one catalog: a type the read publishes is a
        // type a share accepts a resource of, however the owning service spells the
        // resource code in the ARN.
        let (status, created) = create(
            &router,
            &json!({
                "name": format!("catalog-share-{index}"),
                "resourceArns": [row.arn.clone()],
                "principals": [OTHER_ACCOUNT],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: sharing {:?} expected 200, got {status} with {created:?}",
            row.arn
        );
        let share_arn = arn_of(
            case,
            &created
                .get("resourceShare")
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: create reported no share")),
        );
        let (status, found) = get_associations(
            &router,
            &json!({"associationType": "RESOURCE", "resourceShareArns": [share_arn]}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: reading the associations expected 200, got {status} with {found:?}"
        );
        let entities: Vec<String> = associations_of(case, &found)
            .iter()
            .map(|association| field(case, association, "associatedEntity"))
            .collect();
        assert_eq!(
            entities,
            vec![row.arn],
            "case {case:?}: expected the resource the share named, got {found:?}"
        );
    }
}

#[tokio::test]
async fn the_catalog_read_pages_and_filters_by_region_scope() {
    let router = emulator();
    let whole = type_names("the whole catalog", &walk_catalog(&router, "all", None, None).await);

    // A page size narrower than the catalog, the widest the model admits, and the
    // filter that keeps everything, which is also what naming no filter gets.
    for (case, max_results, scope) in [
        ("one entry per page", Some(1), None),
        ("two entries per page", Some(2), None),
        ("the whole catalog in one page", Some(500), None),
        (
            "the filter that keeps every scope",
            None,
            Some(RESOURCE_REGION_SCOPE_FILTER_ALL),
        ),
    ] {
        let walked = type_names(case, &walk_catalog(&router, case, max_results, scope).await);
        assert_eq!(
            walked, whole,
            "case {case:?}: expected the same catalog in the same order"
        );
    }

    // Each scope keeps the entries of that scope alone, and the two together are the
    // whole catalog: every entry has exactly one scope.
    let mut partitioned = Vec::new();
    for scope in [RESOURCE_REGION_SCOPE_GLOBAL, RESOURCE_REGION_SCOPE_REGIONAL] {
        let entries = walk_catalog(&router, scope, Some(3), Some(scope)).await;
        for entry in &entries {
            assert_eq!(
                field(scope, entry, "resourceRegionScope"),
                scope,
                "case {scope:?}: expected only entries of that scope, got {entry:?}"
            );
        }
        partitioned.extend(type_names(scope, &entries));
    }
    partitioned.sort();
    let mut expected = whole.clone();
    expected.sort();
    assert_eq!(
        partitioned, expected,
        "case \"GLOBAL and REGIONAL\": the two scopes together are the whole catalog"
    );
}

#[tokio::test]
async fn the_catalog_read_refuses_a_token_and_a_scope_it_never_issued() {
    let router = emulator();
    for (case, request, expected) in [
        (
            "a nextToken that is not one this service issued",
            json!({"nextToken": "not-a-token"}),
            "InvalidNextTokenException",
        ),
        (
            "a nextToken naming no resource type",
            json!({"nextToken": hex::encode("ec2")}),
            "InvalidNextTokenException",
        ),
        (
            "a resourceRegionScope the model does not admit",
            json!({"resourceRegionScope": "EVERYWHERE"}),
            // The member is a modelled enum, so a value outside it is refused by the
            // constraint walk ahead of the read.
            "ValidationException",
        ),
    ] {
        let (status, body) = list_resource_types(&router, &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            expected,
            "case {case:?}: expected {expected}, got {body:?}"
        );
    }
}

#[tokio::test]
async fn sharing_a_certificate_authority_records_the_source_it_names() {
    let router = emulator();
    let case = "a CA shared with a service principal under a source constraint";
    let certificate_authority = format!(
        "arn:aws:acm-pca:{REGION}:{ACCOUNT}:certificate-authority/\
         6f7a8b9c-0d1e-4f2a-8b3c-4d5e6f7a8b9c"
    );
    let share = create_ok(
        &router,
        case,
        &json!({
            "name": "ca-share-with-source",
            "resourceArns": [certificate_authority.clone()],
            "principals": [SERVICE_PRINCIPAL],
            "sources": [ACCOUNT],
            "permissionArns": [aws_managed_permission_arn(
                "AWSRAMBlankEndEntityCertificateAPICSRPassthroughIssuanceCertificateAuthority"
            )],
            "tags": [{"key": "purpose", "value": "device-enrolment"}],
        }),
    )
    .await;
    let share_arn = arn_of(case, &share);

    // Every entity the create named is associated under its own association type —
    // the source constraint among them.
    for (association_type, entity) in [
        ("RESOURCE", certificate_authority.as_str()),
        ("PRINCIPAL", SERVICE_PRINCIPAL),
        ("SOURCE", ACCOUNT),
    ] {
        let (status, found) = get_associations(
            &router,
            &json!({"associationType": association_type, "resourceShareArns": [share_arn.clone()]}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}/{association_type}: expected 200, got {status} with {found:?}"
        );
        let entities: Vec<String> = associations_of(case, &found)
            .iter()
            .map(|association| field(case, association, "associatedEntity"))
            .collect();
        assert_eq!(
            entities,
            vec![entity.to_string()],
            "case {case:?}/{association_type}: expected the entity the create named, got {found:?}"
        );
    }

    // Round trip: the share the create minted, tags and all, is the share a read
    // reports.
    let (status, read) = get_shares(
        &router,
        &json!({"resourceOwner": "SELF", "resourceShareArns": [share_arn]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading the share back expected 200, got {status} with {read:?}"
    );
    assert_eq!(
        only_share(case, &read),
        share,
        "case {case:?}: the read-back must be the record the create reported"
    );
    assert_eq!(
        tags_of(case, &share),
        vec![("purpose".to_string(), "device-enrolment".to_string())],
        "case {case:?}: expected the tag the create attached, got {share:?}"
    );
}
