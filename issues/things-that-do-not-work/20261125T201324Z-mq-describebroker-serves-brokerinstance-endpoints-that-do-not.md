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
Issue-Commit: a805f6ed4da63b46c278d03c62d2245c90c81566

# mq DescribeBroker serves BrokerInstance endpoints that do not identify the broker

> _Note: this issue was identified against the code base at commit `a805f6ed4da63b46c278d03c62d2245c90c81566`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeBroker

## Description
Single cause (1 of 1): DescribeBroker returns a 200 whose brokerInstances[0] is structurally correct but whose endpoints — and ipAddress — are not derived from the broker resource at all, so the response carries no per-broker connection identity.

AWS: Smithy (services/service-com-amazonaws-mq-mq/smithy-model/com-amazonaws-mq-mq.json) shape com.amazonaws.mq#BrokerInstance: member Endpoints (jsonName "endpoints", target __listOf__string) is documented "The broker's wire-level protocol endpoints"; member IpAddress (jsonName "ipAddress") is "The IP address of the Elastic Network Interface (ENI) attached to the broker. Does not apply to RabbitMQ brokers." The model does not spell out the endpoint string grammar, so for the grammar this is recorded behavior only: for an ActiveMQ single-instance broker, each entry is '<scheme>://<broker-endpoint-host>:<protocol-port>', where the host is the broker's OWN generated endpoint host (the AWS form is b-<broker-id-fragment>-<instance-ordinal>.mq.<region>.amazonaws.com — derived from the broker id assigned by CreateBroker, the instance ordinal, the service endpoint prefix and the broker's region) and the port is the fixed per-protocol Amazon MQ port for that scheme: ssl -> 61617, amqp+ssl -> 5671, stomp+ssl -> 61614, mqtt+ssl -> 8883. The recorded broker id, hostname and region in the fixture are per-run values that fill that template; only the template, the one-host-per-broker-instance rule and the fixed per-protocol ports are the requirement. ipAddress is recorded as shape-asserted only: a routable ENI address belonging to the broker, not a fixed literal.

Emulator: every endpoint is emitted against a single shared loopback host rather than a host derived from the broker, so all four entries name the same host for every broker the emulator holds; the replay reports each of $.brokerInstances[0].endpoints[0..3] as resource_not_identified (expected the broker's own ssl/amqp+ssl/stomp+ssl/mqtt+ssl endpoints, got 'ssl://127.0.0.1:61617', 'amqp+ssl://127.0.0.1:5671', 'stomp+ssl://127.0.0.1:61614' served as 'stomp+ssl://127.0.0.1:46567', 'mqtt+ssl://127.0.0.1:8883'). Two further faults are visible in the same value: the stomp+ssl entry does not carry the fixed Amazon MQ STOMP port (an ephemeral/mapped port is substituted, while the other three schemes do carry their fixed ports), and $.brokerInstances[0].ipAddress is reported value_mismatch against the shape assertion because a loopback literal is served where a broker-owned address is modelled. Status and the rest of the DescribeBroker body match (200 vs 200).

Why: a client cannot tell two brokers apart from their endpoints, and cannot route to a specific broker from a DescribeBroker response. Anything that discovers a connection target by reading brokerInstances[].endpoints — a JMS/AMQP/STOMP/MQTT client wired from the API, a connection-factory builder, or an assertion that the endpoint belongs to the broker just created — binds to a host shared by every broker, and per-scheme port lookups break for STOMP because the published protocol port is not the one served. Code that parses the endpoint to recover the broker id or region (a common pattern for logging and for cross-referencing an ARN) finds neither.

Fix: the mq broker state's endpoint synthesis — the component that materialises BrokerInstance for a stored broker and is read back by the DescribeBroker handler (and populated when CreateBroker admits a broker). It must build one endpoint host per broker instance from the broker's own id/ordinal/region rather than from the listener address, pair each scheme with its fixed Amazon MQ protocol port instead of the locally bound port, and populate ipAddress with a broker-owned address for ActiveMQ brokers (leaving it unset for RabbitMQ, per the member's documentation).

Traces: fixture mq-activemq-broker-endpoint-and-user-discovery, step task#7 (DescribeBroker, 200 vs 200, five diverging keys: $.brokerInstances[0].ipAddress and $.brokerInstances[0].endpoints[0..3]). No open issue covers this: 20261118T001804Z-mq-describebrokerinstanceoptions-omits-the-published-rabbitmq-host-instance.md is about the DescribeBrokerInstanceOptions catalog, 20261103T045546Z-mq-refuses-invalid-requests-with-the-wrong-status.md about refusal status/message fidelity, and 20261112T040505Z-mq-tag-mutations-omit-the-content-length-header.md about tag 204 headers.

## Evidence
Replay of fixture mq-activemq-broker-endpoint-and-user-discovery step task#7 (DescribeBroker), expected status 200, actual status 200, verdict mismatch:

$.brokerInstances[0].ipAddress: value_mismatch (expected '<shaped-not-asserted>' — a broker-owned ENI address — got '127.0.0.1')
$.brokerInstances[0].endpoints[0]: resource_not_identified (expected 'ssl://<broker-endpoint-host>:61617', recorded as 'ssl://b-aefab202-6add-2a12-7445-a9950213daa1-1.mq.us-west-2.amazonaws.com:61617'; got 'ssl://127.0.0.1:61617')
$.brokerInstances[0].endpoints[1]: resource_not_identified (expected 'amqp+ssl://<broker-endpoint-host>:5671'; got 'amqp+ssl://127.0.0.1:5671')
$.brokerInstances[0].endpoints[2]: resource_not_identified (expected 'stomp+ssl://<broker-endpoint-host>:61614'; got 'stomp+ssl://127.0.0.1:46567' — the fixed STOMP port 61614 is replaced by a mapped port)
$.brokerInstances[0].endpoints[3]: resource_not_identified (expected 'mqtt+ssl://<broker-endpoint-host>:8883'; got 'mqtt+ssl://127.0.0.1:8883')

The recorded host 'b-aefab202-...-1.mq.us-west-2.amazonaws.com' and its broker id are per-run values illustrating the template b-<broker-id-fragment>-<instance-ordinal>.mq.<region>.amazonaws.com; the requirement is that the host is derived from the broker's own identity (one distinct host per broker instance) and that each scheme carries its fixed Amazon MQ protocol port.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Fixture mq-activemq-broker-endpoint-and-user-discovery creates an ActiveMQ single-instance broker and, at step task#7, calls DescribeBroker on it.
3. Inspect brokerInstances[0] in the 200 response: endpoints[0..3] all name the emulator's own listener host instead of a host derived from the broker, endpoints[2] (stomp+ssl) does not carry the fixed Amazon MQ STOMP port 61614, and ipAddress is a loopback literal rather than a broker-owned address.
4. Create a second broker and DescribeBroker it: its endpoints are indistinguishable from the first broker's, confirming the endpoint host is not derived from broker identity.
