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
Issue-Commit: d41690d88a18f10b9fd412359aaaeaed2d9bc8e2

# kinesis-video-signaling: GetIceServerConfig announces TURN Uris at an authority the emulator is not reached on

> _Note: this issue was identified against the code base at commit `d41690d88a18f10b9fd412359aaaeaed2d9bc8e2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetIceServerConfig

## Description
One net-new wire cause, from the single diverging step of this service's wire corpus (5 steps, 4 passed, 1 mismatched, score 80.0%; emulator_up true, bootstrap_signal false). The replay report marked this step neither ALREADY FILED (stub_path empty) nor OWNED by an open issue (owned_by empty), so it is filed here.

Prior art, named because it does NOT own this step's replay key: open issue 20261109T211529Z-kinesis-video-signaling-geticeserverconfig-answers-404-resourcenotfoundexception-for.md binds the SAME fixture and step, but its divergence is the 2xx->4xx status/shape failure (Issue-Key `kinesis-video-signaling|GetIceServerConfig|2xx->4xx|status` / `|$.IceServerList`). That surface now passes: the operation answers HTTP 200 with a one-element IceServerList carrying Ttl 300, a Username and a Password. What remains, and what is filed here, is a different cause with a different divergence key (`service=kinesis-video-signaling|GetIceServerConfig|status=200->200|keys=$.IceServerList[0],$.IceServerList[0].Uris[0]`): the CONTENT of IceServer$Uris. Open issue 20261109T162245Z-kinesis-video-signaling-the-channel-s-wss-signaling.md covers the WSS signaling data plane and not this operation.

=== Cause 1 of 1: the issued relay authority is taken from the request's client-supplied authority instead of an address this emulator actually serves, so every TURN Uri names a host:port nothing answers on ===

AWS: Smithy (services/service-com-amazonaws-kinesisvideosignaling-awsacuitysignalingservice/smithy-model/com-amazonaws-kinesisvideosignaling-awsacuitysignalingservice.json): GetIceServerConfig is POST /v1/get-ice-server-config, success code 200; output is the single member IceServerList (list of IceServer{Uris, Username @length 1..256 @pattern ^[a-zA-Z0-9_.-]+$, Password same, Ttl @range 30..86400}). IceServer$Uris is documented as "an array of URIs, in the form specified in the I-D.petithuguenin-behave-turn-uris spec. These URIs provide the different addresses and/or protocols that can be used to reach the TURN server", and the operation's own doc says the ICE component uses this configuration "to setup the WebRTC connection, including authenticating with the Traversal Using Relays around NAT (TURN) relay server". So each Uri is not a label: it is an address a WebRTC client DIALS. The model states no host, and no recorded host is a requirement (AWS's own TURN hosts are per-region, per-account and per-run), so for the host itself: recorded behavior only — the recording shows the Uris naming an address that is reachable from the caller, resolved for the endpoint the call was made against. The replay harness therefore asserts the Uris' authority RELATIONALLY (expected `<shaped-not-asserted>`, with a host-ownership check) rather than literally.

Emulator: the step reaches the channel's HTTPS data-plane endpoint and answers HTTP 200, but the IceServerList's single IceServer carries Uris whose authority is a loopback literal with a port the emulator does not serve the call on. The harness's verdict on $.IceServerList[0].Uris[0] is `host_not_own`, and consequently the whole $.IceServerList[0] element is reported as not matching. The emulator's live data listener for this replay is a different port from the one announced in the Uris, so the announced relay is an address that resolves to nothing from the client that was just answered. Mechanically, src/ops/get_ice_server_config.rs computes the authority with `crate::state::relay_uris(ctx.parts(), ctx.endpoint().as_str())`, and state/relay_address.rs:43-53 `relay_authority` prefers `service_common::data_plane_addressing::reached_emulator_host_and_port(parts, served_base)`, which reads the request's own addressing (Host header, else absolute-form URI) before falling back to the served base. Hypothesis: that request-carried authority is whatever endpoint the client was told to address, so when it names a host:port this process is not actually bound on, the handler faithfully re-announces it; the check that it is "this emulator's own" only excludes AWS-looking hosts (`addresses_an_aws_endpoint`), not addresses the emulator does not serve.

Why: this operation is the service's ONLY source of ICE/TURN relay credentials, so the Uris are the entire payload a client acts on. A client gets a well-formed 200 with a Ttl, a Username and a Password, and then dials a relay that is not there: the ICE gathering phase produces no relay candidate, TURN fallback is silently unavailable, and a viewer that cannot establish a direct peer-to-peer connection fails to connect at all. Because the answer is a 200 with a credential set and a lifetime, the client has no signal to retry or to re-resolve the endpoint: it believes a relay is serving it for the reported Ttl. It is also the failure mode the module header of state/relay_address.rs names as the one to avoid — announcing an address other than the one the emulator is reached on sends the client out of the environment the call was served from.

Fix: the relay-address resolution in this service's state, state/relay_address.rs (`relay_authority` / `relay_uris`), and its use by the GetIceServerConfig handler (src/ops/get_ice_server_config.rs, where `relay_uris` is called and where `IceServerRecord::readdress_credential` re-addresses a held issuance). The authority must be one this process is actually SERVING the call on — the emulator's own listener authority (`service_common::data_plane_addressing::listener_authority`, the same seam state/wss_endpoint.rs uses for the announced `wss://` address) or the served base from `ctx.endpoint()` — rather than any authority the request merely carried. A request-carried authority should be adopted only when it matches an address this emulator answers on; otherwise the served/configured address must be used. The URI grammar itself is correct and must be kept: `scheme ":" host [":" port] ["?transport=" transport]` with no `//` marker, one Uri per offered transport (turn/udp, turns/udp, turns/tcp). Status, Ttl, Username and Password are NOT part of this cause: Ttl 300 and the shaped Username/Password already match, so the fix must not change them.

Traces: kinesis-video-signaling-ice-server-config-from-https-endpoint#task#4

## Evidence
Fixture kinesis-video-signaling-ice-server-config-from-https-endpoint, step task#4 (GetIceServerConfig), verdict mismatch. Divergence key: service=kinesis-video-signaling|GetIceServerConfig|status=200->200|keys=$.IceServerList[0],$.IceServerList[0].Uris[0]. affected_count 1 (affected: kinesis-video-signaling-ice-server-config-from-https-endpoint#task#4). stub_path empty, owned_by empty.

Status is NOT the divergence: expected_status 200, actual_status 200.

Replay detail, verbatim:
  "$.IceServerList[0]: missing (expected {'Password': '<shaped-not-asserted>', 'Ttl': 300, 'Uris': ['<shaped-not-asserted>'], 'Username': '<shaped-not-asserted>'}, got {'Uris': ['turn:127.0.0.1:43577?transport=udp', 'turns:127.0.0.1:43577?transport=udp', 'turns:127.0.0.1:43577?transport=tcp'], 'Username': '1794841032:Vi2e0oAg8PCIzdYY', 'Password': 'lwKvVHAchHagzrASKZbMl9GRFDjdPMNv', 'Ttl': 300})
   $.IceServerList[0].Uris[0]: host_not_own (expected '<shaped-not-asserted>', got 'turn:127.0.0.1:43577?transport=udp')"

Reading of that detail, separating the requirement from the per-run values:
- REQUIREMENT (fixed): the response is HTTP 200 with IceServerList holding one IceServer; each Uris entry is a dialable TURN/TURNS URI in the form `scheme ":" authority "?transport=" transport`; that authority is an address this emulator is reached/served on. Ttl is 300 and is asserted.
- PER-RUN, NOT a requirement: the Username token (`1794841032:Vi2e0oAg8PCIzdYY` — a unix expiry plus an opaque token), the Password (`lwKvVHAchHagzrASKZbMl9GRFDjdPMNv`), and any concrete host or port. These are `<shaped-not-asserted>` in the fixture and already satisfy their shape, so the literal values above must not be treated as expected output.
- THE DEFECT: the authority `127.0.0.1:43577` carried by all three Uris is flagged `host_not_own` — it is not an address this emulator answers the call on. The emulator's live data listener for this replay is reported by the harness as `/__admin/health answered 200 and the :35713 data listener accepts`, a different port from the 43577 announced in the Uris. The $.IceServerList[0] "missing" line is the same single cause surfacing at the element level (the element is reported as not matching because its Uris do not), not a second divergence: Username, Password and Ttl are all present and shaped correctly.

Corpus summary: trace_set_id simparity-wire-aws-v1; steps_total 5, steps_passed 4, steps_mismatched 1, steps_errored 0, steps_blocked 0, not_implemented 0, score_pct 80.0, divergence_count 1, emulator_up true, bootstrap_signal false.

Smithy used: GetIceServerConfig {http POST /v1/get-ice-server-config, code 200}; GetIceServerConfigResponse member IceServerList; IceServer members Uris / Username / Password / Ttl (@range 30..86400); IceServer$Uris doc citing the I-D.petithuguenin-behave-turn-uris URI spec.

Code read while grounding the cause: services/service-com-amazonaws-kinesisvideosignaling-awsacuitysignalingservice/src/ops/get_ice_server_config.rs:382 (`let relay_uris = crate::state::relay_uris(ctx.parts(), ctx.endpoint().as_str());`) and :415-418 (`channel.readdress_credential(&relay_uris)`), and src/state/relay_address.rs:43-60 (`relay_authority` preferring `reached_emulator_host_and_port(parts, served_base)` over the served/configured address) with :85-96 (`configured_authority`, loopback plus `data_port_from_env`).

## Steps to Reproduce
1. Boot the emulator and replay fixture kinesis-video-signaling-ice-server-config-from-https-endpoint. Steps task#1-task#3 pass: kinesisvideo CreateSignalingChannel (ChannelType SINGLE_MASTER), DescribeSignalingChannel until ChannelInfo.ChannelStatus is ACTIVE, and GetSignalingChannelEndpoint with SingleMasterChannelEndpointConfiguration={Protocols:[HTTPS], Role:VIEWER}, which yields the channel ARN and an HTTPS data-plane endpoint.
2. Step task#4: POST /v1/get-ice-server-config against that announced HTTPS endpoint with {"ChannelARN": <the ARN from step 1>} (optionally ClientId matching ^[a-zA-Z0-9_.-]+$, and Service omitted or "TURN").
3. Observe HTTP 200 with IceServerList = one IceServer whose Ttl is 300 and whose Username/Password are well-shaped — those parts are correct.
4. Inspect IceServerList[0].Uris. Every entry names the same authority, a loopback literal with a port (e.g. `turn:127.0.0.1:43577?transport=udp`). Compare that authority with the address the emulator is actually serving this call on (the harness reports the accepting data listener, e.g. :35713, and /__admin/health answers there). They differ, which is the `host_not_own` verdict.
5. Confirm the consequence from the client's side: attempt to open a TCP/UDP connection to the authority the Uris name. Nothing is bound there, so the TURN relay a WebRTC client would dial for its relay candidate is unreachable, while the 200 response advertises it as valid for Ttl seconds.
6. Expected: every Uris entry names an authority this emulator answers on — the listener/served address for the call (the same seam the channel's own HTTPS and WSS endpoints are announced at) — still in the documented TURN URI form with no `//` authority marker, one Uri per offered transport, and with Ttl/Username/Password unchanged.
7. Re-replay this fixture and step after changing the relay-authority resolution to verify the step passes.
