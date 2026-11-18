Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecs__logs.obs-ecs-2-fargate-service-metrics-dimensioned-per-service-not-per-cluster
Issue-Key: cloudwatch__ecs__logs.obs-ecs-2-fargate-service-metrics-dimensioned-per-service-not-per-cluster|_poll_until#13|Timeout|raise AssertionError(f"{description} not met within {timeout_s}s. Last result: {last!r}")
Issue-Commit: 027692653b37f8059828c0218f5349555bde1430

# cloudwatch__ecs__logs: a Fargate service publishes no AWS/ECS CPUUtilization/MemoryUtilization series at all, so the ClusterName+ServiceName vs ClusterName-only alarm contrast reaches no verdict (1 cause)

> _Note: this issue was identified against the code base at commit `027692653b37f8059828c0218f5349555bde1430`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMetricStatistics

## Description
Cause 1 of 1: a running Fargate service produces no `AWS/ECS` service-level telemetry, so neither the per-service series nor the alarm built on it is ever observable and the check burns its entire budget without reaching an assertion.

NOTE for the fixer: the open issue `20261110T153046Z-cloudwatch-ecs-logs-aws-ecs-service-metrics-are.md` already names this exact check in its `Issue-Trace-Ids`, but the suite did NOT mark this failure as OWNED by it (its recorded replay key, `..|_poll_until#13|Timeout|time.sleep(interval_s)`, does not match this run's harness-level timeout), so it is re-filed here rather than dropped. If that issue is still open, close them together — this is the same root cause, not a second one.

AWS: recorded behavior only (the Smithy model does not describe metric emission). AWS publishes `AWS/ECS` `CPUUtilization` and `MemoryUtilization` for a service at 60 s resolution, dimensioned EXACTLY `ClusterName` + `ServiceName`, with `Unit` = `Percent`; `MemoryUtilization` is a percentage of the task's hard memory limit (0..100) and neither statistic is negative. For a Fargate-only cluster there is no `ClusterName`-only series, because cluster-level utilization derives from EC2 container instances and Fargate has none — so an alarm dimensioned `ClusterName`+`ServiceName` leaves `INSUFFICIENT_DATA` and transitions on the data, while its `ClusterName`-only twin stays `INSUFFICIENT_DATA` indefinitely, and `ListMetrics(Namespace=AWS/ECS, Dimensions=[ClusterName=<cluster>])` lists both metric names with the single dimension set `(ClusterName, ServiceName)` naming that service. This holds with `containerInsights` pinned `disabled`, i.e. it is the base `AWS/ECS` namespace, not Container Insights.

Emulator: nothing is observed and the run is killed — `Error: Check timed out after 600.0s`. The check's first bounded wait is `DescribeServices` until `runningCount >= 1` (timeout 600 s), followed by 600 s waits for `GetMetricStatistics` datapoints on the `ClusterName`+`ServiceName` pair and for the per-service alarm to reach `ALARM`. The 600 s harness cap is consumed before any of those predicates is met, so the emulator never presents either a converged service or a single `AWS/ECS` datapoint, and no alarm ever leaves its initial state. Nothing distinguishes "no per-service series" from "a leaked ClusterName-only series" here, because no series exists at all.

Why: a customer writes service-level CPU/memory alarms and Application Auto Scaling target-tracking policies against `AWS/ECS` `ClusterName`+`ServiceName`. Against this emulator those alarms sit in `INSUFFICIENT_DATA` forever: scaling never triggers, dashboards are blank, and the negative control (a `ClusterName`-only alarm that must never receive data) passes vacuously, so a customer cannot learn which dimension set ECS actually publishes under.

Fix: the `AWS/ECS` service-metric publisher in the ecs crate (emit `CPUUtilization`/`MemoryUtilization` in `Percent` on the `ClusterName`+`ServiceName` dimension set for a running service, and emit no `ClusterName`-only series for a Fargate-only cluster), plus whatever keeps a Fargate service from reaching `runningCount == desiredCount` so the publisher has a running task to meter. Also related but not covering this: `20261109T143515Z-cloudwatch-ecs-logs-container-insights-publishes-no-cluster-metrics-and-no.md` (Container Insights namespace) and `20261104T062603Z-cloudwatch-ecs-logs-a-cluster-s-inherited-containerinsights.md` (inherited containerInsights capture).

Traces: cloudwatch__ecs__logs.obs-ecs-2-fargate-service-metrics-dimensioned-per-service-not-per-cluster.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout.

[mismatch] cloudwatch__ecs__logs.obs-ecs-2-fargate-service-metrics-dimensioned-per-service-not-per-cluster
  Error: Check timed out after 600.0s

Expected: with a cluster created with settings [{name: containerInsights, value: disabled}], a registered task definition and a Fargate service at desiredCount=1 — DescribeServices reaching runningCount=1; GetMetricStatistics(Namespace='AWS/ECS', MetricName in {CPUUtilization, MemoryUtilization}, Dimensions=[ClusterName=<cluster>, ServiceName=<service>], Period=60) returning datapoints with Unit='Percent' (MemoryUtilization Maximum within 0..100, all Minimum >= 0); the alarm on that dimension pair reaching StateValue='ALARM' with Dimensions exactly {ClusterName, ServiceName}; the ClusterName-only twin alarm remaining 'INSUFFICIENT_DATA' with no ClusterName-only datapoints across a 3-minute barrier; and ListMetrics(AWS/ECS, ClusterName=<cluster>) listing both metric names with the single dimension set (ClusterName, ServiceName) naming that service.

Actual: the 600 s cap elapses with no verdict — no converged service, no AWS/ECS datapoint and no alarm state transition is ever observed. The requirement is the dimension set, unit and alarm behavior, not the particular cluster/service names or utilization numbers of any one run.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. ecs CreateCluster with settings [{name: containerInsights, value: disabled}]; RegisterTaskDefinition for FARGATE with a public image; CreateService with desiredCount=1 and an awsvpc network configuration.
3. Poll ecs DescribeServices until runningCount >= 1.
4. cloudwatch PutMetricAlarm twice: one on Namespace=AWS/ECS MetricName=CPUUtilization with Dimensions=[ClusterName,ServiceName], one with Dimensions=[ClusterName] only.
5. Poll cloudwatch GetMetricStatistics for AWS/ECS CPUUtilization and MemoryUtilization on the ClusterName+ServiceName pair (Period=60), and DescribeAlarms on both alarms; then ListMetrics(Namespace=AWS/ECS, Dimensions=[ClusterName=<cluster>]).
6. Or replay the check named in trace_ids.
