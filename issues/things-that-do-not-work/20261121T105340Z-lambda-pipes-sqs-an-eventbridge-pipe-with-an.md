Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__pipes__sqs.deploy-pipes-missing-role-negative
Issue-Key: lambda__pipes__sqs.deploy-pipes-missing-role-negative|run#18|Timeout|resp = sqs.receive_message(
Issue-Commit: 77bfd47bd7799ecf9c5ff0eea8e18d7399a37670

# lambda__pipes__sqs: an EventBridge Pipe with an SQS source never delivers its message to the Lambda target, so the positive-control leg of the missing-role scenario never completes

> _Note: this issue was identified against the code base at commit `77bfd47bd7799ecf9c5ff0eea8e18d7399a37670`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreatePipe

## Description
Cause 1 of 1: a correctly-permissioned EventBridge Pipe whose source is an SQS queue and whose target is a Lambda function never forwards a queued message to the target, so the check's positive control — which must deliver before the under-permissioned pipe's silence can mean anything — never completes and the check is cut off at the harness's 60 s cap without reaching a verdict.

AWS: Smithy (pipes): `CreatePipe` takes `Source` (the SQS queue ARN), `Target` (the Lambda function ARN) and `RoleArn`, and `DescribePipe` reports `CurrentState`. Recorded behaviour: a pipe created with a role granting `sqs:ReceiveMessage`/`sqs:DeleteMessage` on the source and `lambda:InvokeFunction` on the target reaches `RUNNING` and then, for each message sent to the source queue, invokes the target with that message, so a marker put on the queue is observed at the target within the check's delivery window; a pipe whose role lacks the source queue's poll permissions still reports a terminal startup state (`RUNNING`, `START_FAILED` or `CREATE_FAILED`) but delivers nothing and leaves the message on the queue (`ApproximateNumberOfMessages >= 1`).

Emulator: the stack deploys and the pipe is created, but the positive-control delivery is never observed. The check's first graded assertion — that the marker it put on the good pipe's source queue arrives at the target — is never reached inside the deadline; the suite reports `Check timed out after 60.0s` with no progress past that wait.

Why: a pipe that reports a healthy state but never moves a message makes every EventBridge Pipes workload silently inert, and it makes the negative case indistinguishable from the positive one: an application cannot tell "my pipe's role is wrong" from "pipes never deliver".

Fix: the EventBridge Pipes data plane — the poller that, for a pipe in RUNNING with an SQS source, receives from the source queue and invokes the configured target (and deletes the message only on success).

Traces: lambda__pipes__sqs.deploy-pipes-missing-role-negative.

## Evidence
Failing check `lambda__pipes__sqs.deploy-pipes-missing-role-negative` [mismatch]:

    Error: Check timed out after 60.0s

The check's first graded step is the positive control:
  `good_body = _poll_until(..., description="positive-control pipe delivery")`
  `assert good_body["marker"] == good_marker, "positive-control delivery carried the wrong marker: ..."`
only after which it creates the under-permissioned pipe and asserts
  `assert final_state in {"RUNNING", "START_FAILED", "CREATE_FAILED"}`,
  `assert bad_marker not in seen, "message was delivered despite the pipe lacking receive permission"`,
  `assert int(attrs["ApproximateNumberOfMessages"]) >= 1, "message unexpectedly left the source queue"`.

Expected (AWS): the well-permissioned pipe delivers its marker to the Lambda target inside the polling window, then the negative legs are graded.
Actual (emulator): no delivery is observed; the check makes no progress and is cut off at the 60 s cap.

## Steps to Reproduce
1. Deploy a stack containing: an SQS source queue, a Lambda target that records the message it was invoked with, a role granting sqs:ReceiveMessage/sqs:DeleteMessage on the queue and lambda:InvokeFunction on the target, and a second role lacking the queue's poll permissions.
2. CreatePipe with the good role, Source = the queue ARN, Target = the function ARN. Wait for DescribePipe CurrentState == RUNNING.
3. SendMessage a marker to the source queue and poll the target for that marker. Expected: the marker arrives. Observed: it never does, and the check is killed at its 60 s cap.
4. (Blocked by 3) CreatePipe with the bad role, send a second marker, and assert the pipe reaches a terminal startup state, the marker is never delivered, and ApproximateNumberOfMessages on its source queue stays >= 1.
