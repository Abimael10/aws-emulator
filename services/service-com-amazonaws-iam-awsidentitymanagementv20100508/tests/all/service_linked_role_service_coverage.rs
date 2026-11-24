// SPDX-License-Identifier: CC0-1.0

//! That the registry of published service-linked roles carries the service principals the
//! emulated services' own flows are driven through, end to end over the awsQuery wire.
//!
//! The rule under test: **IAM mints a service-linked role for every AWS service principal
//! that has registered one — under the name that service publishes for it — so a
//! `CreateServiceLinkedRole` naming such a principal is SERVED, whatever shape the
//! principal takes, and the role is filed under the reserved path and reads back under the
//! published name.** `AWSServiceName` is "the service principal for the Amazon Web Services
//! service to which this role is attached … To find the exact service principal for your
//! service-linked role, see Amazon Web Services services that work with IAM … Look for the
//! services that have Yes in the Service-Linked Role column"
//! (`smithy_query(query="doc", shape="CreateServiceLinkedRoleRequest",
//! member="AWSServiceName")`) — every service in that column is a principal the create
//! serves, so a principal the registry fails to carry is refused where AWS mints, and a
//! service whose flows begin by establishing its role cannot be driven at all.
//!
//! `tests/all/service_linked_role_registry.rs` is the suite for WHICH values are served
//! versus refused as naming no service, and `tests/all/service_provisioned_roles.rs` and
//! `tests/all/service_linked_role_published_names.rs` assert the published-name rule over
//! the whole catalog. This suite is the one that pins the CONTENT of the catalog for the
//! services this emulator serves: each row below is a service whose flows establish its
//! role before anything else, so a row dropped from the registry is a service that stops
//! working rather than one name read back differently.
//!
//! The rows span what the rule generalises over: the number of labels a principal carries
//! (one service label, a feature label ahead of the service's, a feature of a service
//! reached through a hyphenated label), the shape of its service label (one word,
//! hyphenated, carrying an API-version-like word), whether the published name is the
//! service's own name, a feature's, or a name suffixed with the feature it enables, the
//! ASCII case the request spells the principal in, and whether a `CustomSuffix` comes with
//! it.
//!
//! The awsQuery harness (router, call, element) is the one `tests/all/user_lifecycle.rs`
//! owns, so every suite in this crate builds and reads a request the same way.

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state::PUBLISHED_SERVICE_LINKED_ROLE_NAMES;

use crate::user_lifecycle::{account_id, call, element, elements, error_code, router};

/// The path IAM reserves for service-linked roles.
const SERVICE_LINKED_ROLE_PATH: &str = "/aws-service-role/";

/// One row: a service principal an emulated service's flows establish a role for, and the
/// role name that service's own documentation publishes for it.
struct Registered {
    case: &'static str,
    service_principal: &'static str,
    published_role_name: &'static str,
}

/// The services whose flows begin by establishing their service-linked role, written out
/// here by hand so this suite pins the catalog's CONTENT independently of the table the
/// service reads.
///
/// Source: the "Using service-linked roles for <service>" page of each service's own user
/// guide — the same documentation `src/state.rs`'s `PUBLISHED_SERVICE_LINKED_ROLE_NAMES`
/// cites — reached from AWS's "Amazon Web Services services that work with IAM" table.
/// Rows include services this emulator serves that no report named, so the rule is asserted
/// past the principals any one report cited.
const REGISTERED: &[Registered] = &[
    Registered {
        case: "a one-word service label",
        service_principal: "braket.amazonaws.com",
        published_role_name: "AWSServiceRoleForAmazonBraket",
    },
    Registered {
        // Amazon OpenSearch Service answers to two principals, each with a role of its own:
        // the Elasticsearch-era `es` endpoint prefix and the service's current label.
        case: "a service's legacy principal, whose role name is its legacy name",
        service_principal: "es.amazonaws.com",
        published_role_name: "AWSServiceRoleForAmazonElasticsearchService",
    },
    Registered {
        case: "a service's current principal beside that legacy one",
        service_principal: "opensearchservice.amazonaws.com",
        published_role_name: "AWSServiceRoleForAmazonOpenSearchService",
    },
    Registered {
        case: "a hyphenated feature label of a service",
        service_principal: "license-manager-linux-subscriptions.amazonaws.com",
        published_role_name: "AWSServiceRoleForAWSLicenseManagerLinuxSubscriptions",
    },
    Registered {
        case: "a second hyphenated feature label of the same service",
        service_principal: "license-manager-user-subscriptions.amazonaws.com",
        published_role_name: "AWSServiceRoleForAWSLicenseManagerUserSubscriptions",
    },
    Registered {
        case: "a service whose published name ends in a word of its own",
        service_principal: "license-manager.amazonaws.com",
        published_role_name: "AWSServiceRoleForAWSLicenseManagerRole",
    },
    Registered {
        case: "a feature label ahead of the service's own",
        service_principal: "mediapipelines.chime.amazonaws.com",
        published_role_name: "AWSServiceRoleForAmazonChimeSDKMediaPipelines",
    },
    Registered {
        case: "a feature label ahead of a service's API label",
        service_principal: "observability.aoss.amazonaws.com",
        published_role_name: "AWSServiceRoleForAmazonOpenSearchServerless",
    },
    Registered {
        case: "a service label that is two words with no separator",
        service_principal: "observabilityadmin.amazonaws.com",
        published_role_name: "AWSServiceRoleForObservabilityAdmin",
    },
    Registered {
        case: "a published name suffixed with the feature its principal names",
        service_principal: "telemetry-enablement.observabilityadmin.amazonaws.com",
        published_role_name: "AWSServiceRoleForObservabilityAdmin_TelemetryEnablement",
    },
];

/// The role names the account reports under the reserved path of `service_principal`.
async fn roles_linked_to(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    service_principal: &str,
) -> Vec<String> {
    let prefix = format!("{SERVICE_LINKED_ROLE_PATH}{service_principal}/");
    let (status, listed) = call(router, "ListRoles", &[("PathPrefix", prefix.as_str())]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the listing must be served, got {status}: {listed}"
    );
    elements(&listed, "RoleName")
}

/// RULE: a service principal a service of this emulator establishes a role for is served —
/// the create answers 200 with the name AWS publishes for that service, filed under the
/// reserved path of that principal — and the stored record is what a read of that name
/// answers with.
#[tokio::test]
async fn every_service_principal_an_emulated_flow_needs_is_served_and_reads_back() {
    for row in REGISTERED {
        let case = row.case;
        // The row is the catalog's own row: a row this suite writes out that the service
        // does not carry is a registry gap, which is what this asserts first.
        assert!(
            PUBLISHED_SERVICE_LINKED_ROLE_NAMES.iter().any(|(principal, name)| {
                *principal == row.service_principal && *name == row.published_role_name
            }),
            "case {case:?}: expected the registry to publish {:?} for {:?}",
            row.published_role_name,
            row.service_principal
        );

        // One account per row: the rows are independent services.
        let router = router();
        let (status, created) = call(
            &router,
            "CreateServiceLinkedRole",
            &[("AWSServiceName", row.service_principal)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create for {:?} to be served, got {status}: {created}",
            row.service_principal
        );
        assert_eq!(
            element(&created, "RoleName"),
            row.published_role_name,
            "case {case:?}: expected the published role name {:?}, got {:?}",
            row.published_role_name,
            element(&created, "RoleName")
        );
        let expected_path = format!("{SERVICE_LINKED_ROLE_PATH}{}/", row.service_principal);
        assert_eq!(
            element(&created, "Path"),
            expected_path,
            "case {case:?}: expected the reserved path {expected_path:?}, got {:?}",
            element(&created, "Path")
        );
        let expected_arn = format!(
            "arn:aws:iam::{}:role{expected_path}{}",
            account_id(),
            row.published_role_name
        );
        assert_eq!(
            element(&created, "Arn"),
            expected_arn,
            "case {case:?}: expected the ARN {expected_arn:?}, got {:?}",
            element(&created, "Arn")
        );

        // The round trip: the created record is the one a read of the published name
        // answers with, which is the name every client of that service asks for.
        let (status, fetched) = call(
            &router,
            "GetRole",
            &[("RoleName", row.published_role_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetRole on {:?} to be served, got {status}: {fetched}",
            row.published_role_name
        );
        assert_eq!(
            element(&fetched, "RoleId"),
            element(&created, "RoleId"),
            "case {case:?}: expected the created role to be the one read back, got {:?}",
            element(&fetched, "RoleId")
        );

        let listed = roles_linked_to(&router, case, row.service_principal).await;
        assert!(
            listed.contains(&row.published_role_name.to_string()),
            "case {case:?}: expected {:?} under {expected_path:?}, got {listed:?}",
            row.published_role_name
        );
    }
}

/// RULE: a service principal is a DNS-style name, so the ASCII case it is spelled in
/// addresses the same service and resolves the same published role; and a `CustomSuffix` is
/// joined to that published name rather than replacing it.
#[tokio::test]
async fn a_registered_principal_resolves_whatever_case_and_suffix_the_request_carries() {
    // Rows: each over one of the registered principals, spelled upper-case, spelled
    // mixed-case, and with a `CustomSuffix` beside it.
    let rows: &[(&str, &str, &str, Option<&str>)] = &[
        (
            "an upper-cased three-label principal",
            "MEDIAPIPELINES.CHIME.AMAZONAWS.COM",
            "AWSServiceRoleForAmazonChimeSDKMediaPipelines",
            None,
        ),
        (
            "a mixed-case hyphenated principal",
            "License-Manager-User-Subscriptions.amazonaws.com",
            "AWSServiceRoleForAWSLicenseManagerUserSubscriptions",
            None,
        ),
        (
            "a custom suffix joined to the published name",
            "braket.amazonaws.com",
            "AWSServiceRoleForAmazonBraket_two",
            Some("two"),
        ),
    ];

    for (case, service_principal, expected_role_name, custom_suffix) in rows {
        let router = router();
        let mut params = vec![("AWSServiceName", *service_principal)];
        if let Some(suffix) = custom_suffix {
            params.push(("CustomSuffix", suffix));
        }

        let (status, created) = call(&router, "CreateServiceLinkedRole", &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to be served, got {status}: {created}"
        );
        assert_eq!(
            element(&created, "RoleName"),
            *expected_role_name,
            "case {case:?}: expected the role name {expected_role_name:?}, got {:?}",
            element(&created, "RoleName")
        );
        // The path carries the principal as the request spelled it, so the role the
        // request made is the one the published name reads back.
        let (status, fetched) = call(&router, "GetRole", &[("RoleName", expected_role_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetRole on {expected_role_name:?} to be served, \
             got {status}: {fetched}"
        );
        assert_eq!(
            element(&fetched, "RoleId"),
            element(&created, "RoleId"),
            "case {case:?}: expected the created role to be the one read back, got {:?}",
            element(&fetched, "RoleId")
        );
    }
}

/// RULE: the role a service links is unique in the account, so a second create for the same
/// principal with no new `CustomSuffix` is refused as a duplicate name and the role already
/// established is left exactly as it was — the invariant a flow that establishes its role
/// before every run depends on.
#[tokio::test]
async fn a_second_create_for_one_registered_principal_leaves_the_role_as_it_was() {
    for row in REGISTERED {
        let case = row.case;
        let router = router();

        let (status, created) = call(
            &router,
            "CreateServiceLinkedRole",
            &[("AWSServiceName", row.service_principal)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the first create to be served, got {status}: {created}"
        );
        let established = element(&created, "RoleId");

        let (status, repeated) = call(
            &router,
            "CreateServiceLinkedRole",
            &[("AWSServiceName", row.service_principal)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the repeat to be refused 400, got {status}: {repeated}"
        );
        assert_eq!(
            error_code(&repeated),
            "InvalidInput",
            "case {case:?}: expected the error code InvalidInput, got {:?}",
            error_code(&repeated)
        );

        let (status, fetched) = call(
            &router,
            "GetRole",
            &[("RoleName", row.published_role_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the established role to still be served, \
             got {status}: {fetched}"
        );
        assert_eq!(
            element(&fetched, "RoleId"),
            established,
            "case {case:?}: expected the established role {established:?} unchanged, got {:?}",
            element(&fetched, "RoleId")
        );
        let listed = roles_linked_to(&router, case, row.service_principal).await;
        assert_eq!(
            listed,
            vec![row.published_role_name.to_string()],
            "case {case:?}: expected exactly the established role under the reserved path, \
             got {listed:?}"
        );
    }
}
