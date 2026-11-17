Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__lambda.cfn-lambda-alias-out-of-band-repoint-drift cloudformation__lambda.cfn-lambda-version-drift-in-sync-then-deleted-after-direct-delete cloudformation__lambda.cfn-lambda-ephemeralstorage-size-below-minimum-rejected cloudformation__lambda.cfn-lambda-ephemeralstorage-update-in-place-resizes-tmp
Issue-Key: cloudformation__lambda.cfn-lambda-alias-out-of-band-repoint-drift|run#15|KeyError|assert before_drifts["Ver"]["StackResourceDriftStatus"] == "IN_SYNC", (
Issue-Key: cloudformation__lambda.cfn-lambda-version-drift-in-sync-then-deleted-after-direct-delete|_resource_drift#12|AssertionError|assert logical_id in out, f"no drift entry for {logical_id!r}: {sorted(out)}"
Issue-Key: cloudformation__lambda.cfn-lambda-ephemeralstorage-size-below-minimum-rejected|run#21|AssertionError|assert not any(e["LogicalResourceId"] == "Fn" for e in events), (
Issue-Key: cloudformation__lambda.cfn-lambda-ephemeralstorage-update-in-place-resizes-tmp|run#15|AssertionError|assert small_result["errno"] == 28, (
Issue-Commit: fe65e03d933b6ac0ef81655d3240f2b613766017

# 3 causes: AWS::Lambda::Version is absent from drift detection, an EphemeralStorage.Size below the floor is not caught by resource-schema validation, and EphemeralStorage does not size /tmp

> _Note: this issue was identified against the code base at commit `fe65e03d933b6ac0ef81655d3240f2b613766017`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackResourceDrifts

## Description
Three distinct root causes reached through CloudFormation stacks that declare Lambda resources.

Cause 1 of 3: drift detection never reports an AWS::Lambda::Version resource — the logical id is simply missing from the results.
AWS: cloudformation DetectStackDrift covers every resource type that supports drift detection, and AWS::Lambda::Version is one of them; DescribeStackResourceDrifts therefore returns a `StackResourceDrifts` entry for the version's logical id with a `StackResourceDriftStatus` of `IN_SYNC` while nothing has changed out of band, and `MODIFIED`/`DELETED` once the live resource diverges or is deleted directly. (Smithy: cloudformation DescribeStackResourceDrifts output member `StackResourceDrifts[].LogicalResourceId` and `StackResourceDrifts[].StackResourceDriftStatus`, enum StackResourceDriftStatus values IN_SYNC / MODIFIED / DELETED / NOT_CHECKED.)
Emulator: the drift results contain only the other resources of the stack — the role and the function are present and the version is not.
Why: a client cannot detect that a published version or the alias routing over it has been changed or deleted outside the stack. Drift is the mechanism for finding exactly that, and silently omitting a resource type reads as "nothing drifted" rather than "not checked".
Fix: the cloudformation drift-detection resource coverage — the AWS::Lambda::Version (and the alias it is read through) drift reader, which must emit a StackResourceDrifts entry rather than skipping the type.
Traces: cloudformation__lambda.cfn-lambda-alias-out-of-band-repoint-drift, cloudformation__lambda.cfn-lambda-version-drift-in-sync-then-deleted-after-direct-delete

Cause 2 of 3: an AWS::Lambda::Function `EphemeralStorage.Size` below the minimum is not caught by CloudFormation's own property validation — the function resource is created and then CREATE_FAILED.
AWS: CloudFormation validates properties against the resource schema before invoking the handler, so a `Size` below the floor fails the stack with no event for the function resource at all — the function is never touched. (Smithy: lambda CreateFunction input member `EphemeralStorage` -> `EphemeralStorage.Size` with range min 512, max 10240; the resource-schema pre-validation stage is documented CloudFormation behavior — recorded behavior only for the ordering.)
Emulator: the event stream shows the function resource going CREATE_IN_PROGRESS then CREATE_FAILED before the rollback, so the out-of-range value reached the Lambda handler instead of being refused by the schema.
Why: a client reading stack events sees a resource-level handler failure where AWS reports a template/schema error, so the failure is attributed to Lambda rather than to the template, and the ordering guarantee that no resource is touched for a schema-invalid template does not hold.
Fix: the AWS::Lambda::Function resource handler's property-validation stage in the cloudformation service — the numeric `range` constraints of the resource schema must be enforced before the resource is provisioned.
Traces: cloudformation__lambda.cfn-lambda-ephemeralstorage-size-below-minimum-rejected

Cause 3 of 3: `EphemeralStorage.Size` is reported but not enforced — /tmp is not actually limited to the configured size.
AWS: `EphemeralStorage.Size` is the size of the function's `/tmp` in MB (range 512–10240). Writing more than that into /tmp fails with POSIX ENOSPC, i.e. errno 28, after roughly `Size` MB have been written; raising Size through a stack update lets the same write succeed.
Emulator: writing 620 MB into a 512 MB /tmp reports nothing written and no errno — so the write neither succeeded nor hit the size limit.
Why: a client cannot observe or test the ephemeral-storage limit at all. Code that sizes /tmp deliberately (large downloads, temp files) behaves differently on the emulator, and a handler that relies on catching ENOSPC never sees it.
Fix: the Lambda execution environment's /tmp provisioning — it must present a writable filesystem of the configured `EphemeralStorage.Size` and fail writes past it with ENOSPC — plus the in-place update path that resizes it.
Traces: cloudformation__lambda.cfn-lambda-ephemeralstorage-update-in-place-resizes-tmp

## Evidence
Cause 1 — actual, verbatim (two checks, same root cause):
  KeyError: 'Ver'            (indexing before_drifts['Ver']['StackResourceDriftStatus'])
  AssertionError: no drift entry for 'Ver': ['ExecRole', 'Fn']
So DescribeStackResourceDrifts returned entries for the role and the function only; the AWS::Lambda::Version logical id is absent. Expected an entry with StackResourceDriftStatus 'IN_SYNC', then 'DELETED' after a direct DeleteFunction of the version.
Cause 2 — actual, verbatim:
  AssertionError: a Size below the floor must fail CloudFormation's own property validation before Fn is ever touched, but it was: [('<stack>', 'CREATE_IN_PROGRESS'), ('Role', 'CREATE_IN_PROGRESS'), ('Role', 'CREATE_IN_PROGRESS'), ('Role', 'CREATE_COMPLETE'), ('Fn', 'CREATE_IN_PROGRESS'), ('Fn', 'CREATE_FAILED'), ('<stack>', 'ROLLBACK_IN_PROGRESS'), ('Fn', 'DELETE_COMPLETE'), ('Role', 'DELETE_IN_PROGRESS'), ('Role', 'DELETE_COMPLETE'), ('<stack>', 'ROLLBACK_COMPLETE')]
(the stack name is per-run; the divergence is the presence of any ('Fn', ...) event).
Cause 3 — actual, verbatim:
  AssertionError: writing 620MB into a 512MB /tmp must hit ENOSPC, got {'written_mb': 0, 'errno': None}
Expected errno 28 (ENOSPC) after ~512 MB written.

## Steps to Reproduce
Cause 1: CreateStack with an AWS::Lambda::Function, an AWS::Lambda::Version (`Ver`) and an AWS::Lambda::Alias; DetectStackDrift and then DescribeStackResourceDrifts. Observed no entry for `Ver` (only `ExecRole` and `Fn`); expected a `Ver` entry with StackResourceDriftStatus IN_SYNC, becoming MODIFIED/DELETED after an out-of-band alias repoint or a direct DeleteFunction of the version.
Cause 2: CreateStack with an AWS::Lambda::Function whose `EphemeralStorage.Size` is below 512; read DescribeStackEvents. Observed Fn CREATE_IN_PROGRESS/CREATE_FAILED; expected no Fn event at all and a schema-level stack failure.
Cause 3: Deploy a function with `EphemeralStorage.Size: 512` and invoke a handler that writes 620 MB into /tmp. Observed `{'written_mb': 0, 'errno': None}`; expected the write to fail with errno 28 (ENOSPC) after ~512 MB, and to succeed after a stack update raising Size.
