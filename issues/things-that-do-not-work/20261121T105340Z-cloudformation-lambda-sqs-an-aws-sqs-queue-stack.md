Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__lambda__sqs.deploy-cloudformation-2-lambdahook-rejects-deny-queue
Issue-Key: cloudformation__lambda__sqs.deploy-cloudformation-2-lambdahook-rejects-deny-queue|run#25|AssertionError|assert allow_url == _outputs(ctx, allow_id)["QueueUrl"], (
Issue-Commit: 77bfd47bd7799ecf9c5ff0eea8e18d7399a37670

# cloudformation__lambda__sqs: an AWS::SQS::Queue stack output publishes a queue URL on the real-AWS host, disagreeing with the queue URL SQS itself serves

> _Note: this issue was identified against the code base at commit `77bfd47bd7799ecf9c5ff0eea8e18d7399a37670`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStacks

## Description
Cause 1 of 1: CloudFormation's AWS::SQS::Queue resource provider synthesizes the queue's URL from a fixed real-AWS template instead of reporting the URL the SQS service actually assigned to the queue it created, so a stack Output built from `Ref`/`Fn::GetAtt <Queue>.QueueUrl` and SQS's own `CreateQueue`/`GetQueueUrl` answer two different strings for one queue.

AWS: Smithy (sqs): `CreateQueueResult.QueueUrl` and `GetQueueUrlResult.QueueUrl` are the URL of that Amazon SQS queue, and it is the exact value every data-plane operation takes in its required `QueueUrl` member (`SendMessage`, `ReceiveMessage`, `GetQueueAttributes`, ...). CloudFormation documents `Ref` on an `AWS::SQS::Queue` as resolving to that queue's URL, and `Fn::GetAtt <Queue>.QueueUrl` to the same value. The host part of a queue URL is environment-specific and is NOT a requirement here; the requirement is the invariant AWS holds unconditionally — the URL CloudFormation reports for a queue and the URL SQS reports for the same queue are the SAME string, and both are served by the endpoint the client is talking to. Recorded behavior only for the concrete host.

Emulator: the two surfaces disagree. For one queue created by the stack, SQS resolves the queue name to the emulator's own served listener (`https://<served-host>:<served-port>/<account-id>/<queue-name>`), while the stack Output for the same resource reports `https://sqs.<region>.amazonaws.com/<account-id>/<queue-name>` — a host the emulator does not serve. The CloudFormation attribute is clearly built from a hardcoded `https://sqs.<region>.amazonaws.com/<account>/<name>` pattern rather than read back from the created queue.

Why: handing a queue to an application through a stack Output is the ordinary way a template wires a queue to its consumer. A client that reads that Output and calls SendMessage/ReceiveMessage with it addresses a host nothing answers on, so the stack deploys "successfully" and the application cannot use the queue at all. It also makes a stack Output un-comparable with the SQS API for any check or tool that cross-checks the two.

Fix: the CloudFormation AWS::SQS::Queue resource provider's Ref/attribute resolution (it must take the QueueUrl returned by the CreateQueue call it made and store that as the resource's primary identifier and QueueUrl attribute, rather than formatting one). Note the related open issues 20261111T055733Z-sqs-queue-urls-returned-by-the-read-operations.md and 20261111T110051Z-sqs-2-net-new-causes-createqueue-publishes-a.md describe the OPPOSITE direction on the SQS side (SQS publishing an unserved real-AWS host); neither covers CloudFormation's own attribute, which is the surface diverging here, and SQS is now the surface serving the reachable URL.

Traces: cloudformation__lambda__sqs.deploy-cloudformation-2-lambdahook-rejects-deny-queue.

## Evidence
Failing check `cloudformation__lambda__sqs.deploy-cloudformation-2-lambdahook-rejects-deny-queue` [mismatch]:

    AssertionError: SQS resolves 'allow-sim2real-xsvc-lh-<id>-a' to
      'https://<served-emulator-host>:<served-port>/000000000000/allow-sim2real-xsvc-lh-<id>-a',
    stack output says
      'https://sqs.us-west-2.amazonaws.com/000000000000/allow-sim2real-xsvc-lh-<id>-a'

Expected (AWS): the two strings are identical, and both name an endpoint that answers SQS calls.
Actual (emulator): CloudFormation reports the fixed `https://sqs.<region>.amazonaws.com/<account>/<name>` form; SQS reports its served listener. (The host:port, account id and queue name above are per-run values; the invariant that diverges is the equality of the two values.)

## Steps to Reproduce
1. create_stack with a template containing an `AWS::SQS::Queue` resource and an Output whose Value is `!Ref <Queue>` (or `!GetAtt <Queue>.QueueUrl`). Wait for CREATE_COMPLETE.
2. Read the Output value from DescribeStacks.
3. Call sqs GetQueueUrl (or CreateQueue's own return for a queue created directly) for the same queue name.
4. Compare the two strings. Expected: equal. Observed: the stack Output is `https://sqs.<region>.amazonaws.com/<account>/<name>` while SQS answers the emulator's served `https://<host>:<port>/<account>/<name>`.
5. Call sqs SendMessage with the stack Output's URL: on AWS it succeeds, against the emulator it addresses an unserved host.
