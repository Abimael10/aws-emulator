Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__cloudwatch__sns__sqs.cdk-deploy-alarm-sns-sqs-ok-transition-without-ok-actions-delivers-nothing cloudformation__ec2__sqs__ssm.cfn-yaml-short-form-intrinsics-shape-resources cloudformation__events__sqs.deploy-cfn-events-rule-with-target-resource-lifecycle cloudformation__kms__sqs.cdk-deploy-kms-alias-encrypted-queue-names-the-key-and-round-trips-a-message-then-destroyed cloudformation__lambda__s3__sqs.s3-to-sqs-to-lambda-pipeline-positive cloudformation__lambda__sqs.cdk-deploy-lambda-sqs-event-source-processes-and-drains cloudformation__lambda__sqs.cfn-sqs-esm-filter-criteria-drops-non-matching-message-without-invoking cloudformation__sns__sqs.cfn-independent-branches-provision-concurrently-and-tear-down-in-reverse cloudformation__sns__sqs.deploy-sns-subscription-missing-protocol-rejected cloudformation__sqs.cdk-rollback-force-completes-past-the-resource-whose-rollback-fails cloudformation__sqs.cdk-rollback-orphan-skips-the-resource-whose-rollback-fails cloudformation__sqs.cfn-sqs-queue-attributes-and-getatt-read-back-then-nonexistent-after-delete cloudformation__sqs.cfn-sqs-queue-in-place-update-retained-replacement-then-not-found cloudformation__sqs__ssm.cdk-destroy-deletes-a-create-failed-stack-and-its-live-resources cloudformation__sqs__ssm.ec2mr-cfnres-lookalike-physical-id-rejected-by-owning-service cloudwatch__events__sqs.xsvc-cloudwatch-1-alarm-state-change-chain-delivered-to-sqs cloudwatch__sns__sqs.metric-datapoints-cross-threshold-and-notify dynamodb__pipes__sqs.pipes-dynamodb-stream-source-preserves-per-key-change-order ec2__lambda__s3__sqs.send-message-to-absent-queue-rejected-with-live-control events__lambda__sqs.eventbridge-rule-on-detail-content-invokes-lambda-only-for-the-matching-entry events__lambda__sqs.eventbridge-rule-sends-to-sqs-target events__lambda__sqs.eventbridge-rule-targeting-a-lambda-alias-invokes-the-aliased-version events__lambda__sqs.events-rule-sqs-esm-lambda-pipeline events__s3__sqs.s3-notification-to-sqs-direct events__s3__sqs.s3-queue-notification-filter-delivers-only-matching-url-encoded-keys events__s3__sqs.s3-queue-notification-names-copy-multipart-and-delete-events lambda__s3__sqs.s3-notification-queue-deleted-after-config-no-delivery-negative lambda__scheduler__sqs.xsvc-lambda-3-scheduler-lambda-target-receives-exact-input-dlq-empty lambda__secretsmanager__sqs.error-propagation-queue-not-exist lambda__sns__sqs.pipeline-sns-sqs-lambda lambda__sns__sqs.sns-notification-config-sqs-destination-stale-arn-fails lambda__sns__sqs.sns-to-sqs-subscription-basic lambda__sqs.sqs-esm-delivers-each-record-byte-exact-with-its-message-attributes lambda__sqs.sqs-esm-on-a-function-alias-invokes-the-aliased-version-not-latest lambda__sqs__stepfunctions.sfn-sqs-task-wrong-queueurl-negative redshift__sqs.cli-redshift-sqs-missing-queue-negative s3__sns__sqs.s3-object-created-notifies-sns sns__sqs.sns-publish-batch-delivers-one-envelope-per-entry-to-the-subscribed-queue sns__sqs.sns-to-sqs-envelope-carries-the-subject-and-typed-message-attributes sqs.cli-sqs-create-send-receive-delete sqs.fifo-queue-moves-an-exact-multibyte-message-and-the-deleted-fifo-queue-is-gone sqs.nonexistent-queue sqs.operations-fail-after-delete sqs.queue-moves-a-batch-of-exact-messages-and-the-deleted-queue-refuses-batch-calls sqs.queue-url-from-another-region-not-served-rejected sqs.receipt-handle-from-a-deleted-queue-incarnation-rejected sqs.recreated-queue-does-not-inherit-the-deleted-queues-policy sqs.tag-nonexistent-queue sqs.tag-operations-on-a-deleted-queue-rejected
Issue-Key: cloudformation__cloudwatch__sns__sqs.cdk-deploy-alarm-sns-sqs-ok-transition-without-ok-actions-delivers-nothing|run#44|AssertionError|assert got == ("AWS.SimpleQueueService.NonExistentQueue", "The specified queue does not exist."), (
Issue-Key: sqs|GetQueueAttributes|AWS.SimpleQueueService.NonExistentQueue
Issue-Key: cloudformation__events__sqs.deploy-cfn-events-rule-with-target-resource-lifecycle|run#28|AssertionError|assert got == ("AWS.SimpleQueueService.NonExistentQueue", "The specified queue does not exist."), (
Issue-Key: cloudformation__kms__sqs.cdk-deploy-kms-alias-encrypted-queue-names-the-key-and-round-trips-a-message-then-destroyed|run#38|AssertionError|assert (queue_code, queue_message) == (
Issue-Key: cloudformation__lambda__s3__sqs|SendMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Key: cloudformation__lambda__sqs.cdk-deploy-lambda-sqs-event-source-processes-and-drains|run#17|AssertionError|assert queue_gone == ("AWS.SimpleQueueService.NonExistentQueue", "The specified queue does not exist."), (
Issue-Key: cloudformation__lambda__sqs.cfn-sqs-esm-filter-criteria-drops-non-matching-message-without-invoking|run#28|AssertionError|assert got == want, f"after the stack delete: expected {want!r}, got {got!r}"
Issue-Key: cloudformation__sns__sqs.cfn-independent-branches-provision-concurrently-and-tear-down-in-reverse|run#55|AssertionError|assert send_code == _NO_SUCH_QUEUE, (
Issue-Key: cloudformation__sqs.cdk-rollback-force-completes-past-the-resource-whose-rollback-fails|run#21|AssertionError|assert gone == ("AWS.SimpleQueueService.NonExistentQueue", "The specified queue does not exist."), (
Issue-Key: cloudformation__sqs.cdk-rollback-orphan-skips-the-resource-whose-rollback-fails|run#21|AssertionError|assert gone == ("AWS.SimpleQueueService.NonExistentQueue", "The specified queue does not exist."), (
Issue-Key: cloudformation__sqs.cfn-sqs-queue-attributes-and-getatt-read-back-then-nonexistent-after-delete|run#22|AssertionError|assert message == "The specified queue does not exist.", (
Issue-Key: cloudformation__sqs.cfn-sqs-queue-in-place-update-retained-replacement-then-not-found|run#45|AssertionError|assert got == _NOT_FOUND, f"GetQueueAttributes after the stack delete answered {got!r}"
Issue-Key: cloudformation__sqs__ssm.cdk-destroy-deletes-a-create-failed-stack-and-its-live-resources|run#28|AssertionError|assert gone == ("AWS.SimpleQueueService.NonExistentQueue", "The specified queue does not exist."), (
Issue-Key: cloudformation__sqs__ssm.ec2mr-cfnres-lookalike-physical-id-rejected-by-owning-service|run#19|AssertionError|assert got == ("AWS.SimpleQueueService.NonExistentQueue",
Issue-Key: cloudwatch__events__sqs|SendMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Key: cloudwatch__sns__sqs|SendMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Key: dynamodb__pipes__sqs|SendMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Key: ec2__lambda__s3__sqs.send-message-to-absent-queue-rejected-with-live-control|run#8|AssertionError|assert message == "The specified queue does not exist.", (
Issue-Key: events__lambda__sqs|SendMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Key: events__s3__sqs|SendMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Key: lambda__s3__sqs.s3-notification-queue-deleted-after-config-no-delivery-negative|run#21|AssertionError|assert final_message == "The specified queue does not exist.", (
Issue-Key: lambda__scheduler__sqs|SendMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Key: lambda__secretsmanager__sqs.error-propagation-queue-not-exist|run#14|AssertionError|assert result["payload"]["message"] == "The specified queue does not exist.", (
Issue-Key: lambda__sns__sqs|SendMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Key: lambda__sns__sqs.sns-notification-config-sqs-destination-stale-arn-fails|run#22|AssertionError|assert message == "The specified queue does not exist.", f"got message {message!r}"
Issue-Key: lambda__sqs|SendMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Key: lambda__sqs__stepfunctions.sfn-sqs-task-wrong-queueurl-negative|run#15|AssertionError|assert cause.startswith("The specified queue does not exist. (Service: AmazonSQS; "), neg_execution
Issue-Key: redshift__sqs.cli-redshift-sqs-missing-queue-negative|run#17|AssertionError|assert msg == "The specified queue does not exist.", msg
Issue-Key: s3__sns__sqs|SendMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Key: sns__sqs|SendMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Key: sqs.fifo-queue-moves-an-exact-multibyte-message-and-the-deleted-fifo-queue-is-gone|_assert_not_found#27|AssertionError|assert message == QUEUE_NOT_FOUND_MSG, (
Issue-Key: sqs.nonexistent-queue|run#8|AssertionError|assert code == (ERR_NONEXISTENT_QUEUE, "The specified queue does not exist."), f"expected {ERR_NONEXISTENT_QUEUE}, got {code!r}"
Issue-Key: sqs.operations-fail-after-delete|run#8|AssertionError|assert err.get("Message") == MSG_NOT_FOUND, f"got {err!r}"
Issue-Key: sqs.queue-moves-a-batch-of-exact-messages-and-the-deleted-queue-refuses-batch-calls|_assert_not_found#24|AssertionError|assert message == QUEUE_NOT_FOUND_MSG, (
Issue-Key: sqs.queue-url-from-another-region-not-served-rejected|run#9|AssertionError|assert (err.get("Code"), err.get("Message")) == (_NO_SUCH_QUEUE, _NO_SUCH_QUEUE_MSG), (
Issue-Key: sqs.receipt-handle-from-a-deleted-queue-incarnation-rejected|run#16|AssertionError|assert (err.get("Code"), err.get("Message")) == (_NO_SUCH_QUEUE, _NO_SUCH_QUEUE_MSG), (
Issue-Key: sqs.recreated-queue-does-not-inherit-the-deleted-queues-policy|run#14|AssertionError|assert err["Message"] == "The specified queue does not exist.", f"got {err['Message']!r}"
Issue-Key: sqs.tag-nonexistent-queue|run#9|AssertionError|assert code == (NONEXISTENT_QUEUE, "The specified queue does not exist."), f"expected {NONEXISTENT_QUEUE}, got {code}"
Issue-Key: sqs.tag-operations-on-a-deleted-queue-rejected|run#12|AssertionError|assert (err.get("Code"), err.get("Message")) == (_NO_SUCH_QUEUE, _NO_SUCH_QUEUE_MSG), (
Issue-Commit: 2a513259435d6dcec5aa5a741411e3a8a3522aaf

# sqs: the QueueDoesNotExist refusal carries the legacy WSDL message ("...does not exist for this wsdl version."), failing 49 behavioral checks across every service that resolves a queue URL

> _Note: this issue was identified against the code base at commit `2a513259435d6dcec5aa5a741411e3a8a3522aaf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendMessage

## Description
Cause 1 of 1: every SQS operation that fails to resolve a queue (deleted queue, never-created name, wrong-region queue URL, stale receipt handle from a previous incarnation) answers the QueueDoesNotExist refusal with the pre-2023 query/WSDL-era message text instead of AWS's current sentence. Status and error code are right, only the message diverges, so 49 behavioral checks in this suite — spread over the sqs, cloudformation__*, cloudwatch__*, events__*, lambda__*, sns__sqs, s3__sns__sqs, redshift__sqs and dynamodb__pipes__sqs headings — fail on the message assertion alone. All 49 failing check ids are in trace_ids; they are one defect in one shared error constructor, not per-service slips (the wording is byte-identical everywhere, including when Step Functions re-wraps it into an execution `cause`).

AWS:      Smithy (com.amazonaws.sqs): error shape `QueueDoesNotExist`, errorKind=client, httpError=400, awsQueryError code `AWS.SimpleQueueService.NonExistentQueue` (httpResponseCode 400), single member `message` targeting `ExceptionMessage`; doc: "Ensure that the QueueUrl is correct and that the queue has not been deleted." The model does not carry the message string, so for the text this is recorded behavior only: AWS answers HTTP 400, Code `AWS.SimpleQueueService.NonExistentQueue`, Message `The specified queue does not exist.` That whole sentence is FIXED text — this error class interpolates NO variable parts (no queue name, queue URL, account id or request id appears in it). When Step Functions surfaces the same refusal it renders `The specified queue does not exist. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <per-call id>; Proxy: null)` under error `SQS.QueueDoesNotExistException` — the sentence is the fixed part, the request id is the per-call value.

Emulator: status 400 and code `AWS.SimpleQueueService.NonExistentQueue` are correct, but the message is `The specified queue does not exist for this wsdl version.` — AWS's retired WSDL-era sentence with the extra trailing ` for this wsdl version`. Observed uniformly on SendMessage, SendMessageBatch, ReceiveMessage, DeleteMessage, GetQueueAttributes, SetQueueAttributes, TagQueue, ListQueueTags, UntagQueue and the queue-resolution step of StartMessageMoveTask, in the JSON protocol (`{"Message": "The specified queue does not exist for this wsdl version.", "Code": "AWS.SimpleQueueService.NonExistentQueue", "QueryErrorCode": "QueueDoesNotExist", "Type": "Sender"}`) and when propagated into an SFN execution `cause`.

Why:      an error message is part of this error class's contract. Clients that assert on, log or pattern-match SQS's not-found sentence — negative-path test suites, CloudFormation/CDK and Cloud Control flows that use native SQS as ground truth after a stack delete, EventBridge/Pipes/Lambda plumbing that embeds the SQS refusal in a dead-letter record or an SFN `cause` — see a sentence AWS never produces, so every exact-match assertion that the queue is gone fails even though the queue really is gone. It also falsely signals that the emulator is serving a legacy SQS API version.

Fix:      the SQS service's shared QueueDoesNotExist/NonExistentQueue error construction — the single place the queue-resolution path of the SQS handlers builds that refusal — must emit the fixed sentence `The specified queue does not exist.` with no suffix, keeping HTTP 400 and code `AWS.SimpleQueueService.NonExistentQueue` unchanged. Any other component that re-renders the refusal (the Step Functions SQS integration's `cause` string) then inherits the corrected text.

Traces:   the 49 behavioral checks listed in trace_ids. NOTE on existing issues: the open issue 20261115T161642Z-cloudcontrol-sqs-the-queuedoesnotexist-refusal-message-is-the-legacy.md describes the same wording defect but is grounded only in six `cloudcontrol.*` checks and carries only their replay keys, and 20261115T161642Z-cloudcontrol-sqs-setqueueattributes-on-a-deleted-queue-is.md covers only Cloud Control's SetQueueAttributes path; neither carries any of the 49 check ids filed here, which is why they are still failing and unowned. The two other open NonExistentQueue issues (20261111T033241Z-events-eventbridge-sqs-target-dead-letters-misclassify-an-unauthorized.md, 20261115T150513Z-cloudwatch-events-s3-sqs-an-eventbridge-sqs-target.md) are about EventBridge dead-letter ERROR_CODE/ERROR_MESSAGE classification and do not cover this message text.

## Evidence
Expected in every case: HTTP 400, Code 'AWS.SimpleQueueService.NonExistentQueue', Message 'The specified queue does not exist.' (fixed text, no interpolated values).
Actual in every case: HTTP 400, Code 'AWS.SimpleQueueService.NonExistentQueue', Message 'The specified queue does not exist for this wsdl version.'

sqs.nonexistent-queue:
  AssertionError: expected AWS.SimpleQueueService.NonExistentQueue, got ('AWS.SimpleQueueService.NonExistentQueue', 'The specified queue does not exist for this wsdl version.')

sqs.operations-fail-after-delete:
  AssertionError: got {'Message': 'The specified queue does not exist for this wsdl version.', 'Code': 'AWS.SimpleQueueService.NonExistentQueue', 'QueryErrorCode': 'QueueDoesNotExist', 'Type': 'Sender'}

sqs.tag-nonexistent-queue (TagQueue on a never-created URL):
  AssertionError: expected AWS.SimpleQueueService.NonExistentQueue, got ('AWS.SimpleQueueService.NonExistentQueue', 'The specified queue does not exist for this wsdl version.')

sqs.tag-operations-on-a-deleted-queue-rejected (ListQueueTags):
  AssertionError: ListQueueTags on the deleted queue answered {'Message': 'The specified queue does not exist for this wsdl version.', 'Code': 'AWS.SimpleQueueService.NonExistentQueue', 'QueryErrorCode': 'QueueDoesNotExist', 'Type': 'Sender'}

sqs.queue-url-from-another-region-not-served-rejected:
  AssertionError: SendMessage on the 'us-west-2' endpoint for a 'us-east-1' queue answered {'Message': 'The specified queue does not exist for this wsdl version.', 'Code': 'AWS.SimpleQueueService.NonExistentQueue', ...}

sqs.receipt-handle-from-a-deleted-queue-incarnation-rejected:
  AssertionError: DeleteMessage on the recreated queue with the old incarnation's handle answered {'Message': 'The specified queue does not exist for this wsdl version.', ...}

events__lambda__sqs.eventbridge-rule-sends-to-sqs-target / events__s3__sqs.s3-notification-to-sqs-direct / lambda__sqs.sqs-esm-delivers-each-record-byte-exact-with-its-message-attributes:
  AssertionError: SendMessage <queue>: expected ('AWS.SimpleQueueService.NonExistentQueue', 400, 'The specified queue does not exist.'), got ('AWS.SimpleQueueService.NonExistentQueue', 400, 'The specified queue does not exist for this wsdl version.')

cloudformation__sqs.cfn-sqs-queue-attributes-and-getatt-read-back-then-nonexistent-after-delete:
  AssertionError: expected the standard NonExistentQueue message, got 'The specified queue does not exist for this wsdl version.'

lambda__secretsmanager__sqs.error-propagation-queue-not-exist (refusal observed from inside a Lambda handler):
  AssertionError: expected 'The specified queue does not exist.', got {'ok': False, 'error': 'QueueDoesNotExist', 'code': 'AWS.SimpleQueueService.NonExistentQueue', 'message': 'The specified queue does not exist for this wsdl version.'}

lambda__sqs__stepfunctions.sfn-sqs-task-wrong-queueurl-negative (same text re-wrapped by Step Functions):
  execution error 'SQS.QueueDoesNotExistException', cause 'The specified queue does not exist for this wsdl version. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Proxy: null)'
  expected cause to start with 'The specified queue does not exist. (Service: AmazonSQS; '

## Steps to Reproduce
1. Create an SQS queue (CreateQueue, or via a CloudFormation/CDK stack).
2. Delete it (DeleteQueue or delete the owning stack) — or simply build a queue URL for a name that was never created, or point a us-west-2 endpoint at a us-east-1 queue URL.
3. Call any queue-resolving SQS operation on that URL, e.g.
     aws sqs send-message --queue-url <deleted-url> --message-body x
     aws sqs get-queue-attributes --queue-url <deleted-url> --attribute-names QueueArn
     aws sqs tag-queue --queue-url <missing-url> --tags a=b
     aws sqs list-queue-tags --queue-url <deleted-url>
   or drive it indirectly: a Step Functions SQS:sendMessage task with a bogus QueueUrl, or a Lambda handler calling SendMessage on a deleted queue.
4. Observe the refusal: HTTP 400 and code AWS.SimpleQueueService.NonExistentQueue are correct, but Message is 'The specified queue does not exist for this wsdl version.' instead of AWS's 'The specified queue does not exist.' The same wrong sentence appears verbatim in the Step Functions execution `cause`.
