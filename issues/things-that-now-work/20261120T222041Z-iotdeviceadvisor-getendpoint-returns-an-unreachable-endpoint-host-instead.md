Issue-Source: trace-sweep-divergence
Issue-Trace: iotdeviceadvisor-get-endpoint-returns-device-advisor-domain
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iotdeviceadvisor-get-endpoint-returns-device-advisor-domain
Issue-Shape-Id: com.amazonaws.iotdeviceadvisor#IotSenateService
Issue-Key: iotdeviceadvisor|GetEndpoint|2xx->2xx|$.endpoint
Issue-Commit: 35b9205adf94dc0bcf5e9943e949b302896dc332

# iotdeviceadvisor GetEndpoint returns an unreachable endpoint host instead of the emulator's own serving address

> _Note: this issue was identified against the code base at commit `35b9205adf94dc0bcf5e9943e949b302896dc332`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetEndpoint

## Description
Cause 1 of 1: GetEndpoint serializes a Device Advisor endpoint whose host is not an address the emulator serves on, so the value it hands back cannot be connected to.

AWS: GetEndpoint is `GET /endpoint` returning 200 with a single output member `endpoint` (Smithy: GetEndpoint.output.endpoint -> Endpoint, @length min 45 max 75; modeled errors InternalServerException 500, ResourceNotFoundException 404, ValidationException 400). The model constrains only the length; the recorded response shows the shape of the value AWS produces: a per-account Device Advisor domain of the form `<opaque-id>.deviceadvisor.iot.<region>.amazonaws.com` — a bare DNS host, no port, whose purpose is to be the host the caller's test device actually connects to. The opaque id and the region are values, not fixed text; the fixed part of the template is the `.deviceadvisor.iot.<region>.amazonaws.com` suffix and the fact that the host returned is one the service answers on.

Emulator: the step replays with the right status (200) and the right member, but `$.endpoint` carries a host the emulator does not serve: replay reports `$.endpoint: host_not_own` — the returned host/port is not the emulator's own data listener (the replay harness reached the emulator on its data listener port, while the endpoint value announced a different port on a `…us-west-2.localhost` host). So the emulator rewrote the AWS domain into a loopback address, but not into the loopback address it is actually reachable on.

Why: a client relying on AWS's behavior takes the value of `endpoint` and opens a connection to it — that is the only reason the operation exists. Against AWS that host resolves and answers; against the emulator the announced host:port is not bound, so every follow-on connection fails with a connection refusal or a DNS failure. The caller cannot tell this from a service outage, and any workflow that provisions a device against the returned endpoint dead-ends even though GetEndpoint itself reported success.

Fix: the GetEndpoint handler's response construction / the endpoint-address rewriting it uses. When the emulator substitutes its own address for the AWS Device Advisor domain it must emit the host and port of the listener the request arrived on (the same address the caller used), rather than a separately derived or hard-coded host:port. The rewritten value must also still satisfy the modeled @length min 45 max 75 on Endpoint.

Traces: iotdeviceadvisor-get-endpoint-returns-device-advisor-domain#task#1 (divergence key service=iotdeviceadvisor|GetEndpoint|status=200->200|keys=$.endpoint).

Not covered by the open iotdeviceadvisor issues: the GetSuiteRunReport 404-vs-400 issue and the suite-run lifecycle issue concern suite runs, not endpoint resolution.

## Evidence
Fixture iotdeviceadvisor-get-endpoint-returns-device-advisor-domain, step task#1, operation GetEndpoint.
Expected status 200, actual status 200 (status matches).
Body divergence on $.endpoint, verdict mismatch, reason host_not_own:
  expected: a Device Advisor domain of the form <opaque-id>.deviceadvisor.iot.us-west-2.amazonaws.com (recorded: 't39rb3yzr67tee.deviceadvisor.iot.us-west-2.amazonaws.com')
  actual:   a host the emulator does not serve on (recorded: '19987a65d0af35.deviceadvisor.iot.us-west-2.localhost:42303', while the emulator's data listener accepted on a different port)
The opaque id, region-qualified host and port are per-environment values; the requirement that is violated is that the announced host:port must be one the emulator itself answers on.
Smithy (com-amazonaws-iotdeviceadvisor-iotsenateservice): GetEndpoint http GET /endpoint code 200; output member endpoint -> Endpoint with @length min 45 max 75.

## Steps to Reproduce
1. Boot the emulator from this checkout and note the host:port its data listener accepts on.
2. Replay fixture iotdeviceadvisor-get-endpoint-returns-device-advisor-domain, step task#1: issue GET /endpoint against the iotdeviceadvisor service (optionally with thingArn/certificateArn/deviceRoleArn/authenticationMethod query parameters).
3. Observe 200 with an `endpoint` member.
4. Compare the host (and port, if any) in `endpoint` with the address from step 1, and try to open a TCP connection to it: the announced host:port is not the listener the request arrived on and the connection is not accepted.
