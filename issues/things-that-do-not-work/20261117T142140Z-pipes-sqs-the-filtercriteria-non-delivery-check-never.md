Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: pipes__sqs.pipe-filter-criteria-non-matching-not-delivered
Issue-Key: pipes__sqs.pipe-filter-criteria-non-matching-not-delivered|_collect#31|Timeout|resp = sqs.receive_message(
Issue-Commit: 845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab

# pipes__sqs: the FilterCriteria non-delivery check never reaches a verdict — a filtered SQS-source pipe's control delivery and source drain are unobservable

> _Note: this issue was identified against the code base at commit `845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreatePipe

## Description
One net-new root cause.

Cause 1 of 1: an EventBridge Pipes pipe with a FilterCriteria over an SQS source does not reach the state the check needs within its own budgets, so neither the matching delivery nor the non-matching suppression can be graded.
AWS: pipes#CreatePipe takes a SourceParameters.FilterCriteria; the pipe converges to CurrentState 'RUNNING', the matching source message is delivered to the target as the SQS record envelope (keys including attributes, awsRegion, body, eventSource, eventSourceARN, md5OfBody, messageAttributes, messageId, receiptHandle) whose messageId and md5OfBody equal what SendMessage returned, the non-matching message is NOT delivered to the target, and BOTH messages are consumed from the source queue so it drains to ApproximateNumberOfMessages 0 and ApproximateNumberOfMessagesNotVisible 0.
Emulator: the check does not complete — 'Error: Check timed out after 60.0s'. The check's own waits are generous (DescribePipe -> RUNNING with timeout_s=240.0, the control record reaching the target with timeout_s=240.0, the source drained to zero with timeout_s=180.0), so no assertion in it was evaluated. Hypothesis: the pipe does not deliver the matching control record to the target (or does not converge to RUNNING) within the window, so the run dies on one of the first two waits; the rest of the pipes__sqs suite passes, so the failure is specific to the filtered-source path.
Why: a client that relies on FilterCriteria to suppress records cannot confirm either half of the contract: that the matching record arrives with the documented SQS envelope and the sender's messageId/md5OfBody, or that the suppressed record is still consumed from the source rather than left to redeliver forever. A pipe that delivers nothing at all and a pipe that filters correctly are indistinguishable.
Fix: the Pipes source poller and filter stage for an SQS source (the FilterCriteria evaluation and the delivery of the matching record to the target, plus the acknowledgement/deletion of the filtered-out record from the source queue) and the pipe's convergence to CurrentState RUNNING.
Traces: pipes__sqs.pipe-filter-criteria-non-matching-not-delivered. Related but NOT covering this check: open issue 20261115T190747Z-pipes-the-sqs-queuedoesnotexist-refusal-reached-through-the.md concerns the SQS QueueDoesNotExist refusal text reached through the pipes lifecycle, not the filtered-delivery path.

## Evidence
Check pipes__sqs.pipe-filter-criteria-non-matching-not-delivered (verdict: mismatch), the only failure under this heading (8/9 of the heading's checks pass).

Expected (AWS): DescribePipe -> CurrentState 'RUNNING'; the matching message arrives at the target as the SQS record envelope with messageId/md5OfBody equal to the SendMessage response; the non-matching message never arrives at the target; the source queue drains to ApproximateNumberOfMessages '0' and ApproximateNumberOfMessagesNotVisible '0'.

Actual (emulator): `Error: Check timed out after 60.0s`. The check declares timeout_s=240.0 for the RUNNING wait, timeout_s=240.0 for the control record arriving at the target and timeout_s=180.0 for the source drain, so it is killed before any of its assertions run.

## Steps to Reproduce
Against the emulator's pipes/SQS endpoints (any region/account):
1. Create a source queue and a target queue; create an IAM role trusted by pipes.amazonaws.com with receive/delete on the source and sqs:SendMessage on the target.
2. CreatePipe(Name=<p>, Source=<source arn>, Target=<target arn>, RoleArn=<role>, SourceParameters={'FilterCriteria': {'Filters': [{'Pattern': '{"body":{"marker":["control"]}}'}]}}).
3. Poll DescribePipe(Name=<p>) until CurrentState == 'RUNNING' (AWS converges well inside 240 s).
4. SendMessage a non-matching body, then SendMessage a matching body with marker 'control'.
5. ReceiveMessage on the target: only the matching record must arrive, as the SQS record envelope whose messageId and md5OfBody equal the SendMessage response for the matching send.
6. GetQueueAttributes on the source for ApproximateNumberOfMessages and ApproximateNumberOfMessagesNotVisible: both must reach '0' — the filtered-out message is consumed, not left behind.
