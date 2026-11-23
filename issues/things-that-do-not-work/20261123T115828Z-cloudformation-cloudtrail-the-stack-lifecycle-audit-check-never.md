Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__cloudtrail.obs-cloudformation-1-stack-lifecycle-is-audited-as-the-caller-and-provisioning-as-the-service
Issue-Key: cloudformation__cloudtrail.obs-cloudformation-1-stack-lifecycle-is-audited-as-the-caller-and-provisioning-as-the-service|_wait_stack#43|AssertionError|raise AssertionError(
Issue-Commit: a3841edc9a1ab0db1665cc08dab76ccb227286ad

# cloudformation__cloudtrail: the stack-lifecycle audit check never reaches its UpdateStack leg — an in-place AWS::SQS::Queue VisibilityTimeout update fails the Queue resource and the stack settles UPDATE_ROLLBACK_COMPLETE

> _Note: this issue was identified against the code base at commit `a3841edc9a1ab0db1665cc08dab76ccb227286ad`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
There is 1 net-new root cause in this issue (Cause 1 of 1); it must be closed before submitting. The grounding check is listed by the suite under the `cloudformation__cloudtrail` heading and is marked neither ALREADY FILED nor OWNED, so no open issue carries its replay key.

Placement note: this check is listed under a composite scenario heading, so the per-service gate cannot place it by exact equality against the bootstrapped token list; get_bootstrap_status() reports an EMPTY pending set, so neither cloudformation nor cloudtrail is un-bootstrapped and this is filed rather than deferred. The service token recorded is the suite heading.

Cause 1 of 1: the stack-update leg of the audit-trail scenario cannot run, because an UpdateStack whose only change to an AWS::SQS::Queue is its VisibilityTimeout fails the Queue resource and rolls the stack back. No CloudTrail assertion in the check is ever reached.
  Failing check id for this cause: cloudformation__cloudtrail.obs-cloudformation-1-stack-lifecycle-is-audited-as-the-caller-and-provisioning-as-the-service.
  AWS: an UpdateStack that changes a mutable AWS::SQS::Queue property is an in-place update — the Queue row reaches UPDATE_COMPLETE, its PhysicalResourceId is unchanged, and the stack reaches UPDATE_COMPLETE, after which the update's own API calls are auditable (cloudformation.amazonaws.com UpdateStack as the caller's identity, sqs.amazonaws.com SetQueueAttributes as the provisioning service). Smithy (sqs): the mutable surface is SetQueueAttributes(QueueUrl, Attributes) / TagQueue / UntagQueue, none of which models an already-exists error; CreateQueue alone models QueueNameExists (httpError 400, awsQueryError 'QueueAlreadyExists'), reachable only when CreateQueue names an existing queue with different attribute values.
  Emulator: the update fails the resource and the stack settles UPDATE_ROLLBACK_COMPLETE with StackStatusReason exactly "The following resource(s) failed to update: [<LogicalId>]. " — fixed text 'The following resource(s) failed to update: [', the failing logical ids from the template (here 'Queue'), then ']. ' including AWS's trailing space. Observed: "stack arn:aws:cloudformation:<region>:<account>:stack/<name>/<id> reached 'UPDATE_ROLLBACK_COMPLETE' instead of 'UPDATE_COMPLETE': 'The following resource(s) failed to update: [Queue]. '". Open issue 20261123T065749Z-cloudformation-sqs-reopens-aws-sqs-queue-still-has-no-in.md records the underlying handler message for its own checks ('Resource handler returned message: "A queue already exists with the same name and a different value for attribute VisibilityTimeout" (RequestToken: <id>, HandlerErrorCode: InvalidRequest)'), i.e. the UPDATE hook re-issues CreateQueue; that issue does not list this check id, so it will not re-replay it.
  Why: a client cannot evolve a CloudFormation-managed queue at all, and every downstream observation that depends on one successful update is unreachable — here the entire audit contract (which principal and which service each stack-lifecycle call is attributed to in CloudTrail) returns no verdict, so a real audit divergence would be invisible.
  Fix: the AWS::SQS::Queue resource provider's UPDATE hook in the CloudFormation service — apply mutable property diffs to the existing queue URL via SQS SetQueueAttributes/TagQueue/UntagQueue and keep the physical id, instead of re-creating the queue. Same component as the `cloudformation`-headed issue filed in this sweep for the same defect.
  Traces: cloudformation__cloudtrail.obs-cloudformation-1-stack-lifecycle-is-audited-as-the-caller-and-provisioning-as-the-service.

## Evidence
cloudformation__cloudtrail.obs-cloudformation-1-stack-lifecycle-is-audited-as-the-caller-and-provisioning-as-the-service [mismatch]
  Scenario: CreateStack (with a stack service RoleARN) of {"Resources":{"Queue":{"Type":"AWS::SQS::Queue","Properties":{...VisibilityTimeout...}}}}, then UpdateStack changing only the queue's VisibilityTimeout, then DeleteStack, then CloudTrail LookupEvents asserting the lifecycle calls are attributed to the caller and the provisioning calls to the service.
  Expected: the update reaches UPDATE_COMPLETE so the CloudTrail assertions can run.
  Actual: AssertionError: stack arn:aws:cloudformation:us-west-2:000000000000:stack/sim2real-xsvc-stk-49d458c61068/f433c781-fed8-c950-17d8-acb546b66d1f reached 'UPDATE_ROLLBACK_COMPLETE' instead of 'UPDATE_COMPLETE': 'The following resource(s) failed to update: [Queue]. '
  (Raised in _wait_stack at the UPDATE_COMPLETE wait; no LookupEvents assertion is reached.)

## Steps to Reproduce
1. CreateStack with {"AWSTemplateFormatVersion":"2010-09-09","Resources":{"Queue":{"Type":"AWS::SQS::Queue","Properties":{"QueueName":"q-<uniq>","VisibilityTimeout":30}}}} and a stack service RoleARN; poll to CREATE_COMPLETE.
2. UpdateStack with the same body but a different VisibilityTimeout; poll DescribeStacks.
   Expected: UPDATE_COMPLETE, Queue row UPDATE_COMPLETE, PhysicalResourceId unchanged.
   Emulator: UPDATE_ROLLBACK_COMPLETE with StackStatusReason 'The following resource(s) failed to update: [Queue]. '.
3. Once step 2 lands, CloudTrail LookupEvents(StartTime=<before the create>) must show the CreateStack/UpdateStack/DeleteStack calls with eventSource cloudformation.amazonaws.com under the calling identity, and the queue's provisioning calls with eventSource sqs.amazonaws.com under the service/assumed stack role.
