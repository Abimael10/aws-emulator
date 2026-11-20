Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__sqs.cfn-execute-obsolete-changeset-rejected
Issue-Key: cloudformation__sqs|DescribeChangeSet|ChangeSetNotFound
Issue-Commit: f3ce2fa378d1283dd48aeebb96e6d14cd3cac708

# cloudformation: executing a change set DELETES its outstanding siblings instead of leaving them OBSOLETE, so DescribeChangeSet answers ChangeSetNotFound and ExecuteChangeSet never refuses with InvalidChangeSetStatus

> _Note: this issue was identified against the code base at commit `f3ce2fa378d1283dd48aeebb96e6d14cd3cac708`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteChangeSet

## Description
Cause 1 of 1: when one change set of a stack is executed, every other outstanding change set on that stack is removed from the store, so it can no longer be read or refused on its own status.

AWS: recorded behavior only for the retention, and the model carries the state it is retained in. Smithy: `ExecutionStatus` is an enum whose values are `UNAVAILABLE | AVAILABLE | EXECUTE_IN_PROGRESS | EXECUTE_COMPLETE | EXECUTE_FAILED | OBSOLETE`, and `DescribeChangeSet` returns it; `ExecuteChangeSet` models `InvalidChangeSetStatusException` (httpError 400, awsQueryError code `InvalidChangeSetStatus`, doc: "The specified change set can't be used to update the stack. For example, the change set status might be CREATE_IN_PROGRESS, or the stack status might be UPDATE_IN_PROGRESS.") alongside `ChangeSetNotFoundException` (httpError 404, awsQueryError code `ChangeSetNotFound`). The recording shows that after a sibling change set executes to UPDATE_COMPLETE, the untouched change set is STILL ADDRESSABLE: `DescribeChangeSet` for it answers 200 and reports `ExecutionStatus` exactly `OBSOLETE`, and `ExecuteChangeSet` on it is then refused 400 `InvalidChangeSetStatus` with the message `ChangeSet [<change-set ARN or name as the caller named it>] cannot be executed in its current execution status of [OBSOLETE]` — fixed text `ChangeSet [`, `] cannot be executed in its current execution status of [`, `]`, with the change-set identifier taken from the request and the execution status taken from the emulator's state (here `OBSOLETE`). The obsolete plan is never applied: the stack keeps the value the EXECUTED change set carried. Note the prose paragraph the operation's own documentation carries ("When you execute a change set, CloudFormation deletes all other change sets associated with the stack because they aren't valid for the updated stack") is what the emulator implements literally; the API's observed contract, and the modelled `OBSOLETE` state that exists only for this situation, is that the siblings are INVALIDATED and kept readable, not unaddressable.

Emulator: `ExecuteChangeSet` drops the sibling records outright, so the very next read of one is a 404. Replay shows the check's `DescribeChangeSet` of the untouched change set raising `ChangeSetNotFoundException: An error occurred (ChangeSetNotFound) when calling the DescribeChangeSet operation: ChangeSet [<arn:aws:cloudformation:<region>:<account>:changeSet/<name>/<uuid>>] does not exist` — an uncaught error, so the check never reaches its `ExecutionStatus == OBSOLETE` assertion, never gets the `InvalidChangeSetStatus` refusal, and never grades the control change set that must still execute afterwards. The emulator's own handler documents this as intended ("each of those plans is TAKEN AWAY, so a later read of one is answered as a change set the stack does not hold"), so the divergence is a deliberate reading of the doc prose rather than a missing branch.

Why: a client that enumerated or holds the id of a second change set — the common pattern of planning two candidate updates and executing one — gets a 404 where AWS gives it a describable change set in `OBSOLETE`, so it cannot tell "invalidated by another execution" from "never existed / already deleted", and tooling that distinguishes those (CDK/CLI deploy wrappers, drift and plan reconcilers) takes the wrong branch. Any client asserting the documented refusal of an obsolete plan gets `ChangeSetNotFound` (404) instead of `InvalidChangeSetStatus` (400), which is a different error class, a different status and a different message, so retry and error-classification logic diverges too.

Fix: in the `ExecuteChangeSet` handler's post-deploy bookkeeping, where the stack's other change sets are reclaimed, together with the change-set state that backs `DescribeChangeSet`/`ExecuteChangeSet`: a sibling of the executed change set must be RETAINED with its `ExecutionStatus` transitioned to `OBSOLETE` (leaving its stored plan unapplied) instead of being removed, and `ExecuteChangeSet` must refuse a change set in `OBSOLETE` with the modelled `InvalidChangeSetStatusException` and the wording above, as it already does for the other non-executable execution statuses.

Traces: cloudformation__sqs.cfn-execute-obsolete-changeset-rejected — creates a stack with one AWS::SQS::Queue (VisibilityTimeout from a parameter), creates two change sets (A wanting 40, B wanting 50) while both are AVAILABLE, executes B, then reads A.

## Evidence
Expected (AWS, from the recording and the model): after executing sibling change set B to UPDATE_COMPLETE, `DescribeChangeSet` for change set A answers 200 with `ExecutionStatus` == `OBSOLETE`; `ExecuteChangeSet(ChangeSetName=<A>)` is then refused HTTP 400, code `InvalidChangeSetStatus`, message `ChangeSet [<A's identifier as passed>] cannot be executed in its current execution status of [OBSOLETE]`; the queue's VisibilityTimeout is B's value (A's value was never applied); and a change set created afterwards still executes to UPDATE_COMPLETE.

Actual (emulator): `DescribeChangeSet` for change set A raises `botocore.errorfactory.ChangeSetNotFoundException: An error occurred (ChangeSetNotFound) when calling the DescribeChangeSet operation: ChangeSet [arn:aws:cloudformation:<region>:<account>:changeSet/<name>/<uuid>] does not exist` (HTTP 404, code `ChangeSetNotFound`). The record was removed by `ExecuteChangeSet`: `store.change_sets.retain(|other| other.stack_id != stack_id || other.change_set_id == change_set_id || other.execution_began())` in services/service-com-amazonaws-cloudformation-cloudformation/src/ops/execute_change_set.rs (~line 876, under the comment quoting "CloudFormation deletes all other change sets associated with the stack"). The ARN, stack name, region, account and uuid above are per-run values filling the template, not part of the requirement; the status (404 vs 400), the error code (`ChangeSetNotFound` vs `InvalidChangeSetStatus`) and the missing `OBSOLETE` execution status are.

## Steps to Reproduce
1. CreateStack with a template declaring one AWS::SQS::Queue whose VisibilityTimeout comes from a Number parameter; wait for CREATE_COMPLETE (parameter value 30).
2. CreateChangeSet A on that stack with Timeout=40 and CreateChangeSet B with Timeout=50; wait for both to reach CREATE_COMPLETE and confirm DescribeChangeSet reports ExecutionStatus AVAILABLE for each.
3. ExecuteChangeSet(B) and wait for the stack to reach UPDATE_COMPLETE.
4. DescribeChangeSet(A) by its ChangeSetId — AWS answers 200 with ExecutionStatus OBSOLETE; the emulator raises 404 ChangeSetNotFound.
5. ExecuteChangeSet(A) — AWS refuses 400 InvalidChangeSetStatus, "ChangeSet [<A>] cannot be executed in its current execution status of [OBSOLETE]"; the emulator cannot be reached for this because step 4 already 404s (and A's record is gone).
6. Confirm the queue's VisibilityTimeout is 50 (B's value, A's never applied) and that a change set created after the update still executes to UPDATE_COMPLETE.

Replay: cloudformation__sqs.cfn-execute-obsolete-changeset-rejected
