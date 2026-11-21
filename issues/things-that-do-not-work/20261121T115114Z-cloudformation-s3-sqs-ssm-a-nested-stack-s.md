Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__s3__sqs__ssm.cdk-default-deploy-nested-stack-queue-url-reaches-parent-output
Issue-Key: cloudformation__s3__sqs__ssm.cdk-default-deploy-nested-stack-queue-url-reaches-parent-output|run#25|AssertionError|assert _outputs(parent) == {"QueueUrl": url}, (
Issue-Commit: a3e7427fc33f71baf84855dcb1036df3c2b77cc6

# cloudformation__s3__sqs__ssm: a nested stack's AWS::SQS::Queue URL passed up to the parent Output is a synthesized real-AWS URL, not the URL SQS serves

> _Note: this issue was identified against the code base at commit `a3e7427fc33f71baf84855dcb1036df3c2b77cc6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStacks

## Description
Cause 1 of 1: the same AWS::SQS::Queue identifier defect reached through a nested stack — CloudFormation's AWS::SQS::Queue resource provider formats the queue URL from a fixed real-AWS template (`https://sqs.<region>.amazonaws.com/<account-id>/<queue-name>`) instead of recording the URL SQS assigned to the queue it created, so the value the nested stack exports and the parent stack publishes as an Output disagrees with SQS's own GetQueueUrl for that queue. Failing check: cloudformation__s3__sqs__ssm.cdk-default-deploy-nested-stack-queue-url-reaches-parent-output.

AWS: Smithy (sqs): `CreateQueueResult.QueueUrl` / `GetQueueUrlResult.QueueUrl` is the URL of that queue and is the exact value every queue-resolving operation takes in its required `QueueUrl` member. CloudFormation documents `Ref` on an `AWS::SQS::Queue` as the queue's URL, and a nested stack's output passed up through `Fn::GetAtt <Nested>.Outputs.<Name>` carries that value verbatim. The host part is environment-specific and is NOT the requirement; the requirement is AWS's unconditional invariant — the URL CloudFormation publishes for a queue equals the URL SQS reports for it, and it is addressable at the endpoint being talked to. Recorded behavior only for the concrete host. The deploy succeeds (HTTP 200 / CREATE_COMPLETE); no error code is involved.

Emulator: the parent Output carries `https://sqs.<region>.amazonaws.com/<account-id>/<queue-name>`, a host the emulator does not serve, while SQS reports the queue at the emulator's own served listener `https://<served-host>:<served-port>/<account-id>/<queue-name>`.

Why: a nested-stack Output is exactly how a CDK app hands a queue created in a child construct to the consumer declared in the parent. A client that reads that Output and calls SendMessage/ReceiveMessage addresses an unserved host, so the deployment reports success and the application cannot reach its queue; anything that cross-checks the stack Output against SQS sees two identities for one queue.

Fix: the CloudFormation AWS::SQS::Queue resource provider's identifier/attribute resolution (store the QueueUrl returned by its own CreateQueue call as the resource's PhysicalResourceId and QueueUrl attribute instead of formatting one), which is what the nested-stack output plumbing then propagates.

Traces: cloudformation__s3__sqs__ssm.cdk-default-deploy-nested-stack-queue-url-reaches-parent-output. The same root cause is filed for other service headings as 20261121T105340Z-cloudformation-lambda-sqs-an-aws-sqs-queue-stack.md and in the companion cloudformation__sqs__ssm issue from this sweep; neither covers this check.

## Evidence
Failing check `cloudformation__s3__sqs__ssm.cdk-default-deploy-nested-stack-queue-url-reaches-parent-output` [mismatch]:

    AssertionError: the parent outputs {'QueueUrl': 'https://sqs.<region>.amazonaws.com/<account>/<queue>'};
    SQS reports the nested queue at 'https://<served-host>:<served-port>/<account>/<queue>'

Expected (AWS): the parent stack's `QueueUrl` Output is the identical string SQS reports for the nested stack's queue, and it is usable as `QueueUrl` against the endpoint being talked to.
Actual (emulator): CloudFormation publishes the fixed `https://sqs.<region>.amazonaws.com/<account>/<name>` form; SQS serves its own listener. Host, port, account id and queue name are per-run values; the diverging invariant is the equality of the two values.

## Steps to Reproduce
1. Deploy a parent CloudFormation stack containing an `AWS::CloudFormation::Stack` whose nested template declares an `AWS::SQS::Queue` and an Output `QueueUrl` = `!Ref Queue`, with the parent re-exporting it as `!GetAtt Nested.Outputs.QueueUrl` (this is what `cdk deploy` of a `NestedStack` emits). Wait for CREATE_COMPLETE on both.
2. cloudformation DescribeStacks on the parent and read the `QueueUrl` Output.
3. sqs GetQueueUrl(QueueName=<the nested queue's name>) and read `QueueUrl`.
4. Expected (AWS): the two strings are equal. Observed: step 2 gives `https://sqs.<region>.amazonaws.com/<account>/<name>`, step 3 gives the emulator's served `https://<host>:<port>/<account>/<name>`.
5. sqs SendMessage with the Output value: on AWS it reaches the queue; against the emulator it names an unserved host.
