Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecs__logs.obs-ecs-2-fargate-service-metrics-dimensioned-per-service-not-per-cluster
Issue-Key: cloudwatch__ecs__logs.obs-ecs-2-fargate-service-metrics-dimensioned-per-service-not-per-cluster|_poll_until#14|Timeout|last = probe()
Issue-Commit: 76a27d9bdd16e996b43ee8be0e1161c3fa717030

# cloudwatch__ecs__logs: the per-service AWS/ECS metric differential reaches no verdict — a Fargate service's desired task never becomes running, so CPUUtilization/MemoryUtilization are never published for ClusterName+ServiceName

> _Note: this issue was identified against the code base at commit `76a27d9bdd16e996b43ee8be0e1161c3fa717030`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeServices

## Description
Cause 1 of 1: a Fargate service with desiredCount 1 never reports runningCount >= 1, so the whole per-service-vs-per-cluster AWS/ECS dimensioning differential is cut off at its 600 s budget with no observation.

AWS: Smithy ecs#CreateService takes desiredCount and ecs#DescribeServices returns Service.runningCount/desiredCount; a FARGATE service (awsvpc, assignPublicIp=ENABLED, task definition cpu 256 / memory 512 with one essential busybox container running `sh -c "sleep 1700"`) places its one task and DescribeServices settles at runningCount=1, desiredCount=1. Recorded behavior for the metrics that follow: AWS/ECS CPUUtilization and MemoryUtilization exist for such a service ONLY as ClusterName+ServiceName series, published in Unit 'Percent' (MemoryUtilization between 0 and 100 as a percentage of the task's hard memory limit), while a ClusterName-only series of the same metric names never receives data (so an alarm on the pair leaves 'missing' data behind on the cluster-only twin).

Emulator: the check makes no observable progress and is killed at 600 s — exactly the budget of its first poll, `DescribeServices(cluster, [service]).runningCount >= 1`, so the service never reports a running task and none of the metric assertions is ever reached. No AWS/ECS datapoint or ListMetrics entry for either dimension set is observed. Hypothesis: the placed task is settled as exited instead of running (the same immediately-STOPPED task behavior this suite shows in sibling groups), so the service's runningCount stays 0 and nothing drives the AWS/ECS namespace.

Why: a client sees a service it created stay permanently at runningCount 0 with no failure to diagnose, and every per-service telemetry consumer (service CPU/memory alarms, scaling policies dimensioned ClusterName+ServiceName, dashboards) receives no data; the fact that AWS deliberately does NOT publish these metrics per-cluster is likewise unverifiable.

Fix: the ECS service scheduler / task-lifecycle component that must bring a Fargate service's desired task to RUNNING and keep runningCount in step, and the AWS/ECS per-service metric publisher that emits CPUUtilization/MemoryUtilization in Percent dimensioned ClusterName+ServiceName. Related open issues cover Container-Insights namespaces for other checks and do not carry this one: 20261122T080343Z-cloudwatch-ecs-logs-a-cluster-that-inherits-containerinsights.md, 20261123T054448Z-cloudwatch-ecs-logs-a-container-insights-cluster-s.md.

Traces: cloudwatch__ecs__logs.obs-ecs-2-fargate-service-metrics-dimensioned-per-service-not-per-cluster.

## Evidence
Expected (AWS): DescribeServices(cluster=<cluster>, services=[<service>]) settles at runningCount=1 / desiredCount=1 within the 600 s poll; then GetMetricStatistics(Namespace='AWS/ECS', MetricName in {CPUUtilization, MemoryUtilization}, Dimensions=[ClusterName=<cluster>, ServiceName=<service>], Period=60) returns datapoints whose Unit is exactly 'Percent', while the same metrics dimensioned ClusterName only return nothing and ListMetrics for the cluster yields only ClusterName+ServiceName dimension sets.

Actual (emulator): 'Error: Check timed out after 600.0s' — the first poll (service runningCount >= 1) never satisfies its predicate and the check ends with no metric observation at all. Cluster, service and task-definition names are per-run values; the divergence is that the service never runs its one desired task and no AWS/ECS series appears.

## Steps to Reproduce
1. Create a VPC with two public subnets behind an internet gateway.
2. CreateCluster with settings=[{name:'containerInsights', value:'disabled'}].
3. RegisterTaskDefinition(family=<f>, requiresCompatibilities=[FARGATE], networkMode=awsvpc, cpu='256', memory='512', containerDefinitions=[{name:'app', image:'public.ecr.aws/docker/library/busybox:1.36', essential:true, command:['sh','-c','sleep 1700']}]).
4. CreateService(cluster, serviceName=<s>, taskDefinition=<arn>, desiredCount=1, launchType=FARGATE, awsvpcConfiguration with those subnets and assignPublicIp=ENABLED).
5. Poll DescribeServices for runningCount >= 1 (600 s). Expected runningCount=1; observed it never rises above 0, so the subsequent AWS/ECS CPUUtilization/MemoryUtilization reads for ClusterName+ServiceName (expected Percent datapoints) and for ClusterName only (expected none) are never reached.
