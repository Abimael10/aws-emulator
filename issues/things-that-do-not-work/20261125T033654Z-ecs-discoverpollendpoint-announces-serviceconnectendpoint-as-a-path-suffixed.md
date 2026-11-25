Issue-Source: trace-sweep-divergence
Issue-Trace: ecs-agent-apis-and-exec-error-paths
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ecs-agent-apis-and-exec-error-paths
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs|DiscoverPollEndpoint|2xx->2xx|$.serviceConnectEndpoint
Issue-Commit: 7ce46e2ec7daea03f5b998d8aff6c247aac00dc8

# ecs DiscoverPollEndpoint announces serviceConnectEndpoint as a path-suffixed URL instead of a bare base endpoint

> _Note: this issue was identified against the code base at commit `7ce46e2ec7daea03f5b998d8aff6c247aac00dc8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DiscoverPollEndpoint

## Description
One net-new cause.

Cause 1 of 1: DiscoverPollEndpoint's `serviceConnectEndpoint` does not have the FORM the corpus records for that member. (fixture ecs-agent-apis-and-exec-error-paths, step task#2)

AWS: Smithy (services/service-com-amazonaws-ecs-amazonec2containerservicev20141113 model, DiscoverPollEndpoint -> DiscoverPollEndpointResponse) models `endpoint`, `telemetryEndpoint` and `serviceConnectEndpoint` as plain String members with no length/pattern constraint; the doc for `serviceConnectEndpoint` is "The endpoint for the Amazon ECS agent to poll for Service Connect configuration". The model therefore states nothing about the value's form, so this is recorded behavior only: the recording asserts the member's SHAPE rather than its literal value (the comparator reports the recorded side as '<shaped-not-asserted>', since the concrete host is environment-specific and never a requirement), and the member is announced as a BASE endpoint — a scheme-and-authority address with no plane path on it — that the agent then appends its own Service Connect poll path to. HTTP status for the step is 200 on both sides, and no error code or message is involved.

Emulator: the step answers 200 but the member's value carries an extra path segment, so it fails the recorded shape. Replay detail: `$.serviceConnectEndpoint: value_mismatch (expected '<shaped-not-asserted>', got 'https://127.0.0.1:44713/service-connect/')`. The emulator builds the value by appending a per-plane path segment to the address it is reached at: `announced_agent_endpoint(base, Some("service-connect"))` renders `{base}/service-connect/` (services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/state/agent_endpoints.rs lines 40-80), and the handler returns it verbatim (same crate, src/ops/discover_poll_endpoint.rs lines 266-272). Hypothesis: the surviving difference is that trailing `/service-connect/` plane path and not the host:port, because the host and port are exactly what the recorded side leaves unasserted, and the sibling member rendered with no plane path (`endpoint`) is not reported as diverging in the same step.

Why: the announced value is documented and consumed as the BASE address of the Service Connect configuration plane — the agent resolves its own request path against it. A base that already carries a `/service-connect/` path makes every URL the agent derives from it carry that segment too, so an agent (or any client reproducing the agent's behavior) that is correct against AWS dials a different address here than the discovery response intends, and any assertion on the shape of the discovered endpoint fails. It also makes the three announced members differ from AWS in form, not just in authority: on AWS each is a distinct host with a bare base path, here they are one authority distinguished by a path.

Fix: the ECS agent-endpoint rendering — `announced_agent_endpoint` / `agent_service_connect_endpoint` in the ecs service crate's state module (state/agent_endpoints.rs), consumed by the DiscoverPollEndpoint handler. The announced value must keep the emulator's own reachable authority (the address the deployment answers on, so no agent is pointed off this machine) while taking the base-endpoint form AWS announces, i.e. without a per-plane path segment baked into the member.

Traces: ecs-agent-apis-and-exec-error-paths#task#2 (DiscoverPollEndpoint, 200 vs 200, key $.serviceConnectEndpoint). No open issue covers this member; the nearest open ECS agent-plane issues (20261122T172353Z-ecs-the-agent-facing-submit-statechange-validation-boundary.md, 20261109T083521Z-ecs-ec2-instances-launched-with-ecs-cluster-user.md) are about Submit*StateChange validation and container-instance registration, not about the endpoints DiscoverPollEndpoint announces.

## Evidence
Replay of fixture ecs-agent-apis-and-exec-error-paths, step task#2 (service ecs, operation DiscoverPollEndpoint):
  expected status 200, actual status 200
  $.serviceConnectEndpoint: value_mismatch (expected '<shaped-not-asserted>', got 'https://127.0.0.1:44713/service-connect/')

Expected (AWS, recorded): serviceConnectEndpoint is a base endpoint — scheme + authority with no plane path appended; its host/port is environment-specific and is not asserted.
Actual (emulator): the reachable authority with a literal `/service-connect/` path appended, produced by `format!("{base}/{path}/")` in state/agent_endpoints.rs.
Fixed vs variable parts: the diverging element is the constant path segment `service-connect` the emulator inserts; the scheme/host/port portion of the value is per-environment and is NOT part of the requirement.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture `ecs-agent-apis-and-exec-error-paths`.
2. At step `task#2` the fixture calls ECS DiscoverPollEndpoint (AWS-JSON 1.1, target AmazonEC2ContainerServiceV20141113.DiscoverPollEndpoint) for a registered container instance in an existing ACTIVE cluster.
3. Inspect the 200 response body's `serviceConnectEndpoint`: the emulator returns `<emulator base>/service-connect/`, i.e. a URL with a path segment, where the recorded AWS response's member is a bare base endpoint with no plane path.
4. Compare with the same response's `endpoint`, which is rendered with no plane path and is not reported as diverging.
