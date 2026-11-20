Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__dynamodb.cdk-import-mapping-file-adopts-a-table-with-its-item-and-refuses-a-missing-one
Issue-Key: cloudformation__dynamodb.cdk-import-mapping-file-adopts-a-table-with-its-item-and-refuses-a-missing-one|run#23|AssertionError|assert len(failed_cs) == 1, f"expected one FAILED change set, got {failed_cs!r}"
Issue-Commit: 0139b649892491ec475f7b8e31445a257f779000

# cloudformation__dynamodb: a `cdk import` whose resource mapping names a table that does not exist leaves NO FAILED change set behind, so the refusal is unauditable

> _Note: this issue was identified against the code base at commit `0139b649892491ec475f7b8e31445a257f779000`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateChangeSet

## Description
Cause 1 of 1: an IMPORT change set naming a DynamoDB table that does not exist never reaches, or never retains, status FAILED.
AWS: `cdk import --resource-mapping <file>` creates a change set with ChangeSetType=IMPORT whose ResourcesToImport names the table to adopt. When the named table does not exist, the change-set creation fails: the change set remains listed by ListChangeSets with Status 'FAILED', and DescribeChangeSet on it answers Status 'FAILED' with StatusReason 'Table: <TableName> does not exist.' — fixed text whose variable part is the table name taken from the mapping file. The CLI exits 1 and prints that same reason line, the stack is left untouched (CREATE_COMPLETE with only the resources it already had) and DescribeTable on the named table still answers ResourceNotFoundException 'Requested resource not found: Table: <TableName> not found'. A later import of an existing table reaches IMPORT_COMPLETE and adopts it. Smithy: cloudformation CreateChangeSet/DescribeChangeSet model ChangeSetType IMPORT and ChangeSetStatus FAILED with StatusReason; the reason text is recorded behavior only.
Emulator: the CLI does exit 1, but ListChangeSets on the stack returns no change set in status FAILED at all — 'expected one FAILED change set, got []' — so there is no change set to describe and no recorded reason for the refusal.
Why: a client adopting existing infrastructure has no durable record of why an import was refused; it cannot tell a typo in the mapping file from an import that was never attempted, and tooling that polls DescribeChangeSet for the FAILED status and its reason hangs or 404s.
Fix: the IMPORT path of the change-set creator — create the change set, run the per-entry existence check, and persist it in status FAILED with the StatusReason naming the missing resource instead of discarding it.
Related (does NOT cover this check): open issue 20261102T094631Z-cloudformation-sqs-4-net-new-causes-an-import.md reports the same shape for an SQS queue under service token cloudformation__sqs; this check is a DynamoDB table import under cloudformation__dynamodb and is still failing.
Traces: cloudformation__dynamodb.cdk-import-mapping-file-adopts-a-table-with-its-item-and-refuses-a-missing-one.

## Evidence
Expected: exactly one change set in Summaries with Status 'FAILED', and DescribeChangeSet on it answering (Status, StatusReason) == ('FAILED', 'Table: <missing-table> does not exist.').
Actual: AssertionError: expected one FAILED change set, got [] — ListChangeSets(StackName=<stack-id>) returned no FAILED summaries after `cdk import` of a mapping naming a nonexistent table (the CLI's own exit code 1 assertion passed, so the import was refused but left no change-set record).

## Steps to Reproduce
Create a DynamoDB table out of band (pk S, PAY_PER_REQUEST) and PutItem one marker item. `cdk deploy <stack>` with the app rendering only a WaitConditionHandle, then `cdk import <stack> --resource-mapping <file>` where the mapping names a table that does NOT exist. Then call ListChangeSets(StackName=<stack-id>) and DescribeChangeSet on the FAILED summary; expect Status 'FAILED' with StatusReason 'Table: <name> does not exist.'. Replay the check id in trace_ids.
