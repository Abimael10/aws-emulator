Issue-Source: trace-sweep-divergence
Issue-Trace: kafka-serverless-iam-auth-without-security-group-tags-brokers-listed-and-not-found
Issue-Step: task#14
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: kafka-serverless-iam-auth-without-security-group-tags-brokers-listed-and-not-found kafka-serverless-iam-bootstrap-broker-discovery kafka-rejected-provisioning-requests
Issue-Shape-Id: com.amazonaws.kafka#Kafka
Issue-Key: kafka|GetBootstrapBrokers|2xx->2xx|$.bootstrapBrokerStringSaslIam
Issue-Key: kafka|CreateCluster|4xx->4xx|$.invalidParameter
Issue-Key: kafka|CreateCluster|4xx->4xx|$.message
Issue-Commit: 24b6d630182dd52ee8f6e18223e41bb2878685a3

# MSK GetBootstrapBrokers advertises an unserved host and CreateCluster's version gate preempts the client-subnet refusal (2 causes)

> _Note: this issue was identified against the code base at commit `24b6d630182dd52ee8f6e18223e41bb2878685a3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetBootstrapBrokers

## Description
TWO net-new root causes in the kafka (MSK) control plane, from the wire replay of simparity-wire-aws-v1. Both are the only clusters in this sweep the report marked neither blocked by a stub nor OWNED by an open issue (every other kafka cluster in the run carries an owned_by). A fixer must close BOTH before submitting.

=== Cause 1 of 2: GetBootstrapBrokers publishes a broker host the emulator does not serve (fixture kafka-serverless-iam-auth-without-security-group-tags-brokers-listed-and-not-found, step task#14; same cluster also kafka-serverless-iam-bootstrap-broker-discovery#task#8) ===

AWS: Smithy (services/service-com-amazonaws-kafka-kafka/smithy-model/com-amazonaws-kafka-kafka.json) GetBootstrapBrokers = GET /v1/clusters/{ClusterArn}/bootstrap-brokers, success code 200, input ClusterArn (@httpLabel, required); output members bootstrapBrokerString / bootstrapBrokerStringTls / bootstrapBrokerStringSaslScram / bootstrapBrokerStringSaslIam plus the Public*, VpcConnectivity* and *Ipv6 siblings, each a string of one or more hostname:port pairs. On real MSK each pair names a broker endpoint the service actually answers on, so the contract a client depends on is that the advertised host:port is reachable and is served by the same deployment that returned it. WHICH host an emulator may legitimately advertise is recorded behavior only: the corpus asserts the response is shaped (<shaped-not-asserted>) and that the advertised host is one the responder owns, and the replay's verdict for this step is host_not_own.

Emulator: the SASL/IAM broker string is a synthesized MSK-shaped DNS name re-rooted under .localhost with an ephemeral port — of the form b-<n>.<cluster-name>-<suffix>.<id>.c7.kafka.<region>.localhost:<port> — which is NOT the address the emulator is reached on (the emulator answers /__admin/health and accepts on its own data listener host:port). The specific name and port in the recording are per-run values and are not the requirement; the divergence is that the host family advertised is one the emulator does not serve, so the value is unusable wherever it is returned.

Why: a client does the one thing this API exists for — take bootstrapBrokerStringSaslIam and hand it to a Kafka client as bootstrap.servers. Against this emulator that name does not resolve (or resolves to a loopback port nothing is listening on), so every produce/consume attempt fails at connection setup while the control plane reports the cluster ACTIVE. The failure surfaces as a broker outage, not as a configuration error, and no client-side change recovers it.

Fix: the GetBootstrapBrokers handler's broker-endpoint assembly in the kafka service (the component that renders the advertised host for each bootstrapBrokerString* member), together with the listener registry it reads the brokers' real bind addresses from: advertise the host:port the emulator is actually reached on rather than a synthesized AWS-shaped name.

Related open issues that do NOT cover this cause (neither carries this replay key, so the step is still live): 20261109T211434Z-kafka-msk-bootstrap-endpoints-are-published-as-real-aws.md (published endpoints as real AWS broker DNS names) and 20261121T065645Z-kafka-getbootstrapbrokers-allocates-one-port-per-broker-node-instead.md (per-broker-node rather than per-mechanism ports). This cause is the advertised HOST not being one the responder serves.

Traces: kafka-serverless-iam-auth-without-security-group-tags-brokers-listed-and-not-found#task#14, kafka-serverless-iam-bootstrap-broker-discovery#task#8.

=== Cause 2 of 2: CreateCluster's Kafka-version/instance-type gate runs ahead of client-subnet validation, so a provisioned create is refused under the wrong invalidParameter with the wrong message (fixture kafka-rejected-provisioning-requests, step task#1) ===

AWS: Smithy CreateCluster = POST /v1/clusters, code 200; required input members brokerNodeGroupInfo (BrokerNodeGroupInfo, with required clientSubnets and instanceType), clusterName, kafkaVersion (__stringMin1Max128) and numberOfBrokerNodes; errors BadRequestException (400) / ConflictException (409) / ForbiddenException (403) / UnauthorizedException (401) / TooManyRequestsException (429) / InternalServerErrorException (500) / ServiceUnavailableException (503). BadRequestException carries invalidParameter ("The parameter that caused the error") and message ("The description of the error"). The model enumerates no concrete Kafka versions, so which versions a create may name is recorded behavior only: the recording shows AWS ACCEPTS the version this request names for a standard broker instance type and refuses the request instead because its client subnets do not exist, answering HTTP 400, x-amzn-errortype BadRequestException, invalidParameter "brokerNodeGroupInfo" and message "One or more of the specified client subnets don’t exist." That message is entirely fixed text (one sentence, with a U+2019 apostrophe in "don’t") and has no variable parts; the variable part of the answer is the invalidParameter value, which must name the request member at fault — the wire name brokerNodeGroupInfo, since the offending value sits in brokerNodeGroupInfo.clientSubnets.

Emulator: the same request is refused 400 BadRequestException with invalidParameter "kafkaVersion" and message "Standard instance types are not supported for Kafka version 3.6.0. Valid values: [3.8.x, 3.8.x.kraft, 3.9.x, 3.9.x.kraft, 4.0.x.kraft, 4.1.x.kraft]". In that emulator message the fixed text is "<category> instance types are not supported for Kafka version <version>. Valid values: [<versions>]" with the category derived from the request's instanceType, the version taken from the request's kafkaVersion and the bracketed list taken from the emulator's own offering table; none of it is a per-run value. Two things diverge: the emulator refuses a version its own ListKafkaVersions page publishes as ACTIVE, and because that gate short-circuits first, the client-subnet existence check never runs and is never reported.

Why: the refusal steers the client at the wrong request member. A client reading invalidParameter edits kafkaVersion, retries with a version from the emulator's bracketed list, and the create then SUCCEEDS with subnets that do not exist — the condition AWS rejects is silently accepted, and the cluster it builds is unreachable. Conversely a client that legitimately provisions on an ACTIVE catalog version below 3.8.x can never create a cluster at all.

Fix: the CreateCluster handler's input validation in the kafka service — admit every Kafka version the service's own catalog publishes ACTIVE for the requested broker category, and validate client-subnet existence, reporting that failure as BadRequestException with invalidParameter "brokerNodeGroupInfo" and AWS's fixed message text. Ordering matters as much as the verdict: the subnet check must not be preempted by the version gate.

Related open issue that does NOT cover this cause: 20261121T113417Z-kafka-createcluster-createclusterv2-refuse-active-kafka-versions-the-service.md reports the narrow version gate from the BEHAVIORAL suite and carries no wire replay key, and it says nothing about the missing client-subnet refusal or the invalidParameter the refusal must name; the replay does not mark this step owned by it.

Traces: kafka-rejected-provisioning-requests#task#1.

## Evidence
Replay summary: trace_set_id simparity-wire-aws-v1, emulator_up true, 51 fixtures replayed, 186 steps, 153 passed, 33 mismatched, score 82.3%, 23 divergence clusters — all service=kafka. 21 clusters carry a non-empty owned_by and are skipped; the two below carry owned_by [] and stub_path "".

Cause 1 — GetBootstrapBrokers, cluster key service=kafka|GetBootstrapBrokers|status=200->200|keys=$.bootstrapBrokerStringSaslIam; expected status 200, actual 200; affected_count 2:
  verdict: mismatch
  $.bootstrapBrokerStringSaslIam: host_not_own (expected '<shaped-not-asserted>', got 'b-1.sim2real-probe-msk-0798de51.d28830.c7.kafka.us-west-2.localhost:46467')
  Expected (AWS/corpus): a hostname:port pair the responder serves, i.e. the address the client reached this emulator on.
  Actual (emulator): an MSK-shaped DNS name re-rooted under .localhost with an ephemeral port, which is not an address this emulator answers on. The literal name/port are per-run; the requirement is host ownership.
  Steps: kafka-serverless-iam-auth-without-security-group-tags-brokers-listed-and-not-found#task#14, kafka-serverless-iam-bootstrap-broker-discovery#task#8.

Cause 2 — CreateCluster, cluster key service=kafka|CreateCluster|status=400->400|keys=$.invalidParameter,$.message; expected status 400, actual 400; affected_count 1:
  verdict: mismatch
  $.invalidParameter: value_mismatch (expected 'brokerNodeGroupInfo', got 'kafkaVersion')
  $.message: value_mismatch (expected 'One or more of the specified client subnets don’t exist.', got 'Standard instance types are not supported for Kafka version 3.6.0. Valid values: [3.8.x, 3.8.x.kraft, 3.9.x, 3.9.x.kraft, 4.0.x.kraft, 4.1.x.kraft]')
  Expected (AWS): HTTP 400, x-amzn-errortype BadRequestException, invalidParameter "brokerNodeGroupInfo", message "One or more of the specified client subnets don’t exist." — the whole sentence is fixed text, no variable parts.
  Actual (emulator): HTTP 400, BadRequestException, invalidParameter "kafkaVersion", message templated "<category> instance types are not supported for Kafka version <version>. Valid values: [<offering list>]" filled from the request's instanceType and kafkaVersion plus the emulator's own offering table; the client-subnet existence check is never reached.
  Step: kafka-rejected-provisioning-requests#task#1.

Smithy cited (services/service-com-amazonaws-kafka-kafka/smithy-model/com-amazonaws-kafka-kafka.json): GetBootstrapBrokers GET /v1/clusters/{ClusterArn}/bootstrap-brokers code 200, output bootstrapBrokerString* members (hostname:port pairs); CreateCluster POST /v1/clusters code 200, required brokerNodeGroupInfo (BrokerNodeGroupInfo.clientSubnets required, instanceType required), clusterName, kafkaVersion, numberOfBrokerNodes; BadRequestException httpError 400 with members invalidParameter + message.

## Steps to Reproduce
Cause 1 (GetBootstrapBrokers advertises an unserved host):
1. Boot the emulator from this checkout; confirm the kafka service answers.
2. Replay fixture kafka-serverless-iam-auth-without-security-group-tags-brokers-listed-and-not-found and stop at step task#14 (or replay kafka-serverless-iam-bootstrap-broker-discovery to step task#8): create a serverless/SASL-IAM cluster, poll it to ACTIVE, then call GetBootstrapBrokers on its ClusterArn.
3. Read bootstrapBrokerStringSaslIam. Today it is a b-<n>.<cluster>.<id>.c7.kafka.<region>.localhost:<port> name. Compare the host:port against the address the request was sent to.
4. Try to open a TCP connection to every advertised pair (or hand the string to a Kafka client as bootstrap.servers). Expect a reachable broker served by this emulator; observe a name/port the emulator does not serve.

Cause 2 (CreateCluster's version gate preempts the client-subnet refusal):
1. Replay fixture kafka-rejected-provisioning-requests step task#1: POST /v1/clusters with clusterName set, numberOfBrokerNodes 2, kafkaVersion = an ACTIVE row from this emulator's own GET /v1/kafka-versions below 3.8.x (the recording uses 3.6.0), and brokerNodeGroupInfo = { instanceType: a standard type such as kafka.m5.large, clientSubnets: two subnet ids that do NOT exist }.
2. Expect HTTP 400 BadRequestException with invalidParameter "brokerNodeGroupInfo" and message "One or more of the specified client subnets don’t exist.".
3. Observe HTTP 400 BadRequestException with invalidParameter "kafkaVersion" and the "Standard instance types are not supported for Kafka version ... Valid values: [...]" message instead.
4. Confirm the second half of the cause: retry the SAME request with kafkaVersion changed to one of the emulator's listed values (e.g. 3.9.x), leaving the nonexistent clientSubnets unchanged. The create is accepted instead of being refused for the subnets.
5. Re-replay kafka-rejected-provisioning-requests#task#1 and the two cause-1 steps to confirm the fix.
