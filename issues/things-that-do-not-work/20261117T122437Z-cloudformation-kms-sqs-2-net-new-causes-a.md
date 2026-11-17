Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__kms__sqs.cfn-sqs-queue-kms-data-key-reuse-period-out-of-range-update-rejected-rolls-back cloudformation__kms__sqs.cfn-sqs-queue-kms-sse-properties-update-in-place-round-trip
Issue-Key: cloudformation__kms__sqs.cfn-sqs-queue-kms-data-key-reuse-period-out-of-range-update-rejected-rolls-back|run#21|AssertionError|assert got == _REJECTED, f"the queue resource's failure reason is {got!r}, expected {_REJECTED!r}"
Issue-Key: cloudformation__kms__sqs.cfn-sqs-queue-kms-sse-properties-update-in-place-round-trip|run#30|AssertionError|assert final["StackStatus"] == "UPDATE_COMPLETE", (
Issue-Commit: 745763357b0410355460567821d75d30e5935144

# cloudformation__kms__sqs: 2 net-new causes — a queue resource's failure reason drops the wrapped SQS service suffix, and an in-place update from SSE-KMS to SqsManagedSseEnabled fails the queue update

> _Note: this issue was identified against the code base at commit `745763357b0410355460567821d75d30e5935144`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
Two net-new root causes for CloudFormation-managed SQS queue encryption. Distinct from the open issue 20261115T171209Z-sqs-the-queuedoesnotexist-refusal-carries-the-legacy-wsdl.md, which concerns the QueueDoesNotExist refusal.

Cause 1 of 2: the message CloudFormation quotes from SQS inside a resource failure reason omits the service/status/request-id suffix AWS includes.
  AWS: recorded behavior only — when SQS refuses the queue update (here KmsDataKeyReusePeriodSeconds out of range), the stack rolls back and the Queue resource's UPDATE_FAILED reason is a registry-handler failure whose quoted message is SQS's own refusal WITH its SDK suffix: 'Invalid value for the parameter KmsDataKeyReusePeriodSeconds. (Service: Sqs, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)' and HandlerErrorCode 'InvalidRequest'. The fixed text is the SQS message plus ' (Service: Sqs, Status Code: 400, Request ID: ' / ') (SDK Attempt Count: ' / ')'; the request id and attempt count are per-call values.
  Emulator: the same reason carries only 'Invalid value for the parameter KmsDataKeyReusePeriodSeconds.' (HandlerErrorCode InvalidRequest is correct) — the wrapped service/status/request-id suffix is missing.
  Why: a pipeline that parses the handler reason to recover which service refused, with what status and request id (the standard way to correlate a stack failure with a service call) finds none of it.
  Fix: the CloudFormation resource-handler error wrapping for AWS::SQS::Queue (the component that renders a service refusal into ResourceStatusReason) must include the AWS SDK service suffix.
  Traces: cloudformation__kms__sqs.cfn-sqs-queue-kms-data-key-reuse-period-out-of-range-update-rejected-rolls-back.

Cause 2 of 2: an in-place update that drops KmsMasterKeyId and sets SqsManagedSseEnabled=true fails the queue resource and rolls the stack back.
  AWS: recorded behavior only — the three encryption stages of one queue are all in-place updates: create with KmsMasterKeyId + KmsDataKeyReusePeriodSeconds, update to a different key and period, then update to SqsManagedSseEnabled=true with KmsMasterKeyId absent. Each ends UPDATE_COMPLETE with the same QueueUrl/Arn; after the last one GetQueueAttributes no longer reports KmsMasterKeyId and reports SqsManagedSseEnabled='true', and the queue still round-trips a message.
  Emulator: the third update settles UPDATE_ROLLBACK_COMPLETE with StackStatusReason 'The following resource(s) failed to update: [Queue]. ' — switching a queue from SSE-KMS to SQS-managed SSE is refused, so the stack reverts.
  Why: a stack that migrates a queue off a customer managed key to SQS-managed encryption (a routine, supported change) cannot be updated at all; the deploy fails and the queue stays on the old key.
  Fix: the AWS::SQS::Queue resource handler's update path and SQS's SetQueueAttributes handling of clearing KmsMasterKeyId while enabling SqsManagedSseEnabled (queue attribute state/validation).
  Traces: cloudformation__kms__sqs.cfn-sqs-queue-kms-sse-properties-update-in-place-round-trip.

## Evidence
cfn-sqs-queue-kms-data-key-reuse-period-out-of-range-update-rejected-rolls-back — AssertionError: the queue resource's failure reason is ('Invalid value for the parameter KmsDataKeyReusePeriodSeconds.', 'InvalidRequest'), expected ('Invalid value for the parameter KmsDataKeyReusePeriodSeconds. (Service: Sqs, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)', 'InvalidRequest').

cfn-sqs-queue-kms-sse-properties-update-in-place-round-trip — AssertionError: SSE-SQS update reached 'UPDATE_ROLLBACK_COMPLETE': 'The following resource(s) failed to update: [Queue]. ' Expected UPDATE_COMPLETE, same QueueUrl, KmsMasterKeyId absent and SqsManagedSseEnabled='true'.

## Steps to Reproduce
1. Cause 1: deploy an AWS::SQS::Queue with KmsMasterKeyId set, then UpdateStack with KmsDataKeyReusePeriodSeconds out of range (e.g. 30); after the rollback read the Queue row's UPDATE_FAILED ResourceStatusReason and compare its quoted message with SQS's refusal including the ' (Service: Sqs, Status Code: 400, Request ID: ...) (SDK Attempt Count: ...)' suffix.
2. Cause 2: deploy the queue with KmsMasterKeyId=<key1>, KmsDataKeyReusePeriodSeconds=70; UpdateStack to <key2>/3600 (expect UPDATE_COMPLETE, same url); UpdateStack again with KmsMasterKeyId absent and SqsManagedSseEnabled=true -> expect UPDATE_COMPLETE, then GetQueueAttributes(All): no KmsMasterKeyId, SqsManagedSseEnabled='true', and a send/receive round trip still works.
