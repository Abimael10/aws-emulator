Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot-data.connected-client-connection-apis-reflect-live-session
Issue-Shape-Id: com.amazonaws.iotdataplane#IotMoonrakerService
Issue-Key: iot-data.connected-client-connection-apis-reflect-live-session|run#40|AssertionError|assert conn.get("sourcePort", 0) > 0, f"GetConnection with includeSocketInformation=True returned sourcePort={conn.get('sourcePort')!r}"
Issue-Commit: 657b97543b8e608c75aab9edda765f25072992b5

# iot-data GetConnection with includeSocketInformation=true omits sourcePort (serves the modeled default 0) for a live MQTT session

> _Note: this issue was identified against the code base at commit `657b97543b8e608c75aab9edda765f25072992b5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetConnection

## Description
Cause 1 of 1: GetConnection(clientId, includeSocketInformation=true) answers 200 for a client that is genuinely connected (connected=true, clientId echoed, sourceIp populated) but reports sourcePort=0 — i.e. the member is not emitted and the model's default surfaces — so the socket information the flag asks for is only half served.

AWS: Smithy (iotdataplane) GetConnection -> GET /connections/{clientId}?includeSocketInformation, output GetConnectionResponse with members connected, clientId, thingName, cleanSession, sourceIp, sourcePort (integer, @default 0), targetIp, targetPort, keepAliveDuration, connectedSince, sessionExpiry, vpcEndpointId; errors ForbiddenException 403, InvalidRequestException 400, ResourceNotFoundException 404, ThrottlingException 429, InternalFailureException 500. The model carries no prose doc for the member, so: recorded behavior only — the recording shows that when includeSocketInformation=true and the client has an open MQTT session, AWS fills the socket quartet from the actual TCP connection, i.e. sourceIp/sourcePort are the device-side address and port of the live socket and both are present; sourcePort is the ephemeral client port, a positive integer in the ephemeral range, never 0. 0 is the shape's absent-value default, not a port a connected socket can have.

Emulator: in the same response that passed connected is True, clientId == the client's id and a non-empty sourceIp, sourcePort came back 0 — 'AssertionError: GetConnection with includeSocketInformation=True returned sourcePort=0'. The emulator therefore knows the session exists and knows its source address but does not record (or does not serialize) the source port of the accepted socket.

Why: a client relying on AWS's behavior uses the socket quartet to identify and correlate a device's connection (fleet troubleshooting, NAT/VPC attribution, matching broker connections against flow logs, deciding whether two client ids share an address:port). With sourcePort=0 the pair is not a real socket: correlation silently matches nothing or matches every connection from that address, and code that validates the port (>0, in the ephemeral range) treats a healthy connection as malformed. Because 0 is also the modeled default, a caller cannot distinguish 'not collected' from 'reported as zero'.

Fix: the iot-data GetConnection handler and the connection-registry state behind the MQTT listener — the accepted socket's peer port must be captured alongside the peer address when a session is established, and serialized as sourcePort whenever includeSocketInformation=true (together with targetIp/targetPort for the broker side). No change to the 200 status or to the rest of the projection is implied.

Traces: iot-data.connected-client-connection-apis-reflect-live-session, which reached this assertion only after a real mTLS MQTT session connected and SUBACKed, so the session state the port belongs to demonstrably exists.

## Evidence
Expected (recorded AWS): GetConnection(clientId=<live client>, includeSocketInformation=true) -> 200 with connected=true, clientId=<the request's clientId>, sourceIp=<the device socket's peer address> and sourcePort=<that socket's peer port, a positive integer>. Fixed text of the requirement: sourcePort is present and > 0 whenever a session is open and socket information was requested; the values themselves (address, port number) come from the live socket and vary per run.

Actual (emulator): 200 with connected=true, clientId echoed correctly, sourceIp non-empty, sourcePort=0.
Failure: AssertionError: GetConnection with includeSocketInformation=True returned sourcePort=0 (check line 178).

No error code or message is involved — the divergence is a missing output member on a success response.

## Steps to Reproduce
1. Boot the emulator. Create a certificate (iot CreateKeysAndCertificate setAsActive=true), create a policy allowing iot:* on *, attach the policy to the certificate ARN.
2. iot DescribeEndpoint(endpointType='iot:Data-ATS') and open an MQTT 3.1.1 mTLS session to that address on port 8883 with a known clientId; wait for CONNACK rc=0 and subscribe to a topic filter (wait for SUBACK).
3. Call iot-data GetConnection(clientId=<that clientId>, includeSocketInformation=True).
4. Observe connected=true and a populated sourceIp but sourcePort=0, where AWS returns the live socket's positive peer port.
5. Re-replay iot-data.connected-client-connection-apis-reflect-live-session to confirm.
