Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.deploy-ecs-cluster-cfn-runs-task ecs.execute-command-without-enable-flag-rejected ecs.put-cluster-capacity-providers-unassociated-provider-used-in-strategy-rejected ecs.run-task-with-inactive-taskdef-rejected ecs.container-depends-on-success-gates-dependent-start
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.deploy-ecs-cluster-cfn-runs-task|run#13|AssertionError|assert task["lastStatus"] == "RUNNING", f"task did not reach RUNNING: {task.get('stoppedReason')}"
Issue-Key: ecs.execute-command-without-enable-flag-rejected|_run_and_wait_running#14|AssertionError|raise AssertionError(
Issue-Key: ecs.put-cluster-capacity-providers-unassociated-provider-used-in-strategy-rejected|run#21|AssertionError|assert placed["lastStatus"] == "RUNNING", (
Issue-Key: ecs.run-task-with-inactive-taskdef-rejected|run#29|AssertionError|assert settled["lastStatus"] == "RUNNING", (
Issue-Key: ecs.container-depends-on-success-gates-dependent-start|run#14|AssertionError|assert _container(ok_task, "init").get("exitCode") == 0, _container(ok_task, "init")
Issue-Commit: 3b95f364a9766d54572805b5303a28b612a84a5b

# ecs: 2 net-new causes — a long-running Fargate container's task goes straight to STOPPED/EssentialContainerExited, and a non-essential init container reports exitCode 143 instead of its own exit status

> _Note: this issue was identified against the code base at commit `3b95f364a9766d54572805b5303a28b612a84a5b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Two net-new root causes in the ECS task data plane. Neither is listed as owned by the replay report; the open issues 20261111T145804Z-ecs-task-data-plane-container-commands-never-execute.md and 20261113T124512Z-ecs-8-net-new-causes-container-dependson-health.md are adjacent but do not carry these checks' replay keys, so they do not cover them.

Cause 1 of 4 (numbered as 1 of 2 causes in this issue): a Fargate task whose essential container runs a long-lived server process never reaches RUNNING.
AWS: RunTask(launchType=FARGATE) on a task definition whose essential container is a long-running server (public.ecr.aws/nginx/nginx:1.27, or busybox:1.36 running `httpd -f -p 80 -h /www`) moves the task PENDING -> RUNNING and it STAYS RUNNING until something stops it. Smithy: DescribeTasks -> Task.lastStatus ("the last known status for the task"), Task.stopCode (enum TaskStopCode: TaskFailedToStart, EssentialContainerExited, UserInitiated, ServiceSchedulerInitiated, SpotInterruption, TerminationNotice, InfrastructureHealth) and Task.stoppedReason are only populated once the task actually stops; stopCode=EssentialContainerExited with stoppedReason "Essential container in task exited" is the fixed AWS text for the case where the container's own process exited, which cannot happen while the server is still serving. Container.exitCode is "the exit code returned from the container" — i.e. the container's real process exit status.
Emulator: DescribeTasks reports lastStatus 'STOPPED' with stopCode 'EssentialContainerExited' and stoppedReason 'Essential container in task exited' within seconds of RunTask, for every such task — the container's command is never actually run to a serving state, so the task is retired immediately.
Why: every client that waits for a Fargate task to reach RUNNING (RunTask + DescribeTasks poll, ECS Exec, capacity-provider placement, CloudFormation-created clusters) sees an immediate failure instead of a running workload, and the stopCode/stoppedReason it reads attributes that failure to the customer's container image.
Fix: the ECS task lifecycle/data-plane simulation behind RunTask and DescribeTasks in the ecs service crate — the component that decides whether a container's process is still running and that sets Task.stopCode/stoppedReason.
Traces: ecs.deploy-ecs-cluster-cfn-runs-task, ecs.execute-command-without-enable-flag-rejected, ecs.put-cluster-capacity-providers-unassociated-provider-used-in-strategy-rejected, ecs.run-task-with-inactive-taskdef-rejected.

Cause 2 of 2: a non-essential init container reports exitCode 143 instead of the exit status of its own command, so a dependsOn SUCCESS gate can never be satisfied.
AWS: a container `init` (essential=false) with command ["sh","-c","sleep 3; exit <n>"] runs that command and reports Container.exitCode == <n> (0 for the success case, 1 for the failure case). A sibling `app` with dependsOn [{containerName: init, condition: SUCCESS}] then starts only when init exited zero; Smithy documents ContainerCondition SUCCESS as "the same as COMPLETE, but it also requires that the container exits with a zero status", and COMPLETE as "validates that a dependent container runs to completion (exits) before permitting other containers to start".
Emulator: the init container is reported lastStatus 'STOPPED' with exitCode 143 (SIGTERM — it was killed, not run to completion) even though its command would have exited 0, and the dependent container's own exit status is therefore never observable. exitCode 143 is never a value AWS produces for a command that exits 0.
Why: a client cannot distinguish a succeeded init container from a failed one, so container-ordering semantics (dependsOn SUCCESS/COMPLETE) silently behave as "dependency failed" for every workload that uses an init/sidecar pattern.
Fix: the same ECS task data-plane container-execution component: the container exit status recorded on Container.exitCode, and the dependsOn gating that consumes it.
Traces: ecs.container-depends-on-success-gates-dependent-start.

## Evidence
Cause 1 — expected: Task.lastStatus == 'RUNNING' while the nginx/busybox-httpd server runs.
Actual (ecs.deploy-ecs-cluster-cfn-runs-task): AssertionError: task did not reach RUNNING: Essential container in task exited.
Actual (ecs.put-cluster-capacity-providers-unassociated-provider-used-in-strategy-rejected): AssertionError: expected the FARGATE-provider task to reach RUNNING, got 'STOPPED' (stoppedReason='Essential container in task exited').
Actual (ecs.run-task-with-inactive-taskdef-rejected): AssertionError: the positive-control task on a fresh ACTIVE revision never reached RUNNING (stopped early: 'Essential container in task exited').
Actual (ecs.execute-command-without-enable-flag-rejected): AssertionError: task never reached RUNNING (last status 'STOPPED'); cannot exercise ExecuteCommand.
Fixed text in the diverging response: stopCode 'EssentialContainerExited' and stoppedReason 'Essential container in task exited' (AWS's own wording, emitted in a state where AWS emits neither); the task/cluster ARNs and the image reference are per-run values.

Cause 2 — expected: container 'init' (command `sh -c "sleep 3; exit 0"`) reports exitCode 0 and the dependent 'app' then reports exitCode 5.
Actual (ecs.container-depends-on-success-gates-dependent-start): AssertionError: {'name': 'init', 'image': 'public.ecr.aws/docker/library/busybox:1.36', 'lastStatus': 'STOPPED', 'exitCode': 143, 'healthStatus': 'UNKNOWN', ...} — exitCode 143 where 0 is required. The ARNs/IPs in that payload are per-run values; the requirement is exitCode == the command's own exit status.

## Steps to Reproduce
Cause 1: CreateCluster; RegisterTaskDefinition(family=..., requiresCompatibilities=[FARGATE], networkMode=awsvpc, cpu=256, memory=512, containerDefinitions=[{name:app, image:public.ecr.aws/nginx/nginx:1.27, essential:true}]); RunTask(cluster, taskDefinition, launchType=FARGATE, count=1, networkConfiguration={awsvpcConfiguration:{subnets:[<subnet>], assignPublicIp:ENABLED}}); poll DescribeTasks until lastStatus is RUNNING or STOPPED. Observe STOPPED with stopCode EssentialContainerExited instead of RUNNING.

Cause 2: RegisterTaskDefinition with two containers — init (essential=false, command ["sh","-c","sleep 3; exit 0"]) and app (essential=true, command ["sh","-c","exit 5"], dependsOn [{containerName:init, condition:SUCCESS}]) — then RunTask(FARGATE) and poll DescribeTasks until the task is STOPPED. Read the containers array: init reports exitCode 143 instead of 0, and app's exitCode is not 5.
