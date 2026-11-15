Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudcontrol__sqs.cloudcontrol-reads-and-writes-through-to-a-natively-created-queue
Issue-Key: cloudcontrol__sqs.cloudcontrol-reads-and-writes-through-to-a-natively-created-queue|run#34|AssertionError|assert mutation_error == NON_EXISTENT_QUEUE, (
Issue-Commit: e35c80769d1da1628850625a18b81ebe6990b182

# cloudcontrol__sqs: SetQueueAttributes on a deleted queue is refused with the legacy WSDL wording of the NonExistentQueue message

> _Note: this issue was identified against the code base at commit `e35c80769d1da1628850625a18b81ebe6990b182`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetQueueAttributes

## Description
Cause 1 of 1: a mutating SQS call against a queue that Cloud Control has just deleted is refused with the pre-2023 query/WSDL-era message text instead of AWS's current QueueDoesNotExist message. Failing check: cloudcontrol__sqs.cloudcontrol-reads-and-writes-through-to-a-natively-created-queue. (Same root cause as the `cloudcontrol`-scoped issue filed in this pass for GetQueueAttributes/ReceiveMessage/GetQueueUrl/SendMessage; it is filed separately because the suite lists this check under a different service heading.)

AWS:      Smithy (com.amazonaws.sqs): error shape `QueueDoesNotExist`, errorKind=client, httpError=400, awsQueryError code `AWS.SimpleQueueService.NonExistentQueue` (httpResponseCode 400), single member `message`; doc: "Ensure that the QueueUrl is correct and that the queue has not been deleted." The message string itself is recorded behavior only: AWS answers HTTP 400, Code `AWS.SimpleQueueService.NonExistentQueue`, Message `The specified queue does not exist.` The entire sentence is FIXED text; nothing from the request (queue name, URL, account id) or from state is interpolated into it.

Emulator: status 400 and code `AWS.SimpleQueueService.NonExistentQueue` are correct, but SetQueueAttributes on the deleted queue answers Message `The specified queue does not exist for this wsdl version.` — the obsolete sentence with the extra ` for this wsdl version` suffix.

Why:      this is the negative control that proves a write which demonstrably worked through Cloud Control a moment earlier now fails. A client (or test) that matches AWS's exact not-found sentence to confirm the delete propagated to the data plane cannot confirm it, and any log/alert keyed on that message text never fires; it also falsely suggests a legacy SQS API version is being served.

Fix:      the SQS service's shared QueueDoesNotExist/NonExistentQueue error construction used by the mutating queue handlers (SetQueueAttributes and its siblings) — emit the fixed sentence `The specified queue does not exist.`, leaving status 400 and the error code unchanged.

Traces:   cloudcontrol__sqs.cloudcontrol-reads-and-writes-through-to-a-natively-created-queue (write-through check: native CreateQueue -> Cloud Control Get/List/Update -> Cloud Control DeleteResource -> SetQueueAttributes must be refused as NonExistentQueue).

## Evidence
cloudcontrol__sqs.cloudcontrol-reads-and-writes-through-to-a-natively-created-queue:
  AssertionError: a SetQueueAttributes on the deleted queue answered
    ('AWS.SimpleQueueService.NonExistentQueue', 'The specified queue does not exist for this wsdl version.'),
  expected
    ('AWS.SimpleQueueService.NonExistentQueue', 'The specified queue does not exist.')

Status and code match AWS; only the message diverges. Fixed text per AWS: 'The specified queue does not exist.' — no variable parts.

## Steps to Reproduce
1. sqs create-queue --queue-name <name> (natively, not through Cloud Control).
2. Confirm Cloud Control sees it: cloudcontrol get-resource --type-name AWS::SQS::Queue --identifier <queue-url>.
3. cloudcontrol delete-resource --type-name AWS::SQS::Queue --identifier <queue-url>, and wait for OperationStatus=SUCCESS.
4. aws sqs set-queue-attributes --queue-url <queue-url> --attributes VisibilityTimeout=75
5. Observe the refusal carries Message 'The specified queue does not exist for this wsdl version.' instead of AWS's 'The specified queue does not exist.' (code AWS.SimpleQueueService.NonExistentQueue, HTTP 400, both correct).
