Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot__sqs.iot-basic-ingest-triggers-the-rule-without-delivering-to-ordinary-subscribers iot__sqs.iot-topic-rule-sql-transforms-the-payload-and-routes-it-to-sqs
Issue-Key: iot__sqs.iot-basic-ingest-triggers-the-rule-without-delivering-to-ordinary-subscribers|_mqtt_connect#16|Timeout|if state["connected"].wait(timeout=20):
Issue-Key: iot__sqs.iot-topic-rule-sql-transforms-the-payload-and-routes-it-to-sqs|_mqtt_connect#18|Timeout|if state["connected"].wait(timeout=20):
Issue-Commit: bf5f1b716fd44070c371033dd9201606434af162

# iot__sqs: no MQTT session can be established at the published iot:Data-ATS endpoint, so both native-MQTT topic-rule checks (basic ingest bypass, SQL payload transform) hang to the harness cap with no verdict

> _Note: this issue was identified against the code base at commit `bf5f1b716fd44070c371033dd9201606434af162`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEndpoint

## Description
Cause 1 of 1: the IoT data plane the account's own iot:Data-ATS endpoint advertises does not carry a usable MQTT session, so no native MQTT publish ever happens and every behaviour downstream of it — the topic rules engine's SQS action, and the Basic Ingest bypass — is unreachable. Both checks are cut off at the harness's 60 s cap without reaching any assertion.

AWS: iot:DescribeEndpoint(endpointType="iot:Data-ATS") returns endpointAddress, the ATS message-broker host for the account; a mutually-authenticated TLS client (the thing's certificate plus an attached policy allowing iot:Connect/Publish/Subscribe/Receive) connects to it on port 8883, completes MQTT CONNECT -> CONNACK, and can then PUBLISH. A publish on an ordinary topic `<topic>` is both delivered to that topic's subscribers AND matched by a topic rule whose SQL is `SELECT * FROM '<topic>'`, whose sqs action writes the (optionally base64-encoded) payload to the configured queue under the given roleArn. A publish on the Basic Ingest topic `$aws/rules/<ruleName>/<topic>` bypasses the broker entirely: it fires that rule into SQS and is NEVER delivered to a `<topic>` subscriber. A rule whose SQL selects and transforms fields (e.g. a WHERE threshold) routes only the matching, transformed payloads to SQS. Smithy (com.amazonaws.iot) models DescribeEndpoint/CreateTopicRule and the sqs action's queueUrl/roleArn/useBase64 members, but says nothing about the wire session itself: the MQTT session behaviour above is recorded behavior plus the MQTT 3.1.1/5 CONNECT-CONNACK requirement, and AWS documents the Basic Ingest bypass ("Reducing messaging costs with Basic Ingest").

Emulator: both checks obtain an endpointAddress, provision a thing, certificate and policy, create the topic rule, and then make no further progress: the run is killed at the 60 s cap with "Check timed out after 60.0s" and no assertion is ever evaluated. The emulator-side symptom already recorded for the same listener on other check groups is that the TLS connection is accepted but CONNACK is never sent, so the client blocks in CONNECT.

Why: the IoT rules engine and Basic Ingest are only reachable over MQTT; a device client that follows the documented discovery path (DescribeEndpoint, then mTLS MQTT on 8883) blocks forever instead of connecting, so no device can publish telemetry, no topic rule can ever fire, and the two properties these checks pin — that Basic Ingest fires the rule WITHOUT delivering to ordinary subscribers, and that rule SQL transforms and filters the payload it routes to SQS — cannot be exercised or relied on at all. The failure is also silent: the client just hangs.

Fix: the iot data-plane listener behind the address DescribeEndpoint(iot:Data-ATS) publishes — it must complete the MQTT CONNECT handshake (CONNACK) for a certificate-authenticated client whose attached policy allows iot:Connect, then honour PUBLISH/SUBSCRIBE, feed published messages to the topic-rules engine (including the `$aws/rules/<ruleName>/<topic>` Basic Ingest form, which must skip subscriber delivery), and execute the rule's sqs action under its roleArn.

Traces: iot__sqs.iot-basic-ingest-triggers-the-rule-without-delivering-to-ordinary-subscribers; iot__sqs.iot-topic-rule-sql-transforms-the-payload-and-routes-it-to-sqs. Open issues describe the same listener defect for OTHER check groups and do not carry these two checks' replay keys, so none of them covers this cause: 20261117T200355Z-iot-the-published-iot-data-ats-mqtt-listener.md (TLS accepted, no CONNACK, 9 native-MQTT checks), 20261117T202147Z-iot-no-mqtt-broker-completes-a-session-at.md, 20261118T161720Z-iot-the-mqtt-broker-at-the-published-iot.md, 20261118T164940Z-iot-retained-message-replay-is-unobservable-the-published.md and 20261109T120740Z-iot-data-the-mqtt-broker-address-published-by.md.

## Evidence
From the behavioral replay:
  [mismatch] iot__sqs.iot-basic-ingest-triggers-the-rule-without-delivering-to-ordinary-subscribers — Error: Check timed out after 60.0s
  [mismatch] iot__sqs.iot-topic-rule-sql-transforms-the-payload-and-routes-it-to-sqs — Error: Check timed out after 60.0s

Both checks are tagged dp:native / plane:data and reach the data plane via `host = ctx.client("iot").describe_endpoint(endpointType="iot:Data-ATS")["endpointAddress"]`, then connect an mTLS MQTT client to (host, 8883) using a certificate whose policy allows iot:Connect/Publish/Subscribe/Receive.
Expected (AWS): the MQTT CONNECT completes (CONNACK), the client publishes, and the checks reach their assertions — for the Basic Ingest check, a publish on `$aws/rules/<rule>/<topic>` lands exactly once in the rule's SQS queue and nothing arrives at the `<topic>` subscriber, while an ordinary publish on `<topic>` does both; for the SQL check, only the matching payloads arrive in SQS, transformed as the rule's SELECT states.
Actual (emulator): no MQTT session is ever established, so neither check evaluates a single assertion — each is killed at the 60 s cap. No queue body and no subscriber delivery are ever observed.

## Steps to Reproduce
Replay the behavioral suite and re-run iot__sqs.iot-basic-ingest-triggers-the-rule-without-delivering-to-ordinary-subscribers and iot__sqs.iot-topic-rule-sql-transforms-the-payload-and-routes-it-to-sqs. Manually: iot:DescribeEndpoint(endpointType="iot:Data-ATS"); create an SQS queue and an IoT rules-engine role allowing sqs:SendMessage on it; iot:CreateTopicRule(sql="SELECT * FROM '<topic>'", awsIotSqlVersion=2016-03-23, ruleDisabled=false, actions=[{sqs:{queueUrl,roleArn,useBase64:false}}]); create a thing, a certificate and a policy allowing iot:Connect/Publish/Subscribe/Receive and attach them; open a TLS 1.2 client-certificate connection to (endpointAddress, 8883) and send an MQTT CONNECT — observe whether a CONNACK is returned; then PUBLISH on `<topic>` and on `$aws/rules/<ruleName>/<topic>` and drain the queue and the `<topic>` subscriber.
