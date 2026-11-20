Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__lambda.cdk-hotswap-lambda-code-update-skips-stack-update-timeout-change-falls-back cloudformation__lambda.cdk-watch-lambda-code-edit-hotswaps-then-timeout-edit-falls-back
Issue-Key: cloudformation__lambda.cdk-hotswap-lambda-code-update-skips-stack-update-timeout-change-falls-back|_cdk#12|AssertionError|assert result.returncode == 0, (
Issue-Key: cloudformation__lambda.cdk-watch-lambda-code-edit-hotswaps-then-timeout-edit-falls-back|_wait_stack_updated#41|AssertionError|raise AssertionError(
Issue-Commit: aa86e9e75ce0346509f1e7a26ee54199b6f59f45

# cloudformation__lambda: DeleteChangeSet refuses an executed change set, so `cdk deploy --hotswap-fallback` / `cdk watch` never complete the non-hotswappable fallback deployment and the stack is never updated

> _Note: this issue was identified against the code base at commit `aa86e9e75ce0346509f1e7a26ee54199b6f59f45`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteChangeSet

## Description
REOPENS 20261109T073558Z-cloudformation-4-net-new-behavioral-causes-deletechangeset-accepts.md: its fix left the call refused.

Cause 1 of 1: DeleteChangeSet refuses a change set whose ExecutionStatus is already terminal (EXECUTE_COMPLETE), which is exactly the call the CDK CLI makes around every deployment that updates an existing stack. Because the refusal aborts the deploy, the full CloudFormation deployment that `--hotswap-fallback` falls back to for a non-hotswappable property change never runs, so the stack is never updated. Failing check ids for this cause: cloudformation__lambda.cdk-hotswap-lambda-code-update-skips-stack-update-timeout-change-falls-back, cloudformation__lambda.cdk-watch-lambda-code-edit-hotswaps-then-timeout-edit-falls-back.

AWS: Smithy (services/service-com-amazonaws-cloudformation-cloudformation/smithy-model/com-amazonaws-cloudformation-cloudformation.json) `DeleteChangeSet` takes ChangeSetName (required, @length 1..1600) + optional StackName, has an EMPTY output shape (DeleteChangeSetOutput), and models exactly ONE error: InvalidChangeSetStatusException, errorKind client, httpError 400, awsQueryError code `InvalidChangeSetStatus`. That error is documented for a change set that "can't be used to update the stack" — a change set still being created (CREATE_IN_PROGRESS/CREATE_PENDING) or a stack in a mid-update status. EXECUTE_COMPLETE is a terminal ExecutionStatus and is not such a case. Recorded behavior: DeleteChangeSet on a change set that has finished executing answers HTTP 200 with the empty DeleteChangeSetOutput, which is why the CDK CLI issues it unconditionally as part of a deploy; the recorded `cdk deploy --hotswap-fallback` and `cdk watch --hotswap-fallback` runs then apply the Timeout change through a full deployment, bump the stack's LastUpdatedTime and reach UPDATE_COMPLETE.

Emulator: the DeleteChangeSet handler's status guard refuses the call with the emulator-authored sentence `Cannot delete ChangeSet in execution status EXECUTE_COMPLETE` — fixed text `Cannot delete ChangeSet in execution status ` followed by the change set's stored ExecutionStatus value read out of the emulator's own change-set state. AWS emits no message at all for this call, so both the refusal and its wording are invented. Surfaced by the CLI as `InvalidChangeSetStatusException: Cannot delete ChangeSet in execution status EXECUTE_COMPLETE`, which ends the deploy with `❌ <stack> failed to deploy` and exit status 1; the second check instead reports that the stack never updated at all within its wait window, because the fallback deployment dies before it can create and execute its own change set.

Why: a client relying on AWS's behavior loses the entire `--hotswap-fallback` contract. The documented CDK flow is that a hotswappable change (Lambda code only) is applied directly, and a non-hotswappable change (here AWS::Lambda::Function Timeout) is reconciled through a normal CloudFormation deployment. Against the emulator, the hotswap leg reports the non-hotswappable property correctly and then the fallback deployment is refused, so the function's Timeout is never changed, no stack event is emitted, LastUpdatedTime never moves, and the CLI exits non-zero. Any pipeline or `cdk watch` loop gated on the CLI exit code fails, and the caller cannot distinguish a failed stack update from a refused change-set cleanup. Change sets also accumulate forever because the only call that removes them is permanently refused.

Fix: the DeleteChangeSet handler's pre-delete ExecutionStatus guard and the change-set state it reads. A change set in a terminal ExecutionStatus (EXECUTE_COMPLETE, EXECUTE_FAILED, OBSOLETE) and one that was never executed (AVAILABLE/UNAVAILABLE) must be deleted and answered HTTP 200 with the empty DeleteChangeSetOutput; the InvalidChangeSetStatusException branch belongs only to a change set still being created or a stack in a mid-update status, and when taken must carry the modelled wire code `InvalidChangeSetStatus` with httpError 400.

Related open issues that do NOT cover these two checks (they are grounded in checks under other service headings, so neither carries these replay keys): 20261120T150300Z-cloudformation-deletechangeset-refuses-an-executed-change-set-so.md, 20261120T150929Z-cloudformation-iam-lambda-s3-deletechangeset-refuses-the-change.md and 20261120T150929Z-cloudformation-s3-deletechangeset-refuses-a-change-set-in.md describe the same DeleteChangeSet guard; closing this one should close with them. Note also that the resolved issue reopened above asserted the opposite requirement (that AWS refuses DeleteChangeSet on an executed change set, with that exact message); the recordings here show the call succeeding, so the guard added by that fix is the defect.

Traces: cloudformation__lambda.cdk-hotswap-lambda-code-update-skips-stack-update-timeout-change-falls-back, cloudformation__lambda.cdk-watch-lambda-code-edit-hotswaps-then-timeout-edit-falls-back.

## Evidence
Expected (AWS, from the recordings and the model): after the hotswap leg rejects the Timeout change as non-hotswappable, the CLI falls back to a full deployment — CreateChangeSet, ExecuteChangeSet to UPDATE_COMPLETE, then DeleteChangeSet(ChangeSetName=<that change set>) answering HTTP 200 with an empty body. `cdk deploy --hotswap-fallback --require-approval never` exits 0, GetFunctionConfiguration reports the new Timeout, and the stack's LastUpdatedTime has moved.

Actual (emulator), check cloudformation__lambda.cdk-hotswap-lambda-code-update-skips-stack-update-timeout-change-falls-back [mismatch]:
  AssertionError: `cdk deploy <stack> --hotswap-fallback --require-approval never` exited 1, expected 0; output tail:
    "⚠️ The following non-hotswappable changes were found:\n    resource: Fn9270CBC0, type: AWS::Lambda::Function, rejected changes: Timeout, reason: resource properties 'Timeout' are not hotswappable on this resource type\n\nCould not perform a hotswap deployment, as the stack <stack> contains non-Asset changes\nFalling back to doing a full deployment\n❌  <stack> failed to deploy\n‣ InvalidChangeSetStatusException: Cannot delete ChangeSet in execution status EXECUTE_COMPLETE"
  The whole sentence `Cannot delete ChangeSet in execution status EXECUTE_COMPLETE` is emulator-authored fixed text; `EXECUTE_COMPLETE` is the change set's stored ExecutionStatus. AWS returns no message for DeleteChangeSet. The per-run stack name, logical id, synthesis/hotswap timings and the asset hashes are not part of the requirement.

Actual (emulator), check cloudformation__lambda.cdk-watch-lambda-code-edit-hotswaps-then-timeout-edit-falls-back [mismatch]:
  AssertionError: stack '<stack arn>' did not update within 420.0s of the Timeout edit; output tail shows `cdk watch` detecting the edit, reporting "rejected changes: Timeout, reason: resource properties 'Timeout' are not hotswappable on this resource type" and then "Could not perform a hotswap deployment, as the stack <stack> contains non-Asset changes" — the fallback deployment never reaches UPDATE_COMPLETE, so LastUpdatedTime never moves. The stack ARN, account, region, timestamps and request ids in that tail are per-run values, not requirements.

## Steps to Reproduce
1. Stage a CDK app with one AWS::Lambda::Function whose code is inline/asset and whose Timeout is 10, and `cdk deploy <stack> --require-approval never` it (CREATE path succeeds, stack reaches CREATE_COMPLETE).
2. Change only the function's code and run `cdk deploy <stack> --hotswap-fallback --require-approval never` — the hotswap leg applies the code directly.
3. Now change the function's Timeout (a non-hotswappable property) and run `cdk deploy <stack> --hotswap-fallback --require-approval never` again.
4. AWS: the CLI reports the non-hotswappable change, falls back to a full deployment, CreateChangeSet/ExecuteChangeSet reach UPDATE_COMPLETE, DeleteChangeSet on the executed change set answers 200 empty, and the command exits 0 with the new Timeout live.
5. Emulator: the fallback deployment is aborted by `InvalidChangeSetStatusException: Cannot delete ChangeSet in execution status EXECUTE_COMPLETE`; the command exits 1 and the function's Timeout and the stack's LastUpdatedTime are unchanged.
6. Same shape via `cdk watch --hotswap-fallback`: edit the handler source (hotswaps), then edit Timeout — the stack never reaches a new UPDATE_COMPLETE.
7. Without the CDK: CreateStack, then CreateChangeSet(ChangeSetType=UPDATE), ExecuteChangeSet, wait for UPDATE_COMPLETE, then DeleteChangeSet(ChangeSetName=<that change set>, StackName=<stack>) and observe HTTP 400 / InvalidChangeSetStatus instead of 200 with an empty body.
