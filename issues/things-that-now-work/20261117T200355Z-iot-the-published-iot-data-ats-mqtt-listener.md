Issue-Severity: CRITICAL
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.e2e-provisioning-claim-to-thing-registration iot.inactive-certificate-is-refused-at-the-mqtt-handshake iot.iot-mqtt-publish-is-delivered-to-a-matching-subscriber-and-not-to-a-non-matching-one iot.iot-mqtt-publish-subscribe iot.mqtt-topic-filter-wildcard-depth-semantics iot.negative-provisioning-template-disabled-blocks-claim iot.presence-lifecycle-events-separate-clean-disconnect-from-clientid-takeover iot.publish-mqtt5-properties-reach-a-v5-subscriber iot.shadow-delta-is-pushed-over-mqtt-and-recomputed-against-reported
Issue-Key: iot.e2e-provisioning-claim-to-thing-registration|_mqtt_connect#33|AssertionError|raise AssertionError(f"MQTT connect to {host}:{port} timed out (rc={rc_holder.get('rc')})")
Issue-Key: iot.inactive-certificate-is-refused-at-the-mqtt-handshake|_connect_once#10|Timeout|state["connected"].wait(timeout=wait_s)
Issue-Key: iot.iot-mqtt-publish-is-delivered-to-a-matching-subscriber-and-not-to-a-non-matching-one|_mqtt_connect#12|Timeout|if state["connected"].wait(timeout=20):
Issue-Key: iot.iot-mqtt-publish-subscribe|_mqtt_awsiotsdk#16|Timeout|time.sleep(3)
Issue-Key: iot.mqtt-topic-filter-wildcard-depth-semantics|_mqtt_connect#12|Timeout|if state["connected"].wait(timeout=20):
Issue-Key: iot.negative-provisioning-template-disabled-blocks-claim|_poll_until#33|AssertionError|raise AssertionError(f"timed out after {timeout_s}s waiting for: {desc}")
Issue-Key: iot.presence-lifecycle-events-separate-clean-disconnect-from-clientid-takeover|_mqtt_connect#9|Timeout|if state["connected"].wait(timeout=20):
Issue-Key: iot.publish-mqtt5-properties-reach-a-v5-subscriber|_mqtt5_connect#10|Timeout|if state["connected"].wait(timeout=20):
Issue-Key: iot.shadow-delta-is-pushed-over-mqtt-and-recomputed-against-reported|_mqtt_connect#16|Timeout|if state["connected"].wait(timeout=20):
Issue-Commit: eb25315957aceaabd7d1a9e502f713207998e734

# iot: the published iot:Data-ATS MQTT listener accepts the TLS connection but never answers CONNACK, so 9 native-MQTT behavior checks never reach a verdict

> _Note: this issue was identified against the code base at commit `eb25315957aceaabd7d1a9e502f713207998e734`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
MQTT CONNECT (iot:Data-ATS data endpoint)

## Description
Cause 1 of 1: the MQTT broker behind the data endpoint DescribeEndpoint(iot:Data-ATS) publishes accepts a client's TCP/TLS connection but never completes the MQTT CONNECT handshake, so no session is ever established and every native-MQTT behavior (publish/subscribe routing, topic-filter wildcard semantics, MQTT5 properties, presence lifecycle events, shadow delta push, fleet provisioning by claim, and the inactive-certificate handshake refusal) is unobservable.

AWS: recorded behavior only — the Smithy model carries no MQTT binding for this transport (iot's modeled surface is the HTTP control plane). The recordings show AWS IoT Core serving MQTT over TLS on port 8883 at the address DescribeEndpoint(endpointType=iot:Data-ATS) returns: after the TLS handshake (client certificate presented), a CONNECT from a client whose certificate is registered, ACTIVE and carries an attached policy allowing iot:Connect is answered with a CONNACK whose return code is 0, and the broker then serves SUBSCRIBE (SUBACK with the granted QoS) and PUBLISH (delivery to matching subscribers only). A certificate that is not ACTIVE is refused at the handshake instead, never with silence.

Emulator: the checks resolve the published address, rewrite it onto the emulator's own loopback host:port (e.g. 127.0.0.1:44445 for 8883) and connect. The connection is accepted — no TLS error, no RST, no HTTP error — but no CONNACK ever arrives: paho's on_connect never fires, so the recorded return code is None ('MQTT connect to 127.0.0.1:44445 timed out (rc=None)', 'timed out after 20.0s waiting for: MQTT CONNACK'), and the remaining seven checks simply hang to the 60 s harness limit ('Check timed out after 60.0s'). No status, error code or message is produced at all.

Why: a client relying on AWS's behavior gets no session and no refusal. Device code blocks on CONNACK until its own timeout rather than connecting or being told why it cannot; every downstream data-plane behavior (message routing by topic filter, retained/MQTT5 property propagation, $aws/things/+/shadow delta pushes, $aws/provisioning-templates/{template}/provision/json fleet provisioning, $aws/events presence lifecycle messages) is unreachable, and negative expectations such as 'an INACTIVE certificate is refused at the handshake' silently look like a hang instead of a rejection.

Fix: the data-plane MQTT listener/broker component that serves the address published for iot:Data-ATS — it must complete the MQTT 3.1.1/5 CONNECT exchange (CONNACK) once the TLS handshake finishes, authenticate the presented client certificate against the registry, then serve SUBSCRIBE/PUBLISH and the $aws/* reserved topics. This is not the iot control-plane operation handlers.

Related already-open issues name the published hostname rather than this silence and do NOT cover these checks: 20261109T120740Z-iot-data-the-mqtt-broker-address-published-by.md, 20261110T222008Z-iot-the-published-data-plane-address-serves-no.md, 20261110T222008Z-iot-data-no-mqtt-broker-answers-at-the.md and 20261115T062827Z-iot-a-claim-certificate-minted-by-createprovisioningclaim-completes.md. The checks filed here already redirect the published hostname to the emulator's own host:port, reach the emulator's listener, and still receive no CONNACK.

Traces: the nine failing checks listed under trace_ids, which are examples of this one cause.

## Evidence
Expected: a CONNACK with return code 0 (session established) for a registered ACTIVE certificate with an iot:Connect policy attached, on TLS port 8883 at the published iot:Data-ATS address; an explicit handshake refusal for a non-ACTIVE certificate.

Actual:
- iot.e2e-provisioning-claim-to-thing-registration: AssertionError: MQTT connect to 127.0.0.1:44445 timed out (rc=None) (check line 217, _mqtt_connect).
- iot.negative-provisioning-template-disabled-blocks-claim: AssertionError: timed out after 20.0s waiting for: MQTT CONNACK (check line 183, _mqtt_provision).
- iot.inactive-certificate-is-refused-at-the-mqtt-handshake, iot.iot-mqtt-publish-subscribe, iot.iot-mqtt-publish-is-delivered-to-a-matching-subscriber-and-not-to-a-non-matching-one, iot.mqtt-topic-filter-wildcard-depth-semantics, iot.presence-lifecycle-events-separate-clean-disconnect-from-clientid-takeover, iot.publish-mqtt5-properties-reach-a-v5-subscriber, iot.shadow-delta-is-pushed-over-mqtt-and-recomputed-against-reported: Check timed out after 60.0s, with no CONNACK observed (the checks assert connect_rcs == [0, 0] and never get there).

The TCP/TLS connect itself succeeds (no connection error is reported); only the MQTT CONNACK is missing.

## Steps to Reproduce
1. Boot the emulator and call iot DescribeEndpoint(endpointType='iot:Data-ATS') to obtain the published data address.
2. Create a certificate (CreateKeysAndCertificate setAsActive=true), attach a policy allowing iot:Connect/iot:Publish/iot:Subscribe/iot:Receive, and attach it to a thing.
3. Point an MQTT 3.1.1 client (paho) at the emulator's host:port for 8883 with that certificate/key (tls_set(certfile=..., keyfile=..., cert_reqs=CERT_NONE)) and call connect(host, 8883, keepalive=30), then loop_start().
4. Observe that the socket connects but on_connect never fires: no CONNACK within 20 s (rc stays None), and the behavioral checks hang to the 60 s limit.
5. Replay the checks in trace_ids to confirm.
