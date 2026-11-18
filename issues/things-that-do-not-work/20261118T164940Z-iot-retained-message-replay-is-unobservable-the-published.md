Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.iot-retained-message-is-pushed-on-subscribe-with-the-retain-flag-set
Issue-Key: iot.iot-retained-message-is-pushed-on-subscribe-with-the-retain-flag-set|_mqtt_connect#10|Timeout|if state["connected"].wait(timeout=20):
Issue-Commit: 53152f24252eb4c6b050f7ba33da4a3e7b513fd2

# iot: retained-message replay is unobservable — the published iot:Data-ATS MQTT listener never completes CONNECT, so the retain-on-subscribe check hangs to the harness limit

> _Note: this issue was identified against the code base at commit `53152f24252eb4c6b050f7ba33da4a3e7b513fd2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
MQTT CONNECT / SUBSCRIBE / PUBLISH (retain) at the iot:Data-ATS data endpoint

## Description
Cause 1 of 1: the MQTT broker serving the address DescribeEndpoint(endpointType='iot:Data-ATS') publishes does not complete an MQTT session, so AWS IoT Core's retained-message semantics (retained PUBLISH stored, replayed with the retain bit set to a later subscriber, live publish delivered with the bit clear, zero-length retained publish deleting the retained message) can never be observed. Failing check: iot.iot-retained-message-is-pushed-on-subscribe-with-the-retain-flag-set ('Check timed out after 60.0s').

AWS: recorded behavior only — the Smithy model for iot carries no MQTT binding (its modeled surface is the HTTP control plane; only the retained-message READ path, iot-data GetRetainedMessage, is modeled). The recording shows AWS IoT Core serving MQTT over TLS on port 8883 at the published iot:Data-ATS address: a CONNECT presenting a registered, ACTIVE certificate whose attached policy allows iot:Connect is answered CONNACK rc=0; SUBSCRIBE at QoS 1 is answered SUBACK granting QoS 1; a PUBLISH with retain=1 by a principal holding iot:RetainPublish is PUBACKed and the payload stored as the topic's retained message; a client that subscribes AFTER that publish is immediately sent the stored payload with the retain flag SET; a subsequent live publish (retain=0) reaches the already-subscribed client with the retain flag CLEAR; a zero-length retained publish deletes the retained message, after which iot-data GetRetainedMessage on that topic refuses with HTTP 404 ResourceNotFoundException and the message 'Retained message not found' (that text is fixed, with no variable part), and a freshly subscribing client is replayed nothing.

Emulator: the check resolves the published address, rewrites it onto the host:port the emulator actually serves for 8883, and connects. No session is ever established — paho's on_connect never fires, so no CONNACK return code is produced at all, and the check hangs until the harness kills it at 60 s. No status, no error code and no message are emitted on this path; the retained PUBLISH, the SUBACK, the retain-bit replay and the delete-by-empty-payload behavior are all unreachable.

Why: a client relying on AWS's behavior gets neither a session nor a refusal. Device code blocks on CONNACK instead of connecting or being told why it cannot, and the whole retained-message contract silently disappears: a device that comes online after a retained publish receives no last-known state, cannot distinguish a replayed retained message (retain bit set) from a live one (bit clear), and a zero-length retained publish neither clears state nor makes GetRetainedMessage answer 404 ResourceNotFoundException 'Retained message not found'.

Fix: the data-plane MQTT listener/broker component behind the address published for iot:Data-ATS — it must complete the MQTT 3.1.1/5 CONNECT exchange after the TLS handshake, authenticate the presented client certificate against the registry, honour iot:Publish vs iot:RetainPublish, and implement retained-message storage shared with the iot-data retained-message HTTP path (GetRetainedMessage/Publish retain=true), including replay-with-retain-bit on SUBSCRIBE and delete-on-empty-retained-payload. This is not an iot control-plane operation handler.

Related open issue: 20261117T200355Z-iot-the-published-iot-data-ats-mqtt-listener.md describes the same missing CONNACK but its Issue-Trace-Ids list does NOT carry this check's replay key, so this check is not covered by it; the retained-message semantics above are additionally not asserted by any check it lists. 20261109T120740Z-iot-data-the-mqtt-broker-address-published-by.md and 20261110T222008Z-iot-data-no-mqtt-broker-answers-at-the.md name the published hostname rather than this silence; this check already redirects the hostname to the emulator's own host:port and still gets no session.

Traces: iot.iot-retained-message-is-pushed-on-subscribe-with-the-retain-flag-set, an example of this one cause.

## Evidence
Expected (recorded AWS): CONNACK rc=0 for the provisioned mTLS identity; SUBACK granting QoS 1; the retained payload replayed to a late subscriber with msg.retain is True; a later live publish delivered to that same subscriber with msg.retain is False; after a zero-length retained publish, iot-data GetRetainedMessage(topic) raises ResourceNotFoundException with message exactly 'Retained message not found' and a third, fresh subscriber is replayed nothing (empty inbox).

Actual (emulator): no MQTT session is established at the published iot:Data-ATS address. The check reports 'Check timed out after 60.0s' — on_connect never fires, so the publisher's CONNACK return code is never even recorded (pub_state['rc'] stays None), and every later assertion (SUBACK granted [1], retain bit on replay, retain bit clear on live delivery, ResourceNotFoundException/'Retained message not found' after the empty retained publish, empty inbox for a fresh subscriber) is unreached. The TCP/TLS connect itself does not error; only the MQTT handshake is absent.

## Steps to Reproduce
1. Boot the emulator; call iot DescribeEndpoint(endpointType='iot:Data-ATS') and point the client at the host:port the emulator serves for 8883.
2. CreateThing; CreatePolicy allowing iot:Connect, iot:Publish, iot:RetainPublish, iot:Subscribe, iot:Receive on Resource '*'; CreateKeysAndCertificate(setAsActive=true); AttachPolicy to the certificate ARN and AttachThingPrincipal.
3. Connect an MQTT 3.1.1 client (paho, tls_set(certfile=..., keyfile=..., cert_reqs=CERT_NONE)) to 8883 and loop_start(). Observe the socket connects but no CONNACK arrives — on_connect never fires.
4. (Blocked by step 3) PUBLISH qos=1 retain=True to sim2real/<uniq>/retain, then connect a SECOND client and SUBSCRIBE qos=1 to that topic: expect the retained payload delivered with retain=True.
5. PUBLISH qos=1 retain=False on the same topic: expect delivery to the subscribed client with retain=False.
6. PUBLISH qos=1 retain=True with an empty payload, then call iot-data GetRetainedMessage(topic=...): expect ResourceNotFoundException 'Retained message not found'; connect a third client, subscribe to the topic plus a sibling topic, publish a control message on the sibling, and expect the retain topic's inbox to be empty.
7. Replay iot.iot-retained-message-is-pushed-on-subscribe-with-the-retain-flag-set to confirm.
