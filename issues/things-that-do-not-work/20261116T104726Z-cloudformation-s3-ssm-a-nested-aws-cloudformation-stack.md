Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__s3__ssm.cfn-nested-stack-in-place-parameter-update-reaches-child-resource-then-not-found
Issue-Key: cloudformation__s3__ssm.cfn-nested-stack-in-place-parameter-update-reaches-child-resource-then-not-found|run#33|AssertionError|assert rows == want, (
Issue-Commit: 749d15b6fbd9d45e9f4cb9d1e27df4cf54db433b

# cloudformation__s3__ssm: a nested AWS::CloudFormation::Stack resource records only one UPDATE_COMPLETE event on an in-place parent update (the parent's cleanup event is missing)

> _Note: this issue was identified against the code base at commit `749d15b6fbd9d45e9f4cb9d1e27df4cf54db433b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
One net-new root cause.

Cause 1 of 1: a nested stack resource's event stream on an in-place parent update is one event short.

AWS: recorded behavior only (the Smithy model has no trait describing stack-event emission; AWS::CloudFormation::Stack is documented at https://docs.aws.amazon.com/AWSCloudFormation/latest/TemplateReference/aws-resource-cloudformation-stack.html). When an UpdateStack on a parent stack updates a nested AWS::CloudFormation::Stack resource in place — no create-only property changes, so the child stack is never replaced — the parent's event stream carries THREE events for that logical resource, all with no ResourceStatusReason and all with the child stack's unchanged StackId as PhysicalResourceId: ResourceStatus UPDATE_IN_PROGRESS, then UPDATE_COMPLETE when the child stack's own update finishes, then a second UPDATE_COMPLETE when the parent's update completes its cleanup phase. The duplicated terminal event is the fixed shape of a nested-stack resource's update, not a per-run artifact; only the physical id value (the child's StackId) and the logical id come from the request and the emulator's state.

Emulator: DescribeStackEvents on the parent returns only two events for the nested resource — [('UPDATE_IN_PROGRESS', None, <child StackId>), ('UPDATE_COMPLETE', None, <child StackId>)]. Status order and physical id are right; the trailing cleanup-phase UPDATE_COMPLETE is never emitted. Everything else in the scenario (the child stack keeps its StackId, the child's Param updates in place, the parameter reaches version 2) matched.

Why: a client that reconciles a nested-stack deployment from the parent's event stream — waiters, CDK/CLI progress printers and drift/audit tooling that count resource transitions per phase, or that distinguish "the child finished" from "the parent finished cleaning up" — sees one fewer transition than AWS produces and can conclude the parent update is still mid-flight, or mis-pair events to update phases.

Fix: CloudFormation's stack-update event recorder for nested AWS::CloudFormation::Stack resources — the parent update's cleanup phase must append the second UPDATE_COMPLETE event for the nested resource, with no reason and the child stack's StackId as PhysicalResourceId.

Related but NOT the same cause, and not covering this: open issue 20261115T073242Z-cloudformation-sns-sqs-a-parent-stack-update-leaves.md (an UNCHANGED nested stack staying in CREATE_COMPLETE). This check's nested stack does update and does reach UPDATE_COMPLETE; the defect is the missing second event.

Traces: cloudformation__s3__ssm.cfn-nested-stack-in-place-parameter-update-reaches-child-resource-then-not-found (assertion at check line 148: rows == want).

## Evidence
Check cloudformation__s3__ssm.cfn-nested-stack-in-place-parameter-update-reaches-child-resource-then-not-found, mismatch.

Expected (AWS), events for logical id 'Child' as (ResourceStatus, ResourceStatusReason, PhysicalResourceId), oldest first:
  [('UPDATE_IN_PROGRESS', None, <child StackId>), ('UPDATE_COMPLETE', None, <child StackId>), ('UPDATE_COMPLETE', None, <child StackId>)]

Actual (emulator):
  AssertionError: Child must update in place as the same child stack, its events were [('UPDATE_IN_PROGRESS', None, 'arn:aws:cloudformation:<region>:<account>:stack/<parent>-Child-<suffix>/<child-uuid>'), ('UPDATE_COMPLETE', None, 'arn:aws:cloudformation:<region>:<account>:stack/<parent>-Child-<suffix>/<child-uuid>')]

(The arn/uuid/name values above are this run's generated ids and are not part of the requirement; the three-event sequence and the repeated physical id are.)

## Steps to Reproduce
1. Put a child template (one AWS::SSM::Parameter whose Name and Value are template Parameters, plus an Output) into an S3 bucket and note its regional object URL.
2. CreateStack a parent whose single resource is AWS::CloudFormation::Stack with that TemplateURL and Parameters {ParamName, Greeting='hello'}; wait for CREATE_COMPLETE and record the child's StackId from DescribeStackResources.
3. Snapshot the parent's event ids via DescribeStackEvents.
4. UpdateStack the parent with the same template but Greeting='bonjour'; wait for UPDATE_COMPLETE.
5. DescribeStackEvents on the parent and select the new events whose LogicalResourceId is the nested-stack resource.
6. Observed: two events (UPDATE_IN_PROGRESS, UPDATE_COMPLETE). AWS returns three: UPDATE_IN_PROGRESS, UPDATE_COMPLETE, UPDATE_COMPLETE, each with the child's StackId and no reason.
