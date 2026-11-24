// SPDX-License-Identifier: CC0-1.0

//! The ASYNCHRONOUS Amazon MSK lifecycles a client drives by POLLING, over the services
//! that own the resources those lifecycles depend on: EC2 owns the subnets a cluster and a
//! replicator are placed in, and IAM owns the role MSK assumes to provision a replicator.
//!
//! RULE: every asynchronous state these calls walk reaches its TERMINAL value on its own,
//! so a client that polls a reader reaches that value within a bounded wait — a created
//! cluster reaches `ACTIVE`, an accepted update's cluster operation reaches
//! `UPDATE_COMPLETE` with the cluster's `CurrentVersion` advanced, and a replicator over
//! two such clusters reaches `RUNNING` when the role it was handed is one IAM holds and
//! MSK may assume, or `FAILED` when it is not. Until the resource settles, the call that
//! CHANGES it is refused with Amazon MSK's own fixed sentence for a resource that is not
//! in a state the call is accepted from, and the same call is ACCEPTED once it has.
//!
//! Three crates have to agree for that to hold — EC2 holds the subnets every create here
//! names, IAM answers for the service-execution role, and MSK owns the clusters, the
//! cluster operations and the replicator — so the suite lives here. The kafka crate's own
//! suites assert the spans and the sentences against an emulator with neither mounted;
//! what this one adds is that the convergence still happens when the neighbouring
//! services ARE there to be asked.
//!
//! The rows differ along what the rule generalises over: the trust policy the role the
//! create passes carries (one that admits MSK, one that admits another service only), and
//! so the terminal state the clock owes the replicator.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The EC2 API version an ec2Query form body names.
const EC2_API_VERSION: &str = "2016-11-15";

/// The IAM API version an awsQuery form body names.
const IAM_API_VERSION: &str = "2010-05-08";

/// The Apache Kafka version and broker type every cluster below is created with: a version
/// Amazon MSK still takes for a new cluster, since every create has to be admitted.
const KAFKA_VERSION: &str = "3.9.x";
const INSTANCE_TYPE: &str = "kafka.m5.large";

/// The service principal Amazon MSK assumes a service-execution role as.
const MSK_SERVICE_PRINCIPAL: &str = "kafka.amazonaws.com";

/// How Amazon MSK refuses a call that changes a cluster which is not `ACTIVE`, and a
/// replicator that is not `RUNNING`: fixed text, with only the member named at fault and
/// the list of states the call IS accepted from varying. Written out as MSK words them
/// rather than built from the service's own constants, so this suite pins the wording.
const CLUSTER_STATE_REFUSAL: &str =
    "This operation is only valid for resources that are in one of the following states :[ACTIVE]";
/// See [`CLUSTER_STATE_REFUSAL`].
const REPLICATOR_STATE_REFUSAL: &str = "Invalid parameter replicatorArn: This operation is only \
                                        valid for resources that are in one of the following \
                                        states: [RUNNING]";

/// How often each poll below looks again, and how many looks it takes. Every wait here is
/// for a CONDITION the service's own clock owes, and the deadline is far longer than the
/// spans the service publishes, so nothing here says anything about how fast the host runs.
const LOOK: Duration = Duration::from_millis(50);
/// See [`LOOK`]: 1200 looks at that interval is a minute of looking.
const LOOKS: usize = 1_200;

/// One emulator carrying Amazon EC2, AWS IAM and Amazon MSK over ONE shared engine, as the
/// production router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_kafka_kafka::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is
/// what routes the request.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One answer: its HTTP status and its body as JSON (`Value::Null` when it is not JSON).
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    /// The wire error code (`__type` of the restJson1 error envelope), or `""` on success.
    fn error_code(&self) -> &str {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The error envelope's message, or `""` when there is none.
    fn message(&self) -> &str {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// A member of the body addressed by JSON pointer, or `""` when it is not a string.
    fn at(&self, pointer: &str) -> &str {
        self.body
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
}

/// Call one Amazon MSK operation over its restJson1 wire.
async fn kafka(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Reply {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("serialize the request body"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("kafka.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("kafka"))
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    Reply { status, body }
}

/// One form-encoded call of `signing_name`, answered with the HTTP status and the XML body.
async fn form_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    host: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One EC2 action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = form_call(
        router,
        "ec2",
        &format!("ec2.{REGION}.amazonaws.com"),
        EC2_API_VERSION,
        action,
        params,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {body}"
    );
    body
}

/// Three subnets, one per Availability Zone, all minted by EC2 itself: what a provisioned
/// cluster and a replicator's client VPC configuration are placed in.
async fn ec2_subnets(router: &MultiServiceRouter) -> Vec<String> {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", "10.86.0.0/16")]).await,
        "vpcId",
    );
    let mut subnets = Vec::new();
    for (cidr, zone) in [
        ("10.86.1.0/24", "us-west-2a"),
        ("10.86.2.0/24", "us-west-2b"),
        ("10.86.3.0/24", "us-west-2c"),
    ] {
        subnets.push(element(
            &ec2(
                router,
                "CreateSubnet",
                &[
                    ("VpcId", vpc.as_str()),
                    ("CidrBlock", cidr),
                    ("AvailabilityZone", zone),
                ],
            )
            .await,
            "subnetId",
        ));
    }
    subnets
}

/// A trust policy that lets the service principal `principal` assume the role it is
/// attached to, in the shape `iam:CreateRole` takes one.
fn trust_policy(principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// Create a role in IAM carrying `document` — through IAM's OWN `CreateRole`, so what MSK
/// then resolves is the document IAM persisted — and hand back the ARN IAM minted.
async fn given_role(router: &MultiServiceRouter, name: &str, document: &str) -> String {
    let (status, created) = form_call(
        router,
        "iam",
        "iam.amazonaws.com",
        IAM_API_VERSION,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", document)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({name:?}) must succeed, got {status} with {created}"
    );
    element(&created, "Arn")
}

/// A `CreateCluster` body placing three brokers in `subnets`, with the SASL/IAM client
/// authentication a client of such a cluster uses.
fn cluster_body(name: &str, subnets: &[String]) -> Value {
    json!({
        "clusterName": name,
        "kafkaVersion": KAFKA_VERSION,
        "numberOfBrokerNodes": subnets.len(),
        "brokerNodeGroupInfo": {
            "instanceType": INSTANCE_TYPE,
            "clientSubnets": subnets,
        },
        "clientAuthentication": { "sasl": { "iam": { "enabled": true } } },
    })
}

/// `CreateCluster`, answering the ARN it minted.
async fn given_cluster(router: &MultiServiceRouter, name: &str, subnets: &[String]) -> String {
    let created = kafka(
        router,
        "POST",
        "/v1/clusters",
        Some(cluster_body(name, subnets)),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateCluster({name:?}) must be accepted, got {} with {}",
        created.status, created.body
    );
    created.at("/clusterArn").to_string()
}

/// `DescribeClusterV2` for one cluster.
async fn described_cluster(router: &MultiServiceRouter, cluster: &str) -> Reply {
    kafka(
        router,
        "GET",
        &format!("/api/v2/clusters/{}", form_encode(cluster)),
        None,
    )
    .await
}

/// The cluster's `State` and `CurrentVersion`, as the V2 reader reports them.
async fn cluster_state(router: &MultiServiceRouter, cluster: &str, case: &str) -> (String, String) {
    let reply = described_cluster(router, cluster).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: DescribeClusterV2 must answer, got {} with {}",
        reply.status, reply.body
    );
    (
        reply.at("/clusterInfo/state").to_string(),
        reply.at("/clusterInfo/currentVersion").to_string(),
    )
}

/// Look again until `cluster` reports `ACTIVE`, answering its `CurrentVersion` then: the
/// transition is one the service's own clock owes, so this waits for the CONDITION.
async fn cluster_once_active(router: &MultiServiceRouter, cluster: &str, case: &str) -> String {
    for _ in 0..LOOKS {
        let (state, version) = cluster_state(router, cluster, case).await;
        if state == "ACTIVE" {
            return version;
        }
        tokio::time::sleep(LOOK).await;
    }
    let (state, _) = cluster_state(router, cluster, case).await;
    panic!("case {case:?}: the cluster never reported ACTIVE, last read {state}")
}

/// `UpdateStorage` with `volume_size_gb`, quoting `version`.
async fn update_storage(
    router: &MultiServiceRouter,
    cluster: &str,
    version: &str,
    volume_size_gb: i64,
) -> Reply {
    kafka(
        router,
        "PUT",
        &format!("/v1/clusters/{}/storage", form_encode(cluster)),
        Some(json!({ "currentVersion": version, "volumeSizeGB": volume_size_gb })),
    )
    .await
}

/// `UpdateRebalancing`, quoting `version`.
async fn update_rebalancing(router: &MultiServiceRouter, cluster: &str, version: &str) -> Reply {
    kafka(
        router,
        "PUT",
        &format!("/v1/clusters/{}/rebalancing", form_encode(cluster)),
        Some(json!({
            "currentVersion": version,
            "rebalancing": { "status": "ACTIVE" },
        })),
    )
    .await
}

/// Look again until the cluster operation `operation` reports `UPDATE_COMPLETE`.
async fn operation_once_complete(router: &MultiServiceRouter, operation: &str, case: &str) {
    let uri = format!("/api/v2/operations/{}", form_encode(operation));
    let mut last = String::new();
    for _ in 0..LOOKS {
        let reply = kafka(router, "GET", &uri, None).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: DescribeClusterOperationV2 must answer, got {} with {}",
            reply.status, reply.body
        );
        last = reply.at("/clusterOperationInfo/operationState").to_string();
        if last == "UPDATE_COMPLETE" {
            return;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the cluster operation never completed, last read {last}");
}

/// A `CreateReplicator` body replicating from `source` to `target` under `role`, with the
/// replicator's client interfaces placed in `subnets`.
fn replicator_body(name: &str, source: &str, target: &str, role: &str, subnets: &[String]) -> Value {
    json!({
        "replicatorName": name,
        "serviceExecutionRoleArn": role,
        "kafkaClusters": [
            { "amazonMskCluster": { "mskClusterArn": source },
              "vpcConfig": { "subnetIds": subnets } },
            { "amazonMskCluster": { "mskClusterArn": target },
              "vpcConfig": { "subnetIds": subnets } },
        ],
        "replicationInfoList": [{
            "sourceKafkaClusterArn": source,
            "targetKafkaClusterArn": target,
            "targetCompressionType": "NONE",
            "topicReplication": { "topicsToReplicate": ["orders.*"] },
            "consumerGroupReplication": { "consumerGroupsToReplicate": [".*"] },
        }],
    })
}

/// An `UpdateReplicationInfo` body quoting `version` and replicating `pattern`.
fn replication_update(source: &str, target: &str, version: &str, pattern: &str) -> Value {
    json!({
        "currentVersion": version,
        "sourceKafkaClusterArn": source,
        "targetKafkaClusterArn": target,
        "topicReplication": {
            "copyAccessControlListsForTopics": false,
            "copyTopicConfigurations": false,
            "detectAndCopyNewTopics": false,
            "topicsToExclude": [],
            "topicsToReplicate": [pattern],
        },
    })
}

/// Look again until the replicator reports `state`, answering the `CurrentVersion` the
/// reader then reports: the transition is one the service's own clock owes.
async fn replicator_once_in_state(
    router: &MultiServiceRouter,
    replicator: &str,
    state: &str,
    case: &str,
) -> String {
    let uri = format!("/replication/v1/replicators/{}", form_encode(replicator));
    let mut last = String::new();
    for _ in 0..LOOKS {
        let reply = kafka(router, "GET", &uri, None).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: DescribeReplicator must answer, got {} with {}",
            reply.status, reply.body
        );
        last = reply.at("/replicatorState").to_string();
        if last == state {
            return reply.at("/currentVersion").to_string();
        }
        // A replicator the clock owes `FAILED` never reaches `RUNNING`, and the other way
        // round, so a terminal state that is not the one expected is answered at once.
        if last == "FAILED" || last == "RUNNING" {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the replicator never reported {state}, last read {last}")
}

/// RULE: a client that POLLS reaches every terminal state of this sequence — both clusters
/// `ACTIVE`, the storage update's operation `UPDATE_COMPLETE`, and the replicator `RUNNING`
/// — while the resources it names are EC2's own subnets and a role IAM holds; the changes
/// it makes before a resource has settled are refused in Amazon MSK's fixed sentences, and
/// the same calls are accepted after.
#[tokio::test]
async fn the_cluster_operation_and_replicator_lifecycles_settle_with_ec2_and_iam_mounted() {
    let case = "two clusters, a storage update and a replicator";
    let router = emulator();
    let subnets = ec2_subnets(&router).await;
    let role = given_role(
        &router,
        "msk-replicator-settles",
        &trust_policy(MSK_SERVICE_PRINCIPAL),
    )
    .await;

    let source = given_cluster(&router, "settles-source", &subnets).await;
    let target = given_cluster(&router, "settles-target", &subnets).await;

    // Still CREATING: both changes are refused in MSK's fixed sentence, and neither moves
    // the cluster's version.
    let (state, version) = cluster_state(&router, &source, case).await;
    assert_eq!(
        state, "CREATING",
        "case {case:?}: a just-created cluster is reported CREATING, got {state}"
    );
    for (call, refused) in [
        (
            "UpdateStorage",
            update_storage(&router, &source, &version, 11).await,
        ),
        (
            "UpdateRebalancing",
            update_rebalancing(&router, &source, &version).await,
        ),
    ] {
        assert_eq!(
            (refused.status, refused.error_code(), refused.message()),
            (400, "BadRequestException", CLUSTER_STATE_REFUSAL),
            "case {case:?}: {call} while CREATING must be refused with MSK's own sentence, \
             got {} with {}",
            refused.status,
            refused.body
        );
    }
    let (_, unchanged) = cluster_state(&router, &source, case).await;
    assert_eq!(
        unchanged, version,
        "case {case:?}: a refused update must leave the cluster's version alone"
    );

    // Both clusters settle, and the update the CREATING cluster refused is now accepted.
    let settled = cluster_once_active(&router, &source, case).await;
    cluster_once_active(&router, &target, case).await;
    let accepted = update_storage(&router, &source, &settled, 11).await;
    assert_eq!(
        accepted.status, 200,
        "case {case:?}: UpdateStorage on an ACTIVE cluster must be accepted, got {} with {}",
        accepted.status, accepted.body
    );
    let operation = accepted.at("/clusterOperationArn").to_string();
    assert!(
        !operation.is_empty(),
        "case {case:?}: UpdateStorage must report the cluster operation it started, got {}",
        accepted.body
    );
    operation_once_complete(&router, &operation, case).await;
    let (state, advanced) = cluster_state(&router, &source, case).await;
    assert_eq!(
        state, "ACTIVE",
        "case {case:?}: a completed update leaves the cluster ACTIVE, got {state}"
    );
    assert_ne!(
        advanced, settled,
        "case {case:?}: a completed update must advance the cluster's version"
    );

    // The replicator over the two clusters: the rows differ along the trust policy of the
    // role the create passes, which is what decides the terminal state the clock owes it.
    // (case, the principal the role trusts, the state the replicator must reach)
    let rows: &[(&str, &str, &str)] = &[
        ("a role whose trust policy admits MSK", MSK_SERVICE_PRINCIPAL, "RUNNING"),
        ("a role whose trust policy admits another service", "ec2.amazonaws.com", "FAILED"),
    ];
    for (index, (row, principal, expected)) in rows.iter().enumerate() {
        let role = match *principal == MSK_SERVICE_PRINCIPAL {
            true => role.clone(),
            false => {
                given_role(
                    &router,
                    &format!("msk-replicator-other-{index}"),
                    &trust_policy(principal),
                )
                .await
            }
        };
        let created = kafka(
            &router,
            "POST",
            "/replication/v1/replicators",
            Some(replicator_body(
                &format!("settles-replicator-{index}"),
                &source,
                &target,
                &role,
                &subnets,
            )),
        )
        .await;
        assert_eq!(
            (created.status, created.at("/replicatorState")),
            (200, "CREATING"),
            "case {row:?}: CreateReplicator must be admitted and report CREATING, got {} \
             with {}",
            created.status,
            created.body
        );
        let replicator = created.at("/replicatorArn").to_string();
        let replicator_uri = format!("/replication/v1/replicators/{}", form_encode(&replicator));
        let described = kafka(&router, "GET", &replicator_uri, None).await;
        assert_eq!(
            (described.status, described.at("/replicatorState")),
            (200, "CREATING"),
            "case {row:?}: a just-created replicator reads back CREATING, got {} with {}",
            described.status,
            described.body
        );
        let creating_version = described.at("/currentVersion").to_string();

        // A change while it is CREATING is refused in the replicator's own sentence.
        let refused = kafka(
            &router,
            "PUT",
            &format!("{replicator_uri}/replication-info"),
            Some(replication_update(
                &source,
                &target,
                &creating_version,
                "payments.*",
            )),
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_code(), refused.message()),
            (400, "BadRequestException", REPLICATOR_STATE_REFUSAL),
            "case {row:?}: a change while CREATING must be refused with MSK's own sentence, \
             got {} with {}",
            refused.status,
            refused.body
        );

        // And the clock settles it: RUNNING for the role MSK may assume, FAILED for the
        // one it may not.
        let settled_version = replicator_once_in_state(&router, &replicator, expected, row).await;
        if *expected != "RUNNING" {
            continue;
        }

        // The same change is accepted once it is RUNNING, and it advances the version.
        let accepted = kafka(
            &router,
            "PUT",
            &format!("{replicator_uri}/replication-info"),
            Some(replication_update(
                &source,
                &target,
                &settled_version,
                "payments.*",
            )),
        )
        .await;
        assert_eq!(
            accepted.status, 200,
            "case {row:?}: the same change on a RUNNING replicator must be accepted, got {} \
             with {}",
            accepted.status, accepted.body
        );
        let (state, updated_version) =
            ("RUNNING", replicator_once_in_state(&router, &replicator, "RUNNING", row).await);
        assert_eq!(
            state, "RUNNING",
            "case {row:?}: an applied change leaves the replicator RUNNING, got {state}"
        );
        assert_ne!(
            updated_version, settled_version,
            "case {row:?}: an accepted change must advance the replicator's version"
        );
        let described = kafka(&router, "GET", &replicator_uri, None).await;
        assert_eq!(
            described
                .body
                .pointer("/replicationInfoList/0/topicReplication/topicsToReplicate")
                .cloned()
                .unwrap_or(Value::Null),
            json!(["payments.*"]),
            "case {row:?}: the accepted change must be readable from the replicator, got {}",
            described.body
        );
    }
}
