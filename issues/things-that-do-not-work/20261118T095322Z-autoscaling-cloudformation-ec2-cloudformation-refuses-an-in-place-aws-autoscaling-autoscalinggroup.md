Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__cloudformation__ec2.cfn-autoscaling-group-in-place-max-size-retained-replacement-enforces-bounds-then-not-found
Issue-Key: autoscaling__cloudformation__ec2.cfn-autoscaling-group-in-place-max-size-retained-replacement-enforces-bounds-then-not-found|_expect#29|AssertionError|raise AssertionError(
Issue-Commit: cd94cc265dda0020a7368311398ddefc098e50a1

# CloudFormation refuses an in-place AWS::AutoScaling::AutoScalingGroup update that changes Tags, rolling the stack back instead of reaching UPDATE_COMPLETE

> _Note: this issue was identified against the code base at commit `cd94cc265dda0020a7368311398ddefc098e50a1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
Cause 1 of 1: an in-place stack update that raises an AWS::AutoScaling::AutoScalingGroup's MaxSize and changes one of its resource Tag values is refused by the resource handler, so the stack rolls back.

AWS: changing MaxSize and the Tags property of an existing AWS::AutoScaling::AutoScalingGroup is a no-interruption, in-place update. CloudFormation keeps the same physical id (the group name), the stack reaches UPDATE_COMPLETE, and the group reads back with the new MaxSize and the new tag value. Tags are not carried by UpdateAutoScalingGroup - Smithy: autoscaling UpdateAutoScalingGroup input has MinSize/MaxSize/DesiredCapacity and no Tags member, while CreateOrUpdateTags (input Tags, required) and DeleteTags exist for exactly this - so a provider applies a Tags diff with CreateOrUpdateTags/DeleteTags, not with UpdateAutoScalingGroup. The absence of a Tags member in UpdateAutoScalingGroup is therefore not a reason to refuse the update.

Emulator: the Group resource update ends UPDATE_FAILED with HandlerErrorCode InvalidRequest and the emulator-authored reason 'Resource handler returned message: "this emulator does not change the Tags of an existing AWS::AutoScaling::AutoScalingGroup, which no member of UpdateAutoScalingGroup carries; the resource was left as it is."', and the stack ends UPDATE_ROLLBACK_COMPLETE with StackStatusReason 'The following resource(s) failed to update: [Group]. '. No such refusal exists on AWS: neither the status, nor an InvalidRequest handler error, nor that message text is AWS behaviour.

Why: a client that updates an ASG's size and tags in one template change sees its stack roll back and its tag change discarded. Any further stack operation starts from UPDATE_ROLLBACK_COMPLETE, and templates that tag autoscaling groups (the common case) cannot be updated at all.

Fix: the CloudFormation resource provider for AWS::AutoScaling::AutoScalingGroup - its update handler must classify a Tags-only diff as an in-place update and apply it through the autoscaling CreateOrUpdateTags/DeleteTags path alongside UpdateAutoScalingGroup for the size members, instead of failing the resource.

Traces: autoscaling__cloudformation__ec2.cfn-autoscaling-group-in-place-max-size-retained-replacement-enforces-bounds-then-not-found - the create leg reaches CREATE_COMPLETE, the failure is on the update leg that moves MaxSize 1 -> 2 and a tag value.

## Evidence
Expected: after update_stack with the same AutoScalingGroupName, MaxSize 2 and a changed resource tag value, DescribeStacks reports StackStatus 'UPDATE_COMPLETE', the Group's physical id is unchanged, and DescribeAutoScalingGroups reads MaxSize 2 with the new tag value.

Actual (replay): AssertionError: stack ended 'UPDATE_ROLLBACK_COMPLETE' ('The following resource(s) failed to update: [Group]. '), expected 'UPDATE_COMPLETE'; failed events: [('Group', 'UPDATE_FAILED', 'Resource handler returned message: "this emulator does not change the Tags of an existing AWS::AutoScaling::AutoScalingGroup, which no member of UpdateAutoScalingGroup carries; the resource was left as it is." (RequestToken: <per-run token>, HandlerErrorCode: InvalidRequest)')]. Fixed text of the divergence: the handler message and HandlerErrorCode InvalidRequest; the request token and group name are per-run values.

## Steps to Reproduce
1. CreateStack with an AWS::AutoScaling::AutoScalingGroup (named group, MinSize 0, MaxSize 1, a launch template, one subnet) carrying a resource Tags entry with value 'blue'; wait for CREATE_COMPLETE.
2. UpdateStack with the same template except MaxSize 2 and the same tag key with value 'green'.
3. Poll DescribeStacks: the stack ends UPDATE_ROLLBACK_COMPLETE and DescribeStackEvents shows Group UPDATE_FAILED with HandlerErrorCode InvalidRequest. AWS reaches UPDATE_COMPLETE and the group keeps its name with MaxSize 2 and the new tag value.
