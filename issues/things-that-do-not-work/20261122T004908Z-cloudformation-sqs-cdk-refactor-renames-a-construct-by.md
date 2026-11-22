Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__sqs.cdk-refactor-renames-a-construct-keeping-the-queues-physical-id-and-messages
Issue-Key: cloudformation__sqs.cdk-refactor-renames-a-construct-keeping-the-queues-physical-id-and-messages|run#32|AssertionError|assert after["ResourceStatus"] == "CREATE_COMPLETE", (
Issue-Commit: 29c63349654daecf518dc89fab6ca09926c7725f

# cloudformation__sqs: `cdk refactor` renames a construct by IMPORTING the queue, so the resource's ResourceStatus becomes IMPORT_COMPLETE instead of staying CREATE_COMPLETE

> _Note: this issue was identified against the code base at commit `29c63349654daecf518dc89fab6ca09926c7725f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteStackRefactor

## Description
Cause 1 of 1: a CloudFormation stack refactor (the `cdk refactor` flow: CreateStackRefactor + ExecuteStackRefactor over a logical-id rename) is implemented in the emulator as a resource IMPORT, so after the refactor the renamed resource is reported by DescribeStackResources with ResourceStatus IMPORT_COMPLETE. AWS reports a refactored resource as still CREATE_COMPLETE.

AWS: a stack refactor moves a resource to a new logical id WITHOUT any create, update, delete or import action against the resource itself — it only rewrites CloudFormation's mapping from logical id to the existing physical resource. Consequently, on DescribeStackResources after the refactor the resource keeps the status it already had from its original provisioning: Smithy: cloudformation StackResource$ResourceStatus targets enum ResourceStatus whose members include both CREATE_COMPLETE and IMPORT_COMPLETE, and a resource that was originally created by the stack (not imported into it) carries CREATE_COMPLETE. The refactor itself is reported through its own status surface (StackRefactor / StackRefactorExecutionStatus), not by restamping the moved resource's ResourceStatus. The requirement here is the rule, not a recorded value: the resource's ResourceStatus is whatever action last acted on the RESOURCE, and a rename is not such an action; the logical id, physical id and stack name are values from the request and the emulator's own state.

Emulator: the rename itself works — the check's earlier assertions pass, so after the refactor LogicalResourceId has changed to the new construct id and PhysicalResourceId is still the original queue URL. But DescribeStackResources then reports ResourceStatus 'IMPORT_COMPLETE' for that resource, where AWS reports 'CREATE_COMPLETE'. Replay detail: `AssertionError: after \`cdk refactor\`, the queue's ResourceStatus is 'IMPORT_COMPLETE', expected CREATE_COMPLETE — a true rename never goes through an UPDATE or a replacement's DELETE+CREATE`.

Why: a client that reads DescribeStackResources / DescribeStackResource to decide how a resource came to be in the stack (drift tooling, import/adoption auditing, CDK and Terraform-style reconciliation, and any gate that treats IMPORT_COMPLETE as "this resource is not managed/created by me") draws the wrong conclusion after a pure rename: the resource looks adopted from outside the stack rather than created by it, even though the stack created it and the refactor changed nothing but its logical id.

Fix: the stack-refactor execution path in the cloudformation service — the component that applies a StackRefactor's logical-id mapping to the stack's resource records — must re-key the existing resource record (logical id) while PRESERVING its ResourceStatus/ResourceStatusReason and PhysicalResourceId, instead of routing the move through the resource-import code path that stamps IMPORT_COMPLETE.

Note: open issue 20261109T185037Z-cloudformation-sqs-5-net-new-causes-resource-import-does-not.md covers `cdk refactor`/`cdk migrate` not taking effect at all, and 20261102T023139Z-cloudformation-stack-refactor-execution-stackset-import-validation-and.md covers stack-refactor execution generally; neither carries this check's replay key and the suite reports this failure as unowned, so it is filed here. This failure is the opposite shape: the refactor DOES now take effect (logical id moved, physical id preserved), and only the resulting ResourceStatus is wrong.

Traces: cloudformation__sqs.cdk-refactor-renames-a-construct-keeping-the-queues-physical-id-and-messages.

## Evidence
Expected (AWS): after `cdk refactor` renames the construct holding the AWS::SQS::Queue, DescribeStackResources on the app stack returns exactly one StackResource with LogicalResourceId starting with the NEW construct id, PhysicalResourceId equal to the pre-refactor queue URL, and ResourceStatus == "CREATE_COMPLETE" (the status from the original create; a rename performs no UPDATE and no replacement DELETE+CREATE, and no import).

Actual (emulator): the same DescribeStackResources call returns the resource with the new LogicalResourceId and the unchanged PhysicalResourceId, but ResourceStatus == "IMPORT_COMPLETE". The check fails with:
  File "check_cloudformation__sqs_cdk_refactor_renames_a_construct_keeping_the_queues_physical_id_and_messages.py", line 118, in run
    assert after["ResourceStatus"] == "CREATE_COMPLETE"
  AssertionError: after `cdk refactor`, the queue's ResourceStatus is 'IMPORT_COMPLETE', expected CREATE_COMPLETE — a true rename never goes through an UPDATE or a replacement's DELETE+CREATE

The assertions before it (new logical id, unchanged physical id) passed, so only the status is divergent.

## Steps to Reproduce
1. `cdk bootstrap` a toolkit stack with a unique qualifier.
2. Deploy the single-resource CDK app `cdk_refactor_queue_rename` (one aws_cdk.aws_sqs.Queue whose construct id is context `construct_id`, initially "Queue"), producing a stack with exactly one AWS::SQS::Queue.
3. DescribeStackResources on the stack; note LogicalResourceId (starts with "Queue"), PhysicalResourceId (the queue URL) and ResourceStatus (CREATE_COMPLETE).
4. SendMessage a marker body to that queue URL, ReceiveMessage it and ChangeMessageVisibility it back with VisibilityTimeout=0 so it stays in the queue.
5. Re-synth the same app with context construct_id="QueueRenamed" and run `cdk refactor --unstable=refactor -y --toolkit-stack-name <toolkit>` (exits 0 and names both construct paths).
6. DescribeStackResources again: the resource's LogicalResourceId is now "QueueRenamed..." and its PhysicalResourceId is unchanged, but ResourceStatus reads IMPORT_COMPLETE instead of CREATE_COMPLETE.
