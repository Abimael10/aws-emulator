Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation.change-set-parameter-change-through-ref-chain-classifies-sources cloudformation.changeset-expiration-window-enforced
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation.change-set-parameter-change-through-ref-chain-classifies-sources|run#24|AssertionError|assert stack["StackStatus"] == "UPDATE_COMPLETE", (
Issue-Key: cloudformation.changeset-expiration-window-enforced|_client_error#17|AssertionError|raise AssertionError(f"expected {what} to be rejected, but the call succeeded")
Issue-Commit: 93aea946e1be9d371a5c94c0146ea55c9b1afa4f

# cloudformation: 2 net-new causes — a cascading replacement through a Ref chain rolls the update back, and ExecuteChangeSet leaves its sibling change sets executable

> _Note: this issue was identified against the code base at commit `93aea946e1be9d371a5c94c0146ea55c9b1afa4f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteChangeSet

## Description
Two net-new root causes in CloudFormation's change-set execution. Both are grounded in failing behavioral checks of the `cloudformation` suite; neither is covered by an open issue (the report marked neither check OWNED).

Cause 1 of 2: executing a change set whose replacement cascades through a Ref chain fails the update.
AWS: recorded behavior only (ExecuteChangeSet models no resource-level error: Smithy declares only ChangeSetNotFoundException, InsufficientCapabilitiesException, InvalidChangeSetStatusException, TokenAlreadyExistsException). For an UPDATE change set whose preview is A = Modify/Replacement True (its create-only Name is built from a changed parameter), B = Modify/Replacement True (B's create-only Name is Fn::Sub on A's Ref) and C = Modify/Replacement False (C's updatable Value is A's Ref), execution applies the preview: the stack reaches UPDATE_COMPLETE, A and B are both replaced and carry the new names derived from the new parameter value, and C keeps its physical id. A dependent resource whose create-only property derives from a replaced resource's Ref is itself replaced, in dependency order, as part of that one update.
Emulator: the stack ends UPDATE_ROLLBACK_COMPLETE with StackStatusReason "The following resource(s) failed to update: [B]." — the downstream replacement B is attempted and fails, so the whole update is rolled back and the previewed change can never be applied.
Why: templates that derive one resource's name from another resource's Ref (an extremely common pattern) can never be updated through a parameter change; every such update rolls back, and the change set preview the client validated is not what execution does.
Fix: the update/replacement ordering in CloudFormation's change-set execution engine — the replacement of a resource whose create-only property resolves from a replaced resource's new Ref (create-new-then-delete-old, in dependency order).
Traces: cloudformation.change-set-parameter-change-through-ref-chain-classifies-sources (the preview assertions pass; only execution diverges).

Cause 2 of 2: executing a change set does not remove the stack's other change sets, so a stale sibling stays describable and executable.
AWS: executing one change set of a stack deletes every other change set of that stack. DescribeChangeSet and ExecuteChangeSet naming the sibling are then both refused with HTTP 404, error code ChangeSetNotFound and message "ChangeSet [<name>] does not exist" — fixed text `ChangeSet [` ... `] does not exist`, with the change-set name taken verbatim from the request. Smithy: DescribeChangeSet and ExecuteChangeSet both declare ChangeSetNotFoundException (httpError 404, awsQueryError code ChangeSetNotFound).
Emulator: after ExecuteChangeSet of the winning change set completes the stack update, DescribeChangeSet of the AVAILABLE sibling still succeeds (200, the change set is returned); the check fails on "expected DescribeChangeSet of the sibling of an executed change set to be rejected, but the call succeeded". The ExecuteChangeSet refusal for the stale sibling is equally unreachable.
Why: a superseded change set remains applicable. A client that holds a stale change set can re-apply an outdated template over a stack that has already moved on, instead of being told the change set no longer exists.
Fix: ExecuteChangeSet's handler and the change-set state it owns (purge the stack's remaining change sets when one executes), plus DescribeChangeSet/ExecuteChangeSet lookup so a purged change set raises ChangeSetNotFoundException with AWS's message.
Traces: cloudformation.changeset-expiration-window-enforced.

## Evidence
Cause 1 — check cloudformation.change-set-parameter-change-through-ref-chain-classifies-sources:
  expected: DescribeStacks StackStatus == 'UPDATE_COMPLETE' after ExecuteChangeSet, and ListStackResources physical ids {A: '/<p2>-a', B: '/<p2>-a-b', C: '/<fixed>-c'}
  actual:   AssertionError: execute: 'UPDATE_ROLLBACK_COMPLETE' 'The following resource(s) failed to update: [B]. '
  (the change-set preview itself matched: A Modify/Replacement True, B Modify/Replacement True with Details [(Name, Always, Static, ResourceReference, A)], C Modify/Replacement False)

Cause 2 — check cloudformation.changeset-expiration-window-enforced:
  expected: DescribeChangeSet(StackName=<stack>, ChangeSetName=<stale cs>) -> 404 ChangeSetNotFound, Message 'ChangeSet [<stale cs name>] does not exist'; ExecuteChangeSet of the same -> the same code and message
  actual:   DescribeChangeSet returned the sibling change set successfully (no error raised) -> AssertionError: expected DescribeChangeSet of the sibling of an executed change set to be rejected, but the call succeeded

## Steps to Reproduce
Cause 1: create a stack from a template with Parameters {Prefix, Fixed} and three AWS::SSM::Parameter resources — A with Name Fn::Sub '/${Prefix}-a', B with Name Fn::Sub '${A}-b' (create-only name derived from A's Ref), C with Name Fn::Sub '/${Fixed}-c' and Value {Ref: A}. Wait for CREATE_COMPLETE. CreateChangeSet (ChangeSetType=UPDATE, UsePreviousTemplate, Prefix changed), wait CREATE_COMPLETE, then ExecuteChangeSet and poll DescribeStacks: the emulator ends UPDATE_ROLLBACK_COMPLETE naming [B]; AWS ends UPDATE_COMPLETE with A and B replaced.

Cause 2: create any stack (e.g. one AWS::SNS::Topic). Create two UPDATE change sets (stale, winner) and wait until both are CREATE_COMPLETE/AVAILABLE. ExecuteChangeSet the winner and wait for UPDATE_COMPLETE. Then call DescribeChangeSet and ExecuteChangeSet for the stale change set: the emulator answers both successfully; AWS refuses both with 404 ChangeSetNotFound 'ChangeSet [<stale>] does not exist'.
