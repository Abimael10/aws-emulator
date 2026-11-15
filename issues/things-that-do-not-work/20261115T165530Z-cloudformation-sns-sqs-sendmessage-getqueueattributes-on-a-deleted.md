Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__sns__sqs.cfn-independent-branches-provision-concurrently-and-tear-down-in-reverse cloudformation__sns__sqs.deploy-sns-subscription-missing-protocol-rejected
Issue-Key: cloudformation__sns__sqs.cfn-independent-branches-provision-concurrently-and-tear-down-in-reverse|run#55|AssertionError|assert send_code == _NO_SUCH_QUEUE, (
Issue-Key: sqs|GetQueueAttributes|AWS.SimpleQueueService.NonExistentQueue
Issue-Commit: 2a513259435d6dcec5aa5a741411e3a8a3522aaf

# cloudformation__sns__sqs: SendMessage/GetQueueAttributes on a deleted queue carry the legacy WSDL wording of the NonExistentQueue message

> _Note: this issue was identified against the code base at commit `2a513259435d6dcec5aa5a741411e3a8a3522aaf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendMessage

## Description
Cause 1 of 1: an SQS call against a queue a stack has deleted (or that a rejected stack never created) is refused with the pre-2023 query/WSDL-era message sentence instead of AWS's current QueueDoesNotExist message. Both failing checks use that refusal as the negative control that the CloudFormation teardown/rollback really removed the queue. Failing checks: cloudformation__sns__sqs.cfn-independent-branches-provision-concurrently-and-tear-down-in-reverse (SendMessage), cloudformation__sns__sqs.deploy-sns-subscription-missing-protocol-rejected (GetQueueAttributes).

AWS:      Smithy (com.amazonaws.sqs): error shape `QueueDoesNotExist`, errorKind=client, httpError=400, awsQueryError code `AWS.SimpleQueueService.NonExistentQueue` (httpResponseCode 400), single member `message`; doc: "Ensure that the QueueUrl is correct and that the queue has not been deleted." The message string is recorded behavior only: AWS answers HTTP 400, Code `AWS.SimpleQueueService.NonExistentQueue`, Message `The specified queue does not exist.` The entire sentence is FIXED text; nothing from the request or from state is interpolated into it.

Emulator: HTTP 400 and the error code are right, but the Message is `The specified queue does not exist for this wsdl version.` — the obsolete sentence with the extra ` for this wsdl version` suffix. It is the same wording on both operations, so the wrong sentence comes from one shared error construction rather than from either handler.

Why:      a client (or a stack test) that matches AWS's exact not-found sentence to prove a delete/rollback propagated to the data plane cannot do so; anything keyed on that message text never matches, and the wording falsely suggests a legacy SQS API version is being served.

Fix:      the SQS service's shared QueueDoesNotExist/NonExistentQueue error construction used by the queue handlers (SendMessage, GetQueueAttributes and their siblings) — emit the fixed sentence `The specified queue does not exist.`, leaving HTTP 400 and the error code unchanged. (Same root cause as the open issue 20261115T161642Z-cloudcontrol-sqs-setqueueattributes-on-a-deleted-queue-is.md, which carries only a `cloudcontrol__sqs` check; filed separately here because these checks are listed under a different service heading.)

Traces:   cloudformation__sns__sqs.cfn-independent-branches-provision-concurrently-and-tear-down-in-reverse, cloudformation__sns__sqs.deploy-sns-subscription-missing-protocol-rejected.

## Evidence
cfn-independent-branches-provision-concurrently-and-tear-down-in-reverse:
  AssertionError: SendMessage against deleted UrlA returned ('AWS.SimpleQueueService.NonExistentQueue', 'The specified queue does not exist for this wsdl version.'), expected ('AWS.SimpleQueueService.NonExistentQueue', 'The specified queue does not exist.')

deploy-sns-subscription-missing-protocol-rejected:
  AssertionError: expected ('AWS.SimpleQueueService.NonExistentQueue', 'The specified queue does not exist.'), got ('AWS.SimpleQueueService.NonExistentQueue', 'The specified queue does not exist for this wsdl version.')

Expected in both: HTTP 400, code 'AWS.SimpleQueueService.NonExistentQueue', Message 'The specified queue does not exist.' — the whole sentence is fixed text.

## Steps to Reproduce
1. Deploy a stack containing an AWS::SQS::Queue, then delete the stack (or let a rejected template roll back).
2. aws sqs send-message --queue-url <the removed queue's url> --message-body gone
3. aws sqs get-queue-attributes --queue-url <the removed queue's url> --attribute-names Policy
4. Observe both refusals carry Message 'The specified queue does not exist for this wsdl version.' instead of AWS's 'The specified queue does not exist.'
