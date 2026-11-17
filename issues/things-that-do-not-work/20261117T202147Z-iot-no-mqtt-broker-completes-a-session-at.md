Issue-Severity: HIGH
Issue-Commit: eb25315957aceaabd7d1a9e502f713207998e734

# iot: no MQTT broker completes a session at the published iot:Data-ATS endpoint, so the MQTT5 property-propagation and shadow-delta push checks hang to the harness timeout

> _Note: this issue was identified against the code base at commit `eb25315957aceaabd7d1a9e502f713207998e734`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEndpoint

## Description
Cause 1 of 1: the address `DescribeEndpoint(endpointType="iot:Data-ATS")` publishes does not carry a usable MQTT broker on 8883 for this emulator, so every step that needs a live MQTT session never completes and the checks below burn their whole budget before reaching any assertion.

AWS: Smithy models `DescribeEndpoint` (iot) returning `endpointAddress`, the account's MQTT broker host; the IoT Core data plane serves mutual-TLS MQTT 3.1.1 and MQTT 5 on port 8883 at that host. A device presenting a certificate created by `CreateKeysAndCertificate` and carrying an attached policy with iot:Connect/Subscribe/Receive/Publish gets CONNACK reason 0, SUBACK granted QoS 1 for its topic filters, and then: (a) for an iot-data `Publish` carrying the modeled MQTT5 parameters (`userProperties`, `payloadFormatIndicator`, `contentType`, `responseTopic`, `correlationData` base64-decoded to raw bytes on the wire, `messageExpiry` forwarded as MessageExpiryInterval decremented by the queue wait and never larger than published), an MQTT 5 subscriber receives each property, and a control publish with no properties arrives with none; (b) for the classic shadow, a desired write pushes `$aws/things/<thing>/shadow/update/delta` with every differing leaf, a converging `reported` write pushes `/update/accepted` and `/update/documents` but NO delta, and a later delta holds only what still differs. This is recorded behavior of the broker; the Smithy model describes the control-plane discovery call and the data-plane Publish parameters, not the MQTT transport itself.

Emulator: both checks are killed by the 60 s harness timeout with no assertion reached (`Error: Check timed out after 60.0s`). Each check first resolves the host via `DescribeEndpoint(iot:Data-ATS)` and then retries `client.connect(host, 8883)` in a bounded loop (120 s for MQTT 5, equivalent for 3.1.1) waiting for a CONNACK with reason 0; neither loop ever exits, so the harness deadline fires first. Nothing downstream of the session — property propagation, SUBACK grants, delta recomputation — is exercised at all.

Why: the entire IoT value proposition for a client is the MQTT session: devices connect, subscribe and receive. A client whose code works against AWS gets a session that never establishes here, so no device-side code path can be developed or tested, and both the MQTT5 request-response metadata contract and the shadow delta/documents push contract are unobservable. A hang is worse than a refusal: the SDK/driver has no error to branch on and the caller stalls.

Fix: the IoT data plane's MQTT listener and the endpoint `DescribeEndpoint` announces — the host must be one the emulator actually accepts mutual-TLS MQTT 3.1.1 and MQTT 5 connections on, the CONNACK must honour the certificate's attached policy, and the broker must then deliver MQTT5 properties on forwarded publishes and push the shadow `/update/delta`, `/update/accepted` and `/update/documents` topics.

Traces: iot.publish-mqtt5-properties-reach-a-v5-subscriber, iot.shadow-delta-is-pushed-over-mqtt-and-recomputed-against-reported.

These two checks are NOT carried by any open issue's replay key. Sibling issues describe the same family for other callers and other legs — 20261110T222008Z-iot-data-no-mqtt-broker-answers-at-the.md (iot-data GetConnection/ListSubscriptions/SendDirectMessage), 20261109T120740Z-iot-data-the-mqtt-broker-address-published-by.md, 20261110T222008Z-iot-the-published-data-plane-address-serves-no.md (mTLS HTTPS POST /topics/{topic}) and 20261109T114414Z-iot-describeendpoint-announces-a-real-amazonaws-com-data.md — so a fix there is likely to settle these two as well, but the MQTT5 property-forwarding and shadow-delta push behaviours above must be confirmed by re-replaying the two check ids listed in trace_ids before this is closed.

## Evidence
Check iot.publish-mqtt5-properties-reach-a-v5-subscriber — Error: Check timed out after 60.0s.
The check does: describe_endpoint(endpointType="iot:Data-ATS") -> host; socket.getaddrinfo(host, 8883) until it resolves (180 s deadline); create_policy + create_keys_and_certificate(setAsActive=True) + attach_policy; then paho MQTTv5 client.connect(host, 8883, clean_start=True) retried until a CONNACK with reason 0 (120 s deadline), asserting `state["reason"] == 0` and SUBACK `granted == [1]` before grading MessageExpiryInterval in (0, 300], the two user properties, payloadFormatIndicator, contentType, responseTopic and CorrelationData (base64-decoded raw bytes). None of those assertions produced output: the harness killed the run inside the pre-assertion session setup.

Check iot.shadow-delta-is-pushed-over-mqtt-and-recomputed-against-reported — Error: Check timed out after 60.0s.
Same shape: describe_endpoint(iot:Data-ATS), create_thing, provision certificate+policy, MQTT 3.1.1 connect asserting `state["rc"] == 0`, subscribe to $aws/things/<thing>/shadow/update/{delta,accepted,documents} asserting granted == [1], then grade that a converging `reported` write pushes accepted+documents and no delta. Again no assertion was reached.

Expected: CONNACK 0 and SUBACK [1] within seconds, then the property/delta pushes above. Actual: no session at all within the 60 s budget — the connect retry loop never succeeds.

## Steps to Reproduce
1. iot.describe_endpoint(endpointType="iot:Data-ATS") and note endpointAddress.
2. iot.create_policy with Allow iot:Connect, iot:Subscribe, iot:Receive, iot:Publish on "*"; iot.create_keys_and_certificate(setAsActive=True); iot.attach_policy(target=<certArn>); write certificatePem and keyPair.PrivateKey to disk.
3. MQTT 5: paho client (CallbackAPIVersion.VERSION2, protocol=MQTTv5), tls_set(certfile, keyfile), connect(<endpointAddress>, 8883, keepalive=30, clean_start=True). AWS: on_connect fires with reason code 0 within seconds. Emulator: no CONNACK; the connect retry loop runs until the harness kills the check at 60 s.
4. Had the session established: subscribe("<base>/telemetry", qos=1) must grant [1]; iot-data publish(topic="<base>/telemetry", qos=1, userProperties=[{"a":"alpha"},{"b":"beta"}], payloadFormatIndicator="UTF8_DATA", contentType=..., responseTopic="<base>/reply", correlationData=<base64>, messageExpiry=300) must arrive at the v5 subscriber with every property set (CorrelationData as the base64-DECODED bytes, MessageExpiryInterval in (0, 300]); a publish with no properties must arrive with none.
5. MQTT 3.1.1: connect the same way, subscribe to $aws/things/<thing>/shadow/update/delta, /update/accepted and /update/documents, then iot-data update_thing_shadow with a desired state. AWS pushes a delta holding every differing leaf, a converging reported write pushes accepted+documents with NO delta, and a later delta holds only what still differs. Emulator: nothing, the session never exists.
