Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kafka.cluster-create-v2-both-cluster-types-rejected kafka.msk-open-monitoring-jmx-exporter-serves-prometheus-and-the-disabled-node-exporter-refuses
Issue-Shape-Id: com.amazonaws.kafka#Kafka
Issue-Key: kafka.cluster-create-v2-both-cluster-types-rejected|run#7|AssertionError|assert message == _ONE_CLUSTER_TYPE_MESSAGE, message
Issue-Key: kafka.msk-open-monitoring-jmx-exporter-serves-prometheus-and-the-disabled-node-exporter-refuses|run#4|AssertionError|assert len(hosts) == _BROKERS, (
Issue-Commit: c18acb986501f04573c6e3384ceaad629e42cdcb

# MSK control-plane answers diverge: CreateClusterV2's both-cluster-types refusal message is generic prose, and GetBootstrapBrokers advertises fewer distinct broker hosts than the cluster has broker nodes (2 causes)

## Resolution

Cause 1 is FIXED and its replay unit passes. `CreateClusterV2` now decides the SELECTION refusal
from the two named blocks alone (`src/ops/create_cluster_v2.rs`): a body naming both cluster types
whose two blocks are each one the service would have built a cluster of that kind from states two
creates at once and is refused with MSK's mutual-exclusion sentence, while a body one of whose
blocks the service would itself have refused states a create of neither kind and keeps the
invalid-body sentence — the two recorded classes that the earlier fixed issues
20261118T020526Z-kafka-msk-control-plane-refusals-createclusterv2-s-both-cluster.md (the
behavioural one) and 20261117T050101Z-kafka-msk-createclusterv2-cluster-type-validation-refusal-carries-emulator.md
(the wire one) each pin. Neither refusal names a member, neither reads the registry, and neither
registers a cluster.

Cause 2 is BLOCKED on the read-only core, and its replay unit still fails on the same assertion.

What the cause asks for, and where each part stands:

* per-broker IDENTITY — already implemented: every broker node of a provisioned cluster carries its
  own MSK endpoint name (`src/state/broker_plane/addressing.rs`, `broker_endpoint_host`), which
  `ListNodes` reports under `BrokerNodeInfo$Endpoints`, and the bootstrap string already carries one
  `host:port` pair per broker node on the ACCESS METHOD's single shared port
  (`src/state/broker_plane.rs`, `broker_authorities`).
* per-broker ADDRESSING of the bootstrap string — BLOCKED. The check requires N DISTINCT hosts
  sharing one port, each of them an address the responder really accepts on (the wire corpus refuses
  a host the responder does not serve; see
  20261121T133835Z-kafka-msk-getbootstrapbrokers-advertises-an-unserved-host-and-createcluster.md,
  verdict `host_not_own`, which is why the per-broker `.localhost` names were withdrawn). The core
  binds every resource listener on the ONE operator-chosen interface
  (`vendor/server/src/listeners.rs:46` `bind_ip_from_env`, `:88` `bind_tokio`, `:230`
  `tcp_plaintext`) at a port it assigns itself, and a socket bound on one address accepts
  connections addressed to that address alone — so a second host beside the published one is an
  address nothing answers at, and a service may not bind a socket or choose a port of its own
  (`vendor/engine/src/lib.rs:21-32`). The core also drops the accepted connection's local address
  (`vendor/server/src/listeners.rs:319` `let (stream, _peer) = ...`), handing `NativeHandler::serve`
  the stream alone (`:136`), so even several hosts resolving to one socket could not be told apart
  as different brokers. The same constraint blocks the rest of this check: MSK's open monitoring
  serves the JMX exporter on the FIXED port 11001 and the node exporter on 11002 of each broker
  host, and a service cannot ask the core for a particular port at all.
  Required core change: let a service open a listener for a resource at an ALREADY-ASSIGNED port on
  another loopback address (so a cluster's N broker nodes share one port across N distinct loopback
  hosts), or accept on the whole loopback block and hand a native handler the local address each
  accepted connection arrived on; and let a service request the documented fixed port a protocol
  pins (11001/11002) for a per-broker surface. Then this cluster can publish one distinctly named,
  dialable broker endpoint per broker node on one shared port, and serve each broker's exporters
  there.

> _Note: this issue was identified against the code base at commit `c18acb986501f04573c6e3384ceaad629e42cdcb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateClusterV2

## Description
TWO net-new root causes on the emulated kafka (MSK) control plane. Both are grounded in failing behavioral checks the suite lists under `### kafka` that the report marks neither ALREADY FILED nor OWNED by an open issue. Related open issues name neighbouring causes for OTHER check ids and carry none of these replay keys: 20261029T202809Z-kafka-msk-error-payloads-name-the-smithy-member-instead.md and 20261102T083811Z-kafka-createconfiguration-s-unsupported-kafkaversions-refusal-message-is-the.md (refusal-message family, different operations), 20261121T065645Z-kafka-getbootstrapbrokers-allocates-one-port-per-broker-node-instead.md and 20261121T133835Z-kafka-msk-getbootstrapbrokers-advertises-an-unserved-host-and-createcluster.md (bootstrap-string family, different checks). Neither covers the two causes below.

=== Cause 1 of 2: CreateClusterV2 refuses a request carrying BOTH Provisioned and Serverless with a generic "Invalid request body" instead of the service's mutually-exclusive-cluster-type message ===
AWS: Smithy kafka CreateClusterV2 (POST /api/v2/clusters, code 200; input members ClusterName -> __stringMin1Max64 @length 1..64 required, Provisioned -> ProvisionedRequest jsonName provisioned, Serverless -> ServerlessRequest jsonName serverless; modeled error BadRequestException, errorKind client, httpError 400). The two cluster-type members are mutually exclusive, and a request that supplies both is refused 400 BadRequestException with the message `You can specify only one cluster type, either provisioned or serverless.` That text is entirely FIXED — it interpolates nothing from the request, no name, arn, id or limit — so it is reproducible verbatim. The refusal must also leave no cluster behind: ListClustersV2 with ClusterNameFilter=<the requested name> answers an empty ClusterInfoList.
Emulator: the refusal class is right (Error.Code BadRequestException) but Error.Message is the emulator's own generic body-validation prose, verbatim `Invalid request body`. The check asserts on the message and fails with `AssertionError: Invalid request body`.
Why: a client that branches on, logs, or surfaces the refusal reason sees a generic parse-failure message where AWS explains which API rule was broken. Operators debugging a CDK/SDK call that filled in both cluster types are told their body was malformed rather than that the two shapes are mutually exclusive, and any test or runbook that matches the documented text fails against the emulator. It also masks a validation hole: a generic "invalid body" is what the emulator says for arbitrary deserialization failures, so there is no evidence the mutual-exclusion rule is being enforced deliberately rather than falling out of a parse error.
Fix: the CreateClusterV2 handler's input validation in the kafka crate (services/service-com-amazonaws-kafka-kafka) — the mutual-exclusion check between the Provisioned and Serverless members must be an explicit rule that raises BadRequestException with the service's fixed message, ahead of (and distinguishable from) the generic request-body deserialization error, and must not create or list a cluster for the refused name.
Traces: kafka.cluster-create-v2-both-cluster-types-rejected.

=== Cause 2 of 2: GetBootstrapBrokers advertises ONE distinct broker host for a 2-broker provisioned cluster, varying only the port, so per-broker addressing is impossible ===
AWS: Smithy kafka GetBootstrapBrokers (GET /v1/clusters/{ClusterArn}/bootstrap-brokers, code 200; output member BootstrapBrokerString, jsonName bootstrapBrokerString, target __string). The string is a comma-separated host:port list with ONE ENTRY PER BROKER NODE, and each entry names a DIFFERENT broker: MSK gives every broker its own DNS name (the documented `b-<n>.<cluster-name>.<id>.<n>.kafka.<region>.amazonaws.com` form, one per broker index) and all of them listen on the same mechanism port (9092 for the PLAINTEXT string). So for a cluster created with NumberOfBrokerNodes=2 the number of DISTINCT hosts in BootstrapBrokerString is 2. The per-broker identity is what makes per-broker endpoints addressable at all — MSK open monitoring documents the JMX exporter on 11001 and the node exporter on 11002 *on each broker*, reached at that broker's own host name. Recorded behavior confirms the per-broker-name form; the Smithy model types the member only as a string.
Emulator: for a 2-broker cluster the PLAINTEXT bootstrap string carries two entries that share a single host and differ only in port — observed as `'127.0.0.1:37565,127.0.0.1:46483'`, which parses to 1 distinct host. (The literal host and the two port numbers are per-run values of this environment and are NOT the requirement; the requirement is that the count of distinct hosts equals NumberOfBrokerNodes and that each is that broker's own name.) The check reaches this after the cluster is ACTIVE, the string is published, the endpoint accepts TCP and the broker answers a hand-framed ApiVersions with error_code 0 and api keys 0/3/19/32 — so the control plane and listener are up; only the per-broker addressing is wrong. It fails with `AssertionError: the bootstrap string '127.0.0.1:37565,127.0.0.1:46483' names 1 distinct broker hosts; a 2-broker cluster must advertise 2 so every exporter is probed`, and everything downstream that is per-broker (both Prometheus exporters on each broker, the UpdateMonitoring enable leg) is consequently unverifiable.
Why: a client cannot reach broker N. Scrapers, dashboards and health checks that iterate the bootstrap hosts and hit a fixed per-broker port (11001/11002 for open monitoring, or any per-broker admin surface) all land on the same single broker, so a cluster where only one broker serves looks healthy and a per-broker outage is invisible. Any code that derives broker identity from the bootstrap string (replica placement checks, partition-leader affinity, rack mapping) is wrong against the emulator.
Fix: the GetBootstrapBrokers handler and the broker-endpoint allocation it reads in the kafka crate's state layer (services/service-com-amazonaws-kafka-kafka) — a provisioned cluster must materialise one distinctly named broker endpoint per broker node, with the mechanism's single port shared across them, and every bootstrap string member must list those per-broker hosts rather than one host multiplexed over N ports. This is adjacent to open issue 20261121T065645Z-kafka-getbootstrapbrokers-allocates-one-port-per-broker-node-instead.md (which reports the complementary symptom — one port per broker node instead of one per authentication mechanism — for a different check) and 20261121T133835Z-kafka-msk-getbootstrapbrokers-advertises-an-unserved-host-and-createcluster.md; neither carries this check's replay key, so this cause is filed here.
Traces: kafka.msk-open-monitoring-jmx-exporter-serves-prometheus-and-the-disabled-node-exporter-refuses.

## Evidence
Suite simparity-behavioral-aws-v1, overall 185/238 (77.7%); service heading `### kafka` 181/228 (79%).

Cause 1 — [mismatch] kafka.cluster-create-v2-both-cluster-types-rejected
  Detail, verbatim (tail): `File "check_kafka_cluster_create_v2_both_cluster_types_rejected.py", line 100, in run / assert message == _ONE_CLUSTER_TYPE_MESSAGE, message / AssertionError: Invalid request body`.
  Request driven: CreateClusterV2 with ClusterName=<generated>, Provisioned={BrokerNodeGroupInfo{InstanceType kafka.m5.large, ClientSubnets=[2 subnets in distinct AZs], SecurityGroups=[sg], StorageInfo.EbsStorageInfo.VolumeSize 10}, KafkaVersion 3.9.x, NumberOfBrokerNodes 2} AND Serverless={VpcConfigs=[{SubnetIds, SecurityGroupIds}], ClientAuthentication.Sasl.Iam.Enabled=true}.
  Expected (AWS): HTTP 400, Error.Code `BadRequestException`, Error.Message exactly `You can specify only one cluster type, either provisioned or serverless.` (wholly fixed text — no request- or state-derived substitutions); then ListClustersV2(ClusterNameFilter=<name>) returns ClusterInfoList == [].
  Actual (emulator): Error.Code `BadRequestException` (this assertion passed), Error.Message `Invalid request body`. The ListClustersV2 assertion was never reached.

Cause 2 — [mismatch] kafka.msk-open-monitoring-jmx-exporter-serves-prometheus-and-the-disabled-node-exporter-refuses
  Detail, verbatim (tail): `File "check_kafka_msk_open_monitoring_jmx_exporter_serves_prometheus_and_the_disabled_node_exporter_refuses.py", line 951, in run / assert len(hosts) == _BROKERS, ( / AssertionError: the bootstrap string '127.0.0.1:37565,127.0.0.1:46483' names 1 distinct broker hosts; a 2-broker cluster must advertise 2 so every exporter is probed`.
  Sequence driven: CreateCluster provisioned, kafka.t3.small, KafkaVersion 3.9.x, NumberOfBrokerNodes=2 across 2 private subnets, OpenMonitoring.Prometheus{JmxExporter.EnabledInBroker=true, NodeExporter.EnabledInBroker=false}, security group admitting tcp/9092, tcp/11001, tcp/11002 -> wait ACTIVE -> GetBootstrapBrokers polled for BootstrapBrokerString -> endpoint accepted TCP -> hand-framed ApiVersions answered error_code 0 advertising api keys 0, 3, 19, 32 -> assert the distinct-host count.
  Expected (AWS): BootstrapBrokerString lists one entry per broker node whose HOSTS are distinct (2 distinct hosts for NumberOfBrokerNodes=2), each a per-broker MSK broker name, all on the PLAINTEXT port; each of those hosts then serves /metrics on 11001 while 11002 refuses until UpdateMonitoring enables the node exporter.
  Actual (emulator): two entries, one distinct host, two different ports -> 1 distinct host for a 2-broker cluster. No HTTP status or error code diverges here; the divergence is in the 200 response's BootstrapBrokerString content.
  Per-run values that are NOT requirements: the host literal `127.0.0.1`, the ports `37565`/`46483`, generated cluster/topic names, cluster ARNs, VPC/subnet/security-group ids, timestamps, request ids. The requirements are: distinct-host count == NumberOfBrokerNodes, the per-broker host form, and the shared mechanism port.

Model evidence: services/service-com-amazonaws-kafka-kafka/smithy-model/com-amazonaws-kafka-kafka.json — CreateClusterV2 (POST /api/v2/clusters; Provisioned -> ProvisionedRequest, Serverless -> ServerlessRequest; BadRequestException httpError 400), GetBootstrapBrokers (GET /v1/clusters/{ClusterArn}/bootstrap-brokers; BootstrapBrokerString jsonName bootstrapBrokerString, target __string).

## Steps to Reproduce
1. Boot the emulator with kafka (and ec2 for the network) mounted.
2. Cause 1: call CreateClusterV2 with a valid ClusterName and BOTH a Provisioned body (kafka.m5.large, 2 ClientSubnets, EbsStorageInfo.VolumeSize 10, KafkaVersion 3.9.x, NumberOfBrokerNodes 2) and a Serverless body (VpcConfigs with the same subnets/security group, ClientAuthentication.Sasl.Iam.Enabled=true). Assert HTTP 400, Error.Code BadRequestException, and Error.Message exactly `You can specify only one cluster type, either provisioned or serverless.` (not `Invalid request body`). Then assert ListClustersV2(ClusterNameFilter=<that name>) returns an empty ClusterInfoList.
3. Cause 2: CreateCluster a provisioned cluster with NumberOfBrokerNodes=2 over 2 client subnets, kafka.t3.small, KafkaVersion 3.9.x, OpenMonitoring.Prometheus{JmxExporter.EnabledInBroker=true, NodeExporter.EnabledInBroker=false}. Poll DescribeCluster until ACTIVE, then GetBootstrapBrokers and split BootstrapBrokerString on ','.
4. Assert the number of DISTINCT hosts in that list equals NumberOfBrokerNodes (2), that each host is a per-broker name and that all entries share one port. Then confirm each of the 2 hosts serves Prometheus text on tcp/11001 (carrying the created topic's label and one series per partition) while tcp/11002 refuses, and that after UpdateMonitoring enables the node exporter every host serves 11002.
5. Re-replay kafka.cluster-create-v2-both-cluster-types-rejected and kafka.msk-open-monitoring-jmx-exporter-serves-prometheus-and-the-disabled-node-exporter-refuses and confirm both pass.
