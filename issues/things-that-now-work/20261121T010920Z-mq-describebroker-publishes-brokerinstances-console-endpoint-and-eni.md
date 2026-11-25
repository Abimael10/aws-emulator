Issue-Source: trace-sweep-divergence
Issue-Trace: mq-activemq-broker-endpoint-and-user-discovery
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: mq-activemq-broker-endpoint-and-user-discovery
Issue-Shape-Id: com.amazonaws.mq#mq
Issue-Key: mq|DescribeBroker|2xx->2xx|$.brokerInstances[*].consoleURL
Issue-Key: mq|DescribeBroker|2xx->2xx|$.brokerInstances[*].endpoints[*]
Issue-Key: mq|DescribeBroker|2xx->2xx|$.brokerInstances[*].ipAddress
Issue-Commit: 7ba8bbb6c9da2c2ac2a2ac1d9e17a82ca3c54e3b

# mq DescribeBroker publishes brokerInstances console, endpoint and ENI addresses the emulator does not serve

> _Note: this issue was identified against the code base at commit `7ba8bbb6c9da2c2ac2a2ac1d9e17a82ca3c54e3b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeBroker

## Description
ONE net-new root cause, from the only wire divergence cluster in the mq corpus that no open issue owns and no stub blocks (stub_path empty, owned_by empty). Every other cluster in this replay is owned by an open issue and is skipped.

Cause 1 of 1: DescribeBroker on a RUNNING ActiveMQ broker returns 200 whose brokerInstances[0] carries addresses that are not addresses of the emulator at all: the broker console URL and all five wire-protocol endpoints are published under a synthesized `b-<broker-uuid>-1.mq.<region>.localhost` host that the emulator does not answer on, and ipAddress is a fabricated private-subnet literal that belongs to no interface the emulator serves. Fixture mq-activemq-broker-endpoint-and-user-discovery, step task#7.

AWS: Smithy (services/service-com-amazonaws-mq-mq/smithy-model/com-amazonaws-mq-mq.json) models DescribeBroker -> GET /v1/brokers/{BrokerId}, 200, DescribeBrokerResponse.BrokerInstances (jsonName "brokerInstances") of BrokerInstance, whose members are documented ConsoleURL (jsonName "consoleURL") = "The brokers web console URL.", Endpoints (jsonName "endpoints") = "The broker's wire-level protocol endpoints." and IpAddress (jsonName "ipAddress") = "The IP address of the Elastic Network Interface (ENI) attached to the broker. Does not apply to RabbitMQ brokers." In the recording, an ActiveMQ SINGLE_INSTANCE broker publishes one authority derived from the broker's own id and region (shape: b-<broker-uuid>-1.mq.<region>.amazonaws.com), used by consoleURL on :8162 and by the five wire endpoints ssl 61617, amqp+ssl 5671, stomp+ssl 61614, mqtt+ssl 8883 and wss 61619, and ipAddress is the broker ENI's address inside the broker's subnet. The schemes, the port set, the ordering and the per-broker derivation are the requirement and the emulator already satisfies them; what AWS additionally guarantees - and what the comparison asserts - is that the published address is an address the broker is actually REACHED at, i.e. for the emulator one of its own hosts (its endpoint host, a documented own host, or a loopback address for a loopback endpoint). The concrete hostname, DNS suffix, port numbers, broker uuid, region and IP literal in the recording are that run's environment values and are not themselves requirements.

Emulator: replaying step task#7 (DescribeBroker, expected 200, got 200 - no status or error divergence), the replay flags every address field of brokerInstances[0] as host_not_own: consoleURL 'https://b-<broker-uuid>-1.mq.<region>.localhost:8162' and endpoints[0..4] 'ssl://b-<broker-uuid>-1.mq.<region>.localhost:61617', 'amqp+ssl://...:5671', 'stomp+ssl://...:61614', 'mqtt+ssl://...:8883', 'wss://...:61619' - a fabricated AWS-style name under .localhost that is neither one of the emulator's own hosts nor a loopback address - plus brokerInstances[0].ipAddress as a value_mismatch against the shape expectation, returning a made-up private address (recorded run: '10.123.100.18'), which is likewise not an address of any interface the emulator serves.

Why: every Amazon MQ client (OpenWire/AMQP/STOMP/MQTT/WSS libraries, generated application config, CloudFormation outputs, event-source mappings) dials exactly the authority the control plane publishes, and operators open the console URL verbatim. A client that follows this 200 response is pointed away from the emulator, so the broker's whole data plane and its web console are unreachable by construction while DescribeBroker keeps reporting a healthy RUNNING broker with endpoints; the failure surfaces to the client as DNS/connect failure that looks like a broker outage. ipAddress is documented as the broker ENI's address and is used to drive subnet/security-group reachability assertions, which are meaningless when the value belongs to nothing. Related but NOT covering this: the open behavioral issue 20261121T003755Z-mq-describebroker-publishes-a-broker-wire-endpoint-at.md reports the same .localhost host for the STOMP endpoint from the behavioral suite, and 20261110T231924Z-mq-the-broker-endpoints-describebroker-now-advertises-have.md reports the advertised endpoints having no listener; neither carries this wire cluster's replay key (owned_by is empty), and this divergence also covers consoleURL and ipAddress, which neither names. The previously resolved 20261117T163254Z-mq-describebroker-publishes-broker-instance-addresses-that-do.md asked for broker-derived authorities - that part now works; the derived name is simply not served.

Fix: the mq brokerInstances builder in the DescribeBroker response construction (and the CreateBroker/ListBrokers paths that assemble the same BrokerInstance), together with the broker endpoint-publication helper that synthesizes the host. It must keep the per-broker, per-engine derivation and the scheme/port table it already has while emitting an authority the emulator is actually reachable on for the console and each wire port, and populate ipAddress from the address of the interface the emulator really serves the broker on (omitting ipAddress for RABBITMQ brokers, per the model's documentation). The published address must agree with whatever the broker data plane binds.

Traces: mq-activemq-broker-endpoint-and-user-discovery#task#7.

## Evidence
Wire corpus simparity-wire-aws-v1 replayed against the emulator booted from this checkout: 68 steps, 52 passed, 16 mismatched, 0 errored, 0 blocked, score 76.5%, 12 divergence clusters - all service mq; 11 are owned by open issues and are skipped. The one unowned cluster (divergence_key service=mq|DescribeBroker|status=200->200|keys=$.brokerInstances[0].consoleURL,$.brokerInstances[0].endpoints[0..4],$.brokerInstances[0].ipAddress; stub_path empty, owned_by empty):

fixture mq-activemq-broker-endpoint-and-user-discovery, step task#7, DescribeBroker, expected_status 200, actual_status 200, verdict mismatch:
  $.brokerInstances[0].ipAddress: value_mismatch (expected '<shaped-not-asserted>', got '10.123.100.18')
  $.brokerInstances[0].consoleURL: host_not_own (expected 'https://b-fdeea551-7a4b-091c-67d5-ab79d4466d50-1.mq.us-west-2.amazonaws.com:8162', got 'https://b-fdeea551-7a4b-091c-67d5-ab79d4466d50-1.mq.us-west-2.localhost:8162')
  $.brokerInstances[0].endpoints[0]: host_not_own (expected 'ssl://b-fdeea551-...-1.mq.us-west-2.amazonaws.com:61617', got 'ssl://b-fdeea551-...-1.mq.us-west-2.localhost:61617')
  $.brokerInstances[0].endpoints[1]: host_not_own (amqp+ssl, :5671, same host substitution)
  $.brokerInstances[0].endpoints[2]: host_not_own (stomp+ssl, :61614, same host substitution)
  $.brokerInstances[0].endpoints[3]: host_not_own (mqtt+ssl, :8883, same host substitution)
  $.brokerInstances[0].endpoints[4]: host_not_own (wss, :61619, same host substitution)

Expected (AWS): the console URL and the five wire endpoints are published at an authority the broker is actually reached at, and ipAddress is the address of the interface serving the broker - so a client that takes the published values can connect.
Actual (emulator): the authority is a synthesized `b-<broker-uuid>-1.mq.<region>.localhost` name the emulator does not serve, and ipAddress is a fabricated private address. The broker uuid 'b-fdeea551-7a4b-091c-67d5-ab79d4466d50', the region 'us-west-2', the ports and the IP literal '10.123.100.18' are that run's values, not part of the requirement; the schemes, port set and ordering already match.
Model citation: BrokerInstance.ConsoleURL = "The brokers web console URL."; BrokerInstance.Endpoints = "The broker's wire-level protocol endpoints."; BrokerInstance.IpAddress = "The IP address of the Elastic Network Interface (ENI) attached to the broker. Does not apply to RabbitMQ brokers."

## Steps to Reproduce
1. Clone the repo, build and boot the emulator from the checkout.
2. Replay the wire corpus simparity-wire-aws-v1, fixture mq-activemq-broker-endpoint-and-user-discovery, through step task#7.
3. The fixture creates an ActiveMQ broker (SINGLE_INSTANCE, mq.t3.micro, one user), polls GET /v1/brokers/{BrokerId} until brokerState is RUNNING, and at task#7 reads brokerInstances[0].
4. Inspect brokerInstances[0].endpoints (five URIs: ssl 61617, amqp+ssl 5671, stomp+ssl 61614, mqtt+ssl 8883, wss 61619) and brokerInstances[0].consoleURL (https, 8162). Observed: every one uses the host `b-<broker-uuid>-1.mq.<region>.localhost`, which the emulator does not answer on, so the published target is rejected as off-emulator before any connection is attempted. Expected: an authority the emulator itself serves (its own endpoint host, a documented own host, or a loopback address for a loopback endpoint), still derived per broker.
5. Inspect brokerInstances[0].ipAddress. Observed: a fabricated private address (10.123.100.18 in this run) matching no interface the emulator serves. Expected: the address of the interface the emulator actually serves this broker on; absent for a RABBITMQ broker.
6. Confirm the fix by taking the published host:port straight from the response and opening a TCP/TLS connection to it, and by opening the consoleURL: both must land on the emulator.
