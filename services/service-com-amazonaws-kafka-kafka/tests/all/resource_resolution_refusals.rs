// SPDX-License-Identifier: CC0-1.0

//! How Amazon MSK refuses a request whose RESOURCE REFERENCE it cannot act on,
//! driven end to end over the same router the emulator serves from (the shared
//! `kafka_harness`).
//!
//! RULES under test, each asserted over a table whose rows differ along what the
//! rule generalises over — which operation, which member carried the reference,
//! which kind of resource it names, and how the reference fails to resolve:
//!
//! 1. A reference this endpoint resolves no resource for is ONE refusal: HTTP 404
//!    `NotFoundException` whose WHOLE message is the service's single fixed
//!    sentence, with the request member at fault carried in `invalidParameter` —
//!    by the camelCase wire name the request itself uses — and nothing of the
//!    request or of this endpoint's own state (no ARN, no region, no account) in
//!    the text. It holds for the operation that CREATES a resource against
//!    another one too: `CreateVpcConnection` cannot connect to a cluster that is
//!    not there, which is a missing resource and not a malformed body.
//! 2. A reference is resolved BEFORE the rest of the body is read, so a request
//!    that both names a resource this endpoint holds none of and carries a member
//!    the service would not accept is answered about the RESOURCE.
//! 3. The tagging operations take the ARN of a resource of a kind this service
//!    TAGS; a `ResourceArn` naming any other kind is the caller's own input to
//!    fix — HTTP 400 `BadRequestException` in one fixed sentence — whether or not
//!    that resource exists, while a well-formed ARN of a taggable kind that names
//!    nothing stays the modelled 404.
//! 4. The list of Kafka clusters a replicator spans is a SET, so a list naming one
//!    cluster twice is refused on that member in the service's fixed sentence for
//!    it.

use serde_json::{json, Value};

// The service's own wire member names and fixed sentences, so what the handlers
// raise and what this suite pins cannot drift apart silently. The texts
// themselves are written out below rather than imported, so an assertion cannot
// agree with a constant that is wrong.
use service_com_amazonaws_kafka_kafka::state::TAGGABLE_RESOURCE_TYPES;

use crate::kafka_harness::{pct, ClusterRequest, Emulator, Reply, ACCOUNT, REGION};

/// The whole message Amazon MSK answers a resource reference it resolves nothing
/// for with (the apostrophe is U+2019, as the service writes it): one fixed
/// sentence, which is why every row below asserts it EQUAL rather than contained.
const RESOURCE_NOT_FOUND: &str = "The requested resource doesn\u{2019}t exist.";

/// The whole message the tagging operations answer a `ResourceArn` they cannot
/// accept as the ARN of a taggable MSK resource with. Also fixed end to end.
const INVALID_TAG_RESOURCE_ARN: &str = "Invalid input resource arn";

/// The whole message `CreateReplicator` answers a cluster list that names one
/// cluster twice with.
const DUPLICATE_KAFKA_CLUSTERS: &str = "Kafka cluster list contains duplicate cluster ARNs. \
                                        Please ensure all the cluster ARNs are unique.";

/// The service-execution role a `CreateReplicator` request passes.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/msk-replicator";

/// One Secrets Manager secret ARN of the form `SecretArnList` carries.
const GOOD_SECRET: &str =
    "arn:aws:secretsmanager:us-west-2:000000000000:secret:AmazonMSK_resolution-1a2b3c";

/// A well-formed cluster ARN of this account and region that names no cluster: a
/// reference the service can read and resolve nothing for, which is what every
/// not-found row is about.
fn ghost_cluster(name: &str) -> String {
    format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/{name}/00000000-0000-0000-0000-000000000000-1")
}

/// The same, for a replicator.
fn ghost_replicator(name: &str) -> String {
    format!(
        "arn:aws:kafka:{REGION}:{ACCOUNT}:replicator/{name}/00000000-0000-0000-0000-000000000000-1"
    )
}

/// Assert `reply` is the service's one not-found refusal, naming `member` as the
/// parameter at fault and carrying NOTHING of `reference` — the reference itself,
/// the region or the account — in its message.
fn assert_not_found(reply: &Reply, case: &str, member: &str, reference: &str) {
    assert_eq!(
        (reply.status, reply.error_code()),
        (404, "NotFoundException"),
        "case {case}: expected 404 NotFoundException, got {} {}",
        reply.status,
        reply.body
    );
    assert_eq!(
        reply.error_message(),
        RESOURCE_NOT_FOUND,
        "case {case}: expected the service's one not-found sentence, got {:?}",
        reply.error_message()
    );
    for leaked in [reference, REGION, ACCOUNT] {
        assert!(
            !reply.error_message().contains(leaked),
            "case {case}: the message must carry no part of the request ({leaked:?}), got {:?}",
            reply.error_message()
        );
    }
    assert_eq!(
        reply.at("/invalidParameter").as_str(),
        Some(member),
        "case {case}: expected the refusal to name {member} at fault, got {}",
        reply.body
    );
}

/// A `CreateVpcConnection` body against `cluster`, every other member usable, so
/// a refusal of it is about the cluster alone.
fn vpc_connection_body(cluster: &str) -> Value {
    json!({
        "targetClusterArn": cluster,
        "authentication": "SASL_IAM",
        "vpcId": "vpc-0a1b2c3d",
        "clientSubnets": ["subnet-0a1b2c3d", "subnet-0a1b2c3e"],
        "securityGroups": ["sg-0a1b2c3d"],
    })
}

/// One `KafkaClusters` entry naming an Amazon MSK cluster by ARN, with the client
/// VPC configuration every entry carries.
fn msk_cluster(arn: &str) -> Value {
    json!({
        "amazonMskCluster": { "mskClusterArn": arn },
        "vpcConfig": { "subnetIds": ["subnet-0a1b2c3d", "subnet-0a1b2c3e"] },
    })
}

/// One row of the not-found table: the operation, how the request reaches it, the
/// reference it carries and the member the refusal must name.
struct Row {
    case: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    member: &'static str,
    reference: String,
}

/// RULE 1: every operation that resolves a resource REFERENCE answers a reference
/// this endpoint holds nothing for with the same 404 `NotFoundException`, the same
/// one fixed sentence, and the member that carried the reference named by its wire
/// name.
#[tokio::test]
async fn an_unresolvable_resource_reference_is_one_fixed_not_found_refusal() {
    let emulator = Emulator::new();
    let cluster = ghost_cluster("resolution-ghost");
    let replicator = ghost_replicator("resolution-ghost");

    /// One row, spelled out once so the table below reads as data.
    fn row(
        case: &'static str,
        method: &'static str,
        uri: String,
        body: Option<Value>,
        member: &'static str,
        reference: &str,
    ) -> Row {
        Row {
            case,
            method,
            uri,
            body,
            member,
            reference: reference.to_string(),
        }
    }

    // The rows span the surfaces that resolve a reference: both cluster readers,
    // the cluster sub-resource readers, the replicator reader and its delete, the
    // two SCRAM batch writers, and the VPC-connection create — which names its
    // cluster in a BODY member rather than in the path, and whose own model lists
    // no not-found error, yet answers the service's one anyway.
    let rows: Vec<Row> = vec![
        row(
            "DescribeCluster",
            "GET",
            format!("/v1/clusters/{}", pct(&cluster)),
            None,
            "clusterArn",
            &cluster,
        ),
        row(
            "DescribeClusterV2",
            "GET",
            format!("/api/v2/clusters/{}", pct(&cluster)),
            None,
            "clusterArn",
            &cluster,
        ),
        row(
            "ListNodes",
            "GET",
            format!("/v1/clusters/{}/nodes", pct(&cluster)),
            None,
            "clusterArn",
            &cluster,
        ),
        row(
            "ListScramSecrets",
            "GET",
            format!("/v1/clusters/{}/scram-secrets", pct(&cluster)),
            None,
            "clusterArn",
            &cluster,
        ),
        row(
            "GetBootstrapBrokers",
            "GET",
            format!("/v1/clusters/{}/bootstrap-brokers", pct(&cluster)),
            None,
            "clusterArn",
            &cluster,
        ),
        row(
            "BatchAssociateScramSecret",
            "POST",
            format!("/v1/clusters/{}/scram-secrets", pct(&cluster)),
            Some(json!({ "secretArnList": [GOOD_SECRET] })),
            "clusterArn",
            &cluster,
        ),
        row(
            "BatchDisassociateScramSecret",
            "PATCH",
            format!("/v1/clusters/{}/scram-secrets", pct(&cluster)),
            Some(json!({ "secretArnList": [GOOD_SECRET] })),
            "clusterArn",
            &cluster,
        ),
        row(
            "DescribeReplicator",
            "GET",
            format!("/replication/v1/replicators/{}", pct(&replicator)),
            None,
            "replicatorArn",
            &replicator,
        ),
        row(
            "DeleteReplicator",
            "DELETE",
            format!("/replication/v1/replicators/{}", pct(&replicator)),
            None,
            "replicatorArn",
            &replicator,
        ),
        row(
            "CreateVpcConnection",
            "POST",
            "/v1/vpc-connection".to_string(),
            Some(vpc_connection_body(&cluster)),
            "clusterArn",
            &cluster,
        ),
    ];

    for row in rows {
        let reply = emulator.call(row.method, &row.uri, row.body).await;
        assert_not_found(&reply, row.case, row.member, &row.reference);
    }
}

/// RULE 1, the replicator surface's own wording: the replicator APIs tell the two
/// absences apart. A replicator this region HELD and released — one that was
/// deleted — is reported as a fault of the parameter that still names it, in the
/// `Invalid parameter <member>` framing those APIs use before the same fixed
/// sentence; an ARN this region never minted is the bare sentence. Both are the
/// same 404 `NotFoundException`, both name `replicatorArn` in
/// `invalidParameter`, and neither carries anything of the request.
#[tokio::test]
async fn a_released_replicator_is_told_apart_from_one_that_never_existed() {
    let emulator = Emulator::new();
    let one = emulator
        .create_ok(&ClusterRequest::named("released-one"))
        .await;
    let two = emulator
        .create_ok(&ClusterRequest::named("released-two"))
        .await;
    let created = emulator
        .call(
            "POST",
            "/replication/v1/replicators",
            Some(json!({
                "replicatorName": "released-replicator",
                "serviceExecutionRoleArn": ROLE_ARN,
                "kafkaClusters": [msk_cluster(&one), msk_cluster(&two)],
                "replicationInfoList": [{
                    "sourceKafkaClusterArn": one,
                    "targetKafkaClusterArn": two,
                    "targetCompressionType": "NONE",
                    "consumerGroupReplication": { "consumerGroupsToReplicate": [".*"] },
                    "topicReplication": { "topicsToReplicate": [".*"] },
                }],
            })),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "expected the replicator this suite releases to be created, got {} {}",
        created.status, created.body
    );
    let held = created
        .at("/replicatorArn")
        .as_str()
        .expect("CreateReplicator reported no replicatorArn")
        .to_string();
    let deleted = emulator
        .call(
            "DELETE",
            &format!("/replication/v1/replicators/{}", pct(&held)),
            None,
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "expected DeleteReplicator to answer 200, got {} {}",
        deleted.status, deleted.body
    );

    // The parameter-framed sentence the replicator APIs answer for a reference of
    // their own that is gone: fixed text around the member's wire name, with no
    // part of the ARN in it.
    let released_message = format!("Invalid parameter replicatorArn: {RESOURCE_NOT_FOUND}");

    // The rows differ along which operation addresses the released replicator —
    // its reader, its delete and the replication update, which is every operation
    // that resolves a replicator by ARN.
    for (case, method, uri, body) in [
        (
            "DescribeReplicator",
            "GET",
            format!("/replication/v1/replicators/{}", pct(&held)),
            None,
        ),
        (
            "DeleteReplicator",
            "DELETE",
            format!("/replication/v1/replicators/{}", pct(&held)),
            None,
        ),
        (
            "UpdateReplicationInfo",
            "PUT",
            format!(
                "/replication/v1/replicators/{}/replication-info",
                pct(&held)
            ),
            Some(json!({
                "currentVersion": "K2EUQ1WTGCTBG2",
                "sourceKafkaClusterArn": one,
                "targetKafkaClusterArn": two,
                "topicReplication": {
                    "topicsToReplicate": [".*"],
                    "topicsToExclude": [],
                    "detectAndCopyNewTopics": true,
                    "copyTopicConfigurations": true,
                    "copyAccessControlListsForTopics": false,
                },
            })),
        ),
    ] {
        let reply = emulator.call(method, &uri, body).await;
        assert_eq!(
            (reply.status, reply.error_code(), reply.error_message()),
            (404, "NotFoundException", released_message.as_str()),
            "case {case} on a released replicator: expected the parameter-framed refusal, got \
             {} {}",
            reply.status,
            reply.body
        );
        assert_eq!(
            reply.at("/invalidParameter").as_str(),
            Some("replicatorArn"),
            "case {case} on a released replicator: expected the refusal to name replicatorArn, \
             got {}",
            reply.body
        );
        assert!(
            !reply.error_message().contains(held.as_str()),
            "case {case}: the message must carry no part of the ARN, got {:?}",
            reply.error_message()
        );
    }

    // An ARN of the same shape this region never minted is the OTHER absence: the
    // bare sentence, from the same operations.
    let never = ghost_replicator("released-never-minted");
    for (case, method, uri) in [
        (
            "DescribeReplicator",
            "GET",
            format!("/replication/v1/replicators/{}", pct(&never)),
        ),
        (
            "DeleteReplicator",
            "DELETE",
            format!("/replication/v1/replicators/{}", pct(&never)),
        ),
    ] {
        let reply = emulator.call(method, &uri, None).await;
        assert_not_found(
            &reply,
            &format!("{case} on a never-minted ARN"),
            "replicatorArn",
            &never,
        );
    }
}

/// RULE 2: the resource a call acts on is resolved BEFORE the rest of the body is
/// read, so a request that both names a cluster this endpoint holds none of and
/// carries a `SecretArnList` entry the service would not accept is answered about
/// the CLUSTER. The same request against a cluster that IS there is answered about
/// the entry, which is what makes this a gate ORDER rather than a blanket answer.
#[tokio::test]
async fn a_scram_batch_answers_the_cluster_miss_before_the_secret_list() {
    let emulator = Emulator::new();
    let ghost = ghost_cluster("scram-order-ghost");
    let live = emulator
        .create_ok(&ClusterRequest {
            sasl_scram: Some(true),
            ..ClusterRequest::named("scram-order-live")
        })
        .await;

    // The rows differ along HOW the secret list is unusable — not an ARN at all,
    // another service's ARN, an empty entry, and an unusable entry behind a usable
    // one — and along which batch operation is asked for.
    let lists: Vec<(&str, Value)> = vec![
        ("not-an-arn", json!(["not-a-secret-arn"])),
        (
            "another-service",
            json!(["arn:aws:s3:::a-bucket/a-key"]),
        ),
        ("empty-entry", json!([""])),
        (
            "usable-then-unusable",
            json!([GOOD_SECRET, "not-a-secret-arn"]),
        ),
    ];

    for (list_case, secrets) in &lists {
        for (operation, method) in [
            ("BatchAssociateScramSecret", "POST"),
            ("BatchDisassociateScramSecret", "PATCH"),
        ] {
            let case = format!("{operation} with {list_case} against an absent cluster");
            let reply = emulator
                .call(
                    method,
                    &format!("/v1/clusters/{}/scram-secrets", pct(&ghost)),
                    Some(json!({ "secretArnList": secrets })),
                )
                .await;
            assert_not_found(&reply, &case, "clusterArn", &ghost);
        }

        // The same list against a cluster that IS there: the association reads the
        // entries, so this one is the parameter refusal naming the list.
        let case = format!("BatchAssociateScramSecret with {list_case} against a live cluster");
        let reply = emulator
            .call(
                "POST",
                &format!("/v1/clusters/{}/scram-secrets", pct(&live)),
                Some(json!({ "secretArnList": secrets })),
            )
            .await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (400, "BadRequestException"),
            "case {case}: expected the parameter refusal once the cluster resolves, got {} {}",
            reply.status,
            reply.body
        );
        assert_eq!(
            reply.at("/invalidParameter").as_str(),
            Some("secretArnList"),
            "case {case}: expected the refusal to name secretArnList, got {}",
            reply.body
        );
    }
}

/// RULE 3: the tagging operations serve the resource kinds this service TAGS; an
/// ARN of any other kind is the caller's own input to fix, whether or not the
/// resource it names exists, while a well-formed ARN of a taggable kind that names
/// nothing is the modelled miss.
#[tokio::test]
async fn the_tagging_operations_refuse_an_arn_of_a_kind_they_do_not_tag() {
    let emulator = Emulator::new();

    // A configuration that EXISTS: no API of this service gives a configuration a
    // `Tags` member, so its own ARN is not a tagging target even though the
    // resource is there.
    let configuration = emulator
        .create_configuration_ok("resolution-untaggable", None, b"num.partitions=1\n")
        .await;
    // A cluster that exists, to show the same operations do serve a kind this
    // service tags.
    let cluster = emulator
        .create_ok(&ClusterRequest::named("resolution-taggable"))
        .await;

    // The kinds the service tags are the ones whose API gives the resource a
    // `Tags` member; a configuration and a cluster operation are not among them.
    for untaggable in ["configuration", "cluster-operation"] {
        assert!(
            !TAGGABLE_RESOURCE_TYPES.contains(&untaggable),
            "case {untaggable}: the service must tag no resource of this kind, got \
             {TAGGABLE_RESOURCE_TYPES:?}"
        );
    }

    // Rows: the live configuration's own ARN, a cluster-operation ARN, an ARN of
    // another vendor's service, and a value that is no ARN at all — every one of
    // them a `ResourceArn` this surface cannot act on.
    let refused: Vec<(&str, String)> = vec![
        ("a live configuration", configuration.clone()),
        (
            "a cluster operation",
            format!(
                "arn:aws:kafka:{REGION}:{ACCOUNT}:cluster-operation/resolution/\
                 00000000-0000-0000-0000-000000000000-1"
            ),
        ),
        (
            "another service's ARN",
            format!("arn:aws:sqs:{REGION}:{ACCOUNT}:queue/resolution"),
        ),
        ("not an ARN at all", "resolution-not-an-arn".to_string()),
    ];

    for (case, arn) in &refused {
        for (operation, method, uri, body) in [
            (
                "TagResource",
                "POST",
                format!("/v1/tags/{}", pct(arn)),
                Some(json!({ "tags": { "env": "prod" } })),
            ),
            (
                "UntagResource",
                "DELETE",
                format!("/v1/tags/{}?tagKeys=env", pct(arn)),
                None,
            ),
            (
                "ListTagsForResource",
                "GET",
                format!("/v1/tags/{}", pct(arn)),
                None,
            ),
        ] {
            let case = format!("{operation} on {case}");
            let reply = emulator.call(method, &uri, body).await;
            assert_eq!(
                (reply.status, reply.error_code(), reply.error_message()),
                (400, "BadRequestException", INVALID_TAG_RESOURCE_ARN),
                "case {case}: expected the fixed input refusal, got {} {}",
                reply.status,
                reply.body
            );
            assert!(
                !reply.error_message().contains(arn.as_str()),
                "case {case}: the message must carry no part of the value it refused, got {:?}",
                reply.error_message()
            );
        }
    }

    // A well-formed ARN of a TAGGABLE kind that names nothing is the other
    // refusal: the modelled 404, not this input error.
    let absent = ghost_cluster("resolution-absent");
    let reply = emulator
        .call("GET", &format!("/v1/tags/{}", pct(&absent)), None)
        .await;
    assert_eq!(
        (reply.status, reply.error_code()),
        (404, "NotFoundException"),
        "case an absent cluster: expected the modelled miss, got {} {}",
        reply.status,
        reply.body
    );

    // And the live cluster is served: the refusals above are about the ARN's kind,
    // not about the surface.
    let tagged = emulator
        .call(
            "POST",
            &format!("/v1/tags/{}", pct(&cluster)),
            Some(json!({ "tags": { "env": "prod" } })),
        )
        .await;
    assert_eq!(
        tagged.status, 204,
        "expected TagResource on a cluster to answer 204, got {} {}",
        tagged.status, tagged.body
    );
    let listed = emulator
        .call("GET", &format!("/v1/tags/{}", pct(&cluster)), None)
        .await;
    assert_eq!(
        listed.at("/tags"),
        &json!({ "env": "prod" }),
        "expected the stored tag to be reported back, got {}",
        listed.body
    );
}

/// RULE 4: the Kafka clusters a replicator spans are a SET, so a `KafkaClusters`
/// list naming one cluster twice is refused on that member, in the service's fixed
/// sentence for it, and registers nothing.
#[tokio::test]
async fn a_cluster_list_that_names_one_cluster_twice_is_refused_on_that_member() {
    let emulator = Emulator::new();
    let one = emulator
        .create_ok(&ClusterRequest::named("duplicate-one"))
        .await;
    let two = emulator
        .create_ok(&ClusterRequest::named("duplicate-two"))
        .await;

    /// A self-managed Apache Kafka cluster entry, named by the id its caller gave
    /// it.
    fn apache(id: &str) -> Value {
        json!({
            "apacheKafkaCluster": {
                "apacheKafkaClusterId": id,
                "bootstrapBrokerString": format!("b-1.{id}.example.internal:9092"),
            },
            "vpcConfig": { "subnetIds": ["subnet-0a1b2c3d", "subnet-0a1b2c3e"] },
        })
    }

    // The rows differ along HOW the list repeats a cluster: the same MSK ARN
    // twice, the same self-managed Apache Kafka cluster twice, and a flow whose
    // two ends name one of the clusters — the one condition, stated about the one
    // member.
    let rows: Vec<(&str, Vec<Value>, Value)> = vec![
        (
            "the same MSK cluster twice",
            vec![msk_cluster(&one), msk_cluster(&one)],
            json!({
                "sourceKafkaClusterArn": one,
                "targetKafkaClusterArn": one,
                "targetCompressionType": "NONE",
                "consumerGroupReplication": { "consumerGroupsToReplicate": [".*"] },
                "topicReplication": { "topicsToReplicate": [".*"] },
            }),
        ),
        (
            "the same Apache Kafka cluster twice",
            vec![apache("broker-one"), apache("broker-one")],
            json!({
                "sourceKafkaClusterId": "broker-one",
                "targetKafkaClusterId": "broker-one",
                "targetCompressionType": "NONE",
                "consumerGroupReplication": { "consumerGroupsToReplicate": [".*"] },
                "topicReplication": { "topicsToReplicate": [".*"] },
            }),
        ),
        (
            "a flow between one of two distinct clusters",
            vec![msk_cluster(&one), msk_cluster(&two)],
            json!({
                "sourceKafkaClusterArn": two,
                "targetKafkaClusterArn": two,
                "targetCompressionType": "NONE",
                "consumerGroupReplication": { "consumerGroupsToReplicate": [".*"] },
                "topicReplication": { "topicsToReplicate": [".*"] },
            }),
        ),
    ];

    for (index, (case, clusters, flow)) in rows.into_iter().enumerate() {
        let reply = emulator
            .call(
                "POST",
                "/replication/v1/replicators",
                Some(json!({
                    "replicatorName": format!("duplicate-clusters-{index}"),
                    "serviceExecutionRoleArn": ROLE_ARN,
                    "kafkaClusters": clusters,
                    "replicationInfoList": [flow],
                })),
            )
            .await;
        assert_eq!(
            (reply.status, reply.error_code(), reply.error_message()),
            (400, "BadRequestException", DUPLICATE_KAFKA_CLUSTERS),
            "case {case}: expected the fixed duplicate-cluster refusal, got {} {}",
            reply.status,
            reply.body
        );
        assert_eq!(
            reply.at("/invalidParameter").as_str(),
            Some("kafkaClusters"),
            "case {case}: expected the refusal to name kafkaClusters, got {}",
            reply.body
        );
        // Nothing was registered: the listing is still empty.
        let listed = emulator
            .call("GET", "/replication/v1/replicators", None)
            .await;
        assert_eq!(
            listed.at("/replicators").as_array().map(Vec::len),
            Some(0),
            "case {case}: a refused create must register no replicator, got {}",
            listed.body
        );
    }

    // The apache-cluster helper keeps the rows above honest about what an entry
    // is: a cluster named by id is one too, and a list naming two DISTINCT
    // clusters is accepted.
    let accepted = emulator
        .call(
            "POST",
            "/replication/v1/replicators",
            Some(json!({
                "replicatorName": "duplicate-clusters-distinct",
                "serviceExecutionRoleArn": ROLE_ARN,
                "kafkaClusters": [msk_cluster(&one), msk_cluster(&two)],
                "replicationInfoList": [{
                    "sourceKafkaClusterArn": one,
                    "targetKafkaClusterArn": two,
                    "targetCompressionType": "NONE",
                    "consumerGroupReplication": { "consumerGroupsToReplicate": [".*"] },
                    "topicReplication": { "topicsToReplicate": [".*"] },
                }],
            })),
        )
        .await;
    assert_eq!(
        accepted.status, 200,
        "expected a list of two distinct clusters to be accepted, got {} {}",
        accepted.status, accepted.body
    );
}
