Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__events__sqs.xsvc-cloudformation-2-stackset-failed-instance-operation-status
Issue-Key: cloudformation__events__sqs.xsvc-cloudformation-2-stackset-failed-instance-operation-status|run#19|AssertionError|assert op["Status"] == "FAILED", (
Issue-Commit: 93aea946e1be9d371a5c94c0146ea55c9b1afa4f

# cloudformation__events__sqs: a CreateStackInstances operation whose instance stack fails reports SUCCEEDED with FailedStackInstancesCount 0

> _Note: this issue was identified against the code base at commit `93aea946e1be9d371a5c94c0146ea55c9b1afa4f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackSetOperation

## Description
Cause 1 of 1: a stack-set operation whose instance stack cannot be created is reported as a success.
AWS: Smithy — DescribeStackSetOperation returns StackSetOperation.Status of StackSetOperationStatus (RUNNING | SUCCEEDED | FAILED | STOPPING | STOPPED | QUEUED) and StackSetOperation.StatusDetails.FailedStackInstancesCount, "the number of stack instances for which the StackSet operation failed". For a stack set whose template contains a resource that cannot be created (an AWS::CloudFormation::WaitCondition with Timeout 1 that nobody signals), CreateStackInstances with FailureToleranceCount 0 goes RUNNING and then FAILED, StatusDetails.FailedStackInstancesCount is 1, the instance's DetailedStatus is FAILED with a StatusReason naming the failing logical resource ("ResourceLogicalId:<id>, ..." — the same text ListStackInstances returns), and EventBridge receives "CloudFormation StackSet Operation Status Change" with status FAILED for that OperationId (so a SUCCEEDED-only rule never matches it).
Emulator: DescribeStackSetOperation for that CreateStackInstances OperationId returns Action CREATE, Status 'SUCCEEDED', StatusDetails {'FailedStackInstancesCount': 0}, an EndTimestamp one second after CreationTimestamp, and no failure is reflected anywhere; the instance's failing resource is never accounted for.
Why: a client's only signal that a multi-account/multi-region rollout failed is the operation status and its FailedStackInstancesCount (and the EventBridge status-change event). Reporting SUCCEEDED makes a failed rollout indistinguishable from a good one: failure tolerance is never exercised, no remediation is triggered, and SUCCEEDED-only event rules fire for failures.
Fix: the stack-set operation engine — propagate each instance stack's terminal status into the operation (Status FAILED when any instance fails under the operation's FailureToleranceCount), into StatusDetails.FailedStackInstancesCount, into the instance's Status/DetailedStatus/StatusReason, and into the EventBridge StackSet Operation / StackInstance status-change notifications.

## Evidence
Check cloudformation__events__sqs.xsvc-cloudformation-2-stackset-failed-instance-operation-status:
  expected: DescribeStackSetOperation(OperationId=<CreateStackInstances op>).Status == 'FAILED' (after RUNNING), StatusDetails.FailedStackInstancesCount == 1, instance detailed-status FAILED with a StatusReason naming the failing logical resource
  actual:   AssertionError: CreateStackInstances '<op-id>' with a template whose resource cannot be created: expected operation FAILED, got 'SUCCEEDED' ({'Action': 'CREATE', 'Status': 'SUCCEEDED', 'OperationPreferences': {'FailureToleranceCount': 0, 'MaxConcurrentCount': 1}, 'StatusDetails': {'FailedStackInstancesCount': 0}, 'RetryUndoOps': [], ...})

## Steps to Reproduce
1. CreateStackSet (self-managed, with administration role and execution role) whose template contains an AWS::CloudFormation::WaitCondition with Timeout 1 that nobody signals.
2. CreateStackInstances for one account/region with OperationPreferences FailureToleranceCount 0, MaxConcurrentCount 1.
3. Poll DescribeStackSetOperation for the returned OperationId: the emulator settles on Status SUCCEEDED with StatusDetails.FailedStackInstancesCount 0 within ~1s; AWS goes RUNNING then FAILED with FailedStackInstancesCount 1, ListStackInstances reports the instance FAILED with a StatusReason naming the WaitCondition, and the EventBridge operation status-change event carries FAILED.
