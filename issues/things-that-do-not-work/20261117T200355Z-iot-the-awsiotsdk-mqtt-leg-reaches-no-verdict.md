Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.iot-mqtt-publish-subscribe
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot.iot-mqtt-publish-subscribe|_mqtt_awsiotsdk#16|Timeout|time.sleep(3)
Issue-Commit: eb25315957aceaabd7d1a9e502f713207998e734

# iot: the awsiotsdk (AWS CRT) leg of iot-mqtt-publish-subscribe still reaches no verdict at the published iot:Data-ATS address, and the two TLS facts that can explain it are the core's

> _Split out of `20261117T200355Z-iot-the-published-iot-data-ats-mqtt-listener.md` (whose one cause — a listener that accepted the connection and never answered CONNACK — is implemented: eight of its nine checks now complete the handshake and seven pass). This check is kept open on its own because the leg that still fails is driven by a DIFFERENT client library and the emulator's answer to it cannot be read from the replay._

## Operation
MQTT CONNECT (iot:Data-ATS data endpoint), driven by the `awsiotsdk` / `awscrt` client

## Description
AWS: recorded behavior only — AWS IoT Core serves MQTT over TLS at the address
`DescribeEndpoint(endpointType='iot:Data-ATS')` returns, and the AWS IoT Device SDK (whose Python
distribution wraps the AWS CRT) opens a session on it with the device's certificate and key, then
subscribes and publishes.

Emulator: the broker at that address now completes the MQTT 3.1.1 and MQTT 5 exchanges — the sibling
checks of the parent issue connect, subscribe, publish and receive over the same socket, in the clear
and over TLS — but this check, whose connection is opened by `_mqtt_awsiotsdk`, still reaches no
verdict: `Check timed out after 60.0s`, with no divergence recorded beyond the timeout. So what the
emulator answered that client, and at which layer the exchange stopped, is not observable from the
replay.

Why: a client written against the AWS IoT Device SDK — the SDK AWS itself documents for devices —
cannot be exercised against this emulator, while a paho client can. Anything that library does
differently (its own TLS stack, its ALPN list, its default port) is untested here.

## Why this is open rather than fixed
Two facts about this emulator's TLS are the candidates, and BOTH live in the read-only `vendor/`
tree, so neither can be changed from inside the emulator:

1. The one process certificate is built with no client authentication —
   `vendor/server/src/tls_certificate_authority.rs:92-100`
   (`TlsCertificateAuthority::server_config`, `with_no_client_auth()`, the configuration every
   listener and the in-band upgrader start from). A CRT client built the way the SDK's
   `mtls_from_path` builds one presents a client certificate the server never asks for, and no
   handler can see one; the same blocker is already recorded for the credential-provider exchange in
   `20261114T082236Z-iot-credential-provider-mtls-exchange-is-vendor-blocked.md`.
2. The upgrader offers at most ONE ALPN protocol — `vendor/server/src/listeners.rs:264-279`
   (`TlsUpgrader::accept(conn, alpn)`: "With `alpn = Some(id)` the server offers only `id`: a client
   offering other protocols is refused with `no_application_protocol`"). The published data endpoint
   serves BOTH wires on one socket, so it would have to offer `x-amzn-mqtt-ca` (the protocol a CRT
   client names when it dials the MQTT port 443) and `http/1.1` (the HTTPS data plane) together.
   Offering either alone refuses the other client, so the broker's listener passes `None` and selects
   no ALPN at all (`services/common/src/in_band_tls.rs:76-79`).

REQUIRED CORE CHANGE: a TLS upgrader that (a) accepts a LIST of ALPN protocols and selects whichever
one the client offered, leaving a client that offers none unaffected, and (b) requests the client
certificate when one is available and places the presented chain where the handler can read it. With
(a) an address can serve both wires to clients that name their protocol; with (b) the broker can
resolve a connection to the IoT certificate principal, which is also what the certificate-status and
IoT-policy enforcement in
`20261122T200530Z-iot-the-iot-data-ats-mqtt-broker-never.md` needs.

## Evidence
Expected: the `awsiotsdk` leg connects to the published address, subscribes to its topic filter and
is delivered its own publication, as the paho legs of the sibling checks are.

Actual: `iot.iot-mqtt-publish-subscribe` — `Check timed out after 60.0s`; the replay reports no
first divergence other than the timeout, and the failing key is a wait inside the helper that opens
the SDK connection (`_mqtt_awsiotsdk#16`). Every other check of the parent issue that drives paho
(3.1.1 and 5) now passes: publish/subscribe routing, topic-filter wildcard depth, presence lifecycle
events, the shadow delta push, the MQTT 5 properties and the fleet-provisioning claim exchange.

## Steps to Reproduce
1. Boot the emulator; `DescribeEndpoint(endpointType='iot:Data-ATS')` for the address, and
   `CreateKeysAndCertificate(setAsActive=true)` with a policy allowing
   `iot:Connect/Publish/Subscribe/Receive` attached to it.
2. Point an `awsiotsdk` MQTT connection (`mqtt_connection_builder.mtls_from_path`) at that
   host and port with the issued certificate and key, subscribe to a topic filter and publish to a
   topic it covers.
3. Observe the session is not established within the check's budget, while the same exchange driven
   by paho (`tls_set(certfile=..., keyfile=..., cert_reqs=CERT_NONE)`) completes.
4. Replay `iot.iot-mqtt-publish-subscribe`.
