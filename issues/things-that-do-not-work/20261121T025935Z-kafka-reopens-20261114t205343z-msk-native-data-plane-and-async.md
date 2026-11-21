Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kafka.kafka-produce-fetch-in-vpc kafka.e2e-cluster-scram-auth-produce-consume-teardown kafka.topic-retention-config-echoed-over-scram-message-readable kafka.cluster-replicator-updates-blocked-while-creating-rejected
Issue-Shape-Id: com.amazonaws.kafka#Kafka
Issue-Key: kafka.kafka-produce-fetch-in-vpc|run#24|AssertionError|raise _dp_fail(
Issue-Key: kafka.e2e-cluster-scram-auth-produce-consume-teardown|run#41|Timeout|received = {record.value for record in consumer}
Issue-Key: kafka.topic-retention-config-echoed-over-scram-message-readable|_consume_from_earliest#20|Timeout|for record in consumer:
Issue-Key: kafka.cluster-replicator-updates-blocked-while-creating-rejected|_wait_for_replicator_state#36|Timeout|time.sleep(interval_s)
Issue-Commit: ae12372f9a47989add7c1dd4c96799b05b75d2b4

# REOPENS 20261114T205343Z: MSK native data plane and async lifecycles are still unusable — CreateTopics closes the socket, the SASL/SCRAM listener never serves, and the cluster/replicator lifecycle never settles (3 causes)

> _Note: this issue was identified against the code base at commit `ae12372f9a47989add7c1dd4c96799b05b75d2b4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTopics

## Description
REOPENS 20261114T205343Z-kafka-msk-data-plane-and-lifecycle-bootstrap-string-carries.md: its fix left the call refused. That issue is marked fixed and claimed causes 2, 3 and 4 (the native broker's missing CreateTopics, the SASL/SCRAM listener that never serves, and the replicator/cluster lifecycle that never settles). Replaying its own checks shows all three legs still do not work; the CreateTopics leg changed symptom only (api key 19 is now advertised) and is still refused. These are not new defects. Three causes; each must be closed separately before this issue is submitted.

Cause 1 of 3: the native Kafka broker advertises CreateTopics (api key 19) in ApiVersions but closes the TCP connection instead of answering a CreateTopics request, so no topic can be administered over the wire protocol and the PLAINTEXT in-VPC produce/fetch round trip is unreachable. (Failing check: kafka.kafka-produce-fetch-in-vpc.)
AWS: recorded behavior only — the Kafka wire protocol is not in the kafka Smithy model (the model covers the control plane; GetBootstrapBrokers is GET /v1/clusters/{ClusterArn}/bootstrap-brokers -> 200 with the bootstrapBrokerString* members whose contract is an endpoint the service itself speaks Kafka on). A real MSK broker on the KafkaVersion the cluster reports (3.9.x here) answers every api key it advertises in ApiVersions: a CreateTopics (19) request on the advertised listener is answered with a CreateTopics response carrying a per-topic Kafka error code (0 on success, e.g. TOPIC_ALREADY_EXISTS when it refuses) — a broker never answers an advertised api key by dropping the socket.
Emulator: the control plane provisions the cluster to ACTIVE and advertises one host:port per broker; ApiVersions is answered and now DOES advertise api key 19 (min_version 2, max_version 5, alongside 0,1,2,3,8,9,10,11,12,13,14,15,16,17,18,19,20,22,23,29,30,31,32,36,37,42), and Metadata is answered and names both brokers and a controller. A CreateTopics request sent to the controller is then answered by closing the connection: `CreateTopics on controller: ConnectionError: broker closed after 0 of 4 bytes` — no response frame, no Kafka error code. The check therefore reports `DATAPLANE_UNIMPLEMENTED: the MSK control plane provisioned 2 brokers and advertised <bootstrap string>, but nothing there completes a Kafka produce/fetch from the in-VPC runner`.
Why: a client that negotiates with ApiVersions (kafka-python, the Java AdminClient, kafka-topics.sh, Flink/Spring connectors that auto-create topics) is told CreateTopics is supported, sends it, and gets a transport-level disconnect instead of a protocol answer — it cannot distinguish an unimplemented operation from a crashed or network-partitioned broker, and it has no error code to branch on. With no topic, produce, fetch, offsets, partition growth and record-format behavior are all unobservable.
Fix: the emulated Kafka wire-protocol broker — the request dispatch of the broker listener must handle every api key its ApiVersions table advertises, and CreateTopics (19) / DeleteTopics (20) must return a well-formed response with a Kafka error code (never an abrupt close); the advertised table must not list keys the dispatcher drops.
Traces: kafka.kafka-produce-fetch-in-vpc. Adjacent open issue that does not cover it: 20261114T205343Z's successor filings for other services, and 20261118T071212Z-kafka-msk-native-data-plane-describegroups-member-entries-carry.md (different checks, DescribeGroups payload).

Cause 2 of 3: on a SASL/SCRAM + TLS provisioned cluster nothing ever authenticates over SASL/SCRAM, so an associated secret never becomes usable and no SCRAM produce/consume or broker DescribeConfigs ever completes. (Failing checks: kafka.e2e-cluster-scram-auth-produce-consume-teardown, kafka.topic-retention-config-echoed-over-scram-message-readable.)
AWS: Smithy kafka BatchAssociateScramSecret returns an empty UnprocessedScramSecrets on success; AWS documents that an associated AmazonMSK_* Secrets Manager credential propagates to the brokers ("MSK syncs the credential data periodically"), after which a SCRAM-SHA-512 SASL_SSL client connected to BootstrapBrokerStringSaslScram produces and consumes records and its AdminClient DescribeConfigs reports the topic's configs (retention.ms, segment.ms, cleanup.policy). Recorded behavior for the wire leg.
Emulator: both checks reach no verdict — each is killed at the suite's cap (`Check timed out after 60.0s`) inside the bounded poll that waits for the first SASL/SCRAM produce to succeed after association; nothing on the advertised SASL/SCRAM endpoint ever completes a produce, and no refusal is returned either.
Why: the documented MSK password path is dead — a client using a Secrets Manager SCRAM secret can neither write nor read and never receives an error to act on, while the control plane reports the association as successful. Any SCRAM-authenticated application hangs instead of failing.
Fix: the emulated broker's SASL/SCRAM listener and the state that joins an associated secret to broker-side credentials, so an associated user authenticates and its produce/fetch/DescribeConfigs complete while a non-associated one is refused.
Traces: kafka.e2e-cluster-scram-auth-produce-consume-teardown, kafka.topic-retention-config-echoed-over-scram-message-readable. Adjacent open issues that do not carry these checks' replay keys: 20261116T122712Z-kafka-msk-end-to-end-cluster-topic-scram-produce.md and 20261116T210722Z-kafka-msk-native-data-plane-sasl-iam-bring-up.md.

Cause 3 of 3: the cluster / cluster-operation / replicator asynchronous lifecycle never settles, so the positive half of the CREATING-state guard can never be exercised. (Failing check: kafka.cluster-replicator-updates-blocked-while-creating-rejected.)
AWS: Smithy kafka UpdateStorage, UpdateRebalancing and UpdateReplicationInfo all model BadRequestException (400) for a resource in the wrong state, with AWS's fixed text — `This operation is only valid for resources that are in one of the following states :[ACTIVE]` for the cluster guard and `Invalid parameter replicatorArn: This operation is only valid for resources that are in one of the following states: [RUNNING]` for the replicator guard (fixed text; only the parameter name and the state list vary) — and the SAME calls are accepted once DescribeClusterV2 reports State ACTIVE / DescribeReplicator reports ReplicatorState RUNNING, with DescribeClusterOperationV2 reaching OperationState UPDATE_COMPLETE and CurrentVersion advancing.
Emulator: the check reaches no verdict (`Check timed out after 60.0s`): after the CREATING-state probes it polls for both clusters to reach ACTIVE, for the UpdateStorage ClusterOperationArn to complete and for the replicator to reach RUNNING, and one of those waits never completes within a client-sized budget.
Why: polling for ACTIVE/RUNNING before mutating is the documented way to drive these APIs, and a client doing so blocks indefinitely; the accept-once-settled half of every state guard is unreachable.
Fix: the asynchronous state machines behind CreateClusterV2/UpdateStorage/CreateReplicator in the kafka service's state — cluster State, cluster-operation OperationState and ReplicatorState must advance to their terminal values on their own and bump CurrentVersion.
Traces: kafka.cluster-replicator-updates-blocked-while-creating-rejected. Adjacent open issue that does not carry this check's replay key: 20261114T065925Z-kafka-msk-asynchronous-lifecycles-never-settle-channel-configuration-delete.md.

## Evidence
Cause 1 — kafka.kafka-produce-fetch-in-vpc (verdict mismatch; not marked ALREADY FILED or OWNED):
  AssertionError: DATAPLANE_UNIMPLEMENTED: the MSK control plane provisioned 2 brokers and advertised '<host>:<portA>,<host>:<portB>', but nothing there completes a Kafka produce/fetch from the in-VPC runner. Brokers that cannot be spoken to are not a data plane. cluster <name> is ACTIVE and advertises the same pair. in-VPC setup: {'ok': False, 'stage': 'io', 'ip': <advertised host>, 'host': <advertised host>, 'api_versions': <answered>, 'bootstrap_metadata': <answered, 2 brokers + controller>, 'controller': {'host': <advertised host>, 'port': <portA>}, 'error': 'CreateTopics on controller: ConnectionError: broker closed after 0 of 4 bytes'}. in-VPC round trip: None.
  The ApiVersions response the check captured decodes to 26 api-key rows and DOES include key 19 (CreateTopics, min 2 / max 5) and key 20 (DeleteTopics, min 1 / max 5). Host and port values are per-run and are not the requirement.
  Expected (AWS): the CreateTopics request is answered with a CreateTopics response frame carrying a per-topic Kafka error code, and the subsequent produce/fetch round trip completes. Actual (emulator): the socket is closed after 0 of 4 bytes of the response length prefix — no frame, no error code.

Cause 2 — kafka.e2e-cluster-scram-auth-produce-consume-teardown and kafka.topic-retention-config-echoed-over-scram-message-readable: `Error: Check timed out after 60.0s`. Both sequences are: CreateClusterV2 (Sasl.Scram enabled, EncryptionInTransit.ClientBroker TLS) -> wait ACTIVE -> CreateTopic -> create an AmazonMSK_* secret with a customer KMS key -> BatchAssociateScramSecret (UnprocessedScramSecrets expected empty) -> poll a SCRAM-SHA-512 SASL_SSL producer against BootstrapBrokerStringSaslScram. Expected (AWS): the produce succeeds once the credential has propagated, the record is readable from the earliest offset, and the broker's DescribeConfigs reports retention.ms=5000 / segment.ms=1000 / cleanup.policy=delete as CreateTopic set them. Actual (emulator): no SCRAM produce ever succeeds and no error is raised; the whole per-check budget is spent in that poll.

Cause 3 — kafka.cluster-replicator-updates-blocked-while-creating-rejected: `Error: Check timed out after 60.0s`. Sequence: CreateCluster x2 -> probe UpdateStorage/UpdateRebalancing while cluster A is CREATING (both must answer 400 BadRequestException `This operation is only valid for resources that are in one of the following states :[ACTIVE]`) -> poll DescribeClusterV2 for ACTIVE -> UpdateStorage VolumeSizeGB 11 -> poll DescribeClusterOperationV2 for UPDATE_COMPLETE -> CreateReplicator -> probe UpdateReplicationInfo while CREATING (400 BadRequestException `Invalid parameter replicatorArn: This operation is only valid for resources that are in one of the following states: [RUNNING]`) -> poll DescribeReplicator for RUNNING -> UpdateReplicationInfo accepted and CurrentVersion changed. Expected: every terminal state is reached and the post-settle updates are accepted. Actual: a wait never completes, so no verdict is reached.

Smithy cited: kafka GetBootstrapBrokers GET /v1/clusters/{ClusterArn}/bootstrap-brokers code 200 (bootstrapBrokerString*/bootstrapBrokerStringSaslScram members); UpdateStorage, UpdateRebalancing, UpdateReplicationInfo and BatchAssociateScramSecret with BadRequestException/400; the Kafka wire protocol itself is not modeled (recorded behavior only).

## Steps to Reproduce
Cause 1:
1. Boot the emulator. CreateCluster a provisioned cluster (2 broker nodes, PLAINTEXT, KafkaVersion 3.9.x) and poll DescribeClusterV2 until State == ACTIVE.
2. GetBootstrapBrokers and take BootstrapBrokerString; connect to the first host:port from inside the cluster's VPC.
3. Send ApiVersions — it is answered and advertises api key 19 — then send Metadata (answered, 2 brokers + controller), then send a CreateTopics (api key 19, version within the advertised min/max) request to the controller.
4. Assert a CreateTopics response frame comes back with a per-topic error code; today the broker closes the connection after 0 of 4 bytes. Then produce one record and fetch it back.
5. Replay kafka.kafka-produce-fetch-in-vpc.

Cause 2:
1. CreateClusterV2 a provisioned cluster with ClientAuthentication.Sasl.Scram.Enabled=true (and Iam enabled) and EncryptionInTransit.ClientBroker=TLS; wait for ACTIVE.
2. CreateTopic (optionally with base64 Configs retention.ms=5000, segment.bytes=1048576, segment.ms=1000, cleanup.policy=delete, file.delete.delay.ms=0).
3. Create a KMS key and an AmazonMSK_* Secrets Manager secret holding {username, password}; call BatchAssociateScramSecret and assert UnprocessedScramSecrets == [].
4. Connect a SCRAM-SHA-512 SASL_SSL producer to BootstrapBrokerStringSaslScram and produce; assert it succeeds within a bounded poll, that the record is consumable from the earliest offset, and that AdminClient DescribeConfigs(config_filter="all") echoes retention.ms/segment.ms/cleanup.policy.
5. Replay kafka.e2e-cluster-scram-auth-produce-consume-teardown and kafka.topic-retention-config-echoed-over-scram-message-readable.

Cause 3:
1. CreateCluster two provisioned clusters (3 brokers, Sasl.Iam) and, while cluster A is CREATING, call UpdateStorage (VolumeSizeGB 11) and UpdateRebalancing; assert 400 BadRequestException with AWS's ACTIVE-state message and an unchanged CurrentVersion.
2. Poll DescribeClusterV2 on both clusters for State ACTIVE, then UpdateStorage on A and poll DescribeClusterOperationV2 for UPDATE_COMPLETE.
3. CreateReplicator over the two clusters; while it is CREATING call UpdateReplicationInfo and assert 400 BadRequestException with AWS's RUNNING-state message.
4. Poll DescribeReplicator for ReplicatorState RUNNING, then call UpdateReplicationInfo and assert CurrentVersion changes.
5. Replay kafka.cluster-replicator-updates-blocked-while-creating-rejected.
