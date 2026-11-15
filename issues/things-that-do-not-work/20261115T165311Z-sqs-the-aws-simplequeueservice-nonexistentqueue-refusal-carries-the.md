Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__events__sqs.deploy-cfn-events-rule-with-target-resource-lifecycle cloudwatch__events__sqs.xsvc-cloudwatch-1-alarm-state-change-chain-delivered-to-sqs events__lambda__sns.eventbridge-rule-target-sns-arn-publish events__lambda__sqs.eventbridge-rule-on-detail-content-invokes-lambda-only-for-the-matching-entry events__lambda__sqs.eventbridge-rule-sends-to-sqs-target events__lambda__sqs.eventbridge-rule-targeting-a-lambda-alias-invokes-the-aliased-version events__lambda__sqs.events-rule-sqs-esm-lambda-pipeline events__s3__sqs.s3-notification-to-sqs-direct events__s3__sqs.s3-queue-notification-filter-delivers-only-matching-url-encoded-keys events__s3__sqs.s3-queue-notification-names-copy-multipart-and-delete-events
Issue-Key: cloudformation__events__sqs.deploy-cfn-events-rule-with-target-resource-lifecycle|run#28|AssertionError|assert got == ("AWS.SimpleQueueService.NonExistentQueue", "The specified queue does not exist."), (
Issue-Key: cloudwatch__events__sqs|SendMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Key: events__lambda__sns|SendMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Key: events__lambda__sqs|SendMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Key: events__s3__sqs|SendMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Commit: 78517b75c0e3f9eb9fe3d5d92b3abb1be57beaa5

# sqs: the AWS.SimpleQueueService.NonExistentQueue refusal carries the legacy WSDL sentence instead of AWS's "The specified queue does not exist."

> _Note: this issue was identified against the code base at commit `78517b75c0e3f9eb9fe3d5d92b3abb1be57beaa5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendMessage

## Description
One net-new root cause, grounded in ten failing checks. The suite lists those checks under five different headings (cloudformation__events__sqs, cloudwatch__events__sqs, events__lambda__sns, events__lambda__sqs, events__s3__sqs), but the defect is single and lives in SQS's shared queue-not-found refusal, so it is filed once, under `sqs`, with every failing check id in trace_ids. In each of the ten checks this is the ONLY assertion that fails: the checks use "send to a queue I just deleted" as their teardown/liveness probe and grade the refusal exactly.

Cause 1 of 1: a request naming a queue that does not exist (deleted, or never created) is refused with the right status and the right error code but with the legacy WSDL-era message sentence.

AWS: Smithy (sqs) declares `QueueDoesNotExist` on SendMessage (and on every operation that resolves a QueueUrl): errorKind client, httpError 400, awsQueryError code `AWS.SimpleQueueService.NonExistentQueue`, doc "Ensure that the QueueUrl is correct and that the queue has not been deleted." The model carries no message text, so the sentence itself is recorded behavior only: for a queue that is simply absent, AWS answers HTTP 400, error code `AWS.SimpleQueueService.NonExistentQueue`, message exactly `The specified queue does not exist.` That sentence is fixed text in its entirety — it has NO variable parts; the queue name, URL, account and region are not interpolated into it. The recordings in this suite also show that AWS keeps a SECOND, distinct fixed sentence under the same error code for the class where the queue exists but the principal may not write to it — `The specified queue does not exist or you do not have access to it.` (that is the sentence EventBridge records in a dead letter's ERROR_MESSAGE). Both sentences are requirements for their own class and must not be collapsed into one.

Emulator: the status (400) and the code (`AWS.SimpleQueueService.NonExistentQueue`) are right, but the message is `The specified queue does not exist for this wsdl version.` — the sentence the retired SQS query API (2009-02-01/2012-11-05 WSDL) returned, which the current API never returns. It is emitted by the single shared constructor `sqs::queue_does_not_exist()`, so every queue-resolution refusal in the service carries it.

Why: the error message is part of the refusal contract. A client, SDK retry/triage wrapper, operator runbook or log-grep that matches SQS's current not-found sentence never matches against this emulator, and anything that renders the error to a user shows a sentence mentioning a "wsdl version" that no current SQS caller can ever see. Because the same constructor serves every operation that resolves a QueueUrl, the divergence is service-wide, not confined to SendMessage.

Fix: the shared SQS queue-not-found refusal constructor `sqs::queue_does_not_exist()` in the SQS crate's shared error helpers — the message must be AWS's current fixed sentence for an absent queue, while the distinct not-authorized sentence stays its own value, so the two classes remain distinguishable.

Traces: cloudformation__events__sqs.deploy-cfn-events-rule-with-target-resource-lifecycle; cloudwatch__events__sqs.xsvc-cloudwatch-1-alarm-state-change-chain-delivered-to-sqs; events__lambda__sns.eventbridge-rule-target-sns-arn-publish; events__lambda__sqs.eventbridge-rule-on-detail-content-invokes-lambda-only-for-the-matching-entry; events__lambda__sqs.eventbridge-rule-sends-to-sqs-target; events__lambda__sqs.eventbridge-rule-targeting-a-lambda-alias-invokes-the-aliased-version; events__lambda__sqs.events-rule-sqs-esm-lambda-pipeline; events__s3__sqs.s3-notification-to-sqs-direct; events__s3__sqs.s3-queue-notification-filter-delivers-only-matching-url-encoded-keys; events__s3__sqs.s3-queue-notification-names-copy-multipart-and-delete-events. Code sites: services/service-com-amazonaws-sqs-amazonsqs/src/ops/send_message.rs:333-338 calls `sqs::queue_does_not_exist()` after `state.queue_by_url_mut(...)` misses.

## Evidence
Expected vs actual on the refusal of SendMessage against a deleted queue (identical in all ten checks):
  expected status  = 400                                            actual 400 (matches)
  expected code    = "AWS.SimpleQueueService.NonExistentQueue"       actual "AWS.SimpleQueueService.NonExistentQueue" (matches)
  expected message = "The specified queue does not exist."           actual "The specified queue does not exist for this wsdl version."
The whole expected message is fixed text; nothing in it comes from the request or state, so the divergence is not a per-run value.

events__lambda__sqs.eventbridge-rule-sends-to-sqs-target (_gone_queue -> _refused, line 225):
  AssertionError: SendMessage <queue>: AWS.SimpleQueueService.NonExistentQueue message: expected 'The specified queue does not exist.', got 'The specified queue does not exist for this wsdl version.'

events__lambda__sqs.eventbridge-rule-on-detail-content-invokes-lambda-only-for-the-matching-entry (_grade, line 180):
  AssertionError: SendMessage <queue>: expected ('AWS.SimpleQueueService.NonExistentQueue', 400, 'The specified queue does not exist.'), got ('AWS.SimpleQueueService.NonExistentQueue', 400, 'The specified queue does not exist for this wsdl version.')

events__lambda__sqs.eventbridge-rule-targeting-a-lambda-alias-invokes-the-aliased-version (_grade, line 199): same expected/actual triple.
events__lambda__sqs.events-rule-sqs-esm-lambda-pipeline (_refused, line 280): same message divergence.
events__lambda__sns.eventbridge-rule-target-sns-arn-publish (_refused, line 193): same message divergence.
events__s3__sqs.s3-notification-to-sqs-direct (_refused, line 248): same message divergence.
events__s3__sqs.s3-queue-notification-filter-delivers-only-matching-url-encoded-keys (_refused, line 271): same message divergence.
events__s3__sqs.s3-queue-notification-names-copy-multipart-and-delete-events (_refused, line 276): same message divergence.
cloudwatch__events__sqs.xsvc-cloudwatch-1-alarm-state-change-chain-delivered-to-sqs (_gone_queue -> _refused, line 461):
  AssertionError: SendMessage <queue>: AWS.SimpleQueueService.NonExistentQueue message is 'The specified queue does not exist for this wsdl version.', expected 'The specified queue does not exist.'
cloudformation__events__sqs.deploy-cfn-events-rule-with-target-resource-lifecycle (line 127), probing the stack's queue after the stack delete:
  assert got == ("AWS.SimpleQueueService.NonExistentQueue", "The specified queue does not exist.")
  AssertionError: ('AWS.SimpleQueueService.NonExistentQueue', 'The specified queue does not exist for this wsdl version.')

## Steps to Reproduce
1. CreateQueue with any name; note the QueueUrl.
2. DeleteQueue on that QueueUrl.
3. SendMessage with that QueueUrl and any MessageBody (poll until it is refused, since the delete is eventually consistent).
   Expected: HTTP 400, error code AWS.SimpleQueueService.NonExistentQueue, message exactly "The specified queue does not exist."
   Emulator: HTTP 400 and the right code, but message "The specified queue does not exist for this wsdl version."
4. Repeat with a QueueUrl that was never created, and with other queue-resolving operations (ReceiveMessage, GetQueueAttributes, SetQueueAttributes, DeleteMessage) — the same constructor is used, so the same wrong sentence comes back.
5. Keep the not-authorized class distinct: give an existing queue a resource policy that does not allow the calling principal and confirm that path still answers the separate sentence "The specified queue does not exist or you do not have access to it." under the same error code.
