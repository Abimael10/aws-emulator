Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: mq.native-protocol-stomp-roundtrip-activemq
Issue-Shape-Id: com.amazonaws.mq#mq
Issue-Key: mq.native-protocol-stomp-roundtrip-activemq|run#29|PublishedUrlRejected|host, port = ctx.endpoint_host(host, int(port_str))
Issue-Commit: 6862b3e6edd2e04884ee1590246106c6221c40d8

# mq: DescribeBroker publishes a broker wire endpoint at a host the emulator does not serve (b-<id>-1.mq.<region>.localhost), so the advertised STOMP+SSL endpoint is unreachable and the ActiveMQ data plane cannot be opened

> _Note: this issue was identified against the code base at commit `6862b3e6edd2e04884ee1590246106c6221c40d8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeBroker

## Description
ONE net-new root cause, grounded in one failing behavioral check whose current replay key is carried by no open issue.

Related but NOT covering it: the open issue 20261110T231924Z-mq-the-broker-endpoints-describebroker-now-advertises-have.md carries this check under the key `_stomp_connect#26|ConnectionRefusedError|raw = socket.create_connection(...)` — i.e. a published address that was connectable-shaped but had no listener. The check now fails one step EARLIER, before any socket is created: the endpoint host the emulator publishes is not an address of the emulator at all, so it is rejected as an off-emulator target. That is a different cause (endpoint publication, not listener binding), it has a different replay key, and the suite marks this check neither ALREADY FILED nor OWNED, so it is filed here and that issue is named.

Cause 1 of 1: a RUNNING ACTIVEMQ broker's BrokerInstances[0].Endpoints entry is published with a host that the emulator itself does not answer on. The emulator serves `stomp+ssl://b-e255efbf-a58b-58c6-6cdb-2456941d52b1-1.mq.us-west-2.localhost:61614` (the broker id and region are per-run/per-config values; the shape of the defect is the synthesized `b-<broker-uuid>-1.mq.<region>.localhost` host). A client that takes the published endpoint and connects to it is pointed away from the emulator, so no STOMP session can even be attempted. Failing check: mq.native-protocol-stomp-roundtrip-activemq.

AWS: Smithy (services/service-com-amazonaws-mq-mq/smithy-model/com-amazonaws-mq-mq.json) models BrokerInstance.Endpoints (jsonName "endpoints", list of string) as "The broker's wire-level protocol endpoints", returned inside DescribeBroker's brokerInstances. In real Amazon MQ this is an address the broker is actually reachable at: for an ActiveMQ SINGLE_INSTANCE broker the list contains a `stomp+ssl://<host>:61614` URI whose host resolves and whose TCP port accepts a TLS connection, answering a STOMP 1.1 CONNECT (login/passcode = a broker user) with a CONNECTED frame, then round-tripping a SEND to /queue/<name> back to a SUBSCRIBE on the same destination as a MESSAGE frame with the body unchanged. Recorded behavior only for the frame exchange itself (the model covers the control plane, not the wire protocol). The hostname text is never a requirement — the requirement is that the address the emulator publishes is the address the emulator is reached on.

Emulator: DescribeBroker returns 200 with brokerState RUNNING and an endpoint whose host is a fabricated AWS-style name under `.localhost` that is not one of the emulator's own hosts and is not a loopback address. The replay refuses to leave the emulator: `simparity.published.PublishedUrlRejected: endpoint_host was given b-e255efbf-a58b-58c6-6cdb-2456941d52b1-1.mq.us-west-2.localhost:61614 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs an own host[:port]: the endpoint host, a documented own host or, for a loopback endpoint, a loopback address` (raised in context.py endpoint_host / _published_target). No TCP connect, no TLS handshake and no STOMP frame are reached, so the published value is the whole of the failure.

Why: every Amazon MQ client (STOMP/OpenWire/AMQP/MQTT libraries, and any application config generated from DescribeBroker) dials exactly the URI the control plane publishes. Announcing a host the emulator does not serve makes the broker's entire data plane unreachable by construction while the control plane keeps reporting a healthy RUNNING broker with endpoints: the client sees a name-resolution/connection failure that looks like a broker outage, and no produce/consume/request-reply flow can be exercised at all. It also masks the separate missing-listener gap, because the suite can never get far enough to test it.

Fix: the mq broker endpoint publication — the component that builds BrokerInstance.Endpoints (and ConsoleURL/IpAddress alongside it) for CreateBroker/DescribeBroker responses. It must emit the emulator's own reachable address for the broker's wire ports instead of a synthesized `b-<id>-1.mq.<region>.localhost` name, and it must agree with whatever the broker data plane actually binds, so the published URI and the bound listener are the same address.

Traces: mq.native-protocol-stomp-roundtrip-activemq.

## Evidence
Behavioral suite simparity-behavioral-aws-v1: 22/31 passed (71.0%); service mq 21/26 (81%), 5 failed. Four of those five are marked OWNED by open issues (mq.create-broker-rejects-password-with-colon-comma-or-equals, mq.create-broker-rejects-rabbitmq-password-with-colon-comma-or-equals, mq.rabbitmq-running-broker-user-api-refused-state-unchanged -> 20261118T104000Z-mq-3-net-new-causes-createbroker-s-invalid.md; mq.create-configuration-duplicate-name-conflict-rejected -> 20261030T170907Z-mq-configuration-operations-...). The check below is marked neither.

[mismatch] mq.native-protocol-stomp-roundtrip-activemq:
  File "context.py", line 665, in endpoint_host
    self._published_target(f"{published_host}:{port}", "endpoint_host")
  File "context.py", line 632, in _published_target
    raise PublishedUrlRejected(f"{message}: {problem[1]}", problem[0], value_class(value))
  simparity.published.PublishedUrlRejected: endpoint_host was given b-e255efbf-a58b-58c6-6cdb-2456941d52b1-1.mq.us-west-2.localhost:61614 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs an own host[:port]: the endpoint host, a documented own host or, for a loopback endpoint, a loopback address

Sequence in the check up to that point, all of which the emulator answered 200: CreateConfiguration(EngineType=ACTIVEMQ), CreateBroker(EngineType=ACTIVEMQ, DeploymentMode=SINGLE_INSTANCE, HostInstanceType=mq.t3.micro, PubliclyAccessible=false, one private subnet, one security group, one user), DescribeBroker polled to BrokerState=RUNNING, BrokerInstances non-empty, and an endpoint with scheme stomp+ssl:// present. The divergence is the HOST inside that endpoint.

Expected (AWS): the published stomp+ssl:// endpoint is an address the broker is reachable at, so the next step — TLS connect to host:61614 and STOMP CONNECT -> CONNECTED, then SEND to /queue/<q> delivered back to SUBSCRIBE as a MESSAGE frame with an identical body — can run.
Actual (emulator): the published host is `b-<broker-uuid>-1.mq.<region>.localhost`, an address the emulator does not serve; the step fails on the published value before any connection is made. The broker id, region and port in the recorded string are per-run/per-configuration values, not part of the requirement.

## Steps to Reproduce
1. Clone the repo, build and boot the emulator from the checkout, and replay the behavioral suite (simparity-behavioral-aws-v1), service mq, check mq.native-protocol-stomp-roundtrip-activemq.
2. CreateConfiguration(EngineType=ACTIVEMQ, Name=<name>-configuration).
3. CreateBroker(BrokerName=<name>, Configuration={Id,Revision} from step 2, DeploymentMode=SINGLE_INSTANCE, EngineType=ACTIVEMQ, HostInstanceType=mq.t3.micro, PubliclyAccessible=false, SubnetIds=[one private subnet], SecurityGroups=[one sg], Users=[{Username,Password}]).
4. Poll GET /v1/brokers/{BrokerId} until brokerState is RUNNING, then read brokerInstances[0].endpoints and select the URI whose scheme is stomp+ssl://.
5. Inspect that URI's host. Observed: a synthesized `b-<broker-uuid>-1.mq.<region>.localhost` name that is not an address the emulator answers on, so the endpoint is rejected as an off-emulator target and no connection is attempted. Expected: a host the emulator itself serves (its own endpoint host, a documented own host, or a loopback address for a loopback endpoint).
6. With a reachable host published, continue: TLS-connect to host:61614, send STOMP CONNECT (accept-version:1.1, login/passcode = the user from step 3) and expect CONNECTED; SEND a payload to /queue/<q>, SUBSCRIBE to the same destination with ack:auto, and expect a MESSAGE frame whose body equals the payload; DISCONNECT.
