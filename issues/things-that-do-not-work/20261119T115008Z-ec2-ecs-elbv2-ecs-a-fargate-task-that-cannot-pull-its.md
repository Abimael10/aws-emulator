Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ecs__elbv2.ecsnt-scaling-the-isolated-service-up-stops-its-task-before-any-target-registers
Issue-Key: ec2__ecs__elbv2.ecsnt-scaling-the-isolated-service-up-stops-its-task-before-any-target-registers|run#14|AssertionError|assert task["stopCode"] == "TaskFailedToStart", f"stopCode: {task.get('stopCode')!r}"
Issue-Commit: b448d9011382618e887f073932b66b1aa6876ff6

# ecs: a Fargate task that cannot pull its image in an egress-less subnet stops with stopCode ServiceSchedulerInitiated instead of TaskFailedToStart/CannotPullContainerError

> _Note: this issue was identified against the code base at commit `b448d9011382618e887f073932b66b1aa6876ff6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTasks

## Description
One net-new root cause, exposed by the ec2__ecs__elbv2 environment-fidelity check that scales a service whose awsvpc subnets have no egress path (no IGW/NAT route) to desiredCount 1. The two other failing ec2__ecs__elbv2 checks are owned by 20261102T085002Z-ec2-ecs-elbv2-3-net-new-ecs-root.md (cluster capacity providers, CODE_DEPLOY task sets, UpdateService gating); that issue does not cover the stop classification of a task that never started, which is what diverges here.

Cause 1 of 1: a task that fails to START is reported as a task the scheduler STOPPED.
AWS: when a Fargate task is placed in subnets with no route to the registry and the container image pull therefore fails, the task reaches lastStatus 'STOPPED' having never run, and DescribeTasks classifies it as a start failure: tasks[].stopCode is the TaskStopCode enum value 'TaskFailedToStart' and tasks[].stoppedReason begins with the fixed token 'CannotPullContainerError' (Smithy: ecs#Task.stopCode -> ecs#TaskStopCode, values TaskFailedToStart | EssentialContainerExited | UserInitiated | ServiceSchedulerInitiated | SpotInterruption | TerminationNotice | InfrastructureHealth; Task.stopCode doc: "The stop code indicating why a task was stopped. The stoppedReason might contain additional details", referring to the Stopped tasks error codes guide). The stoppedReason message is 'CannotPullContainerError: ' followed by the variable registry/daemon detail taken from the failed pull of the request's image — the fixed part that a client matches on is the leading 'CannotPullContainerError' token, not the trailing detail of any one run. The service keeps desiredCount 1 with runningCount 0, and the attached target group never reports a target in state 'healthy'; the same task definition launched in a public subnet with assignPublicIp ENABLED reaches lastStatus 'RUNNING' (recorded behavior only for that control arm).
Emulator: the task does reach lastStatus 'STOPPED', but it is classified as stopCode 'ServiceSchedulerInitiated' — the value AWS uses for a task the ECS scheduler deliberately stopped (a scale-in, a deployment replacement) — so the egress-less placement is not modelled as a start/pull failure at all. The check fails on the first classification assertion, so the stoppedReason prefix is not reached; the emulator's reason string does not announce CannotPullContainerError either.
Why: a client diagnosing why a service serves no traffic branches on stopCode. 'TaskFailedToStart' plus a 'CannotPullContainerError' reason tells it the placement/networking is wrong (no registry egress) and that retrying the same subnets is futile; 'ServiceSchedulerInitiated' tells it the opposite — that ECS itself asked the task to stop, i.e. a scaling or deployment action — so automation (and any runbook or alarm keyed on start failures) concludes the service is behaving normally and never surfaces the missing NAT/IGW route. Blue/green and rollout tooling that aborts on TaskFailedToStart never aborts.
Fix: the ECS task lifecycle in the service scheduler/state that transitions a placed task to STOPPED: classify a task that never reached RUNNING because its image could not be pulled (no route from its awsvpc subnets to the registry) as stopCode TaskFailedToStart and set stoppedReason to a 'CannotPullContainerError: ...' message, reserving ServiceSchedulerInitiated for tasks the scheduler itself stops; DescribeTasks then projects those two members.
Traces: ec2__ecs__elbv2.ecsnt-scaling-the-isolated-service-up-stops-its-task-before-any-target-registers.

## Evidence
Check ec2__ecs__elbv2.ecsnt-scaling-the-isolated-service-up-stops-its-task-before-any-target-registers, line 63 of the check:
  assert task["stopCode"] == "TaskFailedToStart", f"stopCode: {task.get('stopCode')!r}"
  AssertionError: stopCode: 'ServiceSchedulerInitiated'

Expected (AWS): DescribeTasks on the service's first STOPPED task returns stopCode 'TaskFailedToStart' and stoppedReason starting with 'CannotPullContainerError'; DescribeServices then shows (desiredCount, runningCount) == (1, 0) and DescribeTargetHealth never reports state 'healthy'; the public-subnet twin of the same task definition reaches 'RUNNING'.
Actual (emulator): stopCode 'ServiceSchedulerInitiated' for a task that never ran, so the pull failure is not represented; the later stoppedReason / desired-running / target-health assertions are never reached.
(Cluster, service, task and subnet ids in the run are per-run values and not part of the requirement; the requirement is the enum value and the 'CannotPullContainerError' prefix.)

## Steps to Reproduce
Replay the behavioral suite and re-run ec2__ecs__elbv2.ecsnt-scaling-the-isolated-service-up-stops-its-task-before-any-target-registers. Manually: (1) create a VPC with only isolated subnets (no internet gateway / NAT route) and an ECS cluster; (2) register a Fargate task definition whose container image lives in a public registry (e.g. public.ecr.aws/docker/library/nginx) and create a service on it with desiredCount 0, attached to an ALB target group; (3) UpdateService desiredCount=1; (4) poll ListTasks(desiredStatus=STOPPED) + DescribeTasks until a task reports lastStatus STOPPED and read stopCode/stoppedReason — AWS gives 'TaskFailedToStart' and a stoppedReason beginning 'CannotPullContainerError', the emulator gives 'ServiceSchedulerInitiated'; (5) as the control, RunTask the same definition in a subnet routed 0.0.0.0/0 to an IGW with assignPublicIp ENABLED and confirm it reaches RUNNING.
