Issue-Severity: CRITICAL
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.mqtt-connect-is-refused-when-the-client-id-falls-outside-the-policy-client-arn iot.iot-policy-without-publish-permission-blocks-the-publish-and-the-message-never-arrives iot.mqtt-topic-scoped-policy-confines-publish-to-its-resource-arn iot.mqtt-topic-scoped-policy-confines-subscribe-to-its-topicfilter-arn iot.mqtt-topic-filter-wildcard-depth-semantics iot.mqtt-offline-persistent-session-discards-qos0-publishes-and-keeps-qos1 iot.iot-retained-message-is-pushed-on-subscribe-with-the-retain-flag-set iot.mqtt-last-will-fires-on-an-ungraceful-close-and-not-on-a-clean-disconnect iot.presence-lifecycle-events-separate-clean-disconnect-from-clientid-takeover iot.inactive-certificate-is-refused-at-the-mqtt-handshake
Issue-Key: iot.mqtt-connect-is-refused-when-the-client-id-falls-outside-the-policy-client-arn|run#22|AssertionError|assert not any(o["outcome"] == "accepted" for o in outcomes), (
Issue-Key: iot.iot-policy-without-publish-permission-blocks-the-publish-and-the-message-never-arrives|run#21|AssertionError|assert info.is_published() is False, (
Issue-Key: iot.mqtt-topic-scoped-policy-confines-publish-to-its-resource-arn|run#28|AssertionError|assert info.is_published() is False, (
Issue-Key: iot.mqtt-topic-scoped-policy-confines-subscribe-to-its-topicfilter-arn|run#33|AssertionError|assert out_granted is None, (
Issue-Key: iot.mqtt-topic-filter-wildcard-depth-semantics|run#24|AssertionError|assert observed[filt] == expected, (
Issue-Key: iot.mqtt-offline-persistent-session-discards-qos0-publishes-and-keeps-qos1|run#31|AssertionError|assert survived == [], (
Issue-Key: iot.iot-retained-message-is-pushed-on-subscribe-with-the-retain-flag-set|_wait_for_topic#18|Timeout|raise AssertionError(f"{what} within {timeout_s}s; the inbox held {seen!r}")
Issue-Key: iot.mqtt-last-will-fires-on-an-ungraceful-close-and-not-on-a-clean-disconnect|_wait_for_payload#22|Timeout|time.sleep(0.2)
Issue-Key: iot.presence-lifecycle-events-separate-clean-disconnect-from-clientid-takeover|_wait_for_inbox#14|Timeout|time.sleep(0.2)
Issue-Key: iot.inactive-certificate-is-refused-at-the-mqtt-handshake|_await_sustained_refusal#22|Timeout|time.sleep(5)
Issue-Commit: 657b97543b8e608c75aab9edda765f25072992b5

# iot: 7 net-new causes at the iot:Data-ATS MQTT broker — IoT policy authorization is never evaluated, '#' matches the parent level, QoS 0 is queued for an offline session, and retain / last-will / presence / certificate-status are all unobservable

> _Note: this issue was identified against the code base at commit `657b97543b8e608c75aab9edda765f25072992b5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
MQTT CONNECT / PUBLISH / SUBSCRIBE at the iot:Data-ATS endpoint (DescribeEndpoint endpointType=iot:Data-ATS)

## Description
The published iot:Data-ATS broker now completes TLS and answers CONNACK rc=0 (several checks below get CONNACK rc=0, SUBACK [1] and PUBACK), so the older open issues 20261117T200355Z-iot-the-published-iot-data-ats-mqtt-listener.md, 20261118T164940Z-iot-retained-message-replay-is-unobservable-the-published.md and 20261119T032013Z-iot-7-net-new-causes-the-published-iot.md (which all describe a broker that never answers CONNACK) no longer describe what is served and do not cover these. What is served is a broker that performs no authorization at all and whose session/retain/will/presence semantics are absent or are plain-MQTT rather than AWS IoT Core. 7 causes.

Cause 1 of 7: the broker authorizes nothing — the attached IoT policy is never evaluated on CONNECT, PUBLISH or SUBSCRIBE.
AWS: AWS IoT Core authorizes every MQTT action against the policies attached to the connecting certificate principal. iot:Connect is authorized against the client ARN (arn:aws:iot:<region>:<account>:client/<clientId>); iot:Publish against arn:aws:iot:<region>:<account>:topic/<topic>; iot:Subscribe against arn:aws:iot:<region>:<account>:topicfilter/<filter>. A CONNECT whose clientId is not matched by any iot:Connect resource is refused at the handshake (the broker closes / returns a non-zero CONNACK rather than rc=0); a PUBLISH not matched by an iot:Publish grant is not PUBACKed and the message is not delivered; a SUBSCRIBE not matched by an iot:Subscribe grant is answered with granted QoS 128 (failure). Smithy: the IoT control plane models the policy document (CreatePolicy/AttachPolicy) but the broker's enforcement is not modeled — recorded behavior only for the wire effect, as described above.
Emulator: every attempt is allowed. Replay shows (a) 'the broker accepted client id sim2real-iot-denied-<id> from a certificate whose iot:Connect grant names only sim2real-iot-allowed-<id>; attempts were [{outcome: accepted, rc: 0}, x3]'; (b) 'the broker PUBACKed a publish from a certificate with no iot:Publish permission (topic sim2real/<id>/shared)'; (c) 'the broker PUBACKed a publish to sim2real/<id>/denied from a certificate whose iot:Publish grant names only arn:aws:iot:<region>:<account>:topic/sim2real/<id>/allowed'; (d) 'the broker SUBACKed an out-of-scope SUBSCRIBE to sim2real/<id>/denied with granted QoS [1]; the certificate's iot:Subscribe grant names only arn:aws:iot:<region>:<account>:topicfilter/sim2real/<id>/allowed'. The positive controls in the same checks pass, so the wiring works and only the authorization decision is missing.
Why: a client that relies on IoT policies as its security boundary gets none. Least-privilege device policies, per-thing topic scoping and client-id pinning all read as enforced against this emulator while any certificate can connect under any client id and publish or subscribe to any topic, so an authorization regression in the user's policy is invisible until it reaches real AWS.
Fix: the MQTT broker's authorization layer in the iot data-plane/broker component — the CONNECT, PUBLISH and SUBSCRIBE paths must resolve the connecting certificate principal's attached policies and evaluate iot:Connect/iot:Publish/iot:Subscribe (and iot:Receive on delivery) against the client/topic/topicfilter ARNs before accepting, instead of unconditionally accepting.
Traces: iot.mqtt-connect-is-refused-when-the-client-id-falls-outside-the-policy-client-arn, iot.iot-policy-without-publish-permission-blocks-the-publish-and-the-message-never-arrives, iot.mqtt-topic-scoped-policy-confines-publish-to-its-resource-arn, iot.mqtt-topic-scoped-policy-confines-subscribe-to-its-topicfilter-arn.

Cause 2 of 7: the multi-level wildcard '#' also matches the filter's own parent level.
AWS: in AWS IoT Core a topic filter '<prefix>/#' matches only topics that have at least one level below <prefix>; it does NOT match the bare topic '<prefix>'. (This is the documented AWS IoT divergence from the plain MQTT 3.1.1 rule, where 'sport/#' also matches 'sport'.) Recorded behavior: the check publishes to '<prefix>', '<prefix>/one' and '<prefix>/one/two' and expects the '<prefix>/#' subscriber to receive exactly the two deeper topics.
Emulator: the subscriber on '<prefix>/#' received the full topic set including the parent — 'the filter sim2real<id>/# received the topic set [<prefix>, <prefix>/one, <prefix>/one/two], expected exactly [<prefix>/one, <prefix>/one/two] after a 15.0s hold'. The emulator is applying the plain-MQTT wildcard rule.
Why: a client that fans out a parent topic and its subtree to different consumers gets duplicate delivery of the parent message, and a rule or subscriber meant to see only the subtree sees the parent too — the opposite direction of a leak that only shows up on real AWS as a missing message.
Fix: the broker's topic-filter matcher in the iot data-plane/broker component — '#' must require at least one level beyond the literal prefix.
Traces: iot.mqtt-topic-filter-wildcard-depth-semantics.

Cause 3 of 7: QoS 0 publishes to an OFFLINE persistent session are stored and replayed on resume.
AWS: AWS IoT Core's persistent session queue holds only QoS 1 messages for a disconnected session with cleanSession=false; QoS 0 messages addressed to an offline session are discarded and are never delivered when the session resumes.
Emulator: the QoS 0 messages were queued and delivered on resume — 'messages published at QoS 0 to an OFFLINE persistent session were stored and delivered on resume: [{"qos": 0, "seq": 0, ...}, {"qos": 0, "seq": 1, ...}, {"qos": 0, "seq": 2, ...}]'.
Why: a device that reconnects receives a burst of stale QoS 0 telemetry that real AWS drops, so ordering and freshness assumptions (and any de-duplication the device skips because QoS 0 cannot be replayed) break only against real AWS.
Fix: the persistent-session offline queue in the iot broker component — enqueue only QoS 1 for an offline session and drop QoS 0.
Traces: iot.mqtt-offline-persistent-session-discards-qos0-publishes-and-keeps-qos1.

Cause 4 of 7: a retained message is never pushed to a new subscriber with the retain flag set.
AWS: a PUBLISH with the retain flag is stored per topic, and the stored message is delivered to every later SUBSCRIBE whose filter matches, with the retain flag set on the delivered message.
Emulator: the check hangs waiting for that delivery and the replay records 'Check timed out after 60.0s'; no retained message is pushed on subscribe.
Why: a device that bootstraps its state from a retained topic (configuration, last-known desired value) blocks forever against the emulator, so retain-based designs cannot be exercised at all.
Fix: the broker's retain store and its SUBSCRIBE delivery path in the iot data-plane/broker component.
Traces: iot.iot-retained-message-is-pushed-on-subscribe-with-the-retain-flag-set.

Cause 5 of 7: a session's last-will message is never published on an ungraceful close.
AWS: when a session with a configured will terminates without a DISCONNECT packet, the broker publishes the will payload to the will topic; a clean DISCONNECT suppresses it.
Emulator: the check waits for the will publish after an abrupt socket close and the replay records 'Check timed out after 60.0s' — the will is never delivered, so neither half of the distinction (fires on ungraceful close, suppressed on clean disconnect) can be observed.
Why: device-liveness designs built on last-will notifications see no failure notification at all, so a dead device looks alive to every consumer.
Fix: the broker's session-teardown path in the iot data-plane/broker component — publish the stored will on any non-DISCONNECT termination.
Traces: iot.mqtt-last-will-fires-on-an-ungraceful-close-and-not-on-a-clean-disconnect.

Cause 6 of 7: presence lifecycle events ($aws/events/presence/...) are never published, so a clean disconnect and a client-id takeover are indistinguishable.
AWS: with lifecycle events enabled the broker publishes connected/disconnected events on the reserved $aws/events/presence/{connected,disconnected}/<clientId> topics, and the disconnect event carries a disconnectReason that distinguishes a client-initiated disconnect (CLIENT_INITIATED_DISCONNECT) from a takeover of the same client id.
Emulator: the check subscribes to the reserved presence topics and the replay records 'Check timed out after 60.0s' — no lifecycle event is ever delivered.
Why: fleet-presence monitoring and duplicate-client-id detection built on these reserved topics receive nothing, and the disconnectReason that drives alerting never exists.
Fix: the broker's lifecycle-event publisher in the iot data-plane/broker component, including the disconnectReason classification.
Traces: iot.presence-lifecycle-events-separate-clean-disconnect-from-clientid-takeover.

Cause 7 of 7: a certificate's ACTIVE/INACTIVE status is stored but not enforced at the MQTT handshake.
AWS: UpdateCertificate(newStatus=INACTIVE) takes effect at the broker's authenticator: an INACTIVE certificate's CONNECT never reaches CONNACK rc=0 and its publish is withheld, while an ACTIVE certificate on the same policy and topic keeps working; restoring ACTIVE makes the identical connect-and-publish succeed again.
Emulator: after the positive control passes and the status reads back INACTIVE from DescribeCertificate, the check hangs waiting for the refusal and the replay records 'Check timed out after 60.0s' — the handshake with INACTIVE material is not refused.
Why: revoking a compromised device by deactivating its certificate appears to work on the control plane while the device keeps connecting and publishing, which is a silent security failure.
Fix: the broker's TLS/mTLS authenticator in the iot data-plane/broker component — resolve the presented certificate's registry status on every CONNECT and refuse anything not ACTIVE.
Traces: iot.inactive-certificate-is-refused-at-the-mqtt-handshake.

## Evidence
Cause 1 — expected: CONNECT with an unauthorized client id refused (non-zero CONNACK / closed), unauthorized PUBLISH not PUBACKed and not delivered, out-of-scope SUBSCRIBE answered granted QoS 128. Actual: 'the broker accepted client id sim2real-iot-denied-e739825af75b from a certificate whose iot:Connect grant names only sim2real-iot-allowed-23532b6a31b3; attempts were [{outcome: accepted, rc: 0, detail: CONNACK rc=0} x3]'; 'the broker PUBACKed a publish from a certificate with no iot:Publish permission'; 'the broker PUBACKed a publish to .../denied from a certificate whose iot:Publish grant names only arn:aws:iot:us-west-2:000000000000:topic/.../allowed'; 'the broker SUBACKed an out-of-scope SUBSCRIBE ... with granted QoS [1]'.
Cause 2 — expected the '<prefix>/#' subscriber to receive exactly ['<prefix>/one','<prefix>/one/two']; actual it also received '<prefix>'.
Cause 3 — expected [] surviving QoS 0 messages after resume; actual three QoS 0 payloads were replayed.
Causes 4-7 — the check reaches its wait for the retained push / will publish / presence event / handshake refusal and the replay reports 'Check timed out after 60.0s' with no such delivery or refusal, while the positive control in the same check (ACTIVE certificate connect+publish, SUBACK granted [1]) succeeds — so the broker is reachable and serving and only the behavior under test is absent.

## Steps to Reproduce
1. Build and boot the emulator, then DescribeEndpoint(endpointType='iot:Data-ATS') for the broker host.
2. Authorization: create a policy granting iot:Connect only on client/<allowed-id> and iot:Publish only on topic/<allowed-topic>, attach it to a certificate, then (a) CONNECT with a different client id, (b) PUBLISH to an unlisted topic, (c) PUBLISH from a certificate whose policy has no iot:Publish at all, (d) SUBSCRIBE to a filter outside the iot:Subscribe resource. Observe CONNACK rc=0, PUBACK and SUBACK granted [1] in every case.
3. Wildcard: subscribe to '<prefix>/#', publish to '<prefix>', '<prefix>/one', '<prefix>/one/two'; observe the parent topic delivered.
4. Persistent session: connect with cleanSession=false, subscribe QoS 1, disconnect, publish QoS 0 and QoS 1 messages, reconnect; observe the QoS 0 messages replayed.
5. Retain: PUBLISH with retain=true, then SUBSCRIBE from a fresh client; observe nothing delivered.
6. Will: connect with a will, kill the socket without DISCONNECT; observe nothing on the will topic.
7. Presence: subscribe to $aws/events/presence/connected/# and .../disconnected/#, connect and cleanly disconnect a client, then take over the same client id; observe no events.
8. Certificate status: with an ACTIVE certificate confirm connect+publish works, call UpdateCertificate(newStatus='INACTIVE'), confirm DescribeCertificate reads INACTIVE, then CONNECT again with the same material; observe it still succeeds.
Replay: iot.mqtt-connect-is-refused-when-the-client-id-falls-outside-the-policy-client-arn, iot.iot-policy-without-publish-permission-blocks-the-publish-and-the-message-never-arrives, iot.mqtt-topic-scoped-policy-confines-publish-to-its-resource-arn, iot.mqtt-topic-scoped-policy-confines-subscribe-to-its-topicfilter-arn, iot.mqtt-topic-filter-wildcard-depth-semantics, iot.mqtt-offline-persistent-session-discards-qos0-publishes-and-keeps-qos1, iot.iot-retained-message-is-pushed-on-subscribe-with-the-retain-flag-set, iot.mqtt-last-will-fires-on-an-ungraceful-close-and-not-on-a-clean-disconnect, iot.presence-lifecycle-events-separate-clean-disconnect-from-clientid-takeover, iot.inactive-certificate-is-refused-at-the-mqtt-handshake
