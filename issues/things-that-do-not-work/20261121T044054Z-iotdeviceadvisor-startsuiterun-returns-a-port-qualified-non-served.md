Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iotdeviceadvisor.mqtt-connect-native-protocol-round-trip
Issue-Shape-Id: com.amazonaws.iotdeviceadvisor#IotSenateService
Issue-Key: iotdeviceadvisor.mqtt-connect-native-protocol-round-trip|run#43|ValueError|host, port = ctx.endpoint_host(endpoint, 8883)
Issue-Commit: 9a3ba8c99fe3276f826d008a22274dd6f71a6b78

# iotdeviceadvisor StartSuiteRun returns a port-qualified, non-served endpoint instead of a bare Device Advisor test host

> _Note: this issue was identified against the code base at commit `9a3ba8c99fe3276f826d008a22274dd6f71a6b78`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartSuiteRun

## Description
Cause 1 of 1: StartSuiteRun serializes its `endpoint` output member as a host with an embedded TCP port (`<host>:<port>`), and the announced address is not one the emulator accepts MQTT/TLS connections on. Failing check: iotdeviceadvisor.mqtt-connect-native-protocol-round-trip.

AWS: StartSuiteRun is `POST /suiteDefinitions/{suiteDefinitionId}/suiteRuns`, 200, output members suiteRunId, suiteRunArn, createdAt, endpoint (Smithy: StartSuiteRunResponse.endpoint -> Endpoint, @length min 45 max 75; doc 'The response of an Device Advisor test endpoint.'; modeled errors ConflictException 400, ValidationException 400, InternalServerException 500). The model constrains only length, so for the value's form this is recorded behavior only: AWS returns a BARE DNS host of the form `<opaque-id>.deviceadvisor.iot.<region>.amazonaws.com` — no scheme, no path and no `:port` component — which the device under test then connects to over MQTT/TLS on the standard port 8883. The opaque id and the region are per-account/per-run values; the fixed part of the template is the `.deviceadvisor.iot.<region>.amazonaws.com` suffix, the absence of any port component, and the fact that the host returned is one the service actually answers on while the run is RUNNING.

Emulator: the run is created and StartSuiteRun answers 200, but `endpoint` carries a host:port pair rather than a host. Replay output: `ValueError: a published database host is not a bare host: '915c7513f4c78d.deviceadvisor.iot.us-west-2.localhost:33177'` — the harness could not even resolve the value into (host, port) with the default MQTT port 8883, so the MQTT/TLS CONNECT the check exists to make was never attempted. The port embedded in the value is also not the address the emulator serves the suite run's data plane on, which is the same class of defect already filed for GetEndpoint in open issue 20261120T222041Z-iotdeviceadvisor-getendpoint-returns-an-unreachable-endpoint-host-instead.md (`$.endpoint: host_not_own`); that issue covers GetEndpoint only and does not cover StartSuiteRun's response, which is a separate handler and a separate output shape.

Why: the only use of `endpoint` is to be fed to a connection — a device SDK or client takes the string verbatim as the MQTT broker host and dials it on 8883. Against AWS that is a bare resolvable host. Against the emulator the string contains a colon and a port, so a client either fails to resolve it (the port is parsed as part of the hostname) or, if it does split it, dials a port nothing is bound to. Either way a parallelRun Device Advisor workflow dead-ends after a 200 from StartSuiteRun, the MQTT_Connect test case can never be driven, and the caller cannot distinguish this from a broker outage.

Fix: the StartSuiteRun handler's response construction and the endpoint-address rewriting it shares with GetEndpoint. When the emulator substitutes its own address for the AWS Device Advisor test domain it must emit a bare host (no `:port`) that resolves to, and is served by, the listener accepting the suite run's MQTT/TLS connections, and must still satisfy the modeled @length min 45 max 75 on Endpoint.

Traces: behavioral check iotdeviceadvisor.mqtt-connect-native-protocol-round-trip (group native-protocol), which reads start_suite_run(...)['endpoint'] after suiteRunConfiguration.parallelRun=true and then calls ctx.endpoint_host(endpoint, 8883).

## Evidence
Behavioral check iotdeviceadvisor.mqtt-connect-native-protocol-round-trip, verdict [error].
Failure site: check_iotdeviceadvisor_mqtt_connect_native_protocol_round_trip.py line 176, `host, port = ctx.endpoint_host(endpoint, 8883)` where `endpoint = start_resp["endpoint"]` from StartSuiteRun with suiteRunConfiguration={'primaryDevice': ..., 'parallelRun': True}.
Raised: `ValueError: a published database host is not a bare host: '915c7513f4c78d.deviceadvisor.iot.us-west-2.localhost:33177'`
Expected: `endpoint` is a bare host, e.g. `<opaque-id>.deviceadvisor.iot.<region>.amazonaws.com`, with no port component, reachable for MQTT/TLS on 8883 while the run is RUNNING.
Actual: `endpoint` is `<opaque-id>.deviceadvisor.iot.<region>.localhost:<port>` — a port-qualified host, and the port announced is not the emulator's own MQTT/TLS listener.
The opaque id (`915c7513f4c78d`), the region and the port (`33177`) are per-run/per-environment values and are not themselves the requirement; the violated requirements are (a) no port component in the value and (b) the announced host must be one the emulator answers on.
Smithy (com-amazonaws-iotdeviceadvisor-iotsenateservice): StartSuiteRun http POST /suiteDefinitions/{suiteDefinitionId}/suiteRuns code 200; output member endpoint -> Endpoint @length min 45 max 75.
Status code itself matched (200); the divergence is in the body value.

## Steps to Reproduce
1. Boot the emulator from this checkout and note the address its iotdeviceadvisor data plane (MQTT/TLS) listener accepts on.
2. Create an IoT thing, certificate and policy, and an IAM device-permission role; call CreateSuiteDefinition with a rootGroup declaring the MQTT_Connect test case.
3. Call StartSuiteRun with suiteDefinitionId=<id> and suiteRunConfiguration={'primaryDevice': {...}, 'parallelRun': true}.
4. Observe 200 and read the `endpoint` member: it contains a `:<port>` suffix (e.g. `...deviceadvisor.iot.us-west-2.localhost:33177`) rather than a bare DNS host.
5. Feed the value verbatim to an MQTT client as the broker host with port 8883 (what a device SDK does): the value cannot be used as a host, and the embedded port is not bound by the emulator.
