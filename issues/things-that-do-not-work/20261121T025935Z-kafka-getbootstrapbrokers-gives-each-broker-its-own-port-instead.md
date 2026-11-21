Issue-Severity: MEDIUM
Issue-Commit: ae12372f9a47989add7c1dd4c96799b05b75d2b4

# GetBootstrapBrokers gives each broker its own port instead of one port per authentication mechanism, so a mechanism's broker string spans two ports

> _Note: this issue was identified against the code base at commit `ae12372f9a47989add7c1dd4c96799b05b75d2b4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetBootstrapBrokers

## Description
One net-new root cause in the kafka service, grounded in a failing behavioral check that is not marked ALREADY FILED or OWNED.

Cause 1 of 1: the broker strings GetBootstrapBrokers publishes are keyed by BROKER rather than by authentication mechanism — each broker node is advertised on its own distinct port, so a single mechanism's string carries two different ports, and the port therefore does not identify the mechanism.
AWS: Smithy kafka GetBootstrapBrokers (GET /v1/clusters/{ClusterArn}/bootstrap-brokers, 200) returns BootstrapBrokerString (jsonName bootstrapBrokerString), BootstrapBrokerStringTls, BootstrapBrokerStringSaslScram and BootstrapBrokerStringSaslIam, each "a string containing one or more hostname:port pairs". MSK's listener model is one port per access method, shared by every broker: a cluster with Unauthenticated + SASL/SCRAM + SASL/IAM and EncryptionInTransit.ClientBroker=TLS_PLAINTEXT publishes four populated strings, each listing every broker node on ONE port for that mechanism, and the four mechanisms occupy four distinct ports (on real MSK 9092 plaintext, 9094 TLS, 9096 SASL/SCRAM, 9098 SASL/IAM; the particular numbers are an AWS detail, the invariant is what matters). The invariants are: within one broker string every pair uses the same port; across the four mechanism strings there are four distinct ports; and the four strings are four distinct values.
Emulator: the plaintext string for a 2-broker cluster lists the two brokers on two DIFFERENT ports (one ephemeral port per broker node on the same host), so the single-port-per-mechanism invariant fails on the first string checked, and a port no longer tells a client which mechanism it is reaching. The host and the specific port numbers are per-run values and are not the requirement; the defect is that the port varies per broker instead of per mechanism.
Why: clients and tooling treat the MSK port as the access method — security-group rules and VPC endpoint policies are written per port (9092/9094/9096/9098), monitoring and firewall templates pin those ports, and a client that re-derives a sibling endpoint by swapping the port of one broker string (a common pattern, and what per-mechanism ports guarantee) builds an address that does not exist here. Conversely, a client that admits only the mechanism's single port to its brokers will be able to reach one broker node and not the other, which looks like a partial outage rather than a configuration error.
Fix: the GetBootstrapBrokers handler's broker-string assembly in the kafka service, together with the broker listener's port allocation that backs it — one listener port per access method, shared by all broker nodes of the cluster, with distinct ports for plaintext, TLS, SASL/SCRAM and SASL/IAM (and their Public*/VpcConnectivity* siblings), rather than one port per broker.
Traces: kafka.bootstrap-brokers-multi-auth-endpoint-accuracy. Adjacent open issue that does NOT cover this (it is about the published HOST not being the emulator's own, which the replay shows is no longer the symptom): 20261109T211434Z-kafka-msk-bootstrap-endpoints-are-published-as-real-aws.md.

## Evidence
kafka.bootstrap-brokers-multi-auth-endpoint-accuracy (verdict mismatch; not ALREADY FILED, not OWNED).
Sequence: CreateClusterV2 a provisioned cluster (KafkaVersion 3.9.x, 2 brokers, ClientAuthentication {Unauthenticated: enabled, Sasl: {Scram: enabled, Iam: enabled}}, EncryptionInTransit {ClientBroker: TLS_PLAINTEXT, InCluster: true}), wait for State ACTIVE, then GetBootstrapBrokers.
Expected (AWS): BootstrapBrokerString, BootstrapBrokerStringTls, BootstrapBrokerStringSaslScram and BootstrapBrokerStringSaslIam are all populated; each lists both broker nodes on a SINGLE port; the four strings use four DISTINCT ports and are four distinct values; and, with no public access and no client VPC connectivity configured, the Public*/VpcConnectivity* members are absent rather than empty.
Actual (emulator): the plaintext string holds two broker pairs on two different ports — replayed as `plaintext endpoint used more than one port: '<host>:<portA>,<host>:<portB>'` (AssertionError raised by the check at the first invariant). The two port numbers are per-run ephemeral values; the divergence is that the port is allocated per broker node, not per authentication mechanism, so the later per-mechanism invariants (four distinct ports, four distinct strings) are not even reachable.
Smithy cited: GetBootstrapBrokers GET /v1/clusters/{ClusterArn}/bootstrap-brokers code 200, output members bootstrapBrokerString / bootstrapBrokerStringTls / bootstrapBrokerStringSaslScram / bootstrapBrokerStringSaslIam (plus the Public*/VpcConnectivity*/Ipv6 siblings).

## Steps to Reproduce
1. Boot the emulator. Confirm ListKafkaVersions reports 3.9.x ACTIVE.
2. CreateClusterV2 with Provisioned { KafkaVersion 3.9.x, NumberOfBrokerNodes 2, BrokerNodeGroupInfo { InstanceType kafka.t3.small, two ClientSubnets, EbsStorageInfo VolumeSize 100 }, ClientAuthentication { Unauthenticated: {Enabled: true}, Sasl: { Scram: {Enabled: true}, Iam: {Enabled: true} } }, EncryptionInfo { EncryptionInTransit { ClientBroker: TLS_PLAINTEXT, InCluster: true } } }.
3. Poll DescribeClusterV2 until State == ACTIVE, then call GetBootstrapBrokers.
4. For each of BootstrapBrokerString, BootstrapBrokerStringTls, BootstrapBrokerStringSaslScram and BootstrapBrokerStringSaslIam: split on ',' and assert the set of ports has size 1 (every broker of that mechanism on one port). Then assert the union of the four mechanisms' ports has size 4 and the four strings are four distinct values. Today step 4 fails on the first string: the plaintext string's two broker entries carry two different ports.
5. Replay kafka.bootstrap-brokers-multi-auth-endpoint-accuracy.
