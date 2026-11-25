Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ecr__ecs.ecsdeploy-isolated-subnet-task-blocked-from-starting
Issue-Key: ec2__ecr__ecs.ecsdeploy-isolated-subnet-task-blocked-from-starting|run#23|AssertionError|assert ctl_state["lastStatus"] == "RUNNING", (
Issue-Commit: f520e482964e34c3af4b1cbe916bb0f71c86032d

# ec2__ecr__ecs: the routed positive-control task of the isolated-subnet differential stops immediately as EssentialContainerExited instead of running

> _Note: this issue was identified against the code base at commit `f520e482964e34c3af4b1cbe916bb0f71c86032d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Cause 1 of 1: the control task placed in a subnet that DOES have egress stops at once, so the isolated-subnet negative case cannot be told apart from it.
AWS: Smithy (ecs): Task.lastStatus reaches RUNNING for a task in a routed subnet whose essential container runs a long-lived command; Task.stopCode (enum TaskStopCode) is EssentialContainerExited only when the essential container actually ran and exited, and TaskFailedToStart when it could not be started (which is the code AWS uses for a task whose image pull is blocked by a missing route).
Emulator: the routed control task is reported lastStatus='STOPPED' with stopCode='EssentialContainerExited' immediately, so both legs of the differential stop the same way and the blocked-by-isolation behaviour is unobservable.
Why: a client diagnosing 'my task cannot start' cannot distinguish a networking problem from an application exit; stopCode carries the wrong classification.
Fix: the RunTask handler and the task state machine that advances Task.lastStatus and classifies stopCode/stoppedReason.
Traces: ec2__ecr__ecs.ecsdeploy-isolated-subnet-task-blocked-from-starting.

## Evidence
AssertionError: the routed control task must run; it reached 'STOPPED' stopCode='EssentialContainerExited'. Expected lastStatus 'RUNNING' for the routed task (and stopCode TaskFailedToStart, not EssentialContainerExited, for the isolated one).

## Steps to Reproduce
1. Create a VPC with one routed (IGW/NAT) subnet and one isolated subnet; create an ECR repository and push an image.
2. RegisterTaskDefinition (FARGATE, awsvpc) with a long-running command; RunTask once in the routed subnet and once in the isolated subnet.
3. DescribeTasks: the routed control reports STOPPED/EssentialContainerExited instead of RUNNING.
