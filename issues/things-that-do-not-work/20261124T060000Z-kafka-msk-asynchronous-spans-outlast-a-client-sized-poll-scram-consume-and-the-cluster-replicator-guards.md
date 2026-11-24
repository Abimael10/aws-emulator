Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kafka.e2e-cluster-scram-auth-produce-consume-teardown kafka.topic-retention-config-echoed-over-scram-message-readable kafka.cluster-replicator-updates-blocked-while-creating-rejected
Issue-Shape-Id: com.amazonaws.kafka#Kafka
Issue-Key: kafka.e2e-cluster-scram-auth-produce-consume-teardown|run#41|Timeout|received = {record.value for record in consumer}
Issue-Key: kafka.topic-retention-config-echoed-over-scram-message-readable|_consume_from_earliest#20|Timeout|for record in consumer:
Issue-Key: kafka.cluster-replicator-updates-blocked-while-creating-rejected|_wait_for_replicator_state#36|Timeout|time.sleep(interval_s)
Issue-Commit: ae12372f9a47989add7c1dd4c96799b05b75d2b4

# MSK asynchronous spans are longer than a client-sized poll can absorb: the SCRAM consume and the cluster/replicator guard sequence reach no verdict (2 causes, split out of 20261121T025935Z)

> _These are causes 2 and 3 of `20261121T025935Z-kafka-reopens-20261114t205343z-msk-native-data-plane-and-async.md`, re-filed open so they are not recorded as working. Cause 1 of that report (the native broker refusing the oldest api versions and never converting fetched records down) is fixed and replays green; these two were root-caused in the same change but not closed, because closing them means removing an asynchronous transient state ~21 of the kafka crate's own suites pin._

## Operation
DescribeClusterOperationV2

## Description

Cause 1 of 2: the cluster / cluster-operation / replicator sequence settles, but not within what the check's own client-sized polls can absorb. (Failing check: kafka.cluster-replicator-updates-blocked-while-creating-rejected.)
AWS: Smithy kafka UpdateStorage, UpdateRebalancing and UpdateReplicationInfo model BadRequestException (400) for a resource in the wrong state — `This operation is only valid for resources that are in one of the following states :[ACTIVE]` for the cluster guard and `Invalid parameter replicatorArn: This operation is only valid for resources that are in one of the following states: [RUNNING]` for the replicator guard — and the SAME calls are accepted once DescribeClusterV2 reports ACTIVE / DescribeReplicator reports RUNNING, with DescribeClusterOperationV2 reaching UPDATE_COMPLETE and CurrentVersion advancing.
Emulator: every one of those states DOES settle (asserted end to end in `core/router/tests/all/msk_replicator_lifecycle_settles_over_ec2_and_iam.rs`, with EC2 and IAM mounted), yet the check is killed at `Check timed out after 60.0s`. The check walks THREE waiters whose own poll interval is 20 s (recorded in 20261111T235612Z: DescribeClusterV2 20 s, DescribeClusterOperationV2 20 s, DescribeReplicator 20 s). Each pays exactly one full interval, because the service's spans (500 ms) are longer than the waiter's FIRST look and shorter than its second: 3 × 20 s is the whole 60.0 s ceiling.
Evidence from replay: with `CLUSTER_UPDATE_DELAY` set to zero — the one waiter whose transient state this check asserts nothing about — the check PASSES; with all three spans zero it fails fast on its own `creating_info["State"] == "CREATING"` assertion instead of timing out, which is what pins the other two spans as load-bearing.
Why: a client that polls before mutating is the documented way to drive these APIs, so the positive half of every state guard is unreachable for it inside a client-sized budget.
Fix: the cluster-operation state machine in `services/service-com-amazonaws-kafka-kafka/src/state.rs` (`CLUSTER_UPDATE_DELAY`, `PendingUpdate`, `ClusterOperation::end_due`) — an accepted cluster update has to be observable as complete by the FIRST read of the operation it answered, while the create spans (cluster, topic, replicator) keep theirs. That means the `UPDATING` / `UPDATE_IN_PROGRESS` transient goes, and the ~21 suites that pin it have to go with it in the same change: `storage_updates.rs:298`, `broker_count_update.rs:254`, `broker_storage_update.rs:263`, `broker_type_update.rs:197`, `connectivity_update.rs:293`, `monitoring_update.rs:454`, `rebalancing_update.rs:223`, `security_update.rs:348`, `kafka_version_update.rs:303`, `cluster_configuration_update.rs:217`, `cluster_delete_state_guard.rs:467`, `scram_secret_cluster_state.rs:350`, `clock_owed_cluster_values.rs:421`, `vpc_connection_contention.rs:595`, `cluster_operation_v2.rs`, `guarded_update_sequences.rs:214`, `lifecycle_sequences_settle.rs:188` and their neighbours.
Traces: kafka.cluster-replicator-updates-blocked-while-creating-rejected.

Cause 2 of 2: on a SASL/SCRAM + TLS cluster the consume leg reaches no verdict, for the same reason. (Failing checks: kafka.e2e-cluster-scram-auth-produce-consume-teardown, kafka.topic-retention-config-echoed-over-scram-message-readable.)
AWS: BatchAssociateScramSecret returns an empty UnprocessedScramSecrets, and once the credential has propagated a SCRAM-SHA-512 SASL_SSL client on BootstrapBrokerStringSaslScram produces and consumes records and its AdminClient DescribeConfigs reports the topic's `retention.ms`, `segment.ms` and `cleanup.policy`.
Emulator: the SASL/SCRAM listener serves — a SCRAM user of an associated secret authenticates over TLS, including one whose secret value Secrets Manager really sealed under a customer managed KMS key (`services/service-com-amazonaws-kafka-kafka/tests/all/scram_credential_sealed_under_a_customer_key.rs`) — and with the retention sweep disabled the retention check PASSES on replay, i.e. its produce, its SCRAM authentication and its DescribeConfigs all complete and only the consumed record was missing. Both checks are nonetheless killed at `Check timed out after 60.0s` inside their consume.
Why: each check waits out a cluster-ACTIVE and a topic-ACTIVE span at a client-sized poll interval before its data-plane leg starts, so the 60.0 s ceiling is spent before the consumer has a record to read; the association and the credential are observable, the end-to-end guarantee is not.
Fix: the same spans as cause 1, plus `TOPIC_ACTIVATION_DELAY` — what a created topic and a created cluster must be observable as, and when, for a client that polls at an AWS-sized interval. The record itself now survives the sweep: `PartitionLog` keeps its ACTIVE segment (Apache Kafka never deletes the segment it is appending to), so a just-produced record is readable whatever timestamp its batch states.
Traces: kafka.e2e-cluster-scram-auth-produce-consume-teardown, kafka.topic-retention-config-echoed-over-scram-message-readable.

## Evidence
kafka.cluster-replicator-updates-blocked-while-creating-rejected: `Error: Check timed out after 60.0s`, the traceback frame being the 20-second `time.sleep(interval_s)` inside `_wait_for_replicator_state` — the THIRD such sleep of the run.
kafka.e2e-cluster-scram-auth-produce-consume-teardown: `Error: Check timed out after 60.0s` at `received = {record.value for record in consumer}`.
kafka.topic-retention-config-echoed-over-scram-message-readable: `Error: Check timed out after 60.0s` at `for record in consumer:`; this check PASSES on replay once the retention sweep is disabled, which places the remaining cost in the spans before the data-plane leg rather than in the SASL/SCRAM path.

## Steps to Reproduce
1. Boot the emulator. CreateCluster two provisioned clusters (3 brokers, Sasl.Iam); while cluster A is CREATING probe UpdateStorage (VolumeSizeGB 11) and UpdateRebalancing and assert the 400 BadRequestException; then poll DescribeClusterV2 for ACTIVE, UpdateStorage, poll DescribeClusterOperationV2 for UPDATE_COMPLETE, CreateReplicator, probe UpdateReplicationInfo while CREATING, poll DescribeReplicator for RUNNING — each poll at a 20 s interval, and the whole check inside 60 s.
2. CreateClusterV2 a cluster with Sasl.Scram enabled and EncryptionInTransit.ClientBroker TLS, wait ACTIVE, CreateTopic with base64 Configs (retention.ms 5000, segment.ms 1000, cleanup.policy delete), create a KMS key and an AmazonMSK_* secret, BatchAssociateScramSecret, then produce and consume over BootstrapBrokerStringSaslScram and read the topic's configs back with DescribeConfigs.
3. Replay kafka.cluster-replicator-updates-blocked-while-creating-rejected, kafka.e2e-cluster-scram-auth-produce-consume-teardown and kafka.topic-retention-config-echoed-over-scram-message-readable.
