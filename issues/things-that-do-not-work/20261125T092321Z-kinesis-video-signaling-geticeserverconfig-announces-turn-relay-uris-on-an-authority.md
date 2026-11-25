Issue-Source: trace-sweep-divergence
Issue-Trace: kinesis-video-signaling-ice-server-config-from-https-endpoint
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: kinesis-video-signaling-ice-server-config-from-https-endpoint
Issue-Shape-Id: com.amazonaws.kinesisvideosignaling#AWSAcuitySignalingService
Issue-Key: kinesis-video-signaling|GetIceServerConfig|2xx->2xx|$.IceServerList[*]
Issue-Key: kinesis-video-signaling|GetIceServerConfig|2xx->2xx|$.IceServerList[*].Uris[*]
Issue-Commit: 3198778d4a31e10d6693ef53b9d76ae067021adb

# GetIceServerConfig announces TURN relay URIs on an authority the emulator does not serve

> _Note: this issue was identified against the code base at commit `3198778d4a31e10d6693ef53b9d76ae067021adb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetIceServerConfig

## Description
Cause 1 of 1: the TURN relay authority in GetIceServerConfig's IceServerList[].Uris is not an address this emulator answers on. (Fixture kinesis-video-signaling-ice-server-config-from-https-endpoint, step task#4 — the call made against the signaling channel's HTTPS endpoint.)

AWS: Smithy (services/service-com-amazonaws-kinesisvideosignaling-awsacuitysignalingservice/smithy-model): GetIceServerConfig, POST /v1/get-ice-server-config, success 200, output GetIceServerConfigResponse.IceServerList (list of IceServer{Uris, Username, Password, Ttl}); errors ClientLimitExceededException/InvalidArgumentException/InvalidClientException (400), NotAuthorizedException (401), ResourceNotFoundException (404), SessionExpiredException (400). IceServer$Uris is documented as turn:/turns: URIs in the I-D.petithuguenin-behave-turn-uris form; an ICE agent takes each URI verbatim and DIALS it, so AWS answers with the address of a relay that is actually reachable for the endpoint the caller asked at. Recorded behavior only for the concrete shape of the issuance: three URIs (turn ...?transport=udp, turns ...?transport=udp, turns ...?transport=tcp), Username of the form <unix-expiry>:<opaque-token>, a Password, and Ttl (300 in the recording). Both expected and actual status here are 200 and no error code or error message is involved, so no message wording is in dispute; the divergence is in the response body.

Emulator: the replayed step returns 200 with IceServerList[0] whose Uris all name the authority 127.0.0.1:39743, while the emulator is being reached on its data listener (:39175 in this run). The replay classified $.IceServerList[0].Uris[0] as host_not_own: the announced host:port is not one this process serves, so the whole IceServerList[0] object is reported as not matching the expected shape ($.IceServerList[0]: missing). The announced authority tracks an endpoint value that travelled with the request (the signaling channel's HTTPS endpoint resolved in an earlier step of the same fixture) rather than the socket the request was actually served on — exactly the failure the relay-address module's own header forbids ("a host:port the request merely CARRIED ... a forwarding front end, an endpoint of an earlier run").

Why: a client that trusts the answer configures its ICE agent with turn:/turns: URIs pointing at a port nothing in the environment listens on, so every relay candidate fails to connect for the whole reported Ttl, while the response claims a working relay. Generally: whatever host and port this emulator is reached on, every URI it issues must name that same serving authority — a per-run host, port or generated credential value is not part of the requirement, only the invariant that the announced authority is the one serving the call.

Fix: the relay-address resolution behind GetIceServerConfig's serializer input — state::relay_address::relay_authority / relay_uris, as called from the GetIceServerConfig handler with the served-base string — must resolve the authority from the listener this process answers on (the emulator's own data-plane listener address) and must ignore any endpoint authority supplied by, or derived from, the incoming request; a stored issuance must likewise be re-addressed onto that same serving authority before it is re-served.

Traces: fixture kinesis-video-signaling-ice-server-config-from-https-endpoint step task#4. Code under suspicion: services/service-com-amazonaws-kinesisvideosignaling-awsacuitysignalingservice/src/state/relay_address.rs lines 58-77 (relay_authority/relay_uris) and services/service-com-amazonaws-kinesisvideosignaling-awsacuitysignalingservice/src/ops/get_ice_server_config.rs line 382 (relay_uris(ctx.endpoint().as_str())) and lines 415-419 (readdress_credential). No existing open issue covers this operation (list_existing_issues for kinesis-video-signaling / kinesisvideosignaling returns only the already-resolved bootstrap issue).

## Evidence
Replay divergence (verdict=mismatch, divergence_key service=kinesis-video-signaling|GetIceServerConfig|status=200->200|keys=$.IceServerList[0],$.IceServerList[0].Uris[0]):
  expected_status 200, actual_status 200
  $.IceServerList[0]: missing (expected {'Password': '<shaped-not-asserted>', 'Ttl': 300, 'Uris': ['<shaped-not-asserted>'], 'Username': '<shaped-not-asserted>'}, got {'Uris': ['turn:127.0.0.1:39743?transport=udp', 'turns:127.0.0.1:39743?transport=udp', 'turns:127.0.0.1:39743?transport=tcp'], 'Username': '1795598768:VhGDxLmRGZiqgAVV', 'Password': 'UpnMzXXWt2qObMEdlqCzp1KQBvAUTEQj', 'Ttl': 300})
  $.IceServerList[0].Uris[0]: host_not_own (expected '<shaped-not-asserted>', got 'turn:127.0.0.1:39743?transport=udp')
Emulator reachability reported by the same replay: "/__admin/health answered 200 and the :39175 data listener accepts" — i.e. the announced relay port 39743 is not the port serving the call.
Expected: every IceServer$Uris entry names the host:port this emulator is serving the request on, in the documented turn:/turns: form with ?transport=udp|tcp.
Actual: all three URIs name 127.0.0.1:39743, an authority this process does not answer on (host_not_own). The concrete ports, username token and password above are per-run values, not requirements.

## Steps to Reproduce
1. Replay fixture kinesis-video-signaling-ice-server-config-from-https-endpoint against the emulator (replay_wire_corpus); the divergence is at step task#4.
2. Equivalently, by hand: create a Kinesis Video signaling channel, resolve its endpoints, then POST /v1/get-ice-server-config with that channel's ChannelARN (and a ClientId) to the emulator's data listener.
3. Read IceServerList[0].Uris in the 200 response and compare the host:port each turn:/turns: URI names with the host:port the request was served on. They differ: the URIs name the endpoint authority carried from the earlier endpoint resolution (e.g. 127.0.0.1:39743) instead of the serving listener (e.g. 127.0.0.1:39175).
