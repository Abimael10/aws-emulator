Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__stepfunctions.cfn-activity-name-over-max-length-rejected-with-boundary-control cloudformation__stepfunctions.cfn-activity-tag-update-keeps-arn-and-removed-on-stack-delete cloudformation__stepfunctions.cfn-state-machine-tags-value-change-and-key-removal-update-complete cloudformation__stepfunctions.cfn-statemachine-tracingconfiguration-unknown-property-rejected-then-round-trips-enabled
Issue-Key: cloudformation__stepfunctions.cfn-activity-name-over-max-length-rejected-with-boundary-control|stack_failure_reason#9|KeyError|if e["PhysicalResourceId"] == stack_id and e["ResourceStatus"] == "CREATE_FAILED":
Issue-Key: cloudformation__stepfunctions.cfn-activity-tag-update-keeps-arn-and-removed-on-stack-delete|run#15|AssertionError|assert status == "UPDATE_COMPLETE", (
Issue-Key: cloudformation__stepfunctions.cfn-state-machine-tags-value-change-and-key-removal-update-complete|run#18|AssertionError|assert status == "UPDATE_COMPLETE", f"stack update ended {status!r}, expected UPDATE_COMPLETE"
Issue-Key: cloudformation__stepfunctions.cfn-statemachine-tracingconfiguration-unknown-property-rejected-then-round-trips-enabled|run#10|AssertionError|assert bad_status == "ROLLBACK_COMPLETE", f"expected ROLLBACK_COMPLETE, got {bad_status!r}"
Issue-Commit: 16dd3a69d4b4af3373dd95173ca2a60641ca57cc

# 3 net-new causes on CloudFormation Step Functions resources: stack events omit PhysicalResourceId, tag-only updates roll back, and an unknown TracingConfiguration property is accepted

> _Note: this issue was identified against the code base at commit `16dd3a69d4b4af3373dd95173ca2a60641ca57cc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
Three distinct defects reached through CloudFormation stacks that declare AWS::StepFunctions::* resources. Each cause has its own failing check ids.

Cause 1 of 3: the StackEvent rows DescribeStackEvents returns omit the PhysicalResourceId member.
AWS: Smithy: cloudformation#StackEvent declares PhysicalResourceId ("The name or unique identifier associated with the physical instance of the resource"); AWS emits the member on every event row — for a stack-level row it carries the stack's own id, which is how a client separates the stack's events from its resources'.
Emulator: the member is absent from the serialized event, so `e["PhysicalResourceId"]` raises KeyError and the stack-level CREATE_FAILED row cannot be located at all.
Why: any client that reads a failure reason out of DescribeStackEvents — the CLI, the console's Events tab, and every deploy wrapper that reports why a stack failed — cannot identify the rows it needs; the reason itself becomes unreachable.
Fix: the CloudFormation DescribeStackEvents serializer / the stack-event state that records each row's physical id.
Traces: cloudformation__stepfunctions.cfn-activity-name-over-max-length-rejected-with-boundary-control

Cause 2 of 3: a stack update that only changes tags on a Step Functions resource is rolled back.
AWS: recorded behavior only. Changing a tag value and removing a tag key on an AWS::StepFunctions::Activity or AWS::StepFunctions::StateMachine is an in-place update: the stack reaches UPDATE_COMPLETE, the resource keeps its ARN, and ListTagsForResource reports the template's new tag set.
Emulator: the resource's update reports UPDATE_FAILED and the stack ends UPDATE_ROLLBACK_COMPLETE (for the Activity the event sequence is UPDATE_FAILED then UPDATE_COMPLETE then stack UPDATE_ROLLBACK_COMPLETE), so the new tags are never applied.
Why: tags on state machines and activities cannot be evolved through the stack that owns them; every tag-only deploy appears to fail and silently reverts, leaving cost-allocation and tag-based access control on stale values.
Fix: the AWS::StepFunctions::Activity and AWS::StepFunctions::StateMachine resource handlers' Update path (tag diffing into TagResource/UntagResource).
Traces: cloudformation__stepfunctions.cfn-activity-tag-update-keeps-arn-and-removed-on-stack-delete, cloudformation__stepfunctions.cfn-state-machine-tags-value-change-and-key-removal-update-complete

Cause 3 of 3: an unknown property inside TracingConfiguration on an AWS::StepFunctions::StateMachine is accepted.
AWS: recorded behavior only. A property the resource schema does not declare fails the resource, so the stack settles at ROLLBACK_COMPLETE and nothing is created; the same template with only the declared Enabled property round-trips.
Emulator: the stack reaches CREATE_COMPLETE, so the malformed TracingConfiguration is accepted and the state machine is created.
Why: a typo or an unsupported property in the template is deployed silently instead of being refused, so the client believes a configuration is in effect that is not.
Fix: the AWS::StepFunctions::StateMachine resource handler's property-schema validation (reject undeclared members of nested property structures).
Traces: cloudformation__stepfunctions.cfn-statemachine-tracingconfiguration-unknown-property-rejected-then-round-trips-enabled

## Evidence
Cause 1: `if e["PhysicalResourceId"] == stack_id and e["ResourceStatus"] == "CREATE_FAILED"` raises `KeyError: 'PhysicalResourceId'` over the rows DescribeStackEvents returned.
Cause 2: AssertionError: update ended 'UPDATE_ROLLBACK_COMPLETE': [('MyActivity', 'CREATE_COMPLETE'), ('<stack>', 'CREATE_COMPLETE'), ('MyActivity', 'UPDATE_FAILED'), ('MyActivity', 'UPDATE_COMPLETE'), ('<stack>', 'UPDATE_ROLLBACK_COMPLETE')] — expected UPDATE_COMPLETE; and "stack update ended 'UPDATE_ROLLBACK_COMPLETE', expected UPDATE_COMPLETE" for the state machine.
Cause 3: AssertionError: expected ROLLBACK_COMPLETE, got 'CREATE_COMPLETE'.

## Steps to Reproduce
Replay the behavioral suite and re-run exactly these checks:
1. cloudformation__stepfunctions.cfn-activity-name-over-max-length-rejected-with-boundary-control
2. cloudformation__stepfunctions.cfn-activity-tag-update-keeps-arn-and-removed-on-stack-delete
3. cloudformation__stepfunctions.cfn-state-machine-tags-value-change-and-key-removal-update-complete
4. cloudformation__stepfunctions.cfn-statemachine-tracingconfiguration-unknown-property-rejected-then-round-trips-enabled
