Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecs__elbv2__logs.deploy-stack-missing-task-role-trust-fails-service-start cloudwatch__ecs__elbv2__logs.deploy-stack-without-log-permission-no-log-group-created cloudwatch__ecs__elbv2__logs.ecs-container-insights-disabled-no-detailed-metrics
Issue-Key: cloudwatch__ecs__elbv2__logs.deploy-stack-missing-task-role-trust-fails-service-start|run#20|AssertionError|assert final_good["lastStatus"] == "RUNNING", (
Issue-Key: cloudwatch__ecs__elbv2__logs.deploy-stack-without-log-permission-no-log-group-created|run#32|AssertionError|assert final_good["lastStatus"] == "RUNNING", f"positive control did not reach RUNNING: {final_good}"
Issue-Key: cloudwatch__ecs__elbv2__logs.ecs-container-insights-disabled-no-detailed-metrics|_wait_service_running#9|Timeout|time.sleep(2)
Issue-Commit: 76a27d9bdd16e996b43ee8be0e1161c3fa717030

# cloudwatch__ecs__elbv2__logs: 2 net-new causes — the stack's positive-control Fargate tasks never reach RUNNING (immediately STOPPED/exitCode 0), and the Container-Insights enabled-vs-disabled differential reaches no verdict

> _Note: this issue was identified against the code base at commit `76a27d9bdd16e996b43ee8be0e1161c3fa717030`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Two net-new root causes in this group, clustered from the three failing checks no open issue owns. The group's other, ALB-metric scenarios are covered by open issue 20261120T043219Z-cloudwatch-ecs-elbv2-logs-the-alb-fronted-ecs.md, which does not carry any of these three checks.

Cause 1 of 2: a Fargate task whose essential container runs `sleep 60` is settled as STOPPED with exitCode 0 instead of reaching RUNNING, so both permission-boundary differentials lose their positive control.
AWS: Smithy ecs#RunTask -> Task.lastStatus ("The last known status for the task"); a FARGATE/awsvpc task (cpu 256, memory 512) whose single essential container is public.ecr.aws/amazonlinux/amazonlinux:2023 with Command ["sleep","60"], launched into a public subnet with a task/execution role that IS correctly trusted and permissioned, walks PROVISIONING -> PENDING -> RUNNING and stays RUNNING while that command runs; its container carries no exitCode and the task no stoppedReason until the command actually ends. Recorded behavior: the positive-control task reports lastStatus RUNNING.
Emulator: DescribeTasks reports the positive-control task as lastStatus 'STOPPED', desiredStatus 'STOPPED', its container 'STOPPED' with exitCode 0 and its ENI attachment already 'DELETED', one second after createdAt — the container command is never run, every task is reported as having exited immediately. Because the GOOD half of each differential also fails to run, neither check can attribute its negative half (broken task-role trust; a role without logs:CreateLogGroup) to the permission at all.
Why: a client gets the same terminal outcome for a correct configuration and for a broken one, so an ECS permission or trust misconfiguration is indistinguishable from a working deployment, and no task-level behavior (awslogs delivery, log-group auto-creation, ALB target registration) is ever observable.
Fix: the ECS task-lifecycle component behind RunTask — advance a placed task's lastStatus and actually run the container definition's command, recording exitCode/stoppedReason/stopCode only when it terminates. Related open issues carrying the same shape in other groups: 20261118T145001Z-ecs-2-net-new-causes-a-long-running.md, 20261120T071621Z-cloudwatch-ec2-ecs-2-net-new-causes-an.md.
Traces: cloudwatch__ecs__elbv2__logs.deploy-stack-missing-task-role-trust-fails-service-start, cloudwatch__ecs__elbv2__logs.deploy-stack-without-log-permission-no-log-group-created.

Cause 2 of 2: the Container-Insights differential makes no observable progress — an insights-enabled cluster publishes no ECS/ContainerInsights TaskCount.
AWS: recorded behavior only (Container Insights metric publication is not in the Smithy model; ecs#CreateCluster takes settings[] {name: containerInsights, value: enabled|disabled}). A cluster created with containerInsights=enabled that runs a Fargate service publishes cluster-level detailed metrics in namespace ECS/ContainerInsights, including TaskCount dimensioned ClusterName=<cluster>, so GetMetricStatistics returns datapoints; a sibling cluster with containerInsights=disabled running the same service for the same period publishes no such series.
Emulator: the check reaches no verdict at all — it is cut off at the harness's 60 s replay cap with neither cluster's service reporting a running task and no ECS/ContainerInsights datapoint ever appearing, so the enabled-vs-disabled contrast is unobservable. Hypothesis: the service's task never becomes RUNNING (Cause 1), so nothing ever drives the namespace.
Why: a client cannot tell whether Container Insights is on: dashboards and alarms built on ECS/ContainerInsights TaskCount never receive data, and the absence of metrics for a disabled cluster cannot be distinguished from the emulator publishing nothing at all.
Fix: the ECS Container-Insights metric publisher (the component that emits the ECS/ContainerInsights namespace for clusters whose containerInsights setting is enabled) and the task lifecycle it reads from. Related open issues for the account-default variant, which do not carry this check: 20261122T080343Z-cloudwatch-ecs-logs-a-cluster-that-inherits-containerinsights.md, 20261123T014745Z-cloudwatch-ecs-logs-container-insights-inherited-from-the-account-default-publishes.md.
Traces: cloudwatch__ecs__elbv2__logs.ecs-container-insights-disabled-no-detailed-metrics.

## Evidence
Cause 1 — Expected (AWS): the positive-control task of the deployed stack (correct task-role trust / role with logs:CreateLogGroup) reports lastStatus 'RUNNING'.
Actual: AssertionError: positive control did not reach RUNNING: {... 'attachments': [{'type': 'ElasticNetworkInterface', 'status': 'DELETED', ...}], 'containers': [{'name': 'app', 'image': 'public.ecr.aws/amazonlinux/amazonlinux:2023', 'lastStatus': 'STOPPED', 'exitCode': 0, ...}], 'desiredStatus': 'STOPPED', 'createdAt': ... , 'connectivityAt': <createdAt+1s> ...}. Identical shape in both checks. Cluster/task/ENI ids, IPs and timestamps are per-run values; the divergence is the immediate STOPPED with exitCode 0 for a container whose command is `sleep 60`.

Cause 2 — Expected (AWS): GetMetricStatistics(Namespace='ECS/ContainerInsights', MetricName='TaskCount', Dimensions=[ClusterName=<insights-enabled cluster>]) returns datapoints once its Fargate service is running, while the same read for the insights-disabled sibling cluster returns none.
Actual: 'Error: Check timed out after 60.0s' — no step of the check completes, no datapoint for either cluster is ever observed.

## Steps to Reproduce
Cause 1: CreateStack with a VPC + public subnet, an ECS cluster, two task definitions (FARGATE/awsvpc, cpu 256, memory 512, container 'app' = public.ecr.aws/amazonlinux/amazonlinux:2023 with Command ["sleep","60"]) — one with a broken task role (trust not naming ecs-tasks.amazonaws.com) or a role lacking logs:CreateLogGroup, one correct. Then RunTask the GOOD task definition (launchType=FARGATE, awsvpcConfiguration with the stack's subnet/SG, assignPublicIp=ENABLED) and poll DescribeTasks. Expected lastStatus 'RUNNING'; observed 'STOPPED' with container exitCode 0 one second after createdAt.

Cause 2: CreateCluster twice, once with settings=[{name:'containerInsights', value:'enabled'}] and once 'disabled'; RegisterTaskDefinition (FARGATE, busybox, command ["sleep","1800"]) and CreateService(desiredCount=1) on each; wait for each service's task to run, then GetMetricStatistics on ECS/ContainerInsights TaskCount for each ClusterName. Expected datapoints for the enabled cluster and none for the disabled one; observed no progress at all before the replay cap.
