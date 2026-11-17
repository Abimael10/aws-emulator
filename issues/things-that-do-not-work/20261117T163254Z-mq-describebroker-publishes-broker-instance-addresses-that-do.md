Issue-Source: trace-sweep-divergence
Issue-Trace: mq-activemq-broker-endpoint-and-user-discovery
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: mq-activemq-broker-endpoint-and-user-discovery
Issue-Shape-Id: com.amazonaws.mq#mq
Issue-Key: mq|DescribeBroker|2xx->2xx|$.brokerInstances[*].endpoints[*]
Issue-Key: mq|DescribeBroker|2xx->2xx|$.brokerInstances[*].ipAddress
Issue-Commit: 077021ed17173fe4a565f7526f07b6650a74c582

# mq DescribeBroker publishes broker-instance addresses that do not identify the broker

> _Note: this issue was identified against the code base at commit `077021ed17173fe4a565f7526f07b6650a74c582`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeBroker

## Description
ONE net-new root cause, from the only wire divergence in the mq corpus that no open issue owns: every address field of DescribeBroker's brokerInstances[] is emitted as the bare loopback literal instead of being derived from the broker's own identity, so the response carries no resource identity in brokerInstances[0].endpoints[] or brokerInstances[0].ipAddress.

Cause 1 of 1: DescribeBroker on a RUNNING ActiveMQ broker returns 200 with the right endpoint count, schemes and ports, but each endpoint's authority is a fixed host that is the same for every broker in the account, and ipAddress is the loopback address.

AWS: Smithy (services/service-com-amazonaws-mq-mq/smithy-model/com-amazonaws-mq-mq.json), DescribeBroker -> GET /v1/brokers/{BrokerId}, 200, output DescribeBrokerResponse.BrokerInstances (jsonName "brokerInstances") of BrokerInstance, whose members are documented as Endpoints (jsonName "endpoints") = "The broker's wire-level protocol endpoints", IpAddress (jsonName "ipAddress") = "The IP address of the Elastic Network Interface (ENI) attached to the broker. Does not apply to RabbitMQ brokers", and ConsoleURL (jsonName "consoleURL") = "The brokers web console URL". Amazon MQ builds each ActiveMQ endpoint as <scheme>://<broker-host>:<port> where the host is derived from the broker instance's own identity and its region, and the four ActiveMQ wire schemes/ports are ssl 61617, amqp+ssl 5671, stomp+ssl 61614, mqtt+ssl 8883. In the recording the host component is b-<brokerId-without-the-"b-" prefix>-1.mq.<region>.amazonaws.com, i.e. the broker id and region appear verbatim inside the host, and ipAddress is the broker ENI's address in the broker's subnet, never the loopback address. The requirement here is the DERIVATION, not the recorded literal: the four scheme/port pairs must be present (they already are), the authority must be a host unique to and derived from THIS broker (so two brokers in one account never advertise the same authority and the broker id is recoverable from the endpoint), and ipAddress must be an address of the broker's own interface rather than 127.0.0.1. The concrete hostname, DNS suffix and IP value are environment values and are not themselves the requirement.

Emulator: replaying mq-activemq-broker-endpoint-and-user-discovery step task#7 (DescribeBroker, HTTP 200 expected and 200 returned), the replay reports brokerInstances[0].endpoints[0..3] as resource_not_identified -- 'ssl://127.0.0.1:61617', 'amqp+ssl://127.0.0.1:5671', 'stomp+ssl://127.0.0.1:61614', 'mqtt+ssl://127.0.0.1:8883' where the broker-derived authority belongs -- and brokerInstances[0].ipAddress as a value mismatch against the shape expectation, returning '127.0.0.1'. The broker id and region are nowhere in the published addresses, and the ActiveMQ ipAddress is the loopback address rather than an interface address.

Why: a client that uses DescribeBroker for endpoint discovery cannot tell two brokers apart. Every broker in the account resolves to the identical authority, so code that keys a connection pool, a client config, a CloudFormation output or an event-source mapping by the published endpoint collapses distinct brokers onto one entry, and nothing can be routed per broker. Code that parses the broker id or region back out of the endpoint host -- a common pattern for correlating a connection with its control-plane resource and for region-aware client construction -- gets nothing to parse. ipAddress is documented as the broker ENI's address and is used to drive security-group and subnet reachability assertions; reporting the loopback address makes any such check meaningless for an ActiveMQ broker. This is distinct from the open issue 20261110T231924Z-mq-the-broker-endpoints-describebroker-now-advertises-have.md, which is about nothing LISTENING on the advertised address; that issue explicitly treats the host component as a per-run environment value, so the loss of broker identity in the published address is not covered by it, and this divergence's replay key is carried by no open issue.

Fix: the mq DescribeBroker (and CreateBroker/ListBrokers, which build the same BrokerInstance) response construction -- the brokerInstances builder that assembles endpoints[] and ipAddress. It must compose each endpoint's authority from the stored broker's own id and region and keep the per-engine scheme/port table it already has, and populate ipAddress from the broker instance's interface address (omitting it for RABBITMQ brokers, per the model's documentation), rather than substituting a single fixed loopback literal for every broker. The address it publishes must remain one the emulator is actually reachable on.

Traces: mq-activemq-broker-endpoint-and-user-discovery#task#7.

## Evidence
Wire corpus simparity-wire-aws-v1 replayed against the emulator booted from this checkout: 68 steps, 52 passed, 16 mismatched, score 76.5%, 12 divergence clusters -- all service mq, 11 of them already owned by open issues. The one unowned cluster (divergence_key service=mq|DescribeBroker|status=200->200|keys=$.brokerInstances[0].endpoints[0..3],$.brokerInstances[0].ipAddress; stub_path empty, owned_by empty):

fixture mq-activemq-broker-endpoint-and-user-discovery, step task#7, DescribeBroker, expected_status 200, actual_status 200, verdict mismatch:
  $.brokerInstances[0].ipAddress: value_mismatch (expected '<shaped-not-asserted>', got '127.0.0.1')
  $.brokerInstances[0].endpoints[0]: resource_not_identified (expected 'ssl://b-d2783693-0791-2384-7a71-ae0ae15403f9-1.mq.us-west-2.amazonaws.com:61617', got 'ssl://127.0.0.1:61617')
  $.brokerInstances[0].endpoints[1]: resource_not_identified (expected 'amqp+ssl://b-d2783693-0791-2384-7a71-ae0ae15403f9-1.mq.us-west-2.amazonaws.com:5671', got 'amqp+ssl://127.0.0.1:5671')
  $.brokerInstances[0].endpoints[2]: resource_not_identified (expected 'stomp+ssl://b-d2783693-0791-2384-7a71-ae0ae15403f9-1.mq.us-west-2.amazonaws.com:61614', got 'stomp+ssl://127.0.0.1:61614')
  $.brokerInstances[0].endpoints[3]: resource_not_identified (expected 'mqtt+ssl://b-d2783693-0791-2384-7a71-ae0ae15403f9-1.mq.us-west-2.amazonaws.com:8883', got 'mqtt+ssl://127.0.0.1:8883')

The recorded broker id 'b-d2783693-...' and the region 'us-west-2' in the expected hosts are that recording's values; what the comparison flags is that the emulator's authority contains NO broker-derived component at all, identically for every broker. The schemes and ports match, so only the authority and ipAddress diverge. Model citation: BrokerInstance.Endpoints = "The broker's wire-level protocol endpoints"; BrokerInstance.IpAddress = "The IP address of the Elastic Network Interface (ENI) attached to the broker. Does not apply to RabbitMQ brokers."

## Steps to Reproduce
1. Clone the repo, build and boot the emulator from the checkout.
2. Replay the wire corpus simparity-wire-aws-v1, fixture mq-activemq-broker-endpoint-and-user-discovery, through step task#7.
3. The fixture creates an ActiveMQ broker (SINGLE_INSTANCE, mq.t3.micro, one user), polls GET /v1/brokers/{BrokerId} until brokerState is RUNNING, and at task#7 reads brokerInstances[0].
4. Inspect brokerInstances[0].endpoints: expect four URIs (ssl 61617, amqp+ssl 5671, stomp+ssl 61614, mqtt+ssl 8883) whose authority is a host derived from this broker's id and region, so the broker id is recoverable from the endpoint and no two brokers share an authority. Observed: all four use one fixed loopback authority (127.0.0.1) with no broker-derived component.
5. Inspect brokerInstances[0].ipAddress: expect the broker interface's address. Observed: '127.0.0.1'.
6. Create a second ActiveMQ broker in the same account and repeat step 4 on it: the emulator publishes byte-identical endpoints for both brokers, so endpoint-keyed client state cannot distinguish them.
