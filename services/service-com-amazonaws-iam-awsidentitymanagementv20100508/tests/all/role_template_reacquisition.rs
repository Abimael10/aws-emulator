// SPDX-License-Identifier: CC0-1.0

//! Acquiring the SAME role from a role template twice, end to end over the awsQuery wire.
//!
//! RULE: the role an acquisition asks for is identified by the template it comes from, that
//! template's resolved minor version and the name rendered from it — which is what a role
//! created from a template RECORDS ("contains information about the role template that this
//! role was created from. This member is present only for roles created with AcquireRole",
//! `Role$SourceRoleTemplate`, `smithy_query(query="shape", shape="Role")`). So a call whose
//! rendered name is already held by the role of that same template version asks for a role
//! that is already there: the call answers `200` with the role the account holds, member for
//! member, and creates and changes nothing — acquiring a role twice from one template
//! version leaves the account with the one role, with the one set of attachments and inline
//! policies, exactly as acquiring it once does. Only a rendered name held by a role that
//! template version did NOT create is the modelled name conflict, which
//! `tests/all/role_template_acquisition.rs` owns.
//!
//! The suite is a TABLE whose rows differ along what the rule generalises over: whether the
//! template is one the ACCOUNT recorded or one a SERVICE publishes, how each of the two
//! calls addresses the minor version (left to the template's default, or named outright),
//! the path the template files the role under, and how many managed policies and inline
//! policies the role carries.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns; `acquire`, `seed` and
//! the account's own fixture template are the ones `tests/all/role_template_acquisition.rs`
//! owns, and the published ARNs the ones
//! `tests/all/published_role_template_acquisition.rs` owns.

use smithy_server_runtime::MultiServiceRouter;

use crate::published_role_template_acquisition::{BACKUP_TEMPLATE_ARN, POWER_USER_TEMPLATE_ARN};
use crate::role_template_acquisition::{acquire, seed, template, DEFAULT_MINOR_VERSION, TEMPLATE_ARN};
use crate::user_lifecycle::{call, document_element, element, elements, router_with_engine};

/// One row: which template version the two calls address, with which replacement values, and
/// the role the rendering produces.
struct Reacquired {
    case: &'static str,
    template_arn: &'static str,
    /// The minor version of the account's OWN fixture template to record under that ARN
    /// before the first call, or `None` where the row addresses a PUBLISHED template, which
    /// needs nothing recorded.
    recorded: Option<i32>,
    values: &'static [(&'static str, &'static [&'static str])],
    /// The minor version the FIRST call names, or `None` to leave it to the template's
    /// default.
    first_minor_version: Option<i32>,
    /// The minor version the SECOND call names — the same resolved version as the first,
    /// addressed the same way or the other way round.
    second_minor_version: Option<i32>,
    role_name: &'static str,
    path: &'static str,
    /// The managed policies the template attaches, in the order this service lists a role's
    /// attachments in (by ARN).
    managed_policies: &'static [&'static str],
    /// The inline policy names the template embeds.
    inline_policies: &'static [&'static str],
}

/// The rows: the account's own fixture template with the minor version left to the default
/// on both calls and then named outright on the second, the AWS Backup service-role template
/// (two managed policies, the service-role path) and the IAM power-user template (one managed
/// policy, the account root).
const ROWS: &[Reacquired] = &[
    Reacquired {
        case: "a template the account recorded, both calls leaving the minor version to the default",
        template_arn: TEMPLATE_ARN,
        recorded: Some(DEFAULT_MINOR_VERSION),
        values: &[("Team", &["Alpha"])],
        first_minor_version: None,
        second_minor_version: None,
        role_name: "Partner-Alpha-Deploy",
        path: "/partners/Alpha/",
        managed_policies: &["arn:aws:iam::aws:policy/ReadOnlyAccess"],
        inline_policies: &["Alpha-drop"],
    },
    Reacquired {
        case: "a template the account recorded, the second call naming the version the first defaulted to",
        template_arn: TEMPLATE_ARN,
        recorded: Some(DEFAULT_MINOR_VERSION),
        values: &[("Team", &["Beta"]), ("Regions", &["eu-west-1", "ap-south-1"])],
        first_minor_version: None,
        second_minor_version: Some(DEFAULT_MINOR_VERSION),
        role_name: "Partner-Beta-Deploy",
        path: "/partners/Beta/",
        managed_policies: &["arn:aws:iam::aws:policy/ReadOnlyAccess"],
        inline_policies: &["Beta-drop"],
    },
    Reacquired {
        case: "the published AWS Backup service role, the first call naming the version the second defaults to",
        template_arn: BACKUP_TEMPLATE_ARN,
        recorded: None,
        values: &[
            ("RoleName", &["backup-default-service-role"]),
            ("accountId", &["111122223333"]),
        ],
        first_minor_version: Some(0),
        second_minor_version: None,
        role_name: "backup-default-service-role",
        path: "/service-role/",
        managed_policies: &[
            "arn:aws:iam::aws:policy/service-role/AWSBackupServiceRolePolicyForBackup",
            "arn:aws:iam::aws:policy/service-role/AWSBackupServiceRolePolicyForRestores",
        ],
        inline_policies: &[],
    },
    Reacquired {
        case: "the published IAM power-user template, both calls naming the minor version",
        template_arn: POWER_USER_TEMPLATE_ARN,
        recorded: None,
        values: &[
            ("AWSServiceName", &["lambda.amazonaws.com"]),
            ("RoleName", &["power_user.role-1"]),
        ],
        first_minor_version: Some(0),
        second_minor_version: Some(0),
        role_name: "power_user.role-1",
        path: "/",
        managed_policies: &["arn:aws:iam::aws:policy/PowerUserAccess"],
        inline_policies: &[],
    },
];

/// The members of a role that identify it and describe it: what the two calls and the read
/// in between must all report the same values for.
const ROLE_MEMBERS: &[&str] = &[
    "RoleName",
    "RoleId",
    "Arn",
    "Path",
    "CreateDate",
    "Description",
    "MaxSessionDuration",
];

/// The role names the account reports through `ListRoles`, read over the wire.
async fn listed_roles(router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = call(router, "ListRoles", &[]).await;
    assert_eq!(status, 200, "ListRoles must succeed, got {status}: {body}");
    elements(&body, "RoleName")
}

/// RULE: a second acquisition of the role a template version creates is satisfied by the
/// role the account already holds — the same role, member for member, with no second role,
/// no second attachment and no second inline policy.
#[tokio::test]
async fn re_acquiring_a_role_answers_with_the_role_the_account_holds() {
    for row in ROWS {
        let case = row.case;
        let (router, engine) = router_with_engine();
        if let Some(minor_version) = row.recorded {
            seed(&engine, row.template_arn, &template(minor_version));
        }

        let (status, first) = acquire(
            &router,
            None,
            row.template_arn,
            row.first_minor_version,
            row.values,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first acquisition must create the role, got {status}: {first}"
        );
        assert_eq!(
            element(&first, "RoleName"),
            row.role_name,
            "case {case:?}: expected the rendered role name {:?}, got {:?}",
            row.role_name,
            element(&first, "RoleName")
        );

        let (status, second) = acquire(
            &router,
            None,
            row.template_arn,
            row.second_minor_version,
            row.values,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the second acquisition must be satisfied by the held role, got \
             {status}: {second}"
        );
        for member in ROLE_MEMBERS {
            assert_eq!(
                element(&second, member),
                element(&first, member),
                "case {case:?}: expected the second acquisition to report the held role's \
                 {member} {:?}, got {:?}",
                element(&first, member),
                element(&second, member)
            );
        }
        assert_eq!(
            document_element(&second, "AssumeRolePolicyDocument"),
            document_element(&first, "AssumeRolePolicyDocument"),
            "case {case:?}: expected the held role's trust policy {:?}, got {:?}",
            document_element(&first, "AssumeRolePolicyDocument"),
            document_element(&second, "AssumeRolePolicyDocument")
        );
        assert_eq!(
            element(&second, "TemplateMinorVersion"),
            element(&first, "TemplateMinorVersion"),
            "case {case:?}: expected the held role's source minor version {:?}, got {:?}",
            element(&first, "TemplateMinorVersion"),
            element(&second, "TemplateMinorVersion")
        );

        // The round trip: the role the second call answered with is the one the read reports,
        // so the second call changed nothing about the role the first created.
        let (status, read) = call(&router, "GetRole", &[("RoleName", row.role_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the role must be readable, got {status}: {read}"
        );
        for member in ROLE_MEMBERS {
            assert_eq!(
                element(&read, member),
                element(&first, member),
                "case {case:?}: expected the read to report {member} {:?}, got {:?}",
                element(&first, member),
                element(&read, member)
            );
        }
        assert_eq!(
            element(&read, "Path"),
            row.path,
            "case {case:?}: expected the template's path {:?}, got {:?}",
            row.path,
            element(&read, "Path")
        );

        // ONE role, holding ONE copy of what the template attaches and embeds.
        let held = listed_roles(&router).await;
        assert_eq!(
            held.iter().filter(|name| *name == row.role_name).count(),
            1,
            "case {case:?}: expected the account to hold one role under {:?}, got {held:?}",
            row.role_name
        );
        let (status, attached) = call(
            &router,
            "ListAttachedRolePolicies",
            &[("RoleName", row.role_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the role's attachments must be listable, got {status}: {attached}"
        );
        assert_eq!(
            elements(&attached, "PolicyArn"),
            row.managed_policies,
            "case {case:?}: expected the template's managed policies {:?} once, got {:?}",
            row.managed_policies,
            elements(&attached, "PolicyArn")
        );
        let (status, inline) =
            call(&router, "ListRolePolicies", &[("RoleName", row.role_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the role's inline policies must be listable, got {status}: {inline}"
        );
        assert_eq!(
            elements(&inline, "member"),
            row.inline_policies,
            "case {case:?}: expected the template's inline policies {:?} once, got {:?}",
            row.inline_policies,
            elements(&inline, "member")
        );
    }
}

/// RULE, the other half: the role a template version creates is the role an acquisition of
/// THAT version asks for, so a role the account acquired is not what satisfies a different
/// acquisition — a call addressing another template, or another minor version of the same
/// template, that renders the same name is the modelled conflict rather than this role.
///
/// Rows: the two kinds of "another template version" a rendered name can collide across.
#[tokio::test]
async fn an_acquisition_is_satisfied_only_by_the_role_its_own_template_version_created() {
    let other_version = DEFAULT_MINOR_VERSION + 1;
    // Each row: the template version the SECOND acquisition addresses and the replacement
    // values it renders the same role name from.
    struct Collision {
        case: &'static str,
        template_arn: &'static str,
        minor_version: Option<i32>,
        values: &'static [(&'static str, &'static [&'static str])],
    }

    let rows: &[Collision] = &[
        Collision {
            case: "another template rendering the same name",
            template_arn: POWER_USER_TEMPLATE_ARN,
            minor_version: None,
            values: &[
                ("AWSServiceName", &["lambda.amazonaws.com"]),
                ("RoleName", &["Partner-Alpha-Deploy"]),
            ],
        },
        Collision {
            case: "another minor version of the same template",
            template_arn: TEMPLATE_ARN,
            minor_version: Some(DEFAULT_MINOR_VERSION + 1),
            values: &[("Team", &["Alpha"])],
        },
    ];

    for Collision {
        case,
        template_arn,
        minor_version,
        values,
    } in rows
    {
        let (router, engine) = router_with_engine();
        seed(&engine, TEMPLATE_ARN, &template(DEFAULT_MINOR_VERSION));
        seed(&engine, TEMPLATE_ARN, &template(other_version));

        let (status, body) = acquire(
            &router,
            None,
            TEMPLATE_ARN,
            Some(DEFAULT_MINOR_VERSION),
            &[("Team", &["Alpha"])],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first acquisition must create the role, got {status}: {body}"
        );
        let acquired_role_id = element(&body, "RoleId");

        let (status, body) = acquire(&router, None, template_arn, *minor_version, values).await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected 409, got {status}: {body}"
        );
        assert_eq!(
            crate::user_lifecycle::error_code(&body),
            "NameConflict",
            "case {case:?}: expected NameConflict, got {:?}: {body}",
            crate::user_lifecycle::error_code(&body)
        );

        // The role the account holds is untouched by the refused call.
        let (status, read) = call(&router, "GetRole", &[("RoleName", "Partner-Alpha-Deploy")]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the acquired role must still be readable, got {status}: {read}"
        );
        assert_eq!(
            element(&read, "RoleId"),
            acquired_role_id,
            "case {case:?}: expected the acquired role {acquired_role_id:?} to be unchanged, \
             got {:?}",
            element(&read, "RoleId")
        );
        assert_eq!(
            element(&read, "TemplateMinorVersion"),
            DEFAULT_MINOR_VERSION.to_string(),
            "case {case:?}: expected the acquired role to still name the version that created \
             it, got {:?}",
            element(&read, "TemplateMinorVersion")
        );
    }
}
