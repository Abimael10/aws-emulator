Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3__sns__sqs.s3-object-created-notifies-sns
Issue-Key: s3__sns__sqs|SendMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Commit: 2a513259435d6dcec5aa5a741411e3a8a3522aaf

# s3__sns__sqs: SendMessage on a deleted queue carries the legacy WSDL wording of the NonExistentQueue message

> _Note: this issue was identified against the code base at commit `2a513259435d6dcec5aa5a741411e3a8a3522aaf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendMessage

## Description
Cause 1 of 1: SendMessage against a deleted queue is refused with the pre-2023 query/WSDL-era message sentence instead of AWS's current QueueDoesNotExist message. The check uses the refusal to prove the queue that captured the S3->SNS notification has been removed. Failing check: s3__sns__sqs.s3-object-created-notifies-sns.

AWS:      Smithy (com.amazonaws.sqs): error shape `QueueDoesNotExist`, errorKind=client, httpError=400, awsQueryError code `AWS.SimpleQueueService.NonExistentQueue` (httpResponseCode 400), single member `message`; doc: "Ensure that the QueueUrl is correct and that the queue has not been deleted." The message string is recorded behavior only: AWS answers HTTP 400, Code `AWS.SimpleQueueService.NonExistentQueue`, Message `The specified queue does not exist.` The entire sentence is FIXED text, with nothing interpolated from the request or state.

Emulator: HTTP 400 and the error code are right; the Message is `The specified queue does not exist for this wsdl version.` — the obsolete sentence with the extra ` for this wsdl version` suffix.

Why:      a client that matches AWS's exact not-found sentence to confirm a queue is gone cannot match it, and the wording falsely suggests a legacy SQS API version is being served.

Fix:      the SQS service's shared QueueDoesNotExist/NonExistentQueue error construction used by SendMessage and its sibling queue handlers — emit the fixed sentence `The specified queue does not exist.`, leaving HTTP 400 and the error code unchanged. (Same root cause as the open issue 20261115T161642Z-cloudcontrol-sqs-setqueueattributes-on-a-deleted-queue-is.md; filed separately because this check is listed under a different service heading.)

Traces:   s3__sns__sqs.s3-object-created-notifies-sns.

## Evidence
s3__sns__sqs.s3-object-created-notifies-sns:
  AssertionError: SendMessage <queue>: AWS.SimpleQueueService.NonExistentQueue message is 'The specified queue does not exist for this wsdl version.', expected 'The specified queue does not exist.'

Expected: HTTP 400, code 'AWS.SimpleQueueService.NonExistentQueue', Message 'The specified queue does not exist.' (fixed text, no variable parts).

## Steps to Reproduce
1. Create a queue subscribed to the topic an S3 bucket notification publishes to.
2. aws sqs delete-queue --queue-url <url>
3. aws sqs send-message --queue-url <url> --message-body gone
4. Observe Message 'The specified queue does not exist for this wsdl version.' instead of AWS's 'The specified queue does not exist.'
