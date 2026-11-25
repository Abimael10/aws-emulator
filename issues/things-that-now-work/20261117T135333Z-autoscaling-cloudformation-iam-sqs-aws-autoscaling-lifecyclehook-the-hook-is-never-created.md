Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__cloudformation__iam__sqs.deploy-asg-lifecyclehook-notification-target-and-role-round-trip autoscaling__cloudformation__iam__sqs.deploy-asg-lifecyclehook-rolearn-without-publish-permission-rejected
Issue-Key: autoscaling__cloudformation__iam__sqs.deploy-asg-lifecyclehook-notification-target-and-role-round-trip|run#17|AssertionError|assert len(hooks) == 1, f"DescribeLifecycleHooks({hook_name!r}) returned {hooks!r}"
Issue-Key: autoscaling__cloudformation__iam__sqs.deploy-asg-lifecyclehook-rolearn-without-publish-permission-rejected|run#14|AssertionError|assert status == "ROLLBACK_COMPLETE", (
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# AWS::AutoScaling::LifecycleHook: the hook is never created in Auto Scaling state, and an unpermitted RoleARN is accepted

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutLifecycleHook

## Description
Cause 1 of 2: a lifecycle hook deployed by a stack does not exist in Auto Scaling.
AWS: Smithy (autoscaling PutLifecycleHook / DescribeLifecycleHooks) — a hook put on an Auto Scaling group is returned by DescribeLifecycleHooks(AutoScalingGroupName, LifecycleHookNames=[name]) with its NotificationTargetARN, RoleARN, LifecycleTransition and DefaultResult; the recording shows exactly one hook returned after the stack reaches CREATE_COMPLETE.
Emulator: DescribeLifecycleHooks returns an empty list for the hook the stack just created: `AssertionError: DescribeLifecycleHooks('sim2real-cfnhook-rt-hook-<id>') returned []`. The CFN resource reports success while no hook is written to Auto Scaling state.
Why: a client reads back nothing for a resource CloudFormation says exists; nothing can be updated, deleted or exercised through the hook, and no lifecycle notification can ever be published to its target.
Fix: the AWS::AutoScaling::LifecycleHook resource handler / the PutLifecycleHook state write it must perform (the hook must be stored on the group and returned by DescribeLifecycleHooks).
Traces: autoscaling__cloudformation__iam__sqs.deploy-asg-lifecyclehook-notification-target-and-role-round-trip.

Cause 2 of 2: a hook whose RoleARN cannot publish to the notification target is accepted.
AWS: Smithy PutLifecycleHook declares ValidationError (400); AWS validates that the supplied RoleARN grants Auto Scaling permission to publish to the NotificationTargetARN and refuses otherwise, so the stack's resource create fails and the stack settles ROLLBACK_COMPLETE.
Emulator: the stack with an unpermitted hook role settles `CREATE_COMPLETE`: `AssertionError: stack with an unpermitted lifecycle-hook role ended 'CREATE_COMPLETE', expected ROLLBACK_COMPLETE`.
Why: the misconfiguration is never reported; on real AWS the deploy fails immediately, here it "succeeds" and lifecycle notifications would silently never arrive.
Fix: PutLifecycleHook's validation of RoleARN against the notification target (and the CFN handler surfacing that refusal as a resource failure).
Traces: autoscaling__cloudformation__iam__sqs.deploy-asg-lifecyclehook-rolearn-without-publish-permission-rejected.

## Evidence
Cause 1 — expected: DescribeLifecycleHooks returns the one hook created by the stack.
Actual: `AssertionError: DescribeLifecycleHooks('sim2real-cfnhook-rt-hook-0fffb4094c38') returned []`

Cause 2 — expected stack status ROLLBACK_COMPLETE (PutLifecycleHook refuses a RoleARN with no publish permission on the notification target).
Actual: `AssertionError: stack with an unpermitted lifecycle-hook role ended 'CREATE_COMPLETE', expected ROLLBACK_COMPLETE`

## Steps to Reproduce
Cause 1: CreateStack with an AWS::AutoScaling::AutoScalingGroup, a queue as NotificationTargetARN, a role with sqs:SendMessage, and an AWS::AutoScaling::LifecycleHook; after CREATE_COMPLETE call DescribeLifecycleHooks(AutoScalingGroupName=<asg>, LifecycleHookNames=[<hook>]). Observed []; expected the hook.
Cause 2: deploy the same stack but with a RoleARN that grants no publish permission on the notification target; poll DescribeStacks. Observed CREATE_COMPLETE; expected ROLLBACK_COMPLETE.
