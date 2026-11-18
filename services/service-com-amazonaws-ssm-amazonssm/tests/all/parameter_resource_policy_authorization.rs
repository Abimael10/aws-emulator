// SPDX-License-Identifier: CC0-1.0

//! WHO MAY READ a parameter that carries a RESOURCE POLICY of its own: the document
//! `PutResourcePolicy` attached to the parameter takes part in the decision beside the
//! calling principal's identity-based policies.
//!
//! RULE under test: **AWS decides a signed call over the two kinds of document in force
//! together — the calling principal's identity-based policies and the addressed resource's own
//! resource-based policy — before the service resolves what the request names: an explicit
//! `Deny` about the request in either refuses it whatever any grant says, an `Allow` about it
//! in either grants it, and with neither the call falls to IAM's default deny, every refusal
//! being `AccessDeniedException` (HTTP 400) naming the principal, the action, the resource and
//! the rule that withheld the grant.** A statement is about the request only when its action,
//! resource and — for a resource-based document — principal elements all cover it, so a
//! document naming another action, another resource or another principal decides nothing.
//!
//! Why it matters beyond the refusal itself: a parameter's policy is the deny boundary a
//! client tests. An emulator that stores such a policy and then serves the parameter in full
//! reports success for a read AWS refuses, so the handling a caller wrote for the refusal — a
//! fallback, a retry, a provider that answers its own caller `FAILED` — is never exercised,
//! and a client cannot tell a scoped credential from an unscoped one.
//!
//! Every case drives the emulator through its own wire: the principals are made through IAM's
//! and STS's own operations, the parameter through `PutParameter`, the policy through
//! `PutResourcePolicy` and `DeleteResourcePolicy`, and each read is signed with the credential
//! of the principal under test. Nothing reaches into the state struct.
//!
//! The rows differ along what the rule generalises over: WHAT the attached document says
//! (nothing attached at all, a deny covering every principal, a deny naming one principal, a
//! deny naming another principal, a deny of another action, a grant naming a principal with no
//! identity policy, a grant naming another principal), WHICH principal reads (the account's
//! own published credential, a session allowed every action of this service, and a session
//! allowed nothing), and WHICH read is called (the single read, the batch read and the history
//! read). The parameter beside the one the policy is attached to is read in every row too, so
//! a document is shown to decide its OWN resource and nothing else.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::just_in_time_node_access::{assumed_role, call_as, emulator, Principal};
use crate::parameter_store::{call, error_code, error_message, ACCOUNT, REGION};

/// The parameter the policy is attached to, and the one beside it that carries none — both
/// in the advanced tier, which is the tier a parameter must be in to carry a resource policy
/// ("to share a parameter, it must be in the advanced parameter tier",
/// `smithy_query(query="doc",shape="PutResourcePolicy")`).
const SHARED: &str = "/probe/shared";
const UNSHARED: &str = "/probe/unshared";

/// A name this account holds NO parameter for, which the gate is asked about all the same.
const ABSENT: &str = "/probe/never-written";

/// The access key id the account's own published credential presents.
const ACCOUNT_KEY: &str = "AKIDTEST";

/// The tier a parameter carrying a resource policy is served in
/// (`smithy_query(query="shape",shape="ParameterTier")`: Standard, Advanced,
/// Intelligent-Tiering).
const TIER_ADVANCED: &str = "Advanced";

/// Another account's root, the principal a row's document names to show a statement decides
/// only the caller it names. Any account id other than this emulator's serves.
const ANOTHER_ACCOUNT_ROOT: &str = "arn:aws:iam::111122223333:root";

/// The ARN of the parameter `name`, as the resource a policy statement names it by:
/// `arn:aws:ssm:<region>:<account>:parameter/Dev/DBServer` — the hierarchical name keeps its
/// whole path after the `parameter/` prefix (`Parameter$ARN`).
fn parameter_arn(name: &str) -> String {
    format!(
        "arn:aws:ssm:{REGION}:{ACCOUNT}:parameter/{}",
        name.strip_prefix('/').unwrap_or(name)
    )
}

/// What the rule says about one read: it is served, or refused with the rule that withheld
/// the grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decided {
    /// A readable document allows it (or there is no document the gate can bound the caller
    /// by at all).
    Served,
    /// No readable document states the action: IAM's default deny, whose sentence names the
    /// action nothing granted.
    NoGrant,
    /// The addressed resource's OWN policy denies it explicitly.
    ResourceDeny,
}

/// The sentence a refusal carries for one principal, action and resource — IAM's own wording,
/// with the clause of the rule that withheld the grant at the end.
fn not_authorized(principal: &str, operation: &str, resource: &str, decided: Decided) -> String {
    let action = format!("ssm:{operation}");
    let reason = match decided {
        Decided::NoGrant => format!("because no identity-based policy allows the {action} action"),
        Decided::ResourceDeny => "with an explicit deny in a resource-based policy".to_string(),
        Decided::Served => String::new(),
    };
    format!("User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}")
}

/// One read under test: the operation and the body that calls it for `name`.
struct Read {
    case: &'static str,
    operation: &'static str,
    body: fn(&str) -> Value,
}

/// The reads a parameter is served through that address ONE parameter, so the resource the
/// decision is taken on is that parameter's own ARN.
fn reads() -> Vec<Read> {
    vec![
        Read {
            case: "the single read",
            operation: "GetParameter",
            body: |name| json!({ "Name": name }),
        },
        Read {
            case: "the batch read",
            operation: "GetParameters",
            body: |name| json!({ "Names": [name] }),
        },
        Read {
            case: "the history read",
            operation: "GetParameterHistory",
            body: |name| json!({ "Name": name }),
        },
    ]
}

/// Create `name` in the advanced tier through Parameter Store's own write, with the account's
/// own credential — the caller the identity half of the gate cannot bound, so the seeding says
/// nothing about the rows below.
async fn seed(router: &MultiServiceRouter, name: &str) {
    let (status, body) = call(
        router,
        "PutParameter",
        &json!({ "Name": name, "Value": "held", "Type": "String", "Tier": TIER_ADVANCED }),
    )
    .await;
    assert_eq!(
        status, 200,
        "seeding {name:?}: expected PutParameter to succeed, got {status} {body}"
    );
}

/// Attach `policy` to the parameter `name` through `PutResourcePolicy`, answering the
/// `(PolicyId, PolicyHash)` pair a detach addresses it by.
async fn attach(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    policy: &str,
) -> (String, String) {
    let (status, body) = call(
        router,
        "PutResourcePolicy",
        &json!({ "ResourceArn": parameter_arn(name), "Policy": policy }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected PutResourcePolicy to attach the document, got {status} {body}"
    );
    let value = |member: &str| {
        body[member]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a put reports {member}, got {body}"))
            .to_string()
    };
    (value("PolicyId"), value("PolicyHash"))
}

/// Detach the policy that pair addresses, so the next row decides against its own document
/// alone.
async fn detach(router: &MultiServiceRouter, case: &str, name: &str, attached: (String, String)) {
    let (policy_id, policy_hash) = attached;
    let (status, body) = call(
        router,
        "DeleteResourcePolicy",
        &json!({
            "ResourceArn": parameter_arn(name),
            "PolicyId": policy_id,
            "PolicyHash": policy_hash,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DeleteResourcePolicy to detach the document, got {status} {body}"
    );
}

/// A resource-policy document: `effect` about `actions` for `principal`, over the parameter
/// `resource` names.
fn resource_policy(effect: &str, principal: &str, actions: &str, resource: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"probe","Effect":"{effect}",
        "Principal":{{"AWS":"{principal}"}},"Action":"{actions}","Resource":"{resource}"}}]}}"#
    )
}

/// The same document naming EVERY principal, which is the whole-element `"*"` the policy
/// grammar admits for `Principal`.
fn resource_policy_for_everyone(effect: &str, actions: &str, resource: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"probe","Effect":"{effect}",
        "Principal":"*","Action":"{actions}","Resource":"{resource}"}}]}}"#
    )
}

#[tokio::test]
async fn a_read_is_decided_over_the_parameters_own_policy_beside_the_callers() {
    let router = emulator();
    seed(&router, SHARED).await;
    seed(&router, UNSHARED).await;

    // The account's own published credential, which holds no identity policy at all. The ARN
    // it speaks under is the identity this emulator publishes that credential as, read from
    // the one place that spelling lives rather than written out again here.
    let account = Principal {
        access_key_id: ACCOUNT_KEY.to_string(),
        arn: service_common::caller_principal::ambient_user_arn(),
    };
    // A session allowed every action of this service, and one allowed nothing.
    let admin = assumed_role(
        &router,
        "parameter-policy-admin",
        Some(r#"{"Version":"2012-10-17","Statement":[
            {"Effect":"Allow","Action":"ssm:*","Resource":"*"}]}"#),
    )
    .await;
    let nobody = assumed_role(&router, "parameter-policy-nopolicy", None).await;
    // The ARN of the ROLE behind a session is the spelling a statement names it by, which is
    // what the rows naming one principal are written with.
    let admin_role = format!("arn:aws:iam::{ACCOUNT}:role/parameter-policy-admin");
    let nobody_role = format!("arn:aws:iam::{ACCOUNT}:role/parameter-policy-nopolicy");

    let shared_arn = parameter_arn(SHARED);
    // (the case, the document attached to SHARED, what each of the three callers is decided)
    let rows: Vec<(&str, Option<String>, [Decided; 3])> = vec![
        (
            "no policy attached at all",
            None,
            [Decided::Served, Decided::Served, Decided::NoGrant],
        ),
        (
            "a deny of the reads covering every principal",
            Some(resource_policy_for_everyone("Deny", "ssm:Get*", "*")),
            [
                Decided::ResourceDeny,
                Decided::ResourceDeny,
                Decided::ResourceDeny,
            ],
        ),
        (
            "a deny naming the allowed session's role",
            Some(resource_policy("Deny", &admin_role, "ssm:*", &shared_arn)),
            [Decided::Served, Decided::ResourceDeny, Decided::NoGrant],
        ),
        (
            "a deny naming another account entirely",
            Some(resource_policy(
                "Deny",
                ANOTHER_ACCOUNT_ROOT,
                "ssm:*",
                &shared_arn,
            )),
            [Decided::Served, Decided::Served, Decided::NoGrant],
        ),
        (
            "a deny of another action",
            Some(resource_policy_for_everyone(
                "Deny",
                "ssm:PutParameter",
                "*",
            )),
            [Decided::Served, Decided::Served, Decided::NoGrant],
        ),
        (
            "a grant naming the session that holds no identity policy",
            Some(resource_policy(
                "Allow",
                &nobody_role,
                "ssm:Get*",
                &shared_arn,
            )),
            [Decided::Served, Decided::Served, Decided::Served],
        ),
        (
            "a grant naming another account entirely",
            Some(resource_policy(
                "Allow",
                ANOTHER_ACCOUNT_ROOT,
                "ssm:Get*",
                &shared_arn,
            )),
            [Decided::Served, Decided::Served, Decided::NoGrant],
        ),
    ];

    for (case, document, decided) in rows {
        let attached = match &document {
            Some(policy) => Some(attach(&router, case, SHARED, policy).await),
            None => None,
        };
        // Beside what each caller is decided on the parameter the document is attached to:
        // what its own identity policies alone decide, which is what the parameter BESIDE it
        // — attached to no document — is decided by in every row.
        let callers = [
            (
                "the account's own published credential",
                &account,
                decided[0],
                Decided::Served,
            ),
            (
                "a session allowed every action of this service",
                &admin,
                decided[1],
                Decided::Served,
            ),
            (
                "a session allowed nothing",
                &nobody,
                decided[2],
                Decided::NoGrant,
            ),
        ];
        for (who, principal, decided, identity_only) in callers {
            for read in reads() {
                // The parameter the document is attached to.
                let case = format!("{case}, {who}, {}", read.case);
                let (status, body) = call_as(
                    &router,
                    &principal.access_key_id,
                    REGION,
                    read.operation,
                    &(read.body)(SHARED),
                )
                .await;
                if decided == Decided::Served {
                    assert_eq!(
                        status, 200,
                        "case {case:?}: expected the read to be served, got {status} {body}"
                    );
                } else {
                    assert_eq!(
                        (status, error_code(&body).as_str()),
                        (400, "AccessDeniedException"),
                        "case {case:?}: expected the read to be refused, got {status} {body}"
                    );
                    let expected =
                        not_authorized(&principal.arn, read.operation, &shared_arn, decided);
                    assert_eq!(
                        error_message(&body),
                        expected,
                        "case {case:?}: expected the refusal to name the rule that withheld \
                         the grant, got {body}"
                    );
                }

                // ...and the parameter BESIDE it, which the document is not attached to: a
                // statement decides its own resource, so this read is decided by the caller's
                // identity policies alone whatever that document says.
                let beside = format!("{case}, the parameter beside it");
                let (status, body) = call_as(
                    &router,
                    &principal.access_key_id,
                    REGION,
                    read.operation,
                    &(read.body)(UNSHARED),
                )
                .await;
                if identity_only == Decided::Served {
                    assert_eq!(
                        status, 200,
                        "case {beside:?}: expected the unshared parameter's read to be served, \
                         got {status} {body}"
                    );
                } else {
                    assert_eq!(
                        (status, error_code(&body).as_str()),
                        (400, "AccessDeniedException"),
                        "case {beside:?}: expected the unshared parameter's read to be refused, \
                         got {status} {body}"
                    );
                    let expected = not_authorized(
                        &principal.arn,
                        read.operation,
                        &parameter_arn(UNSHARED),
                        identity_only,
                    );
                    assert_eq!(
                        error_message(&body),
                        expected,
                        "case {beside:?}: expected the refusal to be the identity-based one, \
                         got {body}"
                    );
                }
            }
        }
        if let Some(attached) = attached {
            detach(&router, case, SHARED, attached).await;
        }
    }
}

#[tokio::test]
async fn the_decision_precedes_the_store_so_a_refusal_says_nothing_about_what_exists() {
    // The decision is taken on the resource the REQUEST addresses, before the store is read:
    // a caller holding no grant is refused the same way for a name this account holds a
    // parameter for and for one it does not, while the caller the gate serves is told the
    // parameter is not there.
    let router = emulator();
    seed(&router, SHARED).await;
    let nobody = assumed_role(&router, "parameter-policy-absent", None).await;

    for read in reads() {
        for name in [SHARED, ABSENT] {
            let case = format!("{}, {name}", read.case);
            let (status, body) = call_as(
                &router,
                &nobody.access_key_id,
                REGION,
                read.operation,
                &(read.body)(name),
            )
            .await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "AccessDeniedException"),
                "case {case:?}: expected the unauthorized read to be refused, got {status} {body}"
            );
            let expected = not_authorized(
                &nobody.arn,
                read.operation,
                &parameter_arn(name),
                Decided::NoGrant,
            );
            assert_eq!(
                error_message(&body),
                expected,
                "case {case:?}: expected the refusal to name the parameter the request \
                 addressed, got {body}"
            );
        }
    }

    // The same name, read by the caller the gate serves: the store is reached, and reports
    // that it holds no such parameter.
    let (status, body) = call(&router, "GetParameter", &json!({ "Name": ABSENT })).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ParameterNotFound"),
        "case \"the authorized read of a name nothing was written under\": expected \
         ParameterNotFound, got {status} {body}"
    );
}
