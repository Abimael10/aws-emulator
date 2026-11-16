// SPDX-License-Identifier: CC0-1.0

//! The decision the CALLER's own identity-based policies take about a request,
//! driven end to end over the same `MultiServiceRouter` the emulator serves from
//! (the shared `kafka_harness`), with the IAM role and the STS session written
//! through those services' own stores — as `iam:CreateRole`, `iam:PutRolePolicy`
//! and `sts:AssumeRole` write them.
//!
//! RULE under test: AWS authorizes at the IAM layer before the service resolves
//! anything the request names, so a request the calling principal's identity-based
//! policies do not permit is refused `AccessDeniedException` — carrying IAM's own
//! sentence about the principal that signed, the action asked for and the resource
//! the request addressed — whatever that request would otherwise have been
//! answered, and without disclosing whether the resource exists. Which sentence is
//! IAM's order: a document that explicitly DENIES the action refuses the call
//! whatever else is in force, and a request no readable document ALLOWS falls to
//! IAM's default deny, whose sentence names the action nothing granted. A request a
//! readable document allows reaches the operation's own answer.
//!
//! The rows differ along what the rule generalises over: which OPERATION is called
//! (a create whose target is in the body, a cluster-scoped mutation whose target is
//! a path segment, a topic data-plane call, a create that names no resource at
//! all), which actions the stored document names (exactly this one, every action at
//! all, another action, another service's), which resource it is scoped to, and
//! whether a `Deny` sits beside an `Allow`.

use serde_json::{json, Value};

use super::kafka_harness::{
    given_assumed_role_session, given_role_inline_policy, pct, ClusterRequest, Emulator, Reply,
    ACCOUNT, DEFAULT_ACCESS_KEY_ID, REGION,
};

/// The role the rows below sign as, the session's name, and the access key id that
/// session was minted for.
const ROLE: &str = "kafka-identity-gate-caller";
const SESSION: &str = "identity-gate-session";
const SESSION_KEY_ID: &str = "ASIAIDENTITYGATE0000";

/// A cluster id of the form this service mints, for an ARN naming a cluster no
/// registry of this emulator holds.
const GHOST_ID: &str = "11111111-2222-4333-8444-555555555555-1";

/// The wire code and status an authorization refusal carries, spelled out here
/// rather than read from the service so a drift in its own constants fails this
/// suite.
const ACCESS_DENIED_CODE: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The clause IAM closes a refusal that rests on one of the principal's OWN
/// documents with — the half of the sentence that says a document refused the
/// call, which no other decision's sentence carries.
const EXPLICIT_DENY_CLAUSE: &str = "with an explicit deny in an identity-based policy";

/// What the gate answers one row: the refusal a readable document's explicit `Deny`
/// leaves, the refusal of a request no readable document allows, or the operation's
/// own answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gate {
    ExplicitDeny,
    NoGrant,
    Served,
}

/// One request this suite makes, named by the operation that answers it, with the
/// IAM action that operation asks for and the resource it addresses.
struct Call {
    operation: &'static str,
    action: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    /// The resource the decision is taken over — the ARN the request names, and
    /// the wildcard for a call that names none.
    resource: String,
}

/// The sentence IAM refuses a request an identity-based document explicitly denies
/// with: fixed text around the principal that signed, the action and the resource.
fn explicit_deny_message(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         {EXPLICIT_DENY_CLAUSE}"
    )
}

/// The sentence IAM refuses a request no readable document ALLOWS with — its
/// default deny, which closes by naming the action nothing granted rather than a
/// document.
fn no_grant_message(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// The sentence the gate refuses `call` with for a row, and `None` for a row it
/// serves.
fn gate_message(gate: Gate, principal: &str, call: &Call) -> Option<String> {
    match gate {
        Gate::ExplicitDeny => Some(explicit_deny_message(principal, call.action, &call.resource)),
        Gate::NoGrant => Some(no_grant_message(principal, call.action, &call.resource)),
        Gate::Served => None,
    }
}

/// The IAM action namespace `action` is published under, which `<namespace>:*` is
/// the wildcard for every action of: this service publishes its control plane under
/// one and its Apache Kafka data plane under another, so a document written about
/// "every action of this service" has to be written about the namespace the call's
/// own action carries.
fn action_namespace(action: &str) -> &str {
    action.split(':').next().unwrap_or(action)
}

/// The calls the rows below are made over: one per kind of target an MSK request
/// can carry, so the gate is shown to be the service's rather than one handler's.
fn calls(cluster_arn: &str) -> Vec<Call> {
    vec![
        // The target is in the BODY, and the cluster it names does not exist: the
        // gate still answers first.
        Call {
            operation: "CreateVpcConnection",
            action: "kafka:CreateVpcConnection",
            method: "POST",
            uri: "/v1/vpc-connection".to_string(),
            body: Some(json!({
                "targetClusterArn": cluster_arn,
                "authentication": "SASL_IAM",
                "vpcId": "vpc-0a1b2c3d",
                "clientSubnets": ["subnet-0a1b2c3d"],
                "securityGroups": ["sg-0a1b2c3d"],
            })),
            resource: cluster_arn.to_string(),
        },
        // A reader-and-mutation whose target is the URI's path segment.
        Call {
            operation: "RebootBroker",
            action: "kafka:RebootBroker",
            method: "PUT",
            uri: format!("/v1/clusters/{}/reboot-broker", pct(cluster_arn)),
            body: Some(json!({ "brokerIds": ["1"] })),
            resource: cluster_arn.to_string(),
        },
        // A topic call, which reaches the cluster's Apache Kafka plane and is
        // authorized under that plane's own action.
        Call {
            operation: "ListTopics",
            action: "kafka-cluster:Connect",
            method: "GET",
            uri: format!("/v1/clusters/{}/topics", pct(cluster_arn)),
            body: None,
            resource: cluster_arn.to_string(),
        },
        // A create that names no resource at all: the decision is taken over every
        // resource, which is what a document stating the action then matches.
        Call {
            operation: "CreateConfiguration",
            action: "kafka:CreateConfiguration",
            method: "POST",
            uri: "/v1/configurations".to_string(),
            body: Some(json!({
                "name": "identity-gate-configuration",
                "serverProperties": ::aws_smithy_types::base64::encode(b"num.partitions=1\n"),
            })),
            resource: "*".to_string(),
        },
    ]
}

/// Make `call` signed with `access_key_id`.
async fn make(emulator: &Emulator, call: &Call, access_key_id: &str) -> Reply {
    emulator
        .call_as_in(access_key_id, REGION, call.method, &call.uri, call.body.clone())
        .await
}

/// RULE: a document that explicitly denies the action refuses the call, one that
/// allows it leaves the call to the operation, and one that says nothing about it
/// leaves the request with no grant at all — over every operation of the table and
/// every document of it.
#[tokio::test]
async fn an_identity_policy_decides_every_cluster_scoped_call() {
    let emulator = Emulator::new();
    let principal = given_assumed_role_session(&emulator.engine, ROLE, SESSION, SESSION_KEY_ID);
    // A cluster this endpoint really holds, so a served call is answered from
    // state rather than refused for its target.
    let cluster = emulator
        .create_ok(&ClusterRequest::named("identity-gate"))
        .await;

    for call in calls(&cluster) {
        // The documents are written about this call's own action, so one table
        // serves every operation.
        let every_action_of_this_service = format!("{}:*", action_namespace(call.action));
        let rows: Vec<(&str, Option<Value>, Gate)> = vec![
            ("no policy at all", None, Gate::NoGrant),
            (
                "a deny of exactly this action on every resource",
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        { "Effect": "Deny", "Action": call.action, "Resource": "*" }
                    ],
                })),
                Gate::ExplicitDeny,
            ),
            (
                "a deny of every action",
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [{ "Effect": "Deny", "Action": "*", "Resource": "*" }],
                })),
                Gate::ExplicitDeny,
            ),
            (
                "a deny beside an allow of the same action",
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        { "Effect": "Allow", "Action": call.action, "Resource": "*" },
                        { "Effect": "Deny", "Action": call.action, "Resource": "*" },
                    ],
                })),
                Gate::ExplicitDeny,
            ),
            (
                "a deny scoped to the resource the request names",
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        { "Effect": "Deny", "Action": call.action, "Resource": call.resource }
                    ],
                })),
                Gate::ExplicitDeny,
            ),
            (
                "a deny scoped to another resource",
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Deny",
                        "Action": call.action,
                        "Resource": format!(
                            "arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/elsewhere/{GHOST_ID}"
                        ),
                    }],
                })),
                Gate::NoGrant,
            ),
            (
                "a deny of another action of this service",
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        { "Effect": "Deny", "Action": "kafka:DeleteCluster", "Resource": "*" }
                    ],
                })),
                Gate::NoGrant,
            ),
            (
                "a deny of another service's actions",
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [{ "Effect": "Deny", "Action": "s3:*", "Resource": "*" }],
                })),
                Gate::NoGrant,
            ),
            (
                "an allow of another action of this service",
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        { "Effect": "Allow", "Action": "kafka:DeleteCluster", "Resource": "*" }
                    ],
                })),
                Gate::NoGrant,
            ),
            (
                "an allow of exactly this action on every resource",
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        { "Effect": "Allow", "Action": call.action, "Resource": "*" }
                    ],
                })),
                Gate::Served,
            ),
            (
                "an allow of every action of this service",
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Action": every_action_of_this_service,
                        "Resource": "*",
                    }],
                })),
                Gate::Served,
            ),
        ];

        for (case, document, gate) in rows {
            given_role_inline_policy(&emulator.engine, ROLE, document.as_ref());
            let reply = make(&emulator, &call, SESSION_KEY_ID).await;
            if let Some(expected) = gate_message(gate, &principal, &call) {
                assert_eq!(
                    (reply.status, reply.error_code(), reply.error_message()),
                    (ACCESS_DENIED_STATUS, ACCESS_DENIED_CODE, expected.as_str()),
                    "case {case:?}/{}: expected IAM's {gate:?} refusal, got {} {}",
                    call.operation,
                    reply.status,
                    reply.body
                );
            } else {
                assert_ne!(
                    reply.error_code(),
                    ACCESS_DENIED_CODE,
                    "case {case:?}/{}: this caller's policies allow the action, so the gate \
                     must not refuse it, got {} {}",
                    call.operation,
                    reply.status,
                    reply.body
                );
            }
        }
    }

    // The documents are off the role again, so nothing the rows above stored
    // outlives them.
    given_role_inline_policy(&emulator.engine, ROLE, None);
}

/// RULE: the identity decision comes FIRST, so a denied caller is told it is not
/// authorized rather than told anything about the target — a cluster that does not
/// exist, a value that is no cluster ARN, and a body the operation would otherwise
/// refuse are all answered with the denial; and the account's own credential, which
/// no document denies, reaches each operation's own answer for the identical
/// request.
#[tokio::test]
async fn the_identity_decision_precedes_everything_the_request_names() {
    let emulator = Emulator::new();
    let principal = given_assumed_role_session(&emulator.engine, ROLE, SESSION, SESSION_KEY_ID);
    given_role_inline_policy(
        &emulator.engine,
        ROLE,
        Some(&json!({
            "Version": "2012-10-17",
            "Statement": [
                { "Effect": "Deny", "Action": ["kafka:*", "kafka-cluster:*"], "Resource": "*" }
            ],
        })),
    );

    // Targets the operations would each refuse for a reason of their own.
    let absent = format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/absent/{GHOST_ID}");
    for target in [absent.as_str(), "not-a-cluster-arn"] {
        for call in calls(target) {
            let denied = make(&emulator, &call, SESSION_KEY_ID).await;
            assert_eq!(
                (denied.status, denied.error_code(), denied.error_message()),
                (
                    ACCESS_DENIED_STATUS,
                    ACCESS_DENIED_CODE,
                    explicit_deny_message(&principal, call.action, &call.resource).as_str()
                ),
                "case {target:?}/{}: expected the denial before anything resolves the target, \
                 got {} {}",
                call.operation,
                denied.status,
                denied.body
            );

            // The control: the same request from the account's own credential,
            // which no document denies, is answered by the operation itself —
            // whatever it answers, it is not the identity gate's refusal. (A
            // target that names no cluster is refused by the RESOURCE-scoped half
            // of the gate for every caller, which is that decision's own sentence
            // and carries no document clause.)
            let own = make(&emulator, &call, DEFAULT_ACCESS_KEY_ID).await;
            assert!(
                !own.error_message().contains(EXPLICIT_DENY_CLAUSE),
                "case {target:?}/{}: nothing denies the account's own credential, so no \
                 document may be named in its answer, got {} {}",
                call.operation,
                own.status,
                own.body
            );
        }
    }

    given_role_inline_policy(&emulator.engine, ROLE, None);
}
