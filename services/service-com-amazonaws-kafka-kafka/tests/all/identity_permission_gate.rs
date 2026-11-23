// SPDX-License-Identifier: CC0-1.0

//! What GRANTS a request to this service, driven end to end over the same
//! `MultiServiceRouter` the emulator serves from (the shared `kafka_harness`), with
//! the IAM role and the STS session written through those services' own stores — as
//! `iam:CreateRole`, `iam:PutRolePolicy` and `sts:AssumeRole` write them — and the
//! cluster's own policy attached through `PutClusterPolicy` over the wire.
//!
//! RULE under test: a request reaches an operation of this service only when a
//! policy IN FORCE allows the action it asks for — the calling principal's own
//! identity-based policies, or (for the actions an MSK cluster policy governs) the
//! document attached to the cluster the request names — and nothing explicitly
//! denies it. A request no readable policy allows is refused HTTP 403
//! `AccessDeniedException` carrying IAM's default-deny sentence about the principal
//! that signed, the action asked for and the resource the request addressed; the
//! complementary explicit-deny half of that decision is driven by
//! `identity_policy_gate.rs`.
//!
//! The rows differ along what the rule generalises over: which OPERATION is called
//! and which IAM action it asks for, whether the action is one a cluster policy
//! governs, which document is in force (none, one allowing the action, one allowing
//! another action), and — for the attached document — how it names the caller (the
//! session, the role behind it, every principal, the account, another account) and
//! which effect it states.

use serde_json::{json, Value};

use super::kafka_harness::{
    given_assumed_role_session, given_role_inline_policy, pct, poll_until_active, ClusterRequest,
    ConnectionRequest, Emulator, Reply, ACCOUNT, REGION,
};

/// The role the rows below sign as, the session's name, and the access key id that
/// session was minted for.
const ROLE: &str = "kafka-permission-gate-caller";
const SESSION: &str = "permission-gate-session";
const SESSION_KEY_ID: &str = "ASIAPERMISSIONGATE00";

/// The wire code and status an authorization refusal carries, spelled out here
/// rather than read from the service so a drift in its own constants fails this
/// suite.
const ACCESS_DENIED_CODE: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The ARN the account holds the signing role under — the identity whose policies
/// an assumed-role session's request is judged on, and one of the two spellings a
/// resource-based policy may name that session by.
fn role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{ROLE}")
}

/// The sentence IAM refuses a request no readable policy ALLOWS with: fixed text
/// around the principal that signed, the action it asked for and the resource it
/// addressed, closing by naming the action nothing granted.
fn no_grant_message(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// The sentence IAM refuses a request the RESOURCE's own policy explicitly denies
/// with — the clause names that document, so a client learns that fixing its own
/// permissions will not help.
fn resource_deny_message(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         with an explicit deny in a resource-based policy"
    )
}

/// Assert `reply` is the authorization refusal carrying `message` as its whole
/// sentence, with the code and status the real service answers.
fn assert_refused(reply: &Reply, case: &str, message: &str) {
    assert_eq!(
        (reply.status, reply.error_code(), reply.error_message()),
        (ACCESS_DENIED_STATUS, ACCESS_DENIED_CODE, message),
        "case {case}: expected the authorization refusal, got {} {}",
        reply.status,
        reply.body
    );
}

/// Assert `reply` is NOT an authorization refusal: the operation answered for
/// itself, whatever it answered.
fn assert_not_refused(reply: &Reply, case: &str) {
    assert_ne!(
        reply.error_code(),
        ACCESS_DENIED_CODE,
        "case {case}: a policy in force allows this action, so the gate must not refuse it, \
         got {} {}",
        reply.status,
        reply.body
    );
}

/// One call this suite makes: the operation that answers it, the IAM action that
/// operation asks for, and how it is addressed.
struct Call {
    operation: &'static str,
    action: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    /// The status the operation answers once a policy grants the call, where the
    /// call is one that then succeeds; `None` for a call whose own answer depends
    /// on state this suite does not set up (a policy that is not attached, say).
    served_status: Option<u16>,
}

/// Make `call` signed with `access_key_id`.
async fn make(emulator: &Emulator, call: &Call, access_key_id: &str) -> Reply {
    emulator
        .call_as_in(
            access_key_id,
            REGION,
            call.method,
            &call.uri,
            call.body.clone(),
        )
        .await
}

/// The cluster-scoped calls whose action an MSK cluster policy does NOT govern, so
/// the caller's own identity-based policies are the only thing that can grant them:
/// a secret listing (the operation this suite's issue was filed about), two other
/// readers, a reader of the cluster's own policy and a mutation.
fn calls_granted_only_by_identity(cluster_arn: &str) -> Vec<Call> {
    let cluster = pct(cluster_arn);
    vec![
        Call {
            operation: "ListScramSecrets",
            action: "kafka:ListScramSecrets",
            method: "GET",
            uri: format!("/v1/clusters/{cluster}/scram-secrets"),
            body: None,
            served_status: Some(200),
        },
        Call {
            operation: "ListNodes",
            action: "kafka:ListNodes",
            method: "GET",
            uri: format!("/v1/clusters/{cluster}/nodes"),
            body: None,
            served_status: Some(200),
        },
        Call {
            operation: "ListClusterOperationsV2",
            action: "kafka:ListClusterOperationsV2",
            method: "GET",
            uri: format!("/api/v2/clusters/{cluster}/operations"),
            body: None,
            served_status: Some(200),
        },
        Call {
            operation: "GetClusterPolicy",
            action: "kafka:GetClusterPolicy",
            method: "GET",
            uri: format!("/v1/clusters/{cluster}/policy"),
            body: None,
            // The cluster carries no policy at this point, which is this
            // operation's own 404 rather than anything the gate decides.
            served_status: None,
        },
        Call {
            operation: "UpdateMonitoring",
            action: "kafka:UpdateMonitoring",
            method: "PUT",
            uri: format!("/v1/clusters/{cluster}/monitoring"),
            body: Some(json!({
                "currentVersion": "KTVPDKIKX0DER",
                "enhancedMonitoring": "PER_BROKER",
            })),
            // The version in the body is not this cluster's, which is the
            // operation's own refusal — the gate's business is only that it is
            // reached at all.
            served_status: None,
        },
    ]
}

/// The calls whose action an MSK cluster policy GOVERNS, which the document
/// attached to the cluster can therefore grant on its own.
fn calls_a_cluster_policy_governs(cluster_arn: &str) -> Vec<Call> {
    let cluster = pct(cluster_arn);
    vec![
        Call {
            operation: "GetBootstrapBrokers",
            action: "kafka:GetBootstrapBrokers",
            method: "GET",
            uri: format!("/v1/clusters/{cluster}/bootstrap-brokers"),
            body: None,
            served_status: Some(200),
        },
        Call {
            operation: "DescribeCluster",
            action: "kafka:DescribeCluster",
            method: "GET",
            uri: format!("/v1/clusters/{cluster}"),
            body: None,
            served_status: Some(200),
        },
        Call {
            operation: "DescribeClusterV2",
            action: "kafka:DescribeClusterV2",
            method: "GET",
            uri: format!("/api/v2/clusters/{cluster}"),
            body: None,
            served_status: Some(200),
        },
        Call {
            operation: "CreateVpcConnection",
            action: "kafka:CreateVpcConnection",
            method: "POST",
            uri: "/v1/vpc-connection".to_string(),
            // The minimal valid request the shared harness builds for this
            // cluster, so a row that is served is served for the authorization
            // decision rather than refused by a precondition of its own.
            body: Some(ConnectionRequest::to(cluster_arn).to_json()),
            served_status: Some(200),
        },
    ]
}

/// A cluster this endpoint holds, settled `ACTIVE` so every reader below is past
/// the cluster-state preconditions and only the authorization decision is left to
/// answer. Built as a VPC-connection target so the create of that surface is
/// servable too.
async fn active_cluster(emulator: &Emulator, name: &str) -> String {
    let arn = emulator
        .create_ok(&ClusterRequest::vpc_connection_target(name))
        .await;
    poll_until_active(emulator, &arn, name).await;
    arn
}

/// An identity-based document allowing `action` on every resource, in the shape
/// `iam:PutRolePolicy` takes one.
fn identity_allow(action: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": "*" }],
    })
}

/// A cluster policy stating `effect` for `principal` on `action`, scoped to the
/// cluster it is attached to, in the shape `PutClusterPolicy` takes one.
fn cluster_policy(effect: &str, principal: &Value, action: &str, cluster_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": { "AWS": principal },
            "Action": [action],
            "Resource": cluster_arn,
        }],
    })
    .to_string()
}

/// Attach `policy` to the cluster, as the account's own credential — which is the
/// caller that administers the cluster's policy — and assert it was stored.
///
/// A policy the cluster ALREADY carries is replaced under the service's own
/// optimistic lock: the version the stored policy is at is read back and quoted,
/// since `PutClusterPolicy` refuses a replacement that quotes none (the rule the
/// `cluster_policy_write_rules` suite states). The first attachment quotes
/// nothing, as it has no version to quote.
async fn put_cluster_policy(emulator: &Emulator, case: &str, cluster_arn: &str, policy: &str) {
    let policy_uri = format!("/v1/clusters/{}/policy", pct(cluster_arn));
    let held = emulator.call("GET", &policy_uri, None).await;
    let mut body = json!({ "policy": policy });
    if let Some(version) = held.at("/currentVersion").as_str() {
        body["currentVersion"] = json!(version);
    }
    let reply = emulator.call("PUT", &policy_uri, Some(body)).await;
    assert_eq!(
        reply.status, 200,
        "case {case}: expected PutClusterPolicy to store the document, got {} {}",
        reply.status, reply.body
    );
    // Read it back through the service's own reader, so a row that depends on the
    // attached document depends on one the service really holds.
    let stored = emulator.call("GET", &policy_uri, None).await;
    assert_eq!(
        stored.at("/policy").as_str(),
        Some(policy),
        "case {case}: expected GetClusterPolicy to report the stored document, got {} {}",
        stored.status,
        stored.body
    );
}

/// Take the cluster's policy off again, as `DeleteClusterPolicy` does.
async fn delete_cluster_policy(emulator: &Emulator, case: &str, cluster_arn: &str) {
    let reply = emulator
        .call(
            "DELETE",
            &format!("/v1/clusters/{}/policy", pct(cluster_arn)),
            None,
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "case {case}: expected DeleteClusterPolicy to remove the document, got {} {}",
        reply.status, reply.body
    );
}

/// RULE: a caller whose identity-based policies allow nothing is refused every
/// cluster-scoped call with IAM's default-deny sentence, and granting the action it
/// asks for makes the identical call reach the operation — over a table of
/// operations, and whichever document is in force.
#[tokio::test]
async fn a_caller_no_policy_permits_is_refused_and_a_grant_lifts_the_refusal() {
    let emulator = Emulator::new();
    let principal = given_assumed_role_session(&emulator.engine, ROLE, SESSION, SESSION_KEY_ID);
    let cluster = active_cluster(&emulator, "permission-gate").await;

    for call in calls_granted_only_by_identity(&cluster) {
        // The resource every one of these calls is decided over is the cluster it
        // names, which is what the refusal then states.
        let refused = no_grant_message(&principal, call.action, &cluster);

        // No document at all: IAM's default deny.
        given_role_inline_policy(&emulator.engine, ROLE, None);
        let reply = make(&emulator, &call, SESSION_KEY_ID).await;
        assert_refused(
            &reply,
            &format!("no policy at all/{}", call.operation),
            &refused,
        );

        // A document that allows a DIFFERENT action of this service grants
        // nothing about this one, so the same default deny stands.
        given_role_inline_policy(
            &emulator.engine,
            ROLE,
            Some(&identity_allow("kafka:DeleteCluster")),
        );
        let reply = make(&emulator, &call, SESSION_KEY_ID).await;
        assert_refused(
            &reply,
            &format!("an allow of another action/{}", call.operation),
            &refused,
        );

        // A document that allows exactly this action: the identical call reaches
        // the operation.
        given_role_inline_policy(&emulator.engine, ROLE, Some(&identity_allow(call.action)));
        let reply = make(&emulator, &call, SESSION_KEY_ID).await;
        let case = format!("an allow of exactly this action/{}", call.operation);
        assert_not_refused(&reply, &case);
        if let Some(expected) = call.served_status {
            assert_eq!(
                reply.status, expected,
                "case {case}: expected the operation's own {expected}, got {} {}",
                reply.status, reply.body
            );
        }
    }

    given_role_inline_policy(&emulator.engine, ROLE, None);
}

/// RULE: for the actions an MSK cluster policy governs, the document attached to
/// the cluster is itself a grant: a principal with no identity-based permissions
/// is refused, an `Allow` naming it in that document makes the identical call
/// succeed, and deleting the document refuses it again — while a document that
/// names the caller's ACCOUNT delegates the decision to that account's own
/// identity-based policies and so grants such a principal nothing, and an explicit
/// `Deny` in it refuses the call whatever else allows it.
#[tokio::test]
async fn a_cluster_policy_allow_grants_the_actions_it_governs() {
    let emulator = Emulator::new();
    let principal = given_assumed_role_session(&emulator.engine, ROLE, SESSION, SESSION_KEY_ID);
    // The caller holds no identity-based permission at all, so every grant below
    // comes from the cluster's own document.
    given_role_inline_policy(&emulator.engine, ROLE, None);
    let cluster = active_cluster(&emulator, "cluster-policy-grant").await;

    // How the attached document may name this caller, and whether that naming
    // GRANTS it: its own session, the role the session speaks for, every principal
    // — and the two that grant nothing, its account (which delegates to that
    // account's identity-based policies, which permit this caller nothing) and
    // another account altogether.
    let namings: Vec<(&str, Value, bool)> = vec![
        ("the session that signed", json!(principal.clone()), true),
        ("the role behind the session", json!(role_arn()), true),
        ("every principal", json!("*"), true),
        (
            "the caller's own account root",
            json!(format!("arn:aws:iam::{ACCOUNT}:root")),
            false,
        ),
        (
            "another account's root",
            json!("arn:aws:iam::111122223333:root"),
            false,
        ),
    ];

    for call in calls_a_cluster_policy_governs(&cluster) {
        let refused = no_grant_message(&principal, call.action, &cluster);

        // Before any grant: refused, with the sentence naming the action nothing
        // allowed.
        let reply = make(&emulator, &call, SESSION_KEY_ID).await;
        assert_refused(
            &reply,
            &format!("no policy at all/{}", call.operation),
            &refused,
        );

        for (naming, named, grants) in &namings {
            let case = format!("an allow naming {naming}/{}", call.operation);
            put_cluster_policy(
                &emulator,
                &case,
                &cluster,
                &cluster_policy("Allow", named, call.action, &cluster),
            )
            .await;
            let reply = make(&emulator, &call, SESSION_KEY_ID).await;
            if *grants {
                assert_not_refused(&reply, &case);
                if let Some(expected) = call.served_status {
                    assert_eq!(
                        reply.status, expected,
                        "case {case}: expected the operation's own {expected}, got {} {}",
                        reply.status, reply.body
                    );
                }
            } else {
                assert_refused(&reply, &case, &refused);
            }

            // An explicit DENY naming the same principal refuses the call, and
            // names the document that refused it.
            let denied_case = format!("a deny naming {naming}/{}", call.operation);
            put_cluster_policy(
                &emulator,
                &denied_case,
                &cluster,
                &cluster_policy("Deny", named, call.action, &cluster),
            )
            .await;
            let reply = make(&emulator, &call, SESSION_KEY_ID).await;
            if *grants {
                assert_refused(
                    &reply,
                    &denied_case,
                    &resource_deny_message(&principal, call.action, &cluster),
                );
            } else {
                // A statement that denies an ACCOUNT refuses each of its
                // principals, and one that names another account refuses nothing
                // of this one — so the answer is that document's deny for the
                // caller's own account and the default deny for the other's.
                let expected = if *naming == "the caller's own account root" {
                    resource_deny_message(&principal, call.action, &cluster)
                } else {
                    refused.clone()
                };
                assert_refused(&reply, &denied_case, &expected);
            }

            // The grant is gone with the document: the call is refused again.
            delete_cluster_policy(&emulator, &case, &cluster).await;
            let reply = make(&emulator, &call, SESSION_KEY_ID).await;
            assert_refused(&reply, &format!("after the delete/{}", case), &refused);
        }
    }

    // A cluster policy allowing an action it does NOT govern grants nothing: the
    // document an MSK cluster carries is in force on the published set alone.
    let ungoverned = calls_granted_only_by_identity(&cluster);
    let secrets = ungoverned
        .first()
        .expect("the ungoverned table names at least one call");
    let case = format!("an allow of an ungoverned action/{}", secrets.operation);
    put_cluster_policy(
        &emulator,
        &case,
        &cluster,
        &cluster_policy("Allow", &json!(principal.clone()), secrets.action, &cluster),
    )
    .await;
    let reply = make(&emulator, secrets, SESSION_KEY_ID).await;
    assert_refused(
        &reply,
        &case,
        &no_grant_message(&principal, secrets.action, &cluster),
    );
    delete_cluster_policy(&emulator, &case, &cluster).await;
}

/// RULE: the account's own published credential — the caller the emulator holds no
/// identity-based policy for at all — is not refused for the absence of a grant, on
/// the same calls the zero-permission session above is refused.
#[tokio::test]
async fn the_accounts_own_credential_is_not_refused_for_a_missing_grant() {
    let emulator = Emulator::new();
    let cluster = active_cluster(&emulator, "ambient-caller").await;

    let mut calls = calls_granted_only_by_identity(&cluster);
    calls.extend(calls_a_cluster_policy_governs(&cluster));
    for call in calls {
        let reply = emulator
            .call_in(REGION, call.method, &call.uri, call.body.clone())
            .await;
        assert_not_refused(&reply, &format!("the account's own credential/{}", call.operation));
    }
}
