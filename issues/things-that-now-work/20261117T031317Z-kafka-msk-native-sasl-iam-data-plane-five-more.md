Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kafka.kafka-consumer-group-offsets-survive-a-rebalance-and-no-record-is-redelivered kafka.kafka-explicit-partition-key-routing-preserves-per-key-order kafka.kafka-iam-auth-produce-consume-round-trip-over-the-native-protocol kafka.kafka-iam-policy-without-write-permission-fails-produce-and-a-read-still-succeeds kafka.kafka-record-above-max-message-bytes-rejected-and-a-just-under-record-round-trips
Issue-Shape-Id: com.amazonaws.kafka#Kafka
Issue-Key: kafka.kafka-consumer-group-offsets-survive-a-rebalance-and-no-record-is-redelivered|_invoke#9|Timeout|return out
Issue-Key: kafka.kafka-explicit-partition-key-routing-preserves-per-key-order|_invoke#12|Timeout|return out
Issue-Key: kafka.kafka-iam-auth-produce-consume-round-trip-over-the-native-protocol|_invoke#10|Timeout|return out
Issue-Key: kafka.kafka-iam-policy-without-write-permission-fails-produce-and-a-read-still-succeeds|_invoke#10|Timeout|return out
Issue-Key: kafka.kafka-record-above-max-message-bytes-rejected-and-a-just-under-record-round-trips|_invoke#11|Timeout|return out
Issue-Commit: 86222d03dc59131b2036c4687ef7723a3875a5e2

# MSK native SASL/IAM data plane: five more produce/consume checks reach no verdict — the bring-up barrier (cluster ACTIVE -> bootstrapBrokerStringSaslIam -> broker answers Kafka) never clears

> _Note: this issue was identified against the code base at commit `86222d03dc59131b2036c4687ef7723a3875a5e2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetBootstrapBrokers

## Description
One net-new root cause in the emulated MSK service, grounded in five failing behavioral checks the suite lists under `### kafka` and does NOT mark as already filed or owned. Related open issues describe the same family but do not carry these checks' replay keys, so they do not cover them: 20261116T210722Z-kafka-msk-native-data-plane-sasl-iam-bring-up.md (its Issue-Key entries for these five checks no longer match the keys the current replay produces), 20261104T171616Z-kafka-no-kafka-broker-answers-on-the-advertised-bootstrap.md, 20261107T210159Z-kafka-three-more-msk-data-plane-checks-reach-no.md, 20261114T205343Z-kafka-msk-data-plane-and-lifecycle-bootstrap-string-carries.md, 20261116T234316Z-kafka-msk-native-data-plane-the-concurrent-producer-check.md.

Cause 1 of 1 — the native SASL/IAM data-plane bring-up barrier never clears, so five checks are killed before they assert anything about the data plane.
AWS: CreateClusterV2 (Smithy: POST /api/v2/clusters, input member Serverless -> ServerlessRequest / Provisioned -> ProvisionedRequest, output member State -> ClusterState) answers State CREATING and the cluster settles to ACTIVE, observable through DescribeClusterV2. GetBootstrapBrokers (Smithy: GET /v1/clusters/{ClusterArn}/bootstrap-brokers, output member BootstrapBrokerStringSaslIam, jsonName bootstrapBrokerStringSaslIam, target __string) then publishes a comma-separated list of host:port brokers, and a client inside the cluster's own VPC completes a SASL_SSL / AWS_MSK_IAM handshake on the advertised port (tcp/9098 for MSK Serverless, the only listener it offers) and is served ApiVersions, Metadata/DescribeCluster, CreateTopics, Produce, Fetch, OffsetCommit/OffsetFetch and JoinGroup/SyncGroup. Recorded behavior: each of these five checks clears that barrier and then exercises its own subject — keyed partition routing, consumer-group offset survival across a rebalance, the plain IAM produce/consume round trip, an IAM policy without write permission denying a produce while a read still succeeds, and the broker-side max.message.bytes refusal with a just-under record round-tripping.
Emulator: every one of the five hangs inside the shared `_bring_up()` path and the harness kills it — 'Error: Check timed out after 60.0s' in all five cases. No HTTP response and no Kafka-protocol response is ever asserted on. `_bring_up()` is CreateClusterV2 -> poll DescribeClusterV2 until State leaves CREATING and equals ACTIVE -> GetBootstrapBrokers polled until BootstrapBrokerStringSaslIam is non-empty and parses as host:port entries (`_bootstrap`, internal budget 180 s) -> a two-phase readiness barrier `_await_data_plane` (first a TCP accept on the advertised endpoint, then a Kafka DescribeCluster/Metadata the broker must actually answer over SASL_SSL/AWS_MSK_IAM). None of the three stages completes within the check budget. Hypothesis: either the cluster lifecycle does not settle to ACTIVE promptly, or bootstrapBrokerStringSaslIam names an endpoint nothing in the emulator accepts and answers the Kafka wire protocol on (the barrier's own message distinguishes these: '<bootstrap> accepted TCP but never served a Kafka DescribeCluster over SASL/IAM ... A socket that accepts and a broker that answers are different things and this is the second').
Why: a client that creates an MSK cluster and connects with an AWS_MSK_IAM SASL client blocks indefinitely instead of producing or consuming, and receives neither records nor an error. Every ordering, offset, consumer-group and IAM-authorization guarantee of the MSK data plane — and the broker's own max.message.bytes refusal — is unobservable, so an application or test suite written against MSK hangs rather than failing fast.
Fix: the kafka service's cluster-lifecycle state (the CreateClusterV2/DescribeClusterV2 state machine must reach ACTIVE promptly) together with the emulated native broker listener that GetBootstrapBrokers advertises: the handler must publish an endpoint the emulator itself serves, and that listener must accept the connection and answer the Kafka protocol (ApiVersions, Metadata/DescribeCluster, CreateTopics, Produce, Fetch, offset and group APIs) over SASL_SSL with mechanism AWS_MSK_IAM.
Traces: kafka.kafka-consumer-group-offsets-survive-a-rebalance-and-no-record-is-redelivered, kafka.kafka-explicit-partition-key-routing-preserves-per-key-order, kafka.kafka-iam-auth-produce-consume-round-trip-over-the-native-protocol, kafka.kafka-iam-policy-without-write-permission-fails-produce-and-a-read-still-succeeds, kafka.kafka-record-above-max-message-bytes-rejected-and-a-just-under-record-round-trips.

## Evidence
Cause 1 (5 checks, no verdict — every one killed by the harness, zero responses asserted on):
  kafka.kafka-consumer-group-offsets-survive-a-rebalance-and-no-record-is-redelivered -> 'Error: Check timed out after 60.0s'
  kafka.kafka-explicit-partition-key-routing-preserves-per-key-order -> 'Error: Check timed out after 60.0s'
  kafka.kafka-iam-auth-produce-consume-round-trip-over-the-native-protocol -> 'Error: Check timed out after 60.0s'
  kafka.kafka-iam-policy-without-write-permission-fails-produce-and-a-read-still-succeeds -> 'Error: Check timed out after 60.0s'
  kafka.kafka-record-above-max-message-bytes-rejected-and-a-just-under-record-round-trips -> 'Error: Check timed out after 60.0s'

Expected: _bring_up() returns (cluster ACTIVE per DescribeClusterV2, GetBootstrapBrokers.bootstrapBrokerStringSaslIam = a comma-separated list of host:port brokers, that endpoint accepting TCP and serving a Kafka DescribeCluster over SASL_SSL/AWS_MSK_IAM from inside the cluster VPC), after which each check proceeds to its produce/consume assertions.
Actual: the call never returns from bring-up; the check is killed at its budget. The barrier's own failure wording is never even reached, so the divergence surfaces only as a timeout.

Note on values: hosts, ports, cluster names and ARNs in any one replay are environment-specific and are not the requirement; the requirement is that the endpoint GetBootstrapBrokers publishes is one the emulator serves the Kafka protocol on, and that the cluster reaches ACTIVE.

## Steps to Reproduce
1. CreateClusterV2 with a Serverless request (SASL/IAM client authentication enabled) in a private VPC with two client subnets.
2. Poll DescribeClusterV2 until ClusterInfo.State is ACTIVE.
3. Call GetBootstrapBrokers and read bootstrapBrokerStringSaslIam; check it is a comma-separated list of host:port entries.
4. From inside the cluster's VPC, open a TCP connection to that host:port, then run a Kafka DescribeCluster/Metadata over SASL_SSL with mechanism AWS_MSK_IAM, then CreateTopics, Produce and Fetch.
Observe the sequence never completes: nothing accepts-and-answers the Kafka protocol on the advertised endpoint within the budget.
Replay: kafka.kafka-iam-auth-produce-consume-round-trip-over-the-native-protocol, kafka.kafka-explicit-partition-key-routing-preserves-per-key-order, kafka.kafka-consumer-group-offsets-survive-a-rebalance-and-no-record-is-redelivered, kafka.kafka-iam-policy-without-write-permission-fails-produce-and-a-read-still-succeeds, kafka.kafka-record-above-max-message-bytes-rejected-and-a-just-under-record-round-trips.
