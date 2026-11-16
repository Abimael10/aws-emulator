Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kafka.e2e-cluster-topic-lifecycle-produce-consume-teardown
Issue-Shape-Id: com.amazonaws.kafka#Kafka
Issue-Key: kafka.e2e-cluster-topic-lifecycle-produce-consume-teardown|_produce_and_consume#25|Timeout|received = [msg.value for msg in consumer]
Issue-Commit: 13e968f2d712b86cbfec63dd795b2bc21b7f9b4c

# MSK end-to-end cluster/topic SCRAM produce-consume-teardown reaches no verdict: the provisioning lifecycle never settles and nothing serves the advertised Kafka listeners (2 causes)

> _Note: this issue was identified against the code base at commit `13e968f2d712b86cbfec63dd795b2bc21b7f9b4c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeClusterV2

## Description
ONE failing behavioral check that the suite report marked neither ALREADY FILED nor OWNED by an open issue: kafka.e2e-cluster-topic-lifecycle-produce-consume-teardown. Its only detail is the bare 'Error: Check timed out after 60.0s' — no HTTP status, no error code and no traceback frame — so no assertion in it was ever evaluated. The check needs TWO independent emulator behaviours before it can assert anything, and TWO distinct root causes are therefore described below; each must be closed separately before this issue is submitted.

Cause 1 of 2: the PROVISIONED cluster / SCRAM-association / topic lifecycle this check drives never settles, so every waiter burns its whole budget. (Failing check: kafka.e2e-cluster-topic-lifecycle-produce-consume-teardown.)
AWS: Smithy com.amazonaws.kafka#Kafka — CreateClusterV2 returns the ClusterArn with the cluster CREATING and the cluster then reaches ACTIVE, observable through DescribeClusterV2 GET /api/v2/clusters/{ClusterArn} (code 200, output ClusterInfo.State); CreateTopic POST /v1/clusters/{ClusterArn}/topics (code 200, output Status of enum TopicState) and DescribeTopic GET /v1/clusters/{ClusterArn}/topics/{TopicName} (code 200, output Status) take a created topic to ACTIVE; BatchAssociateScramSecret POST /v1/clusters/{ClusterArn}/scram-secrets (code 200) returns with an empty UnprocessedScramSecrets for a well-formed secret; after DeleteTopic and DeleteCluster, DescribeTopic and DescribeClusterV2 both answer the modeled NotFoundException (404, message 'The requested resource doesn’t exist.' — that text is entirely fixed, the ARN and topic name are NOT interpolated into it). Recorded behavior for the cadence: a client polls DescribeClusterV2 at ~30 s and DescribeTopic at ~5 s and the whole create -> ACTIVE -> topic ACTIVE -> delete -> NotFound sequence completes in a handful of intervals.
Emulator: the sequence CreateConfiguration -> CreateClusterV2(kafka.m5.large, 2 brokers, 2 private subnets, Sasl.Scram+Sasl.Iam, TLS, ConfigurationInfo) -> wait State ACTIVE (DescribeClusterV2, 30 s interval, 1800 s budget) -> BatchAssociateScramSecret -> CreateTopic -> wait Status ACTIVE (DescribeTopic, 5 s/300 s) -> GetBootstrapBrokers -> produce/consume -> DeleteTopic/DeleteCluster -> NotFoundException never produces a response the check can assert on; it is killed at 60 s with no frame. Hypothesis (the replay output names no frame, so this is not stated by the evidence): the asynchronous transitions are owed to a wall clock rather than settled on the write path, so the FIRST probe after each write always observes the non-terminal value and the caller is charged a full poll interval per transition — the same clock-owed settling already described for OTHER check ids by the open issues 20261104T171616Z-kafka-msk-asynchronous-transitions-are-clock-owed-so-an.md, 20261105T161551Z-kafka-two-more-msk-checks-never-reach-a-verdict.md and 20261114T065925Z-kafka-msk-asynchronous-lifecycles-never-settle-channel-configuration-delete.md, none of which carries this check's replay key.
Why: a client that provisions an MSK cluster and a topic and waits for them to become usable at an AWS-appropriate cadence pays a full interval per transition the emulator had in fact already finished, so emulator-backed suites stall on sequences AWS completes; and the post-delete NotFoundException contract for both DescribeTopic and DescribeClusterV2 is unverifiable.
Fix: the kafka service's state layer plus the CreateClusterV2 / CreateTopic / BatchAssociateScramSecret / DeleteTopic / DeleteCluster handlers — make these transition instants complete-on-return (or settle them on the write path) so a first probe after a write observes the terminal value, and make the deleted topic and the deleted cluster resolve to NotFoundException.
Traces: kafka.e2e-cluster-topic-lifecycle-produce-consume-teardown.

Cause 2 of 2: nothing serves the Kafka wire protocol on the in-VPC SASL/SCRAM and SASL/IAM endpoints the control plane advertises. (Failing check: kafka.e2e-cluster-topic-lifecycle-produce-consume-teardown.)
AWS: GetBootstrapBrokers GET /v1/clusters/{ClusterArn}/bootstrap-brokers (code 200) returns BootstrapBrokerStringSaslScram and BootstrapBrokerStringSaslIam for an ACTIVE cluster with Sasl.Scram and Sasl.Iam enabled and TLS in transit, and an in-VPC client reaching those host:port pairs has its TCP connection accepted, completes the SASL/SCRAM (port 9096) or SASL/IAM OAUTHBEARER (port 9098) handshake, and can create ACLs, produce a record and consume it back; the per-partition FIFO guarantee holds. Recorded behavior only for the data-plane semantics — the control-plane model states only that these endpoint strings exist.
Emulator: the check never gets a produce or a consume to happen; it also cannot grant the SCRAM principal its ACLs over the advertised SASL/IAM listener. Because the emulator itself announces those bootstrap addresses, an unreachable listener is the emulator's own gap, not the runner's: it announced an address other than one it serves. (The advertised host:port a given run sees is a per-run value; the requirement is that whatever endpoint GetBootstrapBrokers advertises accepts a connection and completes the handshake.)
Why: a client that configures itself from GetBootstrapBrokers hangs until its own timeout, and the one guarantee a faked data plane cannot meet — a record produced over SASL/SCRAM being consumed back from the topic the control plane created — is unverifiable.
Fix: the kafka service must serve the Kafka wire protocol (a broker listener, per-scheme SASL authentication for SCRAM-SHA-512 and IAM OAUTHBEARER, Kafka ACL admin APIs, and the record/partition/offset state the produce and fetch paths read and write) on the ports it advertises, or advertise only endpoints it serves. Cause 1 blocks this check before it reaches the listener, so both must be fixed for the check to reach a verdict.
Traces: kafka.e2e-cluster-topic-lifecycle-produce-consume-teardown. Related open issues named only for the neighbourhood (they do NOT carry this check's replay key): 20261104T171616Z-kafka-no-kafka-broker-answers-on-the-advertised-bootstrap.md and 20261114T205343Z-kafka-msk-data-plane-and-lifecycle-bootstrap-string-carries.md.

## Evidence
Suite report, service heading '### kafka': [mismatch] kafka.e2e-cluster-topic-lifecycle-produce-consume-teardown — 'Error: Check timed out after 60.0s'. No HTTP status, no error code, no traceback frame; the check is not marked ALREADY FILED and the report lists no OWNED-by issue for it.

Read from the check source, the sequence and its AWS-sized cadences are what the 60 s ceiling cannot absorb: _wait_cluster_active polls DescribeClusterV2 every 30 s (1800 s budget) until State == ACTIVE (FAILED is an immediate assertion failure); _wait_topic_status polls DescribeTopic every 5 s (300 s) until Status == ACTIVE; GetBootstrapBrokers is re-polled every 15 s (300 s) until BootstrapBrokerStringSaslScram is non-empty, and again for BootstrapBrokerStringSaslIam; _wait_topic_gone polls every 5 s (300 s) and _wait_cluster_gone every 15 s (900 s) for NotFoundException.

Expected (AWS), none of which was reached:
  - CreateClusterV2 -> the cluster reaches ACTIVE; BatchAssociateScramSecret succeeds on it.
  - CreateTopic(PartitionCount=1, ReplicationFactor=2) -> DescribeTopic Status ACTIVE.
  - GetBootstrapBrokers returns a non-empty BootstrapBrokerStringSaslScram and BootstrapBrokerStringSaslIam; connecting to each advertised host:port is accepted, CreateAcls over the SASL/IAM listener reports no failures, and a payload produced over SASL/SCRAM is observed by a consumer reading from 'earliest'.
  - After DeleteTopic, DescribeTopic answers error code 'NotFoundException' (HTTP 404); after DeleteCluster, DescribeClusterV2 answers error code 'NotFoundException' (HTTP 404). AWS's message for that class is the fixed text 'The requested resource doesn’t exist.' — no request or state value is interpolated.
Actual: no verdict at all; nothing was produced, consumed or refused.

Smithy citations used: GetBootstrapBrokers GET /v1/clusters/{ClusterArn}/bootstrap-brokers code 200 with bootstrapBrokerStringSaslScram / bootstrapBrokerStringSaslIam; CreateTopic POST /v1/clusters/{ClusterArn}/topics code 200 with status of enum TopicState; DescribeTopic GET /v1/clusters/{ClusterArn}/topics/{TopicName} code 200, errors include NotFoundException 404; DescribeClusterV2 GET /api/v2/clusters/{ClusterArn} code 200 with clusterInfo, errors include NotFoundException 404; BatchAssociateScramSecret POST /v1/clusters/{ClusterArn}/scram-secrets code 200 with unprocessedScramSecrets.

## Steps to Reproduce
Cause 1:
1. Boot the emulator. CreateConfiguration(KafkaVersions=['3.9.x'], ServerProperties='allow.everyone.if.no.acl.found=false'), then CreateClusterV2 a PROVISIONED cluster (kafka.m5.large, NumberOfBrokerNodes=2, two private ClientSubnets, ClientAuthentication Sasl.Scram + Sasl.Iam, EncryptionInTransit ClientBroker=TLS, that ConfigurationInfo).
2. IMMEDIATELY after the response, call DescribeClusterV2 once and record whether ClusterInfo.State is already ACTIVE or still CREATING, and for how long it stays non-terminal.
3. Once ACTIVE, call BatchAssociateScramSecret with one well-formed secret ARN and assert UnprocessedScramSecrets is empty; then CreateTopic(PartitionCount=1, ReplicationFactor=2) and, immediately after the response, call DescribeTopic once and record whether Status is already ACTIVE.
4. Assert the whole control-plane sequence completes inside 60 s of wall clock at an AWS cadence (DescribeClusterV2 every 30 s, DescribeTopic every 5 s).
5. DeleteTopic then DeleteCluster, and assert DescribeTopic and DescribeClusterV2 each answer HTTP 404 with error code 'NotFoundException' and the fixed message 'The requested resource doesn’t exist.'

Cause 2:
1. With that cluster ACTIVE and the SCRAM secret associated, call GetBootstrapBrokers and take BootstrapBrokerStringSaslScram and BootstrapBrokerStringSaslIam.
2. Open a plain TCP connection to every advertised host:port in both strings and assert each connection is ACCEPTED (today none is).
3. Over the SASL/IAM endpoint (OAUTHBEARER, SigV4-presigned kafka-cluster:Connect token) call CreateAcls granting ALLOW ALL on TOPIC '*' and GROUP '*' for User:<scram-username> and assert no ACL is reported failed.
4. Over the SASL/SCRAM endpoint (SCRAM-SHA-512, SASL_SSL) produce one record to the topic and consume it back with auto_offset_reset='earliest'; assert the produced payload is among the consumed values.
5. Replay kafka.e2e-cluster-topic-lifecycle-produce-consume-teardown and confirm it produces a pass/fail verdict instead of a 60 s timeout.
