Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__iam__lambda__s3.cdk-bootstrap-asset-deploy-is-content-addressed
Issue-Key: cloudformation__iam__lambda__s3.cdk-bootstrap-asset-deploy-is-content-addressed|_cdk#18|AssertionError|assert rc == expect_rc, f"`cdk {args[0]}` rc={rc}, expected {expect_rc}: {out[-1200:]!r}"
Issue-Commit: 2352e453041dca9d0b463884f1fb4473cbd545be

# cloudformation__iam__lambda__s3: DeleteChangeSet refuses the change set `cdk deploy` just executed ("Cannot delete ChangeSet in execution status EXECUTE_COMPLETE"), so the content-addressed asset redeploy exits 1

> _Note: this issue was identified against the code base at commit `2352e453041dca9d0b463884f1fb4473cbd545be`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteChangeSet

## Description
Cause 1 of 1: DeleteChangeSet refuses a change set whose ExecutionStatus is EXECUTE_COMPLETE. The CDK CLI deletes the change set it has just executed as the final step of a deploy, so `cdk deploy` exits 1 after the stack update has already been applied. Failing check id for this cause: cloudformation__iam__lambda__s3.cdk-bootstrap-asset-deploy-is-content-addressed.

AWS: Smithy (service-com-amazonaws-cloudformation-cloudformation) `DeleteChangeSet` takes ChangeSetName (required) + StackName, has an EMPTY output shape, and models exactly ONE error — InvalidChangeSetStatusException (httpError 400, awsQueryError code `InvalidChangeSetStatus`), documented as "The specified change set can't be used to update the stack. For example, the change set status might be CREATE_IN_PROGRESS, or the stack status might be UPDATE_IN_PROGRESS." The refusal is therefore scoped to a change set still being created or a stack mid-update; EXECUTE_COMPLETE is a terminal ExecutionStatus. Recorded behavior: DeleteChangeSet on an executed change set answers 200 with no body, which is why the CDK CLI issues it unconditionally after ExecuteChangeSet settles.

Emulator: the DeleteChangeSet handler's status guard rejects a change set in EXECUTE_COMPLETE with the emulator-authored sentence `Cannot delete ChangeSet in execution status EXECUTE_COMPLETE`. AWS emits no message for this call at all, so both the refusal and its wording are invented. `cdk deploy` surfaces that line and exits 1.

Why: the CDK asset path cannot be exercised past the first create. A caller gets a failed deploy for a stack that actually reached UPDATE_COMPLETE, cannot tell a failed update from a failed plan cleanup, and its change sets pile up because the client-side cleanup is permanently refused. Any pipeline gated on the CLI exit code fails on a successful deployment.

Fix: the DeleteChangeSet handler's status guard and the change-set state it reads — a change set in a terminal ExecutionStatus (EXECUTE_COMPLETE, EXECUTE_FAILED, OBSOLETE, as well as AVAILABLE/UNAVAILABLE) must be deleted and answered 200 with the empty DeleteChangeSetOutput; the InvalidChangeSetStatusException branch belongs only to CREATE_IN_PROGRESS/CREATE_PENDING change sets or a stack in a mid-update status, and must then carry the modelled code `InvalidChangeSetStatus`.

The same root cause fails two cloudformation__s3 checks (cdk-gc-*); those are filed separately because they are listed under a different service. Related open issues that do NOT cover this: 20261120T085334Z-cloudformation-sqs-cloudformation-executing-a-change-set-deletes-its-outstanding.md and 20261119T062349Z-cloudformation-2-net-new-causes-a-cascading-replacement.md, which concern what ExecuteChangeSet does to a change set's SIBLINGS, not the explicit DeleteChangeSet of the executed change set.

Traces: cloudformation__iam__lambda__s3.cdk-bootstrap-asset-deploy-is-content-addressed.

## Evidence
Expected (AWS, from the recording and the model): after ExecuteChangeSet reaches UPDATE_COMPLETE, `DeleteChangeSet(ChangeSetName=<the executed change set>)` answers HTTP 200 with an empty body, and `cdk deploy --require-approval never` exits 0.

Actual (emulator): `AssertionError: \`cdk deploy\` rc=1, expected 0: '\n✨  Synthesis time: 2.05s\n\nCannot delete ChangeSet in execution status EXECUTE_COMPLETE\n'`. The whole sentence `Cannot delete ChangeSet in execution status EXECUTE_COMPLETE` is emulator-authored fixed text, with `EXECUTE_COMPLETE` taken from the change set's stored ExecutionStatus; AWS returns no message here. The synthesis time and the per-run stack/qualifier/asset-hash values are not part of the requirement.

## Steps to Reproduce
1. `cdk bootstrap` a per-run toolkit (own --qualifier and --toolkit-stack-name).
2. `cdk deploy --require-approval never` an app with one Lambda function whose code is a local file asset — the CREATE path.
3. Redeploy the same stack with the asset contents changed, so the CLI takes the UPDATE path: CreateChangeSet, ExecuteChangeSet to UPDATE_COMPLETE, then DeleteChangeSet on that change set.
4. AWS: DeleteChangeSet answers 200 empty, `cdk deploy` exits 0 and the asset key is content-addressed.
5. Emulator: DeleteChangeSet is refused with `Cannot delete ChangeSet in execution status EXECUTE_COMPLETE`, `cdk deploy` exits 1, and the content-addressing assertions are never reached.
6. Without the CDK: CreateChangeSet on an existing stack, ExecuteChangeSet, wait for UPDATE_COMPLETE, then DeleteChangeSet on that same change set.
