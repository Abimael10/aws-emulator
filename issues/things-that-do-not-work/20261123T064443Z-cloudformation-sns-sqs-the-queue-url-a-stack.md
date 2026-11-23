Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__sns__sqs.cfn-sns-subscription-in-place-update-retained-replacement-delivers-to-both-then-not-found cloudformation__sns__sqs.cfn-sns-topic-in-place-update-retained-replacement-then-not-found cloudformation__sns__sqs.deploy-cfn-subscription-raw-message-delivery-toggled-in-place-changes-delivered-envelope
Issue-Key: sqs|ReceiveMessage|AWS.SimpleQueueService.NonExistentQueue
Issue-Commit: 44d4304dbd56e0f97705f387ff3fc6793140eb84

# cloudformation__sns__sqs: the queue URL a stack publishes for an AWS::SQS::Queue (Ref / PhysicalResourceId / Output) is refused by SQS as AWS.SimpleQueueService.NonExistentQueue, so no CloudFormation-deployed topic-to-queue delivery can be observed

> _Note: this issue was identified against the code base at commit `44d4304dbd56e0f97705f387ff3fc6793140eb84`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackResources

## Description
ONE net-new root cause under the cloudformation__sns__sqs heading, carried by three failing checks. A fixer must close this one cause before submitting.

Cause 1 of 1: the queue URL CloudFormation publishes for an AWS::SQS::Queue resource — Ref, the resource's PhysicalResourceId, and any stack Output built from them — is not a URL the SQS service resolves. Every subsequent data-plane call made with that value is refused AWS.SimpleQueueService.NonExistentQueue / "The specified queue does not exist.", so the deliveries these three checks grade can never be observed. All three templates declare the queue WITHOUT a QueueName (CloudFormation generates it), so the generated-name path is what is exercised.
AWS: Smithy (sqs) CreateQueueResult.QueueUrl and GetQueueUrlResult.QueueUrl are the URL of that queue, and that exact string is what every data-plane operation takes in its required QueueUrl member (SendMessage, ReceiveMessage, GetQueueAttributes, DeleteMessage). CloudFormation documents Ref on an AWS::SQS::Queue as resolving to the queue's URL and Fn::GetAtt <Queue>.QueueUrl to the same value, and a resource's PhysicalResourceId is the owning service's real handle. The invariant AWS holds unconditionally is that the URL CloudFormation reports for a queue and the URL SQS reports for the SAME queue are one string, served by the endpoint the client is talking to — a ReceiveMessage with the stack's value succeeds (200, Messages or an empty body), it is never refused. The host/port of a queue URL is environment-specific and is NOT the requirement; the agreement between the two surfaces is. Recorded behavior only for the refusal the emulator serves: 400 AWS.SimpleQueueService.NonExistentQueue with message "The specified queue does not exist." — fixed text, no variable part — which is correct wording for a queue that really does not exist and simply must not be reached for a queue the stack just created.
Emulator: the stack reaches CREATE_COMPLETE and reports a queue URL for the resource, but sqs.receive_message(QueueUrl=<that value>) is refused with botocore.errorfactory.QueueDoesNotExist: An error occurred (AWS.SimpleQueueService.NonExistentQueue) when calling the ReceiveMessage operation: The specified queue does not exist. The same disagreement is already recorded for other headings as CloudFormation formatting a fixed real-AWS pattern (https://sqs.<region>.amazonaws.com/<account>/<name>) instead of storing the QueueUrl the CreateQueue call returned — see the open issues named below; here it surfaces as an outright refusal at the SQS data plane, which is the strongest form of the same defect.
Why: a Ref/Output of a queue is the ordinary way a template hands its queue to a consumer, and these are the queues the stack's own AWS::SQS::QueuePolicy and AWS::SNS::Subscription point at. A client that deploys the stack, reads the published URL and calls ReceiveMessage/SendMessage is told the queue does not exist, so a stack that deployed 'successfully' is unusable: SNS-to-SQS fan-out cannot be read, no consumer can be wired up from stack outputs, and drift/import logic comparing the stored physical id against the live resource mis-decides.
Fix: the CloudFormation AWS::SQS::Queue resource provider's create/read path and its Ref/attribute resolution — record the QueueUrl returned by the CreateQueue call the provider itself made (including the name CloudFormation generated) as the resource's primary identifier and QueueUrl attribute instead of formatting one, so that Ref, PhysicalResourceId and Outputs all carry the string SQS resolves.
Traces (failing checks, each erroring on the first receive_message against the stack's queue URL): cloudformation__sns__sqs.cfn-sns-topic-in-place-update-retained-replacement-then-not-found (queue URL = PhysicalResourceId of the 'Queue' resource, used by the delivery helper after CREATE_COMPLETE); cloudformation__sns__sqs.cfn-sns-subscription-in-place-update-retained-replacement-delivers-to-both-then-not-found (same, for QueueA/QueueB); cloudformation__sns__sqs.deploy-cfn-subscription-raw-message-delivery-toggled-in-place-changes-delivered-envelope (queue URL = the stack's QueueUrl Output). Everything these checks grade after that point — in-place vs replacing updates, UpdateReplacePolicy Retain, RawMessageDelivery toggling the delivered envelope, and the post-delete NotFound refusals — is blocked behind this one call and must pass once the published URL resolves.

These three checks are not marked ALREADY FILED or OWNED in the suite report and no open issue carries their replay keys. The same root cause is filed for OTHER headings and does not cover them: 20261121T121249Z-cloudformation-sqs-the-aws-sqs-queue-physical-id.md, 20261121T200646Z-cloudformation-2-net-new-causes-the-ref-output.md, 20261121T105340Z-cloudformation-lambda-sqs-an-aws-sqs-queue-stack.md, 20261121T115114Z-cloudformation-sqs-ssm-an-aws-sqs-queue-s.md and 20261121T121249Z-sqs-2-causes-a-cloudformation-deployed-queue-s.md. If one fix closes them all, re-replay the three checks listed in trace_ids to confirm.

## Evidence
All quotes are from the replayed behavioral suite (simparity-behavioral-aws-v1). Stack/queue names, hosts, ports and request ids are per-run values and are not part of the requirement; the requirement is that CloudFormation's queue URL and SQS's queue URL for one queue are the same resolvable string.

cloudformation__sns__sqs.cfn-sns-topic-in-place-update-retained-replacement-then-not-found [error]:
  Template resources: Topic (AWS::SNS::Topic, UpdateReplacePolicy Retain), Queue (AWS::SQS::Queue with only Tags — no QueueName), QueuePolicy, Subscription (protocol sqs, Endpoint = Fn::GetAtt Queue.Arn, RawMessageDelivery true).
  Passing steps: create_stack -> CREATE_COMPLETE; Topic's physical id is its ARN; Outputs TopicArn/TopicName correct; GetTopicAttributes matches the template.
  DIVERGING step: queue_url = PhysicalResourceId of 'Queue'; sns.publish(...); sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=5).
    Expected (AWS): HTTP 200 (eventually carrying the published body).
    Actual (emulator): botocore.errorfactory.QueueDoesNotExist: An error occurred (AWS.SimpleQueueService.NonExistentQueue) when calling the ReceiveMessage operation: The specified queue does not exist.

cloudformation__sns__sqs.cfn-sns-subscription-in-place-update-retained-replacement-delivers-to-both-then-not-found [error]: identical tail — ReceiveMessage on the stack's QueueA/QueueB URL refused AWS.SimpleQueueService.NonExistentQueue / "The specified queue does not exist."

cloudformation__sns__sqs.deploy-cfn-subscription-raw-message-delivery-toggled-in-place-changes-delivered-envelope [error]: queue_url comes from the stack Output "QueueUrl"; the first sqs.receive_message on it is refused AWS.SimpleQueueService.NonExistentQueue / "The specified queue does not exist." The preceding steps (CREATE_COMPLETE, Outputs, the Subscription's physical id, GetSubscriptionAttributes RawMessageDelivery == "false") all pass.

Expected (AWS): the queue URL a stack publishes is accepted by SQS; a client never sees NonExistentQueue for a queue the stack just created.
Actual (emulator): the published URL is unknown to SQS, so the data plane refuses it.

## Steps to Reproduce
Boot the emulator and point botocore at it: cfn = client("cloudformation"), sqs = client("sqs"), sns = client("sns").

1. create_stack with a template whose resources are an AWS::SNS::Topic, an AWS::SQS::Queue declared WITHOUT a QueueName, an AWS::SQS::QueuePolicy over Ref Queue, and an AWS::SNS::Subscription (Protocol "sqs", Endpoint Fn::GetAtt Queue.Arn), plus an Output {"QueueUrl": {"Value": {"Ref": "Queue"}}}. Wait for CREATE_COMPLETE.
2. Read the queue URL three ways: the Output value from describe_stacks, the PhysicalResourceId of 'Queue' from describe_stack_resources, and Fn::GetAtt Queue.QueueUrl.
3. Call sqs.receive_message(QueueUrl=<that value>, WaitTimeSeconds=5).
   Expected: HTTP 200. Observed: 400 AWS.SimpleQueueService.NonExistentQueue, "The specified queue does not exist."
4. Cross-check: sqs.list_queues() (or get_queue_url with the generated name from the published URL's last path segment) and compare the string SQS serves with the one CloudFormation published — on AWS they are identical.
5. After the fix, re-replay the three checks in trace_ids; they must get past the first receive_message and grade the in-place/replacement update, UpdateReplacePolicy Retain and RawMessageDelivery assertions that follow.
