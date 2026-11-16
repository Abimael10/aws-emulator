// SPDX-License-Identifier: CC0-1.0

//! WHAT a `DryRun` probe answers the caller, driven end to end over the wire EC2 speaks
//! (`ec2Query`) with the IAM and STS services mounted on the SAME engine, so the documents
//! under test are the ones `iam:PutRolePolicy` / `iam:PutUserPolicy` store and the principal
//! a refusal blames is the one `sts:AssumeRole` or `iam:CreateAccessKey` minted the
//! credential for.
//!
//! THE RULE UNDER TEST: `DryRun` "checks whether you have the required permissions for the
//! action, without actually making the request, and provides an error response. If you have
//! the required permissions, the error response is DryRunOperation. Otherwise, it is
//! UnauthorizedOperation" (the `DryRun` member's own documentation, carried by every input
//! shape that models it). So the probe is an AUTHORIZATION question and its two answers are
//! the two sides of IAM's decision about `ec2:<OperationName>` on the resources the request
//! addresses: HTTP 412 `DryRunOperation` with one fixed sentence when the documents in force
//! on the principal the request's credential speaks for allow it, and otherwise HTTP 403
//! `UnauthorizedOperation` naming that principal, the action, the resource the decision was
//! taken on and which of IAM's rules withheld the grant. Neither answer performs the
//! request.
//!
//! The tables span what the rule generalises over:
//!
//! * WHICH actions the documents in force name — none of this service's, the exact action
//!   asked for, an action whose name merely extends or is extended by it, another action of
//!   this service, the action under a wildcard, the action in another ASCII case, the action
//!   scoped to the resource the request addresses, the action scoped to another resource of
//!   the same kind, everything BUT the action, and the action under an explicit deny;
//! * WHICH principal the credential speaks for — an assumed-role session and an IAM user
//!   holding a long-term key, each of which a refusal names as itself;
//! * WHETHER this emulator has written the operation at all, since the probe's answer is
//!   about the caller's permissions rather than about the work the operation would do.
//!
//! No row is the specification: each is one input along what the rule generalises over. The
//! codes, statuses and sentences are written out as AWS words them, never read back out of
//! the implementation.

use std::sync::Arc;

use service_com_amazonaws_ec2_amazonec2 as ec2;
use service_com_amazonaws_iam_awsidentitymanagementv20100508 as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615 as sts;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{elements, error_code, error_message, API_VERSION as EC2_API_VERSION};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The access key id of the account's own published credential: a key neither STS nor IAM
/// holds a record for, which is the position AWS account root credentials are in — the
/// identity that provisions the fixtures and reads the store back.
const ACCOUNT_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// The IAM API version a client sends in an awsQuery body.
const IAM_API_VERSION: &str = "2010-05-08";

/// The STS API version a client sends in an awsQuery body.
const STS_API_VERSION: &str = "2011-06-15";

/// EC2's error code for a `DryRun` whose caller DOES hold the action.
const DRY_RUN_OPERATION: &str = "DryRunOperation";

/// The status that answer carries.
const DRY_RUN_STATUS: u16 = 412;

/// What `DryRunOperation` states: the request was authorized and then stopped.
const DRY_RUN_MESSAGE: &str = "Request would have succeeded, but DryRun flag is set.";

/// EC2's error code for a caller that may not perform the action it asked for.
const UNAUTHORIZED_OPERATION: &str = "UnauthorizedOperation";

/// The status that refusal carries.
const UNAUTHORIZED_STATUS: u16 = 403;

/// The sentence EC2 opens an `UnauthorizedOperation` refusal with.
const NOT_AUTHORIZED_SENTENCE: &str = "You are not authorized to perform this operation.";

/// The create a dry-run permission probe is the documented way to pre-flight: it addresses
/// the subnet it would put the interface in, which is the resource IAM judges it on.
const CREATE_INTERFACE: &str = "CreateNetworkInterface";

/// An operation this emulator has NOT written that models `DryRun` and addresses a resource
/// of its own, so the probe's answer for it cannot come from any handler's own work.
const UNWRITTEN_OPERATION: &str = "UnlockSnapshot";

/// A well-formed snapshot id for that probe. Nothing is looked up: the probe is answered
/// from the caller's permissions, and the resource the decision is taken on is read from the
/// id the request carries.
const PROBED_SNAPSHOT: &str = "snap-0fedcba987654321f";

/// A well-formed subnet id that is NOT the one the rows' request names, for the row whose
/// grant is scoped to another resource of the same kind.
const ANOTHER_SUBNET: &str = "subnet-0123456789abcdef0";

/// Two actions of ANOTHER service, which say nothing about this one.
const OTHER_SERVICE_ACTIONS: [&str; 2] =
    ["secretsmanager:DescribeSecret", "secretsmanager:GetSecretValue"];

/// What a row expects the probe to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// HTTP 412 `DryRunOperation`: the documents in force allow the action on the resource
    /// the request addresses, so the probe reports that the request would have succeeded.
    HoldsTheAction,
    /// HTTP 403 `UnauthorizedOperation` because no readable document allows the action on
    /// that resource.
    NoAllow,
    /// HTTP 403 `UnauthorizedOperation` because a statement about the request says `Deny`.
    ExplicitDeny,
}

/// The emulator the rows drive: EC2 beside the IAM and STS services on ONE shared engine, so
/// the policies a row stores and the credential it mints are the ones EC2's gate reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(ec2::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(iam::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(sts::EmulatedService::new(Arc::clone(&engine)));
    router
}

/// One query-protocol call to `service`, presented as signed by the access key id `akid` —
/// the only place the credential appears on the wire, and so what the service reads the
/// caller's identity from. Built by the harness every suite shares
/// ([`super::ec2_query::query_call_as`]), so this suite carries no second copy of the
/// envelope or of its encoding.
async fn call_as(
    router: &MultiServiceRouter,
    service: &str,
    api_version: &str,
    akid: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    super::ec2_query::query_call_as(router, REGION, service, api_version, akid, action, params)
        .await
}

/// One EC2 call signed by `akid`.
async fn ec2_as(
    router: &MultiServiceRouter,
    akid: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    call_as(router, "ec2", EC2_API_VERSION, akid, action, params).await
}

/// One EC2 call made by the account's own published credential, which holds no IAM policies
/// and is therefore not bounded by them: the fixtures and the read-backs go through it.
async fn ec2_as_account(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = ec2_as(router, ACCOUNT_KEY_ID, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} by the account's own credential must be served, got \
         {status} {body}"
    );
    body
}

/// One IAM call, made by the account's own published credential.
async fn iam_call(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = call_as(
        router,
        "iam",
        IAM_API_VERSION,
        ACCOUNT_KEY_ID,
        action,
        params,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must be served, got {status} {body}"
    );
    body
}

/// The text of the first `<tag>` element of a query response, or a panic naming the case.
fn element(case: &str, body: &str, tag: &str) -> String {
    super::ec2_query::element(body, tag)
        .unwrap_or_else(|| panic!("case {case:?}: no <{tag}> in {body}"))
}

/// This emulator's account id, which every ARN below carries.
fn account() -> &'static str {
    ::engine::emulator_account_id()
}

/// A trust policy admitting the account's own principals to assume the role, which is what
/// lets the published credential mint the session a row signs with.
fn trust_policy() -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{}:root"}},"Action":"sts:AssumeRole"}}]}}"#,
        account()
    )
}

/// `["a","b"]` — the JSON array an `Action` element carries when a document names several
/// actions, which is how an inline grant of a handful of API calls is written.
fn json_list(values: &[&str]) -> String {
    let quoted: Vec<String> = values.iter().map(|value| format!("\"{value}\"")).collect();
    format!("[{}]", quoted.join(","))
}

/// One statement: `effect` on every action of `actions`, over `resource`.
fn statement(effect: &str, actions: &[&str], resource: &str) -> String {
    format!(
        r#"{{"Effect":"{effect}","Action":{},"Resource":"{resource}"}}"#,
        json_list(actions)
    )
}

/// One statement allowing everything EXCEPT `actions`, through the negated spelling of the
/// action element.
fn allow_everything_but(actions: &[&str]) -> String {
    format!(
        r#"{{"Effect":"Allow","NotAction":{},"Resource":"*"}}"#,
        json_list(actions)
    )
}

/// An identity-based document made of `statements`.
fn document(statements: &[String]) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{}]}}"#,
        statements.join(",")
    )
}

/// The document the recorded probe's principal carries: a grant of another service's actions
/// and of nothing of this one, optionally with `also` named beside them — which is how the
/// positive control of such a grant is written.
fn other_service_grant(also: &[&str]) -> String {
    let mut actions = OTHER_SERVICE_ACTIONS.to_vec();
    actions.extend_from_slice(also);
    document(&[statement("Allow", &actions, "*")])
}

/// Create the role `role_name` with the inline policy `inline` put on it, then assume it:
/// answers the session's access key id and the ARN `sts:GetCallerIdentity` reports for it,
/// which a refusal must name.
async fn session_of(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    inline: &str,
) -> (String, String) {
    let trust = trust_policy();
    iam_call(
        router,
        case,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    iam_call(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", role_name),
            ("PolicyName", "probed-grant"),
            ("PolicyDocument", inline),
        ],
    )
    .await;
    let role_arn = format!("arn:aws:iam::{}:role/{role_name}", account());
    let session_name = format!("{role_name}-session");
    let (status, assumed) = call_as(
        router,
        "sts",
        STS_API_VERSION,
        ACCOUNT_KEY_ID,
        "AssumeRole",
        &[("RoleArn", &role_arn), ("RoleSessionName", &session_name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the role must be assumable, got {status} {assumed}"
    );
    (
        element(case, &assumed, "AccessKeyId"),
        element(case, &assumed, "Arn"),
    )
}

/// Create the IAM user `user_name` with the inline policy `inline` put on it and mint its
/// long-term access key: answers that key's id and the user's own ARN.
async fn user_key_of(
    router: &MultiServiceRouter,
    case: &str,
    user_name: &str,
    inline: &str,
) -> (String, String) {
    let created = iam_call(router, case, "CreateUser", &[("UserName", user_name)]).await;
    let user_arn = element(case, &created, "Arn");
    iam_call(
        router,
        case,
        "PutUserPolicy",
        &[
            ("UserName", user_name),
            ("PolicyName", "probed-grant"),
            ("PolicyDocument", inline),
        ],
    )
    .await;
    let minted = iam_call(router, case, "CreateAccessKey", &[("UserName", user_name)]).await;
    (element(case, &minted, "AccessKeyId"), user_arn)
}

/// One probed request: the operation, the members the request carries beside the flag, and
/// the ARN of the resource those members address — the resource IAM takes the decision on
/// and a refusal names.
struct Probe<'a> {
    operation: &'a str,
    members: Vec<(&'a str, &'a str)>,
    judged_on: String,
}

impl<'a> Probe<'a> {
    /// The create of an interface in the subnet `subnet_id`, which the request addresses.
    fn create_interface(subnet_id: &'a str) -> Probe<'a> {
        Probe {
            operation: CREATE_INTERFACE,
            members: vec![("SubnetId", subnet_id)],
            judged_on: subnet_arn(subnet_id),
        }
    }

    /// The unlock of a snapshot: an operation nobody has written, addressing the snapshot the
    /// request names.
    fn unlock_snapshot() -> Probe<'static> {
        Probe {
            operation: UNWRITTEN_OPERATION,
            members: vec![("SnapshotId", PROBED_SNAPSHOT)],
            judged_on: ::engine::emulator_arn("ec2", REGION, &format!("snapshot/{PROBED_SNAPSHOT}")),
        }
    }

    /// The request's members with the flag set, which is what makes the call a probe.
    fn with_flag(&self) -> Vec<(&'a str, &'a str)> {
        let mut members = self.members.clone();
        members.push(("DryRun", "true"));
        members
    }
}

/// The ARN of the subnet `subnet_id`, as IAM names a subnet of this Region's account.
fn subnet_arn(subnet_id: &str) -> String {
    ::engine::emulator_arn("ec2", REGION, &format!("subnet/{subnet_id}"))
}

/// The refusal EC2 words for `principal` asking for `action` on `resource`, with the clause
/// the rule the decision was taken under states.
fn refusal_message(principal: &str, action: &str, resource: &str, expected: Expect) -> String {
    let reason = match expected {
        Expect::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        Expect::NoAllow => format!("because no identity-based policy allows the {action} action"),
        Expect::HoldsTheAction => unreachable!("a probe the caller holds carries no refusal"),
    };
    format!(
        "{NOT_AUTHORIZED_SENTENCE} User: {principal} is not authorized to perform: \
         {action} on resource: {resource} {reason}."
    )
}

/// Assert that the probe of `probe` signed by `akid` answered what `expected` requires, with
/// EC2's code, status and wording for that side of the decision.
async fn assert_probe(
    router: &MultiServiceRouter,
    case: &str,
    akid: &str,
    principal: &str,
    probe: &Probe<'_>,
    expected: Expect,
) {
    let (status, body) = ec2_as(router, akid, probe.operation, &probe.with_flag()).await;
    let name = probe.operation;
    match expected {
        Expect::HoldsTheAction => {
            assert_eq!(
                status, DRY_RUN_STATUS,
                "case {case:?} / {name}: expected HTTP {DRY_RUN_STATUS}, got {status} {body}"
            );
            assert_eq!(
                error_code(&body),
                DRY_RUN_OPERATION,
                "case {case:?} / {name}: expected {DRY_RUN_OPERATION}, got {body}"
            );
            assert_eq!(
                error_message(&body),
                DRY_RUN_MESSAGE,
                "case {case:?} / {name}: expected EC2's dry-run wording, got {body}"
            );
        }
        refused => {
            assert_eq!(
                status, UNAUTHORIZED_STATUS,
                "case {case:?} / {name}: expected HTTP {UNAUTHORIZED_STATUS}, got {status} {body}"
            );
            assert_eq!(
                error_code(&body),
                UNAUTHORIZED_OPERATION,
                "case {case:?} / {name}: expected {UNAUTHORIZED_OPERATION}, got {body}"
            );
            let iam_action = format!("ec2:{name}");
            assert_eq!(
                error_message(&body),
                refusal_message(principal, &iam_action, &probe.judged_on, refused),
                "case {case:?} / {name}: expected IAM's refusal wording, got {body}"
            );
        }
    }
}

/// A subnet of the Region's default network, read back through the account's own credential:
/// the subnet the probed create would put its interface in.
async fn a_subnet(router: &MultiServiceRouter) -> String {
    let described = ec2_as_account(router, "the default network", "DescribeSubnets", &[]).await;
    let subnet_id = element("the default network", &described, "subnetId");
    assert_ne!(
        subnet_id, ANOTHER_SUBNET,
        "the id standing for another subnet must not be the one the rows address"
    );
    subnet_id
}

/// Every network interface the account holds, read back through its own credential — what a
/// probe must leave exactly as it found it.
async fn held_interfaces(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let described = ec2_as_account(router, case, "DescribeNetworkInterfaces", &[]).await;
    elements(&described, "networkInterfaceId")
}

// ---------------------------------------------------------------------------
// Which actions the documents in force name
// ---------------------------------------------------------------------------

/// One row of the table below: the documents in force on the role the caller is a session of,
/// and what the probe must answer for it.
struct Row {
    case: &'static str,
    role: &'static str,
    inline: String,
    expect: Expect,
}

/// RULE: the probe's two answers are the two sides of IAM's decision about
/// `ec2:<OperationName>` on the resource the request addresses — so a document that does not
/// allow exactly that action on exactly that resource is answered `UnauthorizedOperation`
/// (HTTP 403) and never the `DryRunOperation` that reports the caller holds it.
#[tokio::test]
async fn the_probe_answers_the_action_the_documents_in_force_name() {
    let router = emulator();
    let subnet_id = a_subnet(&router).await;
    let probe = Probe::create_interface(&subnet_id);
    let this_subnet = subnet_arn(&subnet_id);
    let another_subnet = subnet_arn(ANOTHER_SUBNET);
    let action = CREATE_INTERFACE;
    let qualified = format!("ec2:{action}");
    let rows: Vec<Row> = vec![
        Row {
            case: "a role granted another service's actions and nothing of this one",
            role: "secrets-only-role",
            inline: other_service_grant(&[]),
            expect: Expect::NoAllow,
        },
        Row {
            case: "the same grant with the action asked for named beside them",
            role: "secrets-and-interface-role",
            inline: other_service_grant(&[qualified.as_str()]),
            expect: Expect::HoldsTheAction,
        },
        Row {
            case: "a role granted an action whose name the one asked for is a prefix of",
            role: "interface-permission-role",
            inline: document(&[statement(
                "Allow",
                &["ec2:CreateNetworkInterfacePermission"],
                "*",
            )]),
            expect: Expect::NoAllow,
        },
        Row {
            case: "a role granted an action whose name merely extends the one asked for",
            role: "plural-action-role",
            inline: document(&[statement("Allow", &["ec2:CreateNetworkInterfaces"], "*")]),
            expect: Expect::NoAllow,
        },
        Row {
            case: "a role granted another action of this service",
            role: "delete-interface-role",
            inline: document(&[statement("Allow", &["ec2:DeleteNetworkInterface"], "*")]),
            expect: Expect::NoAllow,
        },
        Row {
            case: "a role granted the action under a wildcard over this service's creates",
            role: "creates-role",
            inline: document(&[statement("Allow", &["ec2:Create*"], "*")]),
            expect: Expect::HoldsTheAction,
        },
        Row {
            // "IAM compares action names case-insensitively", so the spelling of the
            // statement's action is not what decides the call.
            case: "a role granted the action in another ASCII case",
            role: "lowercase-action-role",
            inline: document(&[statement("Allow", &["ec2:createnetworkinterface"], "*")]),
            expect: Expect::HoldsTheAction,
        },
        Row {
            case: "a role whose grant of the action is scoped to the subnet the request names",
            role: "this-subnet-role",
            inline: document(&[statement("Allow", &[qualified.as_str()], &this_subnet)]),
            expect: Expect::HoldsTheAction,
        },
        Row {
            case: "a role whose grant of the action is scoped to another subnet",
            role: "other-subnet-role",
            inline: document(&[statement("Allow", &[qualified.as_str()], &another_subnet)]),
            expect: Expect::NoAllow,
        },
        Row {
            case: "a role granted everything but the action asked for",
            role: "all-but-the-action-role",
            inline: document(&[allow_everything_but(&[qualified.as_str()])]),
            expect: Expect::NoAllow,
        },
        Row {
            case: "a role granted the action under an explicit deny of it",
            role: "denied-action-role",
            inline: document(&[
                statement("Allow", &["ec2:*"], "*"),
                statement("Deny", &[qualified.as_str()], "*"),
            ]),
            expect: Expect::ExplicitDeny,
        },
    ];

    for row in &rows {
        let (akid, principal) = session_of(&router, row.case, row.role, &row.inline).await;
        assert_probe(&router, row.case, &akid, &principal, &probe, row.expect).await;
    }
}

// ---------------------------------------------------------------------------
// Which principal the credential speaks for
// ---------------------------------------------------------------------------

/// RULE: the decision is taken on the principal the request's credential speaks for and the
/// refusal names THAT principal — the session ARN for a temporary credential, the user's own
/// ARN for a long-term one — so the same two documents decide the probe the same way for
/// either kind of principal.
#[tokio::test]
async fn the_probe_decides_on_the_principal_the_credential_speaks_for() {
    let router = emulator();
    let subnet_id = a_subnet(&router).await;
    let probe = Probe::create_interface(&subnet_id);
    let qualified = format!("ec2:{CREATE_INTERFACE}");

    let case = "an IAM user granted another service's actions only";
    let (refused_key, user_arn) =
        user_key_of(&router, case, "secrets-only-user", &other_service_grant(&[])).await;
    assert_probe(
        &router,
        case,
        &refused_key,
        &user_arn,
        &probe,
        Expect::NoAllow,
    )
    .await;

    let case = "an IAM user granted the action asked for beside them";
    let (holding_key, holding_arn) = user_key_of(
        &router,
        case,
        "secrets-and-interface-user",
        &other_service_grant(&[qualified.as_str()]),
    )
    .await;
    assert_probe(
        &router,
        case,
        &holding_key,
        &holding_arn,
        &probe,
        Expect::HoldsTheAction,
    )
    .await;
}

// ---------------------------------------------------------------------------
// An operation this emulator has not written
// ---------------------------------------------------------------------------

/// RULE, applied where there is no handler at all: the probe's answer is about the caller's
/// permissions rather than about the work the operation would have done, so an operation this
/// emulator has not written answers the same two sides of IAM's decision — never
/// `DryRunOperation` for a caller whose documents allow the action nowhere.
#[tokio::test]
async fn the_probe_of_an_operation_nobody_has_written_still_answers_iam() {
    let router = emulator();
    let probe = Probe::unlock_snapshot();
    let qualified = format!("ec2:{UNWRITTEN_OPERATION}");

    let case = "a role granted another service's actions only";
    let (refused_key, refused_arn) = session_of(
        &router,
        case,
        "unwritten-refused-role",
        &other_service_grant(&[]),
    )
    .await;
    assert_probe(
        &router,
        case,
        &refused_key,
        &refused_arn,
        &probe,
        Expect::NoAllow,
    )
    .await;

    let case = "a role granted the action the unwritten operation names";
    let (holding_key, holding_arn) = session_of(
        &router,
        case,
        "unwritten-holding-role",
        &other_service_grant(&[qualified.as_str()]),
    )
    .await;
    assert_probe(
        &router,
        case,
        &holding_key,
        &holding_arn,
        &probe,
        Expect::HoldsTheAction,
    )
    .await;
}

// ---------------------------------------------------------------------------
// Neither answer performs the request
// ---------------------------------------------------------------------------

/// RULE: a probe checks the permissions "without actually making the request", so neither
/// side of the decision creates anything — while the same request WITHOUT the flag, made by
/// the principal the probe reported as holding the action, does create the interface and
/// `DescribeNetworkInterfaces` reports it afterwards.
#[tokio::test]
async fn neither_side_of_the_probe_creates_an_interface_but_the_request_does() {
    let router = emulator();
    let subnet_id = a_subnet(&router).await;
    let probe = Probe::create_interface(&subnet_id);
    let qualified = format!("ec2:{CREATE_INTERFACE}");
    let case = "the interfaces a probe leaves behind";
    let before = held_interfaces(&router, case).await;

    let refused_case = "a role granted another service's actions only";
    let (refused_key, refused_arn) = session_of(
        &router,
        refused_case,
        "probe-refused-role",
        &other_service_grant(&[]),
    )
    .await;
    assert_probe(
        &router,
        refused_case,
        &refused_key,
        &refused_arn,
        &probe,
        Expect::NoAllow,
    )
    .await;

    let holding_case = "a role granted the action asked for";
    let (holding_key, holding_arn) = session_of(
        &router,
        holding_case,
        "probe-holding-role",
        &other_service_grant(&[qualified.as_str()]),
    )
    .await;
    assert_probe(
        &router,
        holding_case,
        &holding_key,
        &holding_arn,
        &probe,
        Expect::HoldsTheAction,
    )
    .await;

    let after_probes = held_interfaces(&router, case).await;
    assert_eq!(
        after_probes, before,
        "case {case:?}: no side of a probe may create an interface: the account held \
         {before:?} and now holds {after_probes:?}"
    );

    // The same request without the flag, by the principal the probe reported as holding the
    // action: the create the probe pre-flighted is the one that goes through.
    let (status, created) = ec2_as(&router, &holding_key, CREATE_INTERFACE, &probe.members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the create the probe reported as authorized must be served, got \
         {status} {created}"
    );
    let interface_id = element(case, &created, "networkInterfaceId");
    let after_create = held_interfaces(&router, case).await;
    assert!(
        after_create.contains(&interface_id) && after_create.len() == before.len() + 1,
        "case {case:?}: the create must add exactly the interface it reported, \
         {interface_id}: the account held {before:?} and now holds {after_create:?}"
    );
}
