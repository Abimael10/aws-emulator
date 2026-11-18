// SPDX-License-Identifier: CC0-1.0

//! What the SESSION POLICIES a temporary credential was minted with do to the EC2 calls that
//! credential signs — driven end to end over the wire the service speaks (`ec2Query`), with
//! the role, the documents in force on it and the session held by the IAM and STS services
//! mounted on the SAME engine, so the scope under test is the one `sts:AssumeRole` recorded
//! for the credential and the principal a refusal blames is the one that mint reports.
//!
//! THE RULE UNDER TEST: EC2 decides every request against EVERY policy document in force on
//! the credential it presented, as each reads AT THE INSTANT OF THE CALL — the identity-based
//! documents of the principal the credential speaks for AND the session policies the mint of
//! that credential passed, which cap what those documents grant and grant nothing of their
//! own: "the resulting session's permissions are the intersection of the role's
//! identity-based policy and the session policies… You cannot use session policies to grant
//! more permissions than those allowed by the identity-based policy of the role that is being
//! assumed" (`AssumeRoleRequest$PolicyArns`). So an explicit `Deny` in a session policy
//! refuses the call with HTTP 403 `UnauthorizedOperation` naming that document, a session
//! policy that states nothing about the action refuses it as an absent grant, and a call both
//! sides allow is served — whichever of the two sides the decision rested on, the caller is
//! ANSWERED rather than left waiting, which is what lets a client that polls for a permission
//! change observe it.
//!
//! The table in [`super::account_wide_read_authorization`] spans the identity-based documents
//! a read is decided under and the kind of credential that presented it, and
//! [`super::lookup_role_policy_changes`] spans those documents changing under a live session;
//! this suite is driven through the first suite's harness and spans the remaining dimension:
//! the SCOPE the mint passed. Its rows differ in where the scoping document lives (none at
//! all, an inline document, a managed policy named by ARN, both at once), what it says (an
//! allow of the whole read family, of one action, a deny of one action, a deny of every
//! action of this service, a grant scoped to one resource ARN) and which of the four reads it
//! is therefore about — plus the two decision paths a scope is read on: an action judged over
//! every resource, one judged on the resources the request addresses, and one that addresses
//! none.

use super::account_wide_read_authorization::{
    account, call_as, ec2_as, element, emulator, iam_call, session_of, statement, ACCOUNT_KEY_ID,
    READS, REGION, STS_API_VERSION,
};
use super::ec2_query::{element as opt_element, elements, error_code, error_message};

/// EC2's error code for a caller that may not perform the action it asked for — "if you have
/// the required permissions, the error response is DryRunOperation. Otherwise, it is
/// UnauthorizedOperation" (`DescribeVpcsRequest$DryRun`).
const UNAUTHORIZED_OPERATION: &str = "UnauthorizedOperation";

/// The reason clauses a refusal closes with, each the shared wording every service that
/// reaches the same decision states (`service_common::iam_authorization`), so this suite
/// asserts the clause the gate's decision rested on rather than a second spelling of it.
fn session_explicit_deny() -> String {
    service_common::iam_authorization::SESSION_POLICY_EXPLICIT_DENY.to_string()
}
fn no_session_policy_allows(action: &str) -> String {
    service_common::iam_authorization::no_session_policy_allows(action)
}
fn no_identity_policy_allows(action: &str) -> String {
    service_common::iam_authorization::no_identity_policy_allows(action)
}

/// The refusal EC2 words for `principal` asking for `action`, closed by `reason`: its own
/// fixed opening sentence, then IAM's clause. An action IAM judges over every resource names
/// no resource, there being none to name.
fn refusal(principal: &str, action: &str, reason: &str) -> String {
    format!(
        "You are not authorized to perform this operation. User: {principal} is not \
         authorized to perform: {action} {reason}"
    )
}

/// The same refusal for an action judged ON a resource the request addressed, which the
/// sentence names and closes with a full stop.
fn refusal_on(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "You are not authorized to perform this operation. User: {principal} is not \
         authorized to perform: {action} on resource: {resource} {reason}."
    )
}

/// A document with one `Allow` of `action` over every resource and one `Deny` of `denied`,
/// which is the shape a scope that takes one action away from a grant has.
fn allow_and_deny(action: &str, denied: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[
         {{"Effect":"Allow","Action":"{action}","Resource":"*"}},
         {{"Effect":"Deny","Action":"{denied}","Resource":"*"}}]}}"#
    )
}

/// The identity-based grant the role under test carries: the `Describe` family over every
/// resource, which is what a lookup role is given to read an account's network with.
fn allow_describes() -> (&'static str, String) {
    ("allow-describes", statement("Allow", "ec2:Describe*", "*"))
}

/// Mint a session of `role_name` SCOPED by the session policies `inline` and `managed`, as a
/// client that narrows a role's permissions for one task does: answers the new session's
/// access key id and the ARN a refusal of its calls must name.
///
/// `managed` names policies by ARN (`PolicyArns.member.<n>.arn`), which stand for whatever
/// their default version says at the instant of each later request.
async fn scoped_session(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    role_name: &str,
    session_name: &str,
    inline: Option<&str>,
    managed: &[String],
) -> (String, String) {
    let role_arn = format!("arn:aws:iam::{}:role/{role_name}", account());
    let mut params: Vec<(String, String)> = vec![
        ("RoleArn".to_string(), role_arn),
        ("RoleSessionName".to_string(), session_name.to_string()),
    ];
    if let Some(document) = inline {
        params.push(("Policy".to_string(), document.to_string()));
    }
    for (index, policy_arn) in managed.iter().enumerate() {
        params.push((
            format!("PolicyArns.member.{}.arn", index + 1),
            policy_arn.clone(),
        ));
    }
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let (status, assumed) = call_as(
        router,
        "sts",
        STS_API_VERSION,
        ACCOUNT_KEY_ID,
        "AssumeRole",
        &borrowed,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the scoped session must be minted, got {status} {assumed}"
    );
    (
        element(case, &assumed, "AccessKeyId"),
        element(case, &assumed, "Arn"),
    )
}

/// Create the customer-managed policy `name` with `document` as its first version: answers
/// its ARN, which a mint names to scope a session by it.
async fn managed_policy(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    name: &str,
    document: &str,
) -> String {
    let (status, created) = iam_call(
        router,
        "CreatePolicy",
        &[("PolicyName", name), ("PolicyDocument", document)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the managed policy {name:?} must be created, got {status} {created}"
    );
    element(case, &created, "Arn")
}

/// What a row expects one call to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// HTTP 200: both the identity's documents and the scope allow the action.
    Served,
    /// HTTP 403 because a statement of a SESSION POLICY about the request says `Deny`.
    SessionDeny,
    /// HTTP 403 because no session policy allows the action at all.
    NoSessionAllow,
    /// HTTP 403 because no identity-based policy allows it — what a scope cannot lift.
    NoIdentityAllow,
}

impl Expect {
    /// The reason clause a refusal of `action` closes with under this expectation.
    fn reason(self, action: &str) -> String {
        match self {
            Expect::Served => unreachable!("a served call carries no refusal"),
            Expect::SessionDeny => session_explicit_deny(),
            Expect::NoSessionAllow => no_session_policy_allows(action),
            Expect::NoIdentityAllow => no_identity_policy_allows(action),
        }
    }
}

/// Assert what the call `read` signed by `akid` answered, and — when it was refused — that
/// the refusal carries EC2's status, code and sentence, naming the document the decision
/// rested on.
async fn assert_read(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    akid: &str,
    principal: &str,
    read: &str,
    expected: Expect,
) {
    let (status, body) = ec2_as(router, akid, read, &[]).await;
    if expected == Expect::Served {
        assert_eq!(
            status, 200,
            "case {case:?} / {read}: expected the read to be served, got {status} {body}"
        );
        return;
    }
    let action = format!("ec2:{read}");
    assert_eq!(
        (
            status,
            error_code(&body).as_str(),
            error_message(&body).as_str()
        ),
        (
            403,
            UNAUTHORIZED_OPERATION,
            refusal(principal, &action, &expected.reason(&action)).as_str()
        ),
        "case {case:?} / {read}: expected {expected:?}, got {status} {body}"
    );
}

// ---------------------------------------------------------------------------
// The scope the mint passed decides each read
// ---------------------------------------------------------------------------

/// One row: the session policies one mint passed, and what each of [`READS`] must answer for
/// the credential it vended.
struct ScopeRow {
    case: &'static str,
    session: &'static str,
    /// The inline session policy document the mint passes, if any.
    inline: Option<String>,
    /// The document of the managed session policy the mint names by ARN, if any.
    managed: Option<String>,
    /// What the four [`READS`] answer, in that order.
    expect: [Expect; 4],
}

/// RULE: a request to EC2 is decided against the session policies the mint of its credential
/// passed as well as against the role's own documents — the scope caps that grant and grants
/// nothing of its own, so an explicit `Deny` in it refuses the action it names with EC2's
/// `UnauthorizedOperation` naming the session policy, an action the scope states nothing
/// about is refused as an absent grant, and what both sides allow is served.
#[tokio::test]
async fn the_scope_a_mint_passed_decides_each_account_wide_read() {
    let router = emulator();
    let role = "scoped-lookup-role";
    let case = "the role the scoped sessions are minted of";
    // The role itself may read the whole family, so every refusal below is the SCOPE's.
    let (unscoped_akid, unscoped_principal) =
        session_of(&router, case, role, &[allow_describes()], &[]).await;
    for read in READS {
        assert_read(
            &router,
            "a session no mint scoped",
            &unscoped_akid,
            &unscoped_principal,
            read,
            Expect::Served,
        )
        .await;
    }

    let vpc_arn = format!(
        "arn:aws:ec2:{REGION}:{}:vpc/vpc-0123456789abcdef0",
        account()
    );
    let rows = vec![
        ScopeRow {
            case: "a mint that passed no session policy at all",
            session: "unscoped",
            inline: None,
            managed: None,
            expect: [Expect::Served; 4],
        },
        ScopeRow {
            case: "an inline scope allowing the whole read family",
            session: "inline-family",
            inline: Some(statement("Allow", "ec2:Describe*", "*")),
            managed: None,
            expect: [Expect::Served; 4],
        },
        ScopeRow {
            case: "an inline scope denying one read beside an allow of the family",
            session: "inline-deny-one",
            inline: Some(allow_and_deny("ec2:Describe*", "ec2:DescribeVpcs")),
            managed: None,
            // The deny names ONE action, so the other three reads are untouched.
            expect: [
                Expect::Served,
                Expect::Served,
                Expect::Served,
                Expect::SessionDeny,
            ],
        },
        ScopeRow {
            case: "an inline scope allowing exactly one read",
            session: "inline-one-read",
            inline: Some(statement("Allow", "ec2:DescribeSubnets", "*")),
            managed: None,
            expect: [
                Expect::NoSessionAllow,
                Expect::NoSessionAllow,
                Expect::Served,
                Expect::NoSessionAllow,
            ],
        },
        ScopeRow {
            case: "an inline scope denying every action of this service",
            session: "inline-deny-service",
            inline: Some(allow_and_deny("ec2:Describe*", "ec2:*")),
            managed: None,
            expect: [Expect::SessionDeny; 4],
        },
        ScopeRow {
            case: "an inline scope whose grant is scoped to one resource ARN",
            // A `Describe` action supports no resource-level permissions, so a scope naming
            // particular ARNs is not about a request IAM judges over every resource — it
            // allows nothing here, exactly as it allows nothing on AWS.
            session: "inline-resource-scoped",
            inline: Some(statement("Allow", "ec2:DescribeVpcs", &vpc_arn)),
            managed: None,
            expect: [Expect::NoSessionAllow; 4],
        },
        ScopeRow {
            case: "a managed scope allowing the whole read family",
            session: "managed-family",
            inline: None,
            managed: Some(statement("Allow", "ec2:Describe*", "*")),
            expect: [Expect::Served; 4],
        },
        ScopeRow {
            case: "a managed scope denying one read beside an inline allow of the family",
            session: "managed-deny-one",
            inline: Some(statement("Allow", "ec2:Describe*", "*")),
            managed: Some(statement("Deny", "ec2:DescribeRouteTables", "*")),
            expect: [
                Expect::Served,
                Expect::SessionDeny,
                Expect::Served,
                Expect::Served,
            ],
        },
    ];

    for row in &rows {
        let case = row.case;
        let mut managed = Vec::new();
        if let Some(document) = &row.managed {
            managed.push(
                managed_policy(
                    &router,
                    case,
                    &format!("scope-{}", row.session),
                    document,
                )
                .await,
            );
        }
        let (akid, principal) = scoped_session(
            &router,
            case,
            role,
            row.session,
            row.inline.as_deref(),
            &managed,
        )
        .await;
        for (read, expected) in READS.iter().zip(row.expect.iter()) {
            assert_read(&router, case, &akid, &principal, read, *expected).await;
        }
    }
}

// ---------------------------------------------------------------------------
// The scope is read at the instant of the call
// ---------------------------------------------------------------------------

/// RULE: a managed session policy stands for the version that is in force — "the one that is
/// in effect for the IAM users, groups, and roles to which the policy is attached"
/// (`SetDefaultPolicyVersion`) — read at the instant of each call, so storing a version that
/// denies a read refuses the NEXT call made with the credential already minted, and putting
/// the earlier version back in force serves it again. Nothing of the decision is remembered
/// from the mint.
#[tokio::test]
async fn a_managed_scope_decides_the_next_call_at_the_version_in_force() {
    let router = emulator();
    let role = "scope-version-role";
    let case = "a managed scope whose default version changes";
    let (_, _) = session_of(&router, case, role, &[allow_describes()], &[]).await;
    let policy_arn = managed_policy(
        &router,
        case,
        "scope-version-policy",
        &statement("Allow", "ec2:Describe*", "*"),
    )
    .await;
    let (akid, principal) =
        scoped_session(
            &router,
            case,
            role,
            "versioned",
            None,
            std::slice::from_ref(&policy_arn),
        )
        .await;

    // At the version the mint read, the scope allows the whole family.
    for read in READS {
        assert_read(&router, case, &akid, &principal, read, Expect::Served).await;
    }

    // A new version that denies one read, put in force under the live session.
    let (status, stored) = iam_call(
        &router,
        "CreatePolicyVersion",
        &[
            ("PolicyArn", &policy_arn),
            (
                "PolicyDocument",
                &allow_and_deny("ec2:Describe*", "ec2:DescribeVpcs"),
            ),
            ("SetAsDefault", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the second version must be stored, got {status} {stored}"
    );
    let second = element(case, &stored, "VersionId");
    assert_ne!(
        second, "v1",
        "case {case:?}: expected a version of its own, got {stored}"
    );
    for (read, expected) in READS.iter().zip(
        [
            Expect::Served,
            Expect::Served,
            Expect::Served,
            Expect::SessionDeny,
        ]
        .iter(),
    ) {
        assert_read(&router, case, &akid, &principal, read, *expected).await;
    }

    // The earlier version back in force: the same credential is served again.
    let (status, restored) = iam_call(
        &router,
        "SetDefaultPolicyVersion",
        &[("PolicyArn", &policy_arn), ("VersionId", "v1")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the first version must be restorable, got {status} {restored}"
    );
    for read in READS {
        assert_read(&router, case, &akid, &principal, read, Expect::Served).await;
    }
}

/// RULE: a session policy grants nothing of its own — "you cannot use session policies to
/// grant more permissions than those allowed by the identity-based policy of the role that is
/// being assumed" (`AssumeRoleRequest$PolicyArns`) — so a scope that allows the whole read
/// family over a role carrying no grant at all leaves every read refused, and the refusal
/// names the side of the intersection that withheld it.
#[tokio::test]
async fn a_scope_grants_nothing_the_roles_own_documents_do_not() {
    let router = emulator();
    let role = "bare-scoped-role";
    let case = "a scope over a role carrying no grant";
    let (_, _) = session_of(&router, case, role, &[], &[]).await;
    let (akid, principal) = scoped_session(
        &router,
        case,
        role,
        "granting",
        Some(&statement("Allow", "ec2:Describe*", "*")),
        &[],
    )
    .await;
    for read in READS {
        assert_read(
            &router,
            case,
            &akid,
            &principal,
            read,
            Expect::NoIdentityAllow,
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// The scope decides the mutating calls too, on the resources they address
// ---------------------------------------------------------------------------

/// RULE: the scope decides every action, not only the reads judged over every resource: an
/// action judged on the resources the request ADDRESSES is refused on the resource the scope
/// denies it for — which the refusal names — and an action that addresses no resource at all
/// is refused when no session policy allows it. A refused call performs nothing: the estate
/// is exactly what it was.
#[tokio::test]
async fn the_scope_decides_an_action_judged_on_the_resource_it_addresses() {
    let router = emulator();
    let role = "scoped-writer-role";
    let case = "a role allowed every action of this service";
    let (_, _) = session_of(
        &router,
        case,
        role,
        &[("allow-ec2", statement("Allow", "ec2:*", "*"))],
        &[],
    )
    .await;

    // A session scoped to the reads alone: the create it does not allow is refused, and
    // creates nothing.
    let (reader_akid, reader_principal) = scoped_session(
        &router,
        case,
        role,
        "reader",
        Some(&statement("Allow", "ec2:Describe*", "*")),
        &[],
    )
    .await;
    let (status, before) = ec2_as(&router, ACCOUNT_KEY_ID, "DescribeVpcs", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the account's own read must be served, got {status} {before}"
    );
    let held_before = elements(&before, "vpcId");
    let (status, refused) = ec2_as(
        &router,
        &reader_akid,
        "CreateVpc",
        &[("CidrBlock", "10.77.0.0/16")],
    )
    .await;
    assert_eq!(
        (
            status,
            error_code(&refused).as_str(),
            error_message(&refused).as_str()
        ),
        (
            403,
            UNAUTHORIZED_OPERATION,
            refusal(
                &reader_principal,
                "ec2:CreateVpc",
                &no_session_policy_allows("ec2:CreateVpc")
            )
            .as_str()
        ),
        "case {case:?}: expected the create to be refused by the scope, got {status} {refused}"
    );
    let (status, after) = ec2_as(&router, ACCOUNT_KEY_ID, "DescribeVpcs", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the account's own read must be served, got {status} {after}"
    );
    assert_eq!(
        elements(&after, "vpcId"),
        held_before,
        "case {case:?}: expected the refused create to leave the estate alone, got {after}"
    );

    // A session scoped to every action of this service creates the VPC…
    let (writer_akid, _) = scoped_session(
        &router,
        case,
        role,
        "writer",
        Some(&statement("Allow", "ec2:*", "*")),
        &[],
    )
    .await;
    let (status, created) = ec2_as(
        &router,
        &writer_akid,
        "CreateVpc",
        &[("CidrBlock", "10.78.0.0/16")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the create must be served for a scope that allows it, got {status} \
         {created}"
    );
    let vpc_id = element(case, &created, "vpcId");

    // …and a session whose scope DENIES the delete is refused ON the VPC it addressed,
    // which still exists afterwards.
    let (deleter_akid, deleter_principal) = scoped_session(
        &router,
        case,
        role,
        "deleter",
        Some(&allow_and_deny("ec2:*", "ec2:DeleteVpc")),
        &[],
    )
    .await;
    let (status, refused) = ec2_as(&router, &deleter_akid, "DeleteVpc", &[("VpcId", &vpc_id)]).await;
    let vpc_arn = ::engine::emulator_arn("ec2", REGION, &format!("vpc/{vpc_id}"));
    assert_eq!(
        (
            status,
            error_code(&refused).as_str(),
            error_message(&refused).as_str()
        ),
        (
            403,
            UNAUTHORIZED_OPERATION,
            refusal_on(
                &deleter_principal,
                "ec2:DeleteVpc",
                &vpc_arn,
                &session_explicit_deny()
            )
            .as_str()
        ),
        "case {case:?}: expected the delete to be refused on {vpc_arn}, got {status} {refused}"
    );
    let (status, reported) = ec2_as(
        &router,
        ACCOUNT_KEY_ID,
        "DescribeVpcs",
        &[("VpcId.1", &vpc_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read back must be served, got {status} {reported}"
    );
    assert_eq!(
        opt_element(&reported, "vpcId"),
        Some(vpc_id.clone()),
        "case {case:?}: expected {vpc_id} to still be held, got {reported}"
    );
}
