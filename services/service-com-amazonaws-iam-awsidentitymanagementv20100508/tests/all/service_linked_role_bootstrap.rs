// SPDX-License-Identifier: CC0-1.0

//! The bootstrap a client makes before it uses a service that depends on a
//! service-linked role: `CreateServiceLinkedRole` for the service's principal, then
//! `GetRole` under the name AWS publishes for that service, polled until it resolves.
//!
//! The rule: the complete role name is the service-provided prefix plus the label AWS
//! ASSIGNS the service ("a string that you provide, which is combined with the
//! service-provided prefix to form the complete role name",
//! `CreateServiceLinkedRole$CustomSuffix`), so the create files the role under that
//! assigned name and `GetRole` resolves it there — a name derived from the request's
//! own `AWSServiceName` by capitalising its first label resolves nothing
//! (`NoSuchEntityException`, HTTP 404, awsQueryError code `NoSuchEntity`, the one
//! client error `GetRole` models:
//! `smithy_query(query="operation", operation="GetRole")`).
//!
//! The rows below span the families whose assigned label no transformation of the
//! principal produces: a two-word hyphenated label whose name joins the words in mixed
//! case, and the per-resource-type names of a service that publishes one role per
//! namespace it scales. The awsQuery harness (router, call, element) is the one
//! `tests/all/user_lifecycle.rs` owns.

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state::{
    ACCOUNT_CREATION_SERVICE_LINKED_ROLE_PRINCIPALS, PUBLISHED_SERVICE_LINKED_ROLE_NAMES,
};

use crate::user_lifecycle::{account_id, call, element, error_code, router};

/// Service principals and the role name AWS assigns each of them, written out by hand
/// so this suite pins the names independently of the table the service reads.
///
/// Source: the "Using service-linked roles for <service>" page of each service's own
/// user guide — the same documentation `src/state.rs`'s
/// `PUBLISHED_SERVICE_LINKED_ROLE_NAMES` cites — and, for the Application Auto Scaling
/// family, the per-namespace roles that service's own user guide publishes, whose
/// resource-type labels this repository already carries in
/// service-com-amazonaws-applicationautoscaling-anyscalefrontendservice
/// `src/state.rs`.
///
/// Rows span a hyphenated two-word label joined in mixed case (WorkSpaces Instances),
/// a three-label principal whose name carries an acronym (EMR), one whose name carries
/// a capitalisation the label does not (WorkSpaces Pool), one whose resource type is
/// unrelated to the namespace's own word (Lambda concurrency), one spelling a
/// hyphenated label as one word (custom resources) and one whose name carries a service
/// acronym and a feature (EC2 Spot Fleet requests).
const ASSIGNED_NAMES: &[(&str, &str)] = &[
    (
        "workspaces-instances.amazonaws.com",
        "AWSServiceRoleForWorkspacesInstances",
    ),
    (
        "elasticmapreduce.application-autoscaling.amazonaws.com",
        "AWSServiceRoleForApplicationAutoScaling_EMRInstanceGroup",
    ),
    (
        "workspaces.application-autoscaling.amazonaws.com",
        "AWSServiceRoleForApplicationAutoScaling_WorkSpacesPool",
    ),
    (
        "lambda.application-autoscaling.amazonaws.com",
        "AWSServiceRoleForApplicationAutoScaling_LambdaConcurrency",
    ),
    (
        "custom-resource.application-autoscaling.amazonaws.com",
        "AWSServiceRoleForApplicationAutoScaling_CustomResource",
    ),
    (
        "ec2.application-autoscaling.amazonaws.com",
        "AWSServiceRoleForApplicationAutoScaling_EC2SpotFleetRequest",
    ),
];

/// The name a transformation of the REQUEST would produce: the service-provided prefix
/// followed by the principal's first label with only its initial letter upper-cased.
///
/// Written out here so each row can assert that this name addresses no role when AWS
/// assigns the service one of its own.
fn mechanically_derived_name(service_principal: &str) -> String {
    let label = service_principal
        .split('.')
        .next()
        .unwrap_or(service_principal);
    let mut name = String::from("AWSServiceRoleFor");
    let mut characters = label.chars();
    if let Some(first) = characters.next() {
        name.extend(first.to_uppercase());
        name.push_str(characters.as_str());
    }
    name
}

/// RULE: a service AWS assigns a service-linked role name to has its role filed and
/// read under THAT name, so the documented bootstrap — create the role for the
/// principal, then read it back by the published name — resolves the created role on
/// the next call, while the name derived from the request's own `AWSServiceName`
/// resolves nothing (`NoSuchEntity`, HTTP 404).
#[tokio::test]
async fn the_bootstrap_reads_the_created_role_back_under_its_assigned_name() {
    for (service_principal, assigned_name) in ASSIGNED_NAMES {
        let case = *service_principal;
        // Each row is an independent service, and a second create of one role name is
        // a duplicate, so each gets its own account.
        let router = router();

        // The table this suite writes out and the catalog the service reads are one
        // set of rows, so they cannot drift apart.
        assert!(
            PUBLISHED_SERVICE_LINKED_ROLE_NAMES
                .iter()
                .any(|(principal, name)| principal == service_principal
                    && name == assigned_name),
            "case {case:?}: expected the catalog to assign it {assigned_name:?}"
        );

        let (status, created) = call(
            &router,
            "CreateServiceLinkedRole",
            &[("AWSServiceName", service_principal)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to answer 200, got {status}: {created}"
        );
        assert_eq!(
            element(&created, "RoleName"),
            *assigned_name,
            "case {case:?}: expected the assigned role name {assigned_name:?}, got {:?}",
            element(&created, "RoleName")
        );
        let expected_path = format!("/aws-service-role/{service_principal}/");
        assert_eq!(
            element(&created, "Path"),
            expected_path,
            "case {case:?}: expected the service-linked role path {expected_path:?}, got {:?}",
            element(&created, "Path")
        );
        let expected_arn = format!(
            "arn:aws:iam::{}:role{expected_path}{assigned_name}",
            account_id()
        );
        assert_eq!(
            element(&created, "Arn"),
            expected_arn,
            "case {case:?}: expected the ARN {expected_arn:?}, got {:?}",
            element(&created, "Arn")
        );

        // The read-your-writes the bootstrap polls for: the assigned name resolves the
        // record the create reported, and reports it under that same name.
        let (status, fetched) = call(&router, "GetRole", &[("RoleName", assigned_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetRole on {assigned_name:?} to answer 200, \
             got {status}: {fetched}"
        );
        assert_eq!(
            element(&fetched, "RoleId"),
            element(&created, "RoleId"),
            "case {case:?}: expected the created role to be the one read back, got {:?}",
            element(&fetched, "RoleId")
        );
        assert_eq!(
            element(&fetched, "RoleName"),
            *assigned_name,
            "case {case:?}: expected the role reported under {assigned_name:?}, got {:?}",
            element(&fetched, "RoleName")
        );

        // IAM entity names "are not distinguished by case" (`CreateRole$RoleName`), so
        // any spelling of the assigned name addresses that same record, reported under
        // its canonical spelling.
        let lowercased = assigned_name.to_ascii_lowercase();
        let (status, body) = call(&router, "GetRole", &[("RoleName", lowercased.as_str())]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetRole on {lowercased:?} to answer 200, got {status}: {body}"
        );
        assert_eq!(
            element(&body, "RoleName"),
            *assigned_name,
            "case {case:?}: expected the canonical spelling {assigned_name:?}, got {:?}",
            element(&body, "RoleName")
        );

        // And the name a transformation of the request would have produced addresses
        // no role at all.
        let derived = mechanically_derived_name(service_principal);
        assert_ne!(
            derived.to_ascii_lowercase(),
            assigned_name.to_ascii_lowercase(),
            "case {case:?}: this row is only meaningful where the assigned name differs \
             from the derived one, got {derived:?}"
        );
        let (status, body) = call(&router, "GetRole", &[("RoleName", derived.as_str())]).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected GetRole on the derived {derived:?} to answer 404, \
             got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "NoSuchEntity",
            "case {case:?}: expected NoSuchEntity for {derived:?}, got {:?}",
            error_code(&body)
        );
    }
}

/// RULE: the assigned name is the service's, whatever shape its principal has — so for
/// EVERY row of the catalog the bootstrap converges on the assigned name, and no
/// account ends up holding a role under a name derived from the principal instead.
#[tokio::test]
async fn every_assigned_name_is_what_the_bootstrap_converges_on() {
    // Rows: the whole catalog, so the rule is asserted over the class rather than over
    // the handful of principals this suite writes out. One account holds them all:
    // every row is a different role name, and the account's role quota
    // (`MAX_ROLES_PER_ACCOUNT`) is far above the catalog's length.
    let router = router();

    for (service_principal, assigned_name) in PUBLISHED_SERVICE_LINKED_ROLE_NAMES {
        let case = *service_principal;
        // A principal whose service-linked role AWS creates WITH the account already has
        // a role under its assigned name (`tests/all/account_creation_roles.rs`), and a
        // name the account already holds a service role under is refused as taken rather
        // than created a second time — the bootstrap converges on the role already held.
        let created_with_the_account =
            ACCOUNT_CREATION_SERVICE_LINKED_ROLE_PRINCIPALS.contains(service_principal);
        let (status, created) = call(
            &router,
            "CreateServiceLinkedRole",
            &[("AWSServiceName", service_principal)],
        )
        .await;
        if created_with_the_account {
            assert_eq!(
                status, 400,
                "case {case:?}: expected the name to be refused as taken, got {status}: {created}"
            );
            assert_eq!(
                error_code(&created),
                "InvalidInput",
                "case {case:?}: expected InvalidInput, got {:?}",
                error_code(&created)
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the create to answer 200, got {status}: {created}"
            );
            assert_eq!(
                element(&created, "RoleName"),
                *assigned_name,
                "case {case:?}: expected the assigned role name {assigned_name:?}, got {:?}",
                element(&created, "RoleName")
            );
        }

        let (status, fetched) = call(&router, "GetRole", &[("RoleName", assigned_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetRole on {assigned_name:?} to answer 200, \
             got {status}: {fetched}"
        );
        if !created_with_the_account {
            assert_eq!(
                element(&fetched, "RoleId"),
                element(&created, "RoleId"),
                "case {case:?}: expected the created role to be the one read back, got {:?}",
                element(&fetched, "RoleId")
            );
        }

        // A principal the catalog assigns a name to is never filed under the derived
        // one. Skipped where the derived name is a name the catalog assigns to SOME
        // service — its own, or another's, as the first label of one principal can be
        // the assigned label of a different service — since such a name addresses that
        // service's role and so answers 200 by the rule above.
        let derived = mechanically_derived_name(service_principal);
        if PUBLISHED_SERVICE_LINKED_ROLE_NAMES
            .iter()
            .any(|(_, name)| name.eq_ignore_ascii_case(&derived))
        {
            continue;
        }
        let (status, body) = call(&router, "GetRole", &[("RoleName", derived.as_str())]).await;
        assert_eq!(
            status, 404,
            "case {case:?}: the create filed a role under the derived {derived:?}, \
             got {status}: {body}"
        );
    }
}
