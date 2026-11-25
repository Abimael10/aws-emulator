Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__stepfunctions.task-token-heartbeat-timeout-catcher-order-decides-winner
Issue-Key: lambda__stepfunctions.task-token-heartbeat-timeout-catcher-order-decides-winner|run#7|AssertionError|assert out_heartbeat_first == "heartbeat-timeout-catcher-won", (
Issue-Commit: 9b644f2075576c170e34ded6fb832e3c5e4e0784

# lambda__stepfunctions: a waitForTaskToken heartbeat lapse ignores Catch declaration order — the States.Timeout catcher wins even when States.HeartbeatTimeout is listed first

> _Note: this issue was identified against the code base at commit `9b644f2075576c170e34ded6fb832e3c5e4e0784`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
Cause 1 of 1: when a Task using arn:aws:states:::lambda:invoke.waitForTaskToken lapses its HeartbeatSeconds, the emulator resolves the state's Catch array by something other than declaration order — it routes to the States.Timeout catcher even when a States.HeartbeatTimeout catcher is listed FIRST in the same array.
Failing check id: lambda__stepfunctions.task-token-heartbeat-timeout-catcher-order-decides-winner.

AWS: recorded behavior only for the Amazon States Language semantics — the Smithy model (services/service-com-amazonaws-sfn-awsstepfunctions/smithy-model/com-amazonaws-sfn-awsstepfunctions.json) types `definition` as an opaque string (CreateStateMachine.definition, @length 1..1048576) and says nothing about Catch resolution. The recorded behavior, matching the Step Functions error-handling documentation (https://docs.aws.amazon.com/step-functions/latest/dg/concepts-error-handling.html and state-task.html), is twofold: (a) a Task that fails to send a heartbeat within HeartbeatSeconds raises an error that matches BOTH the error name States.HeartbeatTimeout AND the broader name States.Timeout (States.Timeout covers 'the Task ran longer than TimeoutSeconds OR failed to send a heartbeat for more than HeartbeatSeconds'); (b) the Catch field is an ORDERED array — the interpreter scans the catchers in the order they appear and applies the FIRST one whose ErrorEquals matches, never the most specific name. So for Catch [{States.HeartbeatTimeout -> A},{States.Timeout -> B}] the heartbeat lapse goes to A, and for Catch [{States.Timeout -> B},{States.HeartbeatTimeout -> A}] the identical lapse goes to B. In both cases the matched catcher lets the execution finish SUCCEEDED with the chosen branch's output.

Emulator: with the heartbeat-first definition the execution does end SUCCEEDED, but its output is "timeout-catcher-won" — the SECOND-listed States.Timeout catcher — instead of "heartbeat-timeout-catcher-won". The heartbeat clock itself fires and a catcher does match, so only the selection among matching catchers is wrong: the emulator appears to pick a catcher by error-name preference (or by a fixed internal order) rather than by position in the Catch array. The check asserts the heartbeat-first case first, so the symmetric Timeout-first leg was never reached.

Why: a client relying on AWS's behavior gets its workflow silently routed down the wrong branch with no error anywhere. Catch order is the documented and only way to give a heartbeat lapse different compensation from a plain task timeout (e.g. re-dispatch the worker on States.HeartbeatTimeout but abandon the item on States.Timeout). On this emulator the more specific, first-listed handler is dead code: the execution still reports SUCCEEDED, so nothing signals the misrouting — the only evidence is wrong output and wrong side effects downstream.

Fix: the interpreter's Catch matcher (the error-handling component that maps a raised state error to a catcher) — it must iterate the state's Catch array in declaration order and select the first catcher whose ErrorEquals contains any name the raised error matches, with a HeartbeatSeconds lapse matching both States.HeartbeatTimeout and States.Timeout. Related but NOT covering this check (the replay report marked it neither ALREADY FILED nor OWNED): 20261110T140310Z-stepfunctions-5-net-new-causes-activity-backed-task.md, whose Cause 1 covers ACTIVITY-backed tasks never handing out a token at all, so its catcher-order check never reached a matching catcher; here the token path works (lambda:invoke.waitForTaskToken) and the ordering itself is observably wrong.

Traces: lambda__stepfunctions.task-token-heartbeat-timeout-catcher-order-decides-winner.

## Evidence
Check lambda__stepfunctions.task-token-heartbeat-timeout-catcher-order-decides-winner, verdict mismatch.

Setup performed by the check: an IAM role trusting lambda.amazonaws.com (allowing states:SendTaskHeartbeat, states:SendTaskSuccess) and a python3.12 function whose handler deliberately never calls SendTaskHeartbeat/SendTaskSuccess on the token it receives; an IAM role trusting states.amazonaws.com allowing lambda:InvokeFunction; two STANDARD state machines over the SAME definition shape, differing only in Catch order:
  WaitForToken: {Type: Task, Resource: "arn:aws:states:::lambda:invoke.waitForTaskToken", Parameters: {FunctionName: <fn>, Payload: {"token.$": "$$.Task.Token"}}, HeartbeatSeconds: 5, Retry: [{ErrorEquals: [States.TaskFailed], IntervalSeconds: 2, MaxAttempts: 5, BackoffRate: 2.0}], Catch: [{ErrorEquals: [<first>], Next: ...}, {ErrorEquals: [<second>], Next: ...}], End: true}
with the two Pass targets returning the literals "heartbeat-timeout-catcher-won" and "timeout-catcher-won".

Expected (AWS): StartExecution on the machine whose Catch lists States.HeartbeatTimeout FIRST ends SUCCEEDED with output "heartbeat-timeout-catcher-won"; StartExecution on the machine whose Catch lists States.Timeout FIRST ends SUCCEEDED with output "timeout-catcher-won". Same raised error, same two catchers — only array position decides.

Actual (emulator): the first leg ends SUCCEEDED but with the wrong branch's output. The harness reports:
  File "check_lambda__stepfunctions_task_token_heartbeat_timeout_catcher_order_decides_winner.py", line 101, in run
    assert out_heartbeat_first == "heartbeat-timeout-catcher-won"
  AssertionError: with States.HeartbeatTimeout listed first in Catch, the execution output was 'timeout-catcher-won', expected the first-listed catcher to win
The second leg (States.Timeout listed first) is never reached.

## Steps to Reproduce
1. iam:CreateRole trusting lambda.amazonaws.com with an inline policy allowing states:SendTaskHeartbeat and states:SendTaskSuccess on "*"; lambda:CreateFunction(Runtime=python3.12, Handler=index.handler, Timeout=30, MemorySize=128) whose code is `def handler(event, context): return {'received': True, 'token_present': 'token' in event}` — i.e. it never calls back on the task token.
2. iam:CreateRole trusting states.amazonaws.com with an inline policy allowing lambda:InvokeFunction on "*".
3. sfn:CreateStateMachine (type=STANDARD) with definition {"StartAt":"WaitForToken","States":{"WaitForToken":{"Type":"Task","Resource":"arn:aws:states:::lambda:invoke.waitForTaskToken","Parameters":{"FunctionName":"<fn-arn>","Payload":{"token.$":"$$.Task.Token"}},"HeartbeatSeconds":5,"Retry":[{"ErrorEquals":["States.TaskFailed"],"IntervalSeconds":2,"MaxAttempts":5,"BackoffRate":2.0}],"Catch":[{"ErrorEquals":["States.HeartbeatTimeout"],"Next":"CaughtByStatesHeartbeatTimeout"},{"ErrorEquals":["States.Timeout"],"Next":"CaughtByStatesTimeout"}],"End":true},"CaughtByStatesHeartbeatTimeout":{"Type":"Pass","Result":"heartbeat-timeout-catcher-won","End":true},"CaughtByStatesTimeout":{"Type":"Pass","Result":"timeout-catcher-won","End":true}}}.
4. sfn:CreateStateMachine a second machine with the SAME definition except the two Catch entries swapped (States.Timeout listed first).
5. sfn:StartExecution on the first machine with input {} and poll sfn:DescribeExecution until terminal.
   Expected: status SUCCEEDED, output "heartbeat-timeout-catcher-won".
   Observed: status SUCCEEDED, output "timeout-catcher-won".
6. sfn:StartExecution on the second machine with input {} and poll until terminal.
   Expected: status SUCCEEDED, output "timeout-catcher-won".
