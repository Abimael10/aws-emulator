// SPDX-License-Identifier: CC0-1.0

//! WHAT can grant a call that MANAGES one of the client VPC connections made to an
//! MSK cluster, driven end to end over the same `MultiServiceRouter` the emulator
//! serves from (the shared `kafka_harness`), with the IAM role and the STS session
//! written through those services' own stores — as `iam:CreateRole`,
//! `iam:PutRolePolicy` and `sts:AssumeRole` write them.
//!
//! RULE under test: managing a client VPC connection is a call on the TARGET
//! cluster's multi-VPC private connectivity, the surface a cluster's own
//! resource-based policy is attached for, so the grant for it can rest on a document
//! that lives ON that cluster — and a cluster this endpoint's Region holds no record
//! of carries no document at all. Such a call therefore reaches no grant and is
//! refused as unauthorized: HTTP 403 `AccessDeniedException` carrying AWS's sentence
//! for a request no resource-based policy allows, whose variable parts all come from
//! the request and this emulator's own identity — the principal the presented
//! credential speaks for (as `sts:GetCallerIdentity` reports it), the IAM action the
//! operation asks for (`kafka:<ApiName>`), and the cluster ARN exactly as submitted —
//! and naming no member at fault, since nothing about a parameter is being complained
//! about. The decision is taken before the cluster's own state and before every
//! parameter the call then acts on, and it reports nothing about what the registry
//! holds.
//!
//! The rows differ along what the rule generalises over: the CREDENTIAL the request
//! is signed with and therefore the principal the sentence names (the account's own
//! published credential, and an assumed-role session whose identity policy allows the
//! action), the way this endpoint comes to hold no cluster under the submitted ARN,
//! and what the `vpcConnectionArn` member carries — including a value the operation
//! could not act on either, since the decision precedes reading it.
//!
//! The boundaries are pinned beside the rule: the CALLER's own identity-based
//! documents are still asked first, so a session whose document denies the action is
//! refused in IAM's identity-based sentence instead; and a cluster this endpoint DOES
//! hold carries the decision past this gate, where the same request is answered about
//! the connection it names and a connection the cluster really holds is rejected. An
//! ARN of another ACCOUNT is refused by the gate that reads the ARN's account segment
//! before this one (`client_vpc_connection_rejection.rs` and
//! `cluster_mutation_authorization_order.rs` pin that one).

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::kafka_harness::{
    given_assumed_role_session, given_role_inline_policy, poll_until_active,
    poll_until_vpc_connection_reports, ClusterRequest, ConnectionRequest, Emulator, Reply, ACCOUNT,
    DEFAULT_ACCESS_KEY_ID, REGION,
};

/// The operation under test asks for the IAM action `kafka:` plus the API name,
/// which is how every action of this service's control plane is published.
const ACTION: &str = "kafka:RejectClientVpcConnection";

/// The `VpcConnectionState` a connection in service carries, and the one a rejected
/// connection leaves service in — both values of the model's `VpcConnectionState`
/// enum (`smithy_query(query="shape",shape="VpcConnectionState")`), spelled out here
/// so this suite does not read its expectation off the implementation.
const AVAILABLE: &str = "AVAILABLE";
const REJECTING: &str = "REJECTING";

/// The status and wire code an authorization refusal carries, spelled out here
/// rather than read from the service so a drift in its own constants fails this
/// suite.
const ACCESS_DENIED_STATUS: u16 = 403;
const ACCESS_DENIED_CODE: &str = "AccessDeniedException";

/// The role the session rows sign as, the session's name, and the access key id that
/// session was minted for.
const ROLE: &str = "kafka-vpc-connection-target-caller";
const SESSION: &str = "vpc-connection-target-session";
const SESSION_KEY_ID: &str = "ASIAVPCCONNTARGET000";

/// Cluster ids of the form this service mints, for ARNs of clusters that were never
/// issued — three spellings, so no row's answer can rest on one of them.
const UNISSUED_CLUSTER_IDS: [&str; 3] = [
    "11111111-2222-4333-8444-555555555555-1",
    "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee-2",
    "00000000-0000-0000-0000-000000000000-1",
];

/// A well-formed MSK cluster ARN, assembled as the service spells one.
fn arn_of_cluster(region: &str, account: &str, name: &str, id: &str) -> String {
    format!("arn:aws:kafka:{region}:{account}:cluster/{name}/{id}")
}

/// A well-formed MSK VPC-connection ARN of this account and Region that names no
/// connection this emulator holds.
fn ghost_connection_arn(id: &str) -> String {
    format!("arn:aws:kafka:{REGION}:{ACCOUNT}:vpc-connection/{ACCOUNT}/ghost-target/{id}")
}

/// The principal every request signed with the account's own published credential
/// speaks for — the identity `sts:GetCallerIdentity` reports for it, and the one such
/// a request's refusal names.
fn ambient_principal() -> String {
    format!("arn:aws:iam::{ACCOUNT}:user/emulator")
}

/// AWS's sentence for a request no RESOURCE-based policy allows: fixed text around
/// the principal that signed, the action asked for and the resource addressed, with
/// the action repeated in the closing clause.
fn no_resource_policy_message(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no resource-based policy allows the {action} action"
    )
}

/// AWS's sentence for a request one of the principal's OWN documents explicitly
/// denies — the boundary beside the rule, so the two decisions are shown to word
/// their refusals differently.
fn explicit_deny_message(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         with an explicit deny in an identity-based policy"
    )
}

/// An identity-based document with one statement of `effect` about `ACTION` on every
/// resource — the shape `iam:PutRolePolicy` takes.
fn document(effect: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": effect, "Action": ACTION, "Resource": "*" }],
    })
}

/// `RejectClientVpcConnection` against `cluster` for `connection`, signed with
/// `access_key_id` at `region`'s endpoint.
async fn reject_as(
    emulator: &Emulator,
    access_key_id: &str,
    region: &str,
    cluster: &str,
    connection: &str,
) -> Reply {
    emulator
        .call_as_in(
            access_key_id,
            region,
            "PUT",
            &format!(
                "/v1/clusters/{}/client-vpc-connection",
                crate::kafka_harness::pct(cluster)
            ),
            Some(json!({ "vpcConnectionArn": connection })),
        )
        .await
}

/// Assert `reply` is the authorization refusal for a request nothing attached to
/// `resource` could grant, blaming `principal` and naming no member at fault.
fn assert_no_resource_policy_refusal(reply: &Reply, case: &str, principal: &str, resource: &str) {
    let expected = no_resource_policy_message(principal, ACTION, resource);
    assert_eq!(
        (reply.status, reply.error_code(), reply.error_message()),
        (
            ACCESS_DENIED_STATUS,
            ACCESS_DENIED_CODE,
            expected.as_str()
        ),
        "case {case:?}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED_CODE} {expected:?}, \
         got {} {}",
        reply.status,
        reply.body
    );
    assert_eq!(
        reply.at("/invalidParameter"),
        &Value::Null,
        "case {case:?}: an authorization refusal complains about no parameter, got {}",
        reply.body
    );
}

/// RULE: a `ClusterArn` this endpoint's Region holds no cluster under carries no
/// document that could grant the call, so the reject reaches no grant and is refused
/// as unauthorized — whichever credential signed it, however the endpoint came to
/// hold no cluster under that ARN, and whatever the connection member carries.
#[tokio::test]
async fn a_target_no_document_can_be_attached_to_reaches_no_grant() {
    let emulator = Emulator::new();
    // The session's own identity policy ALLOWS the action, so the rows it signs
    // reach this gate rather than the caller's own ([`explicit_deny_message`] is the
    // boundary asserted in the suite below).
    let session = given_assumed_role_session(&emulator.engine, ROLE, SESSION, SESSION_KEY_ID);
    given_role_inline_policy(&emulator.engine, ROLE, Some(&document("Allow")));

    // A cluster this endpoint really holds, with one live connection on it: the
    // control that makes every row below a refusal about the TARGET.
    let held = emulator
        .create_ok(&ClusterRequest::vpc_connection_target("target-grant-held"))
        .await;
    poll_until_active(&emulator, &held, "the held cluster").await;
    let live = emulator
        .create_vpc_connection_ok(&ConnectionRequest::to(&held))
        .await;

    // A cluster this endpoint HELD and then released, which the rule covers exactly
    // as a name never issued.
    let released = emulator
        .create_ok(&ClusterRequest::named("target-grant-released"))
        .await;
    let released_version = poll_until_active(&emulator, &released, "the cluster to release").await;
    let removed = emulator
        .call(
            "DELETE",
            &format!(
                "/v1/clusters/{}?currentVersion={released_version}",
                crate::kafka_harness::pct(&released)
            ),
            None,
        )
        .await;
    assert_eq!(
        removed.status, 200,
        "case \"a released cluster\": expected DeleteCluster to be accepted, got {} {}",
        removed.status, removed.body
    );

    // `(case, the endpoint's region, the cluster ARN this endpoint holds nothing
    // under)`.
    let mut targets: Vec<(String, &str, String)> = vec![
        (
            "a never-issued name".to_string(),
            REGION,
            arn_of_cluster(REGION, ACCOUNT, "never-issued", UNISSUED_CLUSTER_IDS[0]),
        ),
        (
            "a name neighbouring a held cluster".to_string(),
            REGION,
            arn_of_cluster(
                REGION,
                ACCOUNT,
                "target-grant-held-neighbour",
                UNISSUED_CLUSTER_IDS[1],
            ),
        ),
        (
            "an ARN naming another Region".to_string(),
            REGION,
            arn_of_cluster("eu-central-1", ACCOUNT, "elsewhere", UNISSUED_CLUSTER_IDS[2]),
        ),
        (
            "a held cluster seen from another Region's endpoint".to_string(),
            "eu-central-1",
            held.clone(),
        ),
        ("a released cluster".to_string(), REGION, released.clone()),
    ];
    // Every unissued id this suite names is covered as a cluster of its own.
    for id in UNISSUED_CLUSTER_IDS {
        let arn = arn_of_cluster(REGION, ACCOUNT, "never-issued-per-id", id);
        if !targets.iter().any(|(_, _, named)| *named == arn) {
            targets.push((format!("a never-issued cluster under id {id}"), REGION, arn));
        }
    }

    // What the request's OTHER member carries, which the decision is taken before
    // reading: a connection this endpoint really holds, a well-formed connection ARN
    // naming nothing, and a value of no ARN shape at all.
    let connections: Vec<(&str, String)> = vec![
        ("a live connection of the held cluster", live.clone()),
        (
            "a connection no registry holds",
            ghost_connection_arn("99999999-8888-4777-8666-555555555555-9"),
        ),
        ("a value of no ARN shape", "not-an-arn".to_string()),
    ];

    // Both credentials: the account's own published one and the session's, so the
    // principal the sentence names is shown to come from the request.
    let callers: Vec<(&str, &str, String)> = vec![
        (
            "the account's published credential",
            DEFAULT_ACCESS_KEY_ID,
            ambient_principal(),
        ),
        (
            "an assumed-role session",
            SESSION_KEY_ID,
            session.clone(),
        ),
    ];

    let mut principals_seen: BTreeSet<String> = BTreeSet::new();
    for (caller_case, access_key_id, principal) in &callers {
        for (target_case, region, target) in &targets {
            for (connection_case, connection) in &connections {
                let case =
                    format!("{caller_case} / {target_case} / {connection_case}");
                let reply = reject_as(&emulator, access_key_id, region, target, connection).await;
                assert_no_resource_policy_refusal(&reply, &case, principal, target);
                principals_seen.insert(principal.clone());
                // The refusal discloses nothing the request did not carry: neither
                // the cluster this endpoint does hold nor the connection on it
                // appears in a sentence about another ARN.
                if target != &held {
                    for secret in [&held, &live] {
                        assert!(
                            !reply.error_message().contains(secret.as_str()),
                            "case {case:?}: the refusal must carry nothing of the registry, \
                             got {:?}",
                            reply.error_message()
                        );
                    }
                }
            }
        }
    }
    assert_eq!(
        principals_seen.len(),
        2,
        "expected the two signing principals to be named by their own refusals, got \
         {principals_seen:?}"
    );

    // CONTROL (the boundary above the rule): the cluster this endpoint DOES hold
    // carries the call past this gate — the same request is answered about the
    // CONNECTION it names, and the connection that cluster really holds is rejected.
    let unresolvable = reject_as(
        &emulator,
        SESSION_KEY_ID,
        REGION,
        &held,
        &ghost_connection_arn(UNISSUED_CLUSTER_IDS[0]),
    )
    .await;
    assert_eq!(
        (unresolvable.status, unresolvable.error_code()),
        (400, "BadRequestException"),
        "case \"a held cluster with an unresolvable connection\": expected the parameter \
         refusal, got {} {}",
        unresolvable.status,
        unresolvable.body
    );
    let accepted = reject_as(&emulator, SESSION_KEY_ID, REGION, &held, &live).await;
    assert_eq!(
        accepted.status, 200,
        "case \"a held cluster with its own connection\": expected the reject to be served, \
         got {} {}",
        accepted.status, accepted.body
    );
    let described = emulator.describe_vpc_connection(&live).await;
    assert_eq!(
        described.at("/state").as_str(),
        Some(REJECTING),
        "case \"a held cluster with its own connection\": expected the rejected connection to \
         leave service, got {}",
        described.body
    );

    // Nothing this suite stored in IAM outlives it.
    given_role_inline_policy(&emulator.engine, ROLE, None);
}

/// RULE (the boundary beside it): the CALLER's own identity-based documents are
/// asked FIRST, so a principal whose document denies the action is refused in IAM's
/// identity-based sentence — about the same action and the same submitted ARN, and
/// whether or not this endpoint holds a cluster under it.
#[tokio::test]
async fn the_callers_own_documents_are_asked_before_the_target_is() {
    let emulator = Emulator::new();
    let session = given_assumed_role_session(&emulator.engine, ROLE, SESSION, SESSION_KEY_ID);
    given_role_inline_policy(&emulator.engine, ROLE, Some(&document("Deny")));

    let held = emulator
        .create_ok(&ClusterRequest::vpc_connection_target("target-grant-denied"))
        .await;
    poll_until_active(&emulator, &held, "the held cluster").await;
    let live = emulator
        .create_vpc_connection_ok(&ConnectionRequest::to(&held))
        .await;
    let absent = arn_of_cluster(REGION, ACCOUNT, "denied-absent", UNISSUED_CLUSTER_IDS[0]);

    // The two targets differ ONLY in whether this endpoint holds a cluster under
    // them, and the refusal is the same sentence about the ARN the request carried.
    for (case, target) in [
        ("a cluster this endpoint holds", held.as_str()),
        ("an ARN it holds nothing under", absent.as_str()),
    ] {
        let reply = reject_as(&emulator, SESSION_KEY_ID, REGION, target, &live).await;
        let expected = explicit_deny_message(&session, ACTION, target);
        assert_eq!(
            (reply.status, reply.error_code(), reply.error_message()),
            (
                ACCESS_DENIED_STATUS,
                ACCESS_DENIED_CODE,
                expected.as_str()
            ),
            "case {case:?}: expected IAM's identity-based refusal {expected:?}, got {} {}",
            reply.status,
            reply.body
        );
    }

    // And the connection the denied caller named is untouched: it is still in
    // service, read through the connection's own reader once the clock has settled
    // it — which a reject the gate let through would have moved out of service.
    poll_until_vpc_connection_reports(
        &emulator,
        REGION,
        "a refused reject",
        &live,
        AVAILABLE,
    )
    .await;

    given_role_inline_policy(&emulator.engine, ROLE, None);
}
