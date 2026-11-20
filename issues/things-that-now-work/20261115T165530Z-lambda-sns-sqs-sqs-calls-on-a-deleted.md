Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__sns__sqs.pipeline-sns-sqs-lambda lambda__sns__sqs.sns-to-sqs-subscription-basic lambda__sns__sqs.sns-notification-config-sqs-destination-stale-arn-fails
Issue-Key: lambda__sns__sqs|SendMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Key: lambda__sns__sqs.sns-notification-config-sqs-destination-stale-arn-fails|run#22|AssertionError|assert message == "The specified queue does not exist.", f"got message {message!r}"
Issue-Commit: 2a513259435d6dcec5aa5a741411e3a8a3522aaf

# lambda__sns__sqs: SQS calls on a deleted queue carry the legacy WSDL wording of the NonExistentQueue message

> _Note: this issue was identified against the code base at commit `2a513259435d6dcec5aa5a741411e3a8a3522aaf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendMessage

## Description
Cause 1 of 1: an SQS call against a deleted queue is refused with the pre-2023 query/WSDL-era message sentence instead of AWS's current QueueDoesNotExist message. All three checks assert on that sentence — two as the negative control that the pipeline's queue is gone, and one as the refusal a stale SQS subscription destination must produce. Failing checks: lambda__sns__sqs.pipeline-sns-sqs-lambda, lambda__sns__sqs.sns-to-sqs-subscription-basic, lambda__sns__sqs.sns-notification-config-sqs-destination-stale-arn-fails.

AWS:      Smithy (com.amazonaws.sqs): error shape `QueueDoesNotExist`, errorKind=client, httpError=400, awsQueryError code `AWS.SimpleQueueService.NonExistentQueue` (httpResponseCode 400), single member `message`; doc: "Ensure that the QueueUrl is correct and that the queue has not been deleted." The message string is recorded behavior only: AWS answers HTTP 400, Code `AWS.SimpleQueueService.NonExistentQueue`, Message `The specified queue does not exist.` The entire sentence is FIXED text; no queue name, URL or account id is interpolated into it.

Emulator: HTTP 400 and the error code are right on every one of them; the Message is `The specified queue does not exist for this wsdl version.` — the obsolete sentence with the extra ` for this wsdl version` suffix. The identical wording across three checks and several operations shows it comes from one shared error construction.

Why:      a client that matches AWS's exact not-found sentence to tell "queue deleted" from any other 400 cannot match it; alert and retry rules keyed on that text never fire, and the wording falsely suggests a legacy SQS API version is being served.

Fix:      the SQS service's shared QueueDoesNotExist/NonExistentQueue error construction used by the queue handlers (SendMessage and siblings) — emit the fixed sentence `The specified queue does not exist.`, leaving HTTP 400 and the error code unchanged. (Same root cause as the open issue 20261115T161642Z-cloudcontrol-sqs-setqueueattributes-on-a-deleted-queue-is.md; filed separately because these checks are listed under a different service heading.)

Traces:   lambda__sns__sqs.pipeline-sns-sqs-lambda, lambda__sns__sqs.sns-to-sqs-subscription-basic, lambda__sns__sqs.sns-notification-config-sqs-destination-stale-arn-fails.

## Evidence
pipeline-sns-sqs-lambda:
  AssertionError: SendMessage <queue>: AWS.SimpleQueueService.NonExistentQueue message is 'The specified queue does not exist for this wsdl version.', expected 'The specified queue does not exist.'

sns-to-sqs-subscription-basic:
  AssertionError: SendMessage <queue>: AWS.SimpleQueueService.NonExistentQueue message is 'The specified queue does not exist for this wsdl version.', expected 'The specified queue does not exist.'

sns-notification-config-sqs-destination-stale-arn-fails:
  AssertionError: got message 'The specified queue does not exist for this wsdl version.' (expected "The specified queue does not exist.")

Expected in all three: HTTP 400, code 'AWS.SimpleQueueService.NonExistentQueue', Message 'The specified queue does not exist.' — whole sentence fixed text.

## Steps to Reproduce
1. aws sqs create-queue --queue-name <name>; subscribe it to an SNS topic.
2. aws sqs delete-queue --queue-url <url>
3. aws sqs send-message --queue-url <url> --message-body gone
4. Observe Message 'The specified queue does not exist for this wsdl version.' instead of AWS's 'The specified queue does not exist.' (status 400 and code are correct).
