Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__events.deploy-eventbus-named-default-rejected
Issue-Key: cloudformation__events.deploy-eventbus-named-default-rejected|run#14|AssertionError|assert stack_failed_reasons and stack_failed_reasons[0] == expected_reason, (
Issue-Commit: d3b7765ce05e53288156df4eb42460237bf11326

# cloudformation__events: a stack rejected by pre-provisioning validation reports the resource-level conflict message as the stack's CREATE_FAILED reason instead of AWS's "Validation failed with N error(s)..." summary

> _Note: this issue was identified against the code base at commit `d3b7765ce05e53288156df4eb42460237bf11326`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
One net-new root cause, grounded in one failing check (listed by the suite under `cloudformation__events`).

Cause 1 of 1: when CloudFormation refuses a stack during pre-provisioning validation (here a template declaring an AWS::Events::EventBus named 'default', a bus every account already owns), the stack-level CREATE_FAILED stack event carries the per-resource validation detail instead of AWS's fixed aggregate summary sentence. Failing check: cloudformation__events.deploy-eventbus-named-default-rejected.

AWS: a validation failure is reported on two different surfaces, with two different texts. (a) DescribeStackEvents emits a CREATE_FAILED event whose LogicalResourceId is the STACK name, and whose ResourceStatusReason is the fixed aggregate sentence 'Validation failed with N error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation.' — the only variable part is N, the number of validation errors the operation collected (1 here). Smithy (cloudformation): StackEvent.ResourceStatusReason is documented as the 'Success/failure message associated with the resource', and StackEvent.DetailedStatus documents the VALIDATION_FAILED value as 'template validation failed because of invalid properties in the template'. (b) The per-resource detail lives in DescribeEvents: Smithy models DescribeEvents -> OperationEvents (list of OperationEvent) with EventType enum values STACK_EVENT|PROGRESS_EVENT|VALIDATION_ERROR|PROVISIONING_ERROR|HOOK_INVOCATION_ERROR, and OperationEvent members ValidationName, ValidationStatus (enum FAILED|SKIPPED), ValidationFailureMode (HookFailureMode enum FAIL|WARN), ValidationStatusReason and ValidationPath. For this template AWS records one VALIDATION_ERROR OperationEvent with LogicalResourceId='Bus', ResourceType='AWS::Events::EventBus', ValidationName='NAME_CONFLICT_VALIDATION', ValidationStatus='FAILED', ValidationFailureMode='FAIL', ValidationPath='/Resources/Bus' and ValidationStatusReason="Resource of type '<type>' with identifier '<identifier>' already exists." — fixed text apart from the resource type and the conflicting identifier, which come from the template. Nothing is provisioned, so ListStackResources is empty and the pre-existing default bus is untouched.

Emulator: the stack does reach ROLLBACK_COMPLETE, but the stack-level CREATE_FAILED event's ResourceStatusReason is the resource-level sentence "Resource of type 'AWS::Events::EventBus' with identifier 'default' already exists." — the aggregate 'Validation failed with 1 error(s). Call DescribeEvents ...' summary is never produced on any stack-level CREATE_FAILED event. The check stops on that assertion, so the emulator's DescribeEvents VALIDATION_ERROR OperationEvent (ValidationName/ValidationStatus/ValidationFailureMode/ValidationPath), the empty ListStackResources and the surviving default bus were not reached and are unverified; whoever fixes this should confirm those too, since the aggregate reason is only correct if the detail really is retrievable from DescribeEvents.

Why: a client that distinguishes a pre-provisioning validation refusal from a resource-provisioning failure keys on the stack-level reason text (CI log scrapers, CDK/SAM deploy wrappers and operators all read the stack event summary first, then DescribeEvents for detail). Against this emulator the summary is absent, so the 'go call DescribeEvents' contract is invisible, and any consumer matching the aggregate sentence sees a provisioning-style per-resource message instead.

Fix: the CloudFormation stack-operation validation path that authors stack-level stack events — the component that records the stack's CREATE_FAILED event when validation refuses an operation must write the fixed aggregate reason with the collected error count, and route the per-resource conflict text into a VALIDATION_ERROR OperationEvent surfaced by DescribeEvents rather than into the stack event's ResourceStatusReason.

Traces: cloudformation__events.deploy-eventbus-named-default-rejected.

## Evidence
cloudformation__events.deploy-eventbus-named-default-rejected (check line 67):
  assert stack_failed_reasons and stack_failed_reasons[0] == expected_reason
  AssertionError: expected the stack's CREATE_FAILED reason to be 'Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation.', got ["Resource of type 'AWS::Events::EventBus' with identifier 'default' already exists."]

Expected vs actual, for the DescribeStackEvents event whose LogicalResourceId == the stack name and ResourceStatus == CREATE_FAILED:
  expected ResourceStatusReason = "Validation failed with N error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation." (all fixed text; N = count of collected validation errors, 1 in this run)
  actual   ResourceStatusReason = "Resource of type 'AWS::Events::EventBus' with identifier 'default' already exists." (the per-resource detail; its type and identifier are values taken from the template)

The stack status itself (ROLLBACK_COMPLETE) matched. The per-resource detail belongs on a DescribeEvents OperationEvent with EventType='VALIDATION_ERROR', ValidationName='NAME_CONFLICT_VALIDATION', ValidationStatus='FAILED', ValidationFailureMode='FAIL', ValidationPath='/Resources/Bus' (Smithy: EventType / ValidationStatus / HookFailureMode enums, OperationEvent.ValidationStatusReason); that part of the check was not reached.

## Steps to Reproduce
1. events:DescribeEventBus(Name='default') — note the account's pre-existing default bus ARN.
2. cloudformation:CreateStack with TemplateBody {"Resources":{"Bus":{"Type":"AWS::Events::EventBus","Properties":{"Name":"default"}}}}.
3. Poll DescribeStacks until terminal: ROLLBACK_COMPLETE (matches today).
4. DescribeStackEvents(StackName=stack) and select events with LogicalResourceId == the stack name and ResourceStatus == 'CREATE_FAILED'.
   Expected ResourceStatusReason: 'Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation.'
   Emulator: "Resource of type 'AWS::Events::EventBus' with identifier 'default' already exists."
5. DescribeEvents(StackName=stack) and select OperationEvents with EventType='VALIDATION_ERROR'.
   Expected exactly one: LogicalResourceId='Bus', ResourceType='AWS::Events::EventBus', ValidationName='NAME_CONFLICT_VALIDATION', ValidationStatus='FAILED', ValidationFailureMode='FAIL', ValidationStatusReason="Resource of type 'AWS::Events::EventBus' with identifier 'default' already exists.", ValidationPath='/Resources/Bus'.
6. ListStackResources(StackName=stack) must be empty, and DescribeEventBus(Name='default') must still return the ARN from step 1 (also after DeleteStack).
