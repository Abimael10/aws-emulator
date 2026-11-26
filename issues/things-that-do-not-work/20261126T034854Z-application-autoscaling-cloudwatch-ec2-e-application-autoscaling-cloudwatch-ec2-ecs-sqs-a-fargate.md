Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: application-autoscaling__cloudwatch__ec2__ecs__sqs.step-scaling-never-fed-alarm-stays-insufficient-while-fed-twin-scales
Issue-Key: application-autoscaling__cloudwatch__ec2__ecs__sqs.step-scaling-never-fed-alarm-stays-insufficient-while-fed-twin-scales|_poll_until#5|AssertionError|raise AssertionError(f'{description} not observed within {timeout_s}s; last saw {last!r}')
Issue-Commit: 857f59baf94ca876ddeb888a415f04d0868c32ab

# application-autoscaling__cloudwatch__ec2__ecs__sqs: a Fargate service created with desiredCount=1 never reports runningCount=1, so the never-fed step-scaling alarm scenario cannot be set up

> _Note: this issue was identified against the code base at commit `857f59baf94ca876ddeb888a415f04d0868c32ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateService

## Description
One net-new root cause (Cause 1 of 1).

Cause 1 of 1: an ECS Fargate service created with desiredCount=1 against a valid task definition never converges — DescribeServices keeps reporting runningCount 0 — so the estate the step-scaling scenario is built on never comes up and the scenario's actual claim (a never-fed CloudWatch alarm stays INSUFFICIENT_DATA and applies no step while its fed twin scales) is never exercised.
AWS: Smithy: Service$runningCount (com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113) is documented as "The number of tasks in the cluster that are in the RUNNING state." and Service$desiredCount as the number of tasks the service is meant to keep; CreateService declares its modelled errors and the recording shows it accepted (HTTP 200) here, so the convergence itself is recorded behavior only. The recording requires that after CreateService(cluster=<cluster>, serviceName=<name>, taskDefinition=<registered revision>, desiredCount=1, launchType='FARGATE', networkConfiguration=<awsvpc subnets+sg>), the service scheduler starts the task and DescribeServices reports runningCount rising to equal desiredCount (1) within the task's pull-and-start time; the task's lastStatus reaches RUNNING and the service does not accumulate failures for a request AWS accepted. desiredCount is what the client asked for; runningCount is the observation of what is actually running, and a service that is ACTIVE with a satisfiable placement must make them meet.
Emulator: desiredCount is honoured in the read-back, but runningCount stays 0 — "AssertionError: ECS reporting runningCount 1 for '<service>' not observed within 180.0s; last saw 0" — so the service is permanently un-converged and every downstream assertion (the scalable target, the two step policies, the fed/never-fed alarm contrast) is unreachable.
Why: a client that waits for its service to be serving — the standard CreateService-then-wait-for-runningCount==desiredCount deployment gate — never proceeds; nothing ever runs behind the service, no per-service AWS/ECS metric is produced, and every Application Auto Scaling policy registered against ecs:service:DesiredCount on that resource has nothing to scale. Any deployment pipeline modelled on this emulator blocks at the rollout step.
Fix: the ECS service scheduler/state that backs CreateService and DescribeServices — a service whose placement is satisfiable must start its tasks, move them to RUNNING and report runningCount converging on desiredCount, rather than leaving runningCount pinned at 0.
Traces: application-autoscaling__cloudwatch__ec2__ecs__sqs.step-scaling-never-fed-alarm-stays-insufficient-while-fed-twin-scales. Several open issues describe the same non-convergence under other service headings — 20261125T220211Z-cloudwatch-ec2-ecs-ecs-fargate-service-tasks-never-reach-runningcount-desiredcount.md, 20261125T221450Z-ecs-the-service-scheduler-never-converges-5-net.md, 20261125T225717Z-application-autoscaling-ecs-2-net-new-causes-a.md and 20261126T004507Z-cloudwatch-ec2-ecs-a-fargate-service-created-with.md — but none of them carries this check's replay key, so it is filed here and should be re-replayed alongside whichever of those is fixed.

## Evidence
application-autoscaling__cloudwatch__ec2__ecs__sqs.step-scaling-never-fed-alarm-stays-insufficient-while-fed-twin-scales, verdict [mismatch]: AssertionError raised from the check's _poll_until inside _ecs_estate (check line 191) — "ECS reporting runningCount 1 for 'sim2real-behav-aas-unfed-svc-decbeaa8acf7' not observed within 180.0s; last saw 0". The budget is the check's own _RUNNING_S = 180.0, documented there as "Fargate tasks pull and start before runningCount reaches desiredCount".
Expected: DescribeServices reports runningCount == desiredCount == 1 for the newly created Fargate service within that window, after which the scenario registers the scalable target (ServiceNamespace 'ecs', ScalableDimension 'ecs:service:DesiredCount', MinCapacity 1, MaxCapacity 6) and grades the fed vs never-fed step policies. Actual: runningCount is 0 at every poll and the check aborts in estate setup. The service name shown is a per-run value, not part of the requirement.

## Steps to Reproduce
1. ecs CreateCluster(clusterName=<name>) and RegisterTaskDefinition for a Fargate-compatible task (requiresCompatibilities=['FARGATE'], networkMode='awsvpc', cpu/memory set, one container).
2. ecs CreateService(cluster=<cluster>, serviceName=<name>, taskDefinition=<registered revision>, desiredCount=1, launchType='FARGATE', networkConfiguration={awsvpcConfiguration:{subnets:[<subnet>], securityGroups:[<sg>]}}) — accepted with HTTP 200.
3. Poll ecs DescribeServices(cluster=<cluster>, services=[<name>]): services[0].runningCount must reach 1 (== desiredCount) within ~180 s, and ListTasks/DescribeTasks must show one task whose lastStatus is RUNNING. The emulator reports runningCount 0 indefinitely.
4. Re-replay application-autoscaling__cloudwatch__ec2__ecs__sqs.step-scaling-never-fed-alarm-stays-insufficient-while-fed-twin-scales to confirm; with the service converged, the check goes on to grade that a CloudWatch alarm which is never fed any PutMetricData stays INSUFFICIENT_DATA and applies no step, while its fed twin's step policy raises ecs:service:DesiredCount.
