Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__sqs__ssm.ec2mr-cfnres-physical-ids-resolve-in-owning-services cloudformation__sqs__ssm.cdk-default-deploy-legacy-synthesizer-carries-cdk-metadata-without-bootstrap cloudformation__sqs__ssm.change-set-disable-rollback-execute-leaves-create-failed-and-is-updatable
Issue-Key: cloudformation__sqs__ssm.ec2mr-cfnres-physical-ids-resolve-in-owning-services|run#25|AssertionError|assert resolved == url, (
Issue-Key: cloudformation__sqs__ssm.cdk-default-deploy-legacy-synthesizer-carries-cdk-metadata-without-bootstrap|run#17|AssertionError|assert _outputs(deployed) == {"QueueUrl": url}, (
Issue-Key: cloudformation__sqs__ssm.change-set-disable-rollback-execute-leaves-create-failed-and-is-updatable|run#38|AssertionError|assert physical == {"Queue": url, "Keep": keep, "Bad": fixed}, (
Issue-Commit: a3e7427fc33f71baf84855dcb1036df3c2b77cc6

# cloudformation__sqs__ssm: an AWS::SQS::Queue's PhysicalResourceId / Ref / stack Output is a synthesized real-AWS queue URL that disagrees with the URL SQS itself serves

> _Note: this issue was identified against the code base at commit `a3e7427fc33f71baf84855dcb1036df3c2b77cc6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStacks

## Description
Cause 1 of 1: CloudFormation's AWS::SQS::Queue resource provider formats the queue's URL from a fixed real-AWS template (`https://sqs.<region>.amazonaws.com/<account-id>/<queue-name>`) instead of recording the URL the SQS service assigned to the queue it created. Every CloudFormation surface that exposes that value — the resource's PhysicalResourceId in DescribeStackResources, `Ref`/`Fn::GetAtt <Queue>.QueueUrl`, and any stack Output built from them — therefore answers a different string than SQS's own CreateQueue/GetQueueUrl for the same queue. Failing checks: cloudformation__sqs__ssm.ec2mr-cfnres-physical-ids-resolve-in-owning-services, cloudformation__sqs__ssm.cdk-default-deploy-legacy-synthesizer-carries-cdk-metadata-without-bootstrap, cloudformation__sqs__ssm.change-set-disable-rollback-execute-leaves-create-failed-and-is-updatable.

AWS: Smithy (sqs): `CreateQueueResult.QueueUrl` and `GetQueueUrlResult.QueueUrl` are the URL of that queue, and that exact string is what every queue-resolving operation takes in its required `QueueUrl` member (SendMessage, ReceiveMessage, GetQueueAttributes, PurgeQueue, ...). CloudFormation documents `Ref` on an `AWS::SQS::Queue` as resolving to the queue's URL and `Fn::GetAtt <Queue>.QueueUrl` as the same value, and a resource's PhysicalResourceId as the owning service's real handle for it. The host of a queue URL is environment-specific and is NOT the requirement; the requirement is the invariant AWS holds unconditionally — the URL CloudFormation reports for a queue and the URL SQS reports for that same queue are one string, and it is addressable at the endpoint the client is talking to. Recorded behavior only for the concrete host. Both surfaces answer HTTP 200 here; no error code is involved.

Emulator: the two surfaces disagree for one queue. SQS resolves the queue name to the listener the emulator actually serves (`https://<served-host>:<served-port>/<account-id>/<queue-name>`), while CloudFormation reports `https://sqs.<region>.amazonaws.com/<account-id>/<queue-name>` — a host the emulator does not serve — as the PhysicalResourceId and in the stack Output. The value is plainly built from a hardcoded pattern rather than read back from the CreateQueue the provider performed.

Why: passing a queue to its consumer through a stack Output (or reading the PhysicalResourceId back from DescribeStackResources) is the ordinary way a template wires a queue to an application, and it is what `cdk deploy` emits. A client that takes that value and calls SendMessage/ReceiveMessage addresses a host nothing answers on, so the stack reports success while the application cannot use the queue at all; tooling that cross-checks CloudFormation against SQS (drift detection, reconcilers, the CDK's own output assertions) sees two irreconcilable identities for one resource.

Fix: the CloudFormation AWS::SQS::Queue resource provider's identifier/attribute resolution — it must store the `QueueUrl` returned by the CreateQueue call it made as the resource's PhysicalResourceId and QueueUrl attribute, rather than formatting one from region/account/name.

Traces: cloudformation__sqs__ssm.ec2mr-cfnres-physical-ids-resolve-in-owning-services (PhysicalResourceId vs GetQueueUrl), cloudformation__sqs__ssm.cdk-default-deploy-legacy-synthesizer-carries-cdk-metadata-without-bootstrap (stack Output vs SQS), cloudformation__sqs__ssm.change-set-disable-rollback-execute-leaves-create-failed-and-is-updatable (physical ids after the fixing update). The same defect is filed for a different service heading as 20261121T105340Z-cloudformation-lambda-sqs-an-aws-sqs-queue-stack.md; these three checks are not covered by it.

## Evidence
Failing check `cloudformation__sqs__ssm.ec2mr-cfnres-physical-ids-resolve-in-owning-services` [mismatch]:

    AssertionError: GetQueueUrl('<stack>-Queue-<suffix>') returned
      'https://<served-host>:<served-port>/<account>/<stack>-Queue-<suffix>',
    not the PhysicalResourceId 'https://sqs.<region>.amazonaws.com/<account>/<stack>-Queue-<suffix>'

Failing check `cloudformation__sqs__ssm.cdk-default-deploy-legacy-synthesizer-carries-cdk-metadata-without-bootstrap` [mismatch]:

    AssertionError: the stack outputs {'QueueUrl': 'https://sqs.<region>.amazonaws.com/<account>/<queue>'};
    SQS reports the queue at 'https://<served-host>:<served-port>/<account>/<queue>'

Failing check `cloudformation__sqs__ssm.change-set-disable-rollback-execute-leaves-create-failed-and-is-updatable` [mismatch]:

    AssertionError: physical ids after the fix are {... 'Queue': 'https://sqs.<region>.amazonaws.com/<account>/<queue>'}
    (expected the URL SQS reports for that queue)

Expected (AWS): the CloudFormation-reported value and the SQS-reported value are the identical string, usable as `QueueUrl` against the endpoint being talked to.
Actual (emulator): CloudFormation serves the fixed `https://sqs.<region>.amazonaws.com/<account>/<name>` form while SQS serves its own listener. Host, port, account id, stack and queue names above are per-run values; the invariant that diverges is the equality of the two values.

## Steps to Reproduce
1. cloudformation CreateStack with a template containing `{"Resources": {"Queue": {"Type": "AWS::SQS::Queue", "Properties": {"MessageRetentionPeriod": 3600}}}, "Outputs": {"QueueUrl": {"Value": {"Ref": "Queue"}}}}`. Wait for CREATE_COMPLETE.
2. cloudformation DescribeStackResources(StackName) and read the `Queue` row's `PhysicalResourceId`; also read the `QueueUrl` Output from DescribeStacks.
3. sqs GetQueueUrl(QueueName=<the generated queue name>) and read `QueueUrl`.
4. Expected (AWS): steps 2 and 3 return the same string. Observed: step 2 returns `https://sqs.<region>.amazonaws.com/<account>/<name>`, step 3 returns the emulator's served `https://<host>:<port>/<account>/<name>`.
5. sqs GetQueueAttributes / SendMessage with the value from step 2: on AWS it addresses the queue; against the emulator it names a host that is not served.
