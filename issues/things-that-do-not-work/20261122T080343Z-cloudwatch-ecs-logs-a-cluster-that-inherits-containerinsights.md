Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecs__logs.obs-ecs-2-account-default-insights-inherited-explicit-disabled-publishes-nothing
Issue-Key: cloudwatch__ecs__logs.obs-ecs-2-account-default-insights-inherited-explicit-disabled-publishes-nothing|_poll_until#50|Timeout|raise AssertionError(f"{description} not met within {timeout_s}s. Last result: {last!r}")
Issue-Commit: 634fb37e35f51bdb636290ffeec5191f736cf22e

# cloudwatch__ecs__logs: a cluster that inherits containerInsights=enabled from the account default publishes no ECS/ContainerInsights TaskCount and no performance log group

> _Note: this issue was identified against the code base at commit `634fb37e35f51bdb636290ffeec5191f736cf22e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMetricStatistics

## Description
Cause 1 of 1: an ECS cluster created with NO settings while the account default containerInsights is enabled resolves its effective setting to enabled (DescribeClusters reports it), but it never publishes any ECS/ContainerInsights series for itself, so a client waiting for TaskCount waits forever.

AWS: Smithy (ecs): ClusterSetting.name targets enum ClusterSettingName with the single value containerInsights; ClusterSetting.value doc — "The supported values are enhanced, enabled, and disabled... If a cluster value is specified, it will override the containerInsights value set with PutAccountSetting or PutAccountSettingDefault." Metric and log emission itself is not expressible in the Smithy model, so for the emission the claim is recorded behavior only: with the account default put to enabled by PutAccountSettingDefault, a cluster created with no settings inherits enabled at creation time and, once a FARGATE task in it reaches RUNNING, publishes ECS/ContainerInsights TaskCount at period 60 with Unit "Count" and Maximum equal to the number of running tasks under the single dimension ClusterName, and CloudWatch creates the performance log group /aws/ecs/containerinsights/<clusterName>/performance on the account's behalf. A sibling cluster created with an explicit containerInsights=disabled stays silent (no datapoints, nothing in ListMetrics, no log group) until UpdateClusterSettings sets it to enabled, after which it publishes too. The inherited value is captured at CreateCluster, so restoring the account default afterwards does not change it. These appear within a minute or two of the task running, well inside the recording's 600 s wait.

Emulator: no TaskCount datapoint for the inheriting cluster ever appears. Nothing is refused — GetMetricStatistics(Namespace='ECS/ContainerInsights', MetricName='TaskCount', Dimensions=[{ClusterName: <inheriting cluster>}], Period 60, Statistics [Maximum, SampleCount]) answers 200 with an empty Datapoints list for the whole wait, so the check's bounded wait for the first datapoint never ends and the harness kills the run: "Check timed out after 600.0s". Because the emitter publishes nothing at all, the check also never reaches the later legs (the ClusterName performance log group for the inheriting cluster, and the repair leg where UpdateClusterSettings(enabled) on the explicitly-disabled sibling must make it start publishing), so those remain unverified rather than passing.

Why: a client that enables Container Insights account-wide and then relies on inheritance gets a cluster that reports containerInsights=enabled through DescribeClusters while CloudWatch has no data for it — Container Insights dashboards, TaskCount/CpuUtilized alarms and capacity automation see an empty series and cannot distinguish "no tasks" from "not emitting", and the per-cluster performance log group a consumer subscribes to is never created.

Fix: the Container Insights emitter in the ecs crate — the publisher that writes the ECS/ContainerInsights series and the /aws/ecs/containerinsights/<cluster>/performance log group — and the path that resolves a cluster's effective containerInsights value from the account default captured at CreateCluster, so an inherited enabled drives emission exactly as an explicit enabled does.

Traces: cloudwatch__ecs__logs.obs-ecs-2-account-default-insights-inherited-explicit-disabled-publishes-nothing.

Related open issue that does NOT carry this check's replay key and therefore does not cover it: 20261109T143515Z-cloudwatch-ecs-logs-container-insights-publishes-no-cluster-metrics-and-no.md (the explicitly-enabled-cluster variant plus per-service AWS/ECS dimensions). A fixer should merge the two, but this check's inheritance path (account default -> cluster at creation) is its own unverified behavior.

## Evidence
Check cloudwatch__ecs__logs.obs-ecs-2-account-default-insights-inherited-explicit-disabled-publishes-nothing [mismatch]:

  Error: Check timed out after 600.0s

Expected: after PutAccountSettingDefault(name='containerInsights', value='enabled'), a cluster created with no settings whose FARGATE task is RUNNING yields at least one GetMetricStatistics datapoint in ECS/ContainerInsights TaskCount with Maximum == 1.0 and Unit == 'Count' under Dimensions=[{Name: 'ClusterName', Value: <cluster>}], followed by the performance log group /aws/ecs/containerinsights/<cluster>/performance, while the sibling cluster created with containerInsights=disabled yields no datapoints, no ListMetrics entries and no log group.
Actual: the TaskCount query for the inheriting cluster returns Datapoints: [] for the entire 600 s budget (no status, code or message to compare — the call succeeds and is empty), so the check is killed by the harness wall clock before it can grade the disabled-sibling contrast or the UpdateClusterSettings repair leg.

## Steps to Reproduce
1. ecs DeleteAccountSetting(name='containerInsights') on the caller if a principal-level value exists, then PutAccountSettingDefault(name='containerInsights', value='enabled'); confirm ListAccountSettings(name='containerInsights', effectiveSettings=true) reads enabled.
2. ecs CreateCluster with NO settings (call it INHERIT) and CreateCluster with settings=[{name: 'containerInsights', value: 'disabled'}] (call it CONTROL). DescribeClusters(include=['SETTINGS']) must read enabled for INHERIT and disabled for CONTROL.
3. RegisterTaskDefinition (FARGATE, awsvpc, cpu 256, memory 512, a sleeping public image) and RunTask one task in each cluster in a VPC with public subnets; wait for both tasks to reach RUNNING.
4. cloudwatch GetMetricStatistics(Namespace='ECS/ContainerInsights', MetricName='TaskCount', Dimensions=[{Name:'ClusterName', Value: INHERIT}], Period=60, Statistics=['Maximum','SampleCount']).
5. Expect a datapoint with Maximum 1.0 and Unit 'Count' within ~600 s; observe Datapoints: [] indefinitely (and no /aws/ecs/containerinsights/<INHERIT>/performance log group), so the check never completes.
