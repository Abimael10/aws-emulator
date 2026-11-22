Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: pipes.e2e-pipe-source-to-target-delivery-and-cleanup
Issue-Shape-Id: com.amazonaws.pipes#Pipes
Issue-Key: pipes.e2e-pipe-source-to-target-delivery-and-cleanup|_assert_queue_gone#36|AssertionError|assert refused[2] == expected_message, f"expected the message {expected_message!r}, got {refused[2]!r}"
Issue-Commit: 302c678b7901f99ac739cc877697e5b939709ef8

# pipes: the SQS QueueDoesNotExist refusal reached through the pipes end-to-end lifecycle carries the legacy WSDL message ("...does not exist for this wsdl version.")

> _Note: this issue was identified against the code base at commit `302c678b7901f99ac739cc877697e5b939709ef8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetQueueUrl

## Description
Cause 1 of 1: the SQS queue-resolution refusal served to the pipes end-to-end lifecycle check uses the retired query/WSDL-era sentence instead of AWS's current one. The pipes e2e check creates a source and a target queue, runs a full pipe lifecycle over them, then deletes both queues and proves each one gone by calling GetQueueUrl on the deleted name and SendMessage on the deleted URL. Both calls are correctly refused with HTTP 400 and code AWS.SimpleQueueService.NonExistentQueue, but the message text diverges, so the cleanup-proof leg of the check fails even though the queues really are gone. Failing check id: pipes.e2e-pipe-source-to-target-delivery-and-cleanup.

AWS:      Smithy (com.amazonaws.sqs): error shape `QueueDoesNotExist`, errorKind=client, httpError=400, awsQueryError code `AWS.SimpleQueueService.NonExistentQueue` (httpResponseCode 400), single member `message` targeting `ExceptionMessage`; doc: "Ensure that the QueueUrl is correct and that the queue has not been deleted." The model does not carry the message string, so for the text this is recorded behavior only: AWS answers HTTP 400, Code `AWS.SimpleQueueService.NonExistentQueue`, Message `The specified queue does not exist.` That entire sentence is FIXED text for this error class — it interpolates NO variable parts at all: no queue name, queue URL, ARN, account id or request id appears in it. The same fixed sentence is served whether the queue was deleted, never created, or addressed through another region's endpoint.

Emulator: HTTP 400 and code `AWS.SimpleQueueService.NonExistentQueue` are correct, but the message is `The specified queue does not exist for this wsdl version.` — AWS's retired WSDL-era sentence with the extra trailing ` for this wsdl version`. Observed on GetQueueUrl for a deleted queue name (the assertion that failed) and, in the same check, on SendMessage to the deleted queue URL.

Why:      a client that proves a queue is gone by matching SQS's not-found sentence — teardown/cleanup assertions, negative-path suites, and any eventing plumbing (Pipes, EventBridge, Lambda ESM) that embeds the SQS refusal into a dead-letter record or a surfaced cause string — sees a sentence AWS never produces. The exact-match check fails even though the status and code are right, and the wording falsely signals that the emulator is serving a legacy SQS API version.

Fix:      the SQS service's shared QueueDoesNotExist / NonExistentQueue error construction — the single place the queue-resolution path of the SQS handlers builds this refusal, which GetQueueUrl, SendMessage and every other queue-resolving operation share — must emit the fixed sentence `The specified queue does not exist.` with no suffix, leaving HTTP 400 and code `AWS.SimpleQueueService.NonExistentQueue` unchanged.

NOTE on existing issues: the open issue 20261115T171209Z-sqs-the-queuedoesnotexist-refusal-carries-the-legacy-wsdl.md describes the identical wording defect, but it is grounded in a different set of checks (it carries dynamodb__pipes__sqs.pipes-dynamodb-stream-source-preserves-per-key-change-order under the pipes-adjacent headings, not this one) and does not carry the replay key for pipes.e2e-pipe-source-to-target-delivery-and-cleanup, which is why that check is still failing and unowned. Fixing the shared constructor named above should close both; this issue exists so the pipes e2e check is re-replayed as part of the verification.

Traces:   pipes.e2e-pipe-source-to-target-delivery-and-cleanup (failing at the _assert_queue_gone leg, GetQueueUrl on the deleted source queue).

## Evidence
Expected (AWS): HTTP 400, Code 'AWS.SimpleQueueService.NonExistentQueue', Message 'The specified queue does not exist.' — the whole sentence is fixed text, no values from the request or state are interpolated.
Actual (emulator): HTTP 400, Code 'AWS.SimpleQueueService.NonExistentQueue', Message 'The specified queue does not exist for this wsdl version.'

pipes.e2e-pipe-source-to-target-delivery-and-cleanup:
  File "check_pipes_e2e_pipe_source_to_target_delivery_and_cleanup.py", line 99, in run
    _assert_queue_gone(ctx, url, arn.rsplit(":", 1)[-1])
  File "check_pipes_e2e_pipe_source_to_target_delivery_and_cleanup.py", line 161, in _assert_queue_gone
    assert refused[2] == expected_message, f"expected the message {expected_message!r}, got {refused[2]!r}"
  AssertionError: expected the message 'The specified queue does not exist.', got 'The specified queue does not exist for this wsdl version.'

The check reaches the same wrong sentence twice in _assert_queue_gone: once via GetQueueUrl(QueueName=<deleted name>) and once via SendMessage(QueueUrl=<deleted url>). Status (400) and error code (AWS.SimpleQueueService.NonExistentQueue) match AWS in both; only the message diverges, by the trailing ' for this wsdl version'.

Smithy citation used: com.amazonaws.sqs#QueueDoesNotExist — errorKind=client, httpError=400, awsQueryError {code: AWS.SimpleQueueService.NonExistentQueue, httpResponseCode: 400}, member message -> ExceptionMessage.

## Steps to Reproduce
1. Create an SQS queue: aws sqs create-queue --queue-name sim2real-pipes-e2e-src-<suffix>
2. Note its name and URL, then delete it: aws sqs delete-queue --queue-url <url>
3. Prove it gone the way the pipes e2e cleanup leg does:
     aws sqs get-queue-url --queue-name sim2real-pipes-e2e-src-<suffix>
     aws sqs send-message --queue-url <deleted-url> --message-body x
4. Both are refused with HTTP 400 and code AWS.SimpleQueueService.NonExistentQueue (correct), but the Message is 'The specified queue does not exist for this wsdl version.' instead of AWS's fixed sentence 'The specified queue does not exist.'
5. Equivalently, replay the behavioral check pipes.e2e-pipe-source-to-target-delivery-and-cleanup: the full create/start/deliver/stop/delete lifecycle passes and the assertion fails only on this message text in _assert_queue_gone.
