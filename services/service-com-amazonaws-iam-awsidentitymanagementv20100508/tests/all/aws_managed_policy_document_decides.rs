// SPDX-License-Identifier: CC0-1.0

//! What a policy AWS OWNS does to the calls of the principal it is in force on.
//!
//! The rule under test: an AWS-managed policy is evaluated against the document AWS
//! publishes for it, exactly as an inline policy is evaluated against the text that was
//! put — so a published document's `Deny` about the request refuses the call whatever
//! grant stands beside it, a published document's `Allow` serves the calls it covers and
//! withholds every other, and the refusal is HTTP 403 `AccessDenied` whose clause names
//! the ARN of the published policy that denied it. The policy is read the same way
//! wherever it is in force: attached to the identity, attached to a group the identity
//! belongs to, set as the identity's permissions boundary, or carried by the ROLE a
//! temporary credential is a session of — "the temporary security credentials … have the
//! permissions of the role" (`AssumeRole`).
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: which
//! published policy is in force and which way its statements go, how it got in force,
//! whether a grant stands beside it, which operation is called and which resource form
//! the refusal names, and whether the caller is an IAM user holding a long-term key or a
//! session of a role.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns and the other
//! suites in this crate share, so a request is built and read exactly one way.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state::{
    self, RoleSessionRecord, SessionPolicies,
};

use crate::identity_policy_enforcement::{principal, statement, UNREADABLE_GRANT};
use crate::user_permissions_boundary::{create_policy, put_boundary};
use crate::user_lifecycle::{
    account_id, assert_access_denied_message, call, call_signed_by, document_element, element,
    error_code, router, router_with_engine,
};

/// The AWS-managed policy whose single statement takes every permission away.
///
/// SOURCE: the AWS Managed Policy Reference, which publishes it at the account root of
/// AWS's own namespace; this crate's published catalog carries it
/// (`state::AWS_MANAGED_POLICIES`) and the first suite below reads its document back
/// through the API.
const DENY_ALL: &str = "arn:aws:iam::aws:policy/AWSDenyAll";

/// The AWS-managed policy that grants IAM's own READ verbs and nothing else — the entry
/// that shows the same reading of a published document in the other direction, and the
/// one this suite's rule is tested on besides the deny-everything policy.
///
/// SOURCE: the AWS Managed Policy Reference, as carried by `state::AWS_MANAGED_POLICIES`.
const IAM_READ_ONLY: &str = "arn:aws:iam::aws:policy/IAMReadOnlyAccess";

/// An `Allow` of every IAM action on every resource: what a row about a `Deny` is layered
/// on top of, so what the row demonstrates is the deny and not a missing grant.
const ALLOW_EVERY_IAM_ACTION: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"iam:*","Resource":"*"}]}"#;

/// The trust policy every fixture role is created with — the roles here are never
/// assumed over the wire, so what it names only has to be a well-formed trust policy.
const TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The session name every fixture session is minted under.
const SESSION_NAME: &str = "probe";

/// How long a fixture session's credential is usable for, so no row is refused for
/// presenting an expired credential.
const SESSION_LIFETIME_SECONDS: i64 = 3_600;

/// What a row expects of the signed call, and the clause its refusal closes with.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Expect {
    /// HTTP 200: the documents in force permit the call.
    Served,
    /// HTTP 403 `AccessDenied` because a statement of the identity-based policy whose ARN
    /// this names said `Deny`.
    DeniedBy(&'static str),
    /// HTTP 403 `AccessDenied` because no identity-based policy allows the action.
    NoAllow,
    /// HTTP 403 `AccessDenied` because a statement of the permissions boundary whose ARN
    /// this names said `Deny`.
    BoundaryDeniedBy(&'static str),
}

impl Expect {
    /// The clause the refusal's sentence closes with before the pointer at the decision's
    /// authorization detail.
    fn reason(&self, iam_action: &str) -> String {
        match self {
            Self::Served => String::new(),
            Self::DeniedBy(arn) => {
                format!("with an explicit deny in an identity-based policy: {arn}")
            }
            Self::NoAllow => {
                format!("because no identity-based policy allows the {iam_action} action")
            }
            Self::BoundaryDeniedBy(arn) => {
                format!("with an explicit deny in a permissions boundary: {arn}")
            }
        }
    }
}

/// One row of the user table: how the published policy is in force on the calling user,
/// the call it then makes, and what the rule requires of the answer.
struct UserCase {
    case: &'static str,
    /// The inline policies put on the user, as `(PolicyName, PolicyDocument)`.
    inline: Vec<(String, String)>,
    /// The managed policies attached to the USER itself, by ARN.
    attached: Vec<&'static str>,
    /// The managed policies attached to a group the user is added to, by ARN.
    inherited: Vec<&'static str>,
    /// The managed policy set as the user's permissions boundary, by ARN.
    boundary: Option<&'static str>,
    /// The awsQuery `Action`, the IAM action the decision is made on, and the form
    /// parameters the call carries.
    action: &'static str,
    iam_action: &'static str,
    params: Vec<(String, String)>,
    /// The resource as a refusal NAMES it.
    named_as: String,
    expect: Expect,
}

/// The rows. Each runs against its own fresh IAM user, so what a row puts in force is the
/// only thing in force when it calls.
fn user_cases(customer_policy_arn: &str) -> Vec<UserCase> {
    let account = account_id();
    vec![
        // The deny-everything policy attached on its own: the principal carries no grant
        // at all AND a published deny, and the deny is what the refusal names.
        UserCase {
            case: "the deny-everything policy attached alone",
            inline: Vec::new(),
            attached: vec![DENY_ALL],
            inherited: Vec::new(),
            boundary: None,
            action: "ListUsers",
            iam_action: "iam:ListUsers",
            params: Vec::new(),
            named_as: format!("arn:aws:iam::{account}:user/"),
            expect: Expect::DeniedBy(DENY_ALL),
        },
        // The same policy beside an inline Allow of every IAM action: no grant overturns
        // an explicit deny, so the answer does not change.
        UserCase {
            case: "the deny-everything policy beside an inline Allow of every action",
            inline: vec![("allow-everything".to_string(), ALLOW_EVERY_IAM_ACTION.to_string())],
            attached: vec![DENY_ALL],
            inherited: Vec::new(),
            boundary: None,
            action: "ListRoles",
            iam_action: "iam:ListRoles",
            params: Vec::new(),
            named_as: format!("arn:aws:iam::{account}:role/"),
            expect: Expect::DeniedBy(DENY_ALL),
        },
        // A read of ONE entity rather than of a collection: the resource form moves what
        // the refusal names and nothing else.
        UserCase {
            case: "the deny-everything policy against a single-entity read",
            inline: vec![("allow-everything".to_string(), ALLOW_EVERY_IAM_ACTION.to_string())],
            attached: vec![DENY_ALL],
            inherited: Vec::new(),
            boundary: None,
            action: "GetGroup",
            iam_action: "iam:GetGroup",
            params: vec![("GroupName".to_string(), "payroll".to_string())],
            named_as: "group payroll".to_string(),
            expect: Expect::DeniedBy(DENY_ALL),
        },
        // The same policy reached through a GROUP the caller belongs to: a document in
        // force on the identity is read the same way whichever entity carries it.
        UserCase {
            case: "the deny-everything policy inherited through a group",
            inline: vec![("allow-everything".to_string(), ALLOW_EVERY_IAM_ACTION.to_string())],
            attached: Vec::new(),
            inherited: vec![DENY_ALL],
            boundary: None,
            action: "ListUsers",
            iam_action: "iam:ListUsers",
            params: Vec::new(),
            named_as: format!("arn:aws:iam::{account}:user/"),
            expect: Expect::DeniedBy(DENY_ALL),
        },
        // The same policy as the CEILING on the identity: the boundary's own deny is what
        // refuses the call, and the clause names the boundary's policy.
        UserCase {
            case: "the deny-everything policy set as the permissions boundary",
            inline: vec![("allow-everything".to_string(), ALLOW_EVERY_IAM_ACTION.to_string())],
            attached: Vec::new(),
            inherited: Vec::new(),
            boundary: Some(DENY_ALL),
            action: "ListUsers",
            iam_action: "iam:ListUsers",
            params: Vec::new(),
            named_as: format!("arn:aws:iam::{account}:user/"),
            expect: Expect::BoundaryDeniedBy(DENY_ALL),
        },
        // A published document whose statements ALLOW: the read verbs it covers are
        // served for a principal carrying nothing else.
        UserCase {
            case: "the read-only policy attached alone, on a read it grants",
            inline: Vec::new(),
            attached: vec![IAM_READ_ONLY],
            inherited: Vec::new(),
            boundary: None,
            action: "ListUsers",
            iam_action: "iam:ListUsers",
            params: Vec::new(),
            named_as: format!("arn:aws:iam::{account}:user/"),
            expect: Expect::Served,
        },
        // The same attachment on a call its document does NOT cover: the published
        // document bounds what it grants, so the call falls to the implicit deny.
        UserCase {
            case: "the read-only policy attached alone, on a write it does not grant",
            inline: Vec::new(),
            attached: vec![IAM_READ_ONLY],
            inherited: Vec::new(),
            boundary: None,
            action: "UntagPolicy",
            iam_action: "iam:UntagPolicy",
            params: vec![
                ("PolicyArn".to_string(), customer_policy_arn.to_string()),
                ("TagKeys.member.1".to_string(), "env".to_string()),
            ],
            named_as: format!("policy {customer_policy_arn}"),
            expect: Expect::NoAllow,
        },
        // An attachment whose document this emulator holds NONE of: there is no published
        // text to decide against here, so such an attachment is not evidence either way
        // and the call is not refused on evidence that does not exist.
        UserCase {
            case: "an attachment whose document this emulator does not hold",
            inline: Vec::new(),
            attached: vec![UNREADABLE_GRANT],
            inherited: Vec::new(),
            boundary: None,
            action: "ListUsers",
            iam_action: "iam:ListUsers",
            params: Vec::new(),
            named_as: format!("arn:aws:iam::{account}:user/"),
            expect: Expect::Served,
        },
    ]
}

/// RULE: the document AWS publishes for an attached managed policy decides the calls of
/// the principal it is in force on — its `Deny` refuses whatever grant stands beside it
/// and its `Allow` covers exactly the calls it names — and a refusal is HTTP 403
/// `AccessDenied` whose clause names the published policy that decided it.
#[tokio::test]
async fn a_published_document_decides_the_calls_of_the_user_it_is_in_force_on() {
    let router = router();
    let customer_policy_arn = create_policy(&router, "UntaggableHere", "/").await;

    for (row, case) in user_cases(&customer_policy_arn).into_iter().enumerate() {
        let user_name = format!("carrier{row}");
        let access_key_id = principal(&router, case.case, &user_name, &case.inline).await;
        for policy_arn in &case.attached {
            attach(&router, case.case, "AttachUserPolicy", "UserName", &user_name, policy_arn)
                .await;
        }
        if !case.inherited.is_empty() {
            let group_name = format!("inheriting{row}");
            let (status, created) = call(&router, "CreateGroup", &[("GroupName", &group_name)]).await;
            assert_eq!(
                status, 200,
                "case {:?}: the fixture CreateGroup must succeed, got {status}: {created}",
                case.case
            );
            for policy_arn in &case.inherited {
                attach(
                    &router,
                    case.case,
                    "AttachGroupPolicy",
                    "GroupName",
                    &group_name,
                    policy_arn,
                )
                .await;
            }
            let (status, added) = call(
                &router,
                "AddUserToGroup",
                &[("GroupName", &group_name), ("UserName", &user_name)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?}: the fixture AddUserToGroup must succeed, got {status}: {added}",
                case.case
            );
        }
        if let Some(boundary) = case.boundary {
            let (status, put) = put_boundary(&router, &user_name, boundary).await;
            assert_eq!(
                status, 200,
                "case {:?}: the fixture PutUserPermissionsBoundary must succeed, got \
                 {status}: {put}",
                case.case
            );
        }

        let params: Vec<(&str, &str)> = case
            .params
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let (status, body) =
            call_signed_by(&router, Some(&access_key_id), case.action, &params).await;

        if case.expect == Expect::Served {
            assert_eq!(
                status, 200,
                "case {:?}: expected the call the documents in force permit to be served, \
                 got {status}: {body}",
                case.case
            );
            continue;
        }
        assert_eq!(
            status, 403,
            "case {:?}: expected HTTP 403, got {status}: {body}",
            case.case
        );
        assert_eq!(
            error_code(&body),
            "AccessDenied",
            "case {:?}: expected the error code AccessDenied, got {:?}: {body}",
            case.case,
            error_code(&body)
        );
        assert_access_denied_message(
            case.case,
            &element(&body, "Message"),
            &format!("arn:aws:iam::{}:user/{user_name}", account_id()),
            case.iam_action,
            &case.named_as,
            &case.expect.reason(case.iam_action),
        );
    }
}

/// One row of the role-session table: what the session's ROLE carries, the call the
/// session makes, and what the rule requires of the answer.
struct SessionCase {
    case: &'static str,
    /// The inline policy document the role carries, if any.
    inline: Option<String>,
    /// The managed policies attached to the role, by ARN.
    attached: Vec<&'static str>,
    /// The awsQuery `Action` and the IAM action the decision is made on.
    action: &'static str,
    iam_action: &'static str,
    expect: Expect,
}

/// The rows. Every one of them calls on the SAME target role, so what differs between two
/// rows is only what the calling session's role carries and which action it asks for.
fn session_cases() -> Vec<SessionCase> {
    vec![
        // The issue's own shape: a session of a role whose only permission policy is the
        // published deny-everything one.
        SessionCase {
            case: "a session of a role carrying only the deny-everything policy",
            inline: None,
            attached: vec![DENY_ALL],
            action: "ListAttachedRolePolicies",
            iam_action: "iam:ListAttachedRolePolicies",
            expect: Expect::DeniedBy(DENY_ALL),
        },
        // The same role on another action: the deny covers every action, so the answer
        // does not depend on which one is asked for.
        SessionCase {
            case: "the same role on another read of the same target",
            inline: None,
            attached: vec![DENY_ALL],
            action: "ListRolePolicies",
            iam_action: "iam:ListRolePolicies",
            expect: Expect::DeniedBy(DENY_ALL),
        },
        // The same policy beside an inline Allow of the very action: no grant overturns
        // an explicit deny, for a session as for a user.
        SessionCase {
            case: "a session of a role carrying the deny-everything policy and a grant",
            inline: Some(ALLOW_EVERY_IAM_ACTION.to_string()),
            attached: vec![DENY_ALL],
            action: "ListAttachedRolePolicies",
            iam_action: "iam:ListAttachedRolePolicies",
            expect: Expect::DeniedBy(DENY_ALL),
        },
        // A role carrying an inline Allow of the action: served, so no row above is
        // refused by the fixture rather than by the document under test.
        SessionCase {
            case: "a session of a role carrying an inline Allow of the action",
            inline: Some(statement("Allow", "iam:ListAttachedRolePolicies", "*")),
            attached: Vec::new(),
            action: "ListAttachedRolePolicies",
            iam_action: "iam:ListAttachedRolePolicies",
            expect: Expect::Served,
        },
        // A role carrying the published READ-ONLY policy: the document AWS publishes for
        // it covers this read, so the same call is served through an attachment.
        SessionCase {
            case: "a session of a role carrying only the read-only policy",
            inline: None,
            attached: vec![IAM_READ_ONLY],
            action: "ListAttachedRolePolicies",
            iam_action: "iam:ListAttachedRolePolicies",
            expect: Expect::Served,
        },
        // A role carrying nothing at all: no document denies the call and none allows it,
        // which is the implicit deny — the same refusal class with another clause.
        SessionCase {
            case: "a session of a role carrying no policy at all",
            inline: None,
            attached: Vec::new(),
            action: "ListAttachedRolePolicies",
            iam_action: "iam:ListAttachedRolePolicies",
            expect: Expect::NoAllow,
        },
    ]
}

/// RULE: a role session is authorized by the identity-based policies of the ROLE it is a
/// session of, read the same way a user's are — so the published document of a managed
/// policy attached to that role decides the session's calls, and a refusal names the
/// session as the principal and the published policy as the document that denied it.
#[tokio::test]
async fn a_published_document_the_role_carries_decides_its_sessions_calls() {
    let (router, engine) = router_with_engine();
    // The role every row reads, carrying nothing itself: a row's answer is about the
    // CALLER's documents, not about the target's.
    let target = "target-role";
    create_role(&router, "the target role", target).await;

    for (row, case) in session_cases().into_iter().enumerate() {
        let role_name = format!("caller{row}");
        create_role(&router, case.case, &role_name).await;
        if let Some(document) = &case.inline {
            let (status, put) = call(
                &router,
                "PutRolePolicy",
                &[
                    ("RoleName", &role_name),
                    ("PolicyName", "role-grant"),
                    ("PolicyDocument", document),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?}: the fixture PutRolePolicy must succeed, got {status}: {put}",
                case.case
            );
        }
        for policy_arn in &case.attached {
            attach(&router, case.case, "AttachRolePolicy", "RoleName", &role_name, policy_arn)
                .await;
        }
        // The attachments are read back through the operation that reports them, so a row
        // that is about an attached policy is about one the account really holds.
        let (status, listed) = call(
            &router,
            "ListAttachedRolePolicies",
            &[("RoleName", &role_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: the fixture ListAttachedRolePolicies must succeed, got {status}: \
             {listed}",
            case.case
        );
        let reported = crate::user_lifecycle::elements(&listed, "PolicyArn");
        assert_eq!(
            reported,
            case.attached
                .iter()
                .map(|arn| (*arn).to_string())
                .collect::<Vec<String>>(),
            "case {:?}: expected the role to report exactly the attachments the fixture \
             made, got {reported:?}: {listed}",
            case.case
        );

        let (access_key_id, session_arn) = record_session(&engine, &role_name, row);
        let (status, body) = call_signed_by(
            &router,
            Some(&access_key_id),
            case.action,
            &[("RoleName", target)],
        )
        .await;

        if case.expect == Expect::Served {
            assert_eq!(
                status, 200,
                "case {:?}: expected the call the role's documents permit to be served, got \
                 {status}: {body}",
                case.case
            );
            continue;
        }
        assert_eq!(
            status, 403,
            "case {:?}: expected HTTP 403, got {status}: {body}",
            case.case
        );
        assert_eq!(
            error_code(&body),
            "AccessDenied",
            "case {:?}: expected the error code AccessDenied, got {:?}: {body}",
            case.case,
            error_code(&body)
        );
        assert_access_denied_message(
            case.case,
            &element(&body, "Message"),
            &session_arn,
            case.iam_action,
            &format!("role {target}"),
            &case.expect.reason(case.iam_action),
        );
    }
}

/// RULE: the published document this emulator decides a deny-everything attachment
/// against is the one AWS publishes for that policy — one statement, `Deny` on every
/// action over every resource — and it is readable through the API that describes an
/// AWS-managed policy, so a client can see the text its calls are being refused by.
#[tokio::test]
async fn the_published_deny_everything_policy_is_readable_and_denies_every_action() {
    let router = router();

    let (status, body) = call(&router, "GetPolicy", &[("PolicyArn", DENY_ALL)]).await;
    assert_eq!(
        status, 200,
        "GetPolicy must report the deny-everything policy, got {status}: {body}"
    );
    assert_eq!(
        element(&body, "Arn"),
        DENY_ALL,
        "the reported Arn must be the ARN asked for: {body}"
    );
    assert_eq!(
        element(&body, "PolicyName"),
        "AWSDenyAll",
        "the reported PolicyName must be the one the ARN spells: {body}"
    );
    assert_eq!(
        element(&body, "Path"),
        "/",
        "the reported Path must be the one the ARN spells: {body}"
    );
    assert_eq!(
        element(&body, "IsAttachable"),
        "true",
        "AWS publishes the policy as one an identity can be scoped down with: {body}"
    );

    let default_version = element(&body, "DefaultVersionId");
    let (status, version) = call(
        &router,
        "GetPolicyVersion",
        &[("PolicyArn", DENY_ALL), ("VersionId", &default_version)],
    )
    .await;
    assert_eq!(
        status, 200,
        "GetPolicyVersion must serve the default version {default_version:?}, got {status}: \
         {version}"
    );
    let document = document_element(&version, "Document");
    let parsed: serde_json::Value = serde_json::from_str(&document)
        .unwrap_or_else(|error| panic!("the served document must be JSON, got {error}: {document}"));
    assert_eq!(
        parsed.get("Statement"),
        Some(&serde_json::json!([{"Effect": "Deny", "Action": "*", "Resource": "*"}])),
        "expected the one statement AWS publishes for the deny-everything policy, got \
         {document}"
    );
}

/// Create the role `role_name` with the fixture trust policy.
async fn create_role(router: &MultiServiceRouter, case: &str, role_name: &str) {
    let (status, created) = call(
        router,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", TRUST_POLICY),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture CreateRole must succeed, got {status}: {created}"
    );
}

/// Attach the managed policy `policy_arn` to the entity `entity_name` of the kind the
/// pair (`action`, `entity_parameter`) names.
async fn attach(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    entity_parameter: &str,
    entity_name: &str,
    policy_arn: &str,
) {
    let (status, attached) = call(
        router,
        action,
        &[
            (entity_parameter, entity_name),
            ("PolicyArn", policy_arn),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture {action} of {policy_arn} must succeed, got {status}: \
         {attached}"
    );
}

/// Record a session of the role `role_name` in this service's own store and answer the
/// access key id its credential is carried under, beside the ARN
/// `sts:GetCallerIdentity` reports for it.
///
/// The credential of a role session is minted by `sts:AssumeRole`, which is not this
/// service's API, so the record that mint leaves behind is written here exactly as the
/// minting operation writes it — the store this service's gate reads the signing
/// principal out of. The session itself is scoped by nothing, so what decides its calls
/// is the role's own documents.
fn record_session(
    held: &engine::InMemoryEngine,
    role_name: &str,
    row: usize,
) -> (String, String) {
    let access_key_id = format!("ASIASESSION{row}");
    let session_arn = format!(
        "arn:aws:sts::{}:assumed-role/{role_name}/{SESSION_NAME}",
        account_id()
    );
    let minted = state::now();
    let store = state::service_state(held).expect("the IAM store");
    store.lock().record_role_session(
        &access_key_id,
        RoleSessionRecord {
            session_arn: session_arn.clone(),
            role_arn: format!("arn:aws:iam::{}:role/{role_name}", account_id()),
            expires: ::aws_smithy_types::DateTime::from_secs(
                minted.secs() + SESSION_LIFETIME_SECONDS,
            ),
            scope: SessionPolicies::default(),
            tags: Vec::new(),
        },
        minted,
    );
    (access_key_id, session_arn)
}
