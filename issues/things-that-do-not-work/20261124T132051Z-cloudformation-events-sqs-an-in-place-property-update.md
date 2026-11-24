Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__events__sqs.cfn-events-event-bus-in-place-update-retained-replacement-then-not-found
Issue-Key: cloudformation__events__sqs.cfn-events-event-bus-in-place-update-retained-replacement-then-not-found|_expect#32|AssertionError|raise AssertionError(
Issue-Commit: f94f81a4e4acb5f06aec5c20cf2f41781bd39454

# cloudformation__events__sqs: an in-place property update of AWS::Events::Rule fails with NotImplemented and rolls the whole stack back

> _Note: this issue was identified against the code base at commit `f94f81a4e4acb5f06aec5c20cf2f41781bd39454`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
Cause 1 of 1: CloudFormation cannot update an existing AWS::Events::Rule in place.

AWS: for AWS::Events::Rule only Name is create-only; EventPattern, Description, State, ScheduleExpression, RoleArn and Targets are all "update requires: no interruption". An UpdateStack whose only change to the rule is one of those properties mutates the live rule (PutRule/PutTargets are upserts) and the stack reaches UPDATE_COMPLETE with the rule keeping its PhysicalResourceId (the rule name). Smithy (eventbridge, com-amazonaws-eventbridge-awsevents): PutRule/PutTargets carry no create-only or immutability constraint on EventPattern/State/Description/Targets — the only identity member is Rule/Name, so changing a pattern or target is a normal mutation, not a replacement. The stack-event text itself is recorded behavior only.

Emulator: the Rule resource is reported UPDATE_FAILED with the ResourceStatusReason "NotImplemented: this emulator does not change the properties of an existing AWS::Events::Rule in place; the resource was left as it is." and the stack terminates in UPDATE_ROLLBACK_COMPLETE with StackStatusReason "The following resource(s) failed to update: [Rule]." (the bracketed logical id is the only variable part of that sentence; the NotImplemented sentence is wholly emulator-authored and has no AWS counterpart). Because the update is refused at the resource handler, every sibling change in the same update — here an EventBus replacement governed by UpdateReplacePolicy: Retain — is rolled back too, so nothing about the retained-replacement contract can be observed either.

Why: any client that evolves an EventBridge rule through CloudFormation (changing an EventPattern, flipping State, adding a target) gets a failed update and a rollback where AWS gives a successful in-place update. A deploy pipeline sees UPDATE_ROLLBACK_COMPLETE and a NotImplemented reason that no AWS error vocabulary contains, so it cannot classify the failure or retry it.

Fix: the AWS::Events::Rule resource provider's update path in the CloudFormation resource-handler registry — it must apply a no-interruption update (re-PutRule the rule and reconcile Targets) instead of returning NotImplemented, and the stack-event reason must stop carrying the emulator-authored sentence.

Traces: cloudformation__events__sqs.cfn-events-event-bus-in-place-update-retained-replacement-then-not-found (replay verdict: mismatch; stack ended 'UPDATE_ROLLBACK_COMPLETE', expected 'UPDATE_COMPLETE').

Related but not covering this: 20261110T232651Z-cloudformation-events-sqs-a-stack-s-aws-events.md and 20261115T015715Z-cloudformation-events-sqs-a-stack-provisioned-aws-events.md are about CREATING a rule on a same-stack bus and about that rule not routing — neither carries the update path.

## Evidence
Expected: UpdateStack that changes only AWS::Events::Rule EventPattern/Targets succeeds — resource status UPDATE_COMPLETE (no interruption), stack status UPDATE_COMPLETE, rule PhysicalResourceId unchanged.

Actual (replay): AssertionError: stack ended 'UPDATE_ROLLBACK_COMPLETE' ('The following resource(s) failed to update: [Rule]. '), expected 'UPDATE_COMPLETE'; failed events: [('Rule', 'UPDATE_FAILED', 'NotImplemented: this emulator does not change the properties of an existing AWS::Events::Rule in place; the resource was left as it is.')]

Fixed text of the diverging reason: "NotImplemented: this emulator does not change the properties of an existing AWS::Events::Rule in place; the resource was left as it is." — entirely emulator-authored. In the stack reason "The following resource(s) failed to update: [Rule]." the fixed text is "The following resource(s) failed to update: [...]" and the only value from the request is the logical resource id.

## Steps to Reproduce
1. CreateStack with an AWS::Events::EventBus (Name, Description, UpdateReplacePolicy: Retain), an AWS::SQS::Queue, and an AWS::Events::Rule on that bus whose EventPattern is {"source": [<src1>]} with the queue as target. Wait for CREATE_COMPLETE.
2. UpdateStack with the same template but a different rule EventPattern source (and a changed bus Description).
3. Poll DescribeStacks/DescribeStackEvents: the Rule row is UPDATE_FAILED with the NotImplemented reason and the stack reaches UPDATE_ROLLBACK_COMPLETE instead of UPDATE_COMPLETE.
