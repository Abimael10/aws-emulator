Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot__sqs.iot-basic-ingest-triggers-the-rule-without-delivering-to-ordinary-subscribers iot__sqs.iot-topic-rule-sql-transforms-the-payload-and-routes-it-to-sqs
Issue-Key: iot__sqs.iot-basic-ingest-triggers-the-rule-without-delivering-to-ordinary-subscribers|_drain#30|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=20)
Issue-Key: iot__sqs.iot-topic-rule-sql-transforms-the-payload-and-routes-it-to-sqs|_drain#22|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=20)
Issue-Commit: afcdf7be57b3fef95e777182b2b5896511e2ffa2

# iot__sqs: the published iot:Data-ATS endpoint carries no usable MQTT session, so both native-MQTT topic-rule checks (Basic Ingest bypass, rule-SQL transform to SQS) reach no verdict and are killed at the harness cap

> _Note: this issue was identified against the code base at commit `afcdf7be57b3fef95e777182b2b5896511e2ffa2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEndpoint

## Description
Cause 1 of 1: the IoT data plane advertised by the account's own iot:Data-ATS endpoint never carries a usable MQTT session, so no native MQTT publish ever happens and every behaviour downstream of it — the topic rules engine's `sqs` action and the Basic Ingest bypass — is unreachable. Both grounding checks (iot__sqs.iot-basic-ingest-triggers-the-rule-without-delivering-to-ordinary-subscribers, iot__sqs.iot-topic-rule-sql-transforms-the-payload-and-routes-it-to-sqs) make no progress past the MQTT client setup and are cut off at the harness's 60 s cap without evaluating a single assertion.

AWS: Smithy (com.amazonaws.iot) models DescribeEndpoint as GET /endpoint, 200, input member endpointType (EndpointType, httpQuery "endpointType", @length 0..128, @pattern ^[\s\S]*$), output member endpointAddress (EndpointAddress); errors InternalFailureException(500), InvalidRequestException(400), ThrottlingException(400), UnauthorizedException(401). Smithy models CreateTopicRule as POST /rules/{ruleName}, 200, with the required httpPayload TopicRulePayload (whose `actions` include the sqs action's queueUrl/roleArn/useBase64 members); errors ConflictingResourceUpdateException(409), InvalidRequestException(400), ResourceAlreadyExistsException(409), SqlParseException(400), UnauthorizedException(401). The model says nothing about the wire session behind endpointAddress; for that the requirement is recorded behavior plus the MQTT 3.1.1/5 protocol: a mutually-authenticated TLS client (the thing's certificate, with an attached policy allowing iot:Connect/iot:Publish/iot:Subscribe/iot:Receive) connects to endpointAddress on port 8883, the broker answers CONNECT with a CONNACK whose return code is 0, SUBSCRIBE is answered with a SUBACK granting the requested QoS, and PUBLISH is acknowledged. A publish on an ordinary topic `<topic>` is BOTH delivered to that topic's subscribers AND matched by a topic rule whose SQL is `SELECT * FROM '<topic>'`, whose sqs action writes the payload (base64-encoded only when useBase64 is true) to the configured queue under the given roleArn. A publish on the Basic Ingest topic `$aws/rules/<ruleName>/<topic>` bypasses the broker: it fires that rule into SQS and is NEVER delivered to a `<topic>` subscriber (AWS documents this as "Reducing messaging costs with Basic Ingest"). A rule whose SQL selects/filters fields routes only the matching, transformed payloads to SQS.

Emulator: both checks successfully obtain an endpointAddress from DescribeEndpoint, create the SQS queue and the rules-engine role, create the topic rule, and provision thing/policy/certificate — then make no further progress. The replay reports, for each, only `Error: Check timed out after 60.0s`; no CONNACK, no SUBACK, no publish acknowledgement, no queue body and no subscriber delivery is ever observed, and no assertion is reached. The same listener defect is already recorded for OTHER check groups as "TLS accepted but CONNACK never sent".

Why: the IoT rules engine and Basic Ingest are reachable only over MQTT. A device client that follows the documented discovery path (DescribeEndpoint(iot:Data-ATS), then mTLS MQTT on 8883) blocks indefinitely instead of connecting, so no device can publish telemetry, no topic rule can ever fire, and the two properties these checks pin — that Basic Ingest fires the rule WITHOUT delivering to ordinary subscribers, and that rule SQL transforms/filters what it routes to SQS — cannot be exercised or relied on. The failure mode is silent (a hang, not a refusal), which is worse for a client than an error: there is no status, error code or message to act on.

Fix: the iot data-plane listener standing behind the address DescribeEndpoint(endpointType="iot:Data-ATS") returns. It must complete the MQTT CONNECT handshake (CONNACK rc=0) for a certificate-authenticated client whose attached policy allows iot:Connect, honour SUBSCRIBE (SUBACK with the granted QoS) and PUBLISH, feed published messages into the topic-rules engine — including the `$aws/rules/<ruleName>/<topic>` Basic Ingest form, which must skip subscriber delivery — and execute the matched rule's sqs action under its roleArn. DescribeEndpoint must publish an address the emulator actually serves on that port.

Related already-open issues: 20261119T002136Z-iot-sqs-no-mqtt-session-can-be-established.md describes this same listener cause for these same two checks but the suite did not mark either check OWNED by it (its recorded Issue-Keys bind the `_mqtt_connect` CONNECT-wait signature, whereas this run's failures carry only the bare 60 s harness-cap signature), so the pair is re-filed here with the replay keys as the suite now reports them. 20261117T200355Z-iot-the-published-iot-data-ats-mqtt-listener.md, 20261117T202147Z-iot-no-mqtt-broker-completes-a-session-at.md, 20261118T161720Z-iot-the-mqtt-broker-at-the-published-iot.md and 20261118T164940Z-iot-retained-message-replay-is-unobservable-the-published.md describe the same listener defect for other check groups and do not carry these two checks.

Traces: iot__sqs.iot-basic-ingest-triggers-the-rule-without-delivering-to-ordinary-subscribers; iot__sqs.iot-topic-rule-sql-transforms-the-payload-and-routes-it-to-sqs.

## Evidence
From the behavioral replay (### iot__sqs):
  [mismatch] iot__sqs.iot-basic-ingest-triggers-the-rule-without-delivering-to-ordinary-subscribers — Error: Check timed out after 60.0s
  [mismatch] iot__sqs.iot-topic-rule-sql-transforms-the-payload-and-routes-it-to-sqs — Error: Check timed out after 60.0s

Both checks are tagged dp:native / plane:data and reach the data plane via `host = ctx.client("iot").describe_endpoint(endpointType="iot:Data-ATS")["endpointAddress"]`, wait for that host to resolve, then open an mTLS MQTT client to (host, 8883) with a certificate whose attached policy allows iot:Connect/iot:Publish/iot:Subscribe/iot:Receive.

Expected (AWS): CONNECT is answered with CONNACK rc=0; SUBSCRIBE on `<topic>` is answered with a SUBACK granting QoS 1; the publisher's PUBLISHes are acknowledged. For the Basic Ingest check, one publish on `$aws/rules/<ruleName>/<topic>` lands exactly once in the rule's SQS queue and NOTHING bearing that payload reaches the `<topic>` subscriber, while each of the ordinary publishes on `<topic>` reaches both the subscriber and the queue (queue holds one body per publish on either path). For the SQL check, only the payloads matching the rule's SELECT/WHERE arrive in SQS, shaped as the SQL states.

Actual (emulator): no MQTT session is ever established — neither check evaluates any assertion; each is killed at the 60 s cap with `Check timed out after 60.0s`. No CONNACK, no SUBACK, no queue body and no subscriber delivery are observed. There is no HTTP status, error code or error message at all, because the hang is on the MQTT/TLS data-plane socket, not on a modeled API call (the control-plane calls DescribeEndpoint, CreateTopicRule, CreateThing, CreatePolicy, CreateKeysAndCertificate all succeed first).

## Steps to Reproduce
1. Replay the behavioral suite and re-run iot__sqs.iot-basic-ingest-triggers-the-rule-without-delivering-to-ordinary-subscribers and iot__sqs.iot-topic-rule-sql-transforms-the-payload-and-routes-it-to-sqs.
2. Manually: call iot:DescribeEndpoint(endpointType="iot:Data-ATS") and note endpointAddress.
3. Create an SQS queue and an IAM role trusted by iot.amazonaws.com allowing sqs:SendMessage on that queue's ARN.
4. iot:CreateTopicRule(ruleName=<name>, topicRulePayload={sql: "SELECT * FROM '<topic>'", awsIotSqlVersion: "2016-03-23", ruleDisabled: false, actions: [{sqs: {queueUrl, roleArn, useBase64: false}}]}).
5. iot:CreateThing, iot:CreatePolicy allowing iot:Connect/iot:Publish/iot:Subscribe/iot:Receive on "*", iot:CreateKeysAndCertificate(setAsActive=true), attach the policy to the certificate and the certificate to the thing.
6. Open a TLS 1.2 client-certificate connection to (endpointAddress, 8883) and send an MQTT CONNECT — observe whether a CONNACK is returned at all (it is not; the client blocks).
7. With a working broker, SUBSCRIBE to `<topic>`, PUBLISH on `<topic>` and on `$aws/rules/<ruleName>/<topic>`, then drain the queue and the subscriber inbox and compare against the Basic Ingest bypass rule.
