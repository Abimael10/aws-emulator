Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue__iam__s3.column-statistics-task-run-schedule-unknown-table-rejected
Issue-Key: glue__iam__s3.column-statistics-task-run-schedule-unknown-table-rejected|run#20|AssertionError|assert message == expected_missing, f"unexpected message for a missing table: {message!r}"
Issue-Commit: 4a31818efe332ca88a0519d4a493d7749df24842

# Glue StartColumnStatisticsTaskRunSchedule/StopColumnStatisticsTaskRunSchedule report a missing schedule as 'Table <name> not found.' instead of AWS's ColumnStatisticsTaskSettings not-found message

> _Note: this issue was identified against the code base at commit `4a31818efe332ca88a0519d4a493d7749df24842`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartColumnStatisticsTaskRunSchedule

## Description
Cause 1 of 1: the EntityNotFoundException raised by the column-statistics schedule start/stop operations for a database/table pair that has no stored ColumnStatisticsTaskSettings carries the wrong message — it reports the missing *table* rather than the missing *settings* entity.

AWS: Smithy (com-amazonaws-glue-awsglue): StartColumnStatisticsTaskRunSchedule(DatabaseName required, TableName required, CatalogID optional) and StopColumnStatisticsTaskRunSchedule(same input) both model EntityNotFoundException (client, HTTP 400). When no ColumnStatisticsTaskSettings exist for the (catalog, database, table) triple, AWS refuses with 400 EntityNotFoundException whose Message names the *settings* entity by its composite key: the fixed text is "ColumnStatisticsTaskSettings " ... " not found." and the variable part between them is the settings identifier built from the request and the caller's account — "<catalogId>:<accountId>:<DatabaseName>-<TableName>" (for the default catalog both id components are the caller's account id). The table itself may or may not exist — the entity that is absent is the settings record, not the table. Recorded behavior (behavioral check) is the source for the message template; the model states only the error shape and status.

Emulator: the same call answers EntityNotFoundException (code correct) but with Message "Table <TableName> not found." — observed verbatim as 'Table sim2real-behav-table-missing-cbbcc8e7fe41 not found.' Here "Table " and " not found." are the emulator's fixed text and the table name is the value taken from the request; the account-qualified ColumnStatisticsTaskSettings identifier never appears, and the database name is dropped entirely. The handler evidently resolves the table first and reports the table lookup failure instead of looking up (and reporting) the settings entity.

Why: a client that distinguishes "this table has no column-statistics schedule yet" (so it should call CreateColumnStatisticsTaskSettings) from "this table does not exist in the catalog" (a catalog/bootstrap error) reads the emulator's message and concludes the table is missing, taking the wrong recovery branch; tooling and tests that match on AWS's message text, or parse the composite settings identifier out of it, never match.

Fix: the error-construction path of the Glue StartColumnStatisticsTaskRunSchedule and StopColumnStatisticsTaskRunSchedule handlers (and the shared column-statistics-settings lookup they use) must raise EntityNotFoundException naming the ColumnStatisticsTaskSettings entity with its <catalogId>:<accountId>:<database>-<table> key, rather than re-using the table-not-found message. Both operations must agree: the check asserts the identical message for Start and Stop.

Traces: glue__iam__s3.column-statistics-task-run-schedule-unknown-table-rejected — the Start leg fails the message assertion first, so the Stop leg (which the check asserts with the same expected message) is reached only once Start is fixed. Note: the open issues 20261117T133709Z-glue-entity-not-found-refusals-carry-emulator-authored.md and 20261119T084143Z-glue-refusal-messages-diverge-startcrawlerschedule-s-entitynotfoundexception-and.md describe emulator-authored Glue not-found wording for other operations; neither carries this check or these two operations.

## Evidence
Failing check: [mismatch] glue__iam__s3.column-statistics-task-run-schedule-unknown-table-rejected
AssertionError: unexpected message for a missing table: 'Table sim2real-behav-table-missing-cbbcc8e7fe41 not found.'

Expected (AWS), per the check's assertion:
  Error.Code    = EntityNotFoundException (HTTP 400)
  Error.Message = "ColumnStatisticsTaskSettings <catalogId>:<accountId>:<DatabaseName>-<TableName> not found."
                  fixed text: "ColumnStatisticsTaskSettings " and " not found."
                  values from the request/state: the account id (twice, for the default catalog), the DatabaseName and the TableName sent in the request

Actual (emulator):
  Error.Code    = EntityNotFoundException (matches)
  Error.Message = "Table <TableName> not found."
                  fixed text: "Table " and " not found."; only the requested TableName is interpolated, the database name and the account-qualified settings key are absent

The code is right and only the message diverges, so the divergence is in how the not-found condition is described, not in whether the call is refused.

## Steps to Reproduce
1. Create a Glue database D and, in it, a table T backed by an S3 location (the table is only needed for the positive control later in the check).
2. Call glue:StartColumnStatisticsTaskRunSchedule(DatabaseName=D, TableName=M) where M is a table name that does not exist and for which no ColumnStatisticsTaskSettings were ever created.
3. Observe the refusal: AWS answers 400 EntityNotFoundException with Message "ColumnStatisticsTaskSettings <account>:<account>:<D>-<M> not found."; the emulator answers 400 EntityNotFoundException with Message "Table <M> not found."
4. Repeat with glue:StopColumnStatisticsTaskRunSchedule(DatabaseName=D, TableName=M): the same AWS message is expected.
Replay: glue__iam__s3.column-statistics-task-run-schedule-unknown-table-rejected
