Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecs__logs.container-insights-publishes-cluster-metrics-and-performance-logs
Issue-Key: cloudwatch__ecs__logs.container-insights-publishes-cluster-metrics-and-performance-logs|_poll_until#70|AssertionError|raise AssertionError(f"{description} not met within {timeout_s}s. Last result: {last!r}")
Issue-Commit: e66fdf3591ed5b4fa9ba72fa7427a145df3792df

# cloudwatch__ecs__logs: a Container-Insights cluster's Fargate task creates its awslogs stream but delivers no container stdout events

> _Note: this issue was identified against the code base at commit `e66fdf3591ed5b4fa9ba72fa7427a145df3792df`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Cause 1 of 1: the ECS `awslogs` log driver creates the task's log stream but never delivers the container's stdout as log events.

AWS: Smithy (ECS, com.amazonaws.ecs#LogConfiguration) models `logDriver` (required, enum `LogDriver`) and `options`; its documentation states that for Fargate the supported drivers are `awslogs`, `splunk` and `awsfirelens`, that `awslogs-group`, `awslogs-region` and `awslogs-stream-prefix` are required on Fargate, and that "If you specify a prefix with this option, then the log stream takes the format prefix-name/container-name/ecs-task-id" (see "Send Amazon ECS logs to CloudWatch"). Beyond the stream NAME the delivery itself is not modelled, so this is recorded behavior only: on real AWS a Fargate task whose container definition carries `logDriver: awslogs` with `awslogs-stream-prefix: ecs` and whose execution role is granted `logs:CreateLogStream` and `logs:PutLogEvents` emits each line the container writes to stdout/stderr into the log group named by `awslogs-group`, in the stream `<prefix>/<container-name>/<task-id>`, one log event per line with the line's text verbatim as the event `message`. Those events are then readable with CloudWatch Logs `GetLogEvents`/`FilterLogEvents` on that group and stream while the task is RUNNING.

Emulator: the stream is created but stays permanently empty. In the replayed check the task reaches `lastStatus == RUNNING`, the whole Container-Insights path succeeds (the full ECS/ContainerInsights metric set under both the `(ClusterName)` and `(ClusterName, TaskDefinitionFamily)` dimension sets, MemoryReserved/CpuReserved/EphemeralStorageReserved values and units, TaskCount, the `/aws/ecs/containerinsights/<cluster>/performance` group with its `ClusterTelemetry-<cluster>` and `FargateTelemetry-*` streams and a matching Type=Task EMF record), and the application log stream `ecs/app/<task-id>` is created in the application log group — but reads of that stream return an empty event list for the full 300 s wait. The container's command allocates a tmpfs ballast and then `echo`es a marker to stdout; the marker never appears:

  AssertionError: marker 'sim2real-insights-<id>' in <app-log-group>/ecs/app/<task-id> not met within 300s. Last result: []

The `[]` is the literal result of the log read, i.e. zero events, not a wrong message. No error status or error code is returned anywhere — every API call succeeds — so this is a silent data-plane gap, not a refusal.

Why: a client that reads a Fargate task's application logs through the `awslogs` driver sees a stream that exists but is forever empty. Container stdout is the only channel a Fargate task has for application output, so every log-based workflow built on it breaks silently: log-driven assertions, `FilterLogEvents` searches, metric filters and subscription filters over the application group, and any diagnosis of what a task printed. Because the stream IS created, the usual "is logging wired up?" probe passes and the failure looks like an application that produced no output rather than an emulator that dropped it. It also makes the Container-Insights path misleading: telemetry about the task is published while the task's own output is discarded, so the two sources disagree about whether the container ran at all.

Fix: the ECS task runtime's `awslogs` log-driver delivery path — the component that, for a task placed by RunTask, tails the container's stdout/stderr and turns each line into a CloudWatch Logs event in the configured group/stream. The stream-creation half is present (the stream named `<awslogs-stream-prefix>/<container-name>/<task-id>` appears); the event-emission half is missing, so container output is never forwarded into the logs service's stored events for that stream.

Traces: cloudwatch__ecs__logs.container-insights-publishes-cluster-metrics-and-performance-logs — the check fails only at its final step, `_await_log_marker` on the application log group, after every Container-Insights metric and performance-log assertion has already passed.

Related but NOT covering this: open issues 20261104T062603Z-ecs-logs-the-awslogs-driver-never-delivers-awsvpc.md, 20261105T143800Z-ecs-logs-4-net-new-causes-awslogs-delivery.md, 20261111T030649Z-ecs-logs-6-net-new-causes-execution-role.md (service ecs__logs), 20261116T034914Z-cloudwatch-ecs-elbv2-logs-an-alb-registered-ecs.md (service cloudwatch__ecs__elbv2__logs) and 20261116T034914Z-ecs-logs-containers-never-run-and-stopped-tasks.md describe awslogs delivery gaps under other service headings; the one open issue under this heading, 20261122T080343Z-cloudwatch-ecs-logs-a-cluster-that-inherits-containerinsights.md, is about containerInsights inherited from the account default and owns a different check.

## Evidence
Expected (real AWS): the Fargate task's container writes a line to stdout; the `awslogs` driver creates the stream `ecs/app/<task-id>` in the configured `awslogs-group` AND appends one log event per line, with the line's text as the event `message`, so a `GetLogEvents`/`FilterLogEvents` read of that group+stream returns an event containing the echoed marker. (Fixed text of the expectation: one event per stdout line, message byte-identical to the line. Variable parts taken from the request/state: the log group name from `awslogs-group`, the stream name from `<awslogs-stream-prefix>/<container-name>/<task-id>` where the task id comes from the RunTask-returned task ARN, and the marker text from the container `command`.)

Actual (emulator): all control-plane calls return 2xx and the stream is created, but every read of that stream returns an empty event list for 300 s:

  File "check_cloudwatch__ecs__logs_container_insights_publishes_cluster_metrics_and_performance_logs.py", line 694, in _await_log_marker
    msgs = _poll_until(
  File "check_cloudwatch__ecs__logs_container_insights_publishes_cluster_metrics_and_performance_logs.py", line 360, in _poll_until
    raise AssertionError(f"{description} not met within {timeout_s}s. Last result: {last!r}")
  AssertionError: marker 'sim2real-insights-474eab7c' in sim2real-xsvc-lg-1943c423087e/ecs/app/00000000000000000000000000000001 not met within 300s. Last result: []

The group/stream/task-id values above are this run's generated names and are not themselves the requirement; the requirement is that the stream named by the prefix/container/task-id rule is non-empty and carries the container's stdout verbatim. Note also that `_await_log_stream` on the same group+stream succeeded immediately beforehand, proving the stream exists and only its events are missing. No HTTP error status and no error code is produced — the divergence is a missing-data one.

## Steps to Reproduce
1. Create a CloudWatch Logs log group G.
2. ECS CreateCluster with settings `[{name: containerInsights, value: enabled}]`.
3. Create an execution role trusted by `ecs-tasks.amazonaws.com` allowing `logs:CreateLogStream` and `logs:PutLogEvents` on `*`.
4. RegisterTaskDefinition for Fargate (cpu "256", memory 512) with one essential container named `app` whose `command` allocates a tmpfs ballast and then `echo`es a unique marker and sleeps, and whose `logConfiguration` is `{logDriver: "awslogs", options: {awslogs-group: G, awslogs-region: <region>, awslogs-stream-prefix: "ecs"}}`, using that execution role.
5. RunTask on the cluster with a public subnet; poll DescribeTasks until `lastStatus == RUNNING`.
6. Wait for the Container-Insights metrics and the `/aws/ecs/containerinsights/<cluster>/performance` group/streams — all of these succeed.
7. logs:DescribeLogStreams on G and confirm the stream `ecs/app/<task-id>` exists (it does).
8. logs:GetLogEvents (or FilterLogEvents) on G / `ecs/app/<task-id>` and look for the marker.
   Expected: an event whose message contains the marker the container echoed.
   Actual: an empty event list, indefinitely.
