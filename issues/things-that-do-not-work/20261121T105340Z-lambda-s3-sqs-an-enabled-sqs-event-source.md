Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__s3__sqs.sqs-esm-disabled-mapping-no-invocation-negative
Issue-Key: lambda__s3__sqs.sqs-esm-disabled-mapping-no-invocation-negative|run#18|Timeout|time.sleep(5)
Issue-Commit: 77bfd47bd7799ecf9c5ff0eea8e18d7399a37670

# lambda__s3__sqs: an ENABLED SQS event-source mapping never delivers its queued message, so the disabled-mapping scenario's calibration leg never completes

> _Note: this issue was identified against the code base at commit `77bfd47bd7799ecf9c5ff0eea8e18d7399a37670`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
Cause 1 of 1: an event-source mapping created with `Enabled=true` over an SQS queue does not invoke its function for a message put on that queue. The check's calibration leg — one message delivered through the ENABLED mapping, which is what makes a later absence meaningful — never completes, and the check is cut off at the harness's 60 s cap without reaching a verdict.

AWS: Smithy (lambda): `CreateEventSourceMapping` takes `EventSourceArn` (the queue ARN), `FunctionName`, `BatchSize`, `Enabled`; `GetEventSourceMapping` reports `State` (`Enabled`/`Disabled`). Recorded behaviour: a mapping created with `Enabled=true` reaches `State: "Enabled"` and then, for a message sent to the source queue, invokes the function (observable in the function's own side effect) within seconds; `UpdateEventSourceMapping(Enabled=false)` takes it to `State: "Disabled"` and, after the pollers stop, a message sent while disabled is NOT delivered and stays on the queue (`ApproximateNumberOfMessages >= 1`); re-enabling the same mapping resumes delivery of the still-queued message.

Emulator: the calibration message sent while the mapping is `Enabled` is never delivered. The check polls the function's report destination for it (90 s window) and the suite kills the check at its 60 s cap (`Check timed out after 60.0s`) before the calibration assertion is reached, so nothing past mapping creation is graded.

Why: SQS-triggered Lambda is one of the most common event wirings there is. A mapping that reports `Enabled` and polls nothing makes every queue-driven function silently inert, and it collapses the distinction the negative case depends on: `Enabled=false` delivering nothing is indistinguishable from `Enabled=true` delivering nothing.

Fix: the Lambda event-source-mapping poller for SQS sources (the component that, for a mapping in state Enabled, receives from the source queue, invokes the function with the SQS records event, and deletes the messages on success). The same behaviour is already filed under other service headings by open issues 20261118T062834Z-lambda-pipes-sqs-an-enabled-sqs-event-source.md and 20261118T150346Z-events-lambda-sqs-an-enabled-sqs-event-source-f5f960.md; neither carries this check's replay key.

Traces: lambda__s3__sqs.sqs-esm-disabled-mapping-no-invocation-negative.

## Evidence
Failing check `lambda__s3__sqs.sqs-esm-disabled-mapping-no-invocation-negative` [mismatch]:

    Error: Check timed out after 60.0s

The check's first graded step, after `_create_esm(..., enabled=True)` and `_wait_for_esm_state(..., "Enabled")`:
  `calib_reports = _poll_reports(ctx, bucket, "reports/", lambda reports: bool(_matches(reports, calib_marker)), timeout_s=90)`
  `assert _matches(calib_reports, calib_marker), "calibration message was never delivered while the ESM was enabled; an absence measured against this pipeline would prove nothing"`
and only then `update_event_source_mapping(Enabled=False)`, `_wait_for_esm_state(..., "Disabled")`,
  `assert not hits_while_disabled, "disabled ESM still invoked Lambda ..."`,
  `assert int(attrs["ApproximateNumberOfMessages"]) >= 1, "message vanished while ESM was disabled"`,
  `assert matched, "re-enabled ESM never delivered the previously queued message"`.

Expected (AWS): the calibration message is delivered through the enabled mapping inside the 90 s window.
Actual (emulator): no delivery is observed; the check makes no progress and is cut off at the 60 s cap.

## Steps to Reproduce
1. Create an SQS queue and a python3.12 Lambda function that records each received record to a destination it owns; wait for State=Active.
2. CreateEventSourceMapping(EventSourceArn=<queue arn>, FunctionName=<fn>, BatchSize=1, Enabled=true) and poll GetEventSourceMapping until State == 'Enabled'.
3. SendMessage a calibration marker to the queue and poll the function's destination for it. Expected: it arrives within seconds. Observed: never, and the check is killed at its 60 s cap.
4. (Blocked by 3) UpdateEventSourceMapping(Enabled=false), poll to State == 'Disabled', send a second marker, sample the destination repeatedly and assert the marker is never delivered and ApproximateNumberOfMessages >= 1.
5. (Blocked by 3) UpdateEventSourceMapping(Enabled=true), poll to State == 'Enabled', and assert the still-queued marker is then delivered.
