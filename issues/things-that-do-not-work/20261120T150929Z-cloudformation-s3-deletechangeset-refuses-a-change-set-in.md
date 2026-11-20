Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__s3.cdk-gc-created-buffer-days-protects-a-freshly-isolated-asset-from-deletion cloudformation__s3.cdk-gc-tags-the-unused-asset-isolated-and-deletes-it-while-the-referenced-one-stays
Issue-Key: cloudformation__s3.cdk-gc-created-buffer-days-protects-a-freshly-isolated-asset-from-deletion|_cdk#19|AssertionError|assert result.returncode == expect_rc, (
Issue-Key: cloudformation__s3.cdk-gc-tags-the-unused-asset-isolated-and-deletes-it-while-the-referenced-one-stays|_cdk#19|AssertionError|assert result.returncode == expect_rc, (
Issue-Commit: 2352e453041dca9d0b463884f1fb4473cbd545be

# cloudformation__s3: DeleteChangeSet refuses a change set in execution status EXECUTE_COMPLETE, so every `cdk deploy` that updates a stack exits 1 after the update has already been applied

> _Note: this issue was identified against the code base at commit `2352e453041dca9d0b463884f1fb4473cbd545be`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteChangeSet

## Description
Cause 1 of 1: DeleteChangeSet refuses a change set whose ExecutionStatus is EXECUTE_COMPLETE, answering the emulator-authored refusal `Cannot delete ChangeSet in execution status EXECUTE_COMPLETE`. The CDK CLI deletes the change set it just executed as the last step of a deploy, so the deploy fails AFTER the stack update has succeeded. Failing check ids for this cause: cloudformation__s3.cdk-gc-created-buffer-days-protects-a-freshly-isolated-asset-from-deletion, cloudformation__s3.cdk-gc-tags-the-unused-asset-isolated-and-deletes-it-while-the-referenced-one-stays.

AWS: Smithy (service-com-amazonaws-cloudformation-cloudformation) `DeleteChangeSet` takes ChangeSetName (required) + StackName and has an EMPTY output shape; the ONLY error it models is InvalidChangeSetStatusException (httpError 400, awsQueryError code `InvalidChangeSetStatus`), documented as "The specified change set can't be used to update the stack. For example, the change set status might be CREATE_IN_PROGRESS, or the stack status might be UPDATE_IN_PROGRESS." — i.e. the refusal is scoped to a change set still being CREATED or a stack mid-update, not to one that has finished executing. EXECUTE_COMPLETE is a terminal ExecutionStatus; recorded behavior: DeleteChangeSet on an executed change set succeeds (200, empty body), which is exactly why the CDK CLI's deploy path issues it unconditionally after ExecuteChangeSet reaches UPDATE_COMPLETE.

Emulator: the DeleteChangeSet handler treats any non-pristine execution status as undeletable and refuses with the message `Cannot delete ChangeSet in execution status EXECUTE_COMPLETE`. That sentence is not AWS's: AWS returns no body at all here, and where AWS does refuse this operation it does so as `InvalidChangeSetStatus` with its own wording. The refusal is fatal to the caller: `cdk deploy` exits 1 with that text as the last line, after it has already published the assets and executed the change set, so the tooling reports a failed deploy for a stack that actually updated.

Why: every CDK (and `cloudformation deploy`) user on the update path gets a non-zero exit from a deploy that worked. The caller cannot distinguish "the update failed" from "the cleanup of the plan failed", so it rolls back or retries a stack that is already correct, and any check or pipeline gated on the CLI's exit code fails. Change sets also accumulate, since the client's own cleanup is permanently refused.

Fix: the DeleteChangeSet handler's status guard (and the change-set state it reads): a change set in a terminal ExecutionStatus — EXECUTE_COMPLETE, EXECUTE_FAILED, OBSOLETE, UNAVAILABLE, AVAILABLE — must be deleted and answered 200 with the empty DeleteChangeSetOutput; the InvalidChangeSetStatusException branch belongs only to a change set still in CREATE_IN_PROGRESS/CREATE_PENDING or to a stack in a mid-update status, and when it fires it must carry the modelled code `InvalidChangeSetStatus` rather than a bare sentence.

Related open issues, which do NOT cover this: 20261120T085334Z-cloudformation-sqs-cloudformation-executing-a-change-set-deletes-its-outstanding.md and 20261119T062349Z-cloudformation-2-net-new-causes-a-cascading-replacement.md concern what ExecuteChangeSet does to a change set's SIBLINGS; this is the explicit DeleteChangeSet call on the executed change set itself being refused. The same root cause also fails cloudformation__iam__lambda__s3.cdk-bootstrap-asset-deploy-is-content-addressed, filed separately because it is listed under a different service.

Traces: the two failing check ids listed above.

## Evidence
Expected (AWS, from the recording and the model): after ExecuteChangeSet drives the stack to UPDATE_COMPLETE, `DeleteChangeSet(ChangeSetName=<the executed change set>)` answers HTTP 200 with an empty body (DeleteChangeSetOutput has no members), and `cdk deploy <stack>` exits 0.

Actual (emulator): `cdk deploy` exits 1 and its output tail ends with the emulator-authored refusal of the post-execution cleanup call:
  cdk-gc-tags-...: "`cdk deploy sim2real-cdkgc-<id>` exited 1, expected 0; output tail: '... success: Published sim2real-cdkgc-<id> Template (...)\nsuccess: Published Fn/Code (...)\nCannot delete ChangeSet in execution status EXECUTE_COMPLETE\n'"
  cdk-gc-created-buffer-days-...: "`cdk deploy sim2real-cdkgcbuf-<id>` exited 1, expected 0; output tail: '... success: Published Fn/Code (...)\nCannot delete ChangeSet in execution status EXECUTE_COMPLETE\n'"
In `Cannot delete ChangeSet in execution status EXECUTE_COMPLETE`, every word is emulator-authored fixed text and `EXECUTE_COMPLETE` is the ExecutionStatus read from the emulator's state; AWS emits no message at all for this call. The stack names, asset hashes and ids are per-run values, not part of the requirement. Note the assets publish fine and the template is published — the only diverging call is the trailing DeleteChangeSet.

## Steps to Reproduce
1. `cdk bootstrap` a toolkit with a per-run qualifier, then `cdk deploy <stack>` an app with one Lambda function whose code is a local asset (this first deploy takes the CREATE path).
2. Change the asset (point the function at a second code directory) and `cdk deploy <stack>` again — this is the UPDATE path, so the CLI creates a change set, executes it to UPDATE_COMPLETE, then calls DeleteChangeSet on it.
3. AWS: DeleteChangeSet answers 200 empty and `cdk deploy` exits 0.
4. Emulator: DeleteChangeSet is refused with `Cannot delete ChangeSet in execution status EXECUTE_COMPLETE` and `cdk deploy` exits 1, even though the stack reached UPDATE_COMPLETE.
5. Equivalently, without the CDK: CreateChangeSet on an existing stack, ExecuteChangeSet, wait for UPDATE_COMPLETE, then DeleteChangeSet on that same change set.
