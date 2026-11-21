Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kafka.bootstrap-brokers-multi-auth-endpoint-accuracy
Issue-Shape-Id: com.amazonaws.kafka#Kafka
Issue-Key: kafka.bootstrap-brokers-multi-auth-endpoint-accuracy|run#13|AssertionError|assert len(_ports(plaintext)) == 1, f"plaintext endpoint used more than one port: {plaintext!r}"
Issue-Commit: 8482ca1a85e7d388da6ae1cfee3bd2042dc26182

# GetBootstrapBrokers allocates one port per broker node instead of one port per authentication mechanism, so a single mechanism's broker string spans two ports

> _Note: this issue was identified against the code base at commit `8482ca1a85e7d388da6ae1cfee3bd2042dc26182`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetBootstrapBrokers

## Description
One net-new root cause in the kafka service, grounded in the only failing behavioral check in this sweep that the report marked neither ALREADY FILED nor OWNED by an open issue.

NOTE ON OVERLAP: open issue 20261121T025935Z-kafka-getbootstrapbrokers-gives-each-broker-its-own-port-instead.md describes this same cause and names this same check, but the replay report does not mark this check OWNED by it, so the check is still live and is re-filed here with its replay key. If that issue is still open when this is triaged, treat the two as one work item and close this one as its duplicate; do not fix it twice.

Cause 1 of 1: the hostname:port pairs GetBootstrapBrokers publishes are keyed by BROKER NODE rather than by access method. Each broker of a cluster is advertised on its own distinct port, so one mechanism's broker string carries two different ports and the port no longer identifies the mechanism.

AWS: Smithy kafka GetBootstrapBrokers — GET /v1/clusters/{ClusterArn}/bootstrap-brokers, success code 200; input ClusterArn (@httpLabel, required); output members bootstrapBrokerString, bootstrapBrokerStringTls, bootstrapBrokerStringSaslScram, bootstrapBrokerStringSaslIam plus the bootstrapBrokerStringPublic* / bootstrapBrokerStringVpcConnectivity* / *Ipv6 siblings, each documented as "a string containing one or more hostname:port pairs". MSK's listener model is one port per access method, shared by every broker node of the cluster: for a provisioned cluster with ClientAuthentication {Unauthenticated enabled, Sasl {Scram enabled, Iam enabled}} and EncryptionInTransit.ClientBroker = TLS_PLAINTEXT, all four of bootstrapBrokerString, bootstrapBrokerStringTls, bootstrapBrokerStringSaslScram and bootstrapBrokerStringSaslIam are populated; within ONE of those strings every pair uses the SAME port; across the four strings there are four DISTINCT ports; and the four strings are four distinct values. (On real MSK those ports are 9092 plaintext / 9094 TLS / 9096 SASL/SCRAM / 9098 SASL/IAM; the specific numbers are an AWS detail and are not the requirement — the per-mechanism-single-port invariant is.) With no public access and no client VPC connectivity configured, the Public* and VpcConnectivity* members are absent from the payload, not present and empty.

Emulator: for a 2-broker cluster the plaintext string lists the two broker nodes on two DIFFERENT ports on the same host — replayed as `plaintext endpoint used more than one port: '<host>:<portA>,<host>:<portB>'`. The host and the two port numbers are per-run ephemeral values and are not themselves the divergence; the divergence is that a port is allocated per broker node instead of per access method, which breaks the first invariant and makes the later ones (four distinct ports across mechanisms, four distinct strings) unreachable.

Why: clients and infrastructure treat the MSK port as the access method. Security-group rules, VPC endpoint policies, firewall and monitoring templates are written per port; a client that derives a sibling endpoint by swapping the port of a known broker string — the pattern per-mechanism ports guarantee — builds an address that does not exist here. Conversely a client that admits only the mechanism's single port to its brokers reaches one broker node and not the other, which presents as a partial broker outage rather than a configuration error.

Fix: the GetBootstrapBrokers handler's broker-string assembly in the kafka service, together with the broker listener port allocation that backs it — one listener port per access method (plaintext, TLS, SASL/SCRAM, SASL/IAM, and their Public*/VpcConnectivity* siblings), shared by every broker node of the cluster, instead of one port per broker node.

Traces: kafka.bootstrap-brokers-multi-auth-endpoint-accuracy. Adjacent open issue that does NOT cover this cause: 20261109T211434Z-kafka-msk-bootstrap-endpoints-are-published-as-real-aws.md (that one is about the published HOST not being an address the emulator serves; the replay shows the host is now local, and the surviving symptom is the per-broker port).

## Evidence
Failing check: kafka.bootstrap-brokers-multi-auth-endpoint-accuracy (verdict: mismatch; not marked ALREADY FILED, not marked OWNED).

Sequence replayed: ListKafkaVersions (3.9.x must be ACTIVE) -> CreateClusterV2 with Provisioned { KafkaVersion 3.9.x, NumberOfBrokerNodes 2, BrokerNodeGroupInfo { InstanceType kafka.t3.small, two ClientSubnets in distinct AZs, StorageInfo.EbsStorageInfo.VolumeSize 100 }, ClientAuthentication { Unauthenticated {Enabled true}, Sasl { Scram {Enabled true}, Iam {Enabled true} } }, EncryptionInfo { EncryptionInTransit { ClientBroker TLS_PLAINTEXT, InCluster true } } } -> poll DescribeClusterV2 to State ACTIVE -> GetBootstrapBrokers.

Expected (AWS): HTTP 200 with bootstrapBrokerString, bootstrapBrokerStringTls, bootstrapBrokerStringSaslScram and bootstrapBrokerStringSaslIam all populated; each string lists both broker nodes on a single port; the union of the four mechanisms' ports has size 4; the four strings are four distinct values; and bootstrapBrokerStringPublicTls / PublicSaslScram / PublicSaslIam / VpcConnectivityTls / VpcConnectivitySaslScram / VpcConnectivitySaslIam are absent from the response.

Actual (emulator): the plaintext string holds two broker pairs on two different ports, so the check fails at its first invariant with `AssertionError: plaintext endpoint used more than one port: '127.0.0.1:41221,127.0.0.1:43103'` (check_kafka_bootstrap_brokers_multi_auth_endpoint_accuracy.py line 44). The host `127.0.0.1` and the ports 41221/43103 are per-run values that fill the template; the requirement they violate is that all pairs within one mechanism's string share one port and that the four mechanisms use four distinct ports.

Smithy cited: GetBootstrapBrokers, GET /v1/clusters/{ClusterArn}/bootstrap-brokers, code 200; output members bootstrapBrokerString / bootstrapBrokerStringTls / bootstrapBrokerStringSaslScram / bootstrapBrokerStringSaslIam (+ Public*/VpcConnectivity*/Ipv6 siblings); errors BadRequestException 400, ConflictException 409, ForbiddenException 403, UnauthorizedException 401, InternalServerErrorException 500.

## Steps to Reproduce
1. Boot the emulator and confirm ListKafkaVersions reports 3.9.x with Status ACTIVE.
2. CreateClusterV2 with Provisioned { KafkaVersion 3.9.x, NumberOfBrokerNodes 2, BrokerNodeGroupInfo { InstanceType kafka.t3.small, two ClientSubnets from distinct AZs, StorageInfo.EbsStorageInfo.VolumeSize 100 }, ClientAuthentication { Unauthenticated {Enabled true}, Sasl { Scram {Enabled true}, Iam {Enabled true} } }, EncryptionInfo { EncryptionInTransit { ClientBroker TLS_PLAINTEXT, InCluster true } } }.
3. Poll DescribeClusterV2 until ClusterInfo.State == ACTIVE.
4. Call GetBootstrapBrokers with that ClusterArn.
5. For each of BootstrapBrokerString, BootstrapBrokerStringTls, BootstrapBrokerStringSaslScram and BootstrapBrokerStringSaslIam: split on ',' and assert the set of ports (the part after the last ':') has size 1. Then assert the union of the four mechanisms' port sets has size 4, that the four strings are four distinct values, and that the Public*/VpcConnectivity* members are absent.
6. Today step 5 fails on the first string: the plaintext string's two broker entries carry two different ports (one ephemeral port per broker node).
7. Replay kafka.bootstrap-brokers-multi-auth-endpoint-accuracy.
