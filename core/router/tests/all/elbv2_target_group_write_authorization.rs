// SPDX-License-Identifier: CC0-1.0

//! Authorization of the Elastic Load Balancing v2 calls addressed to ONE target group:
//! three services, one decision.
//!
//! RULE: a call that writes a target group — the registrations of its targets, the group's
//! own settings, its deletion — is served only when the IAM policies in force on the
//! principal the request signed as allow `elasticloadbalancing:<OperationName>` on THAT
//! target group's ARN. The caller is the identity the access key it signed with names (the
//! STS session registry, the same source `sts:GetCallerIdentity` answers from), and its
//! permissions are the documents the account holds for that identity — for an assumed-role
//! session, the role's inline and attached policies. IAM's order decides: an explicit `Deny`
//! refuses whatever any grant says; otherwise the call needs an `Allow` about it; with
//! neither it falls to IAM's implicit deny. The refusal is HTTP 403 with the error code
//! `AccessDenied` and IAM's own sentence, built from the request — the principal, the action
//! and the target group it addressed — and NOTHING the refused call asked for is written.
//!
//! The account's own published credential is the account's administrative identity: the IAM
//! store holds no policies for it and the account owns its target groups, so it keeps
//! registering and writing throughout, which is what every other suite of this emulator
//! signs as.
//!
//! Three services have to agree for that to hold (IAM holds the role and its policies, STS
//! mints the session credential and names its principal, Elastic Load Balancing v2 takes the
//! decision), so this is the suite that can pin it; a crate-local test can only ever reach
//! one of them.
//!
//! The rows span what the rule generalises over: what the role's own document says (nothing
//! at all, another service's action, another action of this service, the action on another
//! target group of the account, the action on this one, a wildcard covering both, an explicit
//! deny over a wildcard allow), and — in the second test — WHICH of the target-group calls is
//! being decided, since the action asked about is each operation's own name.

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode, opt_element};

/// The Region every request below is signed for.
///
/// This constant and the request plumbing below are `pub(crate)` because the sibling suite
/// `elbv2_denied_registration_and_unused_target_health.rs` drives the same three services over
/// the same wire — IAM holds the role, STS mints the session, Elastic Load Balancing v2 takes
/// the decision — so there is ONE copy of it and the two suites cannot drift into signing or
/// addressing differently.
pub(crate) const REGION: &str = "us-east-1";

/// The API versions the awsQuery requests of each service carry.
const ELB_VERSION: &str = "2015-12-01";
const IAM_VERSION: &str = "2010-05-08";
const STS_VERSION: &str = "2011-06-15";

/// The session name every `AssumeRole` below asks for, which is the last segment of the
/// assumed-role ARN a refusal names.
const SESSION: &str = "probe";

/// The IAM action prefix of this service: an action is `elasticloadbalancing:<OperationName>`.
pub(crate) const ACTION_PREFIX: &str = "elasticloadbalancing";

/// The access key id of the account's own published credential, which is what every other
/// suite signs with: a key neither STS nor IAM holds a record for resolves to the account's
/// own identity.
pub(crate) const ACCOUNT_KEY: &str = "AKIDEXAMPLE";

/// A trust policy letting the account root assume a role. It carries none of the characters
/// that are significant in an `application/x-www-form-urlencoded` body.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"AWS":"arn:aws:iam::000000000000:root"},"Action":"sts:AssumeRole"}]}"#
);

/// The VPC every target group here is created in. Nothing of this suite depends on the
/// account's network: the authorization decision is taken before any target is resolved, and
/// an `ip` registration whose VPC Amazon EC2 holds nothing about resolves no zone and is
/// stored as the caller sent it.
const VPC: &str = "vpc-targetgroup-authz";

/// The whole emulator over one engine, exactly as the binary assembles it.
pub(crate) fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's: what decides a
    // call here is the IDENTITY the access key id names.
    router.set_request_authentication(false);
    router
}

/// One form body: the action, the version and the parameters.
pub(crate) fn form(action: &str, version: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// Send one form-encoded request to the service `signing_name` addresses, signed with
/// `access_key_id` — the key the service reads the caller's identity from — and answer its
/// status and body.
pub(crate) async fn send(
    router: &MultiServiceRouter,
    access_key_id: &str,
    signing_name: &str,
    body: String,
) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One Elastic Load Balancing v2 call, signed with `access_key_id`.
pub(crate) async fn elbv2(
    router: &MultiServiceRouter,
    access_key_id: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    send(
        router,
        access_key_id,
        "elasticloadbalancingv2",
        form(action, ELB_VERSION, params),
    )
    .await
}

/// One Elastic Load Balancing v2 call made as the account itself, which must succeed.
pub(crate) async fn as_account(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = elbv2(router, ACCOUNT_KEY, action, params).await;
    assert_eq!(
        status, 200,
        "setup: ELBv2 {action} as the account must succeed, got {status} with body {body}"
    );
    body
}

/// One IAM call that must succeed.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(router, ACCOUNT_KEY, "iam", form(action, IAM_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "setup: IAM {action} must succeed, got {status} with body {body}"
    );
    body
}

/// The principal a session speaks for, and the key it signs with.
pub(crate) struct Session {
    /// The access key id STS minted for the session.
    pub(crate) key: String,
    /// The assumed-role ARN STS reports for it, which is what a refusal names.
    pub(crate) principal: String,
}

/// A role carrying `document` as its inline policy (none when `document` is `None`), assumed
/// for a session: what the account holds for the principal, written through IAM's own
/// `CreateRole`/`PutRolePolicy` and minted through STS's own `AssumeRole`.
pub(crate) async fn session_of(
    router: &MultiServiceRouter,
    role_name: &str,
    document: Option<&str>,
) -> Session {
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", TRUST_POLICY),
        ],
    )
    .await;
    let role_arn = element(&created, "Arn");
    if let Some(document) = document {
        iam(
            router,
            "PutRolePolicy",
            &[
                ("RoleName", role_name),
                ("PolicyName", "the-one-document"),
                ("PolicyDocument", document),
            ],
        )
        .await;
    }
    let (status, assumed) = send(
        router,
        ACCOUNT_KEY,
        "sts",
        form(
            "AssumeRole",
            STS_VERSION,
            &[("RoleArn", &role_arn), ("RoleSessionName", SESSION)],
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: AssumeRole({role_name}) must succeed, got {status} with body {assumed}"
    );
    Session {
        key: element(&assumed, "AccessKeyId"),
        principal: element(&assumed, "Arn"),
    }
}

/// One identity-based statement, as a document carrying nothing a form body treats specially.
fn policy(effect: &str, action: &str, resource: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"{effect}","Action":"{action}","Resource":"{resource}"}}]}}"#
    )
}

/// An `ip` target group of this account, created through this service's own
/// `CreateTargetGroup`; answers the ARN it minted.
async fn given_target_group(router: &MultiServiceRouter, name: &str) -> String {
    let created = as_account(
        router,
        "CreateTargetGroup",
        &[
            ("Name", name),
            ("Protocol", "HTTP"),
            ("Port", "80"),
            ("VpcId", VPC),
            ("TargetType", "ip"),
        ],
    )
    .await;
    element(&created, "TargetGroupArn")
}

/// Every target id the group at `arn` holds a registration for, read back where a client
/// reads it — through `DescribeTargetHealth`, as the account.
async fn registered_targets(router: &MultiServiceRouter, arn: &str) -> Vec<String> {
    let reported = as_account(router, "DescribeTargetHealth", &[("TargetGroupArn", arn)]).await;
    elements(&reported, "Id")
}

/// The error code an answer carries, or `"<none>"` for one that carries no `<Code>`.
pub(crate) fn error_code(body: &str) -> String {
    opt_element(body, "Code").unwrap_or_else(|| "<none>".to_string())
}

/// IAM's own refusal sentence for `action` on `resource`, closed by `reason`.
pub(crate) fn refusal(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}"
    )
}

/// The clause IAM closes a refusal with when no readable document allows the action.
fn no_policy_allows(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// What a row of the registration table expects.
struct Row {
    case: &'static str,
    role: &'static str,
    /// The inline document in force on the role, or `None` for a role the account grants
    /// nothing.
    document: Option<String>,
    /// The address the row registers, one per row so a row's outcome is read off the group
    /// without undoing another row's.
    address: &'static str,
    /// `None` where the registration is permitted; otherwise the clause the refusal closes
    /// with.
    refused_because: Option<String>,
}

#[tokio::test]
async fn a_target_is_registered_only_by_a_principal_the_policies_in_force_allow() {
    let router = router();
    let group = given_target_group(&router, "authz-probe").await;
    // Another target group OF THE SAME ACCOUNT, so the row scoped to it names an ARN this
    // API really minted rather than one the suite wrote.
    let elsewhere = given_target_group(&router, "authz-elsewhere").await;
    let action = format!("{ACTION_PREFIX}:RegisterTargets");

    let rows = [
        Row {
            case: "a role the account grants nothing",
            role: "elb-no-document",
            document: None,
            address: "10.0.1.11",
            refused_because: Some(no_policy_allows(&action)),
        },
        Row {
            case: "another service's action",
            role: "elb-ecs-only",
            document: Some(policy("Allow", "ecs:CreateService", "*")),
            address: "10.0.1.12",
            refused_because: Some(no_policy_allows(&action)),
        },
        Row {
            case: "another action of this service",
            role: "elb-describe-only",
            document: Some(policy(
                "Allow",
                "elasticloadbalancing:DescribeTargetGroups",
                "*",
            )),
            address: "10.0.1.13",
            refused_because: Some(no_policy_allows(&action)),
        },
        Row {
            case: "the action on another target group of the account",
            role: "elb-other-group",
            document: Some(policy(
                "Allow",
                "elasticloadbalancing:RegisterTargets",
                &elsewhere,
            )),
            address: "10.0.1.14",
            refused_because: Some(no_policy_allows(&action)),
        },
        Row {
            case: "the action on this target group",
            role: "elb-exact-grant",
            document: Some(policy(
                "Allow",
                "elasticloadbalancing:RegisterTargets",
                &group,
            )),
            address: "10.0.1.15",
            refused_because: None,
        },
        Row {
            case: "a wildcard covering the action and the resource",
            role: "elb-wildcard",
            document: Some(policy("Allow", "elasticloadbalancing:*", "*")),
            address: "10.0.1.16",
            refused_because: None,
        },
        Row {
            case: "an explicit deny over a wildcard allow",
            role: "elb-explicit-deny",
            document: Some(
                concat!(
                    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
                    r#""Action":"elasticloadbalancing:*","Resource":"*"},"#,
                    r#"{"Effect":"Deny","Action":"elasticloadbalancing:RegisterTargets","#,
                    r#""Resource":"*"}]}"#
                )
                .to_string(),
            ),
            address: "10.0.1.17",
            refused_because: Some("with an explicit deny in an identity-based policy".to_string()),
        },
    ];

    for row in rows {
        let Row {
            case,
            role,
            document,
            address,
            refused_because,
        } = row;
        let session = session_of(&router, role, document.as_deref()).await;
        let (status, body) = elbv2(
            &router,
            &session.key,
            "RegisterTargets",
            &[
                ("TargetGroupArn", &group),
                ("Targets.member.1.Id", address),
            ],
        )
        .await;
        let held = registered_targets(&router, &group).await;
        match &refused_because {
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the registration to be served, got {status} with \
                     body {body}"
                );
                assert!(
                    held.iter().any(|id| id == address),
                    "case {case:?}: expected {address:?} to be registered, got {held:?}"
                );
            }
            Some(reason) => {
                assert_eq!(
                    status, 403,
                    "case {case:?}: expected HTTP 403, got {status} with body {body}"
                );
                let code = error_code(&body);
                assert_eq!(
                    code, "AccessDenied",
                    "case {case:?}: expected the error code AccessDenied, got {code:?} in {body}"
                );
                let stated = element(&body, "Message");
                let expected = refusal(&session.principal, &action, &group, reason);
                assert_eq!(
                    stated, expected,
                    "case {case:?}: expected IAM's sentence {expected:?}, got {stated:?}"
                );
                assert!(
                    !held.iter().any(|id| id == address),
                    "case {case:?}: a refused registration must store nothing, got {held:?}"
                );
            }
        }
    }
}

/// What a row of the per-operation table drives.
struct OperationRow {
    case: &'static str,
    operation: &'static str,
    /// The parameters the call carries beside its `TargetGroupArn`.
    params: Vec<(&'static str, String)>,
}

#[tokio::test]
async fn every_target_group_write_is_decided_against_the_group_the_request_names() {
    let router = router();
    // One role granting a DIFFERENT action of this service on every resource, so each row's
    // refusal is about the action the row's own call names rather than about the group.
    let other_action = session_of(
        &router,
        "elb-register-only",
        Some(&policy(
            "Allow",
            "elasticloadbalancing:RegisterTargets",
            "*",
        )),
    )
    .await;

    // Every call this service serves that is addressed to ONE target group by its ARN and
    // WRITES it, each with the members its own model requires
    // (`smithy_query(query="operation",operation="<name>")`).
    let rows = [
        OperationRow {
            case: "deregistering a target",
            operation: "DeregisterTargets",
            params: vec![("Targets.member.1.Id", "10.0.2.10".to_string())],
        },
        OperationRow {
            case: "modifying the group's health check",
            operation: "ModifyTargetGroup",
            params: vec![("HealthCheckIntervalSeconds", "10".to_string())],
        },
        OperationRow {
            case: "modifying the group's attributes",
            operation: "ModifyTargetGroupAttributes",
            params: vec![
                (
                    "Attributes.member.1.Key",
                    "deregistration_delay.timeout_seconds".to_string(),
                ),
                ("Attributes.member.1.Value", "30".to_string()),
            ],
        },
        OperationRow {
            case: "deleting the group",
            operation: "DeleteTargetGroup",
            params: Vec::new(),
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let OperationRow {
            case,
            operation,
            params,
        } = row;
        let group = given_target_group(&router, &format!("authz-write-{index}")).await;
        // The target the deregistration row removes has to be there to be removed.
        as_account(
            &router,
            "RegisterTargets",
            &[
                ("TargetGroupArn", &group),
                ("Targets.member.1.Id", "10.0.2.10"),
            ],
        )
        .await;
        let action = format!("{ACTION_PREFIX}:{operation}");
        let mut call: Vec<(&str, &str)> = vec![("TargetGroupArn", group.as_str())];
        for (name, value) in &params {
            call.push((name, value.as_str()));
        }

        // A principal granted another action of this service is refused this one, and the
        // group is still there afterwards.
        let (status, body) = elbv2(&router, &other_action.key, operation, &call).await;
        assert_eq!(
            status, 403,
            "case {case:?}: expected HTTP 403 for a principal granted another action, got \
             {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, "AccessDenied",
            "case {case:?}: expected the error code AccessDenied, got {code:?} in {body}"
        );
        let stated = element(&body, "Message");
        let expected = refusal(
            &other_action.principal,
            &action,
            &group,
            &no_policy_allows(&action),
        );
        assert_eq!(
            stated, expected,
            "case {case:?}: expected IAM's sentence {expected:?}, got {stated:?}"
        );
        let (status, described) = elbv2(
            &router,
            ACCOUNT_KEY,
            "DescribeTargetGroups",
            &[("TargetGroupArns.member.1", &group)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a refused write must leave the group exactly as it was, got \
             {status} with body {described}"
        );
        assert!(
            described.contains(group.as_str()),
            "case {case:?}: a refused write must leave the group exactly as it was, got \
             {described}"
        );

        // And a principal granted THIS action on THIS group is served.
        let granted = session_of(
            &router,
            &format!("elb-granted-{index}"),
            Some(&policy("Allow", &action, &group)),
        )
        .await;
        let (status, body) = elbv2(&router, &granted.key, operation, &call).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the granted call to be served, got {status} with body \
             {body}"
        );
    }
}
