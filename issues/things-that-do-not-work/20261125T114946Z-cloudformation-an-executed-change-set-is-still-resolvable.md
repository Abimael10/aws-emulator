Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation.cdk-deploy-method-direct-creates-and-updates-without-a-change-set
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation.cdk-deploy-method-direct-creates-and-updates-without-a-change-set|_err#18|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Commit: 34e4d963b4ee4434f7d0320b5d4c3556e82ed763

# cloudformation: an executed change set is still resolvable by name after the stack settles, instead of being retired (DescribeChangeSet 404 ChangeSetNotFound)

> _Note: this issue was identified against the code base at commit `34e4d963b4ee4434f7d0320b5d4c3556e82ed763`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeChangeSet

## Description
One net-new cause, grounded in the behavioral check cloudformation.cdk-deploy-method-direct-creates-and-updates-without-a-change-set (GROUP deploy, axis:happy-path, cap:c1, plane:control). The check's `cdk deploy --method=direct` legs all pass (both direct deploys leave the stack with no ChangeSetId and ListChangeSets empty, and the subsequent `cdk deploy --change-set-name <name>` leg records the named change set's ARN as the stack's ChangeSetId); the single divergence is on the read AFTER that change set has been executed and the stack has settled.

Cause 1 of 1: once an UPDATE change set has been EXECUTED and its stack has reached UPDATE_COMPLETE, the emulator still resolves that change set by NAME, where AWS has retired the stack's change sets and refuses the read.

AWS: Smithy (services/service-com-amazonaws-cloudformation-cloudformation/smithy-model/com-amazonaws-cloudformation-cloudformation.json): DescribeChangeSet takes ChangeSetName (ChangeSetNameOrId, @required, @length 1..1600) plus optional StackName, and models exactly one error — ChangeSetNotFoundException, httpError 404, awsQueryError code `ChangeSetNotFound`. The model does not say when a change set stops being addressable, so for the lifecycle itself this is recorded behavior only: the recording shows that after the change set is executed and the stack settles into UPDATE_COMPLETE, DescribeChangeSet(StackName=<stack id>, ChangeSetName=<the change set's NAME>) is refused with HTTP 404, error code `ChangeSetNotFound` and message `ChangeSet [<name>] does not exist`, and ListChangeSets(StackName=<stack id>) returns an empty Summaries list. Message rule: the fixed text is `ChangeSet [` ... `] does not exist`; its only variable part is the identifier exactly as the caller passed it (here the change-set name from the request), never a value from one run. Hypothesis for the mechanism (not stated by the model): executing a change set retires the plans recorded against the configuration that execution replaced, so the NAME is no longer resolvable on the stack — note the sibling check recorded in the open issue 20261121T053932Z-cloudformation-deletechangeset-of-an-executed-execute-complete-update.md still resolves an executed change set by its ARN, so the discriminator a fix must respect is ARN-vs-NAME addressing, not "executed means gone everywhere".

Emulator: the same DescribeChangeSet call answers HTTP 200 with the full executed change set — ChangeSetName/ChangeSetId as created, ExecutionStatus `EXECUTE_COMPLETE`, Status `CREATE_COMPLETE`, StackDriftStatus NOT_CHECKED and the Changes list computed at creation — so no error is raised at all and the check fails with `AssertionError: expected the call to be refused, but it succeeded: {... 'ExecutionStatus': 'EXECUTE_COMPLETE', 'Status': 'CREATE_COMPLETE', ...}`. This matches the emulator's stated design: ExecuteChangeSet's module doc (services/service-com-amazonaws-cloudformation-cloudformation/src/ops/execute_change_set.rs) says an executed change set "carries the record of a change the stack took rather than a plan, so nothing supersedes it" and other plans stay "addressable — a read of one still resolves it, ListChangeSets still lists it", and DescribeChangeSet resolves a change set "by its ARN, or by name plus the stack it belongs to" with no retirement step anywhere in between.

Why: a client cannot tell a plan AWS still holds from one it has retired, and the change-set NAME never becomes free again. Deployment tooling that reuses one stable change-set name per stack (the CDK `--change-set-name` flow this check drives is exactly that) probes the name to decide whether a previous plan is outstanding; against AWS the probe answers 404 ChangeSetNotFound after each successful deploy, against the emulator it answers 200 with a spent EXECUTE_COMPLETE plan, so cleanup/idempotency branches written for AWS take the wrong path, and code that correctly handles the modelled ChangeSetNotFoundException error class never exercises that branch here.

Fix: the change-set lifecycle in the ExecuteChangeSet handler and the change-set store it writes (services/service-com-amazonaws-cloudformation-cloudformation/src/ops/execute_change_set.rs and the change-set records in crate::state), plus the name-resolution path the read operations share (services/service-com-amazonaws-cloudformation-cloudformation/src/ops/describe_change_set.rs, services/service-com-amazonaws-cloudformation-cloudformation/src/ops/list_change_sets.rs), so that a settled execution leaves the stack's change sets unresolvable BY NAME and unlisted while keeping ARN resolution intact for the executed record that issue 20261121T053932Z-cloudformation-deletechangeset-of-an-executed-execute-complete-update.md depends on. That open issue looks adjacent but does not cover this one (the report marks this check neither OWNED nor ALREADY FILED): it is about DeleteChangeSet's status gate, while this is about name addressability after execution.

Traces: behavioral check cloudformation.cdk-deploy-method-direct-creates-and-updates-without-a-change-set.

## Evidence
Behavioral check `cloudformation.cdk-deploy-method-direct-creates-and-updates-without-a-change-set` [mismatch], replayed against the emulator.

Sequence (all earlier steps pass): `cdk deploy <stack> --method=direct` twice — after each, DescribeStacks reports CREATE_COMPLETE then UPDATE_COMPLETE with ChangeSetId absent and ListChangeSets empty; then `cdk deploy <stack> --change-set-name <name>` — the stack reaches UPDATE_COMPLETE with the expected Outputs and records ChangeSetId matching `arn:aws:cloudformation:<region>:<account>:changeSet/<name>/<uuid>`.

Failing step: DescribeChangeSet(StackName=<stack id>, ChangeSetName=<name>) right after that change-set deploy completed.
  Expected (AWS, recorded): HTTP 404, error code `ChangeSetNotFound`, message `ChangeSet [<name>] does not exist` (fixed text `ChangeSet [`/`] does not exist`, variable part = the identifier as passed); and the following ListChangeSets(StackName=<stack id>) returns `Summaries: []`.
  Actual (emulator): HTTP 200 with the executed change set — `ExecutionStatus: 'EXECUTE_COMPLETE'`, `Status: 'CREATE_COMPLETE'`, `StackDriftStatus: 'NOT_CHECKED'`, the creation-time `Changes` list, `DeploymentConfig {Mode: STANDARD, DisableRollback: False}` — and no error.
  Failure text: `File "check_cloudformation_cdk_deploy_method_direct_creates_and_updates_without_a_change_set.py", line 69, in run / gone = _err(lambda: cfn.describe_change_set(StackName=stack_id, ChangeSetName=change_set)) / line 88, in _err / raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")`.

Stack name, change-set name, change-set/stack ids, timestamps and request ids in the run are per-run generated values filling the templates; the requirements are the 404 status, the `ChangeSetNotFound` code, the message template above, and the stack listing no change sets after a settled execution.

Model citation: Smithy DescribeChangeSet errors = [ChangeSetNotFoundException] with httpError 404 and awsQueryError code `ChangeSetNotFound`; ListChangeSets models no errors and returns Summaries.

## Steps to Reproduce
1. Create a stack (any template; the check uses a CDK app whose stack carries one output `Variant` and a WaitConditionHandle) and let it reach CREATE_COMPLETE, then update it so it is a settled CREATE_COMPLETE/UPDATE_COMPLETE stack.
2. CreateChangeSet(StackName=<stack id>, ChangeSetName=<name>, ChangeSetType=UPDATE, TemplateBody=<a template that differs from the deployed one>); wait for Status CREATE_COMPLETE. (Equivalently: `cdk deploy <stack> --require-approval never --change-set-name <name>`, which creates, executes and waits.)
3. ExecuteChangeSet(StackName=<stack id>, ChangeSetName=<name>); wait for the stack to reach UPDATE_COMPLETE and for the change set's execution to conclude.
4. DescribeStacks(StackName=<stack id>) — ChangeSetId is the executed change set's ARN `arn:aws:cloudformation:<region>:<account>:changeSet/<name>/<uuid>` (the emulator already matches here).
5. DescribeChangeSet(StackName=<stack id>, ChangeSetName=<name>) — AWS refuses with HTTP 404, code `ChangeSetNotFound`, message `ChangeSet [<name>] does not exist`. The emulator instead answers 200 with the EXECUTE_COMPLETE change set. This is the divergence.
6. ListChangeSets(StackName=<stack id>) — AWS returns an empty Summaries list for the settled stack.
7. Re-replay behavioral check cloudformation.cdk-deploy-method-direct-creates-and-updates-without-a-change-set to confirm the fix, and re-replay cloudformation.changeset-delete-invalid-status-rejected (open issue 20261121T053932Z) so that resolving an executed change set BY ARN keeps working.
