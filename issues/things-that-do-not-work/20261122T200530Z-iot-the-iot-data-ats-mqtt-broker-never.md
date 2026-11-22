Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.iot-policy-without-publish-permission-blocks-the-publish-and-the-message-never-arrives iot.mqtt-topic-scoped-policy-confines-publish-to-its-resource-arn iot.mqtt-topic-scoped-policy-confines-subscribe-to-its-topicfilter-arn iot.mqtt-connect-is-refused-when-the-client-id-falls-outside-the-policy-client-arn iot.inactive-certificate-is-refused-at-the-mqtt-handshake
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot.iot-policy-without-publish-permission-blocks-the-publish-and-the-message-never-arrives|run#21|AssertionError|assert info.is_published() is False, (
Issue-Key: iot.mqtt-topic-scoped-policy-confines-publish-to-its-resource-arn|run#28|AssertionError|assert info.is_published() is False, (
Issue-Key: iot.mqtt-topic-scoped-policy-confines-subscribe-to-its-topicfilter-arn|run#33|AssertionError|assert out_granted is None, (
Issue-Key: iot.mqtt-connect-is-refused-when-the-client-id-falls-outside-the-policy-client-arn|run#22|AssertionError|assert not any(o["outcome"] == "accepted" for o in outcomes), (
Issue-Key: iot.inactive-certificate-is-refused-at-the-mqtt-handshake|_await_sustained_refusal#22|Timeout|time.sleep(5)
Issue-Commit: d8571c81dd15a8f22d7c2603104b67b8a6bcc578

# iot: the iot:Data-ATS MQTT broker never evaluates IoT policies or certificate status — 4 net-new causes (unauthorized PUBLISH is PUBACKed, out-of-scope SUBSCRIBE is granted, out-of-client-ARN CONNECT is accepted, INACTIVE certificate still authenticates)

> _Note: this issue was identified against the code base at commit `d8571c81dd15a8f22d7c2603104b67b8a6bcc578`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
MQTT data-plane authorization (CONNECT / PUBLISH / SUBSCRIBE on iot:Data-ATS, port 8883)

## Description
The published iot:Data-ATS MQTT listener now completes TLS and MQTT sessions (so the broker-never-answers-CONNACK causes tracked by the open issues 20261117T200355Z-iot-the-published-iot-data-ats-mqtt-listener.md and 20261119T032013Z-iot-7-net-new-causes-the-published-iot.md no longer describe what is observed here), but the sessions it serves are UNAUTHENTICATED and UNAUTHORIZED: the broker admits every CONNECT, PUBACKs every PUBLISH and grants every SUBSCRIBE regardless of the AWS IoT policy attached to the presented certificate, and regardless of that certificate's ACTIVE/INACTIVE status. Four distinct enforcement points are missing. None of these failing checks is marked ALREADY FILED or OWNED in the suite report; the open issues named above are related but do not cover them.

Cause 1 of 4 — PUBLISH is not authorized, neither by action nor by topic resource ARN.
AWS: recorded behavior only (the MQTT data plane is not in the Smithy model; the control-plane model only provides the policy itself — Smithy: CreatePolicy.policyDocument, AttachPolicy(target), and TestAuthorization, which exists precisely to pre-compute these allow/deny decisions). AWS IoT evaluates the policy attached to the connecting certificate on every PUBLISH: a PUBLISH whose action is not granted (`iot:Publish` absent from the policy) or whose topic is outside the granted `arn:aws:iot:<region>:<account>:topic/<topic>` resource is refused — the broker sends NO PUBACK and closes the MQTT session, and the message is never delivered to any subscriber. The topic ARN in the grant is a value from the request/state (region, account id, topic name); the requirement is the scoping rule, not any one recorded topic.
Emulator: with a certificate whose policy grants Connect/Subscribe/Receive but NOT iot:Publish, the broker PUBACKed the QoS-1 publish (`AssertionError: the broker PUBACKed a publish from a certificate with no iot:Publish permission (topic 'sim2real/<id>/shared')`). With a certificate whose iot:Publish grant names exactly one topic ARN, a publish one topic level sideways was also PUBACKed (`AssertionError: the broker PUBACKed a publish to 'sim2real/<id>/denied' from a certificate whose iot:Publish grant names only 'arn:aws:iot:<region>:<account>:topic/sim2real/<id>/allowed'`). In both cases the session was neither closed nor the publish withheld. CONNACK rc=0 and a granted SUBACK on the same session had already proved the policy was in force at the broker, so this is a missing decision, not propagation lag.
Why: a client that relies on IoT policies as its device authorization boundary sees the emulator deliver messages real AWS refuses. Negative tests of least-privilege device policies pass vacuously, and a device-isolation regression (one tenant publishing into another tenant's topic namespace) is invisible until production.
Fix: the iot:Data-ATS broker's PUBLISH ingress path must call the same policy evaluator the control plane's TestAuthorization uses (action match on iot:Publish plus resource match against topic/<topic>), and on deny must suppress the PUBACK and close the session instead of acknowledging.
Traces: iot.iot-policy-without-publish-permission-blocks-the-publish-and-the-message-never-arrives, iot.mqtt-topic-scoped-policy-confines-publish-to-its-resource-arn.

Cause 2 of 4 — SUBSCRIBE is not authorized against the iot:Subscribe topicfilter ARN.
AWS: recorded behavior only. On a SUBSCRIBE whose filter falls outside the certificate's `iot:Subscribe` grant (`arn:aws:iot:<region>:<account>:topicfilter/<filter>`), AWS IoT sends no SUBACK at all and closes the TCP session; it does not answer SUBACK 0x80 (128). The refused filter then delivers nothing to that client even for messages published while the session was live.
Emulator: the broker answered the out-of-scope SUBSCRIBE with a normal SUBACK granting QoS 1 (`AssertionError: the broker SUBACKed an out-of-scope SUBSCRIBE to 'sim2real/<id>/denied' with granted QoS [1]; the certificate's iot:Subscribe grant names only 'arn:aws:iot:<region>:<account>:topicfilter/sim2real/<id>/allowed'`), on the same session whose in-scope filter had already been granted and round-tripped a payload.
Why: a client cannot distinguish an authorized subscription from an unauthorized one; a device subscribed outside its topicfilter scope receives traffic AWS would never deliver to it, so policy-scoped fan-out is silently wrong.
Fix: the broker's SUBSCRIBE ingress path must evaluate iot:Subscribe against topicfilter/<filter> per requested filter and, on deny, send no SUBACK and close the session (and never register the subscription).
Traces: iot.mqtt-topic-scoped-policy-confines-subscribe-to-its-topicfilter-arn.

Cause 3 of 4 — CONNECT is not authorized against the iot:Connect client ARN.
AWS: recorded behavior only. When the certificate's `iot:Connect` grant names a specific client ARN (`arn:aws:iot:<region>:<account>:client/<clientId>`), a CONNECT presenting any other client id is refused: AWS IoT closes the TLS transport during the handshake and never sends a CONNACK. Only the named client id reaches CONNACK rc=0.
Emulator: three consecutive CONNECT attempts with an out-of-scope client id were all accepted with CONNACK rc=0 (`AssertionError: the broker accepted client id 'sim2real-iot-denied-<id>' from a certificate whose iot:Connect grant names only 'sim2real-iot-allowed-<id>'; attempts were [{'outcome': 'accepted', 'rc': 0, ...} x3]`). The client-id component of the grant is simply not consulted.
Why: client-id pinning is the standard way to stop one credential impersonating another device. Against the emulator any client id works, so an impersonation defect in device firmware or in a provisioning template passes every test.
Fix: the broker's CONNECT handler must resolve the mTLS certificate to its attached policies and evaluate iot:Connect against client/<clientId> from the CONNECT packet, closing the transport without a CONNACK on deny.
Traces: iot.mqtt-connect-is-refused-when-the-client-id-falls-outside-the-policy-client-arn.

Cause 4 of 4 — certificate ACTIVE/INACTIVE status is stored but not enforced at the MQTT handshake.
AWS: recorded behavior only for the handshake; the control-plane state machine is modeled (Smithy: UpdateCertificate(certificateId, newStatus: CertificateStatus), PUT /certificates/{certificateId}, errors CertificateStateException 406 / ResourceNotFoundException 404). After UpdateCertificate sets a certificate INACTIVE, that certificate can no longer authenticate: its CONNECT never reaches CONNACK rc=0 (the broker errors/closes the transport), its publishes are withheld, while an ACTIVE twin certificate attached to the identical policy keeps connecting; restoring ACTIVE makes the identical connect-and-publish work again.
Emulator: the check reached no verdict — `Error: Check timed out after 60.0s`. Its own positive control (ACTIVE publish delivered, DescribeCertificate reporting INACTIVE after the update) precedes the refusal probe, and the probe is a bounded wait for a sustained refusal (3 consecutive refused CONNECTs, up to 120 s of status-propagation tolerance) that only runs to its deadline when refusals never come. Hypothesis: the broker's authenticator does not consult certificate status, so the INACTIVE certificate keeps being accepted and the refusal loop never terminates — consistent with causes 1-3, where the broker consults no registry state at all. The verdict here is therefore a hang rather than an observed wrong value; what is grounded is that no refusal was observable within the window.
Why: revocation by UpdateCertificate INACTIVE is the emergency control for a compromised device. If it changes only the DescribeCertificate read and not the broker's admission decision, a client's revocation runbook is validated against an emulator where revocation does nothing, and the refusal is additionally unobservable (a silent hang instead of a close or error).
Fix: the broker's mTLS authentication step must look up the presented certificate in the iot service state and refuse the handshake unless its status is ACTIVE, propagating a status change to new CONNECTs promptly; the refusal must be observable (transport close / CONNACK failure), never a hang.

## Evidence
Suite: 229/351 passed. Failing checks clustered here, all under the report heading `### iot` and none marked ALREADY FILED or OWNED:

1) [mismatch] iot.iot-policy-without-publish-permission-blocks-the-publish-and-the-message-never-arrives
   Expected: no PUBACK and a session close for a PUBLISH from a certificate without iot:Publish; the payload never delivered to the authorized subscriber.
   Actual: `AssertionError: the broker PUBACKed a publish from a certificate with no iot:Publish permission (topic 'sim2real/m9f2c80f92739/shared')` (check line 261). Session was not closed.

2) [mismatch] iot.mqtt-topic-scoped-policy-confines-publish-to-its-resource-arn
   Expected: no PUBACK and a session close for a publish to a topic outside the granted topic ARN.
   Actual: `AssertionError: the broker PUBACKed a publish to 'sim2real/ma246a4c592ba/denied' from a certificate whose iot:Publish grant names only 'arn:aws:iot:us-west-2:000000000000:topic/sim2real/ma246a4c592ba/allowed'` (check line 300). The in-scope publish on the same session had already been delivered to the unscoped watcher, so the policy was in force.

3) [mismatch] iot.mqtt-topic-scoped-policy-confines-subscribe-to-its-topicfilter-arn
   Expected: no SUBACK (not even 0x80) and a TCP close for an out-of-scope SUBSCRIBE.
   Actual: `AssertionError: the broker SUBACKed an out-of-scope SUBSCRIBE to 'sim2real/m6a0c612dc568/denied' with granted QoS [1]; the certificate's iot:Subscribe grant names only 'arn:aws:iot:us-west-2:000000000000:topicfilter/sim2real/m6a0c612dc568/allowed'` (check line 319).

4) [mismatch] iot.mqtt-connect-is-refused-when-the-client-id-falls-outside-the-policy-client-arn
   Expected: every out-of-scope CONNECT closed without a CONNACK (outcome 'closed-without-connack').
   Actual: `AssertionError: the broker accepted client id 'sim2real-iot-denied-b3bdbc531736' from a certificate whose iot:Connect grant names only 'sim2real-iot-allowed-ccb0e3038ec0'; attempts were [{'outcome': 'accepted', 'rc': 0, 'detail': 'CONNACK rc=0'}, {'outcome': 'accepted', 'rc': 0, 'detail': 'CONNACK rc=0'}, {'outcome': 'accepted', 'rc': 0, 'detail': 'CONNACK rc=0'}]` (check line 323). The in-scope client id had already connected, subscribed and round-tripped a message.

5) [mismatch] iot.inactive-certificate-is-refused-at-the-mqtt-handshake
   Expected: after UpdateCertificate newStatus=INACTIVE, at least 3 consecutive CONNECT attempts with that certificate refused with rc != 0 plus an error or disconnect, while the ACTIVE twin still connects; restoring ACTIVE restores publish delivery.
   Actual: `Error: Check timed out after 60.0s` — no refusal was ever observed; the check's bounded refusal wait (_STATUS_PROPAGATION_S = 120 s, _FREEZE_ATTEMPTS = 3) only exhausts when the broker keeps accepting the INACTIVE material. Hypothesis (not directly observed): certificate status is not consulted by the broker's authenticator.

Note on host/port/ids: the account id 000000000000, region, generated topic/client/certificate names and timestamps above are per-run values filling the ARN and message templates; the requirement is the scoping and refusal rules, not those literals.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite (run_behavioral_suite) — the five checks listed in trace_ids fail as shown.
2. Manually, against the emulator's iot endpoint:
   a. CreatePolicy with Statement [{Effect: Allow, Action: [iot:Connect, iot:Subscribe, iot:Receive], Resource: "*"}] (no iot:Publish), CreateKeysAndCertificate(setAsActive=true), AttachPolicy(policy, certificateArn).
   b. DescribeEndpoint(endpointType="iot:Data-ATS") and open a mutual-TLS MQTT 3.1.1 session to that host on 8883 with the issued cert/key. CONNECT succeeds (expected).
   c. PUBLISH qos=1 to any topic: the emulator returns a PUBACK. AWS returns no PUBACK and closes the session.
3. Repeat with a policy whose iot:Publish Resource is arn:aws:iot:<region>:<account>:topic/<base>/allowed and publish to <base>/denied — again PUBACKed instead of refused.
4. Repeat with a policy whose iot:Subscribe Resource is arn:aws:iot:<region>:<account>:topicfilter/<base>/allowed and SUBSCRIBE to <base>/denied — the emulator answers SUBACK granted QoS 1 instead of sending no SUBACK and closing the session.
5. Repeat with a policy whose iot:Connect Resource is arn:aws:iot:<region>:<account>:client/<allowedClientId> and CONNECT with a different client id — the emulator answers CONNACK rc=0 instead of closing the transport without a CONNACK.
6. With a certificate that has connected successfully, call UpdateCertificate(certificateId, newStatus="INACTIVE"), confirm DescribeCertificate reports INACTIVE, then CONNECT again with the same material: the emulator still admits the session (and the status-enforcement check hangs until its timeout) instead of refusing the handshake.
