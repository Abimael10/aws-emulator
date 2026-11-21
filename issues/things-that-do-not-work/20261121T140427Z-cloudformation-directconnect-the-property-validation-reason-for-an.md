Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__directconnect.deploy-directconnect-resiliency-group-invalid-model-rejected-and-rolled-back
Issue-Key: cloudformation__directconnect.deploy-directconnect-resiliency-group-invalid-model-rejected-and-rolled-back|run#16|AssertionError|assert found == want_errors, f"validation errors of the create: expected {want_errors!r}, got {found!r}"
Issue-Commit: 13b3986dce84729d3e2f84097c5bca4e6c6a5a22

# cloudformation__directconnect: the PROPERTY_VALIDATION reason for an invalid enum value echoes the rejected value instead of AWS's redacted '<REDACTED> is not a valid enum value' form

> _Note: this issue was identified against the code base at commit `13b3986dce84729d3e2f84097c5bca4e6c6a5a22`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEvents

## Description
ONE net-new root cause of service `cloudformation__directconnect`, grounded in the single surviving failing check in trace_ids. The check is not marked ALREADY FILED and not marked OWNED in the replay report, and no open issue of this service carries it (the two other failures under `cloudformation__directconnect__ec2` are OWNED by 20261109T120136Z-cloudformation-directconnect-ec2-createstack-refuses-aws-directconnect-directconnectgateway.md and were skipped).

=== Cause 1 of 1: the enum property-validation reason is not redacted ===
AWS: recorded behavior only — CloudFormation models neither stack-event nor operation-event text in Smithy, so the requirement comes from the recording. When a template supplies a property value that is not one of the resource type's schema enum values (here AWS::DirectConnect::ResiliencyGroup with IntendedResiliencyModel set to a string outside the schema's enum), CloudFormation rejects the stack in pre-deployment property validation (the stack settles ROLLBACK_COMPLETE, no resource is created) and DescribeEvents reports exactly one VALIDATION_ERROR event for the offending resource whose ValidationStatusReason is `<REDACTED> is not a valid enum value. Supported values: [<the schema's enum values, in schema order, comma-separated>]`. The fixed text of that message is the literal token `<REDACTED>`, then ` is not a valid enum value. Supported values: [` and the trailing `]`; the only variable part is the list of supported values, which comes from the resource type's schema. CloudFormation deliberately REDACTS the rejected value — the value taken from the request never appears in the reason.
Emulator: every graded field of the row matches (LogicalResourceId, ValidationName=PROPERTY_VALIDATION, ValidationStatus=FAILED, ValidationFailureMode=FAIL, ValidationPath=/Resources/<logical id>/Properties/<property name>), and the supported-values list matches, but the reason substitutes the rejected property value for AWS's fixed `<REDACTED>` token, e.g. `nonexistent-resiliency-model is not a valid enum value. Supported values: [maximum-resiliency, high-resiliency, basic-resiliency]`. The preceding assertions all passed: the stack reached ROLLBACK_COMPLETE, DescribeStackResources was empty, the stack-level CREATE_FAILED reason and the VALIDATION_ERROR ResourceType matched.
Why: a client that reads DescribeEvents for property-validation diagnostics gets a message whose shape differs from AWS's for the whole enum error class — any tooling that matches or parses AWS's `<REDACTED> is not a valid enum value.` sentence fails to recognise the emulator's, and the emulator additionally leaks the submitted property value into a diagnostic field AWS keeps redacted, so a template value a caller expects never to be echoed back (the recording shows CloudFormation suppressing it) is echoed.
Fix: the CloudFormation stack-provisioning path's pre-deployment property validation — specifically the message builder that renders a schema enum mismatch into the resource's PROPERTY_VALIDATION event reason. It must emit the literal `<REDACTED>` in place of the offending value and keep the supported-values list it already renders correctly. The sibling defect for the type-mismatch wording is open as 20261119T151106Z-cloudformation-sns-sqs-the-property-validation-row-for.md (different service token, different message class) and does NOT cover this enum reason.
Traces: cloudformation__directconnect.deploy-directconnect-resiliency-group-invalid-model-rejected-and-rolled-back.

## Evidence
cloudformation__directconnect.deploy-directconnect-resiliency-group-invalid-model-rejected-and-rolled-back [mismatch]:
  AssertionError: validation errors of the create: expected [('Rg', 'PROPERTY_VALIDATION', 'FAILED', 'FAIL', '<REDACTED> is not a valid enum value. Supported values: [maximum-resiliency, high-resiliency, basic-resiliency]', '/Resources/Rg/Properties/IntendedResiliencyModel')], got [('Rg', 'PROPERTY_VALIDATION', 'FAILED', 'FAIL', 'nonexistent-resiliency-model is not a valid enum value. Supported values: [maximum-resiliency, high-resiliency, basic-resiliency]', '/Resources/Rg/Properties/IntendedResiliencyModel')]

Expected (AWS): ValidationStatusReason = '<REDACTED> is not a valid enum value. Supported values: [maximum-resiliency, high-resiliency, basic-resiliency]' — fixed text '<REDACTED> is not a valid enum value. Supported values: [' ... ']', variable part only the schema's enum list.
Actual (emulator): ValidationStatusReason = '<the rejected property value> is not a valid enum value. Supported values: [maximum-resiliency, high-resiliency, basic-resiliency]' — the request's property value appears where AWS writes the fixed token '<REDACTED>'.
All other fields of the single VALIDATION_ERROR row match, and the earlier assertions in the same check passed (ROLLBACK_COMPLETE, zero StackResources, stack-level CREATE_FAILED reason 'Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation.', VALIDATION_ERROR ResourceType 'AWS::DirectConnect::ResiliencyGroup'). The divergence is not a per-run value: '<REDACTED>' is fixed text, not a generated id, timestamp or request id.

## Steps to Reproduce
1. CreateStack with a template whose only resource is 'Rg' of Type AWS::DirectConnect::ResiliencyGroup, Properties { ResiliencyGroupName: <unique name>, IntendedResiliencyModel: 'nonexistent-resiliency-model' (any value outside the schema enum), Tags: [...] }, and Outputs referencing Ref Rg and Fn::GetAtt Rg.ResiliencyGroupId.
2. Poll DescribeStacks(StackName=<stack id>) until terminal: the stack reaches ROLLBACK_COMPLETE and DescribeStackResources returns [] (both already match).
3. Page DescribeEvents(StackName=<stack id>) and collect every event with EventType=VALIDATION_ERROR as (LogicalResourceId, ValidationName, ValidationStatus, ValidationFailureMode, ValidationStatusReason, ValidationPath).
4. AWS returns exactly ('Rg', 'PROPERTY_VALIDATION', 'FAILED', 'FAIL', '<REDACTED> is not a valid enum value. Supported values: [maximum-resiliency, high-resiliency, basic-resiliency]', '/Resources/Rg/Properties/IntendedResiliencyModel'); the emulator returns the same tuple with the rejected value in place of the literal '<REDACTED>'.
5. Re-replay cloudformation__directconnect.deploy-directconnect-resiliency-group-invalid-model-rejected-and-rolled-back.
