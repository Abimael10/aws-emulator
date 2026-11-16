Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation.stack-rollback-stack-client-request-token-marks-rollback-events
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation.stack-rollback-stack-client-request-token-marks-rollback-events|run#16|AssertionError|assert rolled == [
Issue-Commit: 16dfe53738d662d0dc392dd1f50e487d5b567c22

# cloudformation: RollbackStack emits no per-resource delete events and reports "User Initiated" instead of "User Initiated Rollback"

> _Note: this issue was identified against the code base at commit `16dfe53738d662d0dc392dd1f50e487d5b567c22`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RollbackStack

## Description
Cause 1 of 1: the events RollbackStack produces are both incomplete and mis-worded.

AWS: RollbackStack on a stack left in UPDATE_FAILED by an update run with DisableRollback=true rolls the stack back and records the rollback as its own stream of StackEvents, every row carrying the ClientRequestToken passed to RollbackStack (Smithy: StackEvent.ClientRequestToken — "All events triggered by a given stack operation are assigned the same client request token, which you can use to track operations"; StackEvent.ResourceStatusReason — "Success/failure message associated with the resource"). Recorded behavior: the rows, oldest first, are (stack, UPDATE_ROLLBACK_IN_PROGRESS, reason "User Initiated Rollback"), (stack, UPDATE_ROLLBACK_COMPLETE_CLEANUP_IN_PROGRESS, no reason), then one (logical id, DELETE_IN_PROGRESS, no reason) and one (logical id, DELETE_COMPLETE, no reason) for each resource the failed update had added and the rollback removes, then (stack, UPDATE_ROLLBACK_COMPLETE, no reason). The fixed text of the first row's reason is exactly "User Initiated Rollback"; the LogicalResourceIds in the per-resource rows are the template's own logical ids and the token is the caller's.

Emulator: the rollback produces only the three stack-level rows — UPDATE_ROLLBACK_IN_PROGRESS, UPDATE_ROLLBACK_COMPLETE_CLEANUP_IN_PROGRESS, UPDATE_ROLLBACK_COMPLETE (all with the correct ClientRequestToken) — and (a) the UPDATE_ROLLBACK_IN_PROGRESS row's ResourceStatusReason is "User Initiated", the wording CloudFormation uses for a user-initiated create/update, not "User Initiated Rollback"; (b) no DELETE_IN_PROGRESS / DELETE_COMPLETE row is emitted for the resource the rollback deletes. The same run's failed update DID carry its per-resource CREATE_IN_PROGRESS/CREATE_FAILED rows (that assertion passed), so the gap is specific to the rollback phase, not to per-resource events in general.

Why: a client that reads DescribeStackEvents to learn what a rollback actually undid sees an empty resource list — it cannot tell which resources were deleted, nor when the delete of a given logical id finished, and any progress UI or audit trail built on those rows shows the rollback touching nothing. A consumer that distinguishes a rollback from an ordinary update by the "User Initiated Rollback" reason text misclassifies the operation.

Fix: the RollbackStack path of the CloudFormation stack-operation executor — the rollback must record a DELETE_IN_PROGRESS/DELETE_COMPLETE event pair per resource it removes, stamped with the rollback's ClientRequestToken, and the reason constant used for the rollback-initiating stack event must be "User Initiated Rollback".

Traces: cloudformation.stack-rollback-stack-client-request-token-marks-rollback-events (mismatch; AssertionError "events of the rollback: [(<stack>, 'UPDATE_ROLLBACK_IN_PROGRESS', 'User Initiated', <tok>), (<stack>, 'UPDATE_ROLLBACK_COMPLETE_CLEANUP_IN_PROGRESS', None, <tok>), (<stack>, 'UPDATE_ROLLBACK_COMPLETE', None, <tok>)]").

## Evidence
Check cloudformation.stack-rollback-stack-client-request-token-marks-rollback-events.

Expected (AWS), rows as (LogicalResourceId, ResourceStatus, ResourceStatusReason, ClientRequestToken) after RollbackStack(ClientRequestToken=<rollback-tok>):
  (<stack name>, 'UPDATE_ROLLBACK_IN_PROGRESS', 'User Initiated Rollback', <rollback-tok>)
  (<stack name>, 'UPDATE_ROLLBACK_COMPLETE_CLEANUP_IN_PROGRESS', None, <rollback-tok>)
  ('Fast', 'DELETE_IN_PROGRESS', None, <rollback-tok>)
  ('Fast', 'DELETE_COMPLETE', None, <rollback-tok>)
  (<stack name>, 'UPDATE_ROLLBACK_COMPLETE', None, <rollback-tok>)

Actual (emulator):
  (<stack name>, 'UPDATE_ROLLBACK_IN_PROGRESS', 'User Initiated', <rollback-tok>)
  (<stack name>, 'UPDATE_ROLLBACK_COMPLETE_CLEANUP_IN_PROGRESS', None, <rollback-tok>)
  (<stack name>, 'UPDATE_ROLLBACK_COMPLETE', None, <rollback-tok>)

Two divergences: the reason text of the rollback-initiating stack event ('User Initiated' vs the required fixed text 'User Initiated Rollback'), and the complete absence of the per-resource DELETE_IN_PROGRESS/DELETE_COMPLETE pair for the logical id the rollback removes. 'Fast' above is this template's logical id; the requirement is one pair per resource the rollback deletes, not that literal name. The stack name and token values are per-run and are not part of the requirement.

## Steps to Reproduce
1. CreateStack with a template containing only an AWS::CloudFormation::WaitConditionHandle; wait for CREATE_COMPLETE.
2. UpdateStack with the same template plus an AWS::CloudFormation::WaitCondition (Timeout 1, Count 1) that must time out, passing DisableRollback=true and ClientRequestToken=<update-tok>; wait for UPDATE_FAILED.
3. Call RollbackStack(StackName=<stack id>, ClientRequestToken=<rollback-tok>) and wait for UPDATE_ROLLBACK_COMPLETE.
4. Page DescribeStackEvents for the stack and take the rows added after the UPDATE_FAILED. The emulator returns three stack-level rows; AWS returns five, including DELETE_IN_PROGRESS and DELETE_COMPLETE for the wait condition's logical id, and words the first row's reason 'User Initiated Rollback'.
