Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kafka.kafka-consumer-group-offsets-survive-a-rebalance-and-no-record-is-redelivered kafka.kafka-explicit-partition-key-routing-preserves-per-key-order kafka.kafka-four-concurrent-producers-all-land-every-record-with-distinct-offsets kafka.kafka-iam-auth-produce-consume-round-trip-over-the-native-protocol kafka.kafka-iam-policy-without-write-permission-fails-produce-and-a-read-still-succeeds kafka.kafka-record-above-max-message-bytes-rejected-and-a-just-under-record-round-trips kafka.msk-iam-group-scope-denies-a-consumer-group-join-while-an-assigned-read-succeeds kafka.msk-consumer-group-two-member-range-split-then-session-timeout-expiry-reassigns kafka.msk-cluster-configuration-num-partitions-default-is-applied-by-the-broker kafka.msk-open-monitoring-jmx-exporter-serves-prometheus-and-the-disabled-node-exporter-refuses
Issue-Shape-Id: com.amazonaws.kafka#Kafka
Issue-Key: kafka.kafka-consumer-group-offsets-survive-a-rebalance-and-no-record-is-redelivered|_invoke#8|Timeout|out = _HANDLER_NS["handler"](event, _Remaining(budget))
Issue-Key: kafka.kafka-explicit-partition-key-routing-preserves-per-key-order|_invoke#11|Timeout|out = _HANDLER_NS["handler"](event, _Remaining(budget))
Issue-Key: kafka.kafka-four-concurrent-producers-all-land-every-record-with-distinct-offsets|_invoke#13|Timeout|return out
Issue-Key: kafka.kafka-iam-auth-produce-consume-round-trip-over-the-native-protocol|_invoke#9|Timeout|out = _HANDLER_NS["handler"](event, _Remaining(budget))
Issue-Key: kafka.kafka-iam-policy-without-write-permission-fails-produce-and-a-read-still-succeeds|_invoke#10|Timeout|out = _HANDLER_NS["handler"](event, _Remaining(budget))
Issue-Key: kafka.kafka-record-above-max-message-bytes-rejected-and-a-just-under-record-round-trips|_invoke#10|Timeout|out = _HANDLER_NS["handler"](event, _Remaining(budget))
Issue-Key: kafka.msk-iam-group-scope-denies-a-consumer-group-join-while-an-assigned-read-succeeds|_invoke#14|Timeout|return out
Issue-Key: kafka.msk-consumer-group-two-member-range-split-then-session-timeout-expiry-reassigns|_invoke#2|Timeout|_out, err = proc.communicate(json.dumps(job), timeout=budget + 60)
Issue-Key: kafka.msk-cluster-configuration-num-partitions-default-is-applied-by-the-broker|run#12|AssertionError|assert entry.get("n_partitions") == partitions, (
Issue-Key: kafka.msk-open-monitoring-jmx-exporter-serves-prometheus-and-the-disabled-node-exporter-refuses|run#4|AssertionError|assert len(hosts) == _BROKERS, (
Issue-Commit: f4f68cf8a80275b1a5d028e8db7ef86de1b7f16d

# MSK native data plane: SASL/IAM bring-up never completes (8 checks reach no verdict), the broker ignores the cluster Configuration's num.partitions default, and the bootstrap string carries one endpoint for a 2-broker cluster (3 causes)

> _Note: this issue was identified against the code base at commit `f4f68cf8a80275b1a5d028e8db7ef86de1b7f16d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetBootstrapBrokers

## Description
Three net-new root causes in the emulated MSK service, all grounded in failing behavioral checks listed under `### kafka`. Related open issues (20261104T171616Z-kafka-no-kafka-broker-answers-on-the-advertised-bootstrap.md, 20261105T101629Z-kafka-every-native-msk-data-plane-check-is.md, 20261107T210159Z-kafka-three-more-msk-data-plane-checks-reach-no.md, 20261116T122712Z-kafka-msk-end-to-end-cluster-topic-scram-produce.md, 20261114T205343Z-kafka-msk-data-plane-and-lifecycle-bootstrap-string-carries.md) describe the same families but do NOT carry any of the replay keys below, so these causes are not covered by them.

=== Cause 1 of 4 ... see numbering below; this bundle has THREE causes ===

Cause 1 of 3 — the native SASL/IAM data-plane bring-up barrier never clears, so eight checks are killed before they assert anything.
AWS: CreateClusterV2 with a `serverless` request (Smithy: CreateClusterV2, POST /api/v2/clusters, input member Serverless -> ServerlessRequest, output member State -> ClusterState) answers State CREATING and the cluster settles to ACTIVE. GetBootstrapBrokers (Smithy: GET /v1/clusters/{ClusterArn}/bootstrap-brokers, output member BootstrapBrokerStringSaslIam, jsonName bootstrapBrokerStringSaslIam) then publishes a comma-separated list of host:port brokers, and a client inside the cluster's own VPC completes a SASL_SSL / AWS_MSK_IAM handshake on tcp/9098 and gets served ApiVersions, Metadata, CreateTopics, Produce, Fetch, OffsetCommit/OffsetFetch and JoinGroup/SyncGroup. Recorded behavior: every one of these checks clears that barrier and then exercises its subject (keyed partition routing, concurrent-producer offsets, consumer-group offset survival across a rebalance, broker-side max.message.bytes refusal, IAM topic-level write denial, IAM group-scope denial, range assignment plus session-timeout eviction).
Emulator: each check hangs and the harness kills it — 'Check timed out after 60.0s' for seven of them and 'Check timed out after 600.0s' for msk-consumer-group-two-member-range-split-then-session-timeout-expiry-reassigns. No HTTP or Kafka-protocol response is ever asserted on. The shared `_bring_up()` path is CreateClusterV2(Serverless) -> poll DescribeClusterV2 until State in (ACTIVE, FAILED) (the check's own internal budget is 900 s) -> GetBootstrapBrokers -> a two-phase readiness barrier (TCP accept on the advertised port, then a Kafka DescribeCluster/Metadata that the broker must actually answer). Because the whole check budget is 60 s, the barrier never clears inside it. Hypothesis: either the serverless cluster lifecycle does not settle to ACTIVE within the budget, or nothing accepts and answers the Kafka wire protocol on the endpoint the emulator advertises in bootstrapBrokerStringSaslIam.
Why: a client that creates an MSK Serverless cluster and then connects with an AWS_MSK_IAM SASL client hangs instead of producing or consuming. Every ordering, offset, consumer-group and IAM-authorization guarantee of the data plane is unobservable, and a test suite or application built against MSK blocks indefinitely rather than receiving either records or an error.
Fix: the kafka service's cluster-lifecycle state (the CreateClusterV2/DescribeClusterV2 state machine must reach ACTIVE promptly) and the emulated native broker listener that bootstrapBrokerStringSaslIam advertises — it must accept the connection on the host:port it publishes and serve the Kafka protocol over SASL/IAM.
Traces: kafka.kafka-iam-auth-produce-consume-round-trip-over-the-native-protocol, kafka.kafka-explicit-partition-key-routing-preserves-per-key-order, kafka.kafka-four-concurrent-producers-all-land-every-record-with-distinct-offsets, kafka.kafka-consumer-group-offsets-survive-a-rebalance-and-no-record-is-redelivered, kafka.kafka-record-above-max-message-bytes-rejected-and-a-just-under-record-round-trips, kafka.kafka-iam-policy-without-write-permission-fails-produce-and-a-read-still-succeeds, kafka.msk-iam-group-scope-denies-a-consumer-group-join-while-an-assigned-read-succeeds, kafka.msk-consumer-group-two-member-range-split-then-session-timeout-expiry-reassigns.

Cause 2 of 3 — the broker does not apply the attached cluster Configuration's num.partitions when CreateTopics asks for the default.
AWS: a provisioned cluster created with a ConfigurationInfo pointing at an MSK Configuration whose ServerProperties declare `num.partitions` (and `default.replication.factor`) makes the BROKER substitute those values for a CreateTopics request that asks for the server default (num_partitions = -1, replication_factor = -1). Recorded behavior: the topic created with num_partitions=-1 on a cluster whose Configuration declares num.partitions=N comes back with N partitions (ids 0..N-1), the brokers' own describe_configs reports num.partitions=N, and an explicit-count control topic in the same run keeps its explicit count.
Emulator: the topic is created with a single partition regardless of the Configuration — 'AssertionError: a topic created with num_partitions=-1 on a cluster whose Configuration declares num.partitions=5 has 1 partitions (ids [0]). The broker did not apply the cluster Configuration'. (The 5 and the 1 are this run's values; the requirement is that the default resolves to whatever the attached Configuration declares, not to a hard-coded 1.)
Why: a customer who centralises topic defaults in an MSK Configuration — the documented way to set num.partitions and default.replication.factor fleet-wide — silently gets single-partition, single-replica topics. Partitioned throughput and ordering-by-key behaviour then differ from production, and nothing in the control plane reports the discrepancy.
Fix: the emulated broker's CreateTopics path, in the kafka service's topic/broker state: when a request asks for the server default it must resolve num.partitions and default.replication.factor from the ServerProperties of the Configuration attached to the cluster (ConfigurationInfo), falling back to Kafka's own defaults only when no Configuration is attached.
Traces: kafka.msk-cluster-configuration-num-partitions-default-is-applied-by-the-broker.

Cause 3 of 3 — GetBootstrapBrokers publishes one endpoint for a multi-broker cluster.
AWS: the bootstrap strings are comma-separated lists carrying one host:port entry per broker — a cluster with N brokers across N client subnets advertises N distinct broker hosts (Smithy: GetBootstrapBrokers output members BootstrapBrokerString / BootstrapBrokerStringTls / BootstrapBrokerStringSaslIam, all `__string`; the per-broker list is the documented format). Recorded behavior: the open-monitoring check parses the bootstrap string of a 2-broker cluster and finds 2 distinct hosts, then probes the JMX exporter on each.
Emulator: the bootstrap string names a single host:port for a 2-broker cluster — 'AssertionError: the bootstrap string \'127.0.0.1:46661\' names 1 distinct broker hosts; a 2-broker cluster must advertise 2 so every exporter is probed'. The literal host and port are this environment's and are not the requirement; the defect is the CARDINALITY — one entry where the broker count demands one entry per broker.
Why: any client that derives the broker set from the bootstrap string (open-monitoring/Prometheus scrape targets, per-broker health probes, admin tooling that enumerates brokers) sees a one-broker cluster. Per-broker fan-out silently collapses to a single node, and a broker-level failure is invisible.
Fix: the GetBootstrapBrokers handler / its serializer in the kafka service — build each bootstrap string from the cluster's materialised broker list so the entry count equals NumberOfBrokerNodes, instead of emitting one endpoint. 20261114T205343Z-kafka-msk-data-plane-and-lifecycle-bootstrap-string-carries.md names this symptom but does not carry this check's replay key.
Traces: kafka.msk-open-monitoring-jmx-exporter-serves-prometheus-and-the-disabled-node-exporter-refuses.

## Evidence
Cause 1 (8 checks, no verdict):
  kafka.kafka-iam-auth-produce-consume-round-trip-over-the-native-protocol -> 'Error: Check timed out after 60.0s'
  kafka.kafka-explicit-partition-key-routing-preserves-per-key-order -> 'Error: Check timed out after 60.0s'
  kafka.kafka-four-concurrent-producers-all-land-every-record-with-distinct-offsets -> 'Error: Check timed out after 60.0s'
  kafka.kafka-consumer-group-offsets-survive-a-rebalance-and-no-record-is-redelivered -> 'Error: Check timed out after 60.0s'
  kafka.kafka-record-above-max-message-bytes-rejected-and-a-just-under-record-round-trips -> 'Error: Check timed out after 60.0s'
  kafka.kafka-iam-policy-without-write-permission-fails-produce-and-a-read-still-succeeds -> 'Error: Check timed out after 60.0s'
  kafka.msk-iam-group-scope-denies-a-consumer-group-join-while-an-assigned-read-succeeds -> 'Error: Check timed out after 60.0s'
  kafka.msk-consumer-group-two-member-range-split-then-session-timeout-expiry-reassigns -> 'Error: Check timed out after 600.0s'
  Expected: _bring_up() returns (cluster ACTIVE, bootstrapBrokerStringSaslIam reachable and serving Kafka over SASL_SSL/AWS_MSK_IAM) and the check proceeds to produce/consume asserts.
  Actual: the check never returns from bring-up and is killed; zero responses asserted on.

Cause 2:
  Expected: CreateTopics with num_partitions=-1 on a cluster whose Configuration ServerProperties set num.partitions=N yields a topic with N partitions (ids 0..N-1), confirmed by the brokers' describe_configs.
  Actual: AssertionError: a topic created with num_partitions=-1 on a cluster whose Configuration declares num.partitions=5 has 1 partitions (ids [0]). The broker did not apply the cluster Configuration

Cause 3:
  Expected: GetBootstrapBrokers on a 2-broker cluster returns a bootstrap string with 2 distinct host:port entries (one per broker).
  Actual: AssertionError: the bootstrap string '127.0.0.1:46661' names 1 distinct broker hosts; a 2-broker cluster must advertise 2 so every exporter is probed

## Steps to Reproduce
Cause 1: CreateClusterV2 with a Serverless request in a private VPC; poll DescribeClusterV2 until ACTIVE; GetBootstrapBrokers and read bootstrapBrokerStringSaslIam; from inside the cluster VPC open a TCP connection to that host:port and run a Kafka DescribeCluster/Metadata over SASL_SSL with mechanism AWS_MSK_IAM, then CreateTopics, Produce and Fetch. Observe that the sequence never completes within the check budget. Replay: kafka.kafka-iam-auth-produce-consume-round-trip-over-the-native-protocol (and the other seven listed in trace_ids).

Cause 2: CreateConfiguration with ServerProperties containing num.partitions=5; CreateClusterV2 (provisioned) with that ConfigurationInfo; wait ACTIVE; over the native protocol issue CreateTopics with num_partitions=-1 and replication_factor=-1; then describe_topics/describe_configs. Observe 1 partition instead of 5. Replay: kafka.msk-cluster-configuration-num-partitions-default-is-applied-by-the-broker.

Cause 3: CreateClusterV2 (provisioned) with NumberOfBrokerNodes=2 across 2 client subnets; wait ACTIVE; call GetBootstrapBrokers and split BootstrapBrokerString on ','. Observe a single entry. Replay: kafka.msk-open-monitoring-jmx-exporter-serves-prometheus-and-the-disabled-node-exporter-refuses.
