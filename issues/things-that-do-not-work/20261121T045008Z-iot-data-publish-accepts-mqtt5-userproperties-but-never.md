Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.publish-mqtt5-properties-reach-a-v5-subscriber
Issue-Key: iot.publish-mqtt5-properties-reach-a-v5-subscriber|run#30|AssertionError|assert got_user_props == expected_user_props, (
Issue-Commit: 9a3ba8c99fe3276f826d008a22274dd6f71a6b78

# iot-data Publish accepts MQTT5 userProperties but never delivers them to an MQTT 5 subscriber

> _Note: this issue was identified against the code base at commit `9a3ba8c99fe3276f826d008a22274dd6f71a6b78`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Publish

## Description
One net-new root cause in the IoT data plane's MQTT 5 property forwarding.

Cause 1 of 1: The HTTP Publish API accepts the MQTT 5 User Properties parameter and then drops it, so the message is delivered to an MQTT 5 subscriber with no User Property at all. Failing check: iot.publish-mqtt5-properties-reach-a-v5-subscriber.

AWS:      Smithy (iot-data, IotMoonrakerService): Publish POST /topics/{topic}; input member `userProperties` -> SynthesizedJsonUserProperties, bound as httpHeader `x-amz-mqtt5-user-properties`, mediaType application/json. Its documentation: "A JSON string that contains an array of JSON objects ... The following example userProperties parameter is a JSON string which represents two User Properties: [{\"deviceName\": \"alpha\"}, {\"deviceCnt\": \"45\"}]". The operation has no modelled error for this input, and the recording shows Publish returning 200, so the parameter is accepted and the properties are expected to travel with the message: each key/value object in the array becomes one MQTT 5 User Property (MQTT 5.0 3.3.2.3.7) on the PUBLISH the broker delivers to a subscriber that negotiated protocol version 5, in the order published. Hypothesis (neither the model nor the recording states the ordering rule explicitly beyond what the recording shows): the broker preserves the published order.

Emulator: Publish is accepted (HTTP 200, no error) and the message is delivered — the payload and topic are correct, and the sibling MQTT 5 properties set on the same call do arrive (ContentType = the published content type, ResponseTopic = the published response topic, PayloadFormatIndicator = 1 for payloadFormatIndicator=UTF8_DATA). Only the User Properties are missing: the MQTT 5 subscriber receives a PUBLISH whose UserProperty property is absent, i.e. an empty list, where two properties were published. Replay output: "the delivered MQTT5 User Properties were [], published [('<key-1>', '<value-1>'), ('<key-2>', '<value-2>')] — an implementation that accepts the x-amz-mqtt5-user-properties header and drops it delivers an empty list" (the two key/value pairs are the ones taken from the request's userProperties array; they are not fixed text). The control leg of the check confirms the absence is not a constant: a publish that sets no properties correctly delivers none.

Why:      A client that uses User Properties for out-of-band metadata — routing hints, trace/correlation ids, schema or device version tags, tenant ids — sees them silently vanish. Because the API returns 200 and the payload still arrives, there is no signal of the loss: the publisher believes the metadata was sent and the subscriber-side logic that reads it takes the default/empty branch, so messages are mis-routed or mis-interpreted rather than failing loudly. Anything that round-trips metadata through User Properties (request/response correlation together with responseTopic, for example) cannot work at all against the emulator.

Fix:      The iot-data Publish handler and the broker's MQTT 5 egress serialization: the handler must deserialize the `x-amz-mqtt5-user-properties` header (JSON array of single-entry objects, base64-decoded when sent raw over HTTP per the member documentation) into the message's properties in state, and the broker must emit one MQTT 5 User Property per entry, in published order, on the PUBLISH sent to subscribers that negotiated MQTT 5 — alongside the ContentType/ResponseTopic/PayloadFormatIndicator properties it already forwards correctly. The other properties working while this one does not points at the User Properties entry being parsed-and-discarded rather than at a missing MQTT 5 property path.

Traces:   iot.publish-mqtt5-properties-reach-a-v5-subscriber (assertion at check line 129; the control leg and the ContentType/ResponseTopic/PayloadFormatIndicator assertions before it pass). The check's later assertions on MessageExpiryInterval and CorrelationData were never reached, so this issue does not claim anything about them; re-replaying this check after the fix will grade them.

## Evidence
Failing check iot.publish-mqtt5-properties-reach-a-v5-subscriber [mismatch]:

  AssertionError: the delivered MQTT5 User Properties were [], published
  [('sim2real-a', 'alpha'), ('sim2real-b', 'beta')] — an implementation that accepts the
  x-amz-mqtt5-user-properties header and drops it delivers an empty list

(The two pairs are the values from this run's request; the requirement is that whatever array the
request's userProperties carries is delivered, in order.)

Expected: an MQTT 5 subscriber of the topic receives the PUBLISH with one User Property per entry of
the published userProperties array, keys and values unchanged and in published order.
Actual:   the delivered PUBLISH carries no UserProperty property (empty list), while ContentType,
ResponseTopic and PayloadFormatIndicator from the same Publish call are delivered correctly, and the
HTTP Publish call itself returns 200 with no error.

## Steps to Reproduce
1. Create an IoT policy granting iot:Connect, iot:Subscribe, iot:Receive, iot:Publish and attach it to a certificate/thing identity (as the check's provisioning helper does).
2. DescribeEndpoint(endpointType="iot:Data-ATS") and connect an MQTT 5 client (paho, protocol version 5) to that host on 8883 with the mTLS identity; subscribe to "<base>/telemetry" at QoS 1 and assert SUBACK granted [1].
3. Control leg: iot-data Publish(topic="<base>/telemetry", qos=1, payload=<bytes>) with no MQTT5 properties. Observe the delivered message carries none of ContentType, ResponseTopic, CorrelationData, UserProperty, PayloadFormatIndicator.
4. Subject leg: iot-data Publish(topic="<base>/telemetry", qos=1, payload=<bytes>, contentType="application/json", responseTopic="<base>/reply", correlationData=<base64 of raw bytes>, payloadFormatIndicator="UTF8_DATA", userProperties=[{"<key-1>": "<value-1>"}, {"<key-2>": "<value-2>"}], messageExpiry=300).
5. Read the delivered MQTT 5 message's properties: ContentType, ResponseTopic and PayloadFormatIndicator match what was published, but UserProperty is absent/empty instead of the two published key/value pairs.
