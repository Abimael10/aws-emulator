Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__sns__sqs.cdk-sibling-nested-stacks-deliver-topic-to-queue-across-a-producer-only-update
Issue-Key: cloudformation__sns__sqs.cdk-sibling-nested-stacks-deliver-topic-to-queue-across-a-producer-only-update|run#24|AssertionError|assert statuses == ["UPDATE_COMPLETE"] * 3, (
Issue-Commit: d3cdd55e8b72332e5c71fd34997111c995fed5da

# cloudformation__sns__sqs: a parent-stack update leaves an unchanged nested stack in CREATE_COMPLETE instead of UPDATE_COMPLETE

> _Note: this issue was identified against the code base at commit `d3cdd55e8b72332e5c71fd34997111c995fed5da`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
Cause 1 of 1: when a parent stack is updated, a nested child stack whose own template did not change is never put through an update, so it keeps reporting CREATE_COMPLETE.
AWS: recorded behavior only (nested-stack handling is not in the Smithy model). An UpdateStack on a parent that owns AWS::CloudFormation::Stack children puts EVERY child through its own update — each child's StackStatus becomes UPDATE_COMPLETE (with no resource replacement when nothing about it changed) and the parent then reaches UPDATE_COMPLETE. A second `cdk deploy` that changes only the producer nested stack therefore leaves parent, producer and consumer all in UPDATE_COMPLETE, with the same nested stack ids and the same topic and queue physical ids.
Emulator: the parent and the changed producer reach UPDATE_COMPLETE, but the untouched consumer child is left in CREATE_COMPLETE — the observed statuses are ['UPDATE_COMPLETE', 'UPDATE_COMPLETE', 'CREATE_COMPLETE'] for (parent, producer, consumer).
Why: the nested stack's status is the signal clients and tooling use to tell whether a child took part in the latest deployment. A child still in CREATE_COMPLETE after its parent's update reads as never having been reconciled: drift tooling, CDK/CFN deploy summaries and any 'did my deployment reach every stack?' gate draw the wrong conclusion, and a change that must propagate through an unchanged child (a changed export it re-reads) is silently skipped.
Fix: the nested-stack (AWS::CloudFormation::Stack) update path in the CloudFormation provisioner — a parent update must run an update on every child stack, including children whose rendered template is unchanged, and advance their StackStatus to UPDATE_COMPLETE.
Failing check: cloudformation__sns__sqs.cdk-sibling-nested-stacks-deliver-topic-to-queue-across-a-producer-only-update.

## Evidence
Expected: after the producer-only deploy, DescribeStacks on (parent, producer nested stack, consumer nested stack) reports ['UPDATE_COMPLETE', 'UPDATE_COMPLETE', 'UPDATE_COMPLETE'].
Actual: AssertionError: after the producer-only update the parent, producer and consumer are ['UPDATE_COMPLETE', 'UPDATE_COMPLETE', 'CREATE_COMPLETE'] — the consumer nested stack was never updated.

## Steps to Reproduce
1. Replay the behavioral suite and re-run cloudformation__sns__sqs.cdk-sibling-nested-stacks-deliver-topic-to-queue-across-a-producer-only-update.
2. Deploy a CDK app whose parent stack owns two sibling NestedStacks: a producer holding an AWS::SNS::Topic and a consumer holding an AWS::SQS::Queue subscribed to that topic. Confirm delivery.
3. Re-deploy with ONLY the producer changed (e.g. the topic's DisplayName).
4. DescribeStacks on the parent StackId and on both nested StackIds — the unchanged consumer is still CREATE_COMPLETE.
