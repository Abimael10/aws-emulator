Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr__ecs__elbv2.ecs-execution-role-trust-policy-allows-ecs-tasks ecr__ecs__elbv2.ecs-task-launch-fails-without-ecs-tasks-trust
Issue-Key: ecr__ecs__elbv2.ecs-execution-role-trust-policy-allows-ecs-tasks|run#11|AssertionError|assert task["lastStatus"] == "RUNNING", (
Issue-Key: ecr__ecs__elbv2.ecs-task-launch-fails-without-ecs-tasks-trust|run#15|AssertionError|assert good_task["lastStatus"] == "RUNNING", (
Issue-Commit: f520e482964e34c3af4b1cbe916bb0f71c86032d

# ecr__ecs__elbv2: a Fargate task whose container runs a long-lived command goes straight to STOPPED/"Essential container in task exited", so both ecs-tasks trust differentials have no positive control

> _Note: this issue was identified against the code base at commit `f520e482964e34c3af4b1cbe916bb0f71c86032d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Cause 1 of 1: a Fargate task launched from a task definition whose single essential container runs a long-lived command (the amazonlinux:2023 image with a sleep-style command) never reaches lastStatus RUNNING. It is reported STOPPED with stoppedReason "Essential container in task exited" immediately after creation, so every check that needs a live task as its positive control — here the two IAM execution-role-trust differentials — can never reach a verdict, and the trust-policy behaviour they are testing is never exercised.
Failing checks: ecr__ecs__elbv2.ecs-execution-role-trust-policy-allows-ecs-tasks, ecr__ecs__elbv2.ecs-task-launch-fails-without-ecs-tasks-trust

AWS: Smithy (services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/smithy-model/com-amazonaws-ecs-amazonec2containerservicev20141113.json) — Task.lastStatus is documented as "The last known status for the task" over the documented Task Lifecycle (PROVISIONING -> PENDING -> ACTIVATING -> RUNNING -> DEACTIVATING -> STOPPING -> STOPPED), Task.startedAt is "the time when the task transitioned from the PENDING state to the RUNNING state", and Task.stoppedReason is "The reason that the task was stopped" with Task.stopCode classifying it (TaskStopCode enum: TaskFailedToStart, EssentialContainerExited, UserInitiated, ServiceSchedulerInitiated, SpotInterruption, TerminationNotice). The model states no exit rule, so the lifecycle itself is recorded behavior only: a task whose essential container runs a command that does not exit (sleep <n>) settles at lastStatus RUNNING with desiredStatus RUNNING, a startedAt timestamp, its container lastStatus RUNNING and no stoppedReason/stopCode, and stays there until something stops it. "Essential container in task exited" (stopCode EssentialContainerExited) is the message AWS words for a task whose essential container actually TERMINATED — its fixed text is exactly that sentence; it is not emitted for a container that is still supposed to be running, and AWS reports the container's real exitCode with it.

Emulator: the task is created and then immediately settles to lastStatus STOPPED / desiredStatus STOPPED with stoppedReason "Essential container in task exited", the container lastStatus STOPPED with exitCode 0, and (in the awsvpc case) its ElasticNetworkInterface attachment already status DELETED — within about a second of createdAt, although the container's command is a long sleep that never exits. DescribeTasks therefore never returns a RUNNING task at all. Replay: AssertionError "a task using an execution role that trusts ecs-tasks.amazonaws.com did not reach RUNNING: lastStatus=STOPPED stoppedReason=Essential container in task exited" and, on the negative check's positive control, AssertionError "positive control: a task using a correctly-trusted execution role did not reach RUNNING: lastStatus=STOPPED stoppedReason=Essential container in task exited". Hypothesis: the task state machine runs a single synchronous transition to STOPPED instead of holding RUNNING for the lifetime of the container process, so the container's command is never actually kept alive.

Why: a client cannot run anything on ECS. Every workload that depends on a task staying up — a service reaching steady state, a target registering with a load balancer, logs or metrics being produced, an IAM denial being observed from inside the task — is unobservable, and the stop record it does get is actively misleading: a caller diagnosing a crash-looping container reads "Essential container in task exited" with exitCode 0 for a container that never ran, and will look for a bug in its own image.

Fix: the ECS task lifecycle component — the task/container state machine behind RunTask and DescribeTasks (the task-state advance that fabricates the terminal STOPPED transition), plus the stop-record population that attaches stoppedReason/stopCode/exitCode. A task whose essential container's command does not terminate must settle at RUNNING with startedAt set and no stop record, and EssentialContainerExited must only be recorded when the container process really exited, carrying that process's exit code.

Traces: ecr__ecs__elbv2.ecs-execution-role-trust-policy-allows-ecs-tasks and ecr__ecs__elbv2.ecs-task-launch-fails-without-ecs-tasks-trust, both under the `### ecr__ecs__elbv2` heading of the replay report; neither is marked OWNED or ALREADY FILED. Related open issues that describe the same immediate-exit behavior but do NOT carry these checks' replay keys: 20261118T145001Z-ecs-2-net-new-causes-a-long-running.md, 20261118T134608Z-ec2-ecs-elbv2-a-fargate-task-whose-subnet.md and 20261120T071621Z-cloudwatch-ec2-ecs-2-net-new-causes-an.md.

## Evidence
Replay of behavioral suite simparity-behavioral-aws-v1 against the booted emulator (519/647 passed, bootstrap_signal false). Service heading `### ecr__ecs__elbv2`: 9/17 (53%), 6 failed; the two checks below carry no OWNED / ALREADY FILED marker.

[mismatch] ecr__ecs__elbv2.ecs-execution-role-trust-policy-allows-ecs-tasks
  File "check_ecr__ecs__elbv2_ecs_execution_role_trust_policy_allows_ecs_tasks.py", line 42, in run
    assert task["lastStatus"] == "RUNNING", (
  AssertionError: a task using an execution role that trusts ecs-tasks.amazonaws.com did not reach RUNNING: lastStatus=STOPPED stoppedReason=Essential container in task exited

[mismatch] ecr__ecs__elbv2.ecs-task-launch-fails-without-ecs-tasks-trust
  File "check_ecr__ecs__elbv2_ecs_task_launch_fails_without_ecs_tasks_trust.py", line 50, in run
    assert good_task["lastStatus"] == "RUNNING", (
  AssertionError: positive control: a task using a correctly-trusted execution role did not reach RUNNING: lastStatus=STOPPED stoppedReason=Essential container in task exited

Expected (AWS): a Fargate task from a task definition with one essential container (image public.ecr.aws/amazonlinux/amazonlinux:2023, a long sleep command) and an execution role trusting ecs-tasks.amazonaws.com reaches lastStatus RUNNING / desiredStatus RUNNING with startedAt set, its container lastStatus RUNNING, and no stoppedReason, stopCode or exitCode; it stays RUNNING until stopped.
Actual (emulator): DescribeTasks reports lastStatus STOPPED, desiredStatus STOPPED, stoppedReason "Essential container in task exited", container lastStatus STOPPED with exitCode 0, and the awsvpc ElasticNetworkInterface attachment already status DELETED, about one second after createdAt. The arns, eni/mac/ip values and timestamps in the replay are per-run values; the defect is that the task never holds RUNNING and that a stop record is fabricated for a container that never exited.

Smithy consulted: ECS Task.lastStatus / Task.desiredStatus / Task.startedAt / Task.stoppedReason / Task.stopCode docs and the TaskStopCode enum (EssentialContainerExited among them); the model declares no container-exit rule, so the lifecycle requirement is recorded behavior.

## Steps to Reproduce
1. git_clone the repo and replay the behavioral suite (simparity-behavioral-aws-v1); both checks in trace_ids reproduce under the `### ecr__ecs__elbv2` heading.
2. CreateCluster; create an IAM role whose trust policy allows Principal {Service: ecs-tasks.amazonaws.com} to sts:AssumeRole and attach arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy.
3. RegisterTaskDefinition(networkMode='awsvpc', requiresCompatibilities=['FARGATE'], cpu='256', memory='512', executionRoleArn=<that role>, containerDefinitions=[{name:'app', image:'public.ecr.aws/amazonlinux/amazonlinux:2023', command:['sleep','3600'], essential:true}]).
4. RunTask(launchType='FARGATE', networkConfiguration.awsvpcConfiguration={subnets:[<subnet>], securityGroups:[<sg>], assignPublicIp:'ENABLED'}).
5. Poll DescribeTasks on the returned taskArn.
   Expected: lastStatus advances PROVISIONING/PENDING -> RUNNING and stays RUNNING, with startedAt set and no stoppedReason/stopCode/exitCode.
   Observed: lastStatus is STOPPED within ~1 s, desiredStatus STOPPED, stoppedReason 'Essential container in task exited', container exitCode 0, ENI attachment status DELETED.
6. Re-replay both check ids in trace_ids to confirm the fix.
