Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecr__ecs.ecs-cluster-container-insights-metrics-visible-in-cloudwatch
Issue-Key: cloudwatch__ecr__ecs.ecs-cluster-container-insights-metrics-visible-in-cloudwatch|run#12|AssertionError|assert cpu_points, "no ECS/ContainerInsights CpuUtilized datapoints appeared"
Issue-Commit: 3b9b0467f2eb3d7399c7e850c9f1cb96948fddb1

# ECS/ContainerInsights CpuUtilized is never published for a running service on a Container-Insights-enabled cluster (RunningTaskCount now appears, CpuUtilized stays empty)

> _Note: this issue was identified against the code base at commit `3b9b0467f2eb3d7399c7e850c9f1cb96948fddb1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMetricStatistics

## Description
One net-new root cause, grounded in the single failing check listed under the suite heading `cloudwatch__ecr__ecs` that the report does not mark as already filed/owned.

NOTE on existing open issues: 20261110T155833Z-cloudwatch-ecr-ecs-2-net-new-causes-no.md (Cause 2) and 20261109T143515Z-cloudwatch-ecs-logs-container-insights-publishes-no-cluster-metrics-and-no.md describe the broader "no ECS/ContainerInsights metrics at all" symptom, and they carry this check id — but with the replay key `_poll_metric_datapoints#8|Timeout` (the whole 600 s budget consumed with an empty Datapoints list for RunningTaskCount). The current replay no longer behaves that way: RunningTaskCount datapoints DO appear and the `Maximum >= 1` assertion passes, and the check now fails later and differently, at `run#74 AssertionError: no ECS/ContainerInsights CpuUtilized datapoints appeared`. The suite therefore does not report it as owned, so the surviving residue is refiled here with the key the current replay produces. Those issues are not a substitute for this one; a fixer closing either must still replay the check named below.

Cause 1 of 1: a Container-Insights-enabled ECS cluster running a CPU-busy Fargate task publishes RunningTaskCount but no CpuUtilized into ECS/ContainerInsights.
AWS: Smithy (ecs): `ClusterSetting.name` is the enum `ClusterSettingName` whose single value is `containerInsights`, and `ClusterSetting.value` supports `enhanced`/`enabled`/`disabled`; the shape doc states the parameter "is used to turn on CloudWatch Container Insights with enhanced observability or CloudWatch Container Insights for a cluster" and that Container Insights "provides all the Container Insights metrics". The metric emission itself is recorded behavior only (ECS publishes it; it is not modelled in Smithy): with containerInsights=enabled, the ECS/ContainerInsights namespace receives the full per-service metric set for a service with running tasks — not only RunningTaskCount (Maximum >= the number of running tasks) but also the resource-utilisation series, including CpuUtilized, dimensioned by ClusterName + ServiceName. For a task whose container spins the CPU, CpuUtilized has Average > 0. cloudwatch#GetMetricStatistics(Namespace="ECS/ContainerInsights", MetricName="CpuUtilized", Dimensions=[ClusterName, ServiceName], Period=60, Statistics=["Average"]) over the elapsed window then returns a non-empty `Datapoints` list.
Emulator: GetMetricStatistics answers 200 but with an EMPTY `Datapoints` list for CpuUtilized for the whole polling window (2 min, 20 s interval, a 20-minute window ending at now), while the same call for RunningTaskCount on the identical dimensions returns datapoints with Maximum >= 1. So the ECS/ContainerInsights namespace is populated only with the task-count series; the per-service resource-utilisation series is never emitted. The check fails with `AssertionError: no ECS/ContainerInsights CpuUtilized datapoints appeared`.
Why: a client that reads Container Insights for utilisation — a dashboard, a CloudWatch alarm on CpuUtilized, Application Auto Scaling target tracking on a Container Insights metric, or a cost/right-sizing report — sees a cluster whose tasks are counted but whose CPU consumption is permanently unknown. Enabling Container Insights looks partially effective, which is worse than it being plainly absent: the task-count series proves the namespace works, so a caller concludes the workload genuinely consumes no CPU, and any wait-for-datapoint gate on CpuUtilized never completes.
Fix: the ECS cluster Container Insights metric publisher — the component that emits the ECS/ContainerInsights series gated on the cluster's `containerInsights` setting must emit the per-service/per-task resource-utilisation metrics (CpuUtilized, and the companion MemoryUtilized/CpuReserved/MemoryReserved series) alongside RunningTaskCount, derived from the running task's actual consumption — together with the CloudWatch datapoint store that GetMetricStatistics reads for that namespace.
Traces: cloudwatch__ecr__ecs.ecs-cluster-container-insights-metrics-visible-in-cloudwatch.

## Evidence
Failing check: [mismatch] cloudwatch__ecr__ecs.ecs-cluster-container-insights-metrics-visible-in-cloudwatch

Replay output (tail):
  File "run.py", line 445, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
  File "check_cloudwatch__ecr__ecs_ecs_cluster_container_insights_metrics_visible_in_cloudwatch.py", line 74, in run
    assert cpu_points, "no ECS/ContainerInsights CpuUtilized datapoints appeared"
  AssertionError: no ECS/ContainerInsights CpuUtilized datapoints appeared

What the check did (from the replayed source):
1. CreateCluster with settings [{name: containerInsights, value: enabled}].
2. RegisterTaskDefinition: FARGATE, networkMode awsvpc, cpu 256 / memory 512, one essential container `app` (busybox) with command `sh -c "i=0; while true; do i=$((i+1)); done"` — a deliberate CPU spin loop.
3. CreateService launchType FARGATE, desiredCount 1, awsvpcConfiguration on the shared network; waited until DescribeServices reported runningCount >= 1.
4. Polled GetMetricStatistics(Namespace="ECS/ContainerInsights", MetricName="RunningTaskCount", Dimensions=[{ServiceName},{ClusterName}], StartTime=now-20min, EndTime=now, Period=60, Statistics=["Maximum"]) — PASSED: datapoints returned and `Maximum >= 1` held.
5. Polled the same call with MetricName="CpuUtilized", Statistics=["Average"] (timeout 120 s, interval 20 s) — FAILED.

Expected vs actual for step 5:
- Expected: HTTP 200 with a non-empty `Datapoints` list, at least one entry having Average > 0 (the container is spinning the CPU).
- Actual: HTTP 200 with `Datapoints: []` on every poll until the loop's deadline; the helper returns [] and the assertion fires.

Note the asymmetry, which is the whole finding: the same namespace, the same dimensions and the same time window DO yield RunningTaskCount datapoints in this run, so this is not "Container Insights is wholly unpublished" (the state the two existing open issues recorded) but specifically the missing resource-utilisation series.

No per-run value is being asserted: cluster/service names are generated per run, the window is relative to now, and the assertion is only on presence of datapoints and Average > 0.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Re-replay the check in trace_ids (suite heading cloudwatch__ecr__ecs).
3. Or by hand:
   a. iam CreateRole trusting ecs-tasks.amazonaws.com and AttachRolePolicy arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy.
   b. ecs CreateCluster with settings=[{name: containerInsights, value: enabled}].
   c. ecs RegisterTaskDefinition: requiresCompatibilities=[FARGATE], networkMode=awsvpc, cpu=256, memory=512, executionRoleArn from (a), one essential container running `sh -c "i=0; while true; do i=$((i+1)); done"`.
   d. ecs CreateService launchType=FARGATE, desiredCount=1, awsvpcConfiguration with a subnet and the default security group; poll DescribeServices until runningCount >= 1.
   e. cloudwatch GetMetricStatistics(Namespace="ECS/ContainerInsights", MetricName="RunningTaskCount", Dimensions=[{Name:ServiceName,Value:<service>},{Name:ClusterName,Value:<cluster>}], StartTime=now-20m, EndTime=now, Period=60, Statistics=["Maximum"]) — datapoints are returned (control leg).
   f. Repeat (e) with MetricName="CpuUtilized", Statistics=["Average"], polling for ~2 minutes: `Datapoints` stays empty, and no CpuUtilized value for the busy task is ever available.
