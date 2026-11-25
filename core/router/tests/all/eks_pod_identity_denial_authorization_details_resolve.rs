// SPDX-License-Identifier: CC0-1.0

//! What a POD is handed when the identity service refuses it: the refusal's pointer at the
//! authorization details of that decision, and the record the pointer resolves to.
//!
//! RULE: an `AccessDenied` the identity service decides by evaluating the policies in force
//! closes with the pointer at that decision's own authorization details — `. Go to <the
//! console address of those details> for complete details, or call the
//! GetRequestAuthorizationDetails API with the following authorization id: <id>`, naming an
//! id minted for that ONE refused request and given twice — and that id resolves, through
//! the operation the pointer names, to the record of this very decision: the action and the
//! resource it was made on, the effect that decided it, and one entry per document in force
//! (the session's role's own, plus one per session policy the credential's mint scoped it
//! with). The provenance of the credential that signed the refused request selects none of
//! that: a session another service's credential-vending operation minted is pointed at its
//! details exactly as an IAM user's own long-term key is, because "the temporary security
//! credentials … have the permissions of the role" (`sts:AssumeRole`) is what makes the
//! role's documents the ones the decision reads.
//!
//! This is the DIAGNOSIS path a refused pod walks: EKS Pod Identity hands a pod a session of
//! the association's role (`AssumeRoleForPodIdentityResponse$credentials`), the identity
//! service refuses what nothing in force allows, and the operator reads the id out of that
//! refusal to ask what was evaluated. Four services make it — EKS publishes the cluster's
//! Kubernetes API and mints the pod's projected token, EKS Auth exchanges it, IAM decides the
//! later call, and IAM Toolbox owns the authorization details (`GET
//! /authorization-details/{authorizationId}`) — so only an assembled emulator can state it,
//! which is why this suite lives here.
//!
//! The rows span what the rule generalises over: which document decided (nothing in force,
//! a grant for another action, a statement saying `Deny`, a credential scope that withheld
//! the call), which action is asked for, the resource form the decision is made on, and
//! whether the call is refused at all.
//!
//! The pod-side harness — the kubeconfig read out of `DescribeCluster`, the client that
//! dials the published endpoint under the published certificate authority, the pod-bound
//! token and the exchange — is the sibling suite's
//! (`eks_pod_identity_credentials_are_scoped_to_the_role.rs`, which pins what that
//! credential may DO); the pointer reader is `iam_authorization_details_pointer.rs`, which
//! owns the clause's text. Nothing here is written twice.

use std::collections::BTreeSet;
use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use smithy_server_runtime::MultiServiceRouter;

use crate::eks_pod_identity_credentials_are_scoped_to_the_role::{
    create_role, exchange, kubernetes_client, pod_bound_token, provisioned_kubeconfig,
    served_origin, NAMESPACE,
};
use crate::eks_wire::{cluster_body, ec2, eks, iam_as, role_arn, send};
use crate::iam_authorization_details_pointer::assert_identity_refusal;
use crate::query_wire::{element, elements, opt_element};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The `EvaluatedEffect` the record of a call nothing in force allowed carries:
/// "IMPLICIT_DENY - The action was denied because no policy allowed it".
const IMPLICIT_DENY: &str = "IMPLICIT_DENY";

/// The `EvaluatedEffect` the record of a call a statement about it denied carries.
const EXPLICIT_DENY: &str = "EXPLICIT_DENY";

/// The `PolicyType` of a document attached to the identity the session speaks for.
const IDENTITY_BASED_POLICY: &str = "IDENTITY_BASED_POLICY";

/// The `PolicyType` of a document the credential's own mint scoped it with.
const SESSION_POLICY: &str = "SESSION_POLICY";

/// The clause the refusal of a call a statement in the identity's own documents denies
/// carries.
const EXPLICIT_DENY_CLAUSE: &str = "with an explicit deny in an identity-based policy";

/// One identity-based document with a single statement of `effect` about `action`, on every
/// resource.
fn statement(effect: &str, action: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"{effect}\",\
         \"Action\":\"{action}\",\"Resource\":\"*\"}}]}}"
    )
}

/// A grant of every action of the identity service with an explicit `Deny` of `action`: the
/// document a row about the explicit deny is refused under.
fn allow_everything_but(action: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[\
         {{\"Effect\":\"Allow\",\"Action\":\"iam:*\",\"Resource\":\"*\"}},\
         {{\"Effect\":\"Deny\",\"Action\":\"{action}\",\"Resource\":\"*\"}}]}}"
    )
}

/// The clause the refusal of a call no document in force allows carries.
fn no_identity_policy(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// The clause the refusal of a call the credential's own scope withheld carries.
fn no_session_policy(action: &str) -> String {
    format!("because no session policy allows the {action} action")
}

/// What a refused row expects of the refusal: the clause closing its sentence, and the
/// effect the record of the decision reports.
struct Refused {
    reason: String,
    effect: &'static str,
}

/// One row: what the association's role and the association itself say about the call, the
/// call the pod then makes, and what it earns.
struct Row {
    /// What this row is about, quoted by every assertion it makes.
    case: &'static str,
    /// The document embedded in the association's role, when it carries one.
    role_policy: Option<String>,
    /// The association's own session policy, when it carries one.
    session_policy: Option<String>,
    /// The awsQuery `Action` the pod calls with the vended credential.
    action: &'static str,
    /// The action that call is decided on, as a policy statement spells it.
    iam_action: &'static str,
    /// The resource the decision is made on, which is also the one the refusal names.
    resource: String,
    /// What the refusal must carry, and `None` for a row the call is served for.
    refused: Option<Refused>,
}

/// The rows, in the order they are driven.
fn rows(account: &str) -> Vec<Row> {
    let roles = format!("arn:aws:iam::{account}:role/");
    let users = format!("arn:aws:iam::{account}:user/");
    vec![
        // Nothing in force at all: a collection read, decided on the collection's ARN.
        Row {
            case: "a role carrying no document at all",
            role_policy: None,
            session_policy: None,
            action: "ListRoles",
            iam_action: "iam:ListRoles",
            resource: roles.clone(),
            refused: Some(Refused {
                reason: no_identity_policy("iam:ListRoles"),
                effect: IMPLICIT_DENY,
            }),
        },
        // A document IS in force and does not cover the call: it is the one the record must
        // report as evaluated.
        Row {
            case: "a role whose document allows another action",
            role_policy: Some(statement("Allow", "iam:ListUsers")),
            session_policy: None,
            action: "ListRoles",
            iam_action: "iam:ListRoles",
            resource: roles.clone(),
            refused: Some(Refused {
                reason: no_identity_policy("iam:ListRoles"),
                effect: IMPLICIT_DENY,
            }),
        },
        // The other decision: a statement about the request says `Deny` over a grant for
        // everything, which the record reports as the explicit deny it is.
        Row {
            case: "a role whose document denies the action beside a grant for everything",
            role_policy: Some(allow_everything_but("iam:ListRoles")),
            session_policy: None,
            action: "ListRoles",
            iam_action: "iam:ListRoles",
            resource: roles.clone(),
            refused: Some(Refused {
                reason: EXPLICIT_DENY_CLAUSE.to_string(),
                effect: EXPLICIT_DENY,
            }),
        },
        // The credential's own SCOPE withheld the call the role allows: the record carries
        // the role's document and the session policy the mint scoped the credential with.
        Row {
            case: "an association whose session policy allows another action only",
            role_policy: Some(statement("Allow", "iam:ListRoles")),
            session_policy: Some(statement("Allow", "iam:ListUsers")),
            action: "ListRoles",
            iam_action: "iam:ListRoles",
            resource: roles.clone(),
            refused: Some(Refused {
                reason: no_session_policy("iam:ListRoles"),
                effect: IMPLICIT_DENY,
            }),
        },
        // Another action, on another resource form: nothing about the clause or the pointer
        // is particular to the action the rows above ask for.
        Row {
            case: "another action nothing in force allows",
            role_policy: None,
            session_policy: None,
            action: "ListUsers",
            iam_action: "iam:ListUsers",
            resource: users,
            refused: Some(Refused {
                reason: no_identity_policy("iam:ListUsers"),
                effect: IMPLICIT_DENY,
            }),
        },
        // The control: a call the role's own document allows is served, so the rows above
        // are refusals of what nothing allowed rather than of everything.
        Row {
            case: "a call the role's document allows",
            role_policy: Some(statement("Allow", "iam:ListRoles")),
            session_policy: None,
            action: "ListRoles",
            iam_action: "iam:ListRoles",
            resource: roles,
            refused: None,
        },
    ]
}

/// `GET /authorization-details/{id}`: the read IAM Toolbox serves, which is the operation
/// the refusal's pointer names.
fn authorization_details(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    authorization_id: &str,
) -> crate::eks_wire::Answer {
    send(
        router,
        runtime,
        "iam",
        "GET",
        &format!("/authorization-details/{authorization_id}"),
        "application/json",
        String::new(),
    )
}

/// RULE: the identity service's refusal of a request signed with a credential EKS Pod
/// Identity vended points at the authorization details of that decision, under an id of its
/// own, and that id resolves to the record of this decision — the action and resource it was
/// made on, the effect that decided it, and the documents in force on the session.
#[test]
fn a_pod_identity_refusal_points_at_authorization_details_that_resolve() {
    std::thread::Builder::new()
        .name("eks-pod-identity-denial-details".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            // The cluster's Kubernetes API answers on a listener of the emulator's own, so
            // the runtime this suite drives the emulator on has to carry the IO driver that
            // socket is opened on, and its tasks the stack the assembly needs.
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(BOOT_STACK_SIZE)
                .enable_all()
                .build()
                .expect("a runtime with the IO driver");

            // The cluster is created in subnets EC2 resolves, so the account's own default
            // subnets are read from EC2 rather than invented.
            let defaults = ec2(
                &router,
                &runtime,
                "DescribeSubnets",
                &[
                    ("Filter.1.Name", "default-for-az"),
                    ("Filter.1.Value.1", "true"),
                ],
            );
            assert_eq!(
                defaults.status, 200,
                "setup: DescribeSubnets must succeed, got {defaults:?}"
            );
            let subnets = elements(&defaults.text, "subnetId");
            assert!(
                subnets.len() >= 2,
                "setup: the account must have default subnets, got {subnets:?}"
            );
            let subnets = vec![subnets[0].clone(), subnets[1].clone()];

            let cluster = "pod-identity-denial-details";
            let created = eks(
                &router,
                &runtime,
                "POST",
                "/clusters",
                Some(cluster_body(cluster, &subnets)),
            );
            assert_eq!(
                created.status, 200,
                "setup: the cluster must be created, got {created:?}"
            );
            let config = provisioned_kubeconfig(&router, &runtime, cluster);
            let origin = served_origin(&config);
            let client = kubernetes_client(&config);

            let account = ::engine::emulator_account_id();
            let mut minted: Vec<String> = Vec::new();
            for (index, row) in rows(account).into_iter().enumerate() {
                let case = row.case;
                let role_name = format!("pod-identity-details-{index}");
                create_role(
                    &router,
                    &runtime,
                    case,
                    &role_name,
                    row.role_policy.as_deref(),
                );

                // The association binding the service account to that role, written through
                // the control plane exactly as a customer writes it. An association carrying
                // a session policy turns the session tags off, which is what the control
                // plane requires of one ("When policy is specified, disableSessionTags must
                // be set to true").
                let service_account = format!("details-{index}");
                let mut body = serde_json::json!({
                    "namespace": NAMESPACE,
                    "serviceAccount": service_account,
                    "roleArn": role_arn(&role_name),
                });
                if let Some(document) = row.session_policy.as_deref() {
                    body["policy"] = serde_json::json!(document);
                    body["disableSessionTags"] = serde_json::json!(true);
                }
                let association = eks(
                    &router,
                    &runtime,
                    "POST",
                    &format!("/clusters/{cluster}/pod-identity-associations"),
                    Some(body),
                );
                assert_eq!(
                    association.status, 200,
                    "case {case:?}: the association must be created, got {association:?} ({})",
                    association.message()
                );

                // The token a pod of that service account presents, minted over the address
                // the cluster published, and the credential it exchanges that token for.
                let token = pod_bound_token(
                    &client,
                    &origin,
                    case,
                    &service_account,
                    &format!("{service_account}-pod"),
                );
                let (access_key_id, session_arn) =
                    exchange(&router, &runtime, case, cluster, &token);
                let session_name = session_arn
                    .rsplit_once('/')
                    .map(|(_, name)| name.to_string())
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: the exchange must report a session ARN, got {session_arn:?}")
                    });

                // The call the pod then makes with that credential.
                let answered = iam_as(&router, &runtime, &access_key_id, row.action, &[]);
                let Some(refused) = row.refused else {
                    assert_eq!(
                        answered.status, 200,
                        "case {case:?}: a call the role's document allows must be served, got \
                         {answered:?}"
                    );
                    let names = elements(&answered.text, "RoleName");
                    assert!(
                        names.iter().any(|name| name == &role_name),
                        "case {case:?}: expected {role_name} among the listed roles, got {names:?}"
                    );
                    continue;
                };

                assert_eq!(
                    answered.status, 403,
                    "case {case:?}: a call nothing in force allows must be refused 403, got \
                     {answered:?}"
                );
                assert_eq!(
                    opt_element(&answered.text, "Code").as_deref(),
                    Some("AccessDenied"),
                    "case {case:?}: expected the error code AccessDenied, got {}",
                    answered.text
                );
                // The refusal names the decision and closes with the pointer at its own
                // authorization details, whose id it gives twice.
                let sentence = format!(
                    "User: {session_arn} is not authorized to perform: {} on resource: {} {}",
                    row.iam_action, row.resource, refused.reason
                );
                let authorization_id = assert_identity_refusal(
                    case,
                    &element(&answered.text, "Message"),
                    &sentence,
                );
                minted.push(authorization_id.clone());

                // The id resolves, in the service that owns the record, over its own wire.
                let detail = authorization_details(&router, &runtime, &authorization_id);
                assert_eq!(
                    detail.status, 200,
                    "case {case:?}: expected the authorization id {authorization_id:?} the \
                     refusal names to resolve, got {detail:?}"
                );

                // The record is about THIS decision: the action and the resource it was made
                // on, and the effect that decided it.
                let evaluations = detail.body["evaluations"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                assert_eq!(
                    evaluations.len(),
                    1,
                    "case {case:?}: expected the one evaluation this call was decided on, got \
                     {detail:?}"
                );
                let evaluation = &evaluations[0];
                assert_eq!(
                    evaluation["action"].as_str(),
                    Some(row.iam_action),
                    "case {case:?}: expected the evaluation to name the action decided, got \
                     {evaluation:?}"
                );
                assert_eq!(
                    evaluation["resource"].as_str(),
                    Some(row.resource.as_str()),
                    "case {case:?}: expected the evaluation to name the resource decided \
                     against, got {evaluation:?}"
                );
                assert_eq!(
                    evaluation["evaluatedEffect"].as_str(),
                    Some(refused.effect),
                    "case {case:?}: expected the effect {:?}, got {evaluation:?}",
                    refused.effect
                );

                // The context the decision was made in names the SESSION that signed: a pod
                // identity credential is a session of a role, and a session is no user, so
                // what identifies it is the role's id with the session name after it.
                let context = &detail.body["requestContext"];
                assert_eq!(
                    context["aws:PrincipalType"].as_str(),
                    Some("AssumedRole"),
                    "case {case:?}: expected the context to describe a role session, got \
                     {context:?}"
                );
                let user_id = context["aws:userid"].as_str().unwrap_or_default();
                assert!(
                    user_id.ends_with(&format!(":{session_name}")),
                    "case {case:?}: expected the context to name the session {session_name:?}, \
                     got {user_id:?}"
                );

                // The documents that were evaluated: the role's own, and one entry per
                // session policy the credential's mint scoped it with.
                let policies = detail.body["policies"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let expected_policies = usize::from(row.role_policy.is_some())
                    + usize::from(row.session_policy.is_some());
                assert_eq!(
                    policies.len(),
                    expected_policies,
                    "case {case:?}: expected one entry per document in force, got {policies:?}"
                );
                let role = role_arn(&role_name);
                if row.role_policy.is_some() {
                    let embedded = format!("policy://role/{role_name}/");
                    assert!(
                        policies.iter().any(|policy| {
                            policy["type"].as_str() == Some(IDENTITY_BASED_POLICY)
                                && policy["inline"].as_bool() == Some(true)
                                && policy["uri"]
                                    .as_str()
                                    .is_some_and(|uri| uri.starts_with(&embedded))
                                && policy["attachedTo"][0]["arn"].as_str() == Some(role.as_str())
                        }),
                        "case {case:?}: expected the document embedded in {role_name} among the \
                         evaluated policies, got {policies:?}"
                    );
                }
                if row.session_policy.is_some() {
                    assert!(
                        policies.iter().any(|policy| {
                            policy["type"].as_str() == Some(SESSION_POLICY)
                                && policy["attachedTo"][0]["arn"].as_str()
                                    == Some(session_arn.as_str())
                        }),
                        "case {case:?}: expected the session policy scoped to {session_arn} \
                         among the evaluated policies, got {policies:?}"
                    );
                }
            }

            // Each refusal is pointed at a record of its own: the id names the details of
            // ONE refused request.
            let distinct: BTreeSet<&String> = minted.iter().collect();
            assert_eq!(
                distinct.len(),
                minted.len(),
                "expected every refusal to name its own authorization id, got {minted:?}"
            );
        })
        .expect("spawn the eks pod-identity denial-details thread")
        .join()
        .expect("the eks pod-identity denial-details thread");
}
