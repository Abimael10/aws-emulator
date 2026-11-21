Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__sqs.cdk-import-adopts-an-existing-queue-and-refuses-a-missing-one cloudformation__sqs.cfn-on-failure-do-nothing-preserves-partial-resources-that-still-work cloudformation__sqs.cfn-resource-import-adopts-existing-queue-without-recreating-it cloudformation__sqs.cfn-sqs-queue-in-place-update-retained-replacement-then-not-found cloudformation__sqs.cfn-sqs-queue-readonly-queueurl-set-in-template-rejected-rolls-back cloudformation__sqs.cfn-sqs-queueinlinepolicy-ref-is-queue-url-and-removed-by-stack-update cloudformation__sqs.cfn-update-out-of-range-visibility-timeout-rejected-rolls-back cloudformation__sqs.cfn-update-replacement-changes-physical-id cloudformation__sqs.stack-tags-over-fifty-rejected-before-queue-creation
Issue-Key: cloudformation__sqs.cdk-import-adopts-an-existing-queue-and-refuses-a-missing-one|run#22|AssertionError|assert (described_cs["Status"], described_cs["StatusReason"]) == (
Issue-Key: cloudformation__sqs.cfn-on-failure-do-nothing-preserves-partial-resources-that-still-work|run#24|AssertionError|assert _queue_url_or_none(sqs, keep_do) == queue_url, (
Issue-Key: cloudformation__sqs.cfn-resource-import-adopts-existing-queue-without-recreating-it|run#21|AssertionError|assert imported_id == queue_url, (
Issue-Key: cloudformation__sqs.cfn-sqs-queue-in-place-update-retained-replacement-then-not-found|run#15|AssertionError|assert sqs.get_queue_url(QueueName=first)["QueueUrl"] == first_url, (
Issue-Key: cloudformation__sqs.cfn-sqs-queue-readonly-queueurl-set-in-template-rejected-rolls-back|run#14|AssertionError|assert good_physical == good_url, (
Issue-Key: cloudformation__sqs.cfn-sqs-queueinlinepolicy-ref-is-queue-url-and-removed-by-stack-update|run#10|AssertionError|assert queue_url == listed_url, f"Ref Queue {queue_url!r} != GetQueueUrl {listed_url!r}"
Issue-Key: cloudformation__sqs.cfn-update-out-of-range-visibility-timeout-rejected-rolls-back|run#20|AssertionError|assert sqs.get_queue_url(QueueName=queue_name)["QueueUrl"] == queue_url, (
Issue-Key: cloudformation__sqs.cfn-update-replacement-changes-physical-id|run#23|AssertionError|assert sqs.get_queue_url(QueueName=name)["QueueUrl"] == url, name
Issue-Key: cloudformation__sqs.stack-tags-over-fifty-rejected-before-queue-creation|run#12|AssertionError|assert real_url == queue_url, f"SQS's own queue URL {real_url!r} != the stack's Ref {queue_url!r}"
Issue-Commit: a3e7427fc33f71baf84855dcb1036df3c2b77cc6

# cloudformation__sqs: the AWS::SQS::Queue physical id (Ref / Output / change-set identifier) is a synthesized real-AWS-host queue URL, not the URL SQS serves

> _Note: this issue was identified against the code base at commit `a3e7427fc33f71baf84855dcb1036df3c2b77cc6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackResources

## Description
Cause 1 of 1: CloudFormation's AWS::SQS::Queue resource provider formats the queue's URL from a fixed real-AWS pattern (https://sqs.<region>.amazonaws.com/<account-id>/<queue-name>) instead of recording the QueueUrl the SQS service returned for the queue it created, so every surface that republishes that value — PhysicalResourceId in DescribeStackResources/DescribeStackResource, Ref, Fn::GetAtt <Queue>.QueueUrl, stack Outputs, and the identifier echoed in a change set's StatusReason and ResourceChange — names a different string than SQS's own GetQueueUrl/CreateQueue for the SAME queue. It also rewrites an identifier the CALLER supplied: an IMPORT change set given SQS's own queue URL reports the synthesized form back.

AWS: Smithy (sqs): CreateQueueResult.QueueUrl and GetQueueUrlResult.QueueUrl are the URL of that Amazon SQS queue, and it is the exact value every data-plane operation takes in its required QueueUrl member (SendMessage, ReceiveMessage, GetQueueAttributes, ...). CloudFormation documents Ref on an AWS::SQS::Queue as resolving to the queue's URL and Fn::GetAtt <Queue>.QueueUrl to the same value, and the PhysicalResourceId of a resource is the owning service's real handle. The host part of a queue URL is environment-specific and is NOT the requirement here; the requirement is the invariant AWS holds unconditionally — the URL CloudFormation reports for a queue and the URL SQS reports for the same queue are the SAME string, served by the endpoint the client is talking to. For an import whose identifier does not exist, AWS's change-set StatusReason is the fixed text "Resource of type '<type>' with identifier '<identifier>' was not found." where <type> and <identifier> are taken VERBATIM from the request's resource mapping — the identifier is not re-rendered. Recorded behavior only for the concrete host.

Emulator: the two surfaces disagree for one queue. SQS resolves the name to the listener the emulator actually serves (https://<served-host>:<served-port>/<account-id>/<queue-name>); CloudFormation reports https://sqs.<region>.amazonaws.com/<account-id>/<queue-name>, a host the emulator does not serve. Concretely: Ref Queue 'https://sqs.us-west-2.amazonaws.com/000000000000/<queue>' != GetQueueUrl 'https://127.0.0.1:<port>/000000000000/<queue>'; an IMPORT that adopts a live queue reports the physical id changing from the served URL to the synthesized one (read by the check as a replacement); a DO_NOTHING survivor's PhysicalResourceId no longer resolves at SQS; and the cdk-import change set's StatusReason re-renders the supplied identifier into the synthesized host.

Why: a stack Output or PhysicalResourceId is the ordinary way a template hands a queue to its consumer. A client that reads it and calls SendMessage/ReceiveMessage addresses a host nothing answers on, so the stack deploys "successfully" and the application cannot use the queue; drift, import and replacement logic that compares the stored physical id with the live resource also mis-decides, and any tool cross-checking CloudFormation against SQS sees two different resources.

Fix: the CloudFormation AWS::SQS::Queue resource provider's create/read/import path and its Ref/attribute resolution — store the QueueUrl returned by the CreateQueue call it made (or the identifier supplied to the import) as the resource's primary identifier and QueueUrl attribute instead of formatting one, and echo identifiers back verbatim in change-set status reasons. The open issue 20261121T105340Z-cloudformation-lambda-sqs-an-aws-sqs-queue-stack.md records this same cause under the cloudformation__lambda__sqs heading and does not carry these checks' replay keys; it does not cover them.

Traces: the nine failing checks listed in trace_ids, all in this service.

## Evidence
All nine checks fail on the same comparison. Representative assertions (host:port, account id, queue names and ports are per-run values; the diverging invariant is the equality of the two strings):

- cfn-sqs-queueinlinepolicy-ref-is-queue-url-and-removed-by-stack-update [mismatch]:
  AssertionError: Ref Queue 'https://sqs.us-west-2.amazonaws.com/000000000000/<queue>' != GetQueueUrl 'https://127.0.0.1:44415/000000000000/<queue>'
- stack-tags-over-fifty-rejected-before-queue-creation [mismatch]:
  AssertionError: SQS's own queue URL 'https://127.0.0.1:44263/000000000000/<queue>' != the stack's Ref 'https://sqs.us-west-2.amazonaws.com/000000000000/<queue>'
- cfn-sqs-queue-readonly-queueurl-set-in-template-rejected-rolls-back [mismatch]:
  AssertionError: the control queue's physical id 'https://sqs.us-west-2.amazonaws.com/000000000000/<queue>' is not SQS's own url 'https://127.0.0.1:38305/000000000000/<queue>'
- cfn-sqs-queue-in-place-update-retained-replacement-then-not-found [mismatch]:
  AssertionError: the queue's physical id 'https://sqs.us-west-2.amazonaws.com/000000000000/<queue>' must be the QueueUrl SQS answers for '<queue>'
- cfn-resource-import-adopts-existing-queue-without-recreating-it [mismatch]:
  AssertionError: the import replaced the resource instead of adopting it: 'https://127.0.0.1:37359/000000000000/<queue>' -> 'https://sqs.us-west-2.amazonaws.com/000000000000/<queue>'
- cdk-import-adopts-an-existing-queue-and-refuses-a-missing-one [mismatch]: DescribeChangeSet StatusReason is "Resource of type 'AWS::SQS::Queue' with identifier 'https://sqs.us-west-2.amazonaws.com/000000000000/<missing-queue>' was not found." where the resource mapping supplied the identifier 'https://<served-host>:<served-port>/000000000000/<missing-queue>'; ResourceChange.PhysicalResourceId carries the same rewritten identifier.
- cfn-update-out-of-range-visibility-timeout-rejected-rolls-back / cfn-update-replacement-changes-physical-id / cfn-on-failure-do-nothing-preserves-partial-resources-that-still-work [mismatch]: get_queue_url(QueueName=...)['QueueUrl'] != the physical id / Output CloudFormation reports for the same queue.

Expected (AWS): the CloudFormation-reported value and SQS's value are identical, and both name the endpoint that answers SQS calls.
Actual (emulator): CloudFormation reports the fixed https://sqs.<region>.amazonaws.com/<account>/<name> form.

## Steps to Reproduce
1. create_stack with a template containing an AWS::SQS::Queue resource (QueueName given) and an Output whose Value is !Ref <Queue>; wait for CREATE_COMPLETE.
2. Read the Output value from DescribeStacks and the PhysicalResourceId from DescribeStackResources.
3. Call sqs GetQueueUrl for the same queue name.
4. Compare: AWS returns one identical string; the emulator returns https://sqs.<region>.amazonaws.com/<account>/<name> from CloudFormation and its served https://<host>:<port>/<account>/<name> from SQS.
5. Call SendMessage with the CloudFormation value: on AWS it succeeds, against the emulator it addresses an unserved host.
6. For the import path: create a queue with SQS, then create_change_set(ChangeSetType=IMPORT) (or `cdk import`) with ResourcesToImport mapping QueueUrl to the URL SQS returned. Observe the adopted PhysicalResourceId (and, for a nonexistent identifier, the FAILED change set's StatusReason) carry the rewritten real-AWS host instead of the identifier supplied.
