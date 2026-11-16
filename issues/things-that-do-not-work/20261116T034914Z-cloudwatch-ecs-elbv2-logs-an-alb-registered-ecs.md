Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecs__elbv2__logs.alb-target-group-registers-ecs-tasks
Issue-Key: cloudwatch__ecs__elbv2__logs.alb-target-group-registers-ecs-tasks|_poll_until#24|AssertionError|raise AssertionError(f"condition not met within {timeout_s}s; last state={last!r}")
Issue-Commit: 3b9b0467f2eb3d7399c7e850c9f1cb96948fddb1

# cloudwatch__ecs__elbv2__logs: an ALB-registered ECS task's awslogs stream is created but never receives any log events, so the task behind the target group is never proven alive

> _Note: this issue was identified against the code base at commit `3b9b0467f2eb3d7399c7e850c9f1cb96948fddb1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetLogEvents

## Description
Cause 1 of 1: the awslogs log driver creates the task's log stream but never delivers a single event into it, because the task's container never actually runs.
AWS: recorded behavior and Smithy — an ECS service with a loadBalancers config registers its awsvpc task's private IP in the bound target group (that part the emulator does do: the registration and the stream creation both succeed here), and the task's container really runs, so logConfiguration logDriver=awslogs delivers the container's stdout/stderr into the stream it created and GetLogEvents (logs#GetLogEvents, startFromHead=true) returns at least one event. The nginx container's startup output is produced within seconds of the task reaching RUNNING.
Emulator: DescribeLogStreams returns the expected stream, but GetLogEvents on it returns an empty `events` list for the whole 120s polling window (last state=[]). The stream exists with no content, so a task reported RUNNING and registered as a healthy target has demonstrably produced nothing.
Why: the log stream is the only evidence a client has that the container behind an ALB target is serving rather than merely reported RUNNING. An always-empty stream means a client cannot debug a request path, cannot build metric filters or alarms on application output, and a broken deployment is indistinguishable from a working one.
Fix: the ECS task-execution path (running the container image and capturing its stdout/stderr) and the awslogs log-driver delivery path that PutLogEvents into the stream it created for the task.
Traces: cloudwatch__ecs__elbv2__logs.alb-target-group-registers-ecs-tasks.

## Evidence
Expected (AWS): after the service's task is RUNNING and registered in the target group, GetLogEvents(logGroupName=<group>, logStreamName=<the created stream>, startFromHead=True) returns >= 1 event containing the container's startup output. Actual (emulator): AssertionError: condition not met within 39.5s; last state=[] — GetLogEvents returns an empty events list on every poll, although DescribeLogStreams already lists the stream (the 39.5s figure is this run's remaining budget, not a requirement; the empty events list is the invariant).

## Steps to Reproduce
1. Create an ALB, a target group (port 80, health check '/') and a forward listener in the shared VPC.
2. CreateLogGroup; create an ECS execution role; CreateCluster.
3. RegisterTaskDefinition (FARGATE/awsvpc) for public.ecr.aws/docker/library/nginx:alpine with logConfiguration logDriver=awslogs pointing at the group, and a command that writes an index.html marker then execs nginx.
4. CreateService with desiredCount=1 and loadBalancers bound to the target group; wait for runningCount=1 and read the task's private IP (registration in the target group succeeds).
5. DescribeLogStreams on the group → the stream appears. GetLogEvents on that stream with startFromHead=true, polling for ~120s → `events` stays empty.
