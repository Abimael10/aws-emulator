Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ecr__iam__s3__ssm__sts.cdk-bootstrap-re-bootstrap-with-a-new-tag-value-updates-every-toolkit-resource-in-place
Issue-Key: cloudformation__ecr__iam__s3__ssm__sts.cdk-bootstrap-re-bootstrap-with-a-new-tag-value-updates-every-toolkit-resource-in-place|_assert_modern_toolkit#17|AssertionError|assert stack["StackStatus"] == status, (
Issue-Commit: 7148f2b6638ce0f4ccfada01b07f71c83f7419cd

# CloudFormation: a stack updated in place still reports StackStatus CREATE_COMPLETE instead of UPDATE_COMPLETE, so a re-bootstrap/update cannot be distinguished from the original create

> _Note: this issue was identified against the code base at commit `7148f2b6638ce0f4ccfada01b07f71c83f7419cd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
ONE root cause (1 of 1), reached through the cdk re-bootstrap check of this service group.

Cause 1 of 1: after a successful in-place stack update (here the second `cdk bootstrap --force` run of the CDKToolkit stack, which changes the stack's user tag Foo=Bar -> Foo=BarBaz), DescribeStacks still reports the stack's StackStatus as CREATE_COMPLETE. AWS reports UPDATE_COMPLETE.
AWS: Smithy cloudformation#StackStatus is a closed enum whose members include CREATE_COMPLETE, UPDATE_IN_PROGRESS, UPDATE_COMPLETE_CLEANUP_IN_PROGRESS and UPDATE_COMPLETE. A stack's status records the last operation performed on it: a CreateStack that succeeds leaves CREATE_COMPLETE, and an UpdateStack that succeeds moves the stack through UPDATE_IN_PROGRESS and UPDATE_COMPLETE_CLEANUP_IN_PROGRESS to UPDATE_COMPLETE, which is then what DescribeStacks/Stacks[].StackStatus returns until the next operation. CREATE_COMPLETE is never the terminal status of an update. Recorded behavior for this sequence: the first bootstrap leaves CREATE_COMPLETE (the check asserts that and it passes) and the re-bootstrap leaves UPDATE_COMPLETE with the new tag value on the stack and on every taggable resource.
Emulator: the re-bootstrap's `cdk bootstrap --force` exits 0 (the check requires returncode 0 and that assertion passed), so the update was accepted, yet DescribeStacks for the toolkit stack still answers StackStatus 'CREATE_COMPLETE'.
Why: StackStatus is the single field every deploy tool, CI gate and waiter polls to decide what happened to a stack. A client that waits for stack_update_complete, or that branches on CREATE_COMPLETE vs UPDATE_COMPLETE to tell "this stack was created by me just now" from "this stack already existed and I changed it", draws the wrong conclusion; an idempotent re-deploy is indistinguishable from a first deploy, and rollback/drift logic keyed on the update statuses never fires.
Fix: the stack-status transition in the CloudFormation UpdateStack path (the stack state record DescribeStacks projects) must set the UPDATE_* statuses and settle on UPDATE_COMPLETE when an update succeeds, rather than leaving the status the create wrote. Already-filed neighbours, named so they are not mistaken for coverage: 20261113T030715Z-cloudformation-ecr-iam-s3-ssm-cdk-bootstrap-toolkit-the-cloudformation-created-cfn-exec.md and 20261110T232635Z-cloudformation-ecr-iam-s3-ssm-cdk-bootstrap-toolkit-fidelity-ecr-lifecyclepolicy-dropped-defaulted.md own other cdk-bootstrap checks of adjacent service groups, and neither carries this check's replay key or the stack-status cause.
Traces: cloudformation__ecr__iam__s3__ssm__sts.cdk-bootstrap-re-bootstrap-with-a-new-tag-value-updates-every-toolkit-resource-in-place.

## Evidence
Suite simparity-behavioral-aws-v1, replayed against this checkout (713/1010 passed). The check is listed under `### cloudformation__ecr__iam__s3__ssm__sts` and is marked neither ALREADY FILED nor OWNED.

Cause 1 — cloudformation__ecr__iam__s3__ssm__sts.cdk-bootstrap-re-bootstrap-with-a-new-tag-value-updates-every-toolkit-resource-in-place [mismatch], failing at the check's _assert_modern_toolkit:
  AssertionError: toolkit stack is 'CREATE_COMPLETE', expected 'UPDATE_COMPLETE'
  Sequence: (1) `cdk bootstrap --tags Foo=Bar --tags sim2real=<check> --cloudformation-execution-policies arn:aws:iam::aws:policy/AdministratorAccess` -> the check asserts status CREATE_COMPLETE and the user tags [(Foo,Bar),(sim2real,<check>)] and both pass; (2) `cdk bootstrap --tags Foo=BarBaz --tags sim2real=<check> --cloudformation-execution-policies arn:aws:iam::aws:policy/AdministratorAccess --force` -> the CLI exits 0 (asserted); (3) the check re-reads the toolkit stack and requires StackStatus 'UPDATE_COMPLETE' with tags [(Foo,BarBaz),(sim2real,<check>)].
  Expected: DescribeStacks reports StackStatus 'UPDATE_COMPLETE' for the toolkit stack after step 2.
  Actual: 'CREATE_COMPLETE' — the status the create wrote, unchanged by the update.
  The in-place assertions that follow (role ids unchanged, ECR repository not replaced, the S3 object written before still readable) were not reached.

## Steps to Reproduce
Boot the emulator from this checkout and point a `cloudformation` client at it.
1. CreateStack S from any template; poll DescribeStacks until terminal — StackStatus is CREATE_COMPLETE (correct).
2. UpdateStack on S with a change that is applied (e.g. new Tags, or a changed resource property).
3. Poll DescribeStacks until terminal.
4. Observe StackStatus still 'CREATE_COMPLETE' instead of 'UPDATE_COMPLETE' (and no UPDATE_IN_PROGRESS / UPDATE_COMPLETE_CLEANUP_IN_PROGRESS on the way). Equivalently, run `cdk bootstrap` twice with different --tags values and read the CDKToolkit stack's status after the second run.
   Replay: cloudformation__ecr__iam__s3__ssm__sts.cdk-bootstrap-re-bootstrap-with-a-new-tag-value-updates-every-toolkit-resource-in-place
