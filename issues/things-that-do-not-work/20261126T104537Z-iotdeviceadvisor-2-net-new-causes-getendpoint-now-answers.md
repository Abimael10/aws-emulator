Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iotdeviceadvisor.get-endpoint-returns-valid-device-advisor-domain iotdeviceadvisor.mqtt-connect-native-protocol-round-trip
Issue-Shape-Id: com.amazonaws.iotdeviceadvisor#IotSenateService
Issue-Key: iotdeviceadvisor.get-endpoint-returns-valid-device-advisor-domain|run#4|AssertionError|assert 45 <= len(endpoint) <= 75, f"expected endpoint length in [45, 75], got {len(endpoint)}: {endpoint!r}"
Issue-Key: iotdeviceadvisor.mqtt-connect-native-protocol-round-trip|_poll_until#47|Timeout|return last
Issue-Commit: 56ff6e7891d5b9b4ec2836e222d654b8f1faa906

# iotdeviceadvisor: 2 net-new causes — GetEndpoint now answers a bare '127.0.0.1:<port>' that violates Endpoint's @length 45..75 and carries no deviceadvisor/region form, and the native MQTT/TLS round trip against a RUNNING suite run reaches no verdict

> _Note: this issue was identified against the code base at commit `56ff6e7891d5b9b4ec2836e222d654b8f1faa906`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetEndpoint

## Description
Two net-new root causes in the iotdeviceadvisor emulator, both grounded in failing behavioral checks of the simparity-behavioral-aws-v1 suite. They are bundled because they share one service; a fixer must close BOTH before submitting.

Cause 1 of 2: GetEndpoint serializes `endpoint` as a bare socket address (`<ip>:<port>`), which is shorter than the modeled minimum for the Endpoint shape and carries none of the Device Advisor host form. Failing check: iotdeviceadvisor.get-endpoint-returns-valid-device-advisor-domain.
AWS: GetEndpoint is `GET /endpoint`, 200, with a single output member `endpoint` (Smithy, com-amazonaws-iotdeviceadvisor-iotsenateservice: GetEndpointResponse.endpoint -> Endpoint, @length min 45 max 75; modeled errors ValidationException 400, ResourceNotFoundException 404, InternalServerException 500). The model constrains the length only; for the value's FORM this is recorded behavior only: AWS returns a bare DNS host of the shape `<opaque-id>.deviceadvisor.iot.<region>.amazonaws.com` — a hostname with no port component, whose fixed text is the `.deviceadvisor.iot.<region>.` infix plus the account-independent suffix, and whose variable parts are the opaque per-account id and the caller's region. Any conforming value therefore (a) is 45..75 characters long, (b) contains the region the call was made in, and (c) identifies the Device Advisor / device-gateway plane in its name rather than being an anonymous socket address.
Emulator: the call answers 200, but `endpoint` is a 15-character raw socket literal. Replay output: `AssertionError: expected endpoint length in [45, 75], got 15: '127.0.0.1:34275'`. So the modeled @length min 45 on Endpoint is violated outright, the region is absent from the value, and the value is a host:port pair rather than a host. (The concrete ip and port are per-environment values and are not themselves the requirement — the requirements violated are the modeled length bound, the absence of any `:port` component, and the region-qualified Device-Advisor host form.) Note this is the same member as open issue 20261120T222041Z-iotdeviceadvisor-getendpoint-returns-an-unreachable-endpoint-host-instead.md, which reports the opposite failure mode (a `…deviceadvisor.iot.<region>.localhost:<port>` host the emulator did not serve); that issue does not cover the value now served, which has lost the domain form and the modeled length entirely, so this cause is net-new relative to it.
Why: a client takes `endpoint` verbatim as the MQTT broker host for its device under test and dials it on 8883 — that is the operation's only purpose. A value carrying an embedded port is not a host: a device SDK either treats the whole string (colon and digits included) as a hostname and fails to resolve it, or dials a port the caller never asked for. Strict SDKs and validating clients additionally reject the response for breaching the modeled Endpoint length, so the call fails client-side even though the service reported success. Region-derived routing or certificate hostname checks built on the returned domain also break.
Fix: the GetEndpoint handler's response construction and the endpoint-address rewriting it shares with StartSuiteRun. The emulator must announce a bare host (no `:port`) that is region-qualified, carries the Device Advisor host form, satisfies Endpoint's @length min 45 max 75, AND resolves to the listener that actually accepts the device's MQTT/TLS connections.
Traces: iotdeviceadvisor.get-endpoint-returns-valid-device-advisor-domain (group control-plane).

Cause 2 of 2: a suite run started with `parallelRun: true` never yields an observable native MQTT/TLS round trip — the check reaches no verdict inside its budget. Failing check: iotdeviceadvisor.mqtt-connect-native-protocol-round-trip.
AWS: StartSuiteRun is `POST /suiteDefinitions/{suiteDefinitionId}/suiteRuns`, 200, output suiteRunId/suiteRunArn/createdAt/endpoint (Endpoint @length 45..75); GetSuiteRun is `GET /suiteDefinitions/{suiteDefinitionId}/suiteRuns/{suiteRunId}`, 200, with `status` (SuiteRunStatus) and `testResult`. For the data plane this is recorded behavior only: AWS brings the run to `RUNNING`, accepts a real MQTT/TLS CONNECT made with the device's own IoT certificate at the host returned in `endpoint` on port 8883 while the run is RUNNING, then settles the run at `status` = `PASS` with `testResult.groups[0].tests[0].status` = `PASS` for the declared `MQTT_Connect` test case.
Emulator: the check never completes — replay output: `Error: Check timed out after 60.0s`. The run's lifecycle plus data plane therefore produce no terminal verdict: no MQTT_Connect PASS is ever observable. Hypothesis: either the suite run never transitions to `RUNNING` (the check polls GetSuiteRun for `RUNNING` before connecting), or no listener accepts an MQTT/TLS CONNECT at the announced `endpoint`, so the connect retry loop spins until the budget expires; the replay output does not say which of the two blocks first, and both must be served for this flow to reach a verdict. The previously filed open issue 20261121T044054Z-iotdeviceadvisor-startsuiterun-returns-a-port-qualified-non-served.md covers the same check but at a different failure point (StartSuiteRun's `endpoint` was then unparseable as a host and the harness raised ValueError before any connect); the value is now parseable and the check instead stalls, so the stall — the run never reaching a terminal PASS and no MQTT/TLS CONNECT being accepted — is net-new behavior that issue does not describe.
Why: Device Advisor exists to drive a real device against a real broker. A client that creates a thing, certificate, policy and device-permission role, starts a parallel suite run and then connects its device gets a 200 from every control-plane call but can never observe its device connect and can never read a test result: the run neither passes nor fails. A qualification pipeline blocks forever instead of returning a verdict, and the caller cannot distinguish this from a broker outage.
Fix: the StartSuiteRun / GetSuiteRun handlers' suite-run lifecycle (a run must reach `RUNNING` and then settle to a terminal `status` with `testResult.groups[].tests[].status` populated for each declared test case) and the suite-run data plane that must accept MQTT/TLS CONNECT with the device's certificate at the address announced in `endpoint` and record that CONNECT as the MQTT_Connect case's PASS.
Traces: iotdeviceadvisor.mqtt-connect-native-protocol-round-trip (group native-protocol).

## Evidence
Behavioral suite simparity-behavioral-aws-v1, service heading `### iotdeviceadvisor` (19/21 passed, 2 failed).

Cause 1 — [mismatch] iotdeviceadvisor.get-endpoint-returns-valid-device-advisor-domain:
  File check_iotdeviceadvisor_get_endpoint_returns_valid_device_advisor_domain.py line 21, after `resp = da.get_endpoint(); endpoint = resp['endpoint']`.
  Raised: `AssertionError: expected endpoint length in [45, 75], got 15: '127.0.0.1:34275'`.
  Expected: a bare, region-qualified Device Advisor host (`<opaque-id>.deviceadvisor.iot.<region>.amazonaws.com` form), 45..75 chars, no port component; the check further asserts the region is embedded and that 'deviceadvisor' or 'devicegateway' appears in the value.
  Actual: `'127.0.0.1:34275'` — 15 chars, host:port form, no region, no Device Advisor identity.
  Status matched (200); the divergence is the body value. The ip and port are per-environment values, not the requirement.
  Smithy (com-amazonaws-iotdeviceadvisor-iotsenateservice): GetEndpoint http GET /endpoint code 200; output member endpoint -> Endpoint @length min 45 max 75.

Cause 2 — [mismatch] iotdeviceadvisor.mqtt-connect-native-protocol-round-trip:
  Raised: `Error: Check timed out after 60.0s` (no assertion reached; paho-mqtt was importable, so the check's own missing-dependency AssertionError did not fire).
  The check: creates an IoT thing + keys/certificate + policy, an IAM role trusted by iotdeviceadvisor.amazonaws.com, CreateSuiteDefinition with a rootGroup declaring test {id: MQTT_Connect, version: 0.0.0}, StartSuiteRun with suiteRunConfiguration={'primaryDevice': {...}, 'parallelRun': True}, polls GetSuiteRun for status RUNNING (90 s), then performs an MQTT/TLS CONNECT with the device certificate against the returned `endpoint` on port 8883 (150 s retry), then polls GetSuiteRun for a terminal status and asserts status == 'PASS' and testResult.groups[0].tests[0].status == 'PASS'.
  Expected: the run reaches RUNNING, the CONNECT is accepted, the run settles PASS with the MQTT_Connect case PASS.
  Actual: no terminal verdict within the replay budget — neither the RUNNING→PASS lifecycle nor an accepted MQTT/TLS CONNECT is observable.
  Smithy: StartSuiteRun POST /suiteDefinitions/{suiteDefinitionId}/suiteRuns code 200, output endpoint -> Endpoint @length 45..75; GetSuiteRun GET /suiteDefinitions/{suiteDefinitionId}/suiteRuns/{suiteRunId} code 200, output status -> SuiteRunStatus, testResult -> TestResult.

## Steps to Reproduce
Cause 1 (GetEndpoint value):
1. Boot the emulator from this checkout.
2. Call iotdeviceadvisor GetEndpoint (`GET /endpoint`, optionally with thingArn/certificateArn/deviceRoleArn/authenticationMethod query parameters).
3. Observe 200 and read `endpoint`: it is a bare `<ip>:<port>` socket literal (e.g. 15 characters), shorter than Endpoint's modeled @length min 45, with no region and no 'deviceadvisor'/'devicegateway' label.
4. Re-replay behavioral check iotdeviceadvisor.get-endpoint-returns-valid-device-advisor-domain to confirm a fix.

Cause 2 (native MQTT round trip):
1. Boot the emulator and note the address its iotdeviceadvisor suite-run data plane (MQTT/TLS) accepts on.
2. CreateThing, CreateKeysAndCertificate(setAsActive=true), CreatePolicy with iot:Connect/Publish/Subscribe/Receive, AttachThingPrincipal and AttachPolicy on the certificate; create an IAM role trusted by iotdeviceadvisor.amazonaws.com.
3. CreateSuiteDefinition with a rootGroup declaring a group whose test is {"id": "MQTT_Connect", "version": "0.0.0"}, protocol MqttV3_1_1, devices=[{thingArn, certificateArn}], devicePermissionRoleArn=<role>.
4. StartSuiteRun with suiteRunConfiguration={"primaryDevice": {...}, "parallelRun": true}; note the returned `endpoint`.
5. Poll GetSuiteRun: observe whether `status` ever becomes RUNNING.
6. Open an MQTT/TLS CONNECT to the announced endpoint on 8883 using the device's certificate and private key: the connect is not accepted.
7. Poll GetSuiteRun again: `status` never reaches a terminal PASS and `testResult` never reports the MQTT_Connect case, so the flow never reaches a verdict.
8. Re-replay behavioral check iotdeviceadvisor.mqtt-connect-native-protocol-round-trip to confirm a fix.
