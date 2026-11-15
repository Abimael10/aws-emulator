Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudcontrol.concurrent-delete-same-identifier-race cloudcontrol.delete-nonexistent-resource-failed-request-ordering-preserved cloudcontrol.deploy-cfn-resource-behaves cloudcontrol.e2e-create-update-delete-consequence-flow cloudcontrol.e2e-create-use-observe-delete cloudcontrol.e2e-update-changes-runtime-behavior
Issue-Shape-Id: com.amazonaws.cloudcontrol#CloudApiService
Issue-Key: cloudcontrol.concurrent-delete-same-identifier-race|run#33|AssertionError|assert native_message == "The specified queue does not exist.", (
Issue-Key: cloudcontrol.delete-nonexistent-resource-failed-request-ordering-preserved|run#8|AssertionError|assert pre_error == _NO_QUEUE, f"expected the bogus queue URL to not exist before the test, got {pre_error!r}"
Issue-Key: cloudcontrol.deploy-cfn-resource-behaves|run#26|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: cloudcontrol.e2e-create-update-delete-consequence-flow|run#32|AssertionError|assert native_message == "The specified queue does not exist.", (
Issue-Key: cloudcontrol.e2e-create-use-observe-delete|run#42|AssertionError|assert sqs_message == expected_message, f"expected the message {expected_message!r}, got {sqs_message!r}"
Issue-Key: cloudcontrol|SendMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Commit: e35c80769d1da1628850625a18b81ebe6990b182

# sqs: the QueueDoesNotExist refusal message is the legacy WSDL wording ("...does not exist for this wsdl version."), breaking every post-delete Cloud Control flow

> _Note: this issue was identified against the code base at commit `e35c80769d1da1628850625a18b81ebe6990b182`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetQueueAttributes

## Description
Cause 1 of 1: every SQS operation that fails to resolve a queue URL/name answers with the pre-2023 query-protocol message text instead of AWS's current QueueDoesNotExist message. The six failing checks below are all Cloud Control flows that, after deleting (or never creating) a queue, confirm the ground truth directly against SQS; each one gets the right error code and the wrong message, so the flow fails on the message assertion only.

AWS:      Smithy (com.amazonaws.sqs): error shape `QueueDoesNotExist`, errorKind=client, httpError=400, awsQueryError code `AWS.SimpleQueueService.NonExistentQueue` (httpResponseCode 400), single member `message`; doc: "Ensure that the QueueUrl is correct and that the queue has not been deleted." The model does not carry the message string, so for the text this is recorded behavior only: AWS answers HTTP 400, Code `AWS.SimpleQueueService.NonExistentQueue`, Message `The specified queue does not exist.` That whole sentence is FIXED text — this error has NO variable parts (no queue name, url, account id or request id is interpolated into it).

Emulator: the HTTP status (400) and the error code (`AWS.SimpleQueueService.NonExistentQueue`) are correct, but the message is `The specified queue does not exist for this wsdl version.` — AWS's old query/WSDL-era sentence, with the trailing ` for this wsdl version` that current SQS no longer emits. Observed on every queue-resolution failure the checks exercise: GetQueueAttributes (cloudcontrol.concurrent-delete-same-identifier-race, cloudcontrol.delete-nonexistent-resource-failed-request-ordering-preserved, cloudcontrol.deploy-cfn-resource-behaves, cloudcontrol.e2e-create-update-delete-consequence-flow), ReceiveMessage (cloudcontrol.e2e-create-use-observe-delete), and SendMessage/GetQueueUrl on a deleted or never-created queue (cloudcontrol.e2e-update-changes-runtime-behavior). The wording is uniform, so it is one shared error constructor, not a per-operation slip.

Why:      the error message is part of the contract for this error class. A client that asserts on, logs, or pattern-matches SQS's not-found sentence (test harnesses, retry/negative-path assertions, CloudFormation and Cloud Control integration suites that use native SQS as ground truth after a delete) sees a sentence AWS never produces, and any exact-match check on the deletion having taken effect fails even though the queue really is gone. It also misleads a reader into thinking the emulator is serving a legacy SQS API version.

Fix:      the SQS service's shared QueueDoesNotExist/NonExistentQueue error construction — the single place the queue-resolution handlers (GetQueueAttributes, SetQueueAttributes, SendMessage, ReceiveMessage, DeleteMessage, GetQueueUrl, …) build that refusal — must emit the fixed sentence `The specified queue does not exist.` with no suffix, keeping status 400 and code `AWS.SimpleQueueService.NonExistentQueue` unchanged.

Traces:   the six behavioral checks listed in trace_ids; each asserts the exact message after a Cloud Control / CloudFormation delete of an AWS::SQS::Queue. Note: the two open NonExistentQueue issues (20261111T033241Z-events-eventbridge-sqs-target-dead-letters-misclassify-an-unauthorized.md and 20261115T150513Z-cloudwatch-events-s3-sqs-an-eventbridge-sqs-target.md) are about EventBridge dead-letter ERROR_CODE/ERROR_MESSAGE classification and do NOT cover this message text.

## Evidence
cloudcontrol.deploy-cfn-resource-behaves:
  expected message 'The specified queue does not exist.', got 'The specified queue does not exist for this wsdl version.'
  (sqs.get_queue_attributes(QueueUrl=<url of queue whose owning CFN stack was deleted>, AttributeNames=['QueueArn']))

cloudcontrol.delete-nonexistent-resource-failed-request-ordering-preserved:
  expected ('AWS.SimpleQueueService.NonExistentQueue', 'The specified queue does not exist.'),
  got     ('AWS.SimpleQueueService.NonExistentQueue', 'The specified queue does not exist for this wsdl version.')

cloudcontrol.concurrent-delete-same-identifier-race:
  AssertionError: expected SQS's NonExistentQueue message, got 'The specified queue does not exist for this wsdl version.'

cloudcontrol.e2e-create-update-delete-consequence-flow:
  AssertionError: expected native SQS's exact not-found message, got 'The specified queue does not exist for this wsdl version.'

cloudcontrol.e2e-create-use-observe-delete (ReceiveMessage):
  AssertionError: expected the message 'The specified queue does not exist.', got 'The specified queue does not exist for this wsdl version.'

cloudcontrol.e2e-update-changes-runtime-behavior:
  AssertionError: The specified queue does not exist for this wsdl version.

Expected in all cases: HTTP 400, Code 'AWS.SimpleQueueService.NonExistentQueue', Message 'The specified queue does not exist.' (fixed text, no interpolated values).

## Steps to Reproduce
1. Create an SQS queue (natively, via Cloud Control CreateResource on AWS::SQS::Queue, or via a CloudFormation stack).
2. Delete it (DeleteQueue, Cloud Control DeleteResource, or delete the owning stack) — or simply build a queue URL for a name that was never created.
3. Call any queue-resolving SQS operation on that URL, e.g.
     aws sqs get-queue-attributes --queue-url <deleted-url> --attribute-names QueueArn
   also reproducible with receive-message, send-message, set-queue-attributes, and get-queue-url on the missing name.
4. Observe the refusal: status 400 and code AWS.SimpleQueueService.NonExistentQueue are correct, but Message is 'The specified queue does not exist for this wsdl version.' instead of AWS's 'The specified queue does not exist.'
