Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ecs.cfn-ecs-capacity-provider-managed-scaling-update-in-place-then-replaced-then-inactive cloudformation__ecs.cfn-ecs-capacity-provider-managed-termination-protection-without-scale-in-protection-rejected cloudformation__ecs.deploy-ecs-task-definition-dependson-fn-if-unresolved-condition-rejected
Issue-Key: cloudformation__ecs.cfn-ecs-capacity-provider-managed-scaling-update-in-place-then-replaced-then-inactive|_expect#20|AssertionError|raise AssertionError(
Issue-Key: cloudformation__ecs.cfn-ecs-capacity-provider-managed-termination-protection-without-scale-in-protection-rejected|run#21|AssertionError|assert bad_stack_obj["StackStatus"] == "ROLLBACK_COMPLETE", (
Issue-Key: cloudformation__ecs|CreateStack|ValidationError
Issue-Commit: 384e9c06f8b850c3e6c1129231f41c38ae5339ba

# cloudformation__ecs: an AWS::ECS::CapacityProvider cannot be updated in place, an invalid managed-termination-protection provider is created anyway, and an undefined Fn::If condition is refused with the wrong message (3 causes)

> _Note: this issue was identified against the code base at commit `384e9c06f8b850c3e6c1129231f41c38ae5339ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
Three net-new root causes in the CloudFormation handling of ECS resources.

Cause 1 of 3: updating the properties of an existing AWS::ECS::CapacityProvider.
AWS: an UpdateStack that changes a capacity provider's ManagedScaling properties is an in-place update — CloudFormation calls ECS UpdateCapacityProvider and the stack reaches UPDATE_COMPLETE with the provider's new managedScaling values readable through DescribeCapacityProviders (recorded behavior only; Smithy models ECS UpdateCapacityProvider, whose input carries autoScalingGroupProvider.managedScaling, so an in-place property change is the modeled path).
Emulator: the resource's UPDATE fails with the non-AWS reason 'NotImplemented: this emulator does not change the properties of an existing AWS::ECS::CapacityProvider in place; the resource was left as it is.', the stack ends UPDATE_ROLLBACK_COMPLETE with StatusReason 'The following resource(s) failed to update: [CapacityProvider].', and the provider keeps its old properties.
Why: every stack that tunes managed scaling after first deploy rolls back, so no stack can reach the replaced/INACTIVE lifecycle the scenario continues into; 'NotImplemented' is also not a status reason AWS ever writes.
Fix: the AWS::ECS::CapacityProvider resource provider in the CloudFormation crate — implement the in-place update path onto ECS UpdateCapacityProvider. Note the underlying ECS operation is itself an unimplemented scaffold stub (services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/ops/update_capacity_provider.rs), which the suite reports as ALREADY FILED for ecs.capacity-provider-create-update-delete-lifecycle; that stub issue covers the ECS operation, this cause covers the CloudFormation resource that must call it.
Traces: cloudformation__ecs.cfn-ecs-capacity-provider-managed-scaling-update-in-place-then-replaced-then-inactive.

Cause 2 of 3: AWS::ECS::CapacityProvider with ManagedTerminationProtection ENABLED over an Auto Scaling group that has no instance scale-in protection.
AWS: the create is refused and the stack ends ROLLBACK_COMPLETE — managed termination protection requires the Auto Scaling group to have instance protection from scale-in enabled, so ECS CreateCapacityProvider refuses the provider and CloudFormation rolls the stack back (recorded behavior only for the exact error text; the rule is AWS's documented constraint on managedTerminationProtection).
Emulator: the stack ends CREATE_COMPLETE and the invalid provider exists.
Why: the negative control passes silently, so a template that AWS rejects deploys here and every later read of that provider diverges.
Fix: the ECS CreateCapacityProvider validation of autoScalingGroupProvider.managedTerminationProtection against the named Auto Scaling group (and hence the CloudFormation resource's create path, which must surface that refusal as a rollback).
Traces: cloudformation__ecs.cfn-ecs-capacity-provider-managed-termination-protection-without-scale-in-protection-rejected.

Cause 3 of 3: a template whose Fn::If names a condition that is not defined.
AWS: 400 ValidationError 'Template error: unresolved condition dependency HEALTHY in Fn::If' — template 'Template error: unresolved condition dependency ' + the condition name taken from the template + ' in Fn::If' (recorded behavior only).
Emulator: 400 ValidationError 'Template format error: Condition HEALTHY is not defined.' — the right status and class, the wrong sentence (it reports a template FORMAT error, which is the class AWS uses for malformed templates rather than for an unresolved condition dependency).
Why: deployment tooling that matches CloudFormation's validation text to tell a malformed template from an unresolved reference mis-classifies the failure.
Fix: the CloudFormation template validator's condition-resolution error message.
Traces: cloudformation__ecs.deploy-ecs-task-definition-dependson-fn-if-unresolved-condition-rejected. The condition name in the message is taken from the request; 'HEALTHY' is this check's value, not part of the template.

## Evidence
Behavioral replay of simparity-behavioral-aws-v1; all three checks are listed under '### cloudformation__ecs' and none is marked ALREADY FILED or OWNED.
Cause 1 — AssertionError: stack ended 'UPDATE_ROLLBACK_COMPLETE' ('The following resource(s) failed to update: [CapacityProvider]. '), expected 'UPDATE_COMPLETE'; failed events: [('CapacityProvider', 'UPDATE_FAILED', 'NotImplemented: this emulator does not change the properties of an existing AWS::ECS::CapacityProvider in place; the resource was left as it is.')].
Cause 2 — AssertionError: stack with an unprotected Auto Scaling group ended 'CREATE_COMPLETE', expected 'ROLLBACK_COMPLETE'.
Cause 3 — AssertionError: expected ('ValidationError', 'Template error: unresolved condition dependency HEALTHY in Fn::If'), got ('ValidationError', 'Template format error: Condition HEALTHY is not defined.').

## Steps to Reproduce
1. git_clone this checkout and boot the emulator.
2. Cause 1: CreateStack with an AWS::ECS::CapacityProvider (ASG + ManagedScaling), wait CREATE_COMPLETE, then UpdateStack changing only the ManagedScaling properties and poll the stack status and events.
3. Cause 2: CreateStack with an AWS::ECS::CapacityProvider whose AutoScalingGroupProvider.ManagedTerminationProtection is ENABLED over an Auto Scaling group that has NewInstancesProtectedFromScaleIn disabled; poll the terminal stack status.
4. Cause 3: CreateStack with a template whose resource uses Fn::If naming a condition absent from the Conditions block; read the ValidationError code and message.
5. Re-replay the three check ids in trace_ids.
