// SPDX-License-Identifier: CC0-1.0

//! WHICH services `CreateServiceLinkedRole` mints a role for, end to end over the awsQuery
//! wire.
//!
//! The rule under test: a service-linked role is "linked to a specific Amazon Web Services
//! service", and only a service that has REGISTERED such a role with IAM has one to link —
//! so the create is served for a service principal the registry carries, and every other
//! value is refused as naming no AWS service, with HTTP 403, the error code `AccessDenied`
//! and the sentence `<AWSServiceName> is not a valid AWS service name (Service Principal)`,
//! whose only variable part is the value the request supplied. A refused create writes
//! nothing: no role under the name it might have been given, and nothing under the path IAM
//! reserves for service-linked roles.
//!
//! The registry is the catalog of the role AWS publishes per service principal
//! (`state::PUBLISHED_SERVICE_LINKED_ROLE_NAMES`), which is also what NAMES the role, so
//! the served rows below read their expected name out of that catalog rather than deriving
//! one. `tests/all/service_linked_roles.rs` is the suite for what a served create stores;
//! this one is the suite for which values are served at all.
//!
//! Each test is a TABLE whose rows differ along what the rule generalises over: for the
//! served side, the principal's label count and ASCII case and whether a `CustomSuffix` is
//! supplied; for the refused side, the FORM of the rejected value — a bare word with no
//! dots, a DNS-style name no service answers to, a real AWS service that has no
//! service-linked role, a value that is nothing but separators, one carrying the reserved
//! path's own spelling, and one supplied together with a `CustomSuffix`.
//!
//! The awsQuery harness (router, call, element) is the one `tests/all/user_lifecycle.rs`
//! owns, so every suite in this crate builds and reads a request the same way.

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state::PUBLISHED_SERVICE_LINKED_ROLE_NAMES;

use crate::user_lifecycle::{account_id, call, element, elements, error_code, router};

/// The path IAM reserves for service-linked roles, which a refused create must leave
/// empty.
const SERVICE_LINKED_ROLE_PATH: &str = "/aws-service-role/";

/// The sentence AWS refuses a value that names no service with. Written out here rather
/// than taken from the service under test, so the suite asserts AWS's text.
fn not_a_valid_service_name(aws_service_name: &str) -> String {
    format!("{aws_service_name} is not a valid AWS service name (Service Principal)")
}

/// One served row: a principal the registry carries, spelled as the request spells it, and
/// the optional `CustomSuffix` the create supplies.
///
/// The expected role name is not written out: it is the catalog's own entry for the
/// principal ([`registered_role_name`]), so a row cannot drift from the registry it is
/// about.
struct Served {
    case: &'static str,
    service_principal: &'static str,
    custom_suffix: Option<&'static str>,
}

/// Rows spanning two- and three-label principals, a principal whose published name carries
/// a `_` of its own, an upper-cased spelling (a principal addresses the same service
/// whatever its ASCII case), and both presence and absence of a `CustomSuffix`.
const SERVED: &[Served] = &[
    Served {
        case: "a two-label principal",
        service_principal: "elasticloadbalancing.amazonaws.com",
        custom_suffix: None,
    },
    Served {
        case: "a three-label principal",
        service_principal: "ops.apigateway.amazonaws.com",
        custom_suffix: None,
    },
    Served {
        case: "a published name carrying a separator of its own",
        service_principal: "ecs.application-autoscaling.amazonaws.com",
        custom_suffix: None,
    },
    Served {
        case: "an upper-cased spelling of the principal",
        service_principal: "RDS.AMAZONAWS.COM",
        custom_suffix: None,
    },
    Served {
        case: "a custom suffix joined to the published name",
        service_principal: "guardduty.amazonaws.com",
        custom_suffix: Some("audit"),
    },
];

/// One refused row: the value the request supplies for `AWSServiceName`, the optional
/// `CustomSuffix` beside it, and a role name nothing may be filed under afterwards.
struct Refused {
    case: &'static str,
    aws_service_name: &'static str,
    custom_suffix: Option<&'static str>,
    /// A name a caller might expect the refused create to have minted, asserted to address
    /// no role once the request has been refused.
    unfiled_role_name: &'static str,
}

/// Rows spanning the forms a value that names no registered service takes.
const REFUSED: &[Refused] = &[
    Refused {
        case: "a bare word with no dots",
        aws_service_name: "notaservice",
        custom_suffix: None,
        unfiled_role_name: "AWSServiceRoleForNotaservice",
    },
    Refused {
        case: "a DNS-style name no service answers to",
        aws_service_name: "not-a-real-service.amazonaws.com",
        custom_suffix: None,
        unfiled_role_name: "AWSServiceRoleForNot-a-real-service",
    },
    Refused {
        // AWS Elemental MediaConvert is an AWS service (IAM publishes the `mediaconvert`
        // prefix for it) that acts through a role the customer creates, so it has no
        // service-linked role to mint.
        case: "an AWS service with no service-linked role",
        aws_service_name: "mediaconvert.amazonaws.com",
        custom_suffix: None,
        unfiled_role_name: "AWSServiceRoleForMediaconvert",
    },
    Refused {
        case: "nothing but separators",
        aws_service_name: "..",
        custom_suffix: None,
        unfiled_role_name: "AWSServiceRoleFor",
    },
    Refused {
        case: "the reserved path spelled as a service",
        aws_service_name: "aws-service-role.amazonaws.com",
        custom_suffix: None,
        unfiled_role_name: "AWSServiceRoleForAws-service-role",
    },
    Refused {
        case: "an unregistered principal with a custom suffix",
        aws_service_name: "vendor-widget.amazonaws.com",
        custom_suffix: Some("debug"),
        unfiled_role_name: "AWSServiceRoleForVendorWidget_debug",
    },
];

/// The role name the registry carries for `service_principal`, matched the way IAM matches
/// an entity name — without regard to ASCII case — which is how the service resolves it.
fn registered_role_name(service_principal: &str) -> Option<&'static str> {
    PUBLISHED_SERVICE_LINKED_ROLE_NAMES
        .iter()
        .find(|(principal, _)| principal.eq_ignore_ascii_case(service_principal))
        .map(|(_, role_name)| *role_name)
}

/// The form parameters one row's create is driven with.
fn create_params<'row>(
    aws_service_name: &'row str,
    custom_suffix: Option<&'row str>,
) -> Vec<(&'row str, &'row str)> {
    let mut params = vec![("AWSServiceName", aws_service_name)];
    if let Some(suffix) = custom_suffix {
        params.push(("CustomSuffix", suffix));
    }
    params
}

/// The role names the account reports under the reserved path, in the order the listing
/// enumerates them.
async fn roles_under_the_reserved_path(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
) -> Vec<String> {
    let (status, listed) = call(
        router,
        "ListRoles",
        &[("PathPrefix", SERVICE_LINKED_ROLE_PATH)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the listing must be served, got {status}: {listed}"
    );
    elements(&listed, "RoleName")
}

/// RULE: a service principal the registry carries is served — the role is minted under the
/// name AWS publishes for that service, filed under the reserved path, and read back by
/// that name — whatever label count or ASCII case the principal is spelled in and whether
/// a `CustomSuffix` is supplied.
#[tokio::test]
async fn a_registered_service_principal_is_served_and_reads_back() {
    for row in SERVED {
        // One account per row: a second create of one role name is a duplicate, and the
        // rows are independent services.
        let router = router();
        let case = row.case;
        let Some(published) = registered_role_name(row.service_principal) else {
            panic!("case {case:?}: this row is only meaningful for a registered principal");
        };
        let expected_role_name = match row.custom_suffix {
            Some(suffix) => format!("{published}_{suffix}"),
            None => published.to_string(),
        };

        let (status, created) = call(
            &router,
            "CreateServiceLinkedRole",
            &create_params(row.service_principal, row.custom_suffix),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to be served, got {status}: {created}"
        );
        assert_eq!(
            element(&created, "RoleName"),
            expected_role_name,
            "case {case:?}: expected the role name {expected_role_name:?}, got {:?}",
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
            "arn:aws:iam::{}:role{expected_path}{expected_role_name}",
            account_id()
        );
        assert_eq!(
            element(&created, "Arn"),
            expected_arn,
            "case {case:?}: expected the ARN {expected_arn:?}, got {:?}",
            element(&created, "Arn")
        );

        // The round trip: the stored record is what a read of that name answers with.
        let (status, fetched) = call(
            &router,
            "GetRole",
            &[("RoleName", expected_role_name.as_str())],
        )
        .await;
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

        // And the listing under the reserved path reports it.
        let listed = roles_under_the_reserved_path(&router, case).await;
        assert!(
            listed.contains(&expected_role_name),
            "case {case:?}: expected {expected_role_name:?} under the reserved path, \
             got {listed:?}"
        );
    }
}

/// RULE: a value the registry does not carry names no service with a service-linked role,
/// so the create is refused HTTP 403 `AccessDenied` with AWS's sentence naming that value,
/// and nothing is written — whatever form the value takes, and whether or not a
/// `CustomSuffix` came with it.
#[tokio::test]
async fn a_value_naming_no_registered_service_is_refused_and_writes_nothing() {
    let router = router();

    for row in REFUSED {
        let case = row.case;
        assert!(
            registered_role_name(row.aws_service_name).is_none(),
            "case {case:?}: this row is only meaningful for a value the registry does not \
             carry"
        );

        let (status, body) = call(
            &router,
            "CreateServiceLinkedRole",
            &create_params(row.aws_service_name, row.custom_suffix),
        )
        .await;
        assert_eq!(
            status, 403,
            "case {case:?}: expected HTTP 403, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "AccessDenied",
            "case {case:?}: expected the error code AccessDenied, got {:?}",
            error_code(&body)
        );
        let expected = not_a_valid_service_name(row.aws_service_name);
        assert_eq!(
            element(&body, "Message"),
            expected,
            "case {case:?}: expected the message {expected:?}, got {:?}",
            element(&body, "Message")
        );

        // Nothing was filed under the name the refused create might have minted.
        let (status, body) = call(
            &router,
            "GetRole",
            &[("RoleName", row.unfiled_role_name)],
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: a refused create filed {:?} anyway, got {status}: {body}",
            row.unfiled_role_name
        );
        assert_eq!(
            error_code(&body),
            "NoSuchEntity",
            "case {case:?}: expected NoSuchEntity for {:?}, got {:?}",
            row.unfiled_role_name,
            error_code(&body)
        );
    }

    // Across every refused row, the reserved path still carries only the roles AWS created
    // WITH the account (`tests/all/account_creation_roles.rs`): no refused create left a
    // role behind under a name this suite did not predict either.
    let listed = roles_under_the_reserved_path(&router, "after every refusal").await;
    assert_eq!(
        listed,
        crate::account_creation_roles::roles_in_account_order(&[]),
        "expected no role of a refused create under the reserved path, got {listed:?}"
    );
}
