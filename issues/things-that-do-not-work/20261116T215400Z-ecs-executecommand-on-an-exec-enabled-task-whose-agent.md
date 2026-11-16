Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.execute-command-unknown-task
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.execute-command-unknown-task|run#8|AssertionError|assert real_error == _EXEC_NOT_AVAILABLE, f"expected {_EXEC_NOT_AVAILABLE!r}, got {real_error!r}"
Issue-Commit: 8ff3b21e7fb9cd785286bd581017af494993989c

# ExecuteCommand on an exec-enabled task whose agent is not running answers TargetNotConnectedException instead of InvalidParameterException with the documented not-enabled/agent-not-running message

> _Note: this issue was identified against the code base at commit `8ff3b21e7fb9cd785286bd581017af494993989c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteCommand

## Description
Cause 1 of 1: ExecuteCommand's terminal refusal for an exec-enabled task that has no connected exec agent uses the wrong error shape AND the wrong message, so a client cannot distinguish "retry/relaunch with exec enabled" from "the service had an internal problem".

AWS: Smithy com.amazonaws.ecs#ExecuteCommand models the errors AccessDeniedException (403), ClientException (400), ClusterNotFoundException (400), InvalidParameterException (400), ServerException (500) and TargetNotConnectedException (400). For a task that exists, is reachable in the named cluster, and whose execute-command agent has not reached RUNNING (or exec was not turned on at launch), AWS refuses the call with HTTP 400 InvalidParameterException and the fixed message text "The execute command failed because execute command was not enabled when the task was run or the execute command agent isn't running. Wait and try again or run a new task with execute command enabled and try again." That message is entirely fixed text — it interpolates no request or state values. TargetNotConnectedException is reserved by the model for the distinct configuration-issue class it documents ("Incorrect IAM permissions / The SSM agent is not installed or is not running / There is an interface Amazon VPC endpoint for Amazon ECS, but there is not one for Systems Manager Session Manager"); it is not AWS's answer for an agent that simply has not connected yet.

Emulator: for exactly that case it answers HTTP 400 TargetNotConnectedException with the message "The execute command failed due to an internal error. Try again later." (also entirely fixed text). Replay detail: `AssertionError: expected ('InvalidParameterException', "The execute command failed because execute command was not enabled when the task was run or the execute command agent isn't running. Wait and try again or run a new task with execute command enabled and try again."), got ('TargetNotConnectedException', 'The execute command failed due to an internal error. Try again later.')`.

Why: a client that branches on the error code sees a code AWS never returns here, and a message that says "internal error, try again later" instead of the one that tells it exec was not enabled or the agent is not up. Retry/backoff logic keyed on InvalidParameterException never fires, and a human reading the emulated message is pointed at a transient server fault rather than at the task's exec configuration. The check's other assertions (the fabricated-task-id refusal and the DescribeTasks MISSING failure) are unreachable because this one fires first, so the whole negative contract for ExecuteCommand is unobservable.

Fix: the ExecuteCommand handler's refusal path and its message constants — the branch that runs after the cluster, task, container and enable-execute-command preconditions pass must raise the modeled InvalidParameterException with the not-enabled/agent-not-running text, reserving TargetNotConnectedException for the configuration classes the model documents.

Traces: ecs.execute-command-unknown-task. (Related but distinct from the open issue 20261104T062603Z-ecs-exec-the-executecommandagent-managed-agent-never-reaches.md, which covers the agent never reaching RUNNING and the resulting timeouts; that issue does not own this check and does not describe the wrong error code/message returned on the refusal path. No other open or fixed issue mentions TargetNotConnectedException.)

## Evidence
Expected (AWS, per the Smithy error list for com.amazonaws.ecs#ExecuteCommand and its documented refusal): HTTP 400, Error.Code = "InvalidParameterException", Error.Message = "The execute command failed because execute command was not enabled when the task was run or the execute command agent isn't running. Wait and try again or run a new task with execute command enabled and try again." (all fixed text, no interpolated values).

Actual (emulator): HTTP 400, Error.Code = "TargetNotConnectedException", Error.Message = "The execute command failed due to an internal error. Try again later." (all fixed text).

Replay output for ecs.execute-command-unknown-task:
  File "check_ecs_execute_command_unknown_task.py", line 47, in run
    assert real_error == _EXEC_NOT_AVAILABLE, f"expected {_EXEC_NOT_AVAILABLE!r}, got {real_error!r}"
  AssertionError: expected ('InvalidParameterException', "The execute command failed because execute command was not enabled when the task was run or the execute command agent isn't running. Wait and try again or run a new task with execute command enabled and try again."), got ('TargetNotConnectedException', 'The execute command failed due to an internal error. Try again later.')

Because this fires on the first ExecuteCommand assertion, the check never reaches its later assertions that ExecuteCommand on a fabricated task id is refused InvalidParameterException "The specified task was not found. Specify a valid task ID and try again." and that DescribeTasks reports the fabricated id with failures[0].reason == "MISSING"; those remain ungraded.

## Steps to Reproduce
1. CreateCluster.
2. Create an IAM role trusted by ecs-tasks.amazonaws.com and RegisterTaskDefinition with networkMode=awsvpc, requiresCompatibilities=[FARGATE], cpu=256, memory=512, taskRoleArn set, one essential container named "app".
3. RunTask with launchType=FARGATE, count=1, enableExecuteCommand=true and an awsvpcConfiguration naming a subnet and security group; wait until DescribeTasks resolves the returned taskArn.
4. Call ExecuteCommand(cluster=<cluster ARN>, task=<that task ARN>, container="app", command="/bin/sh", interactive=true).
5. Observe the refusal: the emulator answers TargetNotConnectedException / "The execute command failed due to an internal error. Try again later.", where AWS answers InvalidParameterException / "The execute command failed because execute command was not enabled when the task was run or the execute command agent isn't running. Wait and try again or run a new task with execute command enabled and try again."
