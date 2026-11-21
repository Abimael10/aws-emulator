Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__s3__sqs.deploy-cloudformation-3-include-transform-injects-queue-properties
Issue-Key: cloudformation__s3__sqs.deploy-cloudformation-3-include-transform-injects-queue-properties|run#25|AssertionError|assert queue_url == sqs.get_queue_url(QueueName=queue_name)["QueueUrl"], (
Issue-Commit: a3e7427fc33f71baf84855dcb1036df3c2b77cc6

# cloudformation__s3__sqs: the QueueUrl Output of an AWS::Include-transformed stack is a real-AWS-host URL, not the queue SQS serves

> _Note: this issue was identified against the code base at commit `a3e7427fc33f71baf84855dcb1036df3c2b77cc6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStacks

## Description
Cause 1 of 1: the AWS::SQS::Queue provider formats the queue URL from a fixed real-AWS pattern instead of reporting the QueueUrl SQS returned for the queue it created, so the stack's QueueUrl Output names a queue SQS cannot resolve. (The AWS::Include transform itself processed correctly; only the published URL diverges.)

AWS: Smithy (sqs): CreateQueueResult.QueueUrl / GetQueueUrlResult.QueueUrl is that queue's URL and the exact value every data-plane operation requires. CloudFormation documents Ref on AWS::SQS::Queue as resolving to the queue's URL. The host is environment-specific and is not the requirement; the invariant is that CloudFormation's Output and SQS's GetQueueUrl answer for one queue are the SAME string and name the endpoint being served. Recorded behavior only for the concrete host.

Emulator: the Output reads https://sqs.<region>.amazonaws.com/<account-id>/<queue-name> while SQS knows the queue at its served listener, so the stack's Output 'is not the queue SQS knows'.

Why: the Output is how the template hands the queue to its consumer; a client using it sends to a host nothing answers on, so a CREATE_COMPLETE stack yields an unusable queue.

Fix: the CloudFormation AWS::SQS::Queue resource provider's Ref/attribute resolution (report the QueueUrl CreateQueue returned). Same cause as the already-open 20261121T105340Z-cloudformation-lambda-sqs-an-aws-sqs-queue-stack.md, which carries a different service heading and not this check.

Traces: cloudformation__s3__sqs.deploy-cloudformation-3-include-transform-injects-queue-properties.

## Evidence
deploy-cloudformation-3-include-transform-injects-queue-properties [mismatch]:

    AssertionError: the stack's QueueUrl output 'https://sqs.us-west-2.amazonaws.com/000000000000/<queue>' is not the queue SQS knows as '<queue>'

Expected (AWS): the Output equals sqs.get_queue_url(QueueName=<queue>)['QueueUrl'].
Actual (emulator): the fixed real-AWS form. (Account id and queue name are per-run values.)

## Steps to Reproduce
1. Put an AWS::Include snippet in S3 that adds VisibilityTimeout/Tags to a queue's Properties.
2. create_stack with a template using Fn::Transform AWS::Include for that queue and an Output !Ref <Queue>; wait for CREATE_COMPLETE.
3. Compare the Output with sqs GetQueueUrl for the queue name: expected equal, observed the Output carries https://sqs.<region>.amazonaws.com/<account>/<name>.
