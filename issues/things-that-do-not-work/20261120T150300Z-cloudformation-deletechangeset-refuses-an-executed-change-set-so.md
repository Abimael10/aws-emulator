Issue-Source: trace-sweep-divergence
Issue-Trace: cloudformation-change-set-create-and-execute
Issue-Step: task#9
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudformation-change-set-create-and-execute
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation|DeleteChangeSet|2xx->4xx|$.DeleteChangeSetResponse
Issue-Key: cloudformation|DeleteChangeSet|2xx->4xx|status
Issue-Key: cloudformation|DescribeChangeSet|4xx->2xx|$.ErrorResponse
Issue-Key: cloudformation|DescribeChangeSet|4xx->2xx|status
Issue-Commit: eea346863107feece0c07c64ca5ae5407ab94bd8

# cloudformation: DeleteChangeSet refuses an executed change set, so the plan is never discarded and DescribeChangeSet keeps resolving it

> _Note: this issue was identified against the code base at commit `eea346863107feece0c07c64ca5ae5407ab94bd8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteChangeSet

## Description
Two net-new causes, both on the create-a-stack-through-a-change-set-then-tidy-up path: the emulator treats a change set whose execution has FINISHED as undeletable, so the delete is refused and every later read still resolves the change set.

Cause 1 of 2: DeleteChangeSet on a change set that has already been executed (ExecutionStatus EXECUTE_COMPLETE, ChangeSetStatus CREATE_COMPLETE) is refused HTTP 400 InvalidChangeSetStatus instead of succeeding.

AWS: Smithy: DeleteChangeSet takes ChangeSetName (ChangeSetNameOrId, @required, length 1..1600) plus optional StackName, returns an EMPTY output (DeleteChangeSetOutput has no members) and models exactly ONE error, InvalidChangeSetStatusException (httpError 400, awsQueryError code InvalidChangeSetStatus, doc: "The specified change set can't be used to update the stack. For example, the change set status might be CREATE_IN_PROGRESS, or the stack status might be UPDATE_IN_PROGRESS."). The operation's own documentation states the purpose plainly: "Deletes the specified change set. Deleting change sets ensures that no one executes the wrong change set. If the call successfully completes, CloudFormation successfully deleted the change set." Nothing in the model restricts the deletable ExecutionStatus values (the enum is UNAVAILABLE | AVAILABLE | EXECUTE_IN_PROGRESS | EXECUTE_COMPLETE | EXECUTE_FAILED | OBSOLETE), and the recording shows the real service ACCEPTING the delete of an executed change set: the step answers HTTP 200 with the query-protocol body DeleteChangeSetResponse / DeleteChangeSetResult (empty) / ResponseMetadata.RequestId. The modelled refusal is for a change set that is not in a terminal, no-longer-pending state (creation still running, or an execution in flight) — not for one whose execution has already concluded.

Emulator: answers HTTP 400 InvalidChangeSetStatus and writes nothing — no DeleteChangeSetResponse at all. The handler gates the delete on `change_set.creation_settled() && change_set.plan_is_discardable()`, and its own comment states the intent ("every `ExecutionStatus` past the two a plan waits in says something has already happened to that plan — it is being applied to the stack, it has been applied, or another execution superseded it"), so EXECUTE_COMPLETE and EXECUTE_FAILED are lumped in with EXECUTE_IN_PROGRESS and refused.

Why: tidying up after a deploy is the normal change-set lifecycle — create, execute, delete the spent change set (this is what the CLI/CDK deploy wrappers and most CI pipelines do). Against the emulator that cleanup call hard-fails with a 400 client error, so a pipeline either aborts or has to special-case an error class AWS never returns, and the account is left holding change sets that AWS would have removed. A client that classifies InvalidChangeSetStatus as "retry later / stack busy" loops on a request that can never succeed.

Fix: in the DeleteChangeSet handler's status gate (services/service-com-amazonaws-cloudformation-cloudformation/src/ops/delete_change_set.rs, the `if !change_set.creation_settled() || !change_set.plan_is_discardable()` branch) and in the discardability predicate it calls on the stored change set: a change set whose execution has reached a TERMINAL state (EXECUTE_COMPLETE, EXECUTE_FAILED, and likewise OBSOLETE and the plan states AVAILABLE/UNAVAILABLE once creation has settled) must be deleted and answered 200 with the empty DeleteChangeSetOutput; the modelled InvalidChangeSetStatusException must be kept only for a change set that is still in flight (creation not settled, or EXECUTE_IN_PROGRESS).

Traces: cloudformation-change-set-create-and-execute#task#9 — create stack via change set, execute it, then DeleteChangeSet that change set.

Cause 2 of 2: after the delete, DescribeChangeSet for that change set answers HTTP 200 with the change set's full description instead of the modelled 404 not-found error.

AWS: Smithy: DescribeChangeSet models ChangeSetNotFoundException (httpError 404, awsQueryError code ChangeSetNotFound) as its only error; once a change set is deleted it is no longer addressable, and the recording shows the query-protocol error body ErrorResponse / Error with Type `Sender`, Code `ChangeSetNotFound`, Message `ChangeSet [<change-set name or ARN exactly as the caller named it>] does not exist`, plus RequestId. Fixed text of the message: `ChangeSet [` and `] does not exist`; the only variable part is the change-set identifier, taken verbatim from the request's ChangeSetName (a per-run probe name in the recording — not part of the requirement). HTTP 404 and the code ChangeSetNotFound are the requirement.

Emulator: answers HTTP 200 with the change-set description and emits no ErrorResponse, because the delete in cause 1 was refused and the record is still in the store (the refusal path returns before `store.change_sets.retain(...)`, and the handler documents that "a refusal writes nothing — the change set is still resolved by every read afterwards"). Hypothesis: this step passes as soon as cause 1 is fixed, but it must be re-replayed on its own, because the same read must also 404 for a change set deleted in any other state, and the gate must not be relaxed by simply dropping the record on a refusal.

Why: a client cannot tell a successfully retired change set from a live one. Cleanup verification (delete, then confirm the change set is gone) reports the plan still present and AVAILABLE/EXECUTE_COMPLETE, and a stale change-set id keeps resolving, so tooling that keys idempotency or drift decisions on "does this change set still exist" takes the wrong branch and may try to act on a plan AWS has removed.

Fix: the change-set store that backs DescribeChangeSet, together with the DeleteChangeSet handler's removal of the resolved change set (and its nested hierarchy): once a delete succeeds the record and its nested children must be gone, and DescribeChangeSet must raise ChangeSetNotFoundException with the wording above for an identifier the store no longer holds.

Traces: cloudformation-change-set-create-and-execute#task#10 — DescribeChangeSet for the change set deleted in task#9.

Related open issue (does NOT cover either cause): 20261120T085334Z-cloudformation-sqs-cloudformation-executing-a-change-set-deletes-its-outstanding.md covers the opposite direction on sibling retention (siblings removed where AWS keeps them OBSOLETE); here the owner of the plan is the EXECUTED change set itself, the delete is refused, and the record is kept where AWS removes it.

## Evidence
Fixture cloudformation-change-set-create-and-execute, replayed against the emulator (query protocol, cloudformation):

Step task#9 — DeleteChangeSet(ChangeSetName=<the change set just executed>, StackName=<the stack>):
  Expected (recorded AWS): HTTP 200, body $.DeleteChangeSetResponse = {DeleteChangeSetResult: '', ResponseMetadata: {RequestId: <per-run>}}.
  Actual (emulator): HTTP 400; $.DeleteChangeSetResponse missing (got None). The only error the operation models is InvalidChangeSetStatusException / code InvalidChangeSetStatus (400), raised by services/service-com-amazonaws-cloudformation-cloudformation/src/ops/delete_change_set.rs:185-196 via crate::state::cannot_delete_change_set(&change_set.execution_status) because plan_is_discardable() is false for an executed change set.

Step task#10 — DescribeChangeSet(ChangeSetName=<same change set>):
  Expected (recorded AWS): HTTP 404, body $.ErrorResponse = {Error: {Type: 'Sender', Code: 'ChangeSetNotFound', Message: 'ChangeSet [<identifier from the request>] does not exist'}, RequestId: <per-run>}.
  Actual (emulator): HTTP 200 with the change-set description; $.ErrorResponse missing (got None).

The stack name, change-set name, account, region and request ids in the recording are per-run values filling the message/ARN templates. The requirements are: 200 with an empty DeleteChangeSetResult for the delete, and 404 with code ChangeSetNotFound and the message text `ChangeSet [<identifier>] does not exist` for the read afterwards.

## Steps to Reproduce
1. CreateChangeSet with ChangeSetType CREATE for a new stack whose template declares one resource (an AWS::S3::Bucket in the fixture); wait until DescribeChangeSet reports Status CREATE_COMPLETE / ExecutionStatus AVAILABLE.
2. ExecuteChangeSet for that change set and wait for the stack to reach CREATE_COMPLETE (the change set is now ExecutionStatus EXECUTE_COMPLETE).
3. DeleteChangeSet(ChangeSetName=<that change set>, StackName=<the stack>) — AWS answers HTTP 200 with an empty DeleteChangeSetResult; the emulator answers HTTP 400 InvalidChangeSetStatus.
4. DescribeChangeSet(ChangeSetName=<that change set>) — AWS answers HTTP 404 ErrorResponse with Code ChangeSetNotFound and Message `ChangeSet [<identifier as passed>] does not exist`; the emulator answers HTTP 200 with the change set still fully described.
5. Re-replay fixture cloudformation-change-set-create-and-execute steps task#9 and task#10 to confirm both causes are closed.
