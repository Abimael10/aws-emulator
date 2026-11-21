// SPDX-License-Identifier: CC0-1.0

//! `AssumeRoot` end-to-end: the privileged session AWS launches on a MEMBER account of the
//! caller's organization, and the two refusal classes ahead of it.
//!
//! The rules under test:
//!
//! * the request's own values are read BEFORE the grant is — a `TargetPrincipal` that names
//!   no account, and a `TaskPolicyArn` outside the family AWS publishes for root sessions,
//!   are refused `ValidationError` whatever the caller's standing, and so is a value outside
//!   a member's modelled range;
//! * a caller whose organization does not grant centralized root access for root sessions is
//!   refused `AccessDenied`, which is what an account that has not enabled the feature gets;
//!   and
//! * where the organization does grant it, the session is launched on the member account and
//!   speaks for THAT account's root user, which `GetCallerIdentity` reads back for the
//!   credential it vended.
//!
//! Every call goes over the wire as an awsQuery POST through the same router the emulator
//! serves with, and the organization's centralized root access facts are seeded into the IAM
//! store of the one shared engine STS reads them from — the record
//! `iam:EnableOrganizationsRootSessions` writes.

use service_com_amazonaws_iam_awsidentitymanagementv20100508 as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615 as sts;

use super::malformed_session_policy::decode_xml_text;
use super::sts_sessions::{call, element, emulator, emulator_account, member, LONG_TERM_KEY};

/// A member account of the seeded organization: an account id that is NOT this emulator's
/// own, so a session launched on it is launched on another account exactly as AWS's is.
const MEMBER_ACCOUNT: &str = "123456789012";

/// The task policy every row names unless it is about the task policy: one of the five AWS
/// publishes under `/root-task/`.
const TASK_POLICY_ARN: &str = "arn:aws:iam::aws:policy/root-task/S3UnlockBucketPolicy";

/// The error code AWS answers a value it will not accept with, on every protocol.
const VALIDATION_ERROR: &str = "ValidationError";

/// The error code AWS answers an authorization refusal with.
const ACCESS_DENIED: &str = "AccessDenied";

/// The sentence AWS states a refused authorization in: the principal that signed, the action
/// and the resource it asked for it on.
fn not_authorized(target_account: &str) -> String {
    format!(
        "User: {} is not authorized to perform: sts:AssumeRoot on resource: \
         arn:aws:iam::{target_account}:root",
        ::service_common::caller_principal::ambient_user_arn()
    )
}

/// The clause AWS states for one broken constraint: the value as submitted, the member path
/// it sits at, and the requirement it broke.
fn range_clause(value: &str, path: &str, maximum: i64) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{path}' failed to satisfy \
         constraint: Member must have value less than or equal to {maximum}"
    )
}

/// The form fields an `AssumeRoot` call carries: the two required members, plus whatever a
/// row adds.
fn params(target_principal: &str, task_policy_arn: &str) -> Vec<(&'static str, String)> {
    vec![
        ("TargetPrincipal", target_principal.to_string()),
        ("TaskPolicyArn.arn", task_policy_arn.to_string()),
    ]
}

/// Record an organization on the IAM store of the shared engine, as
/// `organizations:CreateOrganization` and IAM's own centralized root access writes leave it:
/// managed by this emulator's account, in all-features mode with trusted access for IAM, and
/// carrying `features` as its enabled centralized root access features.
fn seed_organization(engine: &engine::InMemoryEngine, features: &[&str]) {
    engine
        .state::<iam::ComAmazonawsIamAwsidentitymanagementv20100508State>()
        .lock()
        .organization = Some(iam::state::Organization {
        organization_id: "o-assumerootsuite".to_string(),
        management_account_id: emulator_account(),
        all_features_enabled: true,
        iam_trusted_access_enabled: true,
        root_access_delegated_administrator_ids: std::collections::BTreeSet::new(),
        enabled_root_access_features: features
            .iter()
            .map(|feature| (*feature).to_string())
            .collect(),
    });
}

/// What a row expects of the answer: the status, the error code and the whole message.
struct Refusal {
    status: u16,
    code: &'static str,
    message: String,
}

/// One row: the call it makes, and what the answer must be.
struct Row {
    case: &'static str,
    fields: Vec<(&'static str, String)>,
    expected: Refusal,
}

/// RULE: `AssumeRoot` reads the request's own values before it decides the grant — a target
/// principal that names no account, a task policy outside the published root-task family and
/// a value past a member's modelled maximum are each refused `ValidationError`, while a
/// well-formed request reaches the authorization and is refused `AccessDenied` by an account
/// whose organization grants no root session.
#[tokio::test]
async fn assume_root_validates_the_request_before_it_decides_the_grant() {
    let (router, _engine) = emulator();

    // Rows differ along what the rule generalises over: which member is at fault, which
    // FORM the value takes, and whether the fault is one the model states or one the service
    // does. The recorded cases — `DurationSeconds` one past its maximum, a target principal
    // that is not an account id, and `AdministratorAccess` as the task policy — are rows
    // among them.
    let rows: Vec<Row> = vec![
        Row {
            case: "the caller's own account as the target, at the maximum duration",
            fields: {
                let mut fields = params(&emulator_account(), TASK_POLICY_ARN);
                fields.push(("DurationSeconds", "900".to_string()));
                fields
            },
            expected: Refusal {
                status: 403,
                code: ACCESS_DENIED,
                message: not_authorized(&emulator_account()),
            },
        },
        Row {
            case: "a member account as the target, with no organization at all",
            fields: params(MEMBER_ACCOUNT, TASK_POLICY_ARN),
            expected: Refusal {
                status: 403,
                code: ACCESS_DENIED,
                message: not_authorized(MEMBER_ACCOUNT),
            },
        },
        Row {
            case: "the target named by its root principal ARN",
            fields: params(
                &format!("arn:aws:iam::{MEMBER_ACCOUNT}:root"),
                TASK_POLICY_ARN,
            ),
            expected: Refusal {
                status: 403,
                code: ACCESS_DENIED,
                message: not_authorized(MEMBER_ACCOUNT),
            },
        },
        Row {
            case: "a duration one past the maximum",
            fields: {
                let mut fields = params(&emulator_account(), TASK_POLICY_ARN);
                fields.push(("DurationSeconds", "901".to_string()));
                fields
            },
            expected: Refusal {
                status: 400,
                code: VALIDATION_ERROR,
                message: range_clause("901", "durationSeconds", 900),
            },
        },
        Row {
            case: "a duration far past the maximum",
            fields: {
                let mut fields = params(&emulator_account(), TASK_POLICY_ARN);
                fields.push(("DurationSeconds", "43200".to_string()));
                fields
            },
            expected: Refusal {
                status: 400,
                code: VALIDATION_ERROR,
                message: range_clause("43200", "durationSeconds", 900),
            },
        },
        Row {
            case: "a minimum session token size one past the maximum",
            fields: {
                let mut fields = params(&emulator_account(), TASK_POLICY_ARN);
                fields.push(("MinimumSessionTokenSize", "4097".to_string()));
                fields
            },
            expected: Refusal {
                status: 400,
                code: VALIDATION_ERROR,
                message: range_clause("4097", "minimumSessionTokenSize", 4096),
            },
        },
        Row {
            case: "a target principal that is not an account id",
            fields: params("not-an-account-id-but-long-enough", TASK_POLICY_ARN),
            expected: Refusal {
                status: 400,
                code: VALIDATION_ERROR,
                message: sts::state::INVALID_TARGET_PRINCIPAL_MESSAGE.to_string(),
            },
        },
        Row {
            case: "a target principal that is an IAM user's ARN",
            fields: params(
                &format!("arn:aws:iam::{MEMBER_ACCOUNT}:user/alice"),
                TASK_POLICY_ARN,
            ),
            expected: Refusal {
                status: 400,
                code: VALIDATION_ERROR,
                message: sts::state::INVALID_TARGET_PRINCIPAL_MESSAGE.to_string(),
            },
        },
        Row {
            case: "a target principal of twelve characters that are not all digits",
            fields: params("00000000000a", TASK_POLICY_ARN),
            expected: Refusal {
                status: 400,
                code: VALIDATION_ERROR,
                message: sts::state::INVALID_TARGET_PRINCIPAL_MESSAGE.to_string(),
            },
        },
        Row {
            case: "a task policy AWS publishes outside the root-task family",
            fields: params(
                &emulator_account(),
                "arn:aws:iam::aws:policy/AdministratorAccess",
            ),
            expected: Refusal {
                status: 400,
                code: VALIDATION_ERROR,
                message: sts::state::INVALID_TASK_POLICY_ARN_MESSAGE.to_string(),
            },
        },
        Row {
            case: "a job-function policy as the task policy",
            fields: params(
                &emulator_account(),
                "arn:aws:iam::aws:policy/job-function/SystemAdministrator",
            ),
            expected: Refusal {
                status: 400,
                code: VALIDATION_ERROR,
                message: sts::state::INVALID_TASK_POLICY_ARN_MESSAGE.to_string(),
            },
        },
        Row {
            case: "a root-task policy name filed under another path",
            fields: params(
                &emulator_account(),
                "arn:aws:iam::aws:policy/S3UnlockBucketPolicy",
            ),
            expected: Refusal {
                status: 400,
                code: VALIDATION_ERROR,
                message: sts::state::INVALID_TASK_POLICY_ARN_MESSAGE.to_string(),
            },
        },
        Row {
            case: "an account's own policy at the root-task path",
            fields: params(
                &emulator_account(),
                &format!(
                    "arn:aws:iam::{}:policy/root-task/S3UnlockBucketPolicy",
                    emulator_account()
                ),
            ),
            expected: Refusal {
                status: 400,
                code: VALIDATION_ERROR,
                message: sts::state::INVALID_TASK_POLICY_ARN_MESSAGE.to_string(),
            },
        },
        Row {
            case: "a task policy that is no ARN at all",
            fields: params(&emulator_account(), "S3UnlockBucketPolicy"),
            expected: Refusal {
                status: 400,
                code: VALIDATION_ERROR,
                message: sts::state::INVALID_TASK_POLICY_ARN_MESSAGE.to_string(),
            },
        },
    ];

    for Row {
        case,
        fields,
        expected,
    } in rows
    {
        let (status, body) = call(&router, LONG_TERM_KEY, "AssumeRoot", &fields).await;
        assert_eq!(
            status, expected.status,
            "case {case:?}: expected {}, got {status} {body}",
            expected.status
        );
        let code = element(&body, "Code");
        assert_eq!(
            code.as_deref(),
            Some(expected.code),
            "case {case:?}: expected <Code>{}</Code>, got {code:?} in {body}",
            expected.code
        );
        let message = decode_xml_text(&member(case, &body, "Message"));
        assert_eq!(
            message, expected.message,
            "case {case:?}: expected {:?}, got {message:?}",
            expected.message
        );
        assert!(
            element(&body, "Credentials").is_none(),
            "case {case:?}: a refused request must vend no credential, got {body}"
        );
    }
}

/// RULE: a privileged session is launched only where the organization grants the caller
/// centralized root access for root sessions AND the target is a member account; the
/// credential it vends speaks for that account's root user, which `GetCallerIdentity` reads
/// back for it.
#[tokio::test]
async fn a_privileged_session_is_launched_only_on_a_member_account_of_a_granting_organization()
{
    let (router, engine) = emulator();

    // No organization has the feature yet: the grant is absent, so the well-formed request
    // is refused.
    let refused = call(
        &router,
        LONG_TERM_KEY,
        "AssumeRoot",
        &params(MEMBER_ACCOUNT, TASK_POLICY_ARN),
    )
    .await;
    assert_eq!(
        refused.0, 403,
        "case \"no organization\": expected 403, got {} {}",
        refused.0, refused.1
    );

    // The organization exists and administers centralized root access, but only the OTHER
    // feature of it is enabled, so root sessions are still not granted.
    seed_organization(&engine, &["RootCredentialsManagement"]);
    let (status, body) = call(
        &router,
        LONG_TERM_KEY,
        "AssumeRoot",
        &params(MEMBER_ACCOUNT, TASK_POLICY_ARN),
    )
    .await;
    assert_eq!(
        status, 403,
        "case \"root sessions not enabled\": expected 403, got {status} {body}"
    );
    assert_eq!(
        element(&body, "Code").as_deref(),
        Some(ACCESS_DENIED),
        "case \"root sessions not enabled\": expected {ACCESS_DENIED}, got {body}"
    );

    // Root sessions enabled for the organization: the grant is in force.
    seed_organization(&engine, &["RootCredentialsManagement", "RootSessions"]);

    // The management account is no member account of its own organization, so it is not a
    // target even with the grant in force — and a malformed request is still refused on its
    // values rather than on the grant.
    for (case, fields, expected_status, expected_code) in [
        (
            "the management account as the target",
            params(&emulator_account(), TASK_POLICY_ARN),
            403,
            ACCESS_DENIED,
        ),
        (
            "a malformed target principal under a granting organization",
            params("not-an-account-id-but-long-enough", TASK_POLICY_ARN),
            400,
            VALIDATION_ERROR,
        ),
        (
            "a task policy outside the family under a granting organization",
            params(MEMBER_ACCOUNT, "arn:aws:iam::aws:policy/AdministratorAccess"),
            400,
            VALIDATION_ERROR,
        ),
    ] {
        let (status, body) = call(&router, LONG_TERM_KEY, "AssumeRoot", &fields).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected {expected_status}, got {status} {body}"
        );
        assert_eq!(
            element(&body, "Code").as_deref(),
            Some(expected_code),
            "case {case:?}: expected <Code>{expected_code}</Code>, got {body}"
        );
    }

    // Rows spanning the forms the granted request takes: both spellings of the target
    // account, every task policy of the published family, and the duration at each end of
    // its modelled range.
    let task_policies: Vec<String> = iam::state::AWS_MANAGED_POLICIES
        .iter()
        .filter(|entry| entry.path == "/root-task/")
        .map(iam::state::AwsManagedPolicy::arn)
        .collect();
    assert!(
        task_policies.len() >= 5,
        "expected the published root-task family, got {task_policies:?}"
    );
    let targets = [
        MEMBER_ACCOUNT.to_string(),
        format!("arn:aws:iam::{MEMBER_ACCOUNT}:root"),
    ];
    let durations = [None, Some(900)];
    let mut minted_keys: Vec<String> = Vec::new();

    for task_policy_arn in &task_policies {
        for target in &targets {
            for duration in durations {
                let case = format!(
                    "{target} scoped by {task_policy_arn} for {duration:?} seconds"
                );
                let mut fields = params(target, task_policy_arn);
                if let Some(seconds) = duration {
                    fields.push(("DurationSeconds", seconds.to_string()));
                }
                let (status, body) = call(&router, LONG_TERM_KEY, "AssumeRoot", &fields).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected 200, got {status} {body}"
                );
                let access_key_id = member(&case, &body, "AccessKeyId");
                assert!(
                    !minted_keys.contains(&access_key_id),
                    "case {case:?}: AccessKeyId {access_key_id:?} repeats an earlier session's"
                );
                minted_keys.push(access_key_id.clone());
                assert!(
                    !member(&case, &body, "SecretAccessKey").is_empty()
                        && !member(&case, &body, "SessionToken").is_empty(),
                    "case {case:?}: a minted credential carries both secrets, got {body}"
                );
                // The reported size is the size of the token the response carries.
                let reported = member(&case, &body, "SessionTokenSize");
                assert_eq!(
                    reported,
                    member(&case, &body, "SessionToken").len().to_string(),
                    "case {case:?}: SessionTokenSize must be the size of the token returned, \
                     got {body}"
                );

                // Round trip: the credential speaks for the MEMBER account's root user.
                let (status, identity) =
                    call(&router, &access_key_id, "GetCallerIdentity", &[]).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the minted credential must resolve, got {status} {identity}"
                );
                for (name, expected) in [
                    ("Account", MEMBER_ACCOUNT.to_string()),
                    ("UserId", MEMBER_ACCOUNT.to_string()),
                    ("Arn", format!("arn:aws:iam::{MEMBER_ACCOUNT}:root")),
                ] {
                    let reported = member(&case, &identity, name);
                    assert_eq!(
                        reported, expected,
                        "case {case:?}: expected {name} {expected:?}, got {reported:?}"
                    );
                }

                // The session the registry holds for it carries the task policy as its
                // scope, which is what caps what the credential may do.
                let scope = sts::state::service_state(&*engine)
                    .expect("the STS store")
                    .lock()
                    .session(&access_key_id)
                    .map(|session| session.policies.managed_arns.clone())
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: the mint recorded no session under {access_key_id:?}")
                    });
                assert_eq!(
                    scope,
                    vec![task_policy_arn.clone()],
                    "case {case:?}: expected the task policy as the session's scope, got \
                     {scope:?}"
                );
            }
        }
    }

    // The bottom of the modelled duration range is a legal value too: a session of it is
    // minted and the credential is vended. Nothing is read back with it — such a session is
    // over at or within a second of the instant it was minted, so what a later call makes of
    // it is the clock's answer and not this rule's.
    for seconds in [0, 1] {
        let case = format!("a {seconds}-second privileged session");
        let mut fields = params(MEMBER_ACCOUNT, TASK_POLICY_ARN);
        fields.push(("DurationSeconds", seconds.to_string()));
        let (status, body) = call(&router, LONG_TERM_KEY, "AssumeRoot", &fields).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {body}"
        );
        assert!(
            !member(&case, &body, "AccessKeyId").is_empty(),
            "case {case:?}: expected a minted credential, got {body}"
        );
    }
}
