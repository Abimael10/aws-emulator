Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kafka__lambda.msk-lambda-esm-filter-criteria-delivers-only-matching-records-and-a-cleared-filter-releases-the-rest kafka__lambda.msk-lambda-event-source-mapping-disabled-delivers-nothing-then-reenabled-delivers-the-missed-records
Issue-Key: kafka__lambda.msk-lambda-esm-filter-criteria-delivers-only-matching-records-and-a-cleared-filter-releases-the-rest|_create_topic#5|AssertionError|raise _dp_fail(
Issue-Key: kafka__lambda.msk-lambda-event-source-mapping-disabled-delivers-nothing-then-reenabled-delivers-the-missed-records|_create_topic#3|AssertionError|raise _dp_fail(
Issue-Commit: e13abbd45115774a2130a0adf52b114a76179055

# MSK Lambda event-source-mapping checks cannot set up: the broker's ApiVersions omits CreateTopics (api key 19), so no topic can be created for the ESM to poll (1 cause)

> _Note: this issue was identified against the code base at commit `e13abbd45115774a2130a0adf52b114a76179055`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTopics

## Description
Cause 1 of 1: the emulated MSK broker does not serve the Kafka topic-admin protocol, so the Lambda event-source-mapping checks of this service pair never get a topic to wire an ESM to.
AWS: recorded behavior only (the Kafka wire protocol is not in the kafka Smithy model). A real MSK broker on the cluster's reported KafkaVersion advertises CreateTopics (api key 19) in its ApiVersions response, and a kafka-python KafkaAdminClient authenticating with SASL/AWS_MSK_IAM on the serverless 9098 listener creates the topic; CreateEventSourceMapping then polls that topic.
Emulator: the AdminClient refuses before it can send, because ApiVersions does not list api key 19: `IncompatibleBrokerVersion: [Error 35] IncompatibleBrokerVersion: Kafka broker does not support the 'CreateTopicsRequest' Kafka protocol.` The handshake, the connection and ApiVersions all succeed, so the listener is up; only the topic-admin half of the protocol is absent.
Why: a customer wiring Lambda to MSK creates the topic with a Kafka client and then creates the ESM. With no CreateTopics the first step fails with a message blaming the broker's VERSION, and nothing about ESM filter criteria, offset commits or enable/disable behavior can be observed — a Kafka-triggered Lambda cannot be set up at all.
Fix: the emulated Kafka wire-protocol broker — its ApiVersions table must advertise only the keys it serves, and CreateTopics (19) must be handled on the SASL/IAM listener and return a CreateTopics response (with a Kafka error code when it refuses). This is the same emulator component as the `kafka`-service issue filed in this sweep for causes 2; it is filed separately because these checks are reported under the `kafka__lambda` heading. Adjacent but not covering: 20261104T044826Z-kafka-lambda-msk-event-source-mappings-never-deliver-filter-criteria.md (delivery once an ESM exists) and 20261109T210043Z-kafka-lambda-msk-bootstrap-brokers-are-published-on-a-kafka.md (the published host name).
Traces: kafka__lambda.msk-lambda-esm-filter-criteria-delivers-only-matching-records-and-a-cleared-filter-releases-the-rest, kafka__lambda.msk-lambda-event-source-mapping-disabled-delivers-nothing-then-reenabled-delivers-the-missed-records.

## Evidence
kafka__lambda.msk-lambda-esm-filter-criteria-delivers-only-matching-records-and-a-cleared-filter-releases-the-rest: AssertionError: DATAPLANE_UNIMPLEMENTED: the AdminClient could not create '<topic>' over SASL/IAM on '<host>:<port>' (stage 'create_topic'): "IncompatibleBrokerVersion: [Error 35] IncompatibleBrokerVersion: Kafka broker does not support the 'CreateTopicsRequest' Kafka protocol."; connect attempts [].
kafka__lambda.msk-lambda-event-source-mapping-disabled-delivers-nothing-then-reenabled-delivers-the-missed-records: same AssertionError at _create_topic.
(Topic names, hosts and ports are per-run.) Expected: ApiVersions advertises api key 19 and create_topics returns a CreateTopics response that creates the topic. Actual: key 19 is absent from the ApiVersions response, so the client raises IncompatibleBrokerVersion [Error 35] and no topic exists for the ESM.

## Steps to Reproduce
Create an MSK serverless cluster with SASL/IAM, read BootstrapBrokerStringSaslIam from GetBootstrapBrokers, and from a client inside the cluster's VPC build a kafka-python KafkaAdminClient with security_protocol=SASL_SSL / sasl_mechanism=AWS_MSK_IAM against that endpoint on port 9098 and call create_topics(NewTopic(name, num_partitions=3)). The connection and ApiVersions succeed, the ApiVersions response contains no api key 19, and create_topics raises IncompatibleBrokerVersion [Error 35]; the subsequent CreateEventSourceMapping has no topic to poll.
