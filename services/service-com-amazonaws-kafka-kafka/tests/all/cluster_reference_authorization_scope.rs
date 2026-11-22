// SPDX-License-Identifier: CC0-1.0

//! WHICH values the pre-resolution authorization gate of this service decides over,
//! and WHAT its refusal names as the resource — driven end to end over the same
//! `MultiServiceRouter` the emulator serves from (the shared `kafka_harness`).
//!
//! The RULE under test: the decision a cluster-scoped call takes before it resolves
//! anything is taken over the resource its target REFERENCES, so it is taken only
//! where the target is a reference to a resource of the kind the action applies to.
//! That gives three readings of one submitted value, and the refusal's resource token
//! follows the reading:
//!
//! 1. a REFERENCE of the action's own kind that identifies no resource of it (a
//!    `cluster` resource field of the wrong arity) resolves to NO resource, so the
//!    decision is taken over IAM's wildcard and the refusal names `*`;
//! 2. a value that is no ARN at all resolves to nothing either, so where only the
//!    resource's OWN policy could have granted the action — the four actions an MSK
//!    cluster policy governs — the decision is taken over the value AS SUBMITTED,
//!    there being nothing else it stands for, and the refusal names that value;
//!    where an identity-based document could have granted the action, there is
//!    nothing to decide and the operation answers the parameter;
//! 3. an ARN naming a resource of ANOTHER KIND is no reference of this kind at all,
//!    so no permission scoped to the action's own resource is ever matched against it:
//!    the gate decides nothing and the operation answers the reference it cannot use.
//!
//! Every refusal the gate reaches is ONE sentence — `User: <principal> is not
//! authorized to perform: <action> on resource: <resource>` — which ENDS at the
//! resource token: no clause names a document, because no document was reached to
//! withhold the grant.
//!
//! The rows differ along what the rule generalises over: which operation answers
//! (reads, a listing, a create under the cluster, a delete, an update, a topic
//! data-plane call), which IAM action it is authorized under (`kafka:<ApiName>` and
//! the data plane's `kafka-cluster:Connect`), how the target travels (an
//! `@httpLabel`, a body member, a query parameter), which reading the submitted value
//! falls under, and — for reading 3 — which kind of resource the ARN names (another of
//! this service's, and another vendor's).

use serde_json::{json, Value};

use service_com_amazonaws_kafka_kafka::state::authorization::caller_principal;
use service_com_amazonaws_kafka_kafka::state::{BAD_REQUEST_MESSAGE, CLUSTER_ARN_MEMBER};

use crate::kafka_harness::{pct, Emulator, Reply, ACCOUNT, REGION};

/// The status and code an authorization refusal carries, spelled out here rather
/// than read from the service, so a drift in its own constants fails this suite.
const ACCESS_DENIED_STATUS: u16 = 403;
const ACCESS_DENIED_CODE: &str = "AccessDeniedException";

/// The resource an authorization decision is taken over when the target referenced
/// none: IAM's wildcard.
const ANY_RESOURCE: &str = "*";

/// A cluster id of the form this service mints, for a reference that is well formed
/// in every part but names a cluster no registry holds.
const GHOST_ID: &str = "11111111-2222-4333-8444-555555555555-1";

/// The whole sentence the gate refuses with: the principal that signed, the action
/// asked for and the resource the decision was taken over, ending there.
fn not_authorized(action: &str, resource: &str) -> String {
    format!(
        "User: {} is not authorized to perform: {action} on resource: {resource}",
        caller_principal()
    )
}

/// Where a call carries the target the decision is taken over.
#[derive(Debug, Clone)]
enum Target {
    /// `{base}/{target}{suffix}` — the target is an `@httpLabel`.
    Label {
        base: &'static str,
        suffix: &'static str,
    },
    /// The target travels in the request body, under `member`.
    Body {
        uri: &'static str,
        member: &'static str,
    },
    /// The target travels as a query parameter.
    Query {
        uri: &'static str,
        parameter: &'static str,
    },
}

/// One cluster-scoped call: how the model binds it, the body it carries, the IAM
/// action it is authorized under, and whether that action is one only a cluster's own
/// policy can grant.
#[derive(Debug, Clone)]
struct Call {
    case: &'static str,
    method: &'static str,
    target: Target,
    body: Option<Value>,
    action: String,
    only_a_cluster_policy_can_grant: bool,
}

impl Call {
    /// A control-plane call authorized under `kafka:<ApiName>`.
    fn control(
        case: &'static str,
        method: &'static str,
        base: &'static str,
        suffix: &'static str,
        body: Option<Value>,
    ) -> Self {
        Self {
            case,
            method,
            target: Target::Label { base, suffix },
            body,
            action: format!("kafka:{case}"),
            only_a_cluster_policy_can_grant: false,
        }
    }

    /// The same, for one of the four actions an MSK cluster POLICY governs.
    fn cluster_policy_action(
        case: &'static str,
        method: &'static str,
        base: &'static str,
        suffix: &'static str,
    ) -> Self {
        Self {
            only_a_cluster_policy_can_grant: true,
            ..Self::control(case, method, base, suffix, None)
        }
    }

    /// The request this call makes against the target `value`.
    fn request(&self, value: &str) -> (String, Option<Value>) {
        match &self.target {
            Target::Label { base, suffix } => {
                (format!("{base}/{}{suffix}", pct(value)), self.body.clone())
            }
            Target::Body { uri, member } => {
                let mut body = self.body.clone().unwrap_or_else(|| json!({}));
                body[*member] = json!(value);
                ((*uri).to_string(), Some(body))
            }
            Target::Query { uri, parameter } => (
                format!("{uri}?{parameter}={}", pct(value)),
                self.body.clone(),
            ),
        }
    }

    /// Make this call against `value`.
    async fn make(&self, emulator: &Emulator, value: &str) -> Reply {
        let (uri, body) = self.request(value);
        emulator.call(self.method, &uri, body).await
    }
}

/// A `CreateChannel` body carrying the two members the model requires, so the
/// boundary validator passes it and the gate — or the handler — is what answers.
fn channel_body() -> Value {
    json!({
        "channelName": "scope-channel",
        "topicConfigurationList": [{
            "recordConverter": { "valueConverter": "JSON" },
            "topicArn": format!("arn:aws:kafka:{REGION}:{ACCOUNT}:topic/channels/orders"),
        }],
    })
}

/// The calls the rule is driven over: reads on both URI families, a listing, a
/// create under the cluster, a delete, an update, a topic data-plane call, and the
/// two whose target travels outside the URI path.
fn calls() -> Vec<Call> {
    vec![
        Call::cluster_policy_action("DescribeCluster", "GET", "/v1/clusters", ""),
        Call::cluster_policy_action("DescribeClusterV2", "GET", "/api/v2/clusters", ""),
        Call::cluster_policy_action(
            "GetBootstrapBrokers",
            "GET",
            "/v1/clusters",
            "/bootstrap-brokers",
        ),
        Call::control("DeleteCluster", "DELETE", "/v1/clusters", "", None),
        Call::control("ListNodes", "GET", "/v1/clusters", "/nodes", None),
        Call::control("GetClusterPolicy", "GET", "/v1/clusters", "/policy", None),
        Call::control(
            "CreateChannel",
            "POST",
            "/v1/clusters",
            "/channels",
            Some(channel_body()),
        ),
        Call::control(
            "UpdateBrokerCount",
            "PUT",
            "/v1/clusters",
            "/nodes/count",
            Some(json!({
                "currentVersion": "K3AEGXETSR30VB",
                "targetNumberOfBrokerNodes": 4,
            })),
        ),
        Call {
            // A topic call reaches the cluster's Apache Kafka plane, so it is
            // authorized under that plane's own action.
            action: "kafka-cluster:Connect".to_string(),
            ..Call::control(
                "CreateTopic",
                "POST",
                "/v1/clusters",
                "/topics",
                Some(json!({
                    "topicName": "orders",
                    "partitionCount": 1,
                    "replicationFactor": 1,
                })),
            )
        },
        Call {
            target: Target::Query {
                uri: "/v1/compatible-kafka-versions",
                parameter: "clusterArn",
            },
            ..Call::control("GetCompatibleKafkaVersions", "GET", "", "", None)
        },
        Call {
            target: Target::Body {
                uri: "/v1/vpc-connection",
                member: "targetClusterArn",
            },
            body: Some(json!({
                "authentication": "SASL_IAM",
                "vpcId": "vpc-0a1b2c3d",
                "clientSubnets": ["subnet-0a1b2c3d"],
                "securityGroups": ["sg-0a1b2c3d"],
            })),
            only_a_cluster_policy_can_grant: true,
            ..Call::control("CreateVpcConnection", "POST", "", "", None)
        },
    ]
}

/// Every way a value can be a CLUSTER reference and still identify no cluster: the
/// resource field with its id missing, with one segment too many, with no id path at
/// all, and one whose Region segment is empty. Each is of the caller's own account,
/// so no row is refused on the account it names.
fn cluster_references_naming_no_cluster() -> Vec<(&'static str, String)> {
    vec![
        (
            "the resource field without its id",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/only-a-name"),
        ),
        (
            "the resource field with an extra segment",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/a-cluster/{GHOST_ID}/extra"),
        ),
        (
            "the resource type alone",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster"),
        ),
        (
            "an empty region segment",
            format!("arn:aws:kafka::{ACCOUNT}:cluster/a-cluster/{GHOST_ID}"),
        ),
        (
            "an empty account segment",
            format!("arn:aws:kafka:{REGION}::cluster/a-cluster/{GHOST_ID}"),
        ),
    ]
}

/// Every way a submitted value can be no ARN at all: there is nothing to resolve it
/// to, whatever its text.
fn values_that_are_no_arn() -> Vec<(&'static str, String)> {
    vec![
        ("free text", "not-an-arn".to_string()),
        ("free text with spaces", "not an arn at all".to_string()),
        ("a truncated ARN", "arn:aws:kafka".to_string()),
        ("a non-ASCII value", "ニセのクラスター-arn".to_string()),
        (
            "a long opaque value",
            format!("not-a-cluster-arn-{}", "x".repeat(200)),
        ),
    ]
}

/// RULE (reading 1): a cluster reference that identifies no cluster is refused by the
/// gate over the WILDCARD — one sentence, ending at that token, carrying neither the
/// submitted value nor a clause about a document — whichever operation answers, under
/// whichever IAM action, and whatever makes the reference identify no cluster.
#[tokio::test]
async fn a_cluster_reference_identifying_no_cluster_is_refused_over_the_wildcard() {
    let emulator = Emulator::new();

    for (value_case, value) in cluster_references_naming_no_cluster() {
        for call in calls() {
            let case = format!("{} / {value_case}", call.case);
            let reply = call.make(&emulator, &value).await;
            assert_eq!(
                (reply.status, reply.error_code()),
                (ACCESS_DENIED_STATUS, ACCESS_DENIED_CODE),
                "case {case:?}: expected the authorization refusal, got {} {}",
                reply.status,
                reply.body
            );
            assert_eq!(
                reply.error_message(),
                not_authorized(&call.action, ANY_RESOURCE),
                "case {case:?}: expected the refusal over the wildcard, got {:?}",
                reply.error_message()
            );
            assert!(
                !reply.error_message().contains(value.as_str()),
                "case {case:?}: the decision was taken over no resource, so the refusal must not \
                 echo the submitted value, got {:?}",
                reply.error_message()
            );
            assert!(
                !reply.error_message().contains("because"),
                "case {case:?}: no document was reached, so the sentence names none, got {:?}",
                reply.error_message()
            );
        }
    }
}

/// RULE (reading 2): a value that is no ARN at all stands for nothing a document
/// could be attached to, so for the four actions only a cluster's own policy can
/// grant the decision is taken over the VALUE AS SUBMITTED and the refusal names it
/// verbatim — again one sentence, with no clause — while for the actions an
/// identity-based document could grant there is nothing to decide and the operation
/// answers the parameter instead.
#[tokio::test]
async fn a_value_that_is_no_arn_is_decided_over_itself_or_left_to_the_operation() {
    let emulator = Emulator::new();

    for (value_case, value) in values_that_are_no_arn() {
        for call in calls() {
            let case = format!("{} / {value_case}", call.case);
            let reply = call.make(&emulator, &value).await;
            if !call.only_a_cluster_policy_can_grant {
                assert_eq!(
                    (reply.status, reply.error_code()),
                    (400, "BadRequestException"),
                    "case {case:?}: an identity-based document could grant this action, so the \
                     gate decides nothing over a value that is no ARN and the operation answers \
                     the parameter, got {} {}",
                    reply.status,
                    reply.body
                );
                assert!(
                    !reply.error_message().contains("is not authorized to perform"),
                    "case {case:?}: the operation's own refusal carries no authorization \
                     sentence, got {:?}",
                    reply.error_message()
                );
                continue;
            }
            assert_eq!(
                (reply.status, reply.error_code()),
                (ACCESS_DENIED_STATUS, ACCESS_DENIED_CODE),
                "case {case:?}: expected the authorization refusal, got {} {}",
                reply.status,
                reply.body
            );
            assert_eq!(
                reply.error_message(),
                not_authorized(&call.action, &value),
                "case {case:?}: expected the refusal over the submitted value, got {:?}",
                reply.error_message()
            );
            assert!(
                !reply.error_message().contains("because"),
                "case {case:?}: no document was reached, so the sentence names none, got {:?}",
                reply.error_message()
            );
        }
    }
}

/// The ARNs of ANOTHER resource kind the readings below are driven over, built around
/// the live configuration `configuration` this endpoint really holds: other kinds of
/// this service's own resources, and another vendor's.
fn arns_of_another_kind(configuration: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "a configuration this endpoint holds",
            configuration.to_string(),
        ),
        (
            "a configuration never minted",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:configuration/never-minted/{GHOST_ID}"),
        ),
        (
            "a replicator of this service",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:replicator/a-replicator/{GHOST_ID}"),
        ),
        (
            "a VPC connection of this service",
            format!(
                "arn:aws:kafka:{REGION}:{ACCOUNT}:vpc-connection/{ACCOUNT}/a-cluster/{GHOST_ID}"
            ),
        ),
        (
            "a cluster-operation of this service",
            format!(
                "arn:aws:kafka:{REGION}:{ACCOUNT}:cluster-operation/a-cluster/{GHOST_ID}/\
                 99999999-8888-4777-8666-555555555555"
            ),
        ),
        (
            "another vendor's resource",
            format!("arn:aws:ec2:{REGION}:{ACCOUNT}:instance/i-0a1b2c3d"),
        ),
    ]
}

/// RULE (reading 3): an ARN naming a resource of another KIND is no cluster
/// reference, so the gate decides nothing about it and the operation answers —
/// never with the authorization refusal, whichever operation, under whichever
/// action, and however the target travels.
#[tokio::test]
async fn an_arn_of_another_resource_kind_is_never_the_gates_answer() {
    let emulator = Emulator::new();
    let configuration = emulator
        .create_configuration_ok(
            "scope-configuration",
            None,
            b"auto.create.topics.enable=true",
        )
        .await;

    for (value_case, value) in arns_of_another_kind(&configuration) {
        for call in calls() {
            let case = format!("{} / {value_case}", call.case);
            let reply = call.make(&emulator, &value).await;
            assert_eq!(
                (reply.status, reply.error_code()),
                (400, "BadRequestException"),
                "case {case:?}: an ARN of another resource kind is no reference the gate decides \
                 over, so the operation answers the parameter, got {} {}",
                reply.status,
                reply.body
            );
            assert!(
                !reply.error_message().contains("is not authorized to perform"),
                "case {case:?}: the operation's own refusal carries no authorization sentence, \
                 got {:?}",
                reply.error_message()
            );
        }
    }

    // And the resource those ARNs really name is untouched by the refusals: the
    // configuration this endpoint holds still reads back through its own reader.
    let described = emulator.describe_configuration(&configuration).await;
    assert_eq!(
        (described.status, described.at("/arn").as_str()),
        (200, Some(configuration.as_str())),
        "case control: the configuration those refusals named must still be served, got {} {}",
        described.status,
        described.body
    );
    assert_eq!(
        described.at("/name").as_str(),
        Some("scope-configuration"),
        "case control: expected the reader to echo the configuration's name, got {}",
        described.body
    );
}

/// RULE (reading 3, the delete's own answer): handed an ARN of another resource kind
/// as its `ClusterArn`, the cluster DELETE answers the parameter it cannot read —
/// HTTP 400 `BadRequestException` with the service's fixed parameter sentence as the
/// WHOLE message and the input that surface blames in `invalidParameter` — so a
/// caller pointed at the wrong resource is told to fix the request rather than its
/// permissions. Driven over every kind of ARN that is not a cluster's, with and
/// without the optional version pre-condition.
#[tokio::test]
async fn the_cluster_delete_refuses_an_arn_of_another_kind_as_a_parameter() {
    let emulator = Emulator::new();
    let configuration = emulator
        .create_configuration_ok(
            "delete-scope-configuration",
            None,
            b"auto.create.topics.enable=true",
        )
        .await;

    // The version pre-condition is optional, and which input the refusal blames
    // follows it: a request that quoted a version asked for a check this reference
    // can never satisfy, and one that quoted none is told about the reference.
    let probes: [(Option<&str>, &str); 3] = [
        (None, CLUSTER_ARN_MEMBER),
        (Some("K3AEGXETSR30VB"), "currentVersion"),
        (Some("not-a-version"), "currentVersion"),
    ];

    for (value_case, value) in arns_of_another_kind(&configuration) {
        for (version, blamed) in probes {
            let case = format!("{value_case} / currentVersion={version:?}");
            let uri = match version {
                None => format!("/v1/clusters/{}", pct(&value)),
                Some(version) => {
                    format!("/v1/clusters/{}?currentVersion={}", pct(&value), pct(version))
                }
            };
            let reply = emulator.call("DELETE", &uri, None).await;
            assert_eq!(
                (reply.status, reply.error_code()),
                (400, "BadRequestException"),
                "case {case:?}: expected the parameter refusal, got {} {}",
                reply.status,
                reply.body
            );
            assert_eq!(
                reply.error_message(),
                BAD_REQUEST_MESSAGE,
                "case {case:?}: expected the fixed parameter sentence, got {:?}",
                reply.error_message()
            );
            assert_eq!(
                reply.at("/invalidParameter").as_str(),
                Some(blamed),
                "case {case:?}: expected the refusal to name {blamed}, got {}",
                reply.body
            );
            assert!(
                !reply.error_message().contains(value.as_str()),
                "case {case:?}: the sentence is fixed, so the submitted reference must not reach \
                 it, got {:?}",
                reply.error_message()
            );
        }
    }

    // Nothing was deleted by any of it: the configuration those deletes were
    // pointed at is still there, and still carries its name.
    let described = emulator.describe_configuration(&configuration).await;
    assert_eq!(
        (
            described.status,
            described.at("/arn").as_str(),
            described.at("/name").as_str(),
        ),
        (
            200,
            Some(configuration.as_str()),
            Some("delete-scope-configuration")
        ),
        "case control: a refused delete must leave the resource it named alone, got {} {}",
        described.status,
        described.body
    );
}
