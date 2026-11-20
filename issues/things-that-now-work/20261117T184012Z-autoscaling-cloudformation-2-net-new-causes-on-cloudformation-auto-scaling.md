Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__cloudformation.deploy-cfn-asg-deletion-protection-blocks-delete-until-cleared autoscaling__cloudformation.deploy-cfn-asg-invalid-deletion-protection-value-rejected autoscaling__cloudformation.deploy-cfn-asg-lifecycle-hook-specification-list-inline-round-trip-and-update
Issue-Key: autoscaling__cloudformation.deploy-cfn-asg-deletion-protection-blocks-delete-until-cleared|_asg_handler_error#26|AssertionError|assert inner, f"handler message had an unexpected shape: {outer['msg']!r}"
Issue-Key: autoscaling__cloudformation.deploy-cfn-asg-invalid-deletion-protection-value-rejected|_asg_handler_error#12|AssertionError|assert inner, f"handler message had an unexpected shape: {outer['msg']!r}"
Issue-Key: autoscaling__cloudformation.deploy-cfn-asg-lifecycle-hook-specification-list-inline-round-trip-and-update|run#21|AssertionError|assert status == "UPDATE_COMPLETE", f"updating update on {stack_id} ended {status!r}"
Issue-Commit: 16dd3a69d4b4af3373dd95173ca2a60641ca57cc

# 2 net-new causes on CloudFormation Auto Scaling resources: the handler failure reason drops the SDK error suffix, and an inline LifecycleHookSpecificationList update rolls back

> _Note: this issue was identified against the code base at commit `16dd3a69d4b4af3373dd95173ca2a60641ca57cc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
Two distinct defects in the CloudFormation AWS::AutoScaling::AutoScalingGroup resource handler. Each cause has its own failing check ids.

Cause 1 of 2: the ResourceStatusReason of an Auto Scaling handler failure carries the inner service message with no SDK error suffix.
AWS: recorded behavior only. The reason is wrapped twice: the outer CloudFormation wrapper `Resource handler returned message: "<inner>" (RequestToken: <uuid>, HandlerErrorCode: <code>)` and, inside it, the SDK's rendering of the Auto Scaling error `<service message> (Service: AutoScaling, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)`. Fixed text: the literal `(Service: AutoScaling, Status Code: 400, Request ID: ` and ` (SDK Attempt Count: ` fragments and their punctuation; variable parts: the service message (from the request and the group's state), the request id and the attempt count (per run).
Emulator: the outer wrapper is present but the inner message is the bare Auto Scaling message with the whole ` (Service: AutoScaling, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)` suffix missing — e.g. `You cannot delete this group because its deletion protection is set to prevent-all-deletion. ...` and `1 validation error detected: Value '<value>' at 'deletionProtection' failed to satisfy constraint: Member must satisfy enum value set: [none, prevent-force-deletion, prevent-all-deletion]` with nothing after them.
Why: a client that parses the reason to recover the originating service, the HTTP status or the request id (which is how a failed deploy is correlated with a service-side log) finds none of them; reason-matching automation that pins the AWS shape fails.
Fix: the AWS::AutoScaling::AutoScalingGroup resource handler's failure-reason serializer (it must re-render the downstream service error with the SDK suffix before wrapping it).
Traces: autoscaling__cloudformation.deploy-cfn-asg-deletion-protection-blocks-delete-until-cleared, autoscaling__cloudformation.deploy-cfn-asg-invalid-deletion-protection-value-rejected

Cause 2 of 2: a stack update that changes an inline LifecycleHookSpecificationList on an AWS::AutoScaling::AutoScalingGroup is rolled back.
AWS: recorded behavior only. Changing the inline LifecycleHookSpecificationList of an existing group is an in-place update: the stack reaches UPDATE_COMPLETE and the group's hooks match the new list.
Emulator: the update ends UPDATE_ROLLBACK_COMPLETE, so the new hook specification is never applied and the group keeps the pre-update hooks.
Why: a client cannot evolve the lifecycle hooks of a group it already deployed; every such update appears to fail and silently reverts.
Fix: the AWS::AutoScaling::AutoScalingGroup resource handler's Update path for the LifecycleHookSpecificationList property.
Traces: autoscaling__cloudformation.deploy-cfn-asg-lifecycle-hook-specification-list-inline-round-trip-and-update

## Evidence
Cause 1: AssertionError: "handler message had an unexpected shape: 'You cannot delete this group because its deletion protection is set to prevent-all-deletion. If you are sure that you want to delete this group, remove deletion protection for the group and try again.'" and "handler message had an unexpected shape: \"1 validation error detected: Value 'prevent-every-deletion' at 'deletionProtection' failed to satisfy constraint: Member must satisfy enum value set: [none, prevent-force-deletion, prevent-all-deletion]\"". The check's regex requires the inner message to end with ` (Service: AutoScaling, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)`.

Cause 2: AssertionError: "updating update on arn:aws:cloudformation:...:stack/<name>/<uuid> ended 'UPDATE_ROLLBACK_COMPLETE'" (expected UPDATE_COMPLETE).

## Steps to Reproduce
Replay the behavioral suite and re-run exactly these checks:
1. autoscaling__cloudformation.deploy-cfn-asg-deletion-protection-blocks-delete-until-cleared
2. autoscaling__cloudformation.deploy-cfn-asg-invalid-deletion-protection-value-rejected
3. autoscaling__cloudformation.deploy-cfn-asg-lifecycle-hook-specification-list-inline-round-trip-and-update
