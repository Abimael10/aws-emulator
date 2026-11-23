Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__logs.log-cwl-role-without-logs-permission-keeps-the-tail-and-creates-no-group
Issue-Key: lambda__logs.log-cwl-role-without-logs-permission-keeps-the-tail-and-creates-no-group|_poll_until#11|Timeout|last = probe()
Issue-Commit: ddd7f54964d818ea183d1c749ef5ca4926ba59fa

# lambda__logs: the no-logs-permission scenario never completes — the Tail-plus-twin log-delivery probe stalls with no verdict

> _Note: this issue was identified against the code base at commit `ddd7f54964d818ea183d1c749ef5ca4926ba59fa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Cause 1 of 1: the check produces no verdict at all — it is still waiting when its budget expires.

AWS: recorded behavior only. Two functions are created from the same code, one with a bare execution role (no CloudWatch Logs permission) and one with AWSLambdaBasicExecutionRole. Invoke(LogType=Tail) on both returns {"ok": true} and a LogResult holding the full record set for that invocation (START RequestId: <rid> Version: $LATEST, the handler's print line, and a REPORT RequestId: <rid>\t... line); the twin's print then lands in /aws/lambda/<twin> within a bounded delivery latency, while no /aws/lambda/<bare> group is ever created because the role grants nothing. All of those observations complete in seconds on AWS.

Emulator: the run stalls — the replay reports `Error: Check timed out after 60.0s` with no assertion reached, so the scenario never gets as far as a comparable value. Hypothesis: the stall is in the bounded wait for the twin's print to appear in /aws/lambda/<twin> (the only unbounded-in-practice poll in the body, since a missing delivery can never satisfy it), which would match the already-open lambda log-delivery causes; the Invoke/Tail leg before it could equally be the one not returning. Either way the emulator never reaches the state the recording reaches.

Why: a client exercising the ordinary "invoke with LogType=Tail, then read the function's log group" loop gets neither the result nor a failure — it hangs; and the account-level claim this check guards (a role without logs permission must not cause a log group to be created) cannot be observed at all.

Fix: the Lambda invocation path and its CloudWatch Logs delivery for a function with a default log group — whichever of Invoke(LogType=Tail) or the /aws/lambda/<fn> delivery never settles must complete; no call in this scenario may hang.

Related: open issues 20261107T143806Z-lambda-logs-2-net-new-causes-delivered-log.md and 20261107T035448Z-lambda-logs-3-net-new-causes-loggingconfig-loggroup.md own other lambda__logs delivery checks (custom log groups, LoggingConfig); this check carries neither of their replay keys and uses the default group with a bare role.

Traces: lambda__logs.log-cwl-role-without-logs-permission-keeps-the-tail-and-creates-no-group.

## Evidence
Expected: both Invoke(LogType=Tail) calls return {"ok": True} with a Tail containing START/print/REPORT for the returned RequestId; the basic-role twin's print appears in /aws/lambda/<twin>; /aws/lambda/<bare> never appears.
Actual (replay): `Error: Check timed out after 60.0s` — the body never reached an assertion, so no response value was produced to compare. The 60 s figure is this run's budget, not a requirement; the defect is that the scenario does not settle.

## Steps to Reproduce
1. Create two IAM roles trusting lambda.amazonaws.com — one bare, one with arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole.
2. CreateFunction twice (python3.12, handler printing "out-<marker>" and returning {"ok": true}), one per role.
3. Invoke each with LogType=Tail and decode LogResult; expect START/print/REPORT lines for the returned RequestId.
4. Poll DescribeLogGroups/FilterLogEvents for the basic-role function's print in /aws/lambda/<fn>, then check that /aws/lambda/<bare fn> is never created. The run never gets through these steps.
