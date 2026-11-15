Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation.stack-deploys-real-queue cloudformation.stack-deploys-real-fifo-queue-that-deduplicates-and-orders cloudformation.update-stack-adds-then-removes-a-real-queue cloudformation.update-stack-replaces-a-renamed-queue-and-updates-visibility-in-place
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation.stack-deploys-real-queue|_assert_queue_gone#18|AssertionError|assert got == gone, f"SendMessage to the deleted queue: expected {gone}, got {got!r}"
Issue-Key: cloudformation.stack-deploys-real-fifo-queue-that-deduplicates-and-orders|_assert_queue_gone#21|AssertionError|assert got == gone, f"SendMessage to the deleted queue: expected {gone}, got {got!r}"
Issue-Key: cloudformation.update-stack-adds-then-removes-a-real-queue|_assert_queue_gone#27|AssertionError|assert refused.response["Error"]["Message"] == _NO_SUCH_QUEUE_MESSAGE, (
Issue-Key: cloudformation.update-stack-replaces-a-renamed-queue-and-updates-visibility-in-place|_assert_queue_gone#20|AssertionError|assert got == gone, f"SendMessage to the deleted queue: expected {gone}, got {got!r}"
Issue-Commit: 7159003333263c232287cf539a8a92dd610dcec6

# cloudformation: SQS calls on a queue a stack deleted are refused with the legacy WSDL wording of NonExistentQueue instead of AWS's "The specified queue does not exist."

> _Note: this issue was identified against the code base at commit `7159003333263c232287cf539a8a92dd610dcec6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendMessage

## Description
One cause (Cause 1 of 1), reached by four CloudFormation lifecycle checks that prove a queue is gone after a stack delete / replacement.

Cause 1 of 1: the QueueDoesNotExist refusal SQS serves for a queue CloudFormation deleted carries an extra legacy clause in its message. Failing checks: cloudformation.stack-deploys-real-queue, cloudformation.stack-deploys-real-fifo-queue-that-deduplicates-and-orders, cloudformation.update-stack-adds-then-removes-a-real-queue, cloudformation.update-stack-replaces-a-renamed-queue-and-updates-visibility-in-place

AWS: Smithy (com-amazonaws-sqs-amazonsqs): QueueDoesNotExist is errorKind client with httpError 400 and awsQueryError {code: AWS.SimpleQueueService.NonExistentQueue, httpResponseCode: 400}; its documentation is "Ensure that the QueueUrl is correct and that the queue has not been deleted." Recorded behavior: for a request naming a queue that does not exist (including one just deleted), AWS answers HTTP 400, error code AWS.SimpleQueueService.NonExistentQueue and the message is exactly the fixed sentence "The specified queue does not exist." — the message has no variable parts: no queue name, url, account or wsdl/API version is interpolated into it.

Emulator: the status (400) and the code (AWS.SimpleQueueService.NonExistentQueue) are correct, but the message is "The specified queue does not exist for this wsdl version." — AWS's fixed sentence with the trailing legacy query-protocol clause " for this wsdl version" appended before the period. That wording belongs to much older SQS API versions and is not what the current API returns.

Why: a client that asserts on, logs or branches on the refusal message — exactly what a post-delete "the queue really is gone" probe does after DeleteStack or a replacing UpdateStack — sees a message it cannot match, so a correct deletion reads as an unexpected failure. The code and status are right, so the divergence is invisible to code that only inspects the code, and silently wrong for everything that reads the message.

Fix: the shared QueueDoesNotExist refusal construction in the sqs queue-lookup path (the message text used by SendMessage and the other queue-targeted operations), not the status or error code.

Note: several open issues already describe this same legacy wording for other services' checks — e.g. 20261115T171209Z-sqs-the-queuedoesnotexist-refusal-carries-the-legacy-wsdl.md, 20261115T165530Z-cloudformation-sns-sqs-sendmessage-getqueueattributes-on-a-deleted.md and 20261115T165715Z-cloudformation-sqs-ssm-sqs-s-queuedoesnotexist-refusal-keeps.md — but none of them carries the four cloudformation check ids listed above, so those checks are not covered by any filed issue.

Traces: the four failing checks above; each fails only on the message text, after the stack lifecycle itself behaved correctly.

## Evidence
cloudformation.stack-deploys-real-queue, [mismatch]:
AssertionError: SendMessage to the deleted queue: expected ('AWS.SimpleQueueService.NonExistentQueue', 'The specified queue does not exist.'), got ('AWS.SimpleQueueService.NonExistentQueue', 'The specified queue does not exist for this wsdl version.')

cloudformation.stack-deploys-real-fifo-queue-that-deduplicates-and-orders, [mismatch]: identical expected/actual pair.

cloudformation.update-stack-replaces-a-renamed-queue-and-updates-visibility-in-place, [mismatch]: identical expected/actual pair (on the replaced queue's old url).

cloudformation.update-stack-adds-then-removes-a-real-queue, [mismatch]:
AssertionError: expected the message 'The specified queue does not exist.', got 'The specified queue does not exist for this wsdl version.'

Expected (AWS): HTTP 400, Error.Code AWS.SimpleQueueService.NonExistentQueue, Error.Message "The specified queue does not exist." (entirely fixed text).
Actual (emulator): HTTP 400, same code, Error.Message "The specified queue does not exist for this wsdl version."

## Steps to Reproduce
1. CreateStack with a template declaring one AWS::SQS::Queue; wait for CREATE_COMPLETE and read the queue url from the stack output.
2. DeleteStack and wait for DELETE_COMPLETE (or UpdateStack so the queue is removed / renamed so it is replaced).
3. Call sqs SendMessage against the deleted queue's url.
4. Expected: 400 AWS.SimpleQueueService.NonExistentQueue with message "The specified queue does not exist." Observed: the same status and code with message "The specified queue does not exist for this wsdl version."
5. The same divergence appears on the FIFO variant and on the old url of a queue replaced by a renaming update.
