Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__logs.ecs-service-connect-client-task-reaches-the-server-service-by-its-discovery-name
Issue-Key: ecs__logs.ecs-service-connect-client-task-reaches-the-server-service-by-its-discovery-name|_poll_until#21|Timeout|time.sleep(interval_s)
Issue-Commit: dff6e291e5e6489707888b0049fa49b367909ca3

# ecs__logs: ECS Service Connect delivers no traffic — a client task never reaches the server service by its discoveryName

> _Note: this cause was identified against the code base at commit `dff6e291e5e6489707888b0049fa49b367909ca3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

> _Provenance: this was cause 1 of 2 of `20261111T010150Z-ecs-logs-2-net-new-causes-service-connect-discovery-never.md`. Cause 2 of that issue (a container health probe that nothing consumed, so an unhealthy task was never replaced and the deployment circuit breaker never failed or rolled back the rollout) is fixed and that report now sits under `issues/things-that-now-work/`; this cause is unfixed and is re-filed here so it is not recorded as working. Its `Issue-Key` moved with it. `20261115T045707Z-ecs-logs-a-service-connect-client-task-never.md` is the same check filed separately and carries its own replay key._

## Operation
CreateService

## Description
AWS: recorded behavior only for the data path (ECS's model exposes CreateService/UpdateService `serviceConnectConfiguration` with `services[].portName`, `services[].discoveryName` and `services[].clientAliases[].{port,dnsName}`). The recording shows the client task's HTTP request to the discoveryName returning 200 with a body carrying the server's per-run marker, and the server container's own start line present in its awslogs stream — i.e. the response travelled the ECS-injected Service Connect mesh.

Emulator: the check never reaches a graded state — 'Error: Check timed out after 60.0s'. Neither the server's log line nor a response through the discovery name is ever observable, so Service Connect's injected proxy/DNS resolution is not emulated. Nothing is refused: the control-plane calls all answer 200 and the data path simply never materialises.

Why: a client whose microservices address each other only by Service Connect discovery names has no working service-to-service path at all; nothing in a mesh-based deployment can be exercised, and the failure is a silent hang rather than a refusal.

The ONE blocker, for whoever picks this up: the 200 and the marker have to come from the SERVER container's own image program, and the start line in its awslogs stream is that program's own output. This emulator runs a container's argument vector in a confined, in-process session with no process to start, no network to reach and no filesystem (`services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/state/container_shell.rs`, module header), and the tree already pins that for exactly this workload shape: the row "a program that writes and then needs the image's own server" in `services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/tests/all/container_program_not_performed.rs` asserts that `sh -c "echo marker > /usr/share/nginx/html/index.html && nginx -g 'daemon off;'"` runs nothing and claims nothing. `vendor/engine/src/lib.rs` states the platform position ("Containers and external processes are not available to a generated emulator yet"). So answering the client's GET with a 200 and the server's marker without running that image would be a response synthesised to look computed, and running it needs a container runtime this platform does not provide — the honest exit for this cause, until a container runtime exists, is `mark_wont_fix(platform_blocker)` naming `src/state/container_shell.rs`.

What IS implementable in this crate today, and is worth doing on its own even though it does not make the check pass: the REGISTRATION half of Service Connect. A service created or updated with an enabled `serviceConnectConfiguration` should register each `services[].discoveryName` as a Cloud Map service in the namespace (the Cloud Map store in the shared engine, the way `src/state/service_discovery.rs` already registers a `serviceRegistries` entry's instances), register one instance per RUNNING task of the service, and report the mapping as `Deployment$serviceConnectResources` ("Each list entry maps a discovery name to a Cloud Map service name"), which `src/state/store.rs` currently hardcodes to an empty list for both the primary and the superseded deployment projections. That makes the namespace's members real to every Cloud Map read and to `ListServicesByNamespace`; the traffic path stays blocked by the paragraph above.

Traces: ecs__logs.ecs-service-connect-client-task-reaches-the-server-service-by-its-discovery-name

## Evidence
Expected (AWS): the client task's HTTP GET to the server's Service Connect discoveryName returns 200 whose body contains the server's per-run marker, and the server's start line is in its awslogs stream. Actual (emulator): 'Error: Check timed out after 60.0s' — nothing observable. The 60 s is the check's own budget, not a requirement; the defect is that reachability never materialises.

## Steps to Reproduce
1. CreateCluster with a `serviceConnectDefaults` namespace, and a log group for the containers' `awslogs` driver.
2. RegisterTaskDefinition for a server whose container declares a named port mapping, and CreateService for it with `serviceConnectConfiguration` `{enabled: true, services: [{portName, discoveryName}]}`.
3. CreateService for a client in the same namespace whose container requests `http://<discoveryName>:<port>/`.
4. Read the client's awslogs stream for the body marker, and the server's for its start line: neither appears, and the check burns its whole budget.
