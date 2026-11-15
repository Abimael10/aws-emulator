Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__logs.ecs-service-connect-client-task-reaches-the-server-service-by-its-discovery-name
Issue-Key: ecs__logs.ecs-service-connect-client-task-reaches-the-server-service-by-its-discovery-name|_poll_until#13|Timeout|last = probe()
Issue-Commit: da3e7884167b3f933d6f1406ab01004ac62fd391

# ecs__logs: a Service Connect client task never reaches the server service by its discovery name — the check exhausts its budget with no reachability ever observed

> _Note: this issue was identified against the code base at commit `da3e7884167b3f933d6f1406ab01004ac62fd391`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateService

## Description
One net-new root cause with one grounded check (Cause 1 of 1).

Cause 1 of 1: an ECS Service Connect-enabled client task never resolves or reaches the server service by its configured discovery name, so the check is cut off by its budget ("Check timed out after 60.0s") without the reachability condition ever being observed.
AWS: Smithy (ecs) CreateService/UpdateService take serviceConnectConfiguration {enabled, namespace, services[] {portName, discoveryName, clientAliases[] {port, dnsName}}}, and RunTask/CreateService tasks in an enabled namespace get the Service Connect proxy injected. On AWS, a client task in the same namespace resolves the server service's discoveryName (or client alias) over the namespace's DNS and gets an HTTP response from the server container, and the proxy's traffic is observable in the task's awslogs stream. Status/error codes: these are 200-level control-plane calls; the divergence is that the data-plane reachability the configuration promises never materialises.
Emulator: recorded behavior only — the check produces no comparable response; it polls for the client task's successful request to the discovery name and exhausts its whole budget without it, so Service Connect's discovery/proxy path is never satisfied.
Why: a client that adopts Service Connect gets a configuration the control plane accepts but that never produces service-to-service reachability, so every inter-service call inside the namespace fails and the application is silently broken with no error to diagnose — and no amount of waiting converges.
Fix: the ECS Service Connect implementation — the namespace/discovery-name registration performed when a service is created with serviceConnectConfiguration, and the per-task Service Connect proxy that must make the server service reachable from a sibling task by its discoveryName/clientAlias.
Adjacent open filings that do NOT carry this check's replay key (the suite did not mark it OWNED): 20261111T010150Z-ecs-logs-2-net-new-causes-service-connect-discovery-never.md and 20261105T015737Z-ecs-responses-diverge-service-connect-endpoint-failure-arn.md.
Traces: ecs__logs.ecs-service-connect-client-task-reaches-the-server-service-by-its-discovery-name.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator; service heading ecs__logs 1/23 (4%), 13 failed (12 marked OWNED, 9 unloadable); the id below is the sole survivor:

[mismatch] ecs__logs.ecs-service-connect-client-task-reaches-the-server-service-by-its-discovery-name
  Error: Check timed out after 60.0s
  Expected: the client task's request to the server service's Service Connect discovery name succeeds and the exchange is observable. Actual: never observed inside the check's budget.

The 60.0s is the check's own budget, not a requirement; the defect is that Service Connect reachability never materialises.

Smithy consulted (model services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/com-amazonaws-ecs-amazonec2containerservicev20141113.json): CreateService/UpdateService serviceConnectConfiguration {enabled, namespace, services[].{portName, discoveryName, clientAliases[].{port,dnsName}}}. The reachability behaviour itself is recorded behavior only.

## Steps to Reproduce
1. Clone the repo and replay the behavioral suite; the id in trace_ids reproduces.
2. Create a cluster with a Service Connect namespace and a log group for awslogs.
3. CreateService for a server task definition with serviceConnectConfiguration enabled, services[] naming the container's portName and a discoveryName.
4. CreateService (or RunTask) for a client task in the same namespace with serviceConnectConfiguration enabled, and have it request the server by the discovery name.
5. Expected: the client's request to the discovery name succeeds against the server container. Actual: the reachability condition is never observed and the check burns its whole budget.
6. Re-replay the id in trace_ids to confirm the fix.
