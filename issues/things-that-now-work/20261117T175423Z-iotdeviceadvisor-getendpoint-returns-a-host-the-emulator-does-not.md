Issue-Source: trace-sweep-divergence
Issue-Trace: iotdeviceadvisor-get-endpoint-returns-device-advisor-domain
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iotdeviceadvisor-get-endpoint-returns-device-advisor-domain
Issue-Shape-Id: com.amazonaws.iotdeviceadvisor#IotSenateService
Issue-Key: iotdeviceadvisor|GetEndpoint|2xx->2xx|$.endpoint
Issue-Commit: c19026fddc546a37c1d4d3954f77370ec86334b1

# GetEndpoint returns a host the emulator does not serve

> _Note: this issue was identified against the code base at commit `c19026fddc546a37c1d4d3954f77370ec86334b1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetEndpoint

## Description
Cause 1 of 1: GetEndpoint answers 200 but the `endpoint` host it advertises is not an address the emulator is reachable on, so a client that follows it leaves the emulator.

AWS: Smithy (com.amazonaws.iotdeviceadvisor#GetEndpoint): GET /endpoint, success code 200, output GetEndpointResponse with a single member `endpoint` targeting `Endpoint` with @length min 45 / max 75; errors InternalServerException (500), ResourceNotFoundException (404), ValidationException (400). The model constrains only the length of the string, not its host; the recorded response shows AWS returning the fully-qualified Device Advisor test-endpoint host for the caller's account and region, in the shape `<endpoint-id>.deviceadvisor.iot.<region>.amazonaws.com`. The `<endpoint-id>` label is an account/endpoint-specific generated value and is NOT part of the requirement — recorded behavior only for the host suffix. The contract a client depends on is that the returned host is the host that actually serves the Device Advisor data plane for the endpoint the caller is talking to: the value is handed straight to a device/SDK as a connection target.

Emulator: the emulator synthesises a real-AWS-style public hostname instead of its own advertised address. Replay classified the mismatch as `host_not_own` on `$.endpoint`: expected a Device Advisor host for the endpoint under test, got `<generated-id>.deviceadvisor.iot.us-west-2.amazonaws.com` — a host the emulator neither serves nor resolves to. Status is 200 and the response shape is otherwise correct; only the announced host is wrong.

Why: a client that does what the API exists for — take `endpoint` and connect to it — is pointed off the emulator at a public amazonaws.com name. In a sealed test environment that is a DNS/connect failure; where egress exists it is worse, because the device traffic is directed at real AWS instead of the emulator, and the test suite the caller then drives never reaches the emulated state. The operation is unusable as a discovery call even though it returns 200, and nothing in the response signals the problem.

Fix: the GetEndpoint handler's response construction for this service — specifically the endpoint-host derivation. It must emit a host rooted in the address the request arrived on (the emulator's own data listener / configured public address) rather than a hardcoded or fabricated `*.amazonaws.com` name, while still satisfying the modeled `Endpoint` @length 45..75 constraint (e.g. keep the `<endpoint-id>.deviceadvisor.iot.<region>.` prefix structure and root it at the emulator's own domain). The per-request generated endpoint id itself is fine as a generated value; it is the domain the emulator claims that diverges.

Traces: iotdeviceadvisor-get-endpoint-returns-device-advisor-domain#task#1 (cluster of 1 step).

## Evidence
Fixture iotdeviceadvisor-get-endpoint-returns-device-advisor-domain, step task#1, operation GetEndpoint.

Expected (recorded AWS): HTTP 200, body `{"endpoint": "<endpoint-id>.deviceadvisor.iot.us-west-2.amazonaws.com"}` — recorded literal `t39rb3yzr67tee.deviceadvisor.iot.us-west-2.amazonaws.com`; the `t39rb3yzr67tee` label is a per-account generated value, not a requirement.

Actual (emulator): HTTP 200, body `{"endpoint": "d2hlnnleu8vtjl.deviceadvisor.iot.us-west-2.amazonaws.com"}`.

Replay verdict: mismatch, divergence_key `service=iotdeviceadvisor|GetEndpoint|status=200->200|keys=$.endpoint`, detail `$.endpoint: host_not_own` — the emulator announced a host other than the one it is reached on (:42899 data listener). Status matches (200->200); the defect is the announced host, not the status, error code or shape.

## Steps to Reproduce
1. Boot the emulator from this checkout (data listener on its configured port; /__admin/health answers 200).
2. Replay fixture `iotdeviceadvisor-get-endpoint-returns-device-advisor-domain`, step `task#1`: GET /endpoint against the iotdeviceadvisor service (GetEndpoint, no required query parameters).
3. Read `$.endpoint` from the 200 response body.
4. Observe it is a `*.deviceadvisor.iot.<region>.amazonaws.com` host that the emulator does not serve, instead of a host rooted at the address the request was made to; attempting to connect to the returned host does not reach the emulator.
