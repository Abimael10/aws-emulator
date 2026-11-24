Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kafka.e2e-cluster-topic-lifecycle-produce-consume-teardown
Issue-Shape-Id: com.amazonaws.kafka#Kafka
Issue-Key: kafka.e2e-cluster-topic-lifecycle-produce-consume-teardown|_produce_and_consume#25|Timeout|received = [msg.value for msg in consumer]
Issue-Commit: 3593513fb7d4f5dd39b253d8c8db33155e4b6dbb

# MSK end-to-end cluster/topic produce-consume-teardown reaches no verdict: the create→ACTIVE spans consume the entire client-sized budget before the Kafka data-plane leg starts

> _Note: this issue was identified against the code base at commit `3593513fb7d4f5dd39b253d8c8db33155e4b6dbb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeClusterV2

## Description
Cause 1 of 1: the full MSK provisioned-cluster lifecycle (CreateClusterV2 → cluster ACTIVE → BatchAssociateScramSecret → CreateTopic → topic ACTIVE → GetBootstrapBrokers → SASL/SCRAM produce/consume → DeleteTopic → DeleteCluster → both resolve NotFoundException) never produces a verdict: the check is killed at the bare `Check timed out after 60.0s` with no AWS error and no assertion reached. (Failing check: kafka.e2e-cluster-topic-lifecycle-produce-consume-teardown.)

AWS: Smithy kafka DescribeClusterV2 (GET /api/v2/clusters/{ClusterArn}, 200, ClusterInfo→Cluster with the ClusterState member) reports a provisioned cluster as CREATING and then ACTIVE; CreateTopic (POST /v1/clusters/{ClusterArn}/topics, Status→TopicState) and DescribeTopic (GET /v1/clusters/{ClusterArn}/topics/{TopicName}, Status→TopicState) report a topic as CREATING and then ACTIVE, and DescribeTopic answers 404 NotFoundException for a topic that is absent, DescribeClusterV2 404 NotFoundException for a cluster that is gone. On real AWS these transitions are what a polling client is documented to drive the API with, and each is observable within a poll or two of an AWS-sized interval; nothing about the sequence requires a live account, a token, or any account-specific id. The data-plane half (SASL/SCRAM SASL_SSL produce then consume on BootstrapBrokerStringSaslScram, after BatchAssociateScramSecret and a CreateAcls grant over the SASL/IAM listener on BootstrapBrokerStringSaslIam) is served by the cluster once ACTIVE.

Emulator: the check spends its whole 60.0 s ceiling inside the waiters that precede the produce/consume leg and is killed there. Its first waiter, `_wait_cluster_active`, polls DescribeClusterV2 at a 30 s interval; the next, `_wait_topic_status`, polls DescribeTopic at 5 s; `get_bootstrap_brokers` is then polled at 15 s until a SASL/SCRAM broker string appears, and the ACL grant retries the SASL/IAM listener at 15 s. Because the emulator's create spans are longer than each waiter's FIRST look, every waiter pays at least one full interval, and the cumulative cost of the cluster-ACTIVE plus topic-ACTIVE plus bootstrap-string plus IAM-listener waits exhausts the budget before a single record is produced. No error code, no status and no message is observed — the emulator simply has not reached ACTIVE by the time the client that polls at an AWS-sized interval gives up, so neither the data-plane round trip nor the teardown assertions (DescribeTopic → 404 NotFoundException, DescribeClusterV2 → 404 NotFoundException after DeleteCluster) are ever exercised.

Why: a client that creates a cluster and a topic, polls for ACTIVE, then produces and consumes — the canonical MSK bring-up — never gets to its data plane against this emulator. The entire happy path of the service's primary use is unreachable, and the deleted-resource NotFoundException contract on both DescribeTopic and DescribeClusterV2 is unverifiable, so an SDK integration cannot be developed or regression-tested against the emulator at all.

Fix: the cluster and topic lifecycle timing in the kafka crate's state layer — `services/service-com-amazonaws-kafka-kafka/src/state.rs` (the cluster create span and `TOPIC_ACTIVATION_DELAY`, plus whatever gates the bootstrap-broker strings and the SASL/IAM listener's readiness behind cluster ACTIVE). What a just-created cluster and topic must be observable as, and by which read, has to fit a client that polls at an AWS-sized interval: the create transient may exist, but it must be consumable by the first or second poll rather than outlasting the client's whole budget. This is the SAME root cause already reported for other checks in open issue 20261124T060000Z-kafka-msk-asynchronous-spans-outlast-a-client-sized-poll-scram-consume-and-the-cluster-replicator-guards.md (and overlaps 20261114T065925Z-kafka-msk-asynchronous-lifecycles-never-settle-channel-configuration-delete.md and 20261116T210722Z-kafka-msk-native-data-plane-sasl-iam-bring-up.md), but neither of those issues carries this check's replay key, so this check is not covered by them and is filed here.

Traces: kafka.e2e-cluster-topic-lifecycle-produce-consume-teardown.

## Evidence
Expected (AWS): CreateClusterV2 a 2-broker provisioned cluster (kafka.m5.large, Sasl.Scram + Sasl.Iam enabled, EncryptionInTransit.ClientBroker=TLS, KafkaVersion 3.9.x, an ACL-enforced Configuration); DescribeClusterV2 reports ClusterInfo.State CREATING then ACTIVE within a few polls; CreateTopic/DescribeTopic report Status CREATING then ACTIVE; GetBootstrapBrokers returns BootstrapBrokerStringSaslScram and BootstrapBrokerStringSaslIam; a SCRAM-SHA-512 SASL_SSL producer's record is consumed back from the earliest offset; after DeleteTopic, DescribeTopic answers 404 with Error.Code NotFoundException, and after DeleteCluster, DescribeClusterV2 answers 404 with Error.Code NotFoundException.

Actual (emulator): `[mismatch] kafka.e2e-cluster-topic-lifecycle-produce-consume-teardown — Error: Check timed out after 60.0s`. The run is killed inside the pre-data-plane waiters; no produce, no consume, and none of the post-delete NotFoundException assertions are reached. There is no diverging status code, error code or message to quote — the divergence is that no terminal state is observable at all inside a client-sized polling budget. The 60.0 s figure is this runner's per-check ceiling and the 30 s / 5 s / 15 s figures are the check's own poll intervals; the requirement is that the cluster-ACTIVE and topic-ACTIVE transitions be observable by an AWS-sized poll, not any particular literal duration.

## Steps to Reproduce
1. Boot the emulator with kafka, ec2, kms and secretsmanager mounted.
2. CreateConfiguration with ServerProperties `allow.everyone.if.no.acl.found=false` for KafkaVersions ["3.9.x"]; create a KMS CMK and an `AmazonMSK_*` Secrets Manager secret holding {username, password}.
3. CreateClusterV2 with Provisioned{NumberOfBrokerNodes=2, InstanceType kafka.m5.large, ClientSubnets=2 private subnets, ConfigurationInfo from step 2, KafkaVersion 3.9.x, ClientAuthentication.Sasl{Scram.Enabled=true, Iam.Enabled=true}, EncryptionInfo.EncryptionInTransit.ClientBroker=TLS}.
4. Poll DescribeClusterV2 for ClusterInfo.State == ACTIVE at a 30 s interval; then BatchAssociateScramSecret with the secret ARN, grant ALLOW ALL ACLs for User:<username> on TOPIC * and GROUP * over BootstrapBrokerStringSaslIam, CreateTopic (PartitionCount 1, ReplicationFactor 2) and poll DescribeTopic for Status == ACTIVE at a 5 s interval.
5. Poll GetBootstrapBrokers for BootstrapBrokerStringSaslScram, produce one record and consume it back with SCRAM-SHA-512 over SASL_SSL, then DeleteTopic and DeleteCluster and assert DescribeTopic and DescribeClusterV2 both answer NotFoundException.
6. Replay kafka.e2e-cluster-topic-lifecycle-produce-consume-teardown and observe the whole 60 s budget is consumed before step 5 begins.
