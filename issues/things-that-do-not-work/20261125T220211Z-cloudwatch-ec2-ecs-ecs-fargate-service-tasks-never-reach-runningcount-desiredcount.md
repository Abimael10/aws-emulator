Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ec2__ecs.metrics-ec2-ecs-container-insights-running-task-count-follows-each-service-desired-count cloudwatch__ec2__ecs.metrics-ec2-ecs-update-service-negative-desired-count-rejected
Issue-Key: cloudwatch__ec2__ecs.metrics-ec2-ecs-container-insights-running-task-count-follows-each-service-desired-count|_poll_until#18|AssertionError|raise AssertionError(f"{description} did not happen within {timeout_s}s; last read {last!r}")
Issue-Key: cloudwatch__ec2__ecs.metrics-ec2-ecs-update-service-negative-desired-count-rejected|run#19|AssertionError|assert (after["desiredCount"], after["runningCount"]) == (1, 1), (
Issue-Commit: 76a27d9bdd16e996b43ee8be0e1161c3fa717030

# ECS Fargate service tasks never reach runningCount: desiredCount is honored but runningCount stays 0

> _Note: this issue was identified against the code base at commit `76a27d9bdd16e996b43ee8be0e1161c3fa717030`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeServices

## Description
One net-new root cause, grounded in the failing behavioral checks under the suite heading `cloudwatch__ec2__ecs`.

Cause 1 of 1: a Fargate service's desired count is recorded but no task of the service ever becomes RUNNING, so runningCount never converges on desiredCount.
AWS: Smithy: ecs#Service.runningCount is "The number of tasks in the cluster that are in the RUNNING state" and ecs#Service.desiredCount is the count the scheduler drives toward; after CreateService/UpdateService the service launches tasks (PROVISIONING -> PENDING -> RUNNING) and DescribeServices reports runningCount == desiredCount for a healthy service whose container command is long-lived. Recorded behavior only for the convergence latency, which the checks poll for (420 s and 900 s budgets).
Emulator: DescribeServices reports the requested desiredCount immediately but runningCount stays 0 indefinitely. In the Container Insights check, UpdateService(desiredCount=2) leaves runningCount=0 after 420 s of polling; in the negative UpdateService check, the service sits at desiredCount=1 / runningCount=0, so the post-refusal invariant assertion ("a refusal must change nothing", which expects 1/1) fails even though the refusal of the negative desiredCount itself was correct.
Why: no client workflow that waits for a service to come up can complete — the `services_stable` waiter, ECS-dimensioned CloudWatch metrics (RunningTaskCount/DesiredTaskCount, CPU/MemoryUtilization), deployment health checks and anything that reads task state all hang or read zero while the control plane reports the service as created.
Fix: the ECS service scheduler state must materialise tasks for a service and count the RUNNING ones into Service.runningCount on CreateService/UpdateService (services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src — the service state and its DescribeServices projection).
Traces: cloudwatch__ec2__ecs.metrics-ec2-ecs-container-insights-running-task-count-follows-each-service-desired-count, cloudwatch__ec2__ecs.metrics-ec2-ecs-update-service-negative-desired-count-rejected.

## Evidence
Check metrics-ec2-ecs-container-insights-running-task-count-follows-each-service-desired-count:
AssertionError: service sim2real-xsvc-ci-2-<id> reaching runningCount=2 did not happen within 420.0s; last read 0

Check metrics-ec2-ecs-update-service-negative-desired-count-rejected:
AssertionError: the refused UpdateService left sim2real-xsvc-ci-1-<id> at desiredCount=1 runningCount=0; a refusal must change nothing (expected desiredCount/runningCount == (1, 1))

Expected (AWS): DescribeServices runningCount rises to the service's desiredCount (2, and 1 respectively).
Actual (emulator): runningCount is 0 for the whole poll budget while desiredCount is correct.

## Steps to Reproduce
1. Create a VPC + subnet + security group, an ECS cluster with containerInsights enabled, and RegisterTaskDefinition (FARGATE, awsvpc, cpu 256 / memory 512, image public.ecr.aws/docker/library/busybox:latest with a long-lived command).
2. CreateService(launchType=FARGATE, desiredCount=1) and poll DescribeServices: runningCount stays 0.
3. UpdateService(desiredCount=2) and poll DescribeServices for runningCount==2: it never arrives (420 s).
4. UpdateService(desiredCount=-1): the refusal is correct, but DescribeServices then shows desiredCount=1/runningCount=0 instead of 1/1.
