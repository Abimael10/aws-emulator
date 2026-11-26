Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.lifecycle-hook-full-cycle autoscaling.record-heartbeat-after-completion-rejected
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling.lifecycle-hook-full-cycle|run#31|AssertionError|assert dead_hook_message == expected_message, (
Issue-Key: autoscaling.record-heartbeat-after-completion-rejected|run#24|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Commit: a6e1f04ac97a18473409adbfc59bb5a800ec5308

# autoscaling: RecordLifecycleActionHeartbeat's "No active Lifecycle Action found" ValidationError appends an " and HookName <name>" clause AWS does not emit

> _Note: this issue was identified against the code base at commit `a6e1f04ac97a18473409adbfc59bb5a800ec5308`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RecordLifecycleActionHeartbeat

## Description
One net-new cause in the autoscaling lifecycle-action refusal plane. The HTTP status (400) and the Query error code (ValidationError) are both correct; what diverges is the fixed text of the message.

Cause 1 of 1: RecordLifecycleActionHeartbeat's no-active-lifecycle-action refusal carries an extra emulator-authored clause naming the hook.

AWS: Smithy (com.amazonaws.autoscaling#AutoScaling_2011_01_01): RecordLifecycleActionHeartbeat.LifecycleHookName (AsciiStringMaxLen255, required) and .AutoScalingGroupName (ResourceName, required) are required; .LifecycleActionToken (@length 36..36) and .InstanceId (XmlStringMaxLen19, @length 1..19) are both optional. The operation's only modeled error is ResourceContentionFault (500, awsQueryError ResourceContention), so the refusal for "the lifecycle action this request names is no longer outstanding" is a server-side validation and is recorded behavior only. Recorded behavior: HTTP 400, ErrorResponse.Error.Code 'ValidationError', and the message 'No active Lifecycle Action found with instance ID <instance-id>'. In that sentence 'No active Lifecycle Action found with instance ID ' is FIXED text and the ONLY variable part is <instance-id>, taken verbatim from the request's InstanceId member. The message does NOT mention the hook, and AWS emits this same sentence whichever way the action stopped being active — whether CompleteLifecycleAction already resolved it, or the hook it belonged to was removed by DeleteLifecycleHook.

Emulator: answers HTTP 400 with Code 'ValidationError' (both right) but the message 'No active Lifecycle Action found with instance ID <instance-id> and HookName <hook-name>' — AWS's fixed sentence with an extra ' and HookName <hook-name>' clause appended, interpolating the request's LifecycleHookName. The divergence is present on both paths the suite exercises: after CompleteLifecycleAction has consumed the action (record-heartbeat-after-completion-rejected) and after DeleteLifecycleHook has removed the hook (lifecycle-hook-full-cycle). It is not a per-run value difference: the hook name shown is a request value filling the emulator's extra template, and the defect is the extra clause itself.

Why: the Query error code 'ValidationError' is shared by every semantic refusal this operation can raise, so the message IS the discriminator a client branches on. A caller, a recorded-golden comparison or a retry/branching rule that matches AWS's sentence (equality, or a trailing-anchored match on the instance id) does not recognize the refusal and reports an unknown failure instead of "the lifecycle action is already resolved" — exactly the condition a lifecycle-hook driver must treat as benign/terminal rather than retryable.

Fix: the RecordLifecycleActionHeartbeat handler's lifecycle-action lookup/validation, where the no-active-action ValidationError message is composed — emit AWS's sentence with the instance id as its sole interpolated value and drop the ' and HookName ...' clause. Keep the status 400 and the code ValidationError as they are.

Related but non-covering open issue: 20261118T090055Z-autoscaling-lifecycle-action-and-health-check-update-refusals-carry.md touches RecordLifecycleActionHeartbeat, but its cause 3 is a different refusal (a request supplying NEITHER a token NOR an instance id against a nonexistent group, where the group lookup pre-empts the presence check and AWS answers the fixed 'No Lifecycle Action found because no token or instance ID is provided'). It does not cover this message, which is the no-active-action refusal for a request that DOES carry a valid InstanceId; neither it nor any other open autoscaling refusal issue carries these checks' replay keys.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout.

[mismatch] autoscaling.record-heartbeat-after-completion-rejected — after an instance reached Pending:Wait on an EC2_INSTANCE_LAUNCHING hook, a heartbeat was accepted (correctly), CompleteLifecycleAction(CONTINUE) resolved the action, the check confirmed the instance had left Pending:Wait, and the next RecordLifecycleActionHeartbeat was then refused. Code was 'ValidationError' (matched). Message: expected 'No active Lifecycle Action found with instance ID i-00000000000000001', got 'No active Lifecycle Action found with instance ID i-00000000000000001 and HookName sim2real-behav-hook-d725705dec3a'.

[mismatch] autoscaling.lifecycle-hook-full-cycle — same group/hook flow, but the action was resolved and the hook then removed with DeleteLifecycleHook; the subsequent RecordLifecycleActionHeartbeat against the deleted hook was refused with Code 'ValidationError' (matched) and message: expected 'No active Lifecycle Action found with instance ID i-00000000000000001', got 'No active Lifecycle Action found with instance ID i-00000000000000001 and HookName sim2real-behav-hook-02a3d7b738f9'.

The instance ids and hook names quoted are one run's generated values filling the templates; the requirement is the fixed text 'No active Lifecycle Action found with instance ID ' plus the request's InstanceId, and nothing after it.

## Steps to Reproduce
1. Create a launch template and an Auto Scaling group with MinSize=0, MaxSize=1, DesiredCapacity=0 in the shared VPC/subnet.
2. PutLifecycleHook with LifecycleTransition='autoscaling:EC2_INSTANCE_LAUNCHING', HeartbeatTimeout=300, DefaultResult='ABANDON'.
3. SetDesiredCapacity(DesiredCapacity=1, HonorCooldown=False) and poll DescribeAutoScalingInstances until the new instance is in 'Pending:Wait'; note its InstanceId.
4. Resolve the outstanding action by one of the two paths: (a) CompleteLifecycleAction(LifecycleActionResult='CONTINUE', InstanceId=<iid>) and poll until the instance leaves Pending:Wait; or (b) resolve it and then DeleteLifecycleHook(LifecycleHookName=<hook>).
5. Call RecordLifecycleActionHeartbeat(LifecycleHookName=<hook>, AutoScalingGroupName=<group>, InstanceId=<iid>).
6. Read ErrorResponse.Error.Code and ErrorResponse.Error.Message: the code is 'ValidationError' as AWS answers, but the message carries an extra ' and HookName <hook>' clause after AWS's fixed sentence.
