Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__elbv2__logs.ecs-container-health-check-failure-makes-the-service-replace-the-task-and-the-alb-drop-the-old-target
Issue-Key: ecs__elbv2__logs.ecs-container-health-check-failure-makes-the-service-replace-the-task-and-the-alb-drop-the-old-target|_poll_until#22|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: b35089c281a7e84768d928c8c72034d27eee1ae8

# ecs__elbv2__logs: a container's CGI endpoint is never executed, so the health check cannot be made to fail and no task replacement is driven

> _Split out of `issues/things-that-now-work/20261110T220525Z-ecs-elbv2-logs-2-net-new-causes-the.md`, whose cause 1 (the ALB fail-open / empty-group-503 chain) is answered. The cause below is NOT fixed; it keeps the original report's trace id and Issue-Key so it stays filed and replayable. It was identified against the code base at commit `b35089c281a7e84768d928c8c72034d27eee1ae8`, so trace it back to the current code base._

## Operation
DescribeTasks (containers[].healthStatus) + the container's own server

## Description
Cause 2 of the split report: a container with a failing HEALTHCHECK must report `healthStatus`
UNHEALTHY and have its task stopped and replaced by the service scheduler, with the stopped task's
address leaving the attached target group. The check drives that failure by asking the container's
own server for a `/cgi-bin/break` endpoint through the load balancer, and the emulator's container
server never executes it, so the failure the whole chain hangs off is never induced.

AWS: Smithy (ecs) `RegisterTaskDefinition` containerDefinitions[].healthCheck {command, interval,
timeout, retries, startPeriod}; `DescribeTasks` -> tasks[].containers[].healthStatus with values
HEALTHY / UNHEALTHY / UNKNOWN, and tasks[].{lastStatus,desiredStatus}. On AWS the container's
`healthStatus` becomes HEALTHY once the command succeeds and UNHEALTHY after `retries` consecutive
failures; a service whose task goes UNHEALTHY stops and replaces it, the stopped task's ip leaves
the target group, the replacement registers and becomes 'healthy', and HTTP through the load
balancer answers 200 before and after. The container of this suite runs BusyBox's `httpd`, whose own
documentation serves `/cgi-bin` by EXECUTING the script a request names there and answering with
that script's output — which is how the check flips the container from passing its probe to failing
it.

Emulator, as the check now sees it: `healthStatus` reaches HEALTHY (the original report's first
key is past — the probe runs and publishes its result,
`services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/state/container_health.rs`),
and the check then polls for the container's `/cgi-bin/break` endpoint to answer through the load
balancer until the run budget ends: `TimeoutError: the /cgi-bin/break endpoint answering at
http://127.0.0.1:<port>/: the run budget ended first; not graded` (the address and port are per-run
values). Nothing answers it because the container's server only READS documents off the container's
filesystem — `state/container_servers.rs` records what the container's program started and
`::service_common::static_web_servers` plus the one static reading beside it decide what a path
answers — so a request for a script under the server's CGI directory is the 404 a static server
gives a path it holds no document at, whatever the container's own program wrote there.

Required: the container's server has to EXECUTE a script the request names under its CGI directory
in that container's own session — the container already has an in-process shell and a filesystem its
program writes through (`state/container_shell.rs`, `state/container_filesystem.rs`) — answer with
the status and body that execution produced, and PERSIST the filesystem change the script makes, so
the container's own `healthCheck` probe then fails and the rest of the chain (UNHEALTHY after
`retries`, the service-driven stop and replacement, the old address leaving
`DescribeTargetHealth`, the replacement reaching 'healthy', HTTP 200 throughout) follows from it.

Traces: ecs__elbv2__logs.ecs-container-health-check-failure-makes-the-service-replace-the-task-and-the-alb-drop-the-old-target

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator.

[error] ecs__elbv2__logs.ecs-container-health-check-failure-makes-the-service-replace-the-task-and-the-alb-drop-the-old-target
  TimeoutError: the /cgi-bin/break endpoint answering at http://127.0.0.1:<port>/: the run budget ended first; not graded
  Expected (AWS): the endpoint answers (its script runs), the container's probe then fails
  `retries` times over and `healthStatus` is UNHEALTHY; the service stops and replaces the task; the
  old ip leaves the target group and the replacement becomes 'healthy'; HTTP through the load
  balancer answers 200 before and after.
  Actual (emulator): the container's server answers nothing for a path under its CGI directory, so
  the probe keeps passing and no replacement is ever driven.

Smithy consulted: ecs RegisterTaskDefinition containerDefinitions[].healthCheck {command, interval,
timeout, retries, startPeriod} and DescribeTasks output tasks[].containers[].healthStatus /
tasks[].lastStatus; elasticloadbalancingv2 DescribeTargetHealth output
TargetHealthDescriptions[].TargetHealth {State, Reason, Description}.

## Steps to Reproduce
1. Clone the repo and replay the behavioral suite; the id in trace_ids reproduces.
2. Register a Fargate task definition whose container runs BusyBox's `httpd` over a document root
   its own command writes, including an executable script under that root's `cgi-bin` directory, and
   whose `healthCheck` command probes a path that script removes.
3. Run it as a service with desiredCount=1 bound to an ip target group behind an HTTP listener and
   poll `DescribeTasks` for containers[].healthStatus HEALTHY. That settles.
4. GET the `cgi-bin` script through the load balancer. Expected: the script runs and answers.
   Actual: 404 — nothing executes it, so the probe never starts failing.
5. Then expect UNHEALTHY, a service-driven task replacement, the old ip leaving
   DescribeTargetHealth, the new ip becoming 'healthy', and HTTP 200 through the load balancer
   throughout.
6. Re-replay the id in trace_ids to confirm the fix.
