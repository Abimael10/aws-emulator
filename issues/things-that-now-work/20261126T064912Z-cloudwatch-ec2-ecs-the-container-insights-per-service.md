Issue-Severity: HIGH
Issue-Commit: c92ef1c593cf3dfd71517e327310a26ec2182aba

# cloudwatch__ec2__ecs: the Container Insights per-service task-count scenario reaches no verdict — a Fargate service never converges and ECS/ContainerInsights RunningTaskCount/DesiredTaskCount are never published, so the check exhausts its 600 s budget

> _Note: this issue was identified against the code base at commit `c92ef1c593cf3dfd71517e327310a26ec2182aba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeServices

## Description
One net-new root cause, grounded in the single failing behavioral check under the suite heading `cloudwatch__ec2__ecs` that is not already owned by an open issue.

Related-but-not-covering open issues: `20261125T220211Z-cloudwatch-ec2-ecs-ecs-fargate-service-tasks-never-reach-runningcount-desiredcount.md` and `20261126T004507Z-cloudwatch-ec2-ecs-a-fargate-service-created-with.md` both describe a Fargate service whose runningCount stays 0. Neither covers this replay: on this run the check no longer reaches either of their recorded assertion keys (`_poll_until#18|AssertionError`) at all — it is killed by the harness budget before any assertion is evaluated (`Check timed out after 600.0s`), so the scenario produces no verdict on the per-service Container Insights metrics those issues never reach. Filing it separately as instructed; a fixer closing the convergence issues must still re-replay this check.

Cause 1 of 1: a Container-Insights-enabled cluster running two Fargate services (desiredCount 2 and 1 off one awsvpc/FARGATE task definition whose container command is long-lived, `sh -c sleep 3600`) never reaches the point where either the ECS control plane or the CloudWatch `ECS/ContainerInsights` namespace can be observed: the check spends its whole 600 s budget inside its bounded polls and is terminated.

AWS: Smithy (ecs): `Service.runningCount` is "The number of tasks in the cluster that are in the RUNNING state" and `Service.desiredCount` is "The desired number of instantiations of the task definition to keep running on the service"; `Service.pendingCount` is the PENDING count. The scheduler drives runningCount to desiredCount, so DescribeServices reports runningCount == desiredCount for a healthy service. Smithy (ecs): `ClusterSettingName` has the single value `containerInsights` and `ClusterSetting.value` supports `enhanced`/`enabled`/`disabled`; with the cluster setting `containerInsights=enabled`, AWS publishes the Container Insights ECS metrics. AWS documents those metrics (https://docs.aws.amazon.com/AmazonCloudWatch/latest/monitoring/Container-Insights-metrics-ECS.html): `RunningTaskCount` and `DesiredTaskCount` in namespace `ECS/ContainerInsights`, at 1-minute resolution, with Unit `Count`, dimensioned by `ClusterName` and — for service-level metrics — by `ClusterName`+`ServiceName`. The per-service value is scoped to that service only: `RunningTaskCount` for a service never exceeds the number of tasks that service ran. Recorded behavior only for latency: convergence and first datapoint arrive well inside the check's ceilings (420 s for runningCount, 900 s for the first matching metric minute, 300 s for DesiredTaskCount).

Emulator: the check is terminated with `Error: Check timed out after 600.0s` — the scenario never reaches a verdict. The emulator accepts CreateCluster with `settings=[{name: containerInsights, value: enabled}]` and DescribeClusters(include=[SETTINGS]) echoes `containerInsights=enabled`, and CreateService records the requested desiredCount, but the scenario then stalls: DescribeServices never reports runningCount equal to desiredCount for either service, and GetMetricStatistics on Namespace `ECS/ContainerInsights`, MetricName `RunningTaskCount`/`DesiredTaskCount`, Dimensions ClusterName+ServiceName, Period 60, Statistics [Maximum, Minimum] never yields a minute whose per-service Maximum values are (2.0, 1.0). Because the first poll alone budgets 420 s, the harness kills the check before the metric polls can even complete, so neither the per-service scoping assertion (`RunningTaskCount` for a service must not exceed the tasks it ran) nor the Unit assertion (`Unit` must be exactly `Count`) is ever exercised.

Why: a client that enables Container Insights to observe its ECS estate gets nothing it can act on. `aws ecs wait services-stable` and the botocore `services_stable` waiter never return; dashboards, Container Insights-driven alarms and any autoscaling or deployment gate keyed on `ECS/ContainerInsights` `RunningTaskCount`/`DesiredTaskCount` sit on empty `Datapoints` forever rather than reading per-service counts. Worse, the divergence is a hang rather than an error, so client code with no deadline blocks indefinitely instead of surfacing a failure.

Fix: two components. (1) The ECS service scheduler state must materialise tasks for a service and project the RUNNING ones into `Service.runningCount` (and PENDING into `Service.pendingCount`) after CreateService/UpdateService, in the ECS service state and its DescribeServices projection. (2) The Container Insights metric emission path must publish, for a cluster whose `containerInsights` setting resolves to `enabled`, the `ECS/ContainerInsights` `RunningTaskCount` and `DesiredTaskCount` metrics at 1-minute resolution with Unit `Count`, dimensioned per cluster and per `ClusterName`+`ServiceName`, with the per-service value scoped to that service's own tasks, so GetMetricStatistics returns Datapoints for them.

Traces: cloudwatch__ec2__ecs.metrics-ec2-ecs-container-insights-running-task-count-follows-each-service-desired-count.

## Evidence
Check cloudwatch__ec2__ecs.metrics-ec2-ecs-container-insights-running-task-count-follows-each-service-desired-count

Actual (emulator):
  Error: Check timed out after 600.0s
(no assertion was reached; the check was killed inside its bounded polls, whose own ceilings are 420 s for runningCount, 900 s for the first matching RunningTaskCount minute and 300 s for DesiredTaskCount)

Expected (AWS), in order, all well inside those ceilings:
1. DescribeClusters(clusters=[<cluster>], include=[SETTINGS]) -> settings contain containerInsights=enabled. (The emulator does satisfy this step.)
2. DescribeServices(cluster=<cluster>, services=[<svc-two>]) -> runningCount == 2; DescribeServices(services=[<svc-one>]) -> runningCount == 1.
3. GetMetricStatistics(Namespace="ECS/ContainerInsights", MetricName="RunningTaskCount", Dimensions=[{ClusterName:<cluster>},{ServiceName:<svc>}], Period=60, Statistics=[Maximum,Minimum]) -> non-empty Datapoints, with at least one common minute whose Maximum is 2.0 for the desired-2 service and 1.0 for the desired-1 service.
4. The same call for MetricName="DesiredTaskCount" -> a common minute reading 2.0 and 1.0 respectively.
5. Every RunningTaskCount datapoint for a service has Maximum <= that service's task count (per-service scoping) and Unit exactly "Count".

Actual (emulator) for 2-5: never observed — DescribeServices keeps reporting runningCount 0 while desiredCount is correct, and GetMetricStatistics for the ECS/ContainerInsights per-service dimensions returns no Datapoints, so the run exhausts the 600 s budget with no verdict.

Note on requirement vs. per-run values: the fixed requirements here are the namespace "ECS/ContainerInsights", the metric names "RunningTaskCount" and "DesiredTaskCount", the dimension names "ClusterName" and "ServiceName", the 60-second period and the Unit "Count". The cluster name, service names, task-definition family, subnet/security-group ids and timestamps are generated per run and are not part of the requirement; the counts 2 and 1 are simply the desiredCount values this scenario requests.

## Steps to Reproduce
1. Create a VPC (10.95.0.0/16) with an internet gateway, a 0.0.0.0/0 route on the main route table, a subnet (10.95.1.0/24) in an available AZ, and a security group with no ingress.
2. CreateCluster(clusterName=<cluster>, settings=[{name: "containerInsights", value: "enabled"}]).
3. DescribeClusters(clusters=[<cluster>], include=["SETTINGS"]) and confirm containerInsights=enabled.
4. RegisterTaskDefinition(family=<family>, networkMode="awsvpc", requiresCompatibilities=["FARGATE"], cpu="256", memory="512", containerDefinitions=[{name:"app", image:"public.ecr.aws/docker/library/busybox:latest", essential:true, command:["sh","-c","sleep 3600"]}]).
5. CreateService twice in that cluster off the same task definition with launchType="FARGATE" and awsvpcConfiguration {subnets:[<subnet>], securityGroups:[<sg>], assignPublicIp:"ENABLED"} — once with desiredCount=2, once with desiredCount=1.
6. Poll DescribeServices for each service until runningCount equals its desiredCount: the emulator keeps returning runningCount=0 while desiredCount reads 2 and 1.
7. Poll GetMetricStatistics(Namespace="ECS/ContainerInsights", MetricName="RunningTaskCount", Dimensions=[{Name:"ClusterName",Value:<cluster>},{Name:"ServiceName",Value:<svc>}], StartTime=now-30m, EndTime=now+1m, Period=60, Statistics=["Maximum","Minimum"]) for each service, looking for a common minute reading (2.0, 1.0): Datapoints stay empty. Repeat for MetricName="DesiredTaskCount" — also empty.
8. The whole scenario is killed at the 600 s harness budget without reaching the per-service-scoping and Unit=="Count" assertions.
