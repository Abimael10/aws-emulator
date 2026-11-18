// SPDX-License-Identifier: CC0-1.0

//! How the MSK operations that resolve a cluster — or one cluster operation —
//! by ARN refuse a value they cannot parse, driven end to end over the wire
//! through the same `MultiServiceRouter` the emulator serves from (the shared
//! `kafka_harness`).
//!
//! TWO ordered gates, and this suite asserts both over a table rather than over
//! one recorded request.
//!
//! RULE 1 (the gate that answers first, for every operation whose target is a
//! resource this service names by ARN): the action such a call asks for applies to
//! that resource, and a permission for it is scoped to the resource it is granted
//! on — so a value that IS an ARN and names no resource of that kind matches no
//! resource-scoped permission, reaches no grant, and is refused HTTP 403
//! `AccessDeniedException` naming the calling principal, the operation's IAM action
//! and the resource the decision was taken over, before anything parses the value
//! or resolves what it names.
//!
//! RULE 2: a value that is no ARN AT ALL names no resource for a policy to be
//! matched against, so it keeps the service's parameter refusal: HTTP 400
//! `BadRequestException` with a FIXED sentence of the service's — fixed text end to
//! end, so no part of the submitted value reaches it and two calls carrying
//! different unusable values are told exactly the same thing — reporting the member
//! that carried it in `invalidParameter` by its WIRE name. Which fixed sentence is
//! the operation's: the generic parameter one on most surfaces, the
//! cluster-ARN-specific one on the SCRAM-secret and client-VPC-connection
//! surfaces, so the table carries the expected sentence per row rather than one
//! blanket string.
//!
//! Rule 2 does NOT hold for the four actions an MSK cluster POLICY governs
//! (`CreateVpcConnection`, `GetBootstrapBrokers`, `DescribeCluster`,
//! `DescribeClusterV2`): the only document that can grant one of them is the one
//! attached to the cluster, so a request naming no cluster reaches no grant
//! whatever the value's shape and Rule 1 answers it even for a value that is no
//! ARN — which is the third suite below.
//!
//! The rows span both dimensions the rules generalise over: which operation
//! answers (every cluster-scoped reader, update, delete and policy call, the two
//! cluster-operation readers, plus `GetCompatibleKafkaVersions`, which carries the
//! ARN in a query parameter rather than in the URI path) and what makes the
//! value unusable (not an ARN at all, a truncated ARN, a very long opaque string
//! and a non-ASCII one for Rule 2; another service's namespace, another resource
//! kind of this service, a cluster ARN missing or carrying an extra segment and one
//! with an empty region for Rule 1 — none of which is a cluster ARN, and none a
//! cluster-OPERATION ARN either, so one table serves both members).
//!
//! `CreateVpcConnection` is outside both rules: it names its target in the BODY, so
//! authorization refuses a value that is no ARN at all there too
//! (`vpc_connection_admission.rs`). The TOPIC surface takes Rule 1's decision under
//! the cluster's DATA-plane action instead of a `kafka:<ApiName>` one
//! (`topic_cluster_arn_authorization.rs`); what those operations answer a value
//! that is not an ARN at all is in `topic_lifecycle.rs`.

use std::collections::BTreeSet;

use serde_json::{json, Value};

// The service's own refusal wordings and the wire names it reports parameters
// by, so the suite and the handlers cannot drift apart on what the service says.
use service_com_amazonaws_kafka_kafka::state::{
    BAD_REQUEST_MESSAGE, CLUSTER_ARN_MEMBER, CLUSTER_ARN_NOT_VALID_MESSAGE,
    CLUSTER_OPERATION_ARN_MEMBER,
};

// The router harness, the signing region, the account id and the URI encoder
// every suite of this crate shares, so this one adds only its own rule.
use super::kafka_harness::{
    pct, sleep_past_activation, ClusterRequest, Emulator, Reply, ACCOUNT, REGION,
};

/// Where a request carries the ARN, which is what decides how a row is built.
enum Carrier {
    /// An `@httpLabel`: the ARN is percent-encoded into one path segment between
    /// `prefix` and `suffix`.
    Label {
        prefix: &'static str,
        suffix: &'static str,
    },
    /// An `@httpQuery` member of `path`, under the query name the model gives it.
    Query {
        path: &'static str,
        name: &'static str,
    },
}

/// One operation the rules cover: how it is called, a body whose OTHER members
/// are all present and legal so the row exercises the ARN alone, the member name
/// its refusal must report and the fixed sentence it must carry.
struct Operation {
    name: &'static str,
    method: &'static str,
    carrier: Carrier,
    body: Option<Value>,
    member: &'static str,
    message: &'static str,
}

impl Operation {
    /// The IAM action a call to this operation asks for.
    fn action(&self) -> String {
        format!("kafka:{}", self.name)
    }

    /// Whether the ONLY document that can grant this operation's action is the
    /// cluster's OWN resource-based policy — in which case a request naming no
    /// cluster can reach no grant whatever the value's shape, so Rule 1 answers it
    /// even for a value that is no ARN at all.
    fn only_a_cluster_policy_can_grant(&self) -> bool {
        CLUSTER_POLICY_ACTIONS.contains(&self.name)
    }
    /// The request URI for `arn`, with the ARN percent-encoded wherever the
    /// model puts it.
    fn uri(&self, arn: &str) -> String {
        match &self.carrier {
            Carrier::Label { prefix, suffix } => format!("{prefix}{}{suffix}", pct(arn)),
            Carrier::Query { path, name } => format!("{path}?{name}={}", pct(arn)),
        }
    }

    /// The request body the row sends, which carries no ARN: every operation this
    /// rule covers takes the ARN in its URI.
    fn body(&self) -> Option<Value> {
        self.body.clone()
    }
}

/// A version string shaped like the ones this service mints, for the
/// `CurrentVersion` the update operations require: the ARN is resolved before
/// any body member, so what the row sends here only has to be present.
const SOME_CURRENT_VERSION: &str = "K3AEGXETSR30VB";

/// The operations whose action an MSK cluster POLICY governs — the resource-based
/// policy `PutClusterPolicy` attaches to a cluster, which Amazon MSK documents as
/// granting another account's principals multi-VPC private connectivity to it:
/// creating a VPC connection, and the two cluster readers plus the
/// bootstrap-broker reader a client needs to use one.
///
/// Written out here rather than read from the service, so a drift in its own
/// constant fails this suite.
const CLUSTER_POLICY_ACTIONS: [&str; 4] = [
    "CreateVpcConnection",
    "GetBootstrapBrokers",
    "DescribeCluster",
    "DescribeClusterV2",
];

/// Every operation of this service that resolves a cluster, or one cluster
/// operation, by ARN — with the route the model binds it to
/// (`smithy_query(query="operation",operation="<op>")` on this crate's
/// `smithy-model/com-amazonaws-kafka-kafka.json`).
fn operations() -> Vec<Operation> {
    let cluster = |suffix: &'static str| Carrier::Label {
        prefix: "/v1/clusters/",
        suffix,
    };
    let configuration = format!(
        "arn:aws:kafka:{REGION}:{ACCOUNT}:configuration/a-configuration/\
         11111111-2222-4333-8444-555555555555-1"
    );
    let secret = format!("arn:aws:secretsmanager:{REGION}:{ACCOUNT}:secret:AmazonMSK_a-user");
    vec![
        Operation {
            name: "ListNodes",
            method: "GET",
            carrier: cluster("/nodes"),
            body: None,
            member: CLUSTER_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
        Operation {
            name: "UpdateBrokerStorage",
            method: "PUT",
            carrier: cluster("/nodes/storage"),
            body: Some(json!({
                "currentVersion": SOME_CURRENT_VERSION,
                "targetBrokerEBSVolumeInfo": [
                    { "kafkaBrokerNodeId": "ALL", "volumeSizeGB": 1100 },
                ],
            })),
            member: CLUSTER_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
        Operation {
            name: "UpdateBrokerType",
            method: "PUT",
            carrier: cluster("/nodes/type"),
            body: Some(json!({
                "currentVersion": SOME_CURRENT_VERSION,
                "targetInstanceType": "kafka.m5.large",
            })),
            member: CLUSTER_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
        Operation {
            name: "UpdateBrokerCount",
            method: "PUT",
            carrier: cluster("/nodes/count"),
            body: Some(json!({
                "currentVersion": SOME_CURRENT_VERSION,
                "targetNumberOfBrokerNodes": 3,
            })),
            member: CLUSTER_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
        Operation {
            name: "DeleteCluster",
            method: "DELETE",
            carrier: cluster(""),
            body: None,
            member: CLUSTER_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
        Operation {
            name: "UpdateSecurity",
            method: "PATCH",
            carrier: cluster("/security"),
            body: Some(json!({ "currentVersion": SOME_CURRENT_VERSION })),
            member: CLUSTER_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
        Operation {
            name: "UpdateStorage",
            method: "PUT",
            carrier: cluster("/storage"),
            body: Some(json!({
                "currentVersion": SOME_CURRENT_VERSION,
                "volumeSizeGB": 1100,
            })),
            member: CLUSTER_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
        Operation {
            name: "GetClusterPolicy",
            method: "GET",
            carrier: cluster("/policy"),
            body: None,
            member: CLUSTER_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
        Operation {
            name: "DeleteClusterPolicy",
            method: "DELETE",
            carrier: cluster("/policy"),
            body: None,
            member: CLUSTER_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
        Operation {
            name: "PutClusterPolicy",
            method: "PUT",
            carrier: cluster("/policy"),
            body: Some(json!({
                "policy": json!({ "Version": "2012-10-17", "Statement": [] }).to_string(),
            })),
            member: CLUSTER_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
        Operation {
            name: "UpdateClusterKafkaVersion",
            method: "PUT",
            carrier: cluster("/version"),
            body: Some(json!({
                "currentVersion": SOME_CURRENT_VERSION,
                "targetKafkaVersion": "3.6.0",
            })),
            member: CLUSTER_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
        Operation {
            name: "ListClusterOperations",
            method: "GET",
            carrier: cluster("/operations"),
            body: None,
            member: CLUSTER_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
        Operation {
            name: "DescribeCluster",
            method: "GET",
            carrier: cluster(""),
            body: None,
            member: CLUSTER_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
        Operation {
            name: "DescribeClusterV2",
            method: "GET",
            carrier: Carrier::Label {
                prefix: "/api/v2/clusters/",
                suffix: "",
            },
            body: None,
            member: CLUSTER_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
        Operation {
            name: "GetBootstrapBrokers",
            method: "GET",
            carrier: cluster("/bootstrap-brokers"),
            body: None,
            member: CLUSTER_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
        Operation {
            name: "UpdateConnectivity",
            method: "PUT",
            carrier: cluster("/connectivity"),
            body: Some(json!({
                "currentVersion": SOME_CURRENT_VERSION,
                "connectivityInfo": { "publicAccess": { "type": "DISABLED" } },
            })),
            member: CLUSTER_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
        Operation {
            name: "UpdateClusterConfiguration",
            method: "PUT",
            carrier: cluster("/configuration"),
            body: Some(json!({
                "currentVersion": SOME_CURRENT_VERSION,
                "configurationInfo": { "arn": configuration, "revision": 1 },
            })),
            member: CLUSTER_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
        Operation {
            name: "ListScramSecrets",
            method: "GET",
            carrier: cluster("/scram-secrets"),
            body: None,
            member: CLUSTER_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
        // The two readers whose target is one cluster OPERATION: the same shape
        // check, on the ARN kind their own member names.
        Operation {
            name: "DescribeClusterOperation",
            method: "GET",
            carrier: Carrier::Label {
                prefix: "/v1/operations/",
                suffix: "",
            },
            body: None,
            member: CLUSTER_OPERATION_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
        Operation {
            name: "DescribeClusterOperationV2",
            method: "GET",
            carrier: Carrier::Label {
                prefix: "/api/v2/operations/",
                suffix: "",
            },
            body: None,
            member: CLUSTER_OPERATION_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
        // The surfaces that answer the cluster-ARN-SPECIFIC sentence for the
        // same condition: one fixed string, a different one, chosen by the
        // operation rather than evened out across the service.
        Operation {
            name: "BatchAssociateScramSecret",
            method: "POST",
            carrier: cluster("/scram-secrets"),
            body: Some(json!({ "secretArnList": [secret.clone()] })),
            member: CLUSTER_ARN_MEMBER,
            message: CLUSTER_ARN_NOT_VALID_MESSAGE,
        },
        Operation {
            name: "BatchDisassociateScramSecret",
            method: "PATCH",
            carrier: cluster("/scram-secrets"),
            body: Some(json!({ "secretArnList": [secret] })),
            member: CLUSTER_ARN_MEMBER,
            message: CLUSTER_ARN_NOT_VALID_MESSAGE,
        },
        Operation {
            name: "ListClientVpcConnections",
            method: "GET",
            carrier: cluster("/client-vpc-connections"),
            body: None,
            member: CLUSTER_ARN_MEMBER,
            message: CLUSTER_ARN_NOT_VALID_MESSAGE,
        },
        // The operation that carries the cluster ARN OUTSIDE the URI path and
        // is still answered as a parameter problem: the rule is about the member
        // that carried the value, not about where the request put it.
        //
        // `CreateVpcConnection` is NOT here. Amazon MSK decides whether the
        // caller may perform `kafka:CreateVpcConnection` on the resource that
        // request names before it resolves or parses the target, so a value that
        // names no cluster is answered with the authorization refusal rather
        // than with a parameter sentence — which is `vpc_connection_admission.rs`.
        Operation {
            name: "GetCompatibleKafkaVersions",
            method: "GET",
            carrier: Carrier::Query {
                path: "/v1/compatible-kafka-versions",
                name: "clusterArn",
            },
            body: None,
            member: CLUSTER_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
        },
    ]
}

/// Every way a submitted value can fail to be an ARN AT ALL, which is what Rule 2
/// covers: there is no resource in any of them for a policy to be matched against,
/// so each is answered as the parameter it is.
fn values_that_are_no_arn() -> Vec<(&'static str, String)> {
    vec![
        ("not-an-arn-at-all", "not-a-cluster-arn".to_string()),
        ("free-text-with-spaces", "not an arn at all".to_string()),
        ("truncated-arn", "arn:aws:kafka".to_string()),
        (
            "a-long-opaque-value",
            format!("not-a-valid-cluster-arn-{}", "x".repeat(200)),
        ),
        ("a-non-ascii-value", "ニセのクラスター-arn".to_string()),
    ]
}

/// Every way a value can BE an ARN and still name no resource either kind of
/// member carries: none of these rows is an MSK cluster ARN, and none is an MSK
/// cluster-OPERATION ARN either, so one table serves both members. The account
/// segment of each row is the emulator's own, so no row is refused on the account
/// it names: what refuses them is that nothing can hold a permission scoped to a
/// resource they do not name.
fn arns_naming_no_cluster() -> Vec<(&'static str, String)> {
    vec![
        (
            "another-service-namespace",
            format!("arn:aws:ec2:{REGION}:{ACCOUNT}:instance/i-0a1b2c3d"),
        ),
        (
            "another-resource-kind-of-this-service",
            format!(
                "arn:aws:kafka:{REGION}:{ACCOUNT}:configuration/a-configuration/\
                 11111111-2222-4333-8444-555555555555-1"
            ),
        ),
        (
            "cluster-arn-missing-its-id-segment",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/only-a-name"),
        ),
        (
            "cluster-arn-with-an-extra-segment",
            format!(
                "arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/a-cluster/\
                 11111111-2222-4333-8444-555555555555-1/extra"
            ),
        ),
        (
            "cluster-arn-without-a-region",
            format!(
                "arn:aws:kafka::{ACCOUNT}:cluster/a-cluster/\
                 11111111-2222-4333-8444-555555555555-1"
            ),
        ),
    ]
}

/// The sentence a request that reaches no grant is refused with: fixed connectives
/// around the calling principal, the IAM action and the resource the decision was
/// taken over, closed by IAM's default-deny clause naming the action nothing
/// allowed.
fn not_authorized(action: &str, resource: &str) -> String {
    format!(
        "User: arn:aws:iam::{ACCOUNT}:user/emulator is not authorized to perform: {action} on \
         resource: {resource} because no identity-based policy allows the {action} action"
    )
}

/// Assert `reply` is the refusal the service answers a value it cannot parse as
/// the ARN kind `member` names: HTTP 400 `BadRequestException`, that surface's
/// fixed sentence as the WHOLE message, and `member` as the parameter at fault.
/// The submitted value must appear nowhere in the message: the service keeps it
/// constant, so echoing the request would make it vary.
fn assert_unparsable_arn(
    reply: &Reply,
    case: &str,
    member: &str,
    message: &str,
    submitted: &str,
) {
    assert_eq!(
        (reply.status, reply.error_code()),
        (400, "BadRequestException"),
        "case {case}: expected 400 BadRequestException, got {} {}",
        reply.status,
        reply.body
    );
    assert_eq!(
        reply.error_message(),
        message,
        "case {case}: expected the fixed sentence {message:?}, got {:?}",
        reply.error_message()
    );
    assert_eq!(
        reply.at("/invalidParameter").as_str(),
        Some(member),
        "case {case}: expected the refusal to name {member}, got {}",
        reply.body
    );
    assert!(
        !reply.error_message().contains(submitted),
        "case {case}: the refusal must not echo the submitted value, got {:?}",
        reply.error_message()
    );
}

/// RULE 2: every operation that resolves a cluster, or one cluster operation, by
/// ARN refuses a value it cannot parse as that kind of ARN with HTTP 400
/// `BadRequestException`, the fixed sentence that surface answers — the whole
/// message, with nothing of the request in it — and the wire name of the member
/// that carried the value in `invalidParameter`; whatever makes the value
/// unparsable, and whichever operation answers.
#[tokio::test]
async fn every_cluster_operation_refuses_an_unparsable_arn_alike() {
    let emulator = Emulator::new();

    // One cluster this endpoint does hold, so the rows below are refused for the
    // shape of the value they carry rather than by a service refusing everything.
    let held = emulator.create_ok(&ClusterRequest::named("held-here")).await;
    sleep_past_activation();

    for operation in operations()
        .into_iter()
        .filter(|operation| !operation.only_a_cluster_policy_can_grant())
    {
        for (case, value) in values_that_are_no_arn() {
            let reply = emulator
                .call(operation.method, &operation.uri(&value), operation.body())
                .await;
            assert_unparsable_arn(
                &reply,
                &format!("{case}/{}", operation.name),
                operation.member,
                operation.message,
                &value,
            );
        }
    }

    // The control: the cluster this endpoint holds is still served, and the
    // refusals above changed nothing about it.
    let described = emulator.describe(&held).await;
    assert_eq!(
        (
            described.status,
            described.at("/clusterInfo/clusterArn").as_str()
        ),
        (200, Some(held.as_str())),
        "case control: the held cluster must still be served, got {} {}",
        described.status,
        described.body
    );
    let nodes = emulator
        .call("GET", &format!("/v1/clusters/{}/nodes", pct(&held)), None)
        .await;
    assert_eq!(
        nodes.status, 200,
        "case control: the held cluster's nodes must still be listed, got {} {}",
        nodes.status, nodes.body
    );
}

/// RULE 2: each surface's sentence is FIXED, so one operation answers exactly one
/// string across every unparsable value — a client that classifies MSK refusals
/// by their message sees one string, however long, how encoded or how shaped the
/// value it sent was.
#[tokio::test]
async fn each_surface_answers_one_byte_identical_message() {
    let emulator = Emulator::new();

    let mut calls = 0usize;
    for operation in operations()
        .into_iter()
        .filter(|operation| !operation.only_a_cluster_policy_can_grant())
    {
        let mut messages: BTreeSet<String> = BTreeSet::new();
        for (case, value) in values_that_are_no_arn() {
            let reply = emulator
                .call(operation.method, &operation.uri(&value), operation.body())
                .await;
            assert_eq!(
                reply.status, 400,
                "case {case}/{}: expected HTTP 400, got {} {}",
                operation.name, reply.status, reply.body
            );
            messages.insert(reply.error_message().to_string());
            calls += 1;
        }
        let distinct: Vec<&String> = messages.iter().collect();
        assert_eq!(
            distinct,
            vec![&operation.message.to_string()],
            "case {}: expected one fixed message across every unparsable value, got {distinct:?}",
            operation.name
        );
    }

    assert!(
        calls > 1,
        "the table must drive more than one call, got {calls}"
    );
}

/// RULE 1: every operation whose target is a resource named by ARN is authorized
/// on that value as submitted, before anything parses it — so a value that reads as
/// an ARN and names no resource of that kind matches no resource-scoped permission,
/// reaches no grant, and is refused HTTP 403 `AccessDeniedException` naming the
/// calling principal, that operation's IAM action and the resource the decision was
/// taken over; whichever operation answers, and whatever makes the value name no
/// resource.
#[tokio::test]
async fn an_arn_that_names_no_cluster_is_refused_by_authorization_not_as_a_parameter() {
    let emulator = Emulator::new();

    // One cluster this endpoint does hold, so what follows is about the value each
    // row carries rather than a surface refusing everything.
    let held = emulator
        .create_ok(&ClusterRequest::named("gate-control"))
        .await;
    sleep_past_activation();

    for operation in operations() {
        for (case, value) in arns_naming_no_cluster() {
            let reply = emulator
                .call(operation.method, &operation.uri(&value), operation.body())
                .await;
            let case = format!("{case}/{}", operation.name);
            assert_eq!(
                (reply.status, reply.error_code()),
                (403, "AccessDeniedException"),
                "case {case}: expected the authorization refusal, got {} {}",
                reply.status,
                reply.body
            );
            assert_eq!(
                reply.error_message(),
                not_authorized(&operation.action(), &value),
                "case {case}: expected the default-deny sentence, got {:?}",
                reply.error_message()
            );
            assert!(
                !reply.error_message().contains(operation.message),
                "case {case}: the gate must not answer the parameter sentence, got {:?}",
                reply.error_message()
            );
        }
    }

    // The control: the cluster this endpoint holds is still served, so the gate
    // refuses only what names no cluster.
    let described = emulator.describe(&held).await;
    assert_eq!(
        (
            described.status,
            described.at("/clusterInfo/clusterArn").as_str()
        ),
        (200, Some(held.as_str())),
        "case control: the held cluster must still be served, got {} {}",
        described.status,
        described.body
    );
}

/// RULE 1 and RULE 2 are ORDERED and distinct: a WELL-FORMED cluster ARN of this
/// account passes the authorization gate and reaches the resolution behind it — so
/// an absent cluster is answered by the operation's own refusal and never as
/// unauthorized — while a value one segment away from it never gets that far, and a
/// value that is no ARN at all stays the parameter refusal.
///
/// Driven over the operations whose target IS a cluster, since the value the first
/// leg sends has to be a resource of the kind the member carries; the two
/// cluster-operation readers resolve their own kind of ARN, which
/// `cluster_operation_v2.rs` drives.
#[tokio::test]
async fn the_gates_are_ordered_and_each_answers_its_own_values() {
    let emulator = Emulator::new();

    let absent = format!(
        "arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/absent/\
         11111111-2222-4333-8444-555555555555-1"
    );
    let unmatchable = format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/only-a-name");
    let no_arn = "not-a-cluster-arn".to_string();

    for operation in operations()
        .into_iter()
        .filter(|operation| operation.member == CLUSTER_ARN_MEMBER)
    {
        let served = emulator
            .call(operation.method, &operation.uri(&absent), operation.body())
            .await;
        assert_ne!(
            served.error_code(),
            "AccessDeniedException",
            "case \"a well-formed absent cluster\"/{}: a cluster ARN of this account must pass \
             the gate, got {} {}",
            operation.name,
            served.status,
            served.body
        );

        let denied = emulator
            .call(
                operation.method,
                &operation.uri(&unmatchable),
                operation.body(),
            )
            .await;
        assert_eq!(
            (denied.status, denied.error_code()),
            (403, "AccessDeniedException"),
            "case \"one segment away\"/{}: expected the authorization refusal, got {} {}",
            operation.name,
            denied.status,
            denied.body
        );

        // A value that is no ARN at all names no resource for a statement to be
        // matched against, so it keeps the parameter refusal — except on the
        // operations whose action only a cluster's own policy can grant, where no
        // grant can rest on anything else and the gate answers that too.
        let parameter = emulator
            .call(operation.method, &operation.uri(&no_arn), operation.body())
            .await;
        let expected = if operation.only_a_cluster_policy_can_grant() {
            (403, "AccessDeniedException")
        } else {
            (400, "BadRequestException")
        };
        assert_eq!(
            (parameter.status, parameter.error_code()),
            expected,
            "case \"no ARN at all\"/{}: expected {expected:?}, got {} {}",
            operation.name,
            parameter.status,
            parameter.body
        );
    }
}

/// RULE 1 (its other half): for the four actions an MSK cluster POLICY governs,
/// the only document that can grant the call is the one attached to the cluster —
/// so a request that names no cluster reaches no grant whatever the value's shape,
/// and even a value that is no ARN at all is the authorization refusal rather than
/// the parameter one. The rows are every such operation this suite drives, crossed
/// with every value that is no ARN.
#[tokio::test]
async fn an_action_only_a_cluster_policy_can_grant_is_refused_for_any_unmatchable_value() {
    let emulator = Emulator::new();

    let mut operations_seen = 0usize;
    for operation in operations()
        .into_iter()
        .filter(Operation::only_a_cluster_policy_can_grant)
    {
        operations_seen += 1;
        for (case, value) in values_that_are_no_arn() {
            let reply = emulator
                .call(operation.method, &operation.uri(&value), operation.body())
                .await;
            let case = format!("{case}/{}", operation.name);
            assert_eq!(
                (reply.status, reply.error_code()),
                (403, "AccessDeniedException"),
                "case {case}: expected the authorization refusal, got {} {}",
                reply.status,
                reply.body
            );
            // The decision was taken over no resource at all, so the refusal names
            // IAM's wildcard rather than echoing what the request sent.
            assert!(
                !reply.error_message().contains(value.as_str()),
                "case {case}: the refusal must not echo the submitted value, got {:?}",
                reply.error_message()
            );
        }
    }

    assert!(
        operations_seen > 1,
        "the table must drive more than one such operation, got {operations_seen}"
    );
}

