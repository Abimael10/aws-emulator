Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ecs.cdk-l2-ecs-asg-capacity-provider-managed-scaling-reaches-the-ecs-api cloudformation__ecs.deploy-ecs-capacity-provider-with-nonexistent-asg-rejected
Issue-Key: cloudformation__ecs.cdk-l2-ecs-asg-capacity-provider-managed-scaling-reaches-the-ecs-api|run#43|AssertionError|assert len(policies) == 1, (
Issue-Key: cloudformation__ecs.deploy-ecs-capacity-provider-with-nonexistent-asg-rejected|run#18|AssertionError|assert got == _REJECTED, f"handler reason {got!r}, expected {_REJECTED!r}"
Issue-Commit: d3cdd55e8b72332e5c71fd34997111c995fed5da

# cloudformation__ecs: 2 net-new causes — managed scaling creates no target-tracking policy on the capacity provider's ASG, and the CREATE_FAILED reason for a nonexistent ASG is not AWS's wrapped handler message

> _Note: this issue was identified against the code base at commit `d3cdd55e8b72332e5c71fd34997111c995fed5da`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCapacityProvider

## Description
Two net-new root causes in the ECS capacity-provider surface reached through CloudFormation. The open issue 20261111T064629Z-cloudwatch-ec2-ecs-ecs-managed-scaling-never-creates-the-target-tracking.md records the same managed-scaling defect for a different service/check set and does not carry this check's replay key.

Cause 1 of 2: enabling managed scaling on an EC2_AUTOSCALING capacity provider creates no scaling policy on the Auto Scaling group.
AWS: Smithy — ecs CreateCapacityProvider takes autoScalingGroupProvider (AutoScalingGroupProvider) whose managedScaling.status may be ENABLED. Recorded behavior: when managed scaling is ENABLED, ECS itself creates exactly one target-tracking scaling policy on the referenced Auto Scaling group (the AWS-managed ECSManagedAutoScalingPolicy tracking the capacity-provider reservation metric), so autoscaling DescribePolicies on that ASG returns one policy even though the template declares none.
Emulator: DescribePolicies on the ASG returns an empty list — the ASG has 0 scaling policies.
Why: the capacity provider never scales the ASG, so cluster capacity never grows and any client inspecting or updating the managed policy (or asserting that ECS owns it) sees nothing; a template that deliberately declares no policy of its own ends up with no scaling at all.
Fix: the CreateCapacityProvider/UpdateCapacityProvider handler in the ECS service — when managedScaling.status is ENABLED, create the managed target-tracking policy on the referenced Auto Scaling group (and remove it when managed scaling is disabled).
Failing check: cloudformation__ecs.cdk-l2-ecs-asg-capacity-provider-managed-scaling-reaches-the-ecs-api.

Cause 2 of 2: the CloudFormation CREATE_FAILED reason for an AWS::ECS::CapacityProvider naming a nonexistent Auto Scaling group is the bare service message instead of AWS's wrapped resource-handler reason.
AWS: Smithy — CreateCapacityProvider declares ClientException (client error, HTTP 400). Recorded behavior: ECS refuses the call with the message 'The specified Auto Scaling group does not exist. Specify a valid Auto Scaling group and try again.', and the CloudFormation resource handler surfaces it in the stack event's ResourceStatusReason wrapped as 'Invalid request provided: CreateCapacityProvider error: The specified Auto Scaling group does not exist. Specify a valid Auto Scaling group and try again. (Service: Ecs, Status Code: 400, Request ID: <request id>) (SDK Attempt Count: <n>)', with HandlerErrorCode 'InvalidRequest'. The fixed text is 'Invalid request provided: CreateCapacityProvider error: ' + the service message + ' (Service: Ecs, Status Code: 400, Request ID: ' + ... + ') (SDK Attempt Count: ' + ... + ')'; only the request id and the attempt count are per-run values.
Emulator: the handler error code is right ('InvalidRequest') but the reason is only the raw ECS message 'The specified Auto Scaling group does not exist. Specify a valid Auto Scaling group and try again.' — the 'Invalid request provided: <Operation> error: ' prefix and the '(Service: ... Status Code: ... Request ID: ...) (SDK Attempt Count: ...)' suffix are absent.
Why: tooling and operators parse the handler reason to recover the failing operation, the originating service and the status code; with the wrapper stripped the reason is indistinguishable from any other ECS message and the failing API call is unidentifiable.
Fix: the CloudFormation resource-handler error wrapping for AWS::ECS::CapacityProvider (the provisioner's handler-failure reason formatter) — wrap the downstream service exception in AWS's 'Invalid request provided: <Operation> error: ... (Service: ..., Status Code: ..., Request ID: ...) (SDK Attempt Count: ...)' form.
Failing check: cloudformation__ecs.deploy-ecs-capacity-provider-with-nonexistent-asg-rejected.

## Evidence
Cause 1 — expected: with managedScaling ENABLED and no policy declared in the template, autoscaling DescribePolicies on the capacity provider's ASG returns exactly 1 policy.
Actual (cdk-l2-ecs-asg-capacity-provider-managed-scaling-reaches-the-ecs-api): AssertionError: the ASG '<asg name>' has 0 scaling policies ([]); with managedScaling ENABLED and no policy declared in the template, ECS must have created exactly one.

Cause 2 — expected handler reason ('Invalid request provided: CreateCapacityProvider error: The specified Auto Scaling group does not exist. Specify a valid Auto Scaling group and try again. (Service: Ecs, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)', 'InvalidRequest').
Actual (deploy-ecs-capacity-provider-with-nonexistent-asg-rejected): handler reason ('The specified Auto Scaling group does not exist. Specify a valid Auto Scaling group and try again.', 'InvalidRequest').

## Steps to Reproduce
1. Replay the behavioral suite and re-run both checks in trace_ids.
2. Cause 1: deploy a CDK L2 ECS cluster whose capacity provider wraps an Auto Scaling group with managed scaling enabled and no scaling policy in the template; then call autoscaling DescribePolicies with AutoScalingGroupName=<the ASG>.
3. Cause 2: CreateStack a template with an AWS::ECS::CapacityProvider whose AutoScalingGroupProvider.AutoScalingGroupArn names a nonexistent Auto Scaling group; poll to the terminal status and read the resource's ResourceStatusReason from DescribeStackEvents.
